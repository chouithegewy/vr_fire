//! vr_fire multiplayer relay: assigns each WebSocket client an id and re-broadcasts its
//! messages (tagged with that id) to everyone else. Game state lives in the clients.
//!
//! Also logs player activity (joins, drops, flips, leaves) to `$RELAY_LOG` and serves a
//! stats page. nginx terminates TLS:
//!   /vr_fire/ws          → 127.0.0.1:$PORT        (default 8795, WebSocket)
//!   /vr_fire/stats[.json] → 127.0.0.1:$STATS_PORT (default 8796, HTML / JSON)
//!
//! Backpressure (F01, docs/superpowers/specs/2026-09-25-relay-backpressure-design.md): at most
//! 32 admitted sockets; each recipient has a bounded outbox where a sender's pending pose is
//! replaced in place; a recipient whose outbox overflows, whose writes stall past the
//! deadline, or who floods input is disconnected without affecting anyone else.

mod limits;
mod outbox;
mod stats;
mod transport;

use limits::{Limits, TokenBucket};
use outbox::{CloseReason, Outbox, Push};
use serde_json::{Value, json};
use stats::Stats;
use std::collections::HashMap;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use transport::{Guarded, Next, RealClock, classify};
use tungstenite::{Message, WebSocket};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A published player. The worker thread owns the WebSocket; others only enqueue into the
/// outbox or request cancellation.
struct Peer {
    id: u64,
    outbox: Outbox,
    cancel: Arc<AtomicBool>,
    /// Cloned socket handle used only to interrupt blocked I/O.
    shutdown: Option<TcpStream>,
}

