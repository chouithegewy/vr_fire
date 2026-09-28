# Robot mode, dinosaur riding, first person and weapons — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a player transform the truck into an original walking robot, ride dinosaurs, play
in first person, and fight other players with a pistol and a scoped sniper, with health kept by
the relay.

**Architecture:** A new `viewer/src/player/` module owns the player's body (truck, robot,
riding), view (first/third person), weapons and health. Pure logic (robot poses and movement,
weapon state, ray tests) sits in small files with unit tests; Bevy glue wires input, camera,
models, HUD and networking. The relay gains `combat.rs`, which keeps health, validates `hit`
claims and announces damage, knockouts and respawns through the existing bounded fan-out.

**Tech Stack:** Rust, Bevy 0.19.1 (viewer, native + wasm), tungstenite relay, serde_json.

**Spec:** `docs/superpowers/specs/2026-09-27-robot-riding-weapons-design.md`

## Global Constraints

- Robot is original ("not Optimus Prime": no Hasbro name, look or logo).
- Keys: X transform, E mount/dismount, V view, Y cycle weapons (none → pistol → sniper), left
  mouse fire, right mouse aim/zoom, R reloads while a weapon is out and resets otherwise.
- Pistol: 20 dmg (40 head), 0.25 s interval, 300 m, 12 rounds, 1.5 s reload, zoom 1.3×.
- Sniper: 60 dmg (100 head), 1.5 s interval, 3,000 m, 5 rounds, 2.5 s reload, zoom 6×.
- Health 100; knockout at 0; respawn after 5 s, nearby, robot mode, full health.
- Only the robot (on foot or riding) can shoot; raptors can't be ridden.
- Relay accepts a hit only if: target connected, alive, not the shooter; shooter's last pose
  is robot/riding; weapon interval elapsed since the shooter's last accepted hit with that
  weapon; distance between last poses ≤ range × 1.1.
- Pose message gains optional `k` ("truck" | "robot" | "ride:<species>"), `w` ("none" |
  "pistol" | "sniper") and `hp`; old viewers keep working.
