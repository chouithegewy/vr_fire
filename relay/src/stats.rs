//! Player activity log and the public stats page.
//!
//! Events (join, drop, flipped, leave) are kept in memory for the page and appended as JSON
//! lines to a log file. No IP addresses are collected; names are the client's random
//! `trucker-NNNN` (sanitised and HTML-escaped), locations are rounded to 0.01° (~1 km).

use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use vr_fire::crs::Albers;

const RECENT: usize = 60;
/// A jump bigger than this between two pose updates is a new drop (clicked elsewhere / R).
const DROP_JUMP_M: f64 = 1_500.0;

pub struct Event {
    pub at: u64,
    pub kind: &'static str,
    pub text: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

pub struct Player {
    pub name: String,
    pub joined: u64,
    pub last_seen: u64,
    pub last_xy: Option<(f64, f64)>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub speed_kmh: f32,
    pub boosting: bool,
    pub updates: u64,
    pub drops: u32,
}

pub struct Stats {
    albers: Albers,
    pub players: HashMap<u64, Player>,
    pub events: VecDeque<Event>,
    pub total_sessions: u64,
    pub peak_online: usize,
    pub names: HashSet<String>,
    pub total_drops: u64,
    pub total_flips: u64,
    log: Option<File>,
}

fn clean_name(raw: &str) -> String {
    let s: String = raw.chars().filter(|c| !c.is_control()).take(24).collect();
    if s.trim().is_empty() { "anonymous".into() } else { s }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn ago(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{:.1} h", secs as f64 / 3600.0),
        _ => format!("{:.1} days", secs as f64 / 86_400.0),
    }
}

impl Stats {
    /// Load history from `log_path` (JSON lines) if given, and keep appending to it.
    pub fn new(log_path: Option<&Path>) -> Self {
        let mut s = Self {
            albers: Albers::new().expect("EPSG:5070"),
            players: HashMap::new(),
            events: VecDeque::new(),
            total_sessions: 0,
            peak_online: 0,
            names: HashSet::new(),
            total_drops: 0,
            total_flips: 0,
            log: None,
        };
        if let Some(p) = log_path {
            if let Ok(f) = File::open(p) {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                    let kind: &'static str = match v["kind"].as_str() {
                        Some("join") => "join",
                        Some("drop") => "drop",
                        Some("flipped") => "flipped",
                        Some("leave") => "leave",
                        _ => continue,
                    };
                    match kind {
                        "join" => s.total_sessions += 1,
                        "drop" => s.total_drops += 1,
                        "flipped" => s.total_flips += 1,
                        _ => {}
                    }
                    if let Some(n) = v["name"].as_str() {
                        s.names.insert(n.to_string());
                    }
                    s.peak_online = s.peak_online.max(v["online"].as_u64().unwrap_or(0) as usize);
                    s.push_event(Event {
                        at: v["t"].as_u64().unwrap_or(0),
                        kind,
                        text: v["text"].as_str().unwrap_or("").to_string(),
                        lat: v["lat"].as_f64(),
                        lon: v["lon"].as_f64(),
                    });
                }
            }
            s.log = std::fs::OpenOptions::new().create(true).append(true).open(p).ok();
        }
        s
    }

    fn push_event(&mut self, e: Event) {
        self.events.push_back(e);
        while self.events.len() > RECENT {
            self.events.pop_front();
        }
    }

    fn record(&mut self, at: u64, kind: &'static str, id: u64, text: String, ll: Option<(f64, f64)>) {
        let name = self.players.get(&id).map(|p| p.name.clone());
        if let Some(f) = &mut self.log {
            let line = json!({"t": at, "kind": kind, "id": id, "name": name, "text": text,
                "lat": ll.map(|v| v.1), "lon": ll.map(|v| v.0), "online": self.players.len()});
            let _ = writeln!(f, "{line}");
        }
        self.push_event(Event { at, kind, text, lat: ll.map(|v| v.1), lon: ll.map(|v| v.0) });
    }

    pub fn join(&mut self, id: u64, now: u64) {
        self.players.insert(
            id,
            Player { name: format!("player {id}"), joined: now, last_seen: now, last_xy: None, lat: None, lon: None, speed_kmh: 0.0, boosting: false, updates: 0, drops: 0 },
        );
        self.total_sessions += 1;
        self.peak_online = self.peak_online.max(self.players.len());
        self.record(now, "join", id, format!("player {id} connected"), None);
    }

    /// A pose update: EPSG:5070 (x, y) and speed (m/s).
    #[allow(clippy::too_many_arguments)]
    pub fn state(&mut self, id: u64, name: &str, x: f64, y: f64, speed: f32, boosting: bool, now: u64) {
        let name = clean_name(name);
        let ll = self.albers.to_lonlat(x, y).ok().map(|(lon, lat)| (round2(lon), round2(lat)));
        let Some(p) = self.players.get_mut(&id) else { return };
        let dropped = p.last_xy.is_none_or(|(px, py)| ((x - px).powi(2) + (y - py).powi(2)).sqrt() > DROP_JUMP_M);
        p.name = name.clone();
        p.last_xy = Some((x, y));
        p.last_seen = now;
        p.updates += 1;
        p.speed_kmh = speed * 3.6;
        p.boosting = boosting;
        if let Some((lon, lat)) = ll {
            (p.lon, p.lat) = (Some(lon), Some(lat));
        }
        self.names.insert(name.clone());
        if dropped {
            p.drops += 1;
            self.total_drops += 1;
            let at = ll.map_or(String::new(), |(lon, lat)| format!(" at {lat:.2}, {lon:.2}"));
            self.record(now, "drop", id, format!("{name} dropped a truck{at}"), ll);
        }
    }

    /// `id` reports being flipped over by `by`.
    pub fn over(&mut self, id: u64, by: &str, now: u64) {
        let victim = self.players.get(&id).map_or(format!("player {id}"), |p| p.name.clone());
        let ll = self.players.get(&id).and_then(|p| Some((p.lon?, p.lat?)));
        self.total_flips += 1;
        self.record(now, "flipped", id, format!("{} flipped {victim} - GAME OVER", clean_name(by)), ll);
    }

    pub fn leave(&mut self, id: u64, now: u64) {
        let Some(p) = self.players.get(&id) else { return };
        let text = format!("{} left after {} ({} drops, {} updates)", p.name, ago(now.saturating_sub(p.joined)), p.drops, p.updates);
        self.record(now, "leave", id, text, None);
        self.players.remove(&id);
    }

    pub fn json(&self, now: u64) -> String {
        let online: Vec<_> = self
            .players
            .iter()
            .map(|(id, p)| {
                json!({"id": id, "name": p.name, "online_for_s": now.saturating_sub(p.joined), "lat": p.lat, "lon": p.lon,
                    "speed_kmh": p.speed_kmh.round(), "boosting": p.boosting, "drops": p.drops})
            })
            .collect();
        let events: Vec<_> = self.events.iter().rev().map(|e| json!({"t": e.at, "kind": e.kind, "text": e.text, "lat": e.lat, "lon": e.lon})).collect();
        json!({"online": online, "recent": events, "totals": {"sessions": self.total_sessions, "unique_names": self.names.len(),
            "peak_online": self.peak_online, "drops": self.total_drops, "flips": self.total_flips}})
        .to_string()
    }

    pub fn html(&self, now: u64) -> String {
        let mut online: Vec<_> = self.players.values().collect();
        online.sort_by_key(|p| p.joined);
        let rows: String = if online.is_empty() {
            "<p class=dim>Nobody is driving right now.</p>".into()
        } else {
            let r: String = online
                .iter()
                .map(|p| {
                    let place = match (p.lat, p.lon) {
                        (Some(la), Some(lo)) => format!("{la:.2}, {lo:.2}"),
                        _ => "on the map".into(),
                    };
                    format!(
                        "<tr><td>{}</td><td>{}</td><td>{place}</td><td>{:.0} km/h{}</td></tr>",
                        escape(&p.name),
                        ago(now.saturating_sub(p.joined)),
                        p.speed_kmh,
                        if p.boosting { " boost" } else { "" }
                    )
                })
                .collect();
            format!("<table><tr><th>player</th><th>online for</th><th>where</th><th>speed</th></tr>{r}</table>")
        };
        let events: String = self
            .events
            .iter()
            .rev()
            .map(|e| format!("<li><span class=dim>{} ago</span> {}</li>", ago(now.saturating_sub(e.at)), escape(&e.text)))
            .collect();
        format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="5"><title>vr_fire players</title><style>
:root{{--bg:#e4e2e7;--text:#57525e;--dim:#86808d;--rule:#d3cfd8;--purple:#7b5e9c}}
@media (prefers-color-scheme:dark){{:root{{--bg:#252228;--text:#a7a1ae;--dim:#7a7481;--rule:#3a3540;--purple:#a58bc4}}}}
html{{background:var(--bg)}}body{{color:var(--text);font:16px/1.6 Charter,"Iowan Old Style",Cambria,Georgia,serif;max-width:40rem;margin:0 auto;padding:8vh 1.5rem}}
h1{{font-size:1.3em;font-weight:400;color:var(--purple)}}h2{{font-size:1.05em;font-weight:400;margin-top:2rem}}a{{color:var(--purple)}}
table{{border-collapse:collapse;width:100%;margin-top:.5rem}}td,th{{text-align:left;padding:.2rem .6rem .2rem 0;border-bottom:1px solid var(--rule);font-weight:400}}
th{{color:var(--dim)}}ul{{list-style:none;padding:0}}li{{padding:.15rem 0}}.dim{{color:var(--dim)}}
</style></head><body>
<h1>vr_fire players</h1>
<p class=dim>{sessions} sessions, {unique} player names, peak {peak} online at once, {drops} trucks dropped, {flips} flips. Updates every 5 s. <a href="/vr_fire/">Play</a></p>
<h2>Online now ({n})</h2>{rows}
<h2>Recent</h2><ul>{events}</ul>
</body></html>"#,
            sessions = self.total_sessions,
            unique = self.names.len(),
            peak = self.peak_online,
            drops = self.total_drops,
            flips = self.total_flips,
            n = online.len(),
        )
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> Stats {
        Stats::new(None)
    }

    #[test]
    fn logs_join_drop_respawn_flip_and_leave() {
        let mut s = stats();
        let a = Albers::new().unwrap();
        let (x, y) = a.from_lonlat(-120.65, 38.45).unwrap();
        s.join(1, 100);
        s.state(1, "trucker-0001", x, y, 12.0, false, 101);
        s.state(1, "trucker-0001", x + 30.0, y, 12.0, false, 102); // driving: no event
        s.state(1, "trucker-0001", x + 50_000.0, y, 0.0, false, 103); // clicked elsewhere
        s.over(1, "trucker-0002", 104);
        s.leave(1, 400);
        let kinds: Vec<&str> = s.events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["join", "drop", "drop", "flipped", "leave"]);
        let drop = &s.events[1];
        assert!((drop.lat.unwrap() - 38.45).abs() < 0.01 && (drop.lon.unwrap() + 120.65).abs() < 0.01);
        assert!(s.events[3].text.contains("trucker-0002 flipped trucker-0001"));
        assert!(s.events[4].text.contains("5 min"));
        assert_eq!((s.total_sessions, s.peak_online, s.names.len()), (1, 1, 1));
    }

    #[test]
    fn names_are_sanitised() {
        let mut s = stats();
        s.join(7, 0);
        s.state(7, "<script>alert(1)</script>xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", 0.0, 0.0, 0.0, false, 1);
        let name = &s.players[&7].name;
        assert!(name.chars().count() <= 24);
        let html = s.html(2);
        assert!(!html.contains("<script>"), "names must be escaped");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn locations_are_rounded() {
        let mut s = stats();
        let a = Albers::new().unwrap();
        let (x, y) = a.from_lonlat(-120.654321, 38.456789).unwrap();
        s.join(1, 0);
        s.state(1, "t", x, y, 0.0, false, 1);
        let e = &s.events[1];
        assert_eq!((e.lat.unwrap() * 100.0).round() / 100.0, e.lat.unwrap());
    }

    #[test]
    fn json_lists_online_players_and_totals() {
        let mut s = stats();
        s.join(1, 0);
        s.join(2, 0);
        s.leave(2, 60);
        let v: serde_json::Value = serde_json::from_str(&s.json(60)).unwrap();
        assert_eq!(v["online"].as_array().unwrap().len(), 1);
        assert_eq!(v["totals"]["sessions"], 2);
        assert_eq!(v["totals"]["peak_online"], 2);
    }
}