impl Peer {
    /// Ask this peer's worker to stop. Call with no registry or outbox lock held.
    fn request_close(&self, reason: CloseReason) {
        self.outbox.close(reason);
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(s) = &self.shutdown {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Default)]
struct Counters {
    admitted: AtomicUsize,
    rejected: AtomicU64,
    handshake_failures: AtomicU64,
    high_water: AtomicUsize,
    coalesced: AtomicU64,
    disconnects: [AtomicU64; CloseReason::ALL.len()],
}

#[derive(Clone, Copy)]
enum Kind {
    Pose,
    Event,
    Leave,
}

struct Room {
    limits: Limits,
    peers: Mutex<HashMap<u64, Arc<Peer>>>,
    stats: Mutex<Stats>,
    counters: Counters,
}

/// One admitted socket; dropping it frees the slot (worker exit, failed handshake or spawn).
struct Permit(Arc<Room>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.counters.admitted.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Room {
    fn new(limits: Limits, stats: Stats) -> Self {
        Room { limits, peers: Mutex::default(), stats: Mutex::new(stats), counters: Counters::default() }
    }

    fn admit(self: &Arc<Self>) -> Option<Permit> {
        let admitted = &self.counters.admitted;
        let prev = admitted.fetch_add(1, Ordering::SeqCst);
        if prev >= self.limits.max_sockets {
            admitted.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Permit(self.clone()))
    }

    fn online(&self) -> usize {
        lock(&self.peers).len()
    }

    fn publish(&self, peer: Arc<Peer>, at: u64) {
        let id = peer.id;
        lock(&self.peers).insert(id, peer);
        lock(&self.stats).join(id, at);
    }

    /// Enqueue into every other peer's outbox (and the sender's too if `include_self`).
    /// Never waits for space and never touches a socket.
    fn fan_out(&self, from: u64, kind: Kind, text: Arc<str>, include_self: bool) {
        // Snapshot (at most max_sockets handles), then enqueue one outbox at a time.
        let targets: Vec<Arc<Peer>> =
            lock(&self.peers).values().filter(|p| include_self || p.id != from).cloned().collect();
        for p in targets {
            let pushed = match kind {
                Kind::Pose => p.outbox.push_pose(from, text.clone()),
                Kind::Event => p.outbox.push_event(text.clone()),
                Kind::Leave => p.outbox.push_leave(from, text.clone()),
            };
            match pushed {
                Push::Replaced => {
                    self.counters.coalesced.fetch_add(1, Ordering::Relaxed);
                }
                // The outbox already closed itself; interrupt its worker (no locks held here).
                Push::Overflow => p.request_close(CloseReason::OutboxFull),
                Push::Queued | Push::Closed => {}
            }
            self.counters.high_water.fetch_max(p.outbox.high_water(), Ordering::Relaxed);
        }
    }

    /// The single exit path for a published peer, run only by its own worker. Returns the
    /// reason actually recorded (an earlier `outbox_full` from another thread wins).
    fn cleanup(&self, peer: &Peer, reason: CloseReason, at: u64) -> CloseReason {
        peer.request_close(reason);
        let reason = peer.outbox.closing().unwrap_or(reason);
        let removed = lock(&self.peers).remove(&peer.id).is_some();
        if removed {
            lock(&self.stats).leave(peer.id, at);
            let leave: Arc<str> = Arc::from(json!({"t": "leave", "id": peer.id}).to_string());
            self.fan_out(peer.id, Kind::Leave, leave, false);
            let i = CloseReason::ALL.iter().position(|r| *r == reason).unwrap_or(0);
            self.counters.disconnects[i].fetch_add(1, Ordering::Relaxed);
        }
        reason
    }

    /// Additive `relay` object for `/stats.json`.
    fn relay_json(&self) -> Value {
        let peers: Vec<Arc<Peer>> = lock(&self.peers).values().cloned().collect();
        let pending: usize = peers.iter().map(|p| p.outbox.len()).sum();
        let c = &self.counters;
        let disconnects: serde_json::Map<String, Value> = CloseReason::ALL
            .iter()
            .enumerate()
            .map(|(i, r)| (r.as_str().to_string(), json!(c.disconnects[i].load(Ordering::Relaxed))))
            .collect();
        json!({
            "admitted": c.admitted.load(Ordering::Relaxed),
            "active": peers.len(),
            "pending": pending,
            "high_water": c.high_water.load(Ordering::Relaxed),
            "coalesced": c.coalesced.load(Ordering::Relaxed),
            "rejected": c.rejected.load(Ordering::Relaxed),
            "handshake_failures": c.handshake_failures.load(Ordering::Relaxed),
            "disconnects": disconnects,
            "limits": {"max_sockets": self.limits.max_sockets, "outbox_capacity": self.limits.outbox_capacity},
        })
    }
}

type Ws = WebSocket<Guarded<TcpStream, RealClock>>;

fn send_reason(err: &tungstenite::Error, ws: &Ws) -> CloseReason {
    match classify(err, ws.get_ref().write_failed()) {
        Next::Close(r) => r,
        // A send that can't complete is a stalled write, never a read poll.
        Next::Poll => CloseReason::WriteTimeout,
    }
}

/// Handle one text message from `id`. Poses and GAME OVER fan out; JSON pings echo.
fn handle(ws: &mut Ws, id: u64, text: &str, room: &Room) -> Result<(), CloseReason> {
    let Ok(mut v) = serde_json::from_str::<Value>(text) else { return Ok(()) };
    let kind = v["t"].as_str().unwrap_or("").to_string();
    match kind.as_str() {
        "ping" => {
            ws.get_mut().arm();
            ws.send(Message::text(text)).map_err(|e| send_reason(&e, ws))
        }
        "s" | "over" => {
            if kind == "over" {
                lock(&room.stats).over(id, v["by"].as_str().unwrap_or("someone"), now());
            } else if v["m"].as_bool() != Some(true) {
                // Presence from a player browsing the map is forwarded but isn't a truck.
                let speed = v["v"].as_array().map_or(0.0, |a| a.iter().filter_map(Value::as_f64).map(|c| c * c).sum::<f64>().sqrt());
                lock(&room.stats).state(
                    id,
                    v["name"].as_str().unwrap_or(""),
                    v["x"].as_f64().unwrap_or(0.0),
                    v["y"].as_f64().unwrap_or(0.0),
                    speed as f32,
                    v["b"].as_bool().unwrap_or(false),
                    now(),
                );
            }
            v["id"] = json!(id);
            let out = v.to_string();
            if out.len() > room.limits.max_message {
                return Ok(()); // the rewritten message would exceed the outbound limit: drop it
            }
            let (k, include_self) = if kind == "s" { (Kind::Pose, false) } else { (Kind::Event, true) };
            room.fan_out(id, k, Arc::from(out), include_self);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Alternate a bounded outbound slice with one input poll until the peer must close.
fn worker_loop(ws: &mut Ws, peer: &Peer, room: &Room) -> CloseReason {
    let l = &room.limits;
    let mut bucket = TokenBucket::new(l.msg_rate, l.msg_burst, Instant::now());
    let mut last_msg = Instant::now();
    loop {
        if let Some(r) = peer.outbox.closing() {
            return r;
        }
        if last_msg.elapsed() > l.idle_timeout {
            return CloseReason::Idle;
        }
        let slice = Instant::now();
        for _ in 0..l.drain_max_msgs {
            if slice.elapsed() >= l.drain_max_time {
                break;
            }
            let Some(out) = peer.outbox.pop() else { break };
            ws.get_mut().arm();
            if let Err(e) = ws.send(Message::text(out.text().to_string())) {
                return send_reason(&e, ws);
            }
        }
        ws.get_mut().arm();
        match ws.read() {
            Ok(msg @ (Message::Text(_) | Message::Binary(_))) => {
                let t = Instant::now();
                last_msg = t;
                if !bucket.take(t) {
                    return CloseReason::RateLimit;
                }
                if let Message::Text(text) = msg {
                    if let Err(r) = handle(ws, peer.id, text.as_str(), room) {
                        return r;
                    }
                }
            }
            Ok(Message::Close(_)) => return CloseReason::ClientClose,
            Ok(_) => {} // Ping/Pong: Tungstenite answers on the next (deadline-bound) call
            Err(e) => match classify(&e, ws.get_ref().write_failed()) {
                Next::Poll => {}
                Next::Close(r) => return r,
            },
        }
    }
}

fn serve(stream: TcpStream, id: u64, room: Arc<Room>, _permit: Permit) {
    let l = &room.limits;
    let _ = stream.set_nodelay(true);
    if let Err(e) = transport::limit_send_buffer(&stream, l.send_buffer) {
        eprintln!("x {id} socket limits: {e}");
        return;
    }
    let Ok(shutdown) = stream.try_clone() else {
        eprintln!("x {id} cannot clone socket");
        return;
    };
    let guarded = Guarded::new(stream, RealClock, l);
    let cancel = guarded.cancel_flag();
    let mut ws = match transport::handshake(guarded, l) {
        Ok(ws) => ws,
        Err(e) => {
            room.counters.handshake_failures.fetch_add(1, Ordering::Relaxed);
            eprintln!("x {id} {e}");
            return;
        }
    };
    // Welcome first; a peer that can't take it is never published (no join/leave events).
    ws.get_mut().arm();
    if let Err(e) = ws.send(Message::text(json!({"t": "welcome", "id": id}).to_string())) {
        eprintln!("x {id} welcome: {}", send_reason(&e, &ws).as_str());
        return;
    }
    let peer = Arc::new(Peer { id, outbox: Outbox::new(l.outbox_capacity), cancel, shutdown: Some(shutdown) });
    room.publish(peer.clone(), now());
    eprintln!("+ {id} ({} online)", room.online());
    let reason = worker_loop(&mut ws, &peer, &room);
    let reason = room.cleanup(&peer, reason, now());
    eprintln!("- {id} {} ({} online)", reason.as_str(), room.online());
}

fn stats_server(port: String, room: Arc<Room>) {
    let Ok(server) = tiny_http::Server::http(format!("127.0.0.1:{port}")) else {
        eprintln!("stats: cannot bind 127.0.0.1:{port}");
        return;
    };
    eprintln!("stats on 127.0.0.1:{port}");
    for req in server.incoming_requests() {
        let (body, ctype) = match req.url() {
            "/stats" | "/stats/" => (lock(&room.stats).html(now()), "text/html; charset=utf-8"),
            "/stats.json" => {
                let base = lock(&room.stats).json(now());
                let mut v: Value = serde_json::from_str(&base).unwrap_or_else(|_| json!({}));
                v["relay"] = room.relay_json();
                (v.to_string(), "application/json")
            }
            _ => {
                let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                continue;
            }
        };
        let resp = tiny_http::Response::from_string(body)
            .with_header(tiny_http::Header::from_bytes("Content-Type", ctype).unwrap())
            .with_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
        let _ = req.respond(resp);
    }
}

fn main() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8795".into());
    let stats_port = std::env::var("STATS_PORT").unwrap_or_else(|_| "8796".into());
    let log = PathBuf::from(std::env::var("RELAY_LOG").unwrap_or_else(|_| "players.jsonl".into()));
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    eprintln!("vr_fire relay on 127.0.0.1:{port}, log {}", log.display());
    let room = Arc::new(Room::new(Limits::default(), Stats::new(Some(&log))));
    {
        let room = room.clone();
        thread::spawn(move || stats_server(stats_port, room));
    }
    for (n, stream) in listener.incoming().enumerate() {
        let Ok(stream) = stream else { continue };
        let Some(permit) = room.admit() else {
            // Full: close before any WebSocket admission; viewers retry after 5 s.
            room.counters.rejected.fetch_add(1, Ordering::Relaxed);
            drop(stream);
            continue;
        };
        let id = n as u64 + 1;
        let r = room.clone();
        // On spawn failure the closure (and its permit) is dropped, freeing the slot.
        if let Err(e) = thread::Builder::new().name(format!("peer-{id}")).stack_size(512 * 1024).spawn(move || serve(stream, id, r, permit)) {
            eprintln!("x {id} spawn: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(capacity: usize) -> Arc<Room> {
        let limits = Limits { outbox_capacity: capacity, max_sockets: 3, ..Limits::default() };
        Arc::new(Room::new(limits, Stats::new(None)))
    }

    fn peer(r: &Room, id: u64) -> Arc<Peer> {
        let p = Arc::new(Peer { id, outbox: Outbox::new(r.limits.outbox_capacity), cancel: Arc::default(), shutdown: None });
        r.publish(p.clone(), 0);
        p
    }

    fn texts(p: &Peer) -> Vec<String> {
        std::iter::from_fn(|| p.outbox.pop()).map(|o| o.text().to_string()).collect()
    }

    #[test]
    fn admission_is_capped_and_every_permit_returns_its_slot() {
        let r = room(8);
        let permits: Vec<_> = (0..3).map(|_| r.admit().expect("slot")).collect();
        assert!(r.admit().is_none(), "4th socket rejected");
        drop(permits);
        assert_eq!(r.counters.admitted.load(Ordering::SeqCst), 0);
        // A failed spawn drops its closure, and with it the permit.
        let p = r.admit().unwrap();
        let closure = move || drop(p);
        drop(closure);
        assert_eq!(r.counters.admitted.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_overflowing_recipient_is_cancelled_and_others_keep_receiving() {
        let r = room(4);
        let (sender, slow, healthy) = (peer(&r, 1), peer(&r, 2), peer(&r, 3));
        for i in 0..10 {
            r.fan_out(sender.id, Kind::Event, Arc::from(format!("e{i}")), false);
            healthy.outbox.pop(); // the healthy peer keeps draining
        }
        assert_eq!(slow.outbox.closing(), Some(CloseReason::OutboxFull));
        assert!(slow.cancel.load(Ordering::SeqCst));
        assert_eq!(slow.outbox.len(), 0);
        assert_eq!(healthy.outbox.closing(), None);
        assert!(r.counters.high_water.load(Ordering::SeqCst) <= 4);
    }

    #[test]
    fn poses_coalesce_per_sender_across_the_room() {
        let r = room(64);
        let (a, b) = (peer(&r, 1), peer(&r, 2));
        for i in 0..1000 {
            r.fan_out(a.id, Kind::Pose, Arc::from(format!("p{i}")), false);
        }
        assert_eq!(texts(&b), ["p999"]);
        assert_eq!(a.outbox.len(), 0, "poses don't echo to the sender");
        assert_eq!(r.counters.coalesced.load(Ordering::SeqCst), 999);
    }

    #[test]
    fn over_reaches_the_sender_too() {
        let r = room(64);
        let (a, b) = (peer(&r, 1), peer(&r, 2));
        r.fan_out(a.id, Kind::Event, Arc::from("over"), true);
        assert_eq!(texts(&a), ["over"]);
        assert_eq!(texts(&b), ["over"]);
    }

    #[test]
    fn cleanup_removes_once_leaves_once_and_purges_the_pending_pose() {
        let r = room(64);
        let (a, b) = (peer(&r, 1), peer(&r, 2));
        r.fan_out(a.id, Kind::Pose, Arc::from("pose"), false);
        // Other threads may request cancellation concurrently; only the owner cleans up.
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let a = a.clone();
                thread::spawn(move || a.request_close(CloseReason::OutboxFull))
            })
            .collect();
        let first = r.cleanup(&a, CloseReason::ReadError, 5);
        for t in threads {
            t.join().unwrap();
        }
        let again = r.cleanup(&a, CloseReason::ReadError, 6);
        assert_eq!(first, again);
        assert_eq!(r.online(), 1);
        let leave = json!({"t": "leave", "id": 1}).to_string();
        assert_eq!(texts(&b), [leave], "one leave, and the stale pose is gone");
        let stats = lock(&r.stats);
        assert_eq!(stats.events.iter().filter(|e| e.kind == "leave").count(), 1);
        let recorded: u64 = r.counters.disconnects.iter().map(|c| c.load(Ordering::SeqCst)).sum();
        assert_eq!(recorded, 1, "one disconnect counted");
    }

    #[test]
    fn a_leave_that_overflows_a_slow_peer_cancels_it_without_recursion() {
        let r = room(1);
        let (a, slow) = (peer(&r, 1), peer(&r, 2));
        r.fan_out(99, Kind::Event, Arc::from("fill"), false);
        r.cleanup(&a, CloseReason::ClientClose, 1);
        assert_eq!(slow.outbox.closing(), Some(CloseReason::OutboxFull));
        assert_eq!(r.online(), 1, "the slow peer's own worker removes it later");
    }
}