- Wire messages: `hit {target, weapon, head}` (client→relay), `dmg {target, by, hp, weapon,
  head}`, `ko {target, by, weapon}`, `respawn {target}` (relay→all; `respawn` is also sent by
  the victim's client when it respawns).
- Flip GAME OVER stays as it is, separate from health.

## Review Focus

1. **Knocked out while riding or mid-transform:** the player dismounts / finishes as a robot and
   respawns cleanly (Task 7 test `knockout_dismounts_and_blocks_input`).
2. **A hit on a player who just left, or a hit claim for yourself:** the relay rejects it and
   nothing breaks (Task 1 tests).
3. **Old viewers in the room:** a pose without `k`/`w`/`hp` still renders as a truck, and
   `dmg`/`ko` messages are ignored by them (Task 8 test `old_poses_default_to_truck`).
4. **Shooting through a hill:** the ray stops at terrain (Task 3 test).
5. **R with a weapon out never resets the robot, and R in the truck never reloads** (Task 5
   test `r_reloads_only_with_a_weapon_out`).

---

## File structure

| File | Responsibility |
|---|---|
| `relay/src/combat.rs` (new) | Health, hit validation, knockouts, respawns (pure, tested) |
| `relay/src/main.rs` | Route `hit` / `respawn` / pose kinds into `combat`; fan out results |
| `relay/tests/backpressure.rs` | New integration test: two clients fight to a knockout |
| `viewer/src/player/mod.rs` (new) | `Player` resource, modes, keys, plugin, respawn |
| `viewer/src/player/robot.rs` (new) | Robot parts with truck/robot poses, transform interpolation, kinematic movement |
| `viewer/src/player/weapons.rs` (new) | Weapon stats, magazine/reload/interval state machine |
| `viewer/src/player/ray.rs` (new) | Ray vs terrain, sphere, capsule, box |
| `viewer/src/player/combat.rs` (new) | Firing, hit resolution, effects, damage/KO handling, scope + HUD overlay |
| `viewer/src/dinos/brain.rs` | `State::Ridden` steering |
| `viewer/src/dinos/mod.rs` | Mount/dismount hooks, stand-in dinosaur model for remote riders |
| `viewer/src/net.rs` | New message kinds and pose fields; remote robot/rider visuals; hitboxes |
| `viewer/src/truck.rs` | Physics and reset skipped unless the player is in the truck |
| `viewer/src/main.rs` | Camera (first person, zoom), mouse look per mode, HUD lines, plugin |
| `viewer/README.md` | Controls and combat rules |

---

### Task 1: Relay combat rules

**Files:**
- Create: `relay/src/combat.rs`
- Test: in-file `#[cfg(test)]`

**Interfaces:**
- Produces:
  ```rust
  pub enum Weapon { Pistol, Sniper }            // Weapon::parse(&str) -> Option<Weapon>, .name()
  pub struct Combat { .. }                      // Combat::default()
  impl Combat {
      pub fn pose(&mut self, id: u64, x: f64, y: f64, h: f64, armed: bool);
      pub fn hit(&mut self, shooter: u64, target: u64, w: Weapon, head: bool, now: Instant) -> Result<Hit, Reject>;
      pub fn respawn(&mut self, id: u64) -> bool;   // true if it was knocked out
      pub fn leave(&mut self, id: u64);
      pub fn rejects(&self) -> &HashMap<&'static str, u64>;
  }
  pub enum Hit { Damage { hp: u32 }, Knockout }
  pub enum Reject { UnknownTarget, SelfHit, TargetDown, NotArmed, TooFast, OutOfRange }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn room() -> (Combat, Instant) {
        let mut c = Combat::default();
        c.pose(1, 0.0, 0.0, 100.0, true);   // shooter, robot
        c.pose(2, 100.0, 0.0, 100.0, true); // target 100 m east
        (c, Instant::now())
    }

    #[test]
    fn pistol_hits_take_20_and_headshots_40() {
        let (mut c, t) = room();
        assert_eq!(c.hit(1, 2, Weapon::Pistol, false, t), Ok(Hit::Damage { hp: 80 }));
        assert_eq!(c.hit(1, 2, Weapon::Pistol, true, t + Duration::from_millis(300)), Ok(Hit::Damage { hp: 40 }));
    }

    #[test]
    fn sniper_headshot_knocks_out_and_respawn_restores() {
        let (mut c, t) = room();
        assert_eq!(c.hit(1, 2, Weapon::Sniper, true, t), Ok(Hit::Knockout));
        assert_eq!(c.hit(1, 2, Weapon::Pistol, false, t + Duration::from_secs(2)), Err(Reject::TargetDown));
        assert!(c.respawn(2));
        assert_eq!(c.hit(1, 2, Weapon::Sniper, false, t + Duration::from_secs(3)), Ok(Hit::Damage { hp: 40 }));
    }

    #[test]
    fn claims_are_rejected_for_each_rule() {
        let (mut c, t) = room();
        assert_eq!(c.hit(1, 9, Weapon::Pistol, false, t), Err(Reject::UnknownTarget));
        assert_eq!(c.hit(1, 1, Weapon::Pistol, false, t), Err(Reject::SelfHit));
        c.pose(3, 0.0, 50.0, 100.0, false); // a truck can't shoot
        assert_eq!(c.hit(3, 2, Weapon::Pistol, false, t), Err(Reject::NotArmed));
        assert_eq!(c.hit(1, 2, Weapon::Pistol, false, t), Ok(Hit::Damage { hp: 80 }));
        assert_eq!(c.hit(1, 2, Weapon::Pistol, false, t + Duration::from_millis(100)), Err(Reject::TooFast));
        c.pose(2, 400.0, 0.0, 100.0, true); // beyond pistol range × 1.1 (330 m)
        assert_eq!(c.hit(1, 2, Weapon::Pistol, false, t + Duration::from_secs(1)), Err(Reject::OutOfRange));
        assert_eq!(c.hit(1, 2, Weapon::Sniper, false, t + Duration::from_secs(1)), Ok(Hit::Damage { hp: 20 }));
        assert_eq!(c.rejects()["too_fast"], 1);
    }

    #[test]
    fn leaving_forgets_the_player() {
        let (mut c, t) = room();
        c.leave(2);
        assert_eq!(c.hit(1, 2, Weapon::Pistol, false, t), Err(Reject::UnknownTarget));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd relay && cargo test -q combat` — Expected: compile error (`combat` not defined).

- [ ] **Step 3: Implement**

```rust
//! Player-vs-player combat on the relay: health, hit validation, knockouts (spec §6).
//! The shooter's client detects hits; the relay keeps health and rejects implausible claims.

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const MAX_HP: u32 = 100;
const RANGE_SLACK: f64 = 1.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weapon { Pistol, Sniper }

impl Weapon {
    pub fn parse(s: &str) -> Option<Weapon> {
        match s { "pistol" => Some(Weapon::Pistol), "sniper" => Some(Weapon::Sniper), _ => None }
    }
    pub fn name(self) -> &'static str { match self { Weapon::Pistol => "pistol", Weapon::Sniper => "sniper" } }
    fn damage(self, head: bool) -> u32 {
        match (self, head) { (Weapon::Pistol, false) => 20, (Weapon::Pistol, true) => 40, (Weapon::Sniper, false) => 60, (Weapon::Sniper, true) => 100 }
    }
    fn interval(self) -> Duration { match self { Weapon::Pistol => Duration::from_millis(250), Weapon::Sniper => Duration::from_millis(1500) } }
    fn range_m(self) -> f64 { match self { Weapon::Pistol => 300.0, Weapon::Sniper => 3000.0 } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit { Damage { hp: u32 }, Knockout }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject { UnknownTarget, SelfHit, TargetDown, NotArmed, TooFast, OutOfRange }

impl Reject {
    pub fn as_str(self) -> &'static str {
        match self {
            Reject::UnknownTarget => "unknown_target", Reject::SelfHit => "self_hit", Reject::TargetDown => "target_down",
            Reject::NotArmed => "not_armed", Reject::TooFast => "too_fast", Reject::OutOfRange => "out_of_range",
        }
    }
}

struct Fighter { hp: u32, pos: [f64; 3], armed: bool, last_hit: HashMap<Weapon, Instant> }

#[derive(Default)]
pub struct Combat { players: HashMap<u64, Fighter>, rejects: HashMap<&'static str, u64> }

impl Combat {
    pub fn pose(&mut self, id: u64, x: f64, y: f64, h: f64, armed: bool) {
        let f = self.players.entry(id).or_insert(Fighter { hp: MAX_HP, pos: [x, y, h], armed, last_hit: HashMap::new() });
        f.pos = [x, y, h];
        f.armed = armed;
    }

    pub fn hit(&mut self, shooter: u64, target: u64, w: Weapon, head: bool, now: Instant) -> Result<Hit, Reject> {
        let r = self.check(shooter, target, w, now);
        if let Err(e) = r {
            *self.rejects.entry(e.as_str()).or_default() += 1;
            return Err(e);
        }
        self.players.get_mut(&shooter).unwrap().last_hit.insert(w, now);
        let t = self.players.get_mut(&target).unwrap();
        t.hp = t.hp.saturating_sub(w.damage(head));
        Ok(if t.hp == 0 { Hit::Knockout } else { Hit::Damage { hp: t.hp } })
    }

    fn check(&self, shooter: u64, target: u64, w: Weapon, now: Instant) -> Result<(), Reject> {
        let t = self.players.get(&target).ok_or(Reject::UnknownTarget)?;
        if shooter == target { return Err(Reject::SelfHit); }
        if t.hp == 0 { return Err(Reject::TargetDown); }
        let s = self.players.get(&shooter).ok_or(Reject::NotArmed)?;
        if !s.armed || s.hp == 0 { return Err(Reject::NotArmed); }
        if s.last_hit.get(&w).is_some_and(|&at| now.saturating_duration_since(at) < w.interval()) { return Err(Reject::TooFast); }
        let d = ((s.pos[0] - t.pos[0]).powi(2) + (s.pos[1] - t.pos[1]).powi(2) + (s.pos[2] - t.pos[2]).powi(2)).sqrt();
        if d > w.range_m() * RANGE_SLACK { return Err(Reject::OutOfRange); }
        Ok(())
    }

    pub fn respawn(&mut self, id: u64) -> bool {
        match self.players.get_mut(&id) {
            Some(f) if f.hp == 0 => { f.hp = MAX_HP; true }
            _ => false,
        }
    }

    pub fn leave(&mut self, id: u64) { self.players.remove(&id); }

    pub fn rejects(&self) -> &HashMap<&'static str, u64> { &self.rejects }
}
```

Add `mod combat;` to `relay/src/main.rs`.

- [ ] **Step 4: Run tests** — `cd relay && cargo test -q combat` — Expected: 4 passed.

- [ ] **Step 5: Commit** — `git add relay/src/combat.rs relay/src/main.rs && git commit -m "Relay: combat rules (health, hit validation, knockouts)"`

---

### Task 2: Relay routing and integration test

**Files:**
- Modify: `relay/src/main.rs` (Room gets `combat: Mutex<Combat>`; `handle()` routes `hit`,
  `respawn`, and pose `k`; `cleanup()` calls `combat.leave`; `relay_json()` adds `rejected_hits`)
- Modify: `relay/src/stats.rs` (event kinds `ko`; `total_kos`)
- Test: `relay/tests/backpressure.rs` (new test `two_players_fight_to_a_knockout`)

**Interfaces:**
- Consumes: Task 1 `Combat`, `Weapon`, `Hit`.
- Produces (wire): `dmg`, `ko`, `respawn` events to all peers (include the shooter and target).

- [ ] **Step 1: Write the failing integration test** (append to `relay/tests/backpressure.rs`)

```rust
#[test]
fn two_players_fight_to_a_knockout() {
    let relay = start_relay();
    let (mut a, _a_id) = join(&relay);
    let (mut b, b_id) = join(&relay);
    let robot = |x: f64| json!({"t":"s","name":"r","x":x,"y":1_900_000.0,"h":500.0,"q":[0,0,0,1],"v":[0,0,0],"b":false,"k":"robot","w":"sniper"}).to_string();
    a.send(Message::text(robot(0.0))).unwrap();
    b.send(Message::text(robot(200.0))).unwrap();
    thread::sleep(Duration::from_millis(200));
    a.send(Message::text(json!({"t":"hit","target":b_id,"weapon":"sniper","head":false}).to_string())).unwrap();
    thread::sleep(Duration::from_millis(1600));
    a.send(Message::text(json!({"t":"hit","target":b_id,"weapon":"sniper","head":false}).to_string())).unwrap();
    let mut seen = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) && !seen.iter().any(|v: &Value| v["t"] == "ko") {
        if let Some(v) = next(&mut b) { seen.push(v); }
    }
    assert!(seen.iter().any(|v| v["t"] == "dmg" && v["hp"] == 40 && v["target"].as_u64() == Some(b_id)), "{seen:?}");
    assert!(seen.iter().any(|v| v["t"] == "ko" && v["target"].as_u64() == Some(b_id) && v["weapon"] == "sniper"));
    // A claim on a knocked-out target is rejected and counted.
    a.send(Message::text(json!({"t":"hit","target":b_id,"weapon":"sniper","head":false}).to_string())).unwrap();
    thread::sleep(Duration::from_millis(300));
    assert!(stats_json(&relay)["relay"]["rejected_hits"]["target_down"].as_u64().unwrap_or(0) >= 1);
    // The victim respawns.
    b.send(Message::text(json!({"t":"respawn"}).to_string())).unwrap();
    let t0 = Instant::now();
    let mut back = false;
    while t0.elapsed() < Duration::from_secs(2) && !back {
        back = next(&mut a).is_some_and(|v| v["t"] == "respawn" && v["target"].as_u64() == Some(b_id));
    }
    assert!(back, "respawn announced");
}
```

- [ ] **Step 2: Run to verify it fails** — `cd relay && cargo test -q --test backpressure two_players` — Expected: FAIL (no `dmg`).

- [ ] **Step 3: Implement routing** in `handle()`:

```rust
"hit" => {
    let target = v["target"].as_u64().unwrap_or(0);
    let Some(w) = v["weapon"].as_str().and_then(combat::Weapon::parse) else { return Ok(()) };
    let head = v["head"].as_bool().unwrap_or(false);
    let result = lock(&room.combat).hit(id, target, w, head, Instant::now());
    let msg = match result {
        Ok(combat::Hit::Damage { hp }) => json!({"t":"dmg","target":target,"by":id,"hp":hp,"weapon":w.name(),"head":head}),
        Ok(combat::Hit::Knockout) => {
            lock(&room.stats).knockout(id, target, w.name(), now());
            json!({"t":"ko","target":target,"by":id,"weapon":w.name()})
        }
        Err(_) => return Ok(()),
    };
    room.fan_out(id, Kind::Event, Arc::from(msg.to_string()), true);
    Ok(())
}
"respawn" => {
    if lock(&room.combat).respawn(id) {
        room.fan_out(id, Kind::Event, Arc::from(json!({"t":"respawn","target":id}).to_string()), true);
    }
    Ok(())
}
```

In the `"s"` branch, after stats: `let k = v["k"].as_str().unwrap_or("truck"); lock(&room.combat).pose(id, x, y, h, k == "robot" || k.starts_with("ride:"));`
(x, y, h from the message; skip presence poses with `m: true`). In `cleanup()`, when removed:
`lock(&self.combat).leave(peer.id);`. In `relay_json()`: `"rejected_hits": lock(&self.combat).rejects()`.
In `stats.rs` add:

```rust
pub fn knockout(&mut self, by: u64, target: u64, weapon: &str, now: u64) {
    let name = |id: u64| self.players.get(&id).map_or(format!("player {id}"), |p| p.name.clone());
    let text = format!("{} knocked out {} ({weapon})", name(by), name(target));
    self.total_kos += 1;
    self.record(now, "ko", target, text, None);
}
```

(with `pub total_kos: u64` counted in `json()`/`html()` next to drops and flips).

- [ ] **Step 4: Run all relay tests** — `cd relay && cargo test -q` — Expected: all pass (unit + 5 integration).

- [ ] **Step 5: Commit** — `git commit -am "Relay: route hits, knockouts and respawns"`

---

### Task 3: Ray tests

**Files:**
- Create: `viewer/src/player/ray.rs`, `viewer/src/player/mod.rs` (`pub mod ray;` only for now), register `mod player;` in `main.rs`.

**Interfaces:**
- Produces:
  ```rust
  pub fn terrain(origin: Vec3, dir: Vec3, max: f32, ground: &dyn Fn(Vec3) -> Option<f32>) -> Option<f32>;
  pub fn sphere(origin: Vec3, dir: Vec3, centre: Vec3, r: f32) -> Option<f32>;
  pub fn capsule(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3, r: f32) -> Option<f32>;
  ```
  `dir` is normalised; results are distances along the ray.

- [ ] **Step 1: Tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sphere_and_capsule_hits_are_nearest_surfaces() {
        let o = Vec3::ZERO; let d = Vec3::X;
        assert!((sphere(o, d, Vec3::new(10.0, 0.0, 0.0), 1.0).unwrap() - 9.0).abs() < 1e-4);
        assert_eq!(sphere(o, d, Vec3::new(10.0, 5.0, 0.0), 1.0), None);
        let t = capsule(o, d, Vec3::new(20.0, -3.0, 0.0), Vec3::new(20.0, 3.0, 0.0), 0.5).unwrap();
        assert!((t - 19.5).abs() < 1e-3, "{t}");
        assert_eq!(sphere(o, d, Vec3::new(-10.0, 0.0, 0.0), 1.0), None, "behind the ray");
    }
    #[test]
    fn a_hill_stops_the_ray_before_the_target() {
        // Flat at 0 m except a 30 m ridge between x = 40 and 60.
        let ground = |p: Vec3| Some(if (40.0..60.0).contains(&p.x) { 30.0 } else { 0.0 });
        let o = Vec3::new(0.0, 10.0, 0.0); let d = Vec3::X;
        let t = terrain(o, d, 200.0, &ground).unwrap();
        assert!((t - 40.0).abs() < 0.6, "{t}");
        assert!(sphere(o, d, Vec3::new(100.0, 10.0, 0.0), 1.0).unwrap() > t);
        assert_eq!(terrain(Vec3::new(0.0, 50.0, 0.0), d, 200.0, &ground), None, "passes over the ridge");
    }
}
```

- [ ] **Step 2: Fail** — `cd viewer && cargo test -q ray` — Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Ray tests for shooting: terrain (marched), spheres and capsules. `dir` must be unit length.
use bevy::prelude::*;

/// First ground hit within `max` metres: march 2 m steps, then bisect to ~5 cm.
pub fn terrain(origin: Vec3, dir: Vec3, max: f32, ground: &dyn Fn(Vec3) -> Option<f32>) -> Option<f32> {
    let above = |t: f32| { let p = origin + dir * t; ground(p).is_none_or(|h| p.y > h) };
    if !above(0.0) { return Some(0.0); }
    let step = 2.0;
    let mut t = 0.0;
    while t < max {
        let next = (t + step).min(max);
        if !above(next) {
            let (mut lo, mut hi) = (t, next);
            while hi - lo > 0.05 { let mid = 0.5 * (lo + hi); if above(mid) { lo = mid } else { hi = mid } }
            return Some(hi);
        }
        t = next;
    }
    None
}

pub fn sphere(origin: Vec3, dir: Vec3, centre: Vec3, r: f32) -> Option<f32> {
    let oc = origin - centre;
    let b = oc.dot(dir);
    let c = oc.length_squared() - r * r;
    let disc = b * b - c;
    if disc < 0.0 { return None; }
    let s = disc.sqrt();
    let t = if -b - s >= 0.0 { -b - s } else { -b + s };
    (t >= 0.0).then_some(t)
}

/// Capsule from `a` to `b` with radius `r`: nearest of the cylinder and the two end spheres.
pub fn capsule(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3, r: f32) -> Option<f32> {
    let ab = b - a;
    let len = ab.length();
    let axis = ab / len.max(1e-6);
    let mut best = [sphere(origin, dir, a, r), sphere(origin, dir, b, r)].into_iter().flatten().fold(None, |m: Option<f32>, t| Some(m.map_or(t, |m| m.min(t))));
    // Infinite cylinder around the axis, clipped to the segment.
    let (d, o) = (dir - axis * dir.dot(axis), (origin - a) - axis * (origin - a).dot(axis));
    let (qa, qb, qc) = (d.dot(d), 2.0 * d.dot(o), o.dot(o) - r * r);
    if qa > 1e-8 {
        let disc = qb * qb - 4.0 * qa * qc;
        if disc >= 0.0 {
            let t = (-qb - disc.sqrt()) / (2.0 * qa);
            let s = (origin + dir * t - a).dot(axis);
            if t >= 0.0 && (0.0..=len).contains(&s) { best = Some(best.map_or(t, |m| m.min(t))); }
        }
    }
    best
}
```

