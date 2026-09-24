//! Green monster truck: procedural model + arcade raycast-suspension physics at 120 Hz.
//! Forward is local −Z. Wheels sample the same terrain triangles that are drawn.

use crate::terrain::Terrain;
use crate::{Mode, WorldOrigin};
use bevy::prelude::*;

pub const MASS: f32 = 4000.0;
const GRAVITY: f32 = 9.81 * 1.5;
const WHEEL_R: f32 = 1.5;
const REST: f32 = 0.8;
const SPRING: f32 = 95_000.0;
const DAMP: f32 = 11_000.0;
const ENGINE: f32 = MASS * 9.0;
const BOOST: f32 = MASS * 32.0;
const GRIP: f32 = 1.15;
const HALF: Vec3 = Vec3::new(1.5, 0.9, 2.9);
pub const WHEELS: [Vec3; 4] =
    [Vec3::new(-2.2, -0.1, -2.4), Vec3::new(2.2, -0.1, -2.4), Vec3::new(-2.2, -0.1, 2.4), Vec3::new(2.2, -0.1, 2.4)];

#[derive(Clone, Copy, Default)]
pub struct Body {
    pub pos: Vec3,
    pub vel: Vec3,
    pub rot: Quat,
    pub ang: Vec3,
}

#[derive(Resource, Default)]
pub struct Truck {
    pub active: bool,
    pub body: Body,
    pub prev: Body,
    pub steer: f32,
    pub wheel_len: [f32; 4],
    pub spin: f32,
    pub boost_fuel: f32,
    pub boosting: bool,
    pub grounded: bool,
    pub waiting_for_ground: bool,
    pub game_over: Option<String>,
    /// Remote trucks this truck touched recently: (player id, seconds ago).
    pub last_contact: Option<(u64, f32)>,
    pub upside_down_for: f32,
    pub game_over_sent: bool,
}

#[derive(Component)]
pub struct TruckRoot;
#[derive(Component)]
pub struct WheelVis(pub usize);
#[derive(Component)]
pub struct Flame;

impl Truck {
    pub fn drop_at(&mut self, pos: Vec3, yaw: f32) {
        self.active = true;
        self.body = Body { pos, vel: Vec3::ZERO, rot: Quat::from_rotation_y(yaw), ang: Vec3::ZERO };
        self.prev = self.body;
        self.boost_fuel = 1.0;
        self.waiting_for_ground = true;
        self.game_over = None;
        self.upside_down_for = 0.0;
        self.game_over_sent = false;
        self.last_contact = None;
    }
    pub fn forward(&self) -> Vec3 {
        self.body.rot * Vec3::NEG_Z
    }
}

