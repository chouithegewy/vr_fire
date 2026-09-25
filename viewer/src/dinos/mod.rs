//! Dinosaur mode (docs/superpowers/specs/2026-09-25-dinosaur-mode-design.md).
//!
//! J toggles roaming dinosaurs around the truck; Q readies the truck's lasso. Everything is
//! local to this client: herds come from a shared per-cell seed, so players start with the
//! same herds, but behaviour and hog-ties aren't synchronised.

pub mod brain;
pub mod lasso;
pub mod spawn;
pub mod species;

use crate::terrain::Terrain;
use crate::truck::Truck;
use crate::{Mode, WorldOrigin, world_pos};
use bevy::math::DVec2;
use bevy::prelude::*;
use brain::{Agent, State, TruckView};
use lasso::{Candidate, Event, Lasso, Stage};
use species::{Joint, Part, Shape, Species, Tone};
use std::collections::{HashMap, HashSet};
use std::f32::consts::FRAC_PI_2;

/// A dinosaur's stable identity: its home cell and index in that cell's herd.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DinoId {
    pub cx: i64,
    pub cy: i64,
    pub n: u8,
}

const MAX_ALIVE: usize = 40;
const SPAWN_M: f64 = 1500.0;
const DESPAWN_M: f64 = 2000.0;
const MAX_TIED_IDS: usize = 500;
const MAX_SPAWN_SLOPE: f32 = 0.58; // 30°
const THINK_S: f32 = 0.2;
/// Where the lasso pole's tip sits on the truck (local frame).
pub const POLE_TIP: Vec3 = Vec3::new(0.9, 2.55, 2.3);

pub struct DinoPlugin;

impl Plugin for DinoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Dinos>().init_resource::<Obstacles>().add_systems(Startup, setup).add_systems(
            Update,
            (keys, populate, simulate, lasso_system, animate, lasso_visuals).chain().after(crate::truck::sync_model),
        );
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(Update, pending_shot);
    }
}

/// Dinosaurs the truck physics collides with this frame (world frame).
#[derive(Resource, Default)]
pub struct Obstacles(pub Vec<Obstacle>);

#[derive(Clone, Copy)]
pub struct Obstacle {
    pub pos: Vec3,
    pub radius: f32,
    pub vel: Vec3,
    pub heavy: bool,
    pub name: &'static str,
}

struct Visual {
    root: Entity,
    parts: Vec<(Entity, Part)>,
    band: Option<Entity>,
}

#[derive(Resource, Default)]
pub struct Dinos {
    pub enabled: bool,
    pub agents: Vec<Agent>,
    visuals: HashMap<DinoId, Visual>,
    cells: HashSet<(i64, i64)>,
    tied_ids: HashSet<DinoId>,
    pub lasso: Lasso,
    think_t: f32,
    spawn_t: f32,
    /// A short message for the HUD and how long it has been shown.
    pub message: Option<(String, f32)>,
    demo: bool,
    /// Autopilot screenshot to take once the pose has updated: (name, seconds left).
    pending_shot: Option<(&'static str, f32)>,
    /// Demo fixture: the truck's recent positions (every 0.1 s), whose centre the tame
    /// stegosaurus follows so it stays inside the autopilot's drifting circles.
    demo_trail: std::collections::VecDeque<DVec2>,
    demo_trail_t: f32,
}

impl Dinos {
    pub fn nearby(&self) -> usize {
        self.agents.len()
    }

    /// Nearest dinosaur that isn't hog-tied or down, from `here` (EPSG:5070).
    pub fn nearest(&self, here: DVec2) -> Option<(&'static str, f64, &'static str)> {
        self.agents
            .iter()
            .filter(|a| !matches!(a.state, State::Tied | State::Down))
            .map(|a| (a, a.pos.distance(here)))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .map(|(a, d)| (a.species.def().name, d, compass(a.pos - here)))
    }

