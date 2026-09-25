//! Bevy side of the terrain benchmark. Loads the tiles listed in `bench/scene.json` (glb, from
//! `vr_fire bake --format both --out bench/tiles`), flies the same orbit as the Unity build, and
//! reports frame-time statistics.
//!
//! Native: `cargo run --release -p terrain-bench` writes `bench/results/bevy_native.json`
//! (override with `BENCH_OUT`, screenshot with `BENCH_SHOT`). Web: logs `BENCH_RESULT {json}`.
//!
//! Coordinates in scene.json are metres east and south of the first tile's NW corner. Bevy and
//! the glb tiles use x = east, z = south, so a point is (east, h, south) here.

use bevy::asset::AssetMetaCheck;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::window::{PresentMode, WindowResolution};
use serde::Deserialize;

const SCENE: &str = include_str!("../../scene.json");

#[derive(Deserialize, Clone)]
struct Tile {
    name: String,
    east_m: f32,
    south_m: f32,
}

#[derive(Deserialize, Clone)]
struct Orbit {
    centre_east_m: f32,
    centre_south_m: f32,
    radius_m: f32,
    altitude_m: f32,
    look_at_height_m: f32,
    turns: f32,
}

#[derive(Deserialize, Clone, Resource)]
struct Scene {
    lod: u32,
    tiles: Vec<Tile>,
    resolution: [u32; 2],
    warmup_s: f32,
    duration_s: f32,
    orbit: Orbit,
}

#[derive(Resource, Default)]
struct Run {
    meshes: Vec<Handle<Mesh>>,
    /// Real time when every tile had loaded; the warm-up starts here.
    ready_at: Option<f64>,
    frame_ms: Vec<f64>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    shot: bool,
    done: bool,
}

/// Camera pose at fraction `u` of the orbit (identical to the Unity benchmark).
fn pose(o: &Orbit, u: f32) -> (Vec3, Vec3) {
    let a = u * o.turns * std::f32::consts::TAU;
    let eye = Vec3::new(o.centre_east_m + a.cos() * o.radius_m, o.altitude_m, o.centre_south_m + a.sin() * o.radius_m);
    (eye, Vec3::new(o.centre_east_m, o.look_at_height_m, o.centre_south_m))
}

fn main() {
    let scene: Scene = serde_json::from_str(SCENE).expect("bench/scene.json");
    let [w, h] = scene.resolution;
    // Native reads bench/ from the repo; the web page lives at bench/bevy/web/ on the server.
    let root = if cfg!(target_arch = "wasm32") { "../..".to_string() } else { format!("{}/..", env!("CARGO_MANIFEST_DIR")) };
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "vr_fire terrain bench (Bevy)".into(),
                        resolution: WindowResolution::new(w, h),
                        resizable: false,
                        present_mode: PresentMode::AutoNoVsync,
                        canvas: Some("#bevy".into()),
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin { file_path: root, meta_check: AssetMetaCheck::Never, ..default() }),
        )
        .insert_resource(scene)
        .insert_resource(ClearColor(Color::srgb(0.55, 0.7, 0.9)))
        .insert_resource(GlobalAmbientLight { color: Color::srgb(0.45, 0.47, 0.5), brightness: 400.0, ..default() })
        .init_resource::<Run>()
        .add_systems(Startup, setup)
        .add_systems(Update, (fly, measure).chain())
        .run();
}

fn setup(mut commands: Commands, scene: Res<Scene>, assets: Res<AssetServer>, mut mats: ResMut<Assets<StandardMaterial>>, mut run: ResMut<Run>) {
    let terrain = mats.add(StandardMaterial {
        base_color: Color::srgb(0.42, 0.45, 0.33),
        perceptual_roughness: 0.9,
        metallic: 0.0,
        ..default()
    });
    for t in &scene.tiles {
        let path = format!("tiles/lod{}/{}.glb", scene.lod, t.name);
        let mesh: Handle<Mesh> = assets.load(GltfAssetLabel::Primitive { mesh: 0, primitive: 0 }.from_asset(path));
        run.meshes.push(mesh.clone());
        commands.spawn((Mesh3d(mesh), MeshMaterial3d(terrain.clone()), Transform::from_xyz(t.east_m, 0.0, t.south_m)));
    }
    commands.spawn((
        DirectionalLight { illuminance: 10_000.0, shadow_maps_enabled: false, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 30f32.to_radians(), -50f32.to_radians(), 0.0)),
    ));
    let (eye, look) = pose(&scene.orbit, 0.0);
    commands.spawn((
        Camera3d::default(),
        Msaa::Sample4,
        Tonemapping::None,
        Projection::Perspective(PerspectiveProjection { fov: 60f32.to_radians(), near: 1.0, far: 30_000.0, ..default() }),
        Transform::from_translation(eye).looking_at(look, Vec3::Y),
    ));
}