/// Spawn the truck model (hidden until dropped). Returns the root entity.
/// `local`: this is the player's own truck (driven by physics); remote trucks get no
/// markers so the local-truck systems never touch them.
pub fn spawn_model(commands: &mut Commands, meshes: &mut Assets<Mesh>, mats: &mut Assets<StandardMaterial>, tint: Color, local: bool) -> Entity {
    let paint = mats.add(StandardMaterial { base_color: tint, metallic: 0.6, perceptual_roughness: 0.3, ..default() });
    let dark = mats.add(StandardMaterial { base_color: Color::srgb(0.05, 0.05, 0.05), perceptual_roughness: 0.9, ..default() });
    let chrome = mats.add(StandardMaterial { base_color: Color::srgb(0.8, 0.8, 0.82), metallic: 1.0, perceptual_roughness: 0.15, ..default() });
    let glass = mats.add(StandardMaterial { base_color: Color::srgb(0.08, 0.12, 0.15), metallic: 0.2, perceptual_roughness: 0.05, ..default() });
    let lamp = mats.add(StandardMaterial { base_color: Color::WHITE, emissive: LinearRgba::rgb(20.0, 18.0, 12.0), ..default() });
    let flame = mats.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.5, 0.1),
        emissive: LinearRgba::rgb(60.0, 18.0, 2.0),
        unlit: true,
        ..default()
    });
    let tire = mats.add(StandardMaterial { base_color: Color::srgb(0.03, 0.03, 0.03), perceptual_roughness: 1.0, ..default() });

    let part = |m: Handle<Mesh>, mat: &Handle<StandardMaterial>, t: Transform| (Mesh3d(m), MeshMaterial3d(mat.clone()), t);
    let root = commands.spawn((Transform::default(), Visibility::Hidden)).id();
    if local {
        commands.entity(root).insert(TruckRoot);
    }
    let mut kids = vec![
        // Frame rails and body tub.
        commands.spawn(part(meshes.add(Cuboid::new(2.2, 0.35, 5.6)), &dark, Transform::from_xyz(0.0, -0.1, 0.0))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(3.0, 0.9, 5.2)), &paint, Transform::from_xyz(0.0, 0.55, 0.1))).id(),
        // Hood bulge + scoop.
        commands.spawn(part(meshes.add(Cuboid::new(1.2, 0.35, 1.6)), &paint, Transform::from_xyz(0.0, 1.15, -1.6))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.7, 0.25, 0.5)), &chrome, Transform::from_xyz(0.0, 1.42, -1.7))).id(),
        // Cab with windows.
        commands.spawn(part(meshes.add(Cuboid::new(2.6, 1.1, 2.0)), &paint, Transform::from_xyz(0.0, 1.55, 0.3))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(2.4, 0.7, 0.05)), &glass, Transform::from_xyz(0.0, 1.65, -0.72))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.05, 0.6, 1.5)), &glass, Transform::from_xyz(-1.31, 1.65, 0.3))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.05, 0.6, 1.5)), &glass, Transform::from_xyz(1.31, 1.65, 0.3))).id(),
        // Roll bar, bumpers, headlights.
        commands.spawn(part(meshes.add(Cuboid::new(2.7, 0.15, 0.15)), &chrome, Transform::from_xyz(0.0, 2.35, 1.4))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.15, 1.2, 0.15)), &chrome, Transform::from_xyz(-1.3, 1.75, 1.4))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.15, 1.2, 0.15)), &chrome, Transform::from_xyz(1.3, 1.75, 1.4))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(3.2, 0.3, 0.3)), &chrome, Transform::from_xyz(0.0, 0.2, -2.85))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(3.2, 0.3, 0.3)), &chrome, Transform::from_xyz(0.0, 0.2, 2.85))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.5, 0.25, 0.1)), &lamp, Transform::from_xyz(-0.9, 0.75, -2.72))).id(),
        commands.spawn(part(meshes.add(Cuboid::new(0.5, 0.25, 0.1)), &lamp, Transform::from_xyz(0.9, 0.75, -2.72))).id(),
    ];
    // Boost flame out the back (scaled by boost).
    kids.push(
        commands
            .spawn((
                Mesh3d(meshes.add(Cone { radius: 0.45, height: 2.5 })),
                MeshMaterial3d(flame),
                Transform::from_xyz(0.0, 0.6, 3.9).with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                Visibility::Hidden,
            ))
            .id(),
    );
    if local {
        commands.entity(*kids.last().unwrap()).insert(Flame);
    }
    // Four monster wheels: tire + green hub + chrome cap.
    let tire_mesh = meshes.add(Cylinder::new(WHEEL_R, 1.35));
    let hub_mesh = meshes.add(Cylinder::new(0.75, 1.4));
    let cap_mesh = meshes.add(Cylinder::new(0.3, 1.45));
    for (i, a) in WHEELS.iter().enumerate() {
        let wheel = commands.spawn((Transform::from_translation(*a), Visibility::Inherited)).id();
        if local {
            commands.entity(wheel).insert(WheelVis(i));
        }
        let axle = Transform::from_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        let t = commands.spawn(part(tire_mesh.clone(), &tire, axle)).id();
        let h = commands.spawn(part(hub_mesh.clone(), &paint, axle)).id();
        let c = commands.spawn(part(cap_mesh.clone(), &chrome, axle)).id();
        commands.entity(wheel).add_children(&[t, h, c]);
        kids.push(wheel);
    }
    commands.entity(root).add_children(&kids);
    root
}