    /// One-line HUD status, e.g. `DINOS 23 nearby | hog-tied 3 | lasso READY -> Stegosaurus loop 210/360`.
    pub fn hud(&self, here: DVec2) -> String {
        let target = self.lasso.target.and_then(|id| self.agents.iter().find(|a| a.id == id));
        let deg = (self.lasso.progress() * 360.0).round();
        let lasso = match (self.lasso.stage, target) {
            (Stage::Stowed, _) => "stowed (Q)".to_string(),
            (Stage::Ready, None) => "READY - get close to a dinosaur".to_string(),
            (Stage::Ready, Some(a)) => format!("READY -> {} loop {deg}/360 (circle it)", a.species.def().name),
            (Stage::Connected, Some(a)) => format!("NETTED {} loop {deg}/360 (circle again to tighten)", a.species.def().name),
            (Stage::Connected, None) => "NETTED".to_string(),
            (Stage::Cinch(_), _) => "tightening...".to_string(),
        };
        let mut s = format!("DINOS {} nearby | hog-tied {} | lasso {lasso}", self.nearby(), self.lasso.tied);
        match self.nearest(here) {
            Some((name, d, dir)) => s += &format!(" | nearest: {name} {} {dir}", if d >= 1000.0 { format!("{:.1} km", d / 1000.0) } else { format!("{d:.0} m") }),
            None if self.agents.is_empty() => s += " | none within 1.5 km yet (drive on)",
            None => {}
        }
        if let Some((m, _)) = &self.message {
            s += &format!("\n{m}");
        }
        s
    }
}

/// Compass bearing (N, NE, ...) of an EPSG:5070 offset (x east, y north).
pub fn compass(d: DVec2) -> &'static str {
    const NAMES: [&str; 8] = ["E", "NE", "N", "NW", "W", "SW", "S", "SE"];
    let octant = (d.y.atan2(d.x) / (std::f64::consts::PI / 4.0)).round().rem_euclid(8.0) as usize;
    NAMES[octant % 8]
}

#[derive(Component)]
struct DinoWorld;
#[derive(Component)]
struct Rope;
#[derive(Component)]
struct TargetRing;
/// The truck's lasso coil and its spinning loop (local truck only).
#[derive(Component)]
pub struct LassoCoil;
#[derive(Component)]
pub struct LassoSpin;

#[derive(Resource)]
struct DinoAssets {
    meshes: HashMap<Species, Vec<Handle<Mesh>>>,
    tones: HashMap<(Species, Tone), Handle<StandardMaterial>>,
    band_mesh: Handle<Mesh>,
    rope_mat: Handle<StandardMaterial>,
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mut by_species = HashMap::new();
    let mut tones = HashMap::new();
    for s in Species::ALL {
        let handles = species::parts(s)
            .iter()
            .map(|p| match p.shape {
                Shape::Box(size) => meshes.add(Cuboid::from_size(size)),
                Shape::Cone { radius, height } => meshes.add(Cone { radius, height }),
            })
            .collect();
        by_species.insert(s, handles);
        let d = s.def();
        for (tone, c) in [(Tone::Skin, d.skin), (Tone::Belly, d.belly), (Tone::Accent, d.accent)] {
            let m = mats.add(StandardMaterial { base_color: Color::srgb(c[0], c[1], c[2]), perceptual_roughness: 0.85, ..default() });
            tones.insert((s, tone), m);
        }
    }
    let rope_mat = mats.add(StandardMaterial { base_color: Color::srgb(0.95, 0.8, 0.25), perceptual_roughness: 0.9, ..default() });
    commands.spawn((DinoWorld, Transform::default(), Visibility::Hidden));
    commands.spawn((
        Rope,
        Mesh3d(meshes.add(Cylinder::new(0.09, 1.0))),
        MeshMaterial3d(rope_mat.clone()),
        Transform::default(),
        Visibility::Hidden,
    ));
    let ring_mat = mats.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.85, 0.2, 0.9),
        emissive: LinearRgba::rgb(4.0, 3.0, 0.5),
        unlit: true,
        ..default()
    });
    commands.spawn((
        TargetRing,
        Mesh3d(meshes.add(Torus::new(0.9, 1.0))),
        MeshMaterial3d(ring_mat),
        Transform::default(),
        Visibility::Hidden,
    ));
    commands.insert_resource(DinoAssets { meshes: by_species, tones, band_mesh: meshes.add(Cuboid::new(1.0, 0.35, 0.7)), rope_mat });
}

fn truck_albers(origin: &WorldOrigin, p: Vec3) -> DVec2 {
    DVec2::new(origin.0.x + p.x as f64, origin.0.y - p.z as f64)
}

fn truck_view(truck: &Truck, origin: &WorldOrigin) -> Option<TruckView> {
    truck.active.then(|| {
        let v = truck.body.vel;
        TruckView { pos: truck_albers(origin, truck.body.pos), vel: DVec2::new(v.x as f64, -v.z as f64) }
    })
}