- [ ] **Step 4: Pass** — `cargo test -q ray` — Expected: 2 passed.

- [ ] **Step 5: Commit** — `git add viewer/src/player viewer/src/main.rs && git commit -m "Player: ray tests for shooting"`

---

### Task 4: Weapons state machine

**Files:**
- Create: `viewer/src/player/weapons.rs` (`pub mod weapons;` in `player/mod.rs`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)] pub enum Held { #[default] None, Pistol, Sniper }
  impl Held { pub fn next(self) -> Held; pub fn name(self) -> &'static str; pub fn stats(self) -> Option<&'static Stats>; }
  pub struct Stats { pub damage: u32, pub head_damage: u32, pub interval: f32, pub range: f32, pub magazine: u32, pub reload: f32, pub zoom: f32, pub spread: f32 }
  #[derive(Default)] pub struct Arsenal { pub held: Held, .. }
  impl Arsenal {
      pub fn cycle(&mut self);
      pub fn tick(&mut self, dt: f32);
      pub fn try_fire(&mut self) -> bool;   // consumes a round if ready
      pub fn reload(&mut self);
      pub fn ammo(&self) -> (u32, u32);     // (in magazine, magazine size) for the held weapon
      pub fn reloading(&self) -> Option<f32>; // 0..1 progress
  }
  ```

- [ ] **Step 1: Tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn y_cycles_none_pistol_sniper() {
        let mut a = Arsenal::default();
        assert_eq!(a.held, Held::None); a.cycle(); assert_eq!(a.held, Held::Pistol); a.cycle(); assert_eq!(a.held, Held::Sniper); a.cycle(); assert_eq!(a.held, Held::None);
    }
    #[test]
    fn fire_interval_magazine_and_reload() {
        let mut a = Arsenal::default(); a.cycle(); // pistol
        assert!(a.try_fire()); assert!(!a.try_fire(), "0.25 s interval");
        a.tick(0.25); assert!(a.try_fire());
        for _ in 0..10 { a.tick(0.25); assert!(a.try_fire()); }
        assert_eq!(a.ammo(), (0, 12)); a.tick(0.25); assert!(!a.try_fire(), "empty");
        a.reload(); assert!(a.reloading().is_some()); a.tick(1.0); assert!(!a.try_fire(), "still reloading");
        a.tick(0.6); assert_eq!(a.ammo(), (12, 12)); assert!(a.try_fire());
    }
    #[test]
    fn each_weapon_keeps_its_own_magazine() {
        let mut a = Arsenal::default(); a.cycle(); a.try_fire(); a.cycle(); // sniper
        assert_eq!(a.ammo(), (5, 5)); assert!(a.try_fire()); a.tick(1.0); assert!(!a.try_fire(), "1.5 s bolt");
        a.cycle(); a.cycle(); assert_eq!(a.ammo(), (11, 12));
    }
}
```

- [ ] **Step 2: Fail** — `cargo test -q weapons` — compile error.

- [ ] **Step 3: Implement**

```rust
//! Pistol and sniper: stats, fire interval, magazines and reloading (spec §5).

pub struct Stats { pub damage: u32, pub head_damage: u32, pub interval: f32, pub range: f32, pub magazine: u32, pub reload: f32, pub zoom: f32, pub spread: f32 }

static PISTOL: Stats = Stats { damage: 20, head_damage: 40, interval: 0.25, range: 300.0, magazine: 12, reload: 1.5, zoom: 1.3, spread: 0.01 };
static SNIPER: Stats = Stats { damage: 60, head_damage: 100, interval: 1.5, range: 3000.0, magazine: 5, reload: 2.5, zoom: 6.0, spread: 0.05 };

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Held { #[default] None, Pistol, Sniper }

impl Held {
    pub fn next(self) -> Held { match self { Held::None => Held::Pistol, Held::Pistol => Held::Sniper, Held::Sniper => Held::None } }
    pub fn name(self) -> &'static str { match self { Held::None => "none", Held::Pistol => "pistol", Held::Sniper => "sniper" } }
    pub fn stats(self) -> Option<&'static Stats> { match self { Held::None => None, Held::Pistol => Some(&PISTOL), Held::Sniper => Some(&SNIPER) } }
    fn slot(self) -> usize { match self { Held::Pistol => 0, _ => 1 } }
}

pub struct Arsenal { pub held: Held, rounds: [u32; 2], cooldown: f32, reload_left: Option<f32> }

impl Default for Arsenal {
    fn default() -> Self { Arsenal { held: Held::None, rounds: [PISTOL.magazine, SNIPER.magazine], cooldown: 0.0, reload_left: None } }
}

impl Arsenal {
    pub fn cycle(&mut self) { self.held = self.held.next(); self.cooldown = 0.0; self.reload_left = None; }
    pub fn tick(&mut self, dt: f32) {
        self.cooldown = (self.cooldown - dt).max(0.0);
        if let (Some(left), Some(s)) = (self.reload_left, self.held.stats()) {
            let left = left - dt;
            if left <= 0.0 { self.rounds[self.held.slot()] = s.magazine; self.reload_left = None; } else { self.reload_left = Some(left); }
        }
    }
    pub fn try_fire(&mut self) -> bool {
        let Some(s) = self.held.stats() else { return false };
        let slot = self.held.slot();
        if self.reload_left.is_some() || self.cooldown > 1e-6 || self.rounds[slot] == 0 { return false; }
        self.rounds[slot] -= 1;
        self.cooldown = s.interval;
        true
    }
    pub fn reload(&mut self) {
        if let Some(s) = self.held.stats() { if self.rounds[self.held.slot()] < s.magazine && self.reload_left.is_none() { self.reload_left = Some(s.reload); } }
    }
    pub fn ammo(&self) -> (u32, u32) { self.held.stats().map_or((0, 0), |s| (self.rounds[self.held.slot()], s.magazine)) }
    pub fn reloading(&self) -> Option<f32> { let s = self.held.stats()?; self.reload_left.map(|l| 1.0 - l / s.reload) }
}
```

- [ ] **Step 4: Pass** — `cargo test -q weapons` — 3 passed.
- [ ] **Step 5: Commit** — `git commit -am "Player: pistol and sniper state"` (plus `git add` of the new file).

---

### Task 5: Robot model, transform poses and movement

**Files:**
- Create: `viewer/src/player/robot.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct RobotPart { pub size: Vec3, pub truck: Transform, pub robot: Transform, pub dark: bool }
  pub fn parts() -> Vec<RobotPart>;                           // ~16 parts
  pub fn pose(p: &RobotPart, t: f32, walk_phase: f32, speed: f32) -> Transform; // t: 0 = truck, 1 = robot
  pub const HEIGHT: f32 = 7.0; pub const EYE: f32 = 6.4; pub const RADIUS: f32 = 1.3;
  #[derive(Clone, Copy, Default)] pub struct Walker { pub pos: Vec3, pub vel: Vec3, pub yaw: f32, pub grounded: bool, pub phase: f32 }
  pub struct WalkInput { pub forward: f32, pub strafe: f32, pub run: bool, pub jump: bool }
  pub fn step(w: &mut Walker, input: &WalkInput, dt: f32, ground: &dyn Fn(Vec3) -> Option<f32>);
  ```

- [ ] **Step 1: Tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transform_starts_as_the_truck_and_ends_as_the_robot() {
        for p in parts() {
            assert!(pose(&p, 0.0, 0.0, 0.0).translation.distance(p.truck.translation) < 1e-4);
            assert!(pose(&p, 1.0, 0.0, 0.0).translation.distance(p.robot.translation) < 1e-4);
        }
        let top = parts().iter().map(|p| p.robot.translation.y + p.size.y / 2.0).fold(0.0, f32::max);
        assert!((top - HEIGHT).abs() < 0.5, "robot ~7 m tall: {top}");
    }
    #[test]
    fn walker_follows_slopes_and_lands_jumps() {
        let ground = |p: Vec3| Some(p.x * 0.2); // 20% slope up to the east
        let mut w = Walker { pos: Vec3::new(0.0, 0.0, 0.0), yaw: -std::f32::consts::FRAC_PI_2, grounded: true, ..default() };
        let input = WalkInput { forward: 1.0, strafe: 0.0, run: false, jump: false };
        for _ in 0..120 { step(&mut w, &input, 1.0 / 60.0, &ground); }
        assert!(w.pos.x > 5.0 && (w.pos.y - w.pos.x * 0.2).abs() < 0.05, "{:?}", w.pos);
        let jump = WalkInput { jump: true, ..input };
        step(&mut w, &jump, 1.0 / 60.0, &ground);
        assert!(!w.grounded && w.vel.y > 0.0);
        for _ in 0..300 { step(&mut w, &input, 1.0 / 60.0, &ground); }
        assert!(w.grounded);
    }
}
```

(`yaw = -π/2` faces +X east because models face −Z.)

- [ ] **Step 2: Fail** — `cargo test -q robot` — compile error.

- [ ] **Step 3: Implement** — part table (truck pose from the truck body layout in
`truck.rs::spawn_model`, robot pose standing), and:

```rust
pub fn pose(p: &RobotPart, t: f32, walk_phase: f32, speed: f32) -> Transform {
    let e = t * t * (3.0 - 2.0 * t); // smoothstep
    let mut tf = Transform {
        translation: p.truck.translation.lerp(p.robot.translation, e),
        rotation: p.truck.rotation.slerp(p.robot.rotation, e),
        scale: Vec3::ONE,
    };
    // Walk swing for limbs (parts tagged by x offset and height in robot pose).
    if t >= 1.0 && p.robot.translation.y < 3.2 && p.robot.translation.x.abs() > 0.3 {
        let side = p.robot.translation.x.signum();
        tf.rotation = Quat::from_rotation_x((walk_phase).sin() * 0.5 * side * (speed / 4.0).min(1.5)) * tf.rotation;
    }
    tf
}

