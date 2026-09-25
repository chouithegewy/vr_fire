//! Dinosaur species: gameplay numbers and procedural low-poly models.
//!
//! Models face local −Z (like the truck) with feet at y = 0. Each part hangs off a joint
//! pivot; animation rotates the pivot (legs swing, heads dip, tails sway).

use bevy::prelude::*;
use std::f32::consts::{FRAC_PI_2, PI};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Species {
    TRex,
    Raptor,
    Stego,
    Trike,
    Brachio,
}

pub struct Def {
    pub name: &'static str,
    /// Nose to tail, metres.
    pub length: f32,
    pub carnivore: bool,
    /// Walk and run speeds, m/s.
    pub walk: f32,
    pub run: f32,
    /// Turn rate, rad/s.
    pub turn: f32,
    /// Contact radius against the truck, metres.
    pub radius: f32,
    /// Heavy animals push (and can flip) the truck; light ones get shoved aside.
    pub heavy: bool,
    /// Where the lasso rope attaches (height of the neck), metres.
    pub neck: f32,
    /// Stride length for the walk cycle, metres.
    pub stride: f32,
    pub skin: [f32; 3],
    pub belly: [f32; 3],
    pub accent: [f32; 3],
}

const fn kmh(v: f32) -> f32 {
    v / 3.6
}

static TREX: Def = Def {
    name: "T. rex",
    length: 12.0,
    carnivore: true,
    walk: kmh(8.0),
    run: kmh(25.0),
    turn: 1.2,
    radius: 3.2,
    heavy: true,
    neck: 5.8,
    stride: 5.0,
    skin: [0.33, 0.30, 0.20],
    belly: [0.55, 0.50, 0.38],
    accent: [0.20, 0.16, 0.10],
};
static RAPTOR: Def = Def {
    name: "Velociraptor",
    length: 2.0,
    carnivore: true,
    walk: kmh(10.0),
    run: kmh(60.0),
    turn: 4.0,
    radius: 0.8,
    heavy: false,
    neck: 1.3,
    stride: 1.4,
    skin: [0.62, 0.50, 0.32],
    belly: [0.80, 0.72, 0.55],
    accent: [0.35, 0.22, 0.12],
};
static STEGO: Def = Def {
    name: "Stegosaurus",
    length: 9.0,
    carnivore: false,
    walk: kmh(4.0),
    run: kmh(15.0),
    turn: 0.9,
    radius: 3.0,
    heavy: true,
    neck: 2.4,
    stride: 3.0,
    skin: [0.40, 0.45, 0.28],
    belly: [0.60, 0.62, 0.45],
    accent: [0.58, 0.26, 0.18],
};
static TRIKE: Def = Def {
    name: "Triceratops",
    length: 8.0,
    carnivore: false,
    walk: kmh(5.0),
    run: kmh(25.0),
    turn: 1.0,
    radius: 3.0,
    heavy: true,
    neck: 2.8,
    stride: 2.8,
    skin: [0.45, 0.40, 0.33],
    belly: [0.62, 0.58, 0.50],
    accent: [0.52, 0.30, 0.20],
};
static BRACHIO: Def = Def {
    name: "Brachiosaurus",
    length: 22.0,
    carnivore: false,
    walk: kmh(3.0),
    run: kmh(8.0),
    turn: 0.5,
    radius: 4.5,
    heavy: true,
    neck: 9.0,
    stride: 6.0,
    skin: [0.42, 0.46, 0.40],
    belly: [0.60, 0.64, 0.58],
    accent: [0.30, 0.33, 0.28],
};

impl Species {
    pub const ALL: [Species; 5] = [Species::TRex, Species::Raptor, Species::Stego, Species::Trike, Species::Brachio];