fn ground(terrain: &Terrain, origin: &WorldOrigin, p: Vec3) -> Option<(f32, Vec3)> {
    let (x, y) = (origin.0.x + p.x as f64, origin.0.y - p.z as f64);
    let h = terrain.height_at(x, y)?.0;
    let e = 1.0;
    let hx = terrain.height_at(x + e, y).map(|v| v.0).unwrap_or(h);
    let hz = terrain.height_at(x, y - e).map(|v| v.0).unwrap_or(h); // +Z world = south = −y
    Some((h, Vec3::new(-(hx - h) / e as f32, 1.0, -(hz - h) / e as f32).normalize()))
}

fn inertia_inv_world(rot: Quat) -> Mat3 {
    let s = HALF * 2.0;
    let i = Vec3::new(s.y * s.y + s.z * s.z, s.x * s.x + s.z * s.z, s.x * s.x + s.y * s.y) * (MASS / 12.0) * 1.4;
    let r = Mat3::from_quat(rot);
    r * Mat3::from_diagonal(i.recip()) * r.transpose()
}

pub fn physics(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    terrain: Res<Terrain>,
    origin: Res<WorldOrigin>,
    mut truck: ResMut<Truck>,
    remotes: Res<crate::net::Remotes>,
) {
    let dt = time.delta_secs();
    let t = &mut *truck;
    if !t.active {
        return;
    }
    t.prev = t.body;
    // Freeze in the air until 10 m terrain under the truck has streamed in.
    if t.waiting_for_ground {
        match ground(&terrain, &origin, t.body.pos) {
            Some((h, _)) if terrain.height_at(origin.0.x + t.body.pos.x as f64, origin.0.y - t.body.pos.z as f64).is_some_and(|v| v.1 == 0) => {
                t.body.pos.y = h + 4.0;
                t.prev = t.body;
                t.waiting_for_ground = false;
            }
            _ => return,
        }
    }
    let driving = *mode == Mode::Drive && t.game_over.is_none();
    let key = |k: &[KeyCode]| driving && k.iter().any(|k| keys.pressed(*k));
    let throttle = key(&[KeyCode::KeyW, KeyCode::ArrowUp]) as i32 as f32 - key(&[KeyCode::KeyS, KeyCode::ArrowDown]) as i32 as f32;
    let steer_in = key(&[KeyCode::KeyA, KeyCode::ArrowLeft]) as i32 as f32 - key(&[KeyCode::KeyD, KeyCode::ArrowRight]) as i32 as f32;
    let speed = t.body.vel.length();
    let max_steer = 0.55 / (1.0 + speed / 25.0);
    t.steer += (steer_in * max_steer - t.steer) * (dt * 6.0).min(1.0);
    t.boosting = key(&[KeyCode::Space]) && t.boost_fuel > 0.0;
    t.boost_fuel = if t.boosting { (t.boost_fuel - dt * 0.5).max(0.0) } else { (t.boost_fuel + dt * 0.12).min(1.0) };

    let b = &mut t.body;
    let mut force = Vec3::new(0.0, -GRAVITY * MASS, 0.0);
    let mut torque = Vec3::ZERO;
    let up = b.rot * Vec3::Y;
    let fwd = b.rot * Vec3::NEG_Z;
    let mut contacts = 0;
    let mut n_sum = Vec3::ZERO;
    for (i, a) in WHEELS.iter().enumerate() {
        let anchor = b.pos + b.rot * *a;
        let down = -up;
        let len = REST + WHEEL_R;
        t.wheel_len[i] = REST;
        if -down.y < 0.25 {
            continue;
        }
        let Some((h, n)) = ground(&terrain, &origin, anchor) else { continue };
        let dist = (anchor.y - h) / -down.y;
        if dist >= len {
            continue;
        }
        contacts += 1;
        let comp = len - dist.max(0.0);
        t.wheel_len[i] = (dist - WHEEL_R).clamp(0.0, REST);
        let p = anchor + down * dist;
        let r = p - b.pos;
        let vp = b.vel + b.ang.cross(r);
        let fn_mag = (SPRING * comp - DAMP * vp.dot(n)).max(0.0);
        let f = n * fn_mag;
        // Tire: 4-wheel steer (rear counter-steers half).
        let steer = if i < 2 { t.steer } else { -t.steer * 0.5 };
        let wf = Quat::from_axis_angle(up, steer) * fwd;
        let wf = (wf - n * wf.dot(n)).normalize_or_zero();
        let side = n.cross(wf);
        let v_long = vp.dot(wf);
        let v_lat = vp.dot(side);
        let mut tire = -side * v_lat * MASS * 2.5 / 4.0;
        let drive = if throttle != 0.0 && v_long * throttle < -1.0 {
            throttle * ENGINE * 1.6 / 4.0 // braking against motion
        } else {
            throttle * ENGINE / 4.0
        };
        // Rolling resistance + engine braking when off the throttle.
        let resist = if throttle == 0.0 { 900.0 } else { 60.0 };
        tire += wf * (drive - v_long * resist);
        let cap = GRIP * fn_mag;
        if tire.length() > cap {
            tire = tire.normalize() * cap;
        }
        let r_tire = r - up * r.dot(up) * 0.75;
        force += f + tire;
        torque += r.cross(f) + r_tire.cross(tire);
        n_sum += n;
    }
    t.grounded = contacts > 0;
    // Anti-roll: with wheels down, lean the chassis back toward the ground normal and damp roll.
    if contacts >= 2 {
        let n_avg = n_sum.normalize_or_zero();
        torque += up.cross(n_avg) * MASS * 18.0;
        torque -= fwd * b.ang.dot(fwd) * MASS * 3.0;
    }
    // Body corners never sink into the ground (so a flipped truck slides on its roof).
    for sx in [-1.0, 1.0] {
        for sy in [-1.0, 1.0] {
            for sz in [-1.0, 1.0] {
                let p = b.pos + b.rot * (HALF * Vec3::new(sx, sy, sz) + Vec3::Y * 0.6);
                let Some((h, n)) = ground(&terrain, &origin, p) else { continue };
                let depth = h - p.y;
                if depth > 0.0 {
                    let r = p - b.pos;
                    let vp = b.vel + b.ang.cross(r);
                    let fnm = (depth * 400_000.0 - vp.dot(n) * 30_000.0).max(0.0);
                    let vt = vp - n * vp.dot(n);
                    let f = n * fnm - vt.normalize_or_zero() * (0.6 * fnm).min(vt.length() * MASS * 20.0);
                    force += f;
                    torque += r.cross(f);
                }
            }
        }
    }
    if t.boosting {
        force += fwd * BOOST + Vec3::Y * MASS * 4.0;
    }
    // Air control: pitch with W/S, yaw with A/D, and a gentle self-level so jumps land wheels-down.
    if contacts == 0 {
        let right = b.rot * Vec3::X;
        if driving {
            torque += (right * -throttle * 1.5 + up * steer_in * 1.5) * MASS;
        }
        torque += fwd * fwd.dot(up.cross(Vec3::Y)) * MASS * 6.0;
        torque -= fwd * b.ang.dot(fwd) * MASS * 1.5;
    }
    // Truck-vs-truck: sphere pushes against remote trucks (each client moves only itself).
    for r in remotes.trucks.values() {
        let d = b.pos - r.pos_world;
        let dist = d.length();
        if dist < 5.2 && dist > 1e-3 {
            let n = d / dist;
            let pen = 5.2 - dist;
            let rel = b.vel - r.vel;
            let f = n * (pen * 350_000.0 - rel.dot(n).min(0.0) * 25_000.0);
            force += f;
            // Contact above our centre lifts one side: this is what flips trucks.
            let lever = Vec3::new(-n.x, 0.0, -n.z) * 1.2 + Vec3::Y * -0.5;
            torque += lever.cross(f) * 0.6;
            t.last_contact = Some((r.id, 0.0));
        }
    }
    let inv_i = inertia_inv_world(b.rot);
    b.vel += force / MASS * dt;
    b.vel *= 1.0 - 0.02 * dt;
    b.ang += inv_i * torque * dt;
    b.ang *= 1.0 - 0.6 * dt;
    b.pos += b.vel * dt;
    b.rot = (Quat::from_scaled_axis(b.ang * dt) * b.rot).normalize();
    t.spin += (b.vel.dot(fwd)) * dt / WHEEL_R;

    // GAME OVER: upside down for 1.5 s within 4 s of touching another truck.
    if let Some((_, age)) = &mut t.last_contact {
        *age += dt;
        if *age > 4.0 {
            t.last_contact = None;
        }
    }
    let upside = (t.body.rot * Vec3::Y).y < -0.2;
    t.upside_down_for = if upside { t.upside_down_for + dt } else { 0.0 };
    if t.game_over.is_none() && t.upside_down_for > 1.5 {
        if let Some((id, _)) = t.last_contact {
            let by = remotes.trucks.get(&id).map(|r| r.name.clone()).unwrap_or_else(|| format!("player {id}"));
            t.game_over = Some(by);
        }
    }
}

