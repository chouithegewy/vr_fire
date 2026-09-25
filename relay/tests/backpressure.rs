//! F01 end-to-end checks against the real relay binary on loopback: a stalled reader is removed
//! without hurting a healthy peer, floods are cut off, admission is capped, and the existing
//! protocol (welcome, ping echo, poses, over, leave) still works.

use serde_json::{Value, json};
use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Client = WebSocket<MaybeTlsStream<TcpStream>>;

struct Relay {
    child: Child,
    port: u16,
    stats_port: u16,
    _dir: std::path::PathBuf,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_relay() -> Relay {
    let (port, stats_port) = (free_port(), free_port());
    let dir = std::env::temp_dir().join(format!("vr_fire_relay_test_{port}"));
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_relay"))
        .env("PORT", port.to_string())
        .env("STATS_PORT", stats_port.to_string())
        .env("RELAY_LOG", dir.join("players.jsonl"))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "relay didn't start");
        thread::sleep(Duration::from_millis(20));
    }
    Relay { child, port, stats_port, _dir: dir }
}

fn stats_json(r: &Relay) -> Value {
    let mut s = TcpStream::connect(("127.0.0.1", r.stats_port)).unwrap();
    use std::io::Write;
    write!(s, "GET /stats.json HTTP/1.0\r\nHost: x\r\n\r\n").unwrap();
    let mut body = String::new();
    s.read_to_string(&mut body).unwrap();
    serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

/// Connect and return (client, assigned id).
fn join(r: &Relay) -> (Client, u64) {
    let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}/vr_fire/ws", r.port)).unwrap();
    let id = loop {
        let v: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        if v["t"] == "welcome" {
            break v["id"].as_u64().unwrap();
        }
    };
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    }
    (ws, id)
}

fn pose(x: f64, pad: usize) -> String {
    json!({"t": "s", "name": "p".repeat(pad.max(1)), "x": x, "y": 1_900_000.0, "h": 500.0,
           "q": [0, 0, 0, 1], "v": [30, 0, 0], "b": false})
    .to_string()
}

/// Next JSON message, or None on a read poll timeout.
fn next(ws: &mut Client) -> Option<Value> {
    match ws.read() {
        Ok(Message::Text(t)) => serde_json::from_str(t.as_str()).ok(),
        Ok(_) => None,
        Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
        Err(e) => panic!("client error: {e}"),
    }
}

fn shrink_receive_buffer(ws: &Client) {
    use std::os::fd::AsRawFd;
    let MaybeTlsStream::Plain(s) = ws.get_ref() else { panic!("plain socket") };
    let v: libc::c_int = 4096;
    // SAFETY: valid fd and a c_int option value.
    let r = unsafe {
        libc::setsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, (&v as *const libc::c_int).cast(), 4)
    };
    assert_eq!(r, 0);
}

#[test]
fn a_stalled_reader_is_removed_while_a_healthy_peer_keeps_advancing() {
    let relay = start_relay();
    let (mut producer, _) = join(&relay);
    let (mut healthy, _) = join(&relay);
    let (stalled, stalled_id) = join(&relay);
    shrink_receive_buffer(&stalled);

    let stop = Arc::new(AtomicBool::new(false));
    let prod = {
        let stop = stop.clone();
        thread::spawn(move || {
            let mut x = 0.0;
            let mut last_ping = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                x += 1.0;
                producer.send(Message::text(pose(x, 3500))).unwrap();
                if last_ping.elapsed() >= Duration::from_secs(1) {
                    producer.send(Message::text(json!({"t": "ping", "c": 1.0}).to_string())).unwrap();
                    last_ping = Instant::now();
                }
                // Drain our own echoes so the producer is never the slow one.
                let t = Instant::now();
                while t.elapsed() < Duration::from_millis(50) {
                    let _ = next(&mut producer);
                }
            }
        })
    };

    let seen = Arc::new(Mutex::new((0.0f64, 0usize)));
    let t0 = Instant::now();
    let mut left_at = None;
    let mut pings_ok = 0;
    let mut last_ping = Instant::now();
    while t0.elapsed() < Duration::from_secs(40) {
        if last_ping.elapsed() >= Duration::from_secs(1) {
            healthy.send(Message::text(json!({"t": "ping", "c": 2.0}).to_string())).unwrap();
            last_ping = Instant::now();
        }
        match next(&mut healthy) {
            Some(v) if v["t"] == "s" => {
                let mut s = seen.lock().unwrap();
                let x = v["x"].as_f64().unwrap();
                assert!(x > s.0, "poses advance");
                *s = (x, s.1 + 1);
            }
            Some(v) if v["t"] == "ping" => pings_ok += 1,
            Some(v) if v["t"] == "leave" => {
                assert_eq!(v["id"].as_u64(), Some(stalled_id), "only the stalled peer leaves");
                left_at.get_or_insert(t0.elapsed());
            }
            _ => {}
        }
        if let Some(at) = left_at {
            if t0.elapsed() > at + Duration::from_secs(3) {
                break; // keep observing briefly after the leave
            }
        }
    }
    // Read the relay's gauges while producer and healthy peer are still connected.
    let stats = stats_json(&relay);
    stop.store(true, Ordering::SeqCst);
    prod.join().unwrap();

    let left_at = left_at.expect("the stalled peer was removed");
    let (x, n) = *seen.lock().unwrap();
    assert!(n > 20 && x > 20.0, "healthy peer kept receiving ({n} poses)");
    assert!(pings_ok >= 2, "healthy peer's pings echoed");
    let d = &stats["relay"]["disconnects"];
    let stall = d["write_timeout"].as_u64().unwrap() + d["outbox_full"].as_u64().unwrap() + d["write_error"].as_u64().unwrap();
    assert_eq!(stall, 1, "one stall disconnect: {d}");
    assert!(stats["relay"]["high_water"].as_u64().unwrap() <= 64);
    assert_eq!(stats["relay"]["active"].as_u64(), Some(2));
    eprintln!("stalled peer removed after {left_at:?}; healthy saw {n} poses; relay {}", stats["relay"]);
    drop(stalled);
}