    pub fn def(self) -> &'static Def {
        match self {
            Species::TRex => &TREX,
            Species::Raptor => &RAPTOR,
            Species::Stego => &STEGO,
            Species::Trike => &TRIKE,
            Species::Brachio => &BRACHIO,
        }
    }

    /// Radius within which the lasso can target this species.
    pub fn lasso_radius(self) -> f32 {
        25.0 + 1.5 * self.def().length
    }

    /// The rope snaps if the truck gets farther than this from a netted animal.
    pub fn rope_length(self) -> f32 {
        self.lasso_radius() + 15.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Joint {
    Fixed,
    /// Walk cycle phase offset, radians.
    Leg(f32),
    Head,
    Tail,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Box(Vec3),
    /// Along the part's +Y before `rot`.
    Cone { radius: f32, height: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    Skin,
    Belly,
    Accent,
}

#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub joint: Joint,
    pub pivot: Vec3,
    pub offset: Vec3,
    pub rot: Quat,
    pub shape: Shape,
    pub tone: Tone,
}

fn part(joint: Joint, pivot: Vec3, offset: Vec3, shape: Shape, tone: Tone) -> Part {
    Part { joint, pivot, offset, rot: Quat::IDENTITY, shape, tone }
}

fn bx(x: f32, y: f32, z: f32) -> Shape {
    Shape::Box(Vec3::new(x, y, z))
}

/// Tail cone pointing backwards (+Z) from its pivot.
fn tail(pivot: Vec3, radius: f32, height: f32, droop: f32) -> Part {
    let rot = Quat::from_rotation_x(FRAC_PI_2 - droop);
    let offset = rot * Vec3::Y * (height / 2.0);
    Part { joint: Joint::Tail, pivot, offset, rot, shape: Shape::Cone { radius, height }, tone: Tone::Skin }
}

/// A leg hanging from `hip` to the ground, `thick` wide, plus a foot.
fn leg(hip: Vec3, thick: f32, phase: f32, out: &mut Vec<Part>) {
    let len = hip.y;
    out.push(part(Joint::Leg(phase), hip, Vec3::new(0.0, -len / 2.0, 0.0), bx(thick, len, thick * 1.1), Tone::Skin));
    out.push(part(
        Joint::Leg(phase),
        hip,
        Vec3::new(0.0, -len + thick * 0.2, -thick * 0.3),
        bx(thick * 1.1, thick * 0.4, thick * 1.6),
        Tone::Belly,
    ));
}

fn quad_legs(front: Vec3, rear: Vec3, thick: f32, out: &mut Vec<Part>) {
    // Diagonal pairs move together.
    leg(Vec3::new(-front.x, front.y, front.z), thick, 0.0, out);
    leg(Vec3::new(front.x, front.y, front.z), thick, PI, out);
    leg(Vec3::new(-rear.x, rear.y, rear.z), thick, PI, out);
    leg(Vec3::new(rear.x, rear.y, rear.z), thick, 0.0, out);
}

/// The model of a species.
pub fn parts(s: Species) -> Vec<Part> {
    let mut p = Vec::new();
    let v = Vec3::new;
    match s {
        Species::TRex => {
            let hip = 3.6;
            p.push(part(Joint::Fixed, v(0.0, hip + 1.0, 0.0), Vec3::ZERO, bx(2.2, 2.4, 4.6), Tone::Skin));
            p.push(part(Joint::Fixed, v(0.0, hip + 0.3, -0.3), Vec3::ZERO, bx(1.8, 1.0, 3.6), Tone::Belly));
            let neck = v(0.0, hip + 1.8, -2.1);
            p.push(part(Joint::Head, neck, v(0.0, 0.2, -0.6), bx(1.2, 1.5, 1.6), Tone::Skin));
            p.push(part(Joint::Head, neck, v(0.0, 0.8, -2.0), bx(1.4, 1.5, 2.5), Tone::Skin));
            p.push(part(Joint::Head, neck, v(0.0, -0.1, -1.9), bx(1.1, 0.45, 2.1), Tone::Belly));
            for x in [-0.8f32, 0.8] {
                let mut arm = part(Joint::Fixed, v(x, hip + 0.7, -1.9), Vec3::ZERO, bx(0.25, 0.9, 0.25), Tone::Skin);
                arm.rot = Quat::from_rotation_x(0.7);
                p.push(arm);
            }
            p.push(tail(v(0.0, hip + 1.2, 2.2), 1.0, 6.5, 0.12));
            leg(v(-0.8, hip + 0.3, 0.4), 0.9, 0.0, &mut p);
            leg(v(0.8, hip + 0.3, 0.4), 0.9, PI, &mut p);
        }
        Species::Raptor => {
            let hip = 0.85;
            p.push(part(Joint::Fixed, v(0.0, hip + 0.2, 0.0), Vec3::ZERO, bx(0.42, 0.48, 1.0), Tone::Skin));
            let neck = v(0.0, hip + 0.35, -0.5);
            p.push(part(Joint::Head, neck, v(0.0, 0.15, -0.12), bx(0.18, 0.35, 0.3), Tone::Skin));
            p.push(part(Joint::Head, neck, v(0.0, 0.32, -0.42), bx(0.22, 0.24, 0.52), Tone::Skin));
            p.push(part(Joint::Head, neck, v(0.0, 0.3, -0.18), bx(0.24, 0.06, 0.2), Tone::Accent));
            p.push(tail(v(0.0, hip + 0.25, 0.5), 0.2, 1.3, 0.05));
            leg(v(-0.17, hip, 0.1), 0.15, 0.0, &mut p);
            leg(v(0.17, hip, 0.1), 0.15, PI, &mut p);
        }
        Species::Stego => {
            p.push(part(Joint::Fixed, v(0.0, 3.0, 0.2), Vec3::ZERO, bx(2.2, 2.4, 5.0), Tone::Skin));
            p.push(part(Joint::Fixed, v(0.0, 2.2, 0.2), Vec3::ZERO, bx(1.8, 0.8, 4.2), Tone::Belly));
            // Two staggered rows of back plates, tallest over the hips.
            for i in 0..8 {
                let z = -2.0 + i as f32 * 0.62;
                let h = 1.0 + 0.7 * (1.0 - ((z - 0.6) / 2.6).powi(2)).max(0.0);
                let x = if i % 2 == 0 { -0.25 } else { 0.25 };
                p.push(part(Joint::Fixed, v(x, 4.2 + h / 2.0, z), Vec3::ZERO, bx(0.12, h, 0.7), Tone::Accent));
            }
            let neck = v(0.0, 2.6, -2.3);
            p.push(part(Joint::Head, neck, v(0.0, -0.2, -0.6), bx(0.7, 0.7, 1.2), Tone::Skin));
            p.push(part(Joint::Head, neck, v(0.0, -0.45, -1.5), bx(0.55, 0.55, 0.9), Tone::Skin));
            let tail_pivot = v(0.0, 3.1, 2.6);
            p.push(tail(tail_pivot, 0.8, 4.2, 0.25));
            for x in [-1.0f32, 1.0] {
                for (z, up) in [(3.4f32, 0.9f32), (3.9, 0.7)] {
                    let rot = Quat::from_rotation_z(-x * 0.9) * Quat::from_rotation_x(-0.2);
                    let offset = v(x * 0.25, -0.4 + up * 0.3, z) + rot * Vec3::Y * 0.4;
                    p.push(Part { joint: Joint::Tail, pivot: tail_pivot, offset, rot, shape: Shape::Cone { radius: 0.12, height: 0.9 }, tone: Tone::Belly });
                }
            }
            quad_legs(v(0.8, 2.2, -1.7), v(0.8, 2.9, 1.7), 0.7, &mut p);
        }
        Species::Trike => {
            p.push(part(Joint::Fixed, v(0.0, 2.8, 0.2), Vec3::ZERO, bx(2.6, 2.2, 4.4), Tone::Skin));
            p.push(part(Joint::Fixed, v(0.0, 2.0, 0.2), Vec3::ZERO, bx(2.1, 0.8, 3.8), Tone::Belly));
            let neck = v(0.0, 2.9, -2.0);
            p.push(part(Joint::Head, neck, v(0.0, -0.2, -1.1), bx(1.5, 1.4, 2.0), Tone::Skin));
            p.push(part(Joint::Head, neck, v(0.0, 0.6, -0.2), bx(3.0, 2.4, 0.3), Tone::Accent));
            let horn = |x: f32, y: f32, z: f32, h: f32| {
                let rot = Quat::from_rotation_x(-FRAC_PI_2 + 0.35);
                Part { joint: Joint::Head, pivot: neck, offset: v(x, y, z) + rot * Vec3::Y * (h / 2.0), rot, shape: Shape::Cone { radius: 0.16, height: h }, tone: Tone::Belly }
            };
            p.push(horn(-0.45, 0.5, -1.4, 1.7));
            p.push(horn(0.45, 0.5, -1.4, 1.7));
            p.push(horn(0.0, -0.3, -2.0, 0.6));
            p.push(tail(v(0.0, 2.9, 2.4), 0.7, 3.0, 0.3));
            quad_legs(v(0.95, 2.0, -1.5), v(0.95, 2.2, 1.6), 0.8, &mut p);
        }
        Species::Brachio => {
            let mut body = part(Joint::Fixed, v(0.0, 7.2, 0.0), Vec3::ZERO, bx(3.5, 4.0, 8.0), Tone::Skin);
            body.rot = Quat::from_rotation_x(0.12);
            p.push(body);
            p.push(part(Joint::Fixed, v(0.0, 5.6, 0.0), Vec3::ZERO, bx(2.8, 1.0, 6.5), Tone::Belly));
            let neck = v(0.0, 8.4, -3.6);
            let up = 0.95f32;
            let dir = Vec3::new(0.0, up.sin(), -up.cos());
            let mut n = part(Joint::Head, neck, dir * 5.0, bx(1.0, 1.0, 10.0), Tone::Skin);
            n.rot = Quat::from_rotation_x(up);
            p.push(n);
            p.push(part(Joint::Head, neck, dir * 10.0 + v(0.0, 0.2, -0.6), bx(0.9, 0.9, 1.9), Tone::Skin));
            p.push(tail(v(0.0, 7.4, 4.0), 1.2, 9.0, 0.35));
            quad_legs(v(1.3, 6.2, -2.8), v(1.3, 5.6, 2.8), 1.2, &mut p);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_species_has_a_body_head_tail_and_grounded_legs() {
        for s in Species::ALL {
            let parts = parts(s);
            let legs: Vec<_> = parts.iter().filter(|p| matches!(p.joint, Joint::Leg(_))).collect();
            let biped = matches!(s, Species::TRex | Species::Raptor);
            assert_eq!(legs.len(), if biped { 4 } else { 8 }, "{s:?}: leg + foot per leg");
            assert!(parts.iter().any(|p| p.joint == Joint::Head), "{s:?} head");
            assert!(parts.iter().any(|p| p.joint == Joint::Tail), "{s:?} tail");
            // Feet reach the ground: the lowest leg point is near y = 0.
            let lowest = legs
                .iter()
                .map(|p| {
                    let Shape::Box(size) = p.shape else { unreachable!() };
                    p.pivot.y + p.offset.y - size.y / 2.0
                })
                .fold(f32::MAX, f32::min);
            assert!(lowest.abs() < 0.25, "{s:?} feet at {lowest}");
        }
    }

    #[test]
    fn models_face_minus_z_and_match_their_length() {
        for s in Species::ALL {
            let parts = parts(s);
            let head_z = parts.iter().filter(|p| p.joint == Joint::Head).map(|p| p.pivot.z + p.offset.z).fold(f32::MAX, f32::min);
            let tail_z = parts.iter().filter(|p| p.joint == Joint::Tail).map(|p| p.pivot.z + p.offset.z).fold(f32::MIN, f32::max);
            assert!(head_z < 0.0 && tail_z > 0.0, "{s:?} faces −Z");
            let span = tail_z - head_z;
            let want = s.def().length;
            assert!(span > want * 0.5 && span < want * 1.2, "{s:?} span {span} vs {want}");
        }
    }

    #[test]
    fn lasso_reach_grows_with_size() {
        assert!(Species::Raptor.lasso_radius() < Species::Brachio.lasso_radius());
        for s in Species::ALL {
            assert!(s.rope_length() > s.lasso_radius());
        }
    }
}
