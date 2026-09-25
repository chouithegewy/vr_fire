//! vr_fire multiplayer relay: assigns each WebSocket client an id and re-broadcasts its
//! messages (tagged with that id) to everyone else. Game state lives in the clients.
//!
//! Also logs player activity (joins, drops, flips, leaves) to `$RELAY_LOG` and serves a
//! stats page. nginx terminates TLS:
//!   /vr_fire/ws          → 127.0.0.1:$PORT        (default 8795, WebSocket)
//!   /vr_fire/stats[.json] → 127.0.0.1:$STATS_PORT (default 8796, HTML / JSON)

mod limits;
mod outbox;
mod stats;
mod transport;

use serde_json::{Value, json};
use stats::Stats;
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tungstenite::{Message, accept};

type Peers = Arc<Mutex<HashMap<u64, Sender<String>>>>;
type Shared = Arc<Mutex<Stats>>;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn broadcast(peers: &Peers, from: u64, text: &str, include_self: bool) {
    for (id, tx) in peers.lock().unwrap().iter() {
        if include_self || *id != from {
            let _ = tx.send(text.to_string());
        }
    }
}

fn serve(stream: TcpStream, id: u64, peers: Peers, stats: Shared) {
    let _ = stream.set_nodelay(true);
    let Ok(mut ws) = accept(stream) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(20)));
    let (tx, rx) = channel::<String>();
    peers.lock().unwrap().insert(id, tx);
    stats.lock().unwrap().join(id, now());
    let _ = ws.send(Message::text(json!({"t": "welcome", "id": id}).to_string()));
    eprintln!("+ {id} ({} online)", peers.lock().unwrap().len());
    let mut last_msg = std::time::Instant::now();
    loop {
        for out in rx.try_iter() {
            if ws.send(Message::text(out)).is_err() {
                break;
            }
        }
        match ws.read() {
            Ok(Message::Text(t)) if t.len() < 4096 => {
                last_msg = std::time::Instant::now();
                if let Ok(mut v) = serde_json::from_str::<Value>(&t) {
                    let kind = v["t"].as_str().unwrap_or("").to_string();
                    if kind == "ping" {
                        // Echo to the sender only, for round-trip time.
                        let _ = ws.send(Message::text(t.to_string()));
                        continue;
                    }
                    if kind != "s" && kind != "over" {
                        continue;
                    }
                    if kind == "s" && v["m"].as_bool() == Some(true) {
                        // Presence from a player browsing the map: forward, but it's not a truck.
                    } else if kind == "s" {
                        let speed = v["v"].as_array().map_or(0.0, |a| a.iter().filter_map(Value::as_f64).map(|c| c * c).sum::<f64>().sqrt());
                        stats.lock().unwrap().state(
                            id,
                            v["name"].as_str().unwrap_or(""),
                            v["x"].as_f64().unwrap_or(0.0),
                            v["y"].as_f64().unwrap_or(0.0),
                            speed as f32,
                            v["b"].as_bool().unwrap_or(false),
                            now(),
                        );
                    } else {
                        stats.lock().unwrap().over(id, v["by"].as_str().unwrap_or("someone"), now());
                    }
                    v["id"] = json!(id);
                    broadcast(&peers, id, &v.to_string(), kind == "over");
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                if last_msg.elapsed() > Duration::from_secs(60) {
                    break; // silent client
                }
            }
            Err(_) => break,
        }
    }
    peers.lock().unwrap().remove(&id);
    stats.lock().unwrap().leave(id, now());
    broadcast(&peers, id, &json!({"t": "leave", "id": id}).to_string(), false);
    eprintln!("- {id} ({} online)", peers.lock().unwrap().len());
}

fn stats_server(port: String, stats: Shared) {
    let Ok(server) = tiny_http::Server::http(format!("127.0.0.1:{port}")) else {
        eprintln!("stats: cannot bind 127.0.0.1:{port}");
        return;
    };
    eprintln!("stats on 127.0.0.1:{port}");
    for req in server.incoming_requests() {
        let (body, ctype) = match req.url() {
            "/stats" | "/stats/" => (stats.lock().unwrap().html(now()), "text/html; charset=utf-8"),
            "/stats.json" => (stats.lock().unwrap().json(now()), "application/json"),
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
    let peers: Peers = Arc::default();
    let stats: Shared = Arc::new(Mutex::new(Stats::new(Some(&log))));
    {
        let stats = stats.clone();
        thread::spawn(move || stats_server(stats_port, stats));
    }
    for (n, stream) in listener.incoming().enumerate() {
        let Ok(stream) = stream else { continue };
        let (peers, stats) = (peers.clone(), stats.clone());
        let id = n as u64 + 1;
        thread::spawn(move || serve(stream, id, peers, stats));
    }
}
