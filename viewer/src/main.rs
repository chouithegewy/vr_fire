//! vr_fire terrain viewer: all of California streamed from USGS 3DEP, zoomable from the
//! whole state down to 10 m, with a droppable green monster truck (Space = MEGA BOOST)
//! and multiplayer through a WebSocket relay. Native and wasm (WebGPU / WebGL2).

mod diag;
mod dinos;
mod cog;
mod imagery;
mod minimap;
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

const WATER_Y: f32 = -150.0;
const CHASE_PITCH: f32 = 0.28;
const WEBGL2: bool = cfg!(all(target_arch = "wasm32", not(feature = "webgpu")));

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
    /// Seconds since the mouse last moved the camera.
    idle: f32,
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
    diag: Res<DiagnosticsStore>,
    dinosaurs: Res<dinos::Dinos>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    let Ok(dir) = std::env::var("VR_FIRE_AUTOPILOT") else { return };
    let t = time.elapsed_secs();
    if (t * 2.0) as u32 != ((t - time.delta_secs()) * 2.0) as u32 && (t >= 30.0 || (t as u32) % 3 == 0) {
        let ms = diag.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME).and_then(|d| d.smoothed()).unwrap_or(0.0);
        info!("autopilot t={t:.1}s step={} truck={:?} speed={:.0}km/h up_y={:.2} frame={ms:.2}ms", *step, truck.body.pos, truck.body.vel.length() * 3.6, (truck.body.rot * Vec3::Y).y);
    }
    // VR_FIRE_AUTOPILOT_PLACE=1..6 picks the fly-to place (default 5, Placerville).
    let place = match std::env::var("VR_FIRE_AUTOPILOT_PLACE").as_deref() {
        Ok("1") => KeyCode::Digit1,
        Ok("2") => KeyCode::Digit2,
        Ok("3") => KeyCode::Digit3,
        Ok("4") => KeyCode::Digit4,
        Ok("6") => KeyCode::Digit6,
        _ => KeyCode::Digit5,
    };
    let shot = |commands: &mut Commands, name: &str| {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(format!("{dir}/{name}.png")));
    };
    // (time, action)
    let drive: &[(f32, u8)] = &[(8.0, 0), (9.0, 1), (20.0, 2), (21.0, 3), (22.0, 12), (30.0, 4), (30.5, 5), (34.5, 6), (35.0, 7), (37.0, 8), (38.0, 9), (41.0, 10), (43.0, 11)];
    // VR_FIRE_AUTOPILOT_DINOS: dinosaur mode, then hard-left circles around a tame stegosaurus
    // with the lasso ready (the dinosaur module screenshots the netted and hog-tied moments).
    let dinos: &[(f32, u8)] = &[(8.0, 0), (9.0, 1), (20.0, 2), (21.0, 3), (30.0, 4), (30.5, 20), (31.0, 21), (35.0, 22), (35.5, 23), (150.0, 11)];
    let demo = std::env::var("VR_FIRE_AUTOPILOT_DINOS").is_ok();
    let plan = if demo { dinos } else { drive };
    if demo && *step >= 9 {
        // Circle slowly: hold left, pulse the throttle to stay near 16 km/h (below the scare speed).
        keys.press(KeyCode::KeyA);
        if truck.body.vel.length() < 4.5 {
            keys.press(KeyCode::KeyW);
        } else {
            keys.release(KeyCode::KeyW);
        }
    }
    while *step < plan.len() && t >= plan[*step].0 {
        // The dinosaur demo waits for its animals (terrain may still be streaming).
        if demo && matches!(plan[*step].1, 22 | 23) && dinosaurs.nearby() == 0 {
            break;
        }
        match plan[*step].1 {
            0 => shot(&mut commands, "1_california"),
            1 => keys.press(place),
            2 => shot(&mut commands, "2_zoomed_placerville"),
            3 => drag.auto_drop = Some(map.focus),
            12 => keys.press(KeyCode::KeyF), // request 1 m lidar under the truck
            4 => {
                keys.release(KeyCode::KeyF);
                shot(&mut commands, "3_dropped");
            }
            5 if std::env::var("VR_FIRE_AUTOPILOT_PARK").is_ok() => {}
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
            20 => keys.press(KeyCode::KeyJ),
            21 => {
                keys.release(KeyCode::KeyJ);
                chase.dist = 45.0;
            }
            22 => shot(&mut commands, "7_dinosaurs"),
            23 => keys.press(KeyCode::KeyQ),
            _ => {
                info!("autopilot done: truck at {:?} speed {:.1}", truck.body.pos, truck.body.vel.length());
                exit.write(AppExit::Success);
            }
        }
        *step += 1;
    }
    if (1..3).contains(&*step) {
        keys.release(place);
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
    .add_plugins(MaterialPlugin::<imagery::TerrainMaterial>::default())
    .insert_resource(ClearColor(Color::srgb(0.55, 0.70, 0.88)))
    .insert_resource(Time::<Fixed>::from_hz(120.0))
    .insert_resource(Mode::Map)
    .insert_resource(MapCam { focus: Vec3::ZERO, yaw: 0.0, pitch: 1.2, dist: 1_300_000.0, fly: None })
    .insert_resource(ChaseCam { dist: 16.0, yaw: 0.0, pitch: CHASE_PITCH, idle: 0.0 })
    .init_resource::<Drag>()
    .init_resource::<Truck>()
    .init_resource::<cog::Cog>()
    .init_resource::<net::Remotes>()
    .init_resource::<net::MapFocus>()
    .add_systems(Startup, (setup, net::setup))
    .add_systems(FixedUpdate, truck::physics)
    .add_systems(
        Update,
        (
            terrain::select_patches,
            terrain::run_jobs,
            imagery::run,
            imagery::apply,
            imagery::near,
            (map_input, truck::reset, mode_keys, source_toggle, cameras, truck::sync_model, publish_map_focus, net::sync).chain(),
            hud,
            cursor_lock,
            water_follow,
            apply_anchors,
        ),
    );
    bevy::asset::embedded_asset!(app, "near.wgsl");
    app.add_systems(Startup, minimap::setup.after(setup));
    app.add_plugins(dinos::DinoPlugin);
    app.add_systems(
        PostUpdate,
        (net::labels, minimap::update, minimap::waypoints).after(bevy::transform::TransformSystems::Propagate),
    );
    #[cfg(not(target_arch = "wasm32"))]
    app.add_systems(PreUpdate, autopilot.after(bevy::input::InputSystems));
    app.run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut terrain_mats: ResMut<Assets<imagery::TerrainMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut map: ResMut<MapCam>,
) {
    // Aerial imagery: statewide atlas over California's bounding box (plus the Nevada edges
    // of border super tiles), detail mosaics streamed per patch.
    let ca = vr_fire::region::Region::from_geojson(include_str!("../../data/regions/california.geojson")).unwrap();
    let imagery = imagery::Imagery::new(&ca.bbox().padded(0.4), &mut terrain_mats, &mut images);
    let terrain = Terrain::new(imagery.atlas_material.clone(), imagery.atlas);
    commands.insert_resource(imagery);
    // World origin: centre of California's Albers extent.
    let (x, y) = terrain.albers.from_lonlat(-119.5, 37.2).unwrap();
    commands.insert_resource(WorldOrigin(DVec2::new(x.round(), y.round())));
    commands.insert_resource(terrain);
    map.focus = Vec3::ZERO;

    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { fov: 55f32.to_radians(), near: 0.5, far: 5.0e6, ..default() }),
        // WebGL2 (no WebGPU in the browser) gets lighter settings: 2× MSAA, one shadow cascade.
        if WEBGL2 { Msaa::Sample2 } else { Msaa::Sample4 },
        DistanceFog {
            color: Color::srgba(0.62, 0.74, 0.88, 1.0),
            falloff: FogFalloff::Linear { start: 50_000.0, end: 400_000.0 },
            ..default()
        },
        Transform::from_xyz(0.0, 1_000_000.0, 600_000.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 12_000.0, shadow_maps_enabled: true, ..default() },
        CascadeShadowConfigBuilder { num_cascades: if WEBGL2 { 1 } else { 3 }, first_cascade_far_bound: 40.0, maximum_distance: 2_000.0, ..default() }.build(),
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 2.4, -0.75, 0.0)),
    ));
    commands.insert_resource(GlobalAmbientLight { brightness: 900.0, ..default() });
    commands.spawn((
        Water,
        Mesh3d(meshes.add(Plane3d::default().mesh().size(4_000_000.0, 4_000_000.0))),
        MeshMaterial3d(mats.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.28, 0.45),
            perceptual_roughness: 0.35,
            metallic: 0.1,
            ..default()
        })),
        // Well below any land (Death Valley is −86 m) and above the −300 m ocean floor, so
        // water and terrain never fight for the same depth.
        Transform::from_xyz(0.0, WATER_Y, 0.0),
        bevy::light::NotShadowReceiver,
    ));
    truck::spawn_model(&mut commands, &mut meshes, &mut mats, Color::srgb(0.1, 0.75, 0.15), true);
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
    let h = |p: Vec3| terrain.height_at(origin.0.x + p.x as f64, origin.0.y - p.z as f64).map(|v| v.0).unwrap_or(0.0).max(WATER_Y);
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
    mut terrain: ResMut<Terrain>,
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
        if keys.just_pressed(KeyCode::KeyF) && truck.active {
            let p = truck.body.pos;
            let t = terrain.grid.tile_containing(origin.0.x + p.x as f64, origin.0.y - p.z as f64);
            terrain.request_hires(t);
        }
        chase.dist = (chase.dist * 0.88f32.powf(wheel)).clamp(6.0, 400.0);
        // The pointer is locked while driving: plain mouse movement orbits the camera.
        let dt = time.delta_secs();
        if motion.delta.length() > 0.5 {
            chase.yaw -= motion.delta.x * 0.003;
            chase.pitch = (chase.pitch + motion.delta.y * 0.003).clamp(-0.1, 1.4);
            chase.idle = 0.0;
        } else {
            chase.idle += dt;
        }
        // Mouse still and driving forward: swing back behind the truck.
        let forward_speed = truck.body.vel.dot(truck.forward());
        if chase.idle > 1.0 && forward_speed > 2.0 {
            let k = 1.0 - (-dt * 2.5).exp();
            let yaw = (chase.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            chase.yaw = yaw * (1.0 - k);
            chase.pitch += (CHASE_PITCH - chase.pitch) * k;
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
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let clicked = mouse.just_released(MouseButton::Left) && drag.moved < 6.0 && !shift;
    // Shift+click or F: fetch 1 m lidar for the tile under the cursor.
    if (mouse.just_released(MouseButton::Left) && drag.moved < 6.0 && shift) || keys.just_pressed(KeyCode::KeyF) {
        if let Some(hit) = cursor_ground(window, cam, &terrain, &origin) {
            let t = terrain.grid.tile_containing(origin.0.x + hit.x as f64, origin.0.y - hit.z as f64);
            terrain.request_hires(t);
        }
    }
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

/// C switches terrain between compressed `.vrh` patches and raw USGS COG.
fn source_toggle(keys: Res<ButtonInput<KeyCode>>, mut terrain: ResMut<Terrain>, mut commands: Commands) {
    if keys.just_pressed(KeyCode::KeyC) {
        let next = match terrain.source {
            terrain::Source::Compressed => terrain::Source::Cog,
            terrain::Source::Cog => terrain::Source::Compressed,
        };
        terrain.set_source(next, &mut commands);
    }
}

/// Lock and hide the pointer while driving (mouse steers the camera); free it on the map.
/// Shares the map camera's focus with multiplayer (presence while browsing the map).
fn publish_map_focus(map: Res<MapCam>, mut focus: ResMut<net::MapFocus>) {
    focus.0 = map.focus;
}

fn cursor_lock(mode: Res<Mode>, mut cursor: Query<&mut bevy::window::CursorOptions, With<PrimaryWindow>>) {
    if !mode.is_changed() {
        return;
    }
    if let Ok(mut c) = cursor.single_mut() {
        let driving = *mode == Mode::Drive;
        c.grab_mode = if driving { bevy::window::CursorGrabMode::Locked } else { bevy::window::CursorGrabMode::None };
        c.visible = !driving;
    }
}

fn mode_keys(keys: Res<ButtonInput<KeyCode>>, mut mode: ResMut<Mode>, truck: Res<Truck>, mut map: ResMut<MapCam>) {
    if *mode == Mode::Drive && (keys.just_pressed(KeyCode::KeyM) || keys.just_pressed(KeyCode::Escape)) {
        // Aerial view over the truck; scroll out from there for the whole state.
        *mode = Mode::Map;
        map.focus = truck.body.pos;
        map.dist = 25_000.0;
        map.pitch = 1.2;
        map.fly = None;
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
    imagery: Res<imagery::Imagery>,
    remotes: Res<net::Remotes>,
    map: Res<MapCam>,
    origin: Res<WorldOrigin>,
    mut hud: Query<&mut Text, (With<Hud>, Without<Banner>)>,
    mut banner: Query<&mut Text, (With<Banner>, Without<Hud>)>,
    adapter: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
    window: Query<&Window, With<PrimaryWindow>>,
    dinos: Res<dinos::Dinos>,
) {
    let fps = diag.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    let mut s = format!("{fps:.0} fps   ");
    let diag_line = {
        let (backend, gpu) = adapter.map_or((String::from("?"), String::new()), |a| (format!("{:?}", a.backend), a.name.clone()));
        let frames: Vec<f64> = diag.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME).map_or(Vec::new(), |d| d.values().copied().collect());
        let size = window.single().map_or((0, 0), |w| (w.physical_width(), w.physical_height()));
        diag::line(diag::backend_name(&backend, cfg!(target_arch = "wasm32")), &gpu, size, &frames)
    };
    let (bx, by) = (origin.0.x + map.focus.x as f64, origin.0.y - map.focus.z as f64);
    let (lon, lat) = terrain.albers.to_lonlat(bx, by).unwrap_or((0.0, 0.0));
    s += &format!(
        "patches {}/{}  jobs {}   terrain [C]: {}   packed {:.1} MB ({} hit, {} COG fallback)  COG {:.1} MB ({} in flight)  imagery {:.1} MB ({} in flight)   {}\n",
        terrain.stats.1,
        terrain.stats.0,
        terrain.pending(),
        match terrain.source {
            terrain::Source::Compressed => "compressed .vrh",
            terrain::Source::Cog => "USGS COG (raw f32)",
        },
        terrain.packed_bytes as f64 / 1e6,
        terrain.packed_hits,
        terrain.packed_misses,
        cog.bytes_fetched as f64 / 1e6,
        cog.inflight(),
        (imagery.bytes + imagery.near_bytes) as f64 / 1e6,
        imagery.pending(),
        if remotes.connected {
            let driving = remotes.trucks.values().filter(|r| !r.on_map).count();
            format!("online as {} | {} other players ({driving} driving)", remotes.name, remotes.trucks.len())
        } else {
            "offline".into()
        }
    );
    s += &diag_line;
    s.push('\n');
    if *mode == Mode::Map {
        s += &format!("MAP  {lat:.4} N {:.4} W  view {:.1} km\n", -lon, map.dist / 1000.0);
        s += "scroll zoom | left-drag pan | right-drag orbit | CLICK to drop the monster truck | SHIFT+CLICK or F: 1 m lidar | 1-6 fly to places";
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
            "DRIVE  {kmh:.0} km/h   MEGA BOOST [{}{}]{}\nWASD drive | SPACE mega boost | R reset | F 1 m lidar here | M map | J dinosaurs | Q lasso | mouse look, scroll zoom",
            "#".repeat(fuel),
            "-".repeat(20 - fuel),
            if truck.waiting_for_ground { "   loading 10 m terrain…" } else { "" }
        );
        if dinos.enabled {
            s += "\n";
            s += &dinos.hud(bevy::math::DVec2::new(origin.0.x + truck.body.pos.x as f64, origin.0.y - truck.body.pos.z as f64));
        }
        if let Some(name) = truck.flattened_by {
            s += &format!("\nFlattened by a {name}! Press R to get back on your wheels.");
        }
    }
    if !terrain.hires.is_empty() {
        let mut items: Vec<_> = terrain.hires.iter().collect();
        items.sort_by_key(|(t, _)| (t.ty, t.tx));
        s += "\n1 m lidar:";
        for (t, st) in items.iter().rev().take(4) {
            s += &match st {
                terrain::HiresState::Waiting { age, .. } => format!("  {t} fetching + processing on server {age:.0}s"),
                terrain::HiresState::Building => format!("  {t} building mesh"),
                terrain::HiresState::Ready => format!("  {t} ready (3.75 m)"),
                terrain::HiresState::Unavailable(m) => format!("  {t} none: {m}"),
            };
        }
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