fn despawn_all(d: &mut Dinos, commands: &mut Commands) {
    for (_, v) in d.visuals.drain() {
        commands.entity(v.root).despawn();
    }
    d.agents.clear();
    d.cells.clear();
    d.lasso = Lasso::with_tally(d.lasso.tied);
}

fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<Mode>,
    truck: Res<Truck>,
    mut d: ResMut<Dinos>,
    mut commands: Commands,
    mut world: Query<&mut Visibility, With<DinoWorld>>,
) {
    if keys.just_pressed(KeyCode::KeyJ) {
        d.enabled = !d.enabled;
        if !d.enabled {
            despawn_all(&mut d, &mut commands);
        }
        let on = d.enabled;
        d.message = Some((if on { "Dinosaur mode ON (J)".into() } else { "Dinosaur mode off".into() }, 0.0));
        #[cfg(not(target_arch = "wasm32"))]
        {
            d.demo = on && std::env::var("VR_FIRE_AUTOPILOT_DINOS").is_ok();
        }
    }
    if d.enabled && *mode == Mode::Drive && truck.active && keys.just_pressed(KeyCode::KeyQ) {
        d.lasso.toggle();
    }
    if let Ok(mut v) = world.single_mut() {
        let want = if d.enabled && *mode == Mode::Drive && truck.active { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}

fn spawn_visual(
    commands: &mut Commands,
    assets: &DinoAssets,
    mats: &mut Assets<StandardMaterial>,
    world: Entity,
    a: &Agent,
) -> Visual {
    let d = a.species.def();
    // Per-animal skin variation; belly and accent are shared per species.
    let mut r = spawn::Rng(a.rng.0 ^ 0xc0107);
    let k = 0.85 + 0.3 * r.f64() as f32;
    let skin = mats.add(StandardMaterial {
        base_color: Color::srgb((d.skin[0] * k).min(1.0), (d.skin[1] * (k * 0.95 + 0.05)).min(1.0), (d.skin[2] * k).min(1.0)),
        perceptual_roughness: 0.85,
        ..default()
    });
    let root = commands.spawn((Transform::default(), Visibility::Inherited)).id();
    commands.entity(world).add_child(root);
    let parts: Vec<(Entity, Part)> = species::parts(a.species)
        .into_iter()
        .zip(&assets.meshes[&a.species])
        .map(|(p, mesh)| {
            let mat = if p.tone == Tone::Skin { skin.clone() } else { assets.tones[&(a.species, p.tone)].clone() };
            let e = commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), part_transform(&p, 0.0, a))).id();
            commands.entity(root).add_child(e);
            (e, p)
        })
        .collect();
    Visual { root, parts, band: None }
}

/// A part's transform in the animal's frame for the current pose.
fn part_transform(p: &Part, t: f32, a: &Agent) -> Transform {
    let d = a.species.def();
    let joint = match p.joint {
        Joint::Fixed => Quat::IDENTITY,
        Joint::Leg(phase) => match a.state {
            // Bound: front legs pulled back, rear legs forward, feet together.
            State::Tied => Quat::from_rotation_x(if p.pivot.z < 0.0 { -0.6 } else { 0.6 }),
            State::Down => Quat::from_rotation_x(0.3),
            _ => {
                let amp = 0.35 * (a.speed / d.walk).clamp(0.0, 1.6);
                Quat::from_rotation_x((a.phase + phase).sin() * amp)
            }
        },
        Joint::Head => {
            let pitch = match a.state {
                State::Eat | State::Graze => {
                    let down = if a.species == Species::Brachio { -0.35 } else { -0.55 };
                    down + (t * 6.0).sin() * 0.06
                }
                State::Netted => (t * 9.0).sin() * 0.25,
                State::Tied | State::Down => -0.2,
                _ => (a.phase * 2.0).sin() * 0.05,
            };
            Quat::from_rotation_x(pitch)
        }
        Joint::Tail => {
            let sway = if matches!(a.state, State::Tied | State::Down) { 0.05 } else { 0.15 };
            Quat::from_rotation_y((t * 1.3 + a.phase * 0.5).sin() * sway)
        }
    };
    Transform::from_translation(p.pivot + joint * p.offset).with_rotation(joint * p.rot)
}

