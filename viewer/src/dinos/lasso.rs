//! Truck lasso rules: one full loop around a dinosaur nets it, a second hog-ties it.
//! Pure logic (no Bevy) so the loop geometry is tested directly.

use super::DinoId;
use bevy::math::DVec2;
use std::f64::consts::{PI, TAU};

/// Accumulated signed angle swept around a point.
#[derive(Default, Clone, Copy, Debug)]
pub struct Sweep {
    last: Option<f64>,
    pub total: f64,
}

impl Sweep {
    pub fn reset(&mut self) {
        *self = Sweep::default();
    }

    /// Add the change from the previous angle (wrapped to (−π, π]); returns the running total.
    pub fn add(&mut self, angle: f64) -> f64 {
        if let Some(last) = self.last {
            self.total += (angle - last + PI).rem_euclid(TAU) - PI;
        }
        self.last = Some(angle);
        self.total
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    Stowed,
    Ready,
    Connected,
    /// Tightening animation; seconds left.
    Cinch(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Connected(DinoId),
    Tied(DinoId),
    Snapped(DinoId),
    Stowed,
}

/// A dinosaur the lasso could act on this frame.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub id: DinoId,
    pub pos: DVec2,
    /// Targeting radius and rope length, metres.
    pub radius: f64,
    pub rope: f64,
    /// Seconds a netted animal can tug before it breaks free (T. rex), if limited.
    pub breaks_free_after: Option<f32>,
    /// False once hog-tied or brought down.
    pub tieable: bool,
}

/// Minimum truck speed for a loop to count (8 km/h).
pub const MIN_SPEED: f32 = 8.0 / 3.6;
const STALL_S: f32 = 2.0;
const IDLE_STOW_S: f32 = 60.0;
const CINCH_S: f32 = 0.8;

#[derive(Clone, Debug)]
pub struct Lasso {
    pub stage: Stage,
    pub target: Option<DinoId>,
    pub sweep: Sweep,
    slow_for: f32,
    idle_for: f32,
    tug_for: f32,
    pub tied: u32,
}

impl Default for Lasso {
    fn default() -> Self {
        Lasso { stage: Stage::Stowed, target: None, sweep: Sweep::default(), slow_for: 0.0, idle_for: 0.0, tug_for: 0.0, tied: 0 }
    }
}

impl Lasso {
    /// A stowed lasso that keeps the session's hog-tie count.
    pub fn with_tally(tied: u32) -> Self {
        Lasso { tied, ..Lasso::default() }
    }

    /// Q: ready a stowed lasso, stow a ready one (a connected rope stays until it snaps or ties).
    pub fn toggle(&mut self) {
        match self.stage {
            Stage::Stowed => {
                self.stage = Stage::Ready;
                self.target = None;
                self.sweep.reset();
                self.idle_for = 0.0;
                self.slow_for = 0.0;
            }
            Stage::Ready => {
                self.stage = Stage::Stowed;
                self.target = None;
            }
            Stage::Connected | Stage::Cinch(_) => {}
        }
    }

    /// Whether the truck is moving fast enough for the loop to count; stalling resets it.
    fn moving(&mut self, dt: f32, speed: f32) -> bool {
        if speed.abs() < MIN_SPEED {
            self.slow_for += dt;
            if self.slow_for >= STALL_S {
                self.sweep.reset();
            }
            false
        } else {
            self.slow_for = 0.0;
            true
        }
    }

    fn snap(&mut self, id: DinoId) -> Option<Event> {
        self.stage = Stage::Ready;
        self.target = None;
        self.sweep.reset();
        self.idle_for = 0.0;
        Some(Event::Snapped(id))
    }

    fn angle(truck: DVec2, c: &Candidate) -> f64 {
        let d = truck - c.pos;
        d.y.atan2(d.x)
    }

    /// Progress of the current loop, 0..1.
    pub fn progress(&self) -> f64 {
        (self.sweep.total.abs() / TAU).min(1.0)
    }

