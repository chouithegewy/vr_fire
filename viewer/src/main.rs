//! vr_fire terrain viewer: all of California streamed from USGS 3DEP, zoomable from the
//! whole state down to 10 m, with a droppable green monster truck (Space = MEGA BOOST)
//! and multiplayer through a WebSocket relay. Native and wasm (WebGPU / WebGL2).

mod cog;
mod net;
mod terrain;
mod truck;

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::light::CascadeShadowConfigBuilder;
use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};
use terrain::Terrain;
use truck::Truck;

/// EPSG:5070 coordinate of world (0, _, 0). World: +X east, +Y up, +Z south.
#[derive(Resource)]
pub struct WorldOrigin(pub DVec2);

/// EPSG:5070 NW corner of an entity placed relative to the world origin.
#[derive(Component)]
pub struct Anchor(pub DVec2);

#[derive(Resource, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Mode {
    Map,
    Drive,
}

pub fn world_pos(o: &WorldOrigin, x: f64, y: f64, h: f32) -> Vec3 {
    Vec3::new((x - o.0.x) as f32, h, (o.0.y - y) as f32)
}

#[derive(Resource)]
struct MapCam {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    dist: f32,
    fly: Option<(Vec3, f32, f32)>,
}

#[derive(Resource)]
struct ChaseCam {
    dist: f32,
    yaw: f32,
    pitch: f32,
}

#[derive(Resource, Default)]
struct Drag {
    start: Option<Vec2>,
    moved: f32,
    /// Set by the native autopilot to drop the truck without a mouse.
    auto_drop: Option<Vec3>,
}

/// `VR_FIRE_AUTOPILOT=<dir>`: scripted fly-in, drop, drive and boost with screenshots, then exit.
#[cfg(not(target_arch = "wasm32"))]
fn autopilot(
    time: Res<Time>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut drag: ResMut<Drag>,
    map: Res<MapCam>,
    truck: Res<Truck>,
    mut chase: ResMut<ChaseCam>,
    mut commands: Commands,
    mut step: Local<usize>,
    mut exit: MessageWriter<AppExit>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    let Ok(dir) = std::env::var("VR_FIRE_AUTOPILOT") else { return };
    let t = time.elapsed_secs();
    if (t * 2.0) as u32 != ((t - time.delta_secs()) * 2.0) as u32 && (t as u32) % 3 == 0 {
        info!("autopilot t={t:.1}s step={} truck={:?}", *step, truck.body.pos);
    }
    let shot =|commands: &mut Commands, name: &str| {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{dir}/{name}.png")));
    };
    // (time, action)
    let plan: [(f32, u8); 12] = [(8.0, 0), (9.0, 1), (20.0, 2), (21.0, 3), (30.0, 4), (30.5, 5), (34.5, 6), (35.0, 7), (37.0, 8), (38.0, 9), (41.0, 10), (43.0, 11)];
    while *step < plan.len() && t >= plan[*step].0 {
        match plan[*step].1 {
            0 => shot(&mut commands, "1_california"),
            1 => keys.press(KeyCode::Digit5),
            2 => shot(&mut commands, "2_zoomed_placerville"),
            3 => drag.auto_drop = Some(map.focus),
            4 => shot(&mut commands, "3_dropped"),
            5 => keys.press(KeyCode::KeyW),
            6 => shot(&mut commands, "4_driving"),
            7 => keys.press(KeyCode::Space),
            8 => shot(&mut commands, "5_mega_boost"),
            9 => {
                keys.release(KeyCode::Space);
                keys.press(KeyCode::KeyA);
                chase.dist = 40.0;
            }
            10 => shot(&mut commands, "6_turning"),
            _ => {
                info!("autopilot done: truck at {:?} speed {:.1}", truck.body.pos, truck.body.vel.length());
                exit.write(AppExit::Success);
            }
        }
        *step += 1;
    }
    if (1..3).contains(&*step) {
        keys.release(KeyCode::Digit5);
    }
}

#[derive(Component)]
struct Hud;
#[derive(Component)]
struct Banner;
#[derive(Component)]
struct Water;