pub fn step(w: &mut Walker, input: &WalkInput, dt: f32, ground: &dyn Fn(Vec3) -> Option<f32>) {
    const GRAVITY: f32 = 18.0; const JUMP: f32 = 9.0; const WALK: f32 = 15.0 / 3.6; const RUN: f32 = 40.0 / 3.6;
    let speed = if input.run { RUN } else { WALK };
    let fwd = Quat::from_rotation_y(w.yaw) * Vec3::NEG_Z;
    let right = Quat::from_rotation_y(w.yaw) * Vec3::X;
    let want = (fwd * input.forward + right * input.strafe).clamp_length_max(1.0) * speed;
    let k = (dt * if w.grounded { 10.0 } else { 2.0 }).min(1.0);
    w.vel.x += (want.x - w.vel.x) * k;
    w.vel.z += (want.z - w.vel.z) * k;
    if w.grounded && input.jump { w.vel.y = JUMP; w.grounded = false; }
    w.vel.y -= GRAVITY * dt;
    w.pos += w.vel * dt;
    let h = ground(w.pos).unwrap_or(w.pos.y);
    if w.pos.y <= h || (w.grounded && w.pos.y - h < 0.6) { w.pos.y = h; w.vel.y = 0.0; w.grounded = true; } else { w.grounded = false; }
    w.phase = (w.phase + Vec2::new(w.vel.x, w.vel.z).length() * dt / 2.5 * std::f32::consts::TAU) % std::f32::consts::TAU;
}
```

- [ ] **Step 4: Pass** — `cargo test -q robot` — 2 passed.
- [ ] **Step 5: Commit** — `git commit -m "Player: robot parts, transform poses, walking"`

---

### Task 6: Riding in the dinosaur brain

**Files:**
- Modify: `viewer/src/dinos/brain.rs` (new `State::Ridden`, `Agent.ride: (f32, f32, bool)` =
  (throttle −1..1, steer −1..1, run), skipped in `think`, steered in `step`)

**Interfaces:**
- Produces: `State::Ridden`; `Agent::rideable(&self) -> bool` (not raptor, not Tied/Down/Netted).

- [ ] **Step 1: Test** (append to brain tests)

```rust
#[test]
fn a_ridden_dinosaur_goes_where_the_rider_steers() {
    let mut a = [at(1, Species::Stego, 0.0, 0.0)];
    a[0].heading = 0.0; // east
    a[0].state = State::Ridden;
    a[0].ride = (1.0, 0.0, true);
    run(&mut a, truck(10.0, 0.0, -30.0, 0.0), 5.0, &flat); // a fast truck doesn't scare it off
    assert_eq!(a[0].state, State::Ridden);
    assert!(a[0].pos.x > 10.0 && a[0].pos.y.abs() < 1.0, "{:?}", a[0].pos);
    a[0].ride = (1.0, 1.0, false);
    let h0 = a[0].heading; run(&mut a, None, 1.0, &flat);
    assert!(a[0].heading > h0 + 0.3, "steer left turns counter-clockwise");
    assert!(!at(2, Species::Raptor, 0.0, 0.0).rideable());
}
```

- [ ] **Step 2: Fail** — `cargo test -q brain` — compile error.
- [ ] **Step 3: Implement** — in `think`: `if a.state == State::Ridden { continue; }`. In `step`,
  a `State::Ridden` arm: `a.heading = wrap(a.heading + (a.ride.1 * def.turn * 1.5 * dt) as f64);`
  target `None`, `want = a.ride.0.max(0.0) * if a.ride.2 { def.run } else { def.walk }`;
  add `Ridden` to `available()`'s exclusions so carnivores don't hunt a ridden animal, and
  `pub fn rideable(&self) -> bool { self.species != Species::Raptor && self.available() }`.
- [ ] **Step 4: Pass** — all dinos tests pass.
- [ ] **Step 5: Commit** — `git commit -am "Dinosaurs: ridden state"`

---

### Task 7: Player state, modes, keys, camera and robot entity

**Files:**
- Modify: `viewer/src/player/mod.rs` (Player resource + plugin + systems)
- Modify: `viewer/src/truck.rs` (`physics` and `reset` return early unless `player.body == Body::Truck`)
- Modify: `viewer/src/main.rs` (`cameras`: first person and zoom; `map_input`: chase mouse
  only in the truck; HUD controls line; add `player::PlayerPlugin`)
- Modify: `viewer/src/dinos/mod.rs` (`pub fn nearest_rideable`, `pub fn set_ride`, `pub fn release_ride`, saddle offsets)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, PartialEq, Debug, Default)] pub enum Body { #[default] Truck, Robot, Riding(DinoId) }
  #[derive(Resource)] pub struct Player {
      pub body: Body, pub morph: f32 /*0 truck..1 robot*/, pub morph_dir: f32, pub walker: Walker,
      pub first_person: bool, pub look_yaw: f32, pub look_pitch: f32, pub zoom: f32,
      pub arsenal: Arsenal, pub hp: u32, pub knocked_out: Option<f32>,
  }
  impl Player { pub fn eye(&self, truck: &Truck, dinos: &Dinos, origin: &WorldOrigin) -> Vec3; pub fn armed(&self) -> bool; pub fn kind(&self, dinos: &Dinos) -> String; }
  pub fn handle_keys(...) // X, E, V, Y, R per spec; R: reload if armed else reset
  ```

