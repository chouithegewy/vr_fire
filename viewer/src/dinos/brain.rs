//! Dinosaur behaviour: a small state machine (decisions at ~5 Hz) and steering on the terrain
//! (every frame). Positions are EPSG:5070 metres; heading 0 = east, counter-clockwise.

use super::DinoId;
use super::spawn::Rng;
use super::species::Species;
use bevy::math::DVec2;
use std::f64::consts::{PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Wander,
    Graze,
    Flee,
    /// Stand and turn toward the threat at `goal`.
    Face,
    Chase(DinoId),
    /// Raptors: run a circle around the truck.
    Circle,
    Eat,
    /// Caught by a carnivore; lies on its side, then disappears.
    Down,
    Netted,
    Tied,
}

#[derive(Clone, Copy, Debug)]
pub struct TruckView {
    pub pos: DVec2,
    pub vel: DVec2,
}

#[derive(Clone, Debug)]
pub struct Agent {
    pub id: DinoId,
    pub species: Species,
    pub pos: DVec2,
    /// Ground height under the animal, metres.
    pub h: f32,
    pub heading: f64,
    pub speed: f32,
    pub state: State,
    /// State timer, seconds (meaning depends on the state).
    pub timer: f32,
    pub goal: DVec2,
    pub home: DVec2,
    /// Seconds since the last meal (carnivores).
    pub hunger: f32,
    /// Walk-cycle phase, radians.
    pub phase: f32,
    pub rng: Rng,
    /// Set when a Down animal has been lying long enough to be removed.
    pub gone: bool,
}

/// Truck speed (m/s) that frightens herbivores within `SCARE_M`.
const SCARE_SPEED: f64 = 20.0 / 3.6;
const SCARE_M: f64 = 40.0;
const PREDATOR_M: f64 = 60.0;
const HUNT_M: f64 = 120.0;
const GIVE_UP_M: f64 = 200.0;
const HUNGRY_S: f32 = 60.0;
const RAPTOR_TRUCK_M: f64 = 80.0;
const RAPTOR_CIRCLE_M: f64 = 25.0;
const WANDER_M: f64 = 150.0;
const DOWN_S: f32 = 60.0;
/// Below this height is ocean or the water plane.
const WATER_H: f32 = -100.0;
const MAX_SLOPE: f32 = 0.7; // ~35°

impl Agent {
    pub fn new(id: DinoId, species: Species, pos: DVec2, home: DVec2) -> Self {
        let mut rng = Rng(((id.cx as u64) << 40) ^ ((id.cy as u64) << 16) ^ id.n as u64 ^ 0x5eed);
        let heading = rng.range(0.0, TAU);
        let hunger = rng.range(0.0, 90.0) as f32;
        Agent {
            id,
            species,
            pos,
            h: 0.0,
            heading,
            speed: 0.0,
            state: State::Wander,
            timer: 0.0,
            goal: pos,
            home,
            hunger,
            phase: rng.range(0.0, TAU) as f32,
            rng,
            gone: false,
        }
    }

    pub fn is_prey(&self) -> bool {
        !self.species.def().carnivore && self.species != Species::Brachio && self.available()
    }

    /// Not caught, netted or tied.
    pub fn available(&self) -> bool {
        !matches!(self.state, State::Down | State::Netted | State::Tied)
    }
}