    /// Advance one frame. `truck` is the truck's ground position (EPSG:5070) and speed (m/s).
    pub fn update(&mut self, dt: f32, truck: DVec2, speed: f32, candidates: &[Candidate]) -> Option<Event> {
        match self.stage {
            Stage::Stowed => None,
            Stage::Cinch(left) => {
                let left = left - dt;
                if left > 0.0 {
                    self.stage = Stage::Cinch(left);
                    return None;
                }
                self.stage = Stage::Stowed;
                self.target = None;
                self.sweep.reset();
                Some(Event::Stowed)
            }
            Stage::Ready => {
                let near = candidates
                    .iter()
                    .filter(|c| c.tieable && c.pos.distance(truck) <= c.radius)
                    .min_by(|a, b| a.pos.distance(truck).total_cmp(&b.pos.distance(truck)));
                let Some(c) = near else {
                    self.target = None;
                    self.sweep.reset();
                    self.idle_for += dt;
                    if self.idle_for >= IDLE_STOW_S {
                        self.stage = Stage::Stowed;
                        self.idle_for = 0.0;
                        return Some(Event::Stowed);
                    }
                    return None;
                };
                self.idle_for = 0.0;
                if self.target != Some(c.id) {
                    self.target = Some(c.id);
                    self.sweep.reset();
                    self.slow_for = 0.0;
                }
                if !self.moving(dt, speed) {
                    return None;
                }
                if self.sweep.add(Self::angle(truck, c)).abs() >= TAU {
                    self.stage = Stage::Connected;
                    self.sweep.reset();
                    self.tug_for = 0.0;
                    return Some(Event::Connected(c.id));
                }
                None
            }
            Stage::Connected => {
                let id = self.target?;
                let Some(c) = candidates.iter().find(|c| c.id == id && c.tieable) else { return self.snap(id) };
                self.tug_for += dt;
                if c.pos.distance(truck) > c.rope || c.breaks_free_after.is_some_and(|t| self.tug_for > t) {
                    return self.snap(id);
                }
                if !self.moving(dt, speed) {
                    return None;
                }
                if self.sweep.add(Self::angle(truck, c)).abs() >= TAU {
                    self.stage = Stage::Cinch(CINCH_S);
                    self.sweep.reset();
                    self.tied += 1;
                    return Some(Event::Tied(id));
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn id(n: u8) -> DinoId {
        DinoId { cx: 0, cy: 0, n }
    }

    fn stego(n: u8, pos: DVec2) -> Candidate {
        Candidate { id: id(n), pos, radius: 38.5, rope: 53.5, breaks_free_after: None, tieable: true }
    }

    /// Drive `turns` loops of radius `r` around `c` at `v` m/s, feeding the lasso each frame.
    fn circle(l: &mut Lasso, c: &[Candidate], centre: DVec2, r: f64, v: f64, turns: f64, start: f64) -> Vec<Event> {
        let omega = v / r;
        let steps = (turns * TAU / omega.abs() / DT as f64).ceil() as usize;
        let mut events = Vec::new();
        for i in 0..=steps {
            let a = start + omega * i as f64 * DT as f64;
            if let Some(e) = l.update(DT, centre + DVec2::new(a.cos(), a.sin()) * r, v as f32, c) {
                events.push(e);
            }
        }
        events
    }

    #[test]
    fn a_full_circle_sweeps_360_degrees_across_the_wrap() {
        let mut s = Sweep::default();
        let mut total = 0.0;
        for i in 0..=360 {
            // Start just below +π so the ±π wrap is crossed early.
            let a = (PI - 0.1 + i as f64 * TAU / 360.0 + PI).rem_euclid(TAU) - PI;
            total = s.add(a);
        }
        assert!((total - TAU).abs() < 1e-9, "{total}");
    }

    #[test]
    fn going_back_the_other_way_unwinds_the_sweep() {
        let mut s = Sweep::default();
        for i in 0..=180 {
            s.add(i as f64 * PI / 180.0);
        }
        for i in (0..=180).rev() {
            s.add(i as f64 * PI / 180.0);
        }
        assert!(s.total.abs() < 1e-9);
    }

    #[test]
    fn one_loop_nets_and_a_second_hog_ties() {
        let mut l = Lasso::default();
        l.toggle();
        assert_eq!(l.stage, Stage::Ready);
        let c = [stego(1, DVec2::ZERO)];
        let first = circle(&mut l, &c, DVec2::ZERO, 20.0, 10.0, 1.02, 0.0);
        assert_eq!(first, [Event::Connected(id(1))]);
        assert_eq!(l.stage, Stage::Connected);
        let second = circle(&mut l, &c, DVec2::ZERO, 20.0, 10.0, 1.02, 0.0);
        assert_eq!(second, [Event::Tied(id(1))]);
        assert_eq!(l.tied, 1);
        // The cinch finishes and the lasso stows itself.
        let mut stowed = false;
        for _ in 0..120 {
            stowed |= l.update(DT, DVec2::new(20.0, 0.0), 10.0, &c) == Some(Event::Stowed);
        }
        assert!(stowed);
        assert_eq!(l.stage, Stage::Stowed);
    }

    #[test]
    fn either_direction_works() {
        let mut l = Lasso::default();
        l.toggle();
        let c = [stego(1, DVec2::ZERO)];
        let e = circle(&mut l, &c, DVec2::ZERO, 20.0, -10.0, 1.02, 0.0);
        assert_eq!(e, [Event::Connected(id(1))]);
    }

    #[test]
    fn a_circle_that_leaves_the_radius_does_not_count() {
        let mut l = Lasso::default();
        l.toggle();
        let c = [stego(1, DVec2::ZERO)];
        // Radius 45 m is outside the stegosaurus's 38.5 m reach.
        assert!(circle(&mut l, &c, DVec2::ZERO, 45.0, 10.0, 1.2, 0.0).is_empty());
        assert_eq!(l.target, None);
    }

    #[test]
    fn stopping_for_two_seconds_resets_the_loop() {
        let mut l = Lasso::default();
        l.toggle();
        let c = [stego(1, DVec2::ZERO)];
        circle(&mut l, &c, DVec2::ZERO, 20.0, 10.0, 0.8, 0.0);
        assert!(l.progress() > 0.7);
        let here = DVec2::from_angle(0.8 * TAU) * 20.0;
        for _ in 0..(2.5 / DT) as usize {
            l.update(DT, here, 0.0, &c);
        }
        assert!(l.progress() < 0.01);
    }

    #[test]
    fn driving_beyond_the_rope_snaps_it() {
        let mut l = Lasso::default();
        l.toggle();
        let c = [stego(1, DVec2::ZERO)];
        circle(&mut l, &c, DVec2::ZERO, 20.0, 10.0, 1.02, 0.0);
        assert_eq!(l.stage, Stage::Connected);
        assert_eq!(l.update(DT, DVec2::new(60.0, 0.0), 10.0, &c), Some(Event::Snapped(id(1))));
        assert_eq!(l.stage, Stage::Ready);
        assert_eq!(l.target, None);
    }

    #[test]
    fn a_trex_breaks_free_if_not_tied_in_time() {
        let mut l = Lasso::default();
        l.toggle();
        let rex = [Candidate { breaks_free_after: Some(20.0), ..stego(7, DVec2::ZERO) }];
        circle(&mut l, &rex, DVec2::ZERO, 20.0, 10.0, 1.02, 0.0);
        assert_eq!(l.stage, Stage::Connected);
        let mut snapped = false;
        for _ in 0..(21.0 / DT) as usize {
            snapped |= l.update(DT, DVec2::new(20.0, 0.0), 0.0, &rex) == Some(Event::Snapped(id(7)));
        }
        assert!(snapped);
    }

    #[test]
    fn the_nearest_tieable_dinosaur_is_targeted() {
        let mut l = Lasso::default();
        l.toggle();
        let c = [
            Candidate { tieable: false, ..stego(1, DVec2::new(5.0, 0.0)) },
            stego(2, DVec2::new(15.0, 0.0)),
            stego(3, DVec2::new(30.0, 0.0)),
        ];
        l.update(DT, DVec2::ZERO, 10.0, &c);
        assert_eq!(l.target, Some(id(2)));
    }

    #[test]
    fn a_ready_lasso_with_nothing_to_catch_stows_after_a_minute() {
        let mut l = Lasso::default();
        l.toggle();
        let mut stowed = false;
        for _ in 0..(61.0 / DT) as usize {
            stowed |= l.update(DT, DVec2::ZERO, 10.0, &[]) == Some(Event::Stowed);
        }
        assert!(stowed);
        assert_eq!(l.stage, Stage::Stowed);
    }
}