- [ ] **Step 1: Tests** (pure `Player` rules, in `player/mod.rs`)

```rust
#[test]
fn r_reloads_only_with_a_weapon_out() {
    let mut p = Player::default();
    assert_eq!(p.r_action(), RAction::Reset);
    p.body = Body::Robot; p.arsenal.cycle();
    assert_eq!(p.r_action(), RAction::Reload);
}
#[test]
fn transform_is_blocked_while_riding_or_airborne() {
    let mut p = Player::default();
    assert!(p.can_transform(true));
    p.body = Body::Riding(DinoId { cx: 0, cy: 0, n: 0 });
    assert!(!p.can_transform(true));
    p.body = Body::Robot;
    assert!(!p.can_transform(false), "not grounded");
}
#[test]
fn knockout_dismounts_and_blocks_input() {
    let mut p = Player { body: Body::Riding(DinoId { cx: 0, cy: 0, n: 0 }), ..default() };
    p.knock_out();
    assert_eq!(p.body, Body::Robot);
    assert!(p.knocked_out.is_some() && !p.accepts_input());
    p.tick_knockout(5.1);
    assert!(p.accepts_input() && p.hp == 100);
}
#[test]
fn drawing_a_weapon_switches_to_first_person_and_holstering_restores() {
    let mut p = Player { body: Body::Robot, ..default() };
    p.cycle_weapon(); assert!(p.first_person);
    p.cycle_weapon(); p.cycle_weapon(); assert!(!p.first_person);
}
```