/// Re-evaluate every animal's state (call a few times per second).
pub fn think(agents: &mut [Agent], truck: Option<TruckView>) {
    struct Seen {
        id: DinoId,
        species: Species,
        pos: DVec2,
        prey: bool,
        available: bool,
    }
    let seen: Vec<Seen> =
        agents.iter().map(|a| Seen { id: a.id, species: a.species, pos: a.pos, prey: a.is_prey(), available: a.available() }).collect();
    for a in agents.iter_mut() {
        if !a.available() || (a.state == State::Eat && a.timer > 0.0) {
            continue;
        }
        let def = a.species.def();
        if !def.carnivore {
            let scared_by_truck = truck.filter(|t| t.pos.distance(a.pos) < SCARE_M && t.vel.length() > SCARE_SPEED).map(|t| t.pos);
            let rex = seen
                .iter()
                .filter(|o| o.species == Species::TRex && o.available && o.pos.distance(a.pos) < PREDATOR_M)
                .min_by(|x, y| x.pos.distance(a.pos).total_cmp(&y.pos.distance(a.pos)))
                .map(|o| o.pos);
            if let Some(threat) = scared_by_truck.or(rex) {
                // Triceratops stand their ground; everyone else runs.
                a.state = if a.species == Species::Trike { State::Face } else { State::Flee };
                a.goal = threat;
                a.timer = if a.state == State::Face { 3.0 } else { 4.0 };
                continue;
            }
            if matches!(a.state, State::Flee | State::Face) {
                if a.timer > 0.0 {
                    continue;
                }
                a.state = State::Wander;
                a.timer = 0.0;
            }
            if a.state == State::Graze {
                continue;
            }
            if a.rng.f64() < 0.03 {
                a.state = State::Graze;
                a.timer = a.rng.range(5.0, 20.0) as f32;
                continue;
            }
        } else if a.species == Species::TRex {
            if let State::Chase(target) = a.state {
                match seen.iter().find(|o| o.id == target) {
                    Some(o) if o.prey && o.pos.distance(a.pos) < GIVE_UP_M => {
                        a.goal = o.pos;
                        continue;
                    }
                    _ => a.state = State::Wander,
                }
            }
            if a.hunger > HUNGRY_S {
                let prey = seen
                    .iter()
                    .filter(|o| o.prey && o.pos.distance(a.pos) < HUNT_M)
                    .min_by(|x, y| x.pos.distance(a.pos).total_cmp(&y.pos.distance(a.pos)));
                if let Some(o) = prey {
                    a.state = State::Chase(o.id);
                    a.goal = o.pos;
                    continue;
                }
            }
        } else if let Some(t) = truck.filter(|t| t.pos.distance(a.pos) < RAPTOR_TRUCK_M) {
            a.state = State::Circle;
            a.goal = t.pos;
            continue;
        }
        if a.state != State::Wander {
            a.state = State::Wander;
            a.timer = 0.0;
        }
        if a.pos.distance(a.goal) < 5.0 || a.timer <= 0.0 {
            let ang = a.rng.range(0.0, TAU);
            a.goal = a.home + DVec2::new(ang.cos(), ang.sin()) * WANDER_M * a.rng.f64().sqrt();
            a.timer = 30.0;
        }
    }
}

fn wrap(a: f64) -> f64 {
    (a + PI).rem_euclid(TAU) - PI
}

