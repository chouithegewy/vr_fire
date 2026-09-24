//! vr_fire multiplayer relay: assigns each WebSocket client an id and re-broadcasts its
//! messages (tagged with that id) to everyone else. Game state lives in the clients.
//! Listens on 127.0.0.1:$PORT (default 8795); nginx terminates TLS at /vr_fire/ws.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tungstenite::{Message, accept};

type Peers = Arc<Mutex<HashMap<u64, Sender<String>>>>;

fn broadcast(peers: &Peers, from: u64, text: &str, include_self: bool) {
    for (id, tx) in peers.lock().unwrap().iter() {
        if include_self || *id != from {
            let _ = tx.send(text.to_string());
        }
    }
}

fn serve(stream: TcpStream, id: u64, peers: Peers) {
    let _ = stream.set_nodelay(true);
    let Ok(mut ws) = accept(stream) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(20)));
    let (tx, rx) = channel::<String>();
    peers.lock().unwrap().insert(id, tx);
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
    broadcast(&peers, id, &json!({"t": "leave", "id": id}).to_string(), false);
    eprintln!("- {id} ({} online)", peers.lock().unwrap().len());
}

fn main() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8795".into());
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    eprintln!("vr_fire relay on 127.0.0.1:{port}");
    let peers: Peers = Arc::default();
    for (n, stream) in listener.incoming().enumerate() {
        let Ok(stream) = stream else { continue };
        let peers = peers.clone();
        let id = n as u64 + 1;
        thread::spawn(move || serve(stream, id, peers));
    }
}