#[test]
fn a_flooding_sender_is_disconnected_and_others_stay() {
    let relay = start_relay();
    let (mut flooder, flooder_id) = join(&relay);
    let (mut healthy, _) = join(&relay);
    for i in 0..200 {
        if flooder.send(Message::text(pose(i as f64, 1))).is_err() {
            break;
        }
    }
    let t0 = Instant::now();
    let mut left = false;
    while t0.elapsed() < Duration::from_secs(5) && !left {
        if let Some(v) = next(&mut healthy) {
            left = v["t"] == "leave" && v["id"].as_u64() == Some(flooder_id);
        }
    }
    assert!(left, "flooder removed");
    assert_eq!(stats_json(&relay)["relay"]["disconnects"]["rate_limit"].as_u64(), Some(1));
    healthy.send(Message::text(json!({"t": "ping", "c": 3.0}).to_string())).unwrap();
    let t0 = Instant::now();
    let mut echoed = false;
    while t0.elapsed() < Duration::from_secs(2) && !echoed {
        echoed = next(&mut healthy).is_some_and(|v| v["t"] == "ping");
    }
    assert!(echoed, "healthy peer still served");
}

#[test]
fn admission_is_capped_at_32_and_slots_return() {
    let relay = start_relay();
    let mut clients: Vec<Client> = (0..32).map(|_| join(&relay).0).collect();
    assert!(
        tungstenite::connect(format!("ws://127.0.0.1:{}/vr_fire/ws", relay.port)).is_err(),
        "33rd connection is refused before the WebSocket handshake"
    );
    let _ = clients.pop().unwrap().close(None);
    let t0 = Instant::now();
    loop {
        if tungstenite::connect(format!("ws://127.0.0.1:{}/vr_fire/ws", relay.port)).is_ok() {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "slot returned after a close");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn protocol_is_unchanged_for_viewers() {
    let relay = start_relay();
    let (mut a, a_id) = join(&relay);
    let (mut b, _) = join(&relay);
    a.send(Message::text(json!({"t": "ping", "c": 42.5}).to_string())).unwrap();
    a.send(Message::text(pose(7.0, 8))).unwrap();
    a.send(Message::text(json!({"t": "over", "by": "trucker-1"}).to_string())).unwrap();
    let collect = |ws: &mut Client| {
        let mut got = Vec::new();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(800) {
            if let Some(v) = next(ws) {
                got.push(v);
            }
        }
        got
    };
    let got_a = collect(&mut a);
    let got_b = collect(&mut b);
    assert!(got_a.iter().any(|v| v["t"] == "ping" && v["c"] == 42.5), "ping echoed to sender");
    assert!(!got_b.iter().any(|v| v["t"] == "ping"), "ping not broadcast");
    assert!(got_b.iter().any(|v| v["t"] == "s" && v["id"].as_u64() == Some(a_id) && v["x"] == 7.0), "pose tagged with sender id");
    assert!(!got_a.iter().any(|v| v["t"] == "s"), "pose not echoed");
    assert!(got_a.iter().any(|v| v["t"] == "over") && got_b.iter().any(|v| v["t"] == "over"), "over reaches everyone");
    let _ = a.close(None);
    let t0 = Instant::now();
    let mut left = false;
    while t0.elapsed() < Duration::from_secs(3) && !left {
        left = next(&mut b).is_some_and(|v| v["t"] == "leave" && v["id"].as_u64() == Some(a_id));
    }
    assert!(left, "leave after a normal close");
}