/// Spawn herds for nearby cells, remove far ones (every 0.5 s).
#[allow(clippy::too_many_arguments)]
fn populate(
    time: Res<Time>,
    truck: Res<Truck>,
    origin: Res<WorldOrigin>,
    terrain: Res<Terrain>,
    assets: Option<Res<DinoAssets>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut d: ResMut<Dinos>,
    mut commands: Commands,
    world: Query<Entity, With<DinoWorld>>,
) {
    let (Some(assets), Ok(world)) = (assets, world.single()) else { return };
    let d = &mut *d;
    if !d.enabled || !truck.active || truck.waiting_for_ground {
        return;
    }
    // Animals brought down long enough ago disappear (their cell doesn't respawn them).
    for a in d.agents.iter().filter(|a| a.gone) {
        if let Some(v) = d.visuals.remove(&a.id) {
            commands.entity(v.root).despawn();
        }
    }
    d.agents.retain(|a| !a.gone);

    d.spawn_t -= time.delta_secs();
    if d.spawn_t > 0.0 {
        return;
    }
    d.spawn_t = 0.5;
    let here = truck_albers(&origin, truck.body.pos);

    if d.demo && d.agents.is_empty() && d.cells.is_empty() {
        spawn_demo(d, &truck, here, &terrain, &assets, &mut mats, &mut commands, world);
    }

    // Unload far cells.
    let far: Vec<(i64, i64)> = d.cells.iter().copied().filter(|&(cx, cy)| spawn::cell_centre(cx, cy).distance(here) > DESPAWN_M).collect();
    for cell in far {
        d.cells.remove(&cell);
        for a in d.agents.iter().filter(|a| (a.id.cx, a.id.cy) == cell) {
            if let Some(v) = d.visuals.remove(&a.id) {
                commands.entity(v.root).despawn();
            }
        }
        d.agents.retain(|a| (a.id.cx, a.id.cy) != cell);
    }
    if d.demo {
        return;
    }
    let height = |p: DVec2| terrain.height_at(p.x, p.y).map(|v| v.0);
    for (cx, cy) in spawn::cells_near(here, SPAWN_M) {
        if d.cells.contains(&(cx, cy)) {
            continue;
        }
        if d.agents.len() >= MAX_ALIVE {
            break;
        }
        let Some(herd) = spawn::herd(cx, cy) else {
            d.cells.insert((cx, cy));
            continue;
        };
        // Wait until the ground under the herd has streamed in.
        if height(herd.centre).is_none() {
            continue;
        }
        d.cells.insert((cx, cy));
        for (n, &p) in herd.members.iter().enumerate() {
            if d.agents.len() >= MAX_ALIVE {
                break;
            }
            let Some(h) = height(p) else { continue };
            let slope = [DVec2::X, DVec2::Y].iter().filter_map(|o| height(p + *o * 5.0)).map(|h2| (h2 - h).abs() / 5.0).fold(0.0, f32::max);
            if h < -100.0 || slope > MAX_SPAWN_SLOPE {
                continue; // water or too steep
            }
            let id = DinoId { cx, cy, n: n as u8 };
            let mut a = Agent::new(id, herd.species, p, herd.centre);
            a.h = h;
            if d.tied_ids.contains(&id) {
                a.state = State::Tied;
            }
            let v = spawn_visual(&mut commands, &assets, &mut mats, world, &a);
            d.visuals.insert(id, v);
            d.agents.push(a);
        }
    }
}

/// Autopilot demo: a tame stegosaurus just left of the truck (inside a hard-left circle), and
/// a small herd ahead for the screenshot.
#[allow(clippy::too_many_arguments)]
fn spawn_demo(
    d: &mut Dinos,
    truck: &Truck,
    here: DVec2,
    terrain: &Terrain,
    assets: &DinoAssets,
    mats: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
    world: Entity,
) {
    let fwd = truck.forward();
    // EPSG:5070 forward (world −Z is north) and its left (counter-clockwise, where A turns).
    let f = DVec2::new(fwd.x as f64, -fwd.z as f64).normalize_or(DVec2::Y);
    let left = DVec2::new(-f.y, f.x);
    let (cx, cy) = spawn::cell_of(here);
    let cast = [
        (Species::Stego, here + left * 9.0),
        (Species::Trike, here + f * 70.0 + left * -25.0),
        (Species::Brachio, here + f * 110.0 + left * 30.0),
        (Species::Raptor, here + f * 140.0 - left * 60.0),
    ];
    for (n, (s, p)) in cast.into_iter().enumerate() {
        let Some((h, _)) = terrain.height_at(p.x, p.y) else { continue };
        let id = DinoId { cx, cy, n: 200 + n as u8 };
        let mut a = Agent::new(id, s, p, p);
        a.h = h;
        if n == 0 {
            a.state = State::Graze;
            a.timer = 1e9; // stays put until netted
        }
        let v = spawn_visual(commands, assets, mats, world, &a);
        d.visuals.insert(id, v);
        d.agents.push(a);
    }
    d.cells.insert((cx, cy));
}