- [ ] **Step 2: Fail** — `cargo test -q player` — compile errors.
- [ ] **Step 3: Implement** the pure methods (`r_action`, `can_transform`, `knock_out`,
  `tick_knockout` (sets `respawn_due` flag at 5 s), `accepts_input`, `cycle_weapon` remembering
  `view_before_weapon`), then the systems:
  - `player_keys`: X → start morph (truck→robot: walker at truck pos, yaw from truck heading,
    truck hidden; robot→truck: `truck.drop_at(walker.pos + 2 m up, yaw)`); E → mount nearest
    rideable within 6 m (`dinos::set_ride`) or dismount (`release_ride`, walker beside it);
    V → toggle first person; Y → `cycle_weapon`; R → per `r_action`.
  - `player_move` (FixedUpdate after truck physics): robot → `robot::step` with WASD/Shift/Space
    relative to `look_yaw`; riding → write `dinos` agent `ride` from WASD and put the walker on
    the saddle; morph advances 1/1.5 s.
  - `player_mouse`: in robot/riding, `AccumulatedMouseMotion` drives `look_yaw`/`look_pitch`
    (robot `walker.yaw` follows `look_yaw`); right mouse held → `zoom` eases to the weapon's zoom.
  - `robot_model`: spawns the robot parts once (hidden) and poses them from `robot::pose(part,
    morph, walker.phase, speed)` at the walker (or saddle) position; hidden in truck mode when
    `morph == 0` and in first person (only the arms stay visible there).
  - `cameras` (main.rs): if `player.body != Truck || player.morph > 0`: first person → eye at
    `player.eye(..)`, rotation from look yaw/pitch; third person → behind the walker like the
    chase camera. `Projection::fov = base_fov / player.zoom`.
