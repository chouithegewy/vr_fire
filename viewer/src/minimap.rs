//! Driving HUD: a California minimap (the detailed-terrain window around you plus a dot
//! per player) and on-screen waypoints pointing to every other truck.

use crate::net::Remotes;
use crate::terrain::Terrain;
use crate::truck::Truck;
use crate::{Mode, WorldOrigin};
use bevy::asset::RenderAssetUsages;
use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use vr_fire::grid::Bounds;
use vr_fire::region::Region;

const MAP_W: f32 = 190.0;
/// Side of the square "window" drawn on the minimap: the 10 m + 30 m detail area.
const WINDOW_M: f64 = 20_000.0;
/// Waypoints are shown for trucks farther than their floating network label reaches.
const WAYPOINT_MIN_M: f32 = 2_500.0;

#[derive(Resource)]
pub struct MiniMap {
    bounds: Bounds,
    size: Vec2,
}

#[derive(Component)]
pub struct MapRoot;
#[derive(Component)]
pub struct MapWindow;
/// None = you.
#[derive(Component)]
pub struct MapDot(pub Option<u64>);
#[derive(Component)]
pub struct Waypoint(pub u64);

pub fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>, terrain: Res<Terrain>) {
    let region = Region::from_geojson(include_str!("../../data/regions/california.geojson")).unwrap();
    let b = terrain.albers.albers_bounds(&region.bbox()).unwrap();
    let w = MAP_W as u32;
    let h = (MAP_W as f64 * (b.y_max - b.y_min) / (b.x_max - b.x_min)).round() as u32;
    // Rasterize the state outline once: soft fill inside, faint edge.
    let mut inside = vec![false; (w * h) as usize];
    for py in 0..h {
        for px in 0..w {
            let x = b.x_min + (px as f64 + 0.5) / w as f64 * (b.x_max - b.x_min);
            let y = b.y_max - (py as f64 + 0.5) / h as f64 * (b.y_max - b.y_min);
            let (lon, lat) = terrain.albers.to_lonlat(x, y).unwrap();
            inside[(py * w + px) as usize] = region.contains(lon, lat);
        }
    }
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for py in 0..h as i32 {
        for px in 0..w as i32 {
            let at = |x: i32, y: i32| x >= 0 && y >= 0 && x < w as i32 && y < h as i32 && inside[(y as u32 * w + x as u32) as usize];
            let me = at(px, py);
            let edge = me && !(at(px - 1, py) && at(px + 1, py) && at(px, py - 1) && at(px, py + 1));
            data.extend_from_slice(match (me, edge) {
                (true, true) => &[200, 214, 190, 255],
                (true, false) => &[118, 150, 104, 230],
                _ => &[0, 0, 0, 0],
            });
        }
    }
    let image = images.add(Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    ));
    let size = Vec2::new(w as f32, h as f32);
    commands.insert_resource(MiniMap { bounds: b, size });
    commands
        .spawn((
            MapRoot,
            ImageNode::new(image),
            BackgroundColor(Color::srgba(0.03, 0.05, 0.09, 0.7)),
            Node {
                position_type: PositionType::Absolute,
                right: px(14),
                bottom: px(14),
                width: px(size.x),
                height: px(size.y),
                ..default()
            },
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                MapWindow,
                Node {
                    position_type: PositionType::Absolute,
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.9)),
            ));
            p.spawn(dot(None, Color::srgb(0.2, 1.0, 0.3)));
        });
}