pub fn reset(keys: Res<ButtonInput<KeyCode>>, mode: Res<Mode>, mut truck: ResMut<Truck>) {
    if *mode == Mode::Drive && keys.just_pressed(KeyCode::KeyR) {
        let fwd = truck.forward();
        let yaw = fwd.x.atan2(fwd.z) + std::f32::consts::PI;
        let pos = truck.body.pos + Vec3::Y * 3.0;
        truck.drop_at(pos, yaw);
    }
}

/// Interpolate the fixed-step body into the rendered transform, pose wheels and flame.
pub fn sync_model(
    fixed: Res<Time<Fixed>>,
    truck: Res<Truck>,
    mut root: Query<(&mut Transform, &mut Visibility), (With<TruckRoot>, Without<WheelVis>, Without<Flame>)>,
    mut wheels: Query<(&WheelVis, &mut Transform), (Without<TruckRoot>, Without<Flame>)>,
    mut flame: Query<(&mut Transform, &mut Visibility), (With<Flame>, Without<TruckRoot>, Without<WheelVis>)>,
) {
    let Ok((mut tf, mut vis)) = root.single_mut() else { return };
    *vis = if truck.active { Visibility::Inherited } else { Visibility::Hidden };
    let a = fixed.overstep_fraction();
    tf.translation = truck.prev.pos.lerp(truck.body.pos, a);
    tf.rotation = truck.prev.rot.slerp(truck.body.rot, a);
    for (w, mut t) in &mut wheels {
        let base = WHEELS[w.0];
        t.translation = base - Vec3::Y * truck.wheel_len[w.0];
        let steer = if w.0 < 2 { truck.steer } else { -truck.steer * 0.5 };
        t.rotation = Quat::from_rotation_y(steer) * Quat::from_rotation_x(-truck.spin);
    }
    if let Ok((mut t, mut v)) = flame.single_mut() {
        *v = if truck.boosting { Visibility::Inherited } else { Visibility::Hidden };
        let flicker = 1.0 + (truck.spin * 7.0).sin() * 0.15;
        t.scale = Vec3::new(1.0, 1.0 + flicker, 1.0);
    }
}