/// Move every animal for `dt` seconds: steering, terrain, catches and timers.
/// `ground` returns the terrain height at a point, or None where it isn't loaded.
pub fn step(agents: &mut [Agent], dt: f32, truck: Option<TruckView>, ground: &dyn Fn(DVec2) -> Option<f32>) {
    // Catches: a chasing carnivore that reaches its prey brings it down and starts eating.
    let mut catches = Vec::new();
    for (i, a) in agents.iter().enumerate() {
        let State::Chase(target) = a.state else { continue };
        if let Some(j) = agents.iter().position(|o| o.id == target && o.is_prey()) {
            let reach = (a.species.def().radius + agents[j].species.def().radius + 1.0) as f64;
            if a.pos.distance(agents[j].pos) < reach {
                catches.push((i, j));
            }
        }
    }
    for (i, j) in catches {
        let prey = &mut agents[j];
        prey.state = State::Down;
        prey.timer = 0.0;
        prey.speed = 0.0;
        let hunter = &mut agents[i];
        hunter.state = State::Eat;
        hunter.timer = 15.0;
        hunter.hunger = 0.0;
    }

    for a in agents.iter_mut() {
        let def = a.species.def();
        if def.carnivore {
            a.hunger += dt;
        }
        if let Some(h) = ground(a.pos) {
            a.h = h;
        }
        match a.state {
            State::Down => {
                a.speed = 0.0;
                a.timer += dt;
                a.gone |= a.timer > DOWN_S;
                continue;
            }
            State::Tied => {
                a.speed = 0.0;
                continue;
            }
            State::Eat | State::Graze => {
                a.timer -= dt;
                if a.timer <= 0.0 {
                    a.state = State::Wander;
                    a.timer = 0.0;
                }
            }
            _ => a.timer -= dt,
        }

        // Where to head and how fast.
        let (target, want, turn_boost) = match a.state {
            State::Wander => (Some(a.goal), def.walk, 1.0),
            State::Graze | State::Eat | State::Down | State::Tied => (None, 0.0, 1.0),
            State::Face => (Some(a.goal), 0.0, 1.5),
            State::Flee => (Some(a.pos + (a.pos - a.goal).normalize_or(DVec2::X) * 50.0), def.run, 2.5),
            State::Chase(_) => (Some(a.goal), def.run, 1.5),
            State::Circle => {
                let off = a.pos - a.goal;
                let ang = off.y.atan2(off.x) + 0.6;
                (Some(a.goal + DVec2::new(ang.cos(), ang.sin()) * RAPTOR_CIRCLE_M), def.run * 0.6, 1.0)
            }
            State::Netted => match truck {
                Some(t) => (Some(a.pos + (a.pos - t.pos).normalize_or(DVec2::X) * 50.0), def.walk * 0.5, 1.0),
                None => (None, 0.0, 1.0),
            },
        };
        if let Some(t) = target {
            if t.distance(a.pos) > 0.5 {
                let want_heading = (t - a.pos).y.atan2((t - a.pos).x);
                let max = (def.turn * turn_boost * dt) as f64;
                a.heading = wrap(a.heading + wrap(want_heading - a.heading).clamp(-max, max));
            }
        }
        let accel = def.run * 1.5 * dt;
        a.speed += (want - a.speed).clamp(-accel * 2.0, accel);

        let dir = DVec2::new(a.heading.cos(), a.heading.sin());
        let mut next = a.pos + dir * (a.speed * dt) as f64;
        if a.state == State::Netted {
            if let Some(t) = truck {
                let rope = (a.species.rope_length() - 1.0) as f64;
                if next.distance(t.pos) > rope {
                    next = t.pos + (next - t.pos).normalize_or(DVec2::X) * rope;
                }
            }
        }
        let moved = next.distance(a.pos) as f32;
        if moved < 1e-6 {
            continue;
        }
        match ground(next) {
            Some(h) if h > WATER_H && (h - a.h).abs() <= MAX_SLOPE * moved + 0.05 => {
                a.pos = next;
                a.h = h;
                a.phase = (a.phase + moved / def.stride * std::f32::consts::TAU) % std::f32::consts::TAU;
            }
            _ => {
                // Water, a cliff or unloaded ground: turn away and slow down.
                a.heading = wrap(a.heading + PI * 0.5 + a.rng.range(0.0, PI));
                a.speed *= 0.3;
                a.goal = a.pos;
                a.timer = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(n: u8, species: Species, x: f64, y: f64) -> Agent {
        let p = DVec2::new(x, y);
        let mut a = Agent::new(DinoId { cx: 0, cy: 0, n }, species, p, p);
        a.hunger = 0.0;
        a
    }

    fn flat(_: DVec2) -> Option<f32> {
        Some(100.0)
    }

    /// Simulate `secs` with decisions at 5 Hz and motion at 20 Hz.
    fn run(agents: &mut [Agent], truck: Option<TruckView>, secs: f32, ground: &dyn Fn(DVec2) -> Option<f32>) {
        let dt = 0.05;
        for i in 0..(secs / dt) as usize {
            if i % 4 == 0 {
                think(agents, truck);
            }
            step(agents, dt, truck, ground);
        }
    }

    fn truck(x: f64, y: f64, vx: f64, vy: f64) -> Option<TruckView> {
        Some(TruckView { pos: DVec2::new(x, y), vel: DVec2::new(vx, vy) })
    }

    #[test]
    fn a_herbivore_flees_a_fast_truck() {
        let mut a = [at(1, Species::Stego, 0.0, 0.0)];
        let t = truck(30.0, 0.0, -10.0, 0.0);
        think(&mut a, t);
        assert_eq!(a[0].state, State::Flee);
        run(&mut a, t, 5.0, &flat);
        assert!(a[0].pos.x < -5.0, "ran away from the truck: {:?}", a[0].pos);
    }

    #[test]
    fn a_slow_truck_is_ignored() {
        let mut a = [at(1, Species::Stego, 0.0, 0.0)];
        think(&mut a, truck(30.0, 0.0, -2.0, 0.0));
        assert_ne!(a[0].state, State::Flee);
    }

    #[test]
    fn a_triceratops_turns_to_face_the_truck() {
        let mut a = [at(1, Species::Trike, 0.0, 0.0)];
        a[0].heading = PI; // facing west, truck to the east
        let t = truck(30.0, 0.0, -10.0, 0.0);
        run(&mut a, t, 4.0, &flat);
        assert_eq!(a[0].state, State::Face);
        assert!(a[0].speed < 0.1);
        let facing = (a[0].heading + PI).rem_euclid(TAU) - PI;
        assert!(facing.abs() < 0.2, "heading {facing}");
    }

    #[test]
    fn herbivores_flee_a_nearby_trex() {
        let mut a = [at(1, Species::Stego, 0.0, 0.0), at(2, Species::TRex, 50.0, 0.0)];
        think(&mut a, None);
        assert_eq!(a[0].state, State::Flee);
    }

    #[test]
    fn a_hungry_trex_chases_catches_and_eats() {
        let mut a = [at(1, Species::TRex, 0.0, 0.0), at(2, Species::Stego, 80.0, 0.0)];
        a[0].hunger = 100.0;
        think(&mut a, None);
        assert_eq!(a[0].state, State::Chase(a[1].id));
        run(&mut a, None, 60.0, &flat);
        assert_eq!(a[1].state, State::Down);
        assert!(matches!(a[0].state, State::Eat | State::Wander | State::Graze));
        assert!(a[0].hunger < 60.0, "ate");
    }

    #[test]
    fn a_fed_trex_does_not_hunt() {
        let mut a = [at(1, Species::TRex, 0.0, 0.0), at(2, Species::Stego, 80.0, 0.0)];
        think(&mut a, None);
        assert!(!matches!(a[0].state, State::Chase(_)));
    }

    #[test]
    fn raptors_circle_a_nearby_truck() {
        let mut a = [at(1, Species::Raptor, 0.0, 0.0)];
        let t = truck(40.0, 0.0, 0.0, 0.0);
        run(&mut a, t, 10.0, &flat);
        assert_eq!(a[0].state, State::Circle);
        let d = a[0].pos.distance(DVec2::new(40.0, 0.0));
        assert!((15.0..35.0).contains(&d), "circling at {d} m");
    }

    #[test]
    fn a_downed_animal_is_removed_after_a_minute() {
        let mut a = [at(1, Species::Stego, 0.0, 0.0)];
        a[0].state = State::Down;
        run(&mut a, None, 61.0, &flat);
        assert!(a[0].gone);
        assert_eq!(a[0].pos, DVec2::ZERO);
    }

    #[test]
    fn animals_never_walk_into_water_or_unloaded_ground() {
        let mut a = [at(1, Species::Stego, 0.0, 0.0)];
        a[0].goal = DVec2::new(100.0, 0.0);
        a[0].home = DVec2::new(100.0, 0.0);
        let shore = |p: DVec2| if p.x < 10.0 { Some(5.0) } else { Some(-150.0) };
        run(&mut a, None, 60.0, &shore);
        assert!(a[0].pos.x < 10.0, "stayed on land: {:?}", a[0].pos);
        let edge = |p: DVec2| if p.x < 10.0 { Some(5.0) } else { None };
        run(&mut a, None, 60.0, &edge);
        assert!(a[0].pos.x < 10.0);
    }

    #[test]
    fn a_tied_animal_never_moves() {
        let mut a = [at(1, Species::Stego, 0.0, 0.0)];
        a[0].state = State::Tied;
        run(&mut a, truck(10.0, 0.0, -30.0, 0.0), 10.0, &flat);
        assert_eq!(a[0].pos, DVec2::ZERO);
        assert_eq!(a[0].state, State::Tied);
    }

    #[test]
    fn a_netted_animal_tugs_but_stays_within_the_rope() {
        let mut a = [at(1, Species::Stego, 30.0, 0.0)];
        a[0].state = State::Netted;
        run(&mut a, truck(0.0, 0.0, 0.0, 0.0), 20.0, &flat);
        assert_eq!(a[0].state, State::Netted);
        let d = a[0].pos.length();
        assert!(d > 30.0 && d <= Species::Stego.rope_length() as f64, "{d}");
    }

    #[test]
    fn wanderers_stay_near_home() {
        let mut a = [at(1, Species::Brachio, 0.0, 0.0)];
        run(&mut a, None, 600.0, &flat);
        assert!(a[0].pos.length() < WANDER_M + 30.0);
    }
}