fn simulate(
    time: Res<Time>,
    truck: Res<Truck>,
    origin: Res<WorldOrigin>,
    terrain: Res<Terrain>,
    mut d: ResMut<Dinos>,
    mut obstacles: ResMut<Obstacles>,
) {
    obstacles.0.clear();
    let d = &mut *d;
    let dt = time.delta_secs().min(0.1);
    if let Some((_, age)) = &mut d.message {
        *age += dt;
        if *age > 6.0 {
            d.message = None;
        }
    }
    if !d.enabled || d.agents.is_empty() {
        return;
    }
    let view = truck_view(&truck, &origin);
    d.think_t -= dt;
    if d.think_t <= 0.0 {
        d.think_t = THINK_S;
        brain::think(&mut d.agents, view);
    }
    let ground = |p: DVec2| terrain.height_at(p.x, p.y).map(|v| v.0);
    brain::step(&mut d.agents, dt, view, &ground);
    if d.demo {
        demo_fixture(d, view, dt);
    }

    for a in &mut d.agents {
        let def = a.species.def();
        let mut centre = world_pos(&origin, a.pos.x, a.pos.y, a.h) + Vec3::Y * def.neck * 0.5;
        // Animals never stay inside the truck: shove them out so penetration can't build up
        // (heavy ones still push back on the truck through `Obstacles`, with the small remainder).
        if truck.active && !matches!(a.state, State::Tied | State::Down) {
            let off = centre - truck.body.pos;
            let reach = def.radius + 2.6;
            let flat = Vec2::new(off.x, off.z);
            if flat.length() < reach && flat.length() > 1e-3 {
                let keep = if def.heavy { 0.1 } else { 0.0 };
                let push = flat.normalize() * (reach - flat.length()) * (1.0 - keep);
                a.pos += DVec2::new(push.x as f64, -push.y as f64);
                centre += Vec3::new(push.x, 0.0, push.y);
            }
        }
        if !def.heavy {
            continue;
        }
        let (radius, vel) = match a.state {
            State::Tied | State::Down => (def.radius * 0.7, Vec3::ZERO),
            _ => {
                let dir = Vec3::new(a.heading.cos() as f32, 0.0, -a.heading.sin() as f32);
                (def.radius, dir * a.speed)
            }
        };
        obstacles.0.push(Obstacle { pos: centre, radius, vel, heavy: def.heavy, name: def.name });
    }
}

/// Autopilot demo only: keep the tame stegosaurus near the centre of the truck's last lap.
fn demo_fixture(d: &mut Dinos, view: Option<TruckView>, dt: f32) {
    let Some(t) = view else { return };
    d.demo_trail_t -= dt;
    if d.demo_trail_t <= 0.0 {
        d.demo_trail_t = 0.1;
        d.demo_trail.push_back(t.pos);
        if d.demo_trail.len() > 140 {
            d.demo_trail.pop_front();
        }
    }
    if d.demo_trail.len() < 100 {
        return;
    }
    let centre = d.demo_trail.iter().copied().sum::<DVec2>() / d.demo_trail.len() as f64;
    if let Some(a) = d.agents.iter_mut().find(|a| a.id.n == 200 && matches!(a.state, State::Graze | State::Netted)) {
        let to = centre - a.pos;
        let step = (2.0 * dt as f64).min(to.length());
        a.pos += to.normalize_or_zero() * step;
    }
}