fn fly(time: Res<Time<Real>>, scene: Res<Scene>, meshes: Res<Assets<Mesh>>, mut run: ResMut<Run>, mut cam: Query<&mut Transform, With<Camera3d>>) {
    let now = time.elapsed_secs_f64();
    if run.ready_at.is_none() {
        if run.meshes.iter().all(|m| meshes.contains(m)) {
            info!("bench: {} tiles loaded after {now:.1}s", run.meshes.len());
            run.ready_at = Some(now);
        }
        return;
    }
    let t = (now - run.ready_at.unwrap()) as f32;
    let u = ((t - scene.warmup_s) / scene.duration_s).clamp(0.0, 1.0);
    let (eye, look) = pose(&scene.orbit, u);
    if let Ok(mut tf) = cam.single_mut() {
        *tf = Transform::from_translation(eye).looking_at(look, Vec3::Y);
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[allow(clippy::too_many_arguments)]
fn measure(
    time: Res<Time<Real>>,
    scene: Res<Scene>,
    meshes: Res<Assets<Mesh>>,
    adapter: Option<Res<RenderAdapterInfo>>,
    windows: Query<&Window>,
    mut run: ResMut<Run>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(ready) = run.ready_at else { return };
    if run.done {
        return;
    }
    let t = (time.elapsed_secs_f64() - ready) as f32;
    if t <= scene.warmup_s {
        return;
    }
    run.frame_ms.push(time.delta_secs_f64() * 1000.0);
    #[cfg(not(target_arch = "wasm32"))]
    if !run.shot && t > scene.warmup_s + scene.duration_s * 0.25 {
        run.shot = true;
        if let Ok(path) = std::env::var("BENCH_SHOT") {
            use bevy::render::view::screenshot::{Screenshot, save_to_disk};
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        }
    }
    let _ = &mut commands;
    if t <= scene.warmup_s + scene.duration_s {
        return;
    }
    run.done = true;
    let mut sorted = run.frame_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let tris: usize = run.meshes.iter().filter_map(|m| meshes.get(m)).map(|m| m.indices().map_or(0, |i| i.len() / 3)).sum();
    let (api, gpu) = adapter.map_or(("?".into(), "?".into()), |a| (format!("{:?}", a.backend), a.name.clone()));
    let size = windows.single().map_or((0, 0), |w| (w.physical_width(), w.physical_height()));
    let json = serde_json::json!({
        "engine": "Bevy 0.19.1",
        "target": if cfg!(target_arch = "wasm32") { "wasm32" } else { "native" },
        "graphics_api": api,
        "gpu": gpu,
        "resolution": [size.0, size.1],
        "msaa": 4,
        "triangles": tris,
        "frames": sorted.len(),
        "fps_mean": (1000.0 / mean * 10.0).round() / 10.0,
        "frame_ms": {
            "mean": (mean * 1000.0).round() / 1000.0,
            "p50": pct(&sorted, 0.5), "p95": pct(&sorted, 0.95), "p99": pct(&sorted, 0.99), "max": sorted[sorted.len() - 1],
        },
    });
    info!("BENCH_RESULT {json}");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let out = std::env::var("BENCH_OUT").unwrap_or_else(|_| format!("{}/../results/bevy_native.json", env!("CARGO_MANIFEST_DIR")));
        let _ = std::fs::create_dir_all(std::path::Path::new(&out).parent().unwrap());
        std::fs::write(&out, serde_json::to_string_pretty(&json).unwrap()).expect("write bench result");
        exit.write(AppExit::Success);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = &mut exit;
}