/// Named places: (key, name, lon, lat, view distance m).
const PLACES: [(KeyCode, &str, f64, f64, f32); 6] = [
    (KeyCode::Digit1, "Yosemite Valley", -119.59, 37.74, 9_000.0),
    (KeyCode::Digit2, "Lake Tahoe", -120.03, 39.09, 40_000.0),
    (KeyCode::Digit3, "Mt. Shasta", -122.19, 41.41, 25_000.0),
    (KeyCode::Digit4, "Death Valley", -116.87, 36.46, 60_000.0),
    (KeyCode::Digit5, "Placerville (tile 102_352)", -120.65, 38.45, 12_000.0),
    (KeyCode::Digit6, "Los Angeles", -118.24, 34.05, 60_000.0),
];

fn main() {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "vr_fire — California monster truck".into(),
                // Vsync paces to the display (120 Hz on a 120 Hz screen). The autopilot turns it
                // off because compositors stop frame callbacks for hidden windows.
                present_mode: if cfg!(not(target_arch = "wasm32")) && std::env::var("VR_FIRE_AUTOPILOT").is_ok() {
                    PresentMode::AutoNoVsync
                } else {
                    PresentMode::AutoVsync
                },
                canvas: Some("#bevy".into()),
                fit_canvas_to_parent: true,
                prevent_default_event_handling: true,
                ..default()
            }),
            ..default()
        }),
    )
    .add_plugins(FrameTimeDiagnosticsPlugin::default())
    .insert_resource(ClearColor(Color::srgb(0.55, 0.70, 0.88)))
    .insert_resource(Time::<Fixed>::from_hz(120.0))
    .insert_resource(Mode::Map)
    .insert_resource(MapCam { focus: Vec3::ZERO, yaw: 0.0, pitch: 1.2, dist: 1_300_000.0, fly: None })
    .insert_resource(ChaseCam { dist: 16.0, yaw: 0.0, pitch: 0.28 })
    .init_resource::<Drag>()
    .init_resource::<Truck>()
    .init_resource::<cog::Cog>()
    .init_resource::<net::Remotes>()
    .add_systems(Startup, (setup, net::setup))
    .add_systems(FixedUpdate, truck::physics)
    .add_systems(
        Update,
        (
            terrain::select_patches,
            terrain::run_jobs,
            (map_input, truck::reset, mode_keys, cameras, truck::sync_model).chain(),
            net::sync,
            hud,
            water_follow,
            apply_anchors,
        ),
    );
    #[cfg(not(target_arch = "wasm32"))]
    app.add_systems(PreUpdate, autopilot.after(bevy::input::InputSystems));
    app.run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut map: ResMut<MapCam>,
) {
    let material = mats.add(StandardMaterial { base_color: Color::WHITE, perceptual_roughness: 0.95, ..default() });
    let terrain = Terrain::new(material);
    // World origin: centre of California's Albers extent.
    let (x, y) = terrain.albers.from_lonlat(-119.5, 37.2).unwrap();
    commands.insert_resource(WorldOrigin(DVec2::new(x.round(), y.round())));
    commands.insert_resource(terrain);
    map.focus = Vec3::ZERO;

    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { fov: 55f32.to_radians(), near: 0.5, far: 5.0e6, ..default() }),
        Msaa::Sample4,
        DistanceFog {
            color: Color::srgba(0.62, 0.74, 0.88, 1.0),
            falloff: FogFalloff::Linear { start: 50_000.0, end: 400_000.0 },
            ..default()
        },
        Transform::from_xyz(0.0, 1_000_000.0, 600_000.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 12_000.0, shadow_maps_enabled: true, ..default() },
        CascadeShadowConfigBuilder { num_cascades: 3, first_cascade_far_bound: 40.0, maximum_distance: 2_000.0, ..default() }.build(),
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 2.4, -0.75, 0.0)),
    ));
    commands.insert_resource(GlobalAmbientLight { brightness: 900.0, ..default() });
    commands.spawn((
        Water,
        Mesh3d(meshes.add(Plane3d::default().mesh().size(4_000_000.0, 4_000_000.0))),
        MeshMaterial3d(mats.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.28, 0.45),
            perceptual_roughness: 0.15,
            metallic: 0.1,
            ..default()
        })),
        Transform::from_xyz(0.0, -2.0, 0.0),
    ));
    truck::spawn_model(&mut commands, &mut meshes, &mut mats, Color::srgb(0.1, 0.75, 0.15));
    commands.spawn((
        Hud,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(15.0), ..default() },
        TextColor(Color::WHITE),
        TextShadow::default(),
        Node { position_type: PositionType::Absolute, top: px(10), left: px(12), ..default() },
    ));
    commands.spawn((
        Banner,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(64.0), ..default() },
        TextColor(Color::srgb(1.0, 0.2, 0.1)),
        TextShadow::default(),
        Node { position_type: PositionType::Absolute, top: percent(38), width: percent(100), justify_content: JustifyContent::Center, ..default() },
        TextLayout::justify(Justify::Center),
    ));
}