fn lasso_system(
    time: Res<Time>,
    mode: Res<Mode>,
    truck: Res<Truck>,
    origin: Res<WorldOrigin>,
    mut d: ResMut<Dinos>,
) {
    let d = &mut *d;
    if !d.enabled || !truck.active || *mode != Mode::Drive {
        return;
    }
    let here = truck_albers(&origin, truck.body.pos);
    let speed = Vec2::new(truck.body.vel.x, truck.body.vel.z).length();
    let candidates: Vec<Candidate> = d
        .agents
        .iter()
        .map(|a| Candidate {
            id: a.id,
            pos: a.pos,
            radius: a.species.lasso_radius() as f64,
            rope: a.species.rope_length() as f64,
            breaks_free_after: (a.species == Species::TRex).then_some(20.0),
            tieable: !matches!(a.state, State::Tied | State::Down),
        })
        .collect();
    let Some(event) = d.lasso.update(time.delta_secs(), here, speed, &candidates) else { return };
    let name = |d: &Dinos, id: DinoId| d.agents.iter().find(|a| a.id == id).map_or("dinosaur", |a| a.species.def().name);
    let msg = match event {
        Event::Connected(id) => {
            if let Some(a) = d.agents.iter_mut().find(|a| a.id == id) {
                a.state = State::Netted;
                a.timer = 0.0;
            }
            Some(format!("Netted a {}! Circle it again to tighten.", name(d, id)))
        }
        Event::Tied(id) => {
            if let Some(a) = d.agents.iter_mut().find(|a| a.id == id) {
                a.state = State::Tied;
                a.speed = 0.0;
            }
            if d.tied_ids.len() < MAX_TIED_IDS {
                d.tied_ids.insert(id);
            }
            Some(format!("Hog-tied a {}!", name(d, id)))
        }
        Event::Snapped(id) => {
            if let Some(a) = d.agents.iter_mut().find(|a| a.id == id) {
                a.state = State::Flee;
                a.goal = here;
                a.timer = 4.0;
            }
            Some(format!("The {} broke the rope!", name(d, id)))
        }
        Event::Stowed => None,
    };
    if let Some(m) = msg {
        info!("dinos: {m}");
        d.message = Some((m, 0.0));
    }
    // Autopilot evidence: screenshot the netted and hog-tied moments.
    #[cfg(not(target_arch = "wasm32"))]
    if d.demo && std::env::var("VR_FIRE_AUTOPILOT").is_ok() {
        let shot = match event {
            Event::Connected(_) => Some("8_netted"),
            Event::Tied(_) => Some("9_hogtied"),
            _ => None,
        };
        if let Some(name) = shot {
            d.pending_shot = Some((name, 0.7));
        }
    }
}

/// Take a pending autopilot screenshot once the new pose has been drawn.
#[cfg(not(target_arch = "wasm32"))]
fn pending_shot(time: Res<Time>, mut d: ResMut<Dinos>, mut commands: Commands) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    let Some((name, left)) = d.pending_shot else { return };
    let left = left - time.delta_secs();
    if left > 0.0 {
        d.pending_shot = Some((name, left));
        return;
    }
    d.pending_shot = None;
    if let Ok(dir) = std::env::var("VR_FIRE_AUTOPILOT") {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{dir}/{name}.png")));
    }
}

/// Place and pose every animal; add the bound-legs band to hog-tied ones.
fn animate(
    time: Res<Time>,
    origin: Res<WorldOrigin>,
    assets: Option<Res<DinoAssets>>,
    mut d: ResMut<Dinos>,
    mut tfs: Query<&mut Transform>,
    mut commands: Commands,
) {
    let Some(assets) = assets else { return };
    let t = time.elapsed_secs();
    let d = &mut *d;
    for a in &d.agents {
        let Some(v) = d.visuals.get_mut(&a.id) else { continue };
        let def = a.species.def();
        let yaw = Quat::from_rotation_y((-(a.heading.cos()) as f32).atan2(a.heading.sin() as f32));
        let lying = matches!(a.state, State::Tied | State::Down);
        let mut root = Transform::from_translation(world_pos(&origin, a.pos.x, a.pos.y, a.h)).with_rotation(yaw);
        if lying {
            // On its side, raised by half the body width.
            root.rotation = yaw * Quat::from_rotation_z(FRAC_PI_2);
            root.translation.y += def.radius * 0.45;
        }
        if let Ok(mut tf) = tfs.get_mut(v.root) {
            *tf = root;
        }
        for (e, p) in &v.parts {
            if let Ok(mut tf) = tfs.get_mut(*e) {
                *tf = part_transform(p, t, a);
            }
        }
        if a.state == State::Tied && v.band.is_none() {
            // A band of rope around the bound legs.
            let legs: Vec<&Part> = v.parts.iter().map(|(_, p)| p).filter(|p| matches!(p.joint, Joint::Leg(_))).collect();
            let hip = legs.iter().map(|p| p.pivot.y).fold(0.0, f32::max);
            let width = legs.iter().map(|p| p.pivot.x.abs()).fold(0.0, f32::max) * 2.0 + 0.6;
            let band = commands
                .spawn((
                    Mesh3d(assets.band_mesh.clone()),
                    MeshMaterial3d(assets.rope_mat.clone()),
                    Transform::from_xyz(0.0, hip * 0.25, 0.0).with_scale(Vec3::new(width, 1.0, (def.length * 0.25).max(0.8))),
                ))
                .id();
            commands.entity(v.root).add_child(band);
            v.band = Some(band);
        }
    }
}