- [ ] **Step 4: Pass** — `cargo test -q` in viewer — all pass; native autopilot still runs
  (`VR_FIRE_AUTOPILOT=... cargo run --release -p viewer` exits 0).
- [ ] **Step 5: Commit** — `git commit -am "Player: robot mode, riding, first person"`

---

### Task 8: Network protocol and remote visuals

**Files:**
- Modify: `viewer/src/net.rs`

**Interfaces:**
- Consumes: `Player::kind`, `Player::arsenal.held`, `player.hp`.
- Produces: `Msg::Hit{target,weapon,head}`, `Msg::Dmg{target,by,hp,weapon,head}`,
  `Msg::Ko{target,by,weapon}`, `Msg::Respawn{#[serde(default)] target}`; `State` gains
  `#[serde(default)] k: String`, `w: String`, `hp: Option<u32>`; `Remote` gains `kind`,
  `weapon`, `hp`, `robot: Option<Entity>`, `standin: Option<Entity>`;
  `pub fn hitboxes(r: &Remote) -> Vec<Hitbox>` with `Hitbox { shape: Shape, head: bool }`.

- [ ] **Step 1: Tests**

```rust
#[test]
fn old_poses_default_to_truck() {
    let old = r#"{"t":"s","id":3,"name":"a","x":1.0,"y":2.0,"h":3.0,"q":[0,0,0,1],"v":[0,0,0],"b":false}"#;
    let Ok(Msg::State { k, w, hp, .. }) = serde_json::from_str::<Msg>(old) else { panic!() };
    assert_eq!((k.as_str(), w.as_str(), hp), ("", "", None));
}
#[test]
fn combat_messages_round_trip() {
    let m: Msg = serde_json::from_str(r#"{"t":"dmg","target":5,"by":2,"hp":40,"weapon":"sniper","head":true}"#).unwrap();
    assert!(matches!(m, Msg::Dmg { target: 5, by: 2, hp: 40, head: true, .. }));
    let m: Msg = serde_json::from_str(r#"{"t":"respawn"}"#).unwrap();
    assert!(matches!(m, Msg::Respawn { target: 0 }));
}
#[test]
fn robot_hitboxes_have_a_head_above_the_body() {
    let boxes = hitboxes_for("robot", Vec3::ZERO);
    let head = boxes.iter().find(|h| h.head).unwrap();
    let body = boxes.iter().find(|h| !h.head).unwrap();
    assert!(head.top() > body.top());
    assert!(hitboxes_for("truck", Vec3::ZERO).iter().all(|h| !h.head));
}
```