/// Where the cursor ray hits the terrain (ray-marched against the built height patches).
fn cursor_ground(
    window: &Window,
    cam: (&Camera, &GlobalTransform),
    terrain: &Terrain,
    origin: &WorldOrigin,
) -> Option<Vec3> {
    let ray = cam.0.viewport_to_world(cam.1, window.cursor_position()?).ok()?;
    let h = |p: Vec3| terrain.height_at(origin.0.x + p.x as f64, origin.0.y - p.z as f64).map(|v| v.0).unwrap_or(0.0).max(-2.0);
    let mut t = 0.0f32;
    let mut prev = 0.0f32;
    for _ in 0..600 {
        let p = ray.origin + *ray.direction * t;
        let above = p.y - h(p);
        if above < 0.0 {
            let (mut lo, mut hi) = (prev, t);
            for _ in 0..24 {
                let mid = 0.5 * (lo + hi);
                let q = ray.origin + *ray.direction * mid;
                if q.y - h(q) < 0.0 { hi = mid } else { lo = mid }
            }
            return Some(ray.origin + *ray.direction * hi);
        }
        prev = t;
        t += (above * 0.5).max(2.0);
        if t > 5.0e6 {
            break;
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn map_input(
    time: Res<Time>,
    mode: Res<Mode>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cams: Query<(&Camera, &GlobalTransform)>,
    terrain: Res<Terrain>,
    mut origin: ResMut<WorldOrigin>,
    mut map: ResMut<MapCam>,
    mut chase: ResMut<ChaseCam>,
    mut drag: ResMut<Drag>,
    mut truck: ResMut<Truck>,
    mut commands: Commands,
) {
    let Ok(window) = windows.single() else { return };
    let Ok(cam) = cams.single() else { return };
    let wheel = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 60.0,
    };
    if *mode == Mode::Drive {
        chase.dist = (chase.dist * 0.88f32.powf(wheel)).clamp(6.0, 400.0);
        if mouse.pressed(MouseButton::Right) || mouse.pressed(MouseButton::Left) {
            chase.yaw -= motion.delta.x * 0.005;
            chase.pitch = (chase.pitch + motion.delta.y * 0.004).clamp(-0.1, 1.4);
        }
        return;
    }
    // Fly-to animation (number keys).
    for (k, _, lon, lat, dist) in PLACES {
        if keys.just_pressed(k) {
            let (x, y) = terrain.albers.from_lonlat(lon, lat).unwrap();
            let target = world_pos(&origin, x, y, 0.0);
            map.fly = Some((target, dist, 0.0));
        }
    }
    if let Some((target, dist, t)) = map.fly {
        let t = t + time.delta_secs() / 1.8;
        let s = (t * std::f32::consts::PI).min(std::f32::consts::PI);
        let k = 0.5 - 0.5 * s.cos();
        map.focus = map.focus.lerp(target, k * 0.2 + 0.02);
        map.dist = map.dist + (dist - map.dist) * (k * 0.2 + 0.02);
        map.pitch += (0.65 - map.pitch) * 0.05;
        map.fly = if t >= 1.0 && map.focus.distance(target) < 1.0 { None } else { Some((target, dist, t)) };
    }
    // Zoom toward the cursor.
    if wheel != 0.0 {
        map.fly = None;
        let old = map.dist;
        map.dist = (map.dist * 0.85f32.powf(wheel)).clamp(40.0, 2_500_000.0);
        if let Some(hit) = cursor_ground(window, cam, &terrain, &origin) {
            let k = 1.0 - map.dist / old;
            map.focus = map.focus.lerp(Vec3::new(hit.x, map.focus.y, hit.z), k);
        }
    }
    // Left-drag pans, right-drag orbits; a left click without dragging drops the truck.
    if mouse.just_pressed(MouseButton::Left) {
        drag.start = window.cursor_position();
        drag.moved = 0.0;
    }
    if mouse.pressed(MouseButton::Left) {
        drag.moved += motion.delta.length();
        let mpp = 2.0 * map.dist * (55f32.to_radians() / 2.0).tan() / window.height();
        let right = Vec3::new(map.yaw.cos(), 0.0, -map.yaw.sin());
        let fwd = Vec3::new(-map.yaw.sin(), 0.0, -map.yaw.cos());
        let d = right * -motion.delta.x * mpp + fwd * motion.delta.y * mpp;
        map.focus += d;
        if d != Vec3::ZERO {
            map.fly = None;
        }
    }
    if mouse.pressed(MouseButton::Right) {
        map.yaw -= motion.delta.x * 0.005;
        map.pitch = (map.pitch + motion.delta.y * 0.004).clamp(0.08, 1.55);
    }
    let clicked = mouse.just_released(MouseButton::Left) && drag.moved < 6.0;
    let auto = drag.auto_drop.take();
    if clicked || keys.just_pressed(KeyCode::KeyT) || auto.is_some() {
        if let Some(hit) = auto.or_else(|| cursor_ground(window, cam, &terrain, &origin)) {
            // Re-centre the world on the drop point so f32 stays precise under the truck.
            let shift = DVec2::new(hit.x as f64, -hit.z as f64);
            origin.0 += shift;
            map.focus -= Vec3::new(hit.x, 0.0, hit.z);
            let yaw = map.yaw;
            truck.drop_at(Vec3::new(0.0, hit.y + 6.0, 0.0), yaw);
            chase.yaw = 0.0;
            commands.insert_resource(Mode::Drive);
        }
    }
    // Keep the orbit focus on the ground.
    if let Some((h, _)) = terrain.height_at(origin.0.x + map.focus.x as f64, origin.0.y - map.focus.z as f64) {
        map.focus.y += (h.max(0.0) - map.focus.y) * (time.delta_secs() * 4.0).min(1.0);
    }
}

fn mode_keys(keys: Res<ButtonInput<KeyCode>>, mut mode: ResMut<Mode>, truck: Res<Truck>, mut map: ResMut<MapCam>) {
    if *mode == Mode::Drive && (keys.just_pressed(KeyCode::KeyM) || keys.just_pressed(KeyCode::Escape)) {
        *mode = Mode::Map;
        map.focus = truck.body.pos;
        map.dist = map.dist.min(3_000.0).max(600.0);
        map.pitch = 0.6;
    } else if *mode == Mode::Map && keys.just_pressed(KeyCode::KeyM) && truck.active {
        *mode = Mode::Drive;
    }
}

fn cameras(
    time: Res<Time>,
    mode: Res<Mode>,
    map: Res<MapCam>,
    chase: Res<ChaseCam>,
    truck: Res<Truck>,
    fixed: Res<Time<Fixed>>,
    terrain: Res<Terrain>,
    origin: Res<WorldOrigin>,
    mut cam: Query<(&mut Transform, &mut Projection, &mut DistanceFog), With<Camera3d>>,
) {
    let Ok((mut tf, mut proj, mut fog)) = cam.single_mut() else { return };
    let (eye, look) = if *mode == Mode::Drive {
        let a = fixed.overstep_fraction();
        let pos = truck.prev.pos.lerp(truck.body.pos, a);
        let fwd = truck.forward();
        let heading = fwd.x.atan2(fwd.z) + chase.yaw;
        let back = -Vec3::new(heading.sin(), 0.0, heading.cos());
        let eye = pos + back * chase.dist * chase.pitch.cos() + Vec3::Y * (chase.dist * chase.pitch.sin() + 2.0);
        let eye = tf.translation.lerp(eye, (time.delta_secs() * 10.0).min(1.0));
        (eye, pos + Vec3::Y * 1.5)
    } else {
        let off = Vec3::new(map.yaw.sin() * map.pitch.cos(), map.pitch.sin(), map.yaw.cos() * map.pitch.cos());
        (map.focus + off * map.dist, map.focus)
    };
    // Never go underground.
    let ground = terrain.height_at(origin.0.x + eye.x as f64, origin.0.y - eye.z as f64).map(|v| v.0).unwrap_or(0.0).max(0.0);
    let eye = Vec3::new(eye.x, eye.y.max(ground + 2.0), eye.z);
    *tf = Transform::from_translation(eye).looking_at(look, Vec3::Y);
    let alt = (eye.y - ground).max(1.0);
    if let Projection::Perspective(p) = &mut *proj {
        p.near = (alt * 0.002).clamp(0.1, 200.0);
    }
    fog.falloff = FogFalloff::Linear { start: 8_000.0 + alt * 6.0, end: 60_000.0 + alt * 40.0 };
}

fn water_follow(cam: Query<&Transform, (With<Camera3d>, Without<Water>)>, mut water: Query<&mut Transform, With<Water>>) {
    if let (Ok(c), Ok(mut w)) = (cam.single(), water.single_mut()) {
        w.translation.x = c.translation.x;
        w.translation.z = c.translation.z;
    }
}

fn apply_anchors(origin: Res<WorldOrigin>, mut q: Query<(&Anchor, &mut Transform)>) {
    if !origin.is_changed() {
        return;
    }
    for (a, mut t) in &mut q {
        t.translation = world_pos(&origin, a.0.x, a.0.y, 0.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn hud(
    diag: Res<DiagnosticsStore>,
    mode: Res<Mode>,
    truck: Res<Truck>,
    terrain: Res<Terrain>,
    cog: Res<cog::Cog>,
    remotes: Res<net::Remotes>,
    map: Res<MapCam>,
    origin: Res<WorldOrigin>,
    mut hud: Query<&mut Text, (With<Hud>, Without<Banner>)>,
    mut banner: Query<&mut Text, (With<Banner>, Without<Hud>)>,
) {
    let fps = diag.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    let mut s = format!("{fps:.0} fps   ");
    let (bx, by) = (origin.0.x + map.focus.x as f64, origin.0.y - map.focus.z as f64);
    let (lon, lat) = terrain.albers.to_lonlat(bx, by).unwrap_or((0.0, 0.0));
    s += &format!(
        "patches {}/{}  jobs {}  fetch {} ({:.1} MB)   {}\n",
        terrain.stats.1,
        terrain.stats.0,
        terrain.pending(),
        cog.inflight(),
        cog.bytes_fetched as f64 / 1e6,
        if remotes.connected { format!("online as {} | {} other trucks", remotes.name, remotes.trucks.len()) } else { "offline".into() }
    );
    if *mode == Mode::Map {
        s += &format!("MAP  {lat:.4} N {:.4} W  view {:.1} km\n", -lon, map.dist / 1000.0);
        s += "scroll zoom | left-drag pan | right-drag orbit | CLICK to drop the monster truck | 1-6 fly to places";
        if truck.active {
            s += " | M back to truck";
        }
        for (k, name, ..) in PLACES {
            s += &format!("\n  {:?}: {name}", k).replace("Digit", "");
        }
    } else {
        let kmh = truck.body.vel.length() * 3.6;
        let fuel = (truck.boost_fuel * 20.0) as usize;
        s += &format!(
            "DRIVE  {kmh:.0} km/h   MEGA BOOST [{}{}]{}\nWASD drive | SPACE mega boost | R reset | M map | scroll/drag camera",
            "#".repeat(fuel),
            "-".repeat(20 - fuel),
            if truck.waiting_for_ground { "   loading 10 m terrain…" } else { "" }
        );
    }
    for (line, _) in &remotes.log {
        s += &format!("\n{line}");
    }
    if let Ok(mut t) = hud.single_mut() {
        t.0 = s;
    }
    if let Ok(mut t) = banner.single_mut() {
        t.0 = match &truck.game_over {
            Some(by) => format!("GAME OVER, MAN!\nflipped by {by}  |  R to respawn"),
            None => String::new(),
        };
    }
}