fn dot(owner: Option<u64>, color: Color) -> impl Bundle {
    (
        MapDot(owner),
        Node {
            position_type: PositionType::Absolute,
            width: px(7),
            height: px(7),
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BackgroundColor(color),
        BorderColor::all(Color::BLACK),
    )
}

fn hue(id: u64) -> Color {
    Color::hsl((id as f32 * 67.0) % 360.0, 0.85, 0.55)
}

impl MiniMap {
    /// Minimap pixel for an EPSG:5070 point.
    fn px(&self, a: DVec2) -> Vec2 {
        let b = &self.bounds;
        Vec2::new(
            ((a.x - b.x_min) / (b.x_max - b.x_min)) as f32 * self.size.x,
            ((b.y_max - a.y) / (b.y_max - b.y_min)) as f32 * self.size.y,
        )
    }
}

fn albers_of(origin: &WorldOrigin, p: Vec3) -> DVec2 {
    DVec2::new(origin.0.x + p.x as f64, origin.0.y - p.z as f64)
}

#[allow(clippy::too_many_arguments)]
pub fn update(
    mode: Res<Mode>,
    truck: Res<Truck>,
    remotes: Res<Remotes>,
    origin: Res<WorldOrigin>,
    map: Option<Res<MiniMap>>,
    mut commands: Commands,
    root: Query<Entity, With<MapRoot>>,
    mut vis: Query<&mut Visibility, With<MapRoot>>,
    mut window: Query<&mut Node, (With<MapWindow>, Without<MapDot>)>,
    mut dots: Query<(Entity, &MapDot, &mut Node), Without<MapWindow>>,
) {
    let Some(map) = map else { return };
    let (Ok(root), Ok(mut v)) = (root.single(), vis.single_mut()) else { return };
    let show = *mode == Mode::Drive && truck.active;
    *v = if show { Visibility::Inherited } else { Visibility::Hidden };
    if !show {
        return;
    }
    let me = albers_of(&origin, truck.body.pos);
    if let Ok(mut n) = window.single_mut() {
        let a = map.px(me - DVec2::splat(WINDOW_M / 2.0));
        let c = map.px(me + DVec2::splat(WINDOW_M / 2.0));
        let side = (c.x - a.x).abs().max(14.0);
        let centre = map.px(me);
        n.left = px(centre.x - side / 2.0);
        n.top = px(centre.y - side / 2.0);
        n.width = px(side);
        n.height = px(side);
    }
    let mut have = std::collections::HashSet::new();
    for (e, d, mut n) in &mut dots {
        let pos = match d.0 {
            None => Some(me),
            Some(id) => remotes.trucks.get(&id).map(|r| r.albers),
        };
        match pos {
            Some(a) => {
                let p = map.px(a);
                n.left = px(p.x - 3.5);
                n.top = px(p.y - 3.5);
                if let Some(id) = d.0 {
                    have.insert(id);
                }
            }
            None => commands.entity(e).despawn(),
        }
    }
    for id in remotes.trucks.keys() {
        if !have.contains(id) {
            let child = commands.spawn(dot(Some(*id), hue(*id))).id();
            commands.entity(root).add_child(child);
        }
    }
}

/// Edge-of-screen (or on-target) markers with name and distance for far or off-screen trucks.
#[allow(clippy::too_many_arguments)]
pub fn waypoints(
    mode: Res<Mode>,
    truck: Res<Truck>,
    remotes: Res<Remotes>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cams: Query<(&Camera, &GlobalTransform)>,
    mut commands: Commands,
    mut q: Query<(Entity, &Waypoint, &mut Text, &mut Node, &mut Visibility)>,
) {
    let (Ok(window), Ok((cam, cam_tf))) = (windows.single(), cams.single()) else { return };
    let active = *mode == Mode::Drive && truck.active;
    let screen = Vec2::new(window.width(), window.height());
    let eye = cam_tf.translation();
    let view = cam_tf.affine().inverse();
    let mut seen = std::collections::HashSet::new();
    for (e, wp, mut text, mut node, mut vis) in &mut q {
        let Some(r) = remotes.trucks.get(&wp.0) else {
            commands.entity(e).despawn();
            continue;
        };
        seen.insert(wp.0);
        let dist = r.pos_world.distance(truck.body.pos);
        let on_screen = cam
            .world_to_viewport(cam_tf, r.pos_world + Vec3::Y * 3.0)
            .ok()
            .filter(|p| p.x > 20.0 && p.y > 20.0 && p.x < screen.x - 20.0 && p.y < screen.y - 20.0);
        if !active || (on_screen.is_some() && r.pos_world.distance(eye) < WAYPOINT_MIN_M) {
            *vis = Visibility::Hidden;
            continue;
        }
        let km = if dist >= 1000.0 { format!("{:.1} km", dist / 1000.0) } else { format!("{dist:.0} m") };
        let (at, arrow) = match on_screen {
            Some(p) => (p - Vec2::new(0.0, 18.0), "v"),
            None => {
                // Direction in camera space (camera looks down −Z; screen y grows downward).
                let local = view.transform_point3(r.pos_world);
                let dir = Vec2::new(local.x, -local.y).normalize_or(Vec2::X);
                let half = screen / 2.0 - Vec2::new(90.0, 40.0);
                let t = (half.x / dir.x.abs().max(1e-3)).min(half.y / dir.y.abs().max(1e-3));
                let arrow = if dir.x.abs() > dir.y.abs() {
                    if dir.x > 0.0 { ">" } else { "<" }
                } else if dir.y > 0.0 {
                    "v"
                } else {
                    "^"
                };
                (screen / 2.0 + dir * t, arrow)
            }
        };
        text.0 = format!("{arrow} {}{}  {km}", r.name, if r.on_map { " (on map)" } else { "" });
        // Keep the whole label on screen.
        node.left = px((at.x - 70.0).clamp(8.0, screen.x - 200.0));
        node.top = px((at.y - 10.0).clamp(8.0, screen.y - 30.0));
        *vis = Visibility::Inherited;
    }
    for (id, _) in remotes.trucks.iter().filter(|(id, _)| !seen.contains(*id)) {
        commands.spawn((
            Waypoint(*id),
            Text::new(""),
            TextFont { font_size: bevy::text::FontSize::Px(14.0), ..default() },
            TextColor(hue(*id)),
            TextShadow::default(),
            Node { position_type: PositionType::Absolute, ..default() },
            Visibility::Hidden,
        ));
    }
}