- [ ] **Step 2: Fail** — `cargo test -q net` — compile errors.
- [ ] **Step 3: Implement**: send `k`/`w`/`hp` in poses; on `dmg`/`ko`/`respawn` update
  `Remote.hp` or our own `Player` (`ko` for us → `player.knock_out()`; kill-feed line in
  `remotes.log`: "trucker-6923 sniped trucker-1433 (headshot)"); spawn a robot model for remotes
  in robot/ride mode (reuse `robot::parts()` at `morph = 1`) and a stand-in dinosaur for
  `ride:<species>` (`dinos::spawn_standin(species)`), hiding the truck model.
  When our own knockout countdown ends (`Player::respawn_due`), respawn: robot mode at a point
  20–40 m from where we fell (terrain height), full health, and send `{"t":"respawn"}`.
- [ ] **Step 4: Pass** — `cargo test -q` — all pass.
- [ ] **Step 5: Commit** — `git commit -am "Net: combat messages and remote robots/riders"`

---

### Task 9: Firing, hits, effects, HUD and scope

**Files:**
- Create: `viewer/src/player/combat.rs`

**Interfaces:**
- Consumes: `ray::{terrain, sphere, capsule}`, `net::hitboxes`, `Arsenal::try_fire`,
  `dinos` agents (capsules from species radius/neck), `Msg::Hit`.
- Produces: `pub fn resolve(origin, dir, range, ground, dinos, remotes) -> Option<Target>` with
  `enum Target { Ground(f32), Dino(DinoId, f32), Player { id: u64, head: bool, t: f32 } }`.

- [ ] **Step 1: Test**

```rust
#[test]
fn nearest_target_wins_and_terrain_blocks() {
    let ground = |_: Vec3| Some(0.0);
    let players = vec![(7u64, hitboxes_for("robot", Vec3::new(0.0, 0.0, -50.0))), (8u64, hitboxes_for("robot", Vec3::new(0.0, 0.0, -80.0)))];
    let hit = resolve_among(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Z, 300.0, &ground, &[], &players);
    assert!(matches!(hit, Some(Target::Player { id: 7, head: false, .. })), "{hit:?}");
    let down = resolve_among(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, -1.0).normalize(), 300.0, &ground, &[], &players);
    assert!(matches!(down, Some(Target::Ground(_))));
}
```

- [ ] **Step 2: Fail**, **Step 3: Implement** `resolve_among` (pure) and systems:
  - `fire`: left mouse (just pressed; pistol is semi-automatic) and `accepts_input` and armed →
    `try_fire` → ray from the camera through the centre with spread (none when scoped) →
    `resolve`; player hit → send `Msg::Hit`, show a hit marker (red if head); dinosaur hit →
    pistol: `Flee` (3rd hit → `Down`), sniper: `Down`; always a tracer and an impact puff.
  - `overlay`: crosshair node in first person; scope overlay when sniper + right mouse: a
    full-screen `ImageNode` from a procedural 1024² RGBA texture (transparent disc, black outside,
    crosshair lines and mil-dots), plus a range readout from a probe ray.
  - HUD panel: weapon, ammo `8/12`, reload bar, health bar, and the knockout countdown.
- [ ] **Step 4: Pass** — tests; native manual run with autopilot (Task 10).
- [ ] **Step 5: Commit** — `git commit -m "Player: firing, hits, scope and HUD"`

---

### Task 10: Autopilot demo, web checks, docs, deploy

**Files:**
- Modify: `viewer/src/main.rs` (autopilot plan `VR_FIRE_AUTOPILOT_ROBOT`)
- Modify: `viewer/README.md` (controls, combat rules)

- [ ] **Step 1:** Add the plan: drop → J → X (transform) → walk 3 s → screenshot `8_robot`
  → E (mount the nearest dinosaur; the demo spawns a tame one ahead) → ride 3 s → screenshot
  `9_riding` → Y Y (sniper) → hold right mouse → screenshot `10_scope` → fire at a demo target
  dinosaur → screenshot `11_hit` → exit.
- [ ] **Step 2:** Run it natively; inspect all four screenshots.
- [ ] **Step 3:** Build both wasm variants; headless Chrome check (no panics / validation
  errors) with a transform + weapon draw.
- [ ] **Step 4:** Two-player check against the deployed relay after deploy: two headless
  clients, one knocks the other out via `hit` messages; `/stats.json` shows `ko` and no
  unexpected rejections.
- [ ] **Step 5:** README controls table + "Combat" section; commit, push, `scripts/deploy_viewer.sh`.