/// Rope from the truck's pole to a netted animal, the target ring, and the coil's glow/spin.
#[allow(clippy::type_complexity)]
fn lasso_visuals(
    time: Res<Time>,
    mode: Res<Mode>,
    truck: Res<Truck>,
    origin: Res<WorldOrigin>,
    d: Res<Dinos>,
    mut rope: Query<(&mut Transform, &mut Visibility), (With<Rope>, Without<TargetRing>, Without<LassoSpin>)>,
    mut ring: Query<(&mut Transform, &mut Visibility), (With<TargetRing>, Without<Rope>, Without<LassoSpin>)>,
    mut spin: Query<(&mut Transform, &mut Visibility), (With<LassoSpin>, Without<Rope>, Without<TargetRing>)>,
    coil: Query<&MeshMaterial3d<StandardMaterial>, With<LassoCoil>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let live = d.enabled && truck.active && *mode == Mode::Drive;
    let target = live.then(|| d.lasso.target.and_then(|id| d.agents.iter().find(|a| a.id == id))).flatten();
    let ready = live && d.lasso.stage != Stage::Stowed;
    if let Ok((mut tf, mut vis)) = spin.single_mut() {
        *vis = if ready && d.lasso.stage == Stage::Ready { Visibility::Inherited } else { Visibility::Hidden };
        tf.rotation = Quat::from_rotation_y(time.elapsed_secs() * 7.0);
    }
    if let Ok(m) = coil.single() {
        if let Some(mut mat) = mats.get_mut(&m.0) {
            let glow = if ready { 2.5 + (time.elapsed_secs() * 5.0).sin() } else { 0.0 };
            mat.emissive = LinearRgba::rgb(glow, glow * 0.8, glow * 0.2);
        }
    }
    let connected = matches!(d.lasso.stage, Stage::Connected | Stage::Cinch(_));
    if let Ok((mut tf, mut vis)) = rope.single_mut() {
        match target.filter(|_| connected) {
            Some(a) => {
                let from = truck.body.pos + truck.body.rot * POLE_TIP;
                let to = world_pos(&origin, a.pos.x, a.pos.y, a.h) + Vec3::Y * a.species.def().neck;
                let span = to - from;
                *tf = Transform::from_translation((from + to) / 2.0)
                    .with_rotation(Quat::from_rotation_arc(Vec3::Y, span.normalize_or(Vec3::Y)))
                    .with_scale(Vec3::new(1.0, span.length(), 1.0));
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
    if let Ok((mut tf, mut vis)) = ring.single_mut() {
        match target.filter(|_| d.lasso.stage == Stage::Ready) {
            Some(a) => {
                let r = a.species.def().radius + 1.5;
                *tf = Transform::from_translation(world_pos(&origin, a.pos.x, a.pos.y, a.h + 0.4))
                    .with_scale(Vec3::new(r, 0.3, r))
                    .with_rotation(Quat::from_rotation_y(time.elapsed_secs()));
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compass_names_the_eight_directions() {
        let cases = [((0.0, 1.0), "N"), ((1.0, 1.0), "NE"), ((1.0, 0.0), "E"), ((1.0, -1.0), "SE"), ((0.0, -1.0), "S"), ((-1.0, -1.0), "SW"), ((-1.0, 0.0), "W"), ((-1.0, 1.0), "NW"), ((0.2, 1.0), "N")];
        for ((x, y), want) in cases {
            assert_eq!(compass(DVec2::new(x, y)), want, "{x},{y}");
        }
    }
}
