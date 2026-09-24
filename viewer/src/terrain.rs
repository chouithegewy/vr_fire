//! Level-of-detail terrain for all of California, streamed from 3DEP COGs.
//!
//! Patches: 37.5 km "super tiles" (51² nodes, 750 m, COG level 5) cover the state; near the
//! camera each splits into its 100 grid tiles (3.75 km, aligned to the ML 30 m grid) at
//! 150 m / 30 m / 10 m. Meshes are built on a per-frame time budget so streaming never
//! blows the 120 fps frame.

use crate::cog::{Cell, Cog, TileKey};
use crate::{Anchor, WorldOrigin};
use bevy::asset::RenderAssetUsages;
use bevy::math::DVec2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use vr_fire::crs::{Albers, LonLatBox};
use vr_fire::grid::{GridSpec, TILE_SIZE_M, TileId};
use vr_fire::region::Region;

const SUPER: i64 = 10;
const SUPER_SIZE: f64 = TILE_SIZE_M * SUPER as f64;
const SPLIT_DIST: f64 = 45_000.0;
const LOD0_DIST: f64 = 3_000.0;
const LOD1_DIST: f64 = 10_000.0;
/// Height given to "no data" (ocean): below the water plane.
pub const OCEAN_M: f32 = -30.0;
const FRAME_BUDGET_MS: f64 = 4.0;
const LATTICE: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Patch {
    Super { sx: i64, sy: i64 },
    Tile { t: TileId, lod: u8 },
}

struct Spec {
    x_min: f64,
    y_max: f64,
    spacing: f64,
    n: usize,
    level: u8,
}

impl Patch {
    fn spec(&self, g: &GridSpec) -> Spec {
        match *self {
            Patch::Super { sx, sy } => Spec {
                x_min: g.origin_x + SUPER_SIZE * sx as f64,
                y_max: g.origin_y - SUPER_SIZE * sy as f64,
                spacing: 750.0,
                n: 51,
                level: 5,
            },
            Patch::Tile { t, lod } => {
                let b = g.tile_bounds(t);
                let (spacing, n, level) = match lod {
                    0 => (10.0, 376, 0),
                    1 => (30.0, 126, 1),
                    _ => (150.0, 26, 3),
                };
                Spec { x_min: b.x_min, y_max: b.y_max, spacing, n, level }
            }
        }
    }
}

/// Heights of a built patch, for physics and picking.
pub struct Heights {
    pub x_min: f64,
    pub y_max: f64,
    pub spacing: f64,
    pub n: usize,
    pub data: Vec<f32>,
}

impl Heights {
    /// Height on the same triangles the mesh draws (split along the NE–SW diagonal).
    pub fn at(&self, x: f64, y: f64) -> Option<f32> {
        let u = (x - self.x_min) / self.spacing;
        let v = (self.y_max - y) / self.spacing;
        let last = (self.n - 1) as f64;
        if !(0.0..=last).contains(&u) || !(0.0..=last).contains(&v) {
            return None;
        }
        let (i, j) = ((u.floor() as usize).min(self.n - 2), (v.floor() as usize).min(self.n - 2));
        let (fu, fv) = ((u - i as f64) as f32, (v - j as f64) as f32);
        let h = |i: usize, j: usize| self.data[j * self.n + i];
        let (a, b, c, d) = (h(i, j), h(i + 1, j), h(i, j + 1), h(i + 1, j + 1));
        Some(if fu + fv <= 1.0 { a + (b - a) * fu + (c - a) * fv } else { d + (c - d) * (1.0 - fu) + (b - d) * (1.0 - fv) })
    }
}

enum Stage {
    Plan,
    Wait(Vec<TileKey>, Vec<Cell>),
    Build,
}

struct Job {
    patch: Patch,
    prio: f64,
    stage: Stage,
    /// Lon/lat of the (n+2)² nodes including a one-node border ring (for seamless normals).
    ll: Vec<(f64, f64)>,
    heights: Vec<f32>,
}

struct Built {
    entity: Entity,
    heights: Arc<Heights>,
}

#[derive(Resource)]
pub struct Terrain {
    pub albers: Albers,
    pub grid: GridSpec,
    supers: Vec<(i64, i64)>,
    built: HashMap<Patch, Built>,
    jobs: Vec<Job>,
    material: Handle<StandardMaterial>,
    since_update: f32,
    pub stats: (usize, usize),
}

pub fn california_supers(grid: &GridSpec, albers: &Albers) -> Vec<(i64, i64)> {
    let region = Region::from_geojson(include_str!("../../data/regions/california.geojson")).unwrap();
    let bb = region_bbox(&region);
    let b = albers.albers_bounds(&bb).unwrap();
    let s0 = ((b.x_min - grid.origin_x) / SUPER_SIZE).floor() as i64;
    let s1 = ((b.x_max - grid.origin_x) / SUPER_SIZE).floor() as i64;
    let t0 = ((grid.origin_y - b.y_max) / SUPER_SIZE).floor() as i64;
    let t1 = ((grid.origin_y - b.y_min) / SUPER_SIZE).floor() as i64;
    let mut out = Vec::new();
    for sy in t0..=t1 {
        for sx in s0..=s1 {
            let x0 = grid.origin_x + SUPER_SIZE * sx as f64;
            let y1 = grid.origin_y - SUPER_SIZE * sy as f64;
            let hit = (0..=6).any(|a| {
                (0..=6).any(|c| {
                    let (lon, lat) = albers.to_lonlat(x0 + SUPER_SIZE * a as f64 / 6.0, y1 - SUPER_SIZE * c as f64 / 6.0).unwrap();
                    region.contains(lon, lat)
                })
            });
            if hit {
                out.push((sx, sy));
            }
        }
    }
    out
}

fn region_bbox(r: &Region) -> LonLatBox {
    r.bbox()
}

impl Terrain {
    pub fn new(material: Handle<StandardMaterial>) -> Self {
        let grid = GridSpec::default();
        let albers = Albers::new().unwrap();
        let supers = california_supers(&grid, &albers);
        info!("California: {} super tiles", supers.len());
        Self { albers, grid, supers, built: HashMap::new(), jobs: Vec::new(), material, since_update: 1.0, stats: (0, 0) }
    }

    /// Terrain height at an EPSG:5070 point from the finest built patch.
    pub fn height_at(&self, x: f64, y: f64) -> Option<(f32, u8)> {
        let t = self.grid.tile_containing(x, y);
        for lod in 0..3u8 {
            if let Some(b) = self.built.get(&Patch::Tile { t, lod }) {
                if let Some(h) = b.heights.at(x, y) {
                    return Some((h, lod));
                }
            }
        }
        let sx = ((x - self.grid.origin_x) / SUPER_SIZE).floor() as i64;
        let sy = ((self.grid.origin_y - y) / SUPER_SIZE).floor() as i64;
        self.built.get(&Patch::Super { sx, sy }).and_then(|b| b.heights.at(x, y)).map(|h| (h, 9))
    }

    pub fn pending(&self) -> usize {
        self.jobs.len()
    }
}

fn dist_to_rect(px: f64, py: f64, x0: f64, y1: f64, size: f64, alt: f64) -> f64 {
    let dx = (x0 - px).max(0.0).max(px - (x0 + size));
    let dy = ((y1 - size) - py).max(0.0).max(py - y1);
    (dx * dx + dy * dy + alt * alt).sqrt()
}

/// Decide which patches should exist and be visible around the camera.
pub fn select_patches(
    time: Res<Time>,
    mut terrain: ResMut<Terrain>,
    origin: Res<WorldOrigin>,
    cams: Query<&GlobalTransform, With<Camera3d>>,
    mut vis: Query<&mut Visibility>,
    mut commands: Commands,
) {
    let terrain = &mut *terrain;
    terrain.since_update += time.delta_secs();
    if terrain.since_update < 0.2 {
        return;
    }
    terrain.since_update = 0.0;
    let Ok(cam) = cams.single() else { return };
    let p = cam.translation();
    let (cx, cy) = (origin.0.x + p.x as f64, origin.0.y - p.z as f64);
    let ground = terrain.height_at(cx, cy).map(|h| h.0).unwrap_or(0.0);
    let alt = (p.y - ground).max(0.0) as f64;
    let g = terrain.grid;

    let mut wanted: HashMap<Patch, f64> = HashMap::new();
    let mut show: HashSet<Patch> = HashSet::new();
    for &(sx, sy) in &terrain.supers {
        let sp = Patch::Super { sx, sy };
        let spec = sp.spec(&g);
        let e = dist_to_rect(cx, cy, spec.x_min, spec.y_max, SUPER_SIZE, alt);
        wanted.insert(sp, e * 4.0 + 1e6); // supers: always wanted, lower priority than near tiles
        if e >= SPLIT_DIST {
            show.insert(sp);
            continue;
        }
        let mut children = Vec::with_capacity(100);
        let mut all_ready = true;
        for j in 0..SUPER {
            for i in 0..SUPER {
                let t = TileId::new(sx * SUPER + i, sy * SUPER + j);
                let b = g.tile_bounds(t);
                let et = dist_to_rect(cx, cy, b.x_min, b.y_max, TILE_SIZE_M, alt);
                let lod = if et < LOD0_DIST { 0 } else if et < LOD1_DIST { 1 } else { 2 };
                wanted.insert(Patch::Tile { t, lod }, et);
                // Display the desired LOD if built, else the nearest built one.
                let order: [u8; 3] = match lod {
                    0 => [0, 1, 2],
                    1 => [1, 0, 2],
                    _ => [2, 1, 0],
                };
                match order.iter().map(|&l| Patch::Tile { t, lod: l }).find(|p| terrain.built.contains_key(p)) {
                    Some(p) => children.push(p),
                    None => all_ready = false,
                }
            }
        }
        if all_ready {
            show.extend(children.iter().copied());
            for c in &children {
                wanted.entry(*c).or_insert(f64::MAX); // keep the displayed fallback alive
            }
        } else {
            show.insert(sp);
            for c in &children {
                wanted.entry(*c).or_insert(f64::MAX);
            }
        }
    }

    // Despawn built patches no longer wanted; set visibility on the rest.
    let stale: Vec<Patch> = terrain.built.keys().filter(|p| !wanted.contains_key(p)).copied().collect();
    for p in stale {
        if let Some(b) = terrain.built.remove(&p) {
            commands.entity(b.entity).despawn();
        }
    }
    for (p, b) in &terrain.built {
        if let Ok(mut v) = vis.get_mut(b.entity) {
            let want = if show.contains(p) { Visibility::Inherited } else { Visibility::Hidden };
            if *v != want {
                *v = want;
            }
        }
    }
    // Refresh the job queue: drop unwanted jobs, add new ones, sort nearest first.
    terrain.jobs.retain(|j| wanted.contains_key(&j.patch));
    let queued: HashSet<Patch> = terrain.jobs.iter().map(|j| j.patch).collect();
    for (p, prio) in &wanted {
        if *prio == f64::MAX || terrain.built.contains_key(p) || queued.contains(p) {
            continue;
        }
        terrain.jobs.push(Job { patch: *p, prio: *prio, stage: Stage::Plan, ll: Vec::new(), heights: Vec::new() });
    }
    for j in terrain.jobs.iter_mut() {
        if let Some(p) = wanted.get(&j.patch) {
            j.prio = *p;
        }
    }
    terrain.jobs.sort_by(|a, b| a.prio.total_cmp(&b.prio));
    terrain.stats = (terrain.built.len(), show.len());
}

/// Lon/lat of (n+2)² nodes (index −1..=n), projecting only a coarse lattice and interpolating.
fn node_lonlats(albers: &Albers, s: &Spec) -> Vec<(f64, f64)> {
    let m = s.n + 2;
    let k = (m - 1).div_ceil(LATTICE) + 1;
    let mut lat_grid = Vec::with_capacity(k * k);
    for b in 0..k {
        for a in 0..k {
            let (ia, ib) = ((a * LATTICE) as f64 - 1.0, (b * LATTICE) as f64 - 1.0);
            lat_grid.push(albers.to_lonlat(s.x_min + ia * s.spacing, s.y_max - ib * s.spacing).unwrap());
        }
    }
    let mut out = Vec::with_capacity(m * m);
    for jj in 0..m {
        let (bj, fj) = ((jj / LATTICE).min(k - 2), (jj as f64 - ((jj / LATTICE).min(k - 2) * LATTICE) as f64) / LATTICE as f64);
        for ii in 0..m {
            let bi = (ii / LATTICE).min(k - 2);
            let fi = (ii as f64 - (bi * LATTICE) as f64) / LATTICE as f64;
            let q = |a: usize, b: usize| lat_grid[b * k + a];
            let (p00, p10, p01, p11) = (q(bi, bj), q(bi + 1, bj), q(bi, bj + 1), q(bi + 1, bj + 1));
            let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
            out.push((
                lerp(lerp(p00.0, p10.0, fi), lerp(p01.0, p11.0, fi), fj),
                lerp(lerp(p00.1, p10.1, fi), lerp(p01.1, p11.1, fi), fj),
            ));
        }
    }
    out
}

/// COG pieces covering the lon/lat nodes at `level`.
fn needed_tiles(cog: &mut Cog, ll: &[(f64, f64)], level: u8) -> Result<Vec<TileKey>, Vec<Cell>> {
    let (mut w, mut s, mut e, mut n) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(lon, lat) in ll {
        w = w.min(lon);
        e = e.max(lon);
        s = s.min(lat);
        n = n.max(lat);
    }
    let nw = Cell::containing(w, n);
    let se = Cell::containing(e, s);
    let mut keys = Vec::new();
    let mut waiting = Vec::new();
    for north in se.north..=nw.north {
        for west in se.west..=nw.west {
            let cell = Cell { north, west };
            match cog.cell(cell) {
                None => waiting.push(cell),
                Some(None) => {}
                Some(Some(meta)) => {
                    let lv = &meta.levels[(level as usize).min(meta.levels.len() - 1)];
                    let px = |lon: f64| ((lon - meta.origin_lon) / lv.px).floor() as i64;
                    let py = |lat: f64| ((meta.origin_lat - lat) / lv.py).floor() as i64;
                    let (c0, c1) = ((px(w) - 1).max(0), (px(e) + 1).min(lv.width as i64 - 1));
                    let (r0, r1) = ((py(n) - 1).max(0), (py(s) + 1).min(lv.height as i64 - 1));
                    if c0 > c1 || r0 > r1 {
                        continue;
                    }
                    for ty in (r0 as u32 / lv.tile)..=(r1 as u32 / lv.tile) {
                        for tx in (c0 as u32 / lv.tile)..=(c1 as u32 / lv.tile) {
                            keys.push(TileKey { cell, level, tx, ty });
                        }
                    }
                }
            }
        }
    }
    if waiting.is_empty() { Ok(keys) } else { Err(waiting) }
}

/// Advance jobs within the frame budget; spawn finished meshes.
pub fn run_jobs(
    mut terrain: ResMut<Terrain>,
    mut cog: ResMut<Cog>,
    origin: Res<WorldOrigin>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    cog.pump();
    let start = Instant::now();
    let terrain = &mut *terrain;
    let mut planning = 0;
    let mut i = 0;
    while i < terrain.jobs.len() {
        if start.elapsed().as_secs_f64() * 1e3 > FRAME_BUDGET_MS {
            break;
        }
        let job = &mut terrain.jobs[i];
        let spec = job.patch.spec(&terrain.grid);
        match &job.stage {
            Stage::Plan => {
                if planning > 48 {
                    i += 1;
                    continue;
                }
                planning += 1;
                job.ll = node_lonlats(&terrain.albers, &spec);
                job.stage = match needed_tiles(&mut cog, &job.ll, spec.level) {
                    Ok(keys) => {
                        for k in &keys {
                            cog.tile(*k);
                        }
                        Stage::Wait(keys, Vec::new())
                    }
                    Err(cells) => Stage::Wait(Vec::new(), cells),
                };
                i += 1;
            }
            Stage::Wait(keys, cells) => {
                planning += 1;
                if !cells.is_empty() {
                    if cells.iter().all(|c| cog.cells.get(c).is_some_and(|s| !matches!(s, crate::cog::CellState::Pending))) {
                        job.stage = Stage::Plan;
                    }
                    i += 1;
                    continue;
                }
                if keys.iter().all(|k| cog.tile(*k).is_some()) {
                    job.stage = Stage::Build;
                } else {
                    i += 1;
                }
            }
            Stage::Build => {
                let m = spec.n + 2;
                while job.heights.len() < m * m && start.elapsed().as_secs_f64() * 1e3 < FRAME_BUDGET_MS {
                    let row = job.heights.len() / m;
                    for ii in 0..m {
                        let (lon, lat) = job.ll[row * m + ii];
                        let h = match cog.sample(lon, lat, spec.level) {
                            Some(Some(h)) => h,
                            _ => OCEAN_M,
                        };
                        job.heights.push(h);
                    }
                }
                if job.heights.len() < m * m {
                    break;
                }
                let job = terrain.jobs.remove(i);
                let (mesh, heights) = build_mesh(&spec, &job.heights);
                let x_min = spec.x_min;
                let y_max = spec.y_max;
                let entity = commands
                    .spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(terrain.material.clone()),
                        Anchor(DVec2::new(x_min, y_max)),
                        Transform::from_translation(crate::world_pos(&origin, x_min, y_max, 0.0)),
                        Visibility::Hidden,
                    ))
                    .id();
                terrain.built.insert(job.patch, Built { entity, heights: Arc::new(heights) });
            }
        }
    }
}

fn color(h: f32, slope: f32) -> [f32; 4] {
    let ramp: [(f32, [f32; 3]); 6] = [
        (-50.0, [0.76, 0.70, 0.50]),
        (150.0, [0.50, 0.55, 0.28]),
        (900.0, [0.17, 0.33, 0.13]),
        (2200.0, [0.28, 0.33, 0.20]),
        (3000.0, [0.45, 0.43, 0.40]),
        (3700.0, [0.95, 0.96, 0.98]),
    ];
    let mut c = ramp[ramp.len() - 1].1;
    for w in ramp.windows(2) {
        if h < w[1].0 {
            let t = ((h - w[0].0) / (w[1].0 - w[0].0)).clamp(0.0, 1.0);
            c = [0, 1, 2].map(|k| w[0].1[k] + (w[1].1[k] - w[0].1[k]) * t);
            break;
        }
    }
    let rock = [0.42, 0.39, 0.35];
    let r = ((slope - 0.55) / 0.35).clamp(0.0, 1.0);
    let c = [0, 1, 2].map(|k| c[k] + (rock[k] - c[k]) * r);
    // sRGB → linear for the vertex color attribute.
    let lin = |v: f32| v.powf(2.2);
    [lin(c[0]), lin(c[1]), lin(c[2]), 1.0]
}

/// Grid mesh (local to the NW corner: +X east, +Y up, +Z south) with skirts.
fn build_mesh(s: &Spec, ringed: &[f32]) -> (Mesh, Heights) {
    let n = s.n;
    let m = n + 2;
    let sp = s.spacing as f32;
    let hr = |i: usize, j: usize| ringed[(j + 1) * m + (i + 1)];
    let hr_s = |i: isize, j: isize| ringed[((j + 1) as usize) * m + (i + 1) as usize];
    let mut pos = Vec::with_capacity(n * n + 4 * n);
    let mut nor = Vec::with_capacity(n * n + 4 * n);
    let mut col = Vec::with_capacity(n * n + 4 * n);
    let mut inner = Vec::with_capacity(n * n);
    for j in 0..n {
        for i in 0..n {
            let h = hr(i, j);
            inner.push(h);
            let (ii, jj) = (i as isize, j as isize);
            let dx = (hr_s(ii + 1, jj) - hr_s(ii - 1, jj)) / (2.0 * sp);
            let dz = (hr_s(ii, jj + 1) - hr_s(ii, jj - 1)) / (2.0 * sp);
            let nv = Vec3::new(-dx, 1.0, -dz).normalize();
            pos.push([i as f32 * sp, h, j as f32 * sp]);
            nor.push(nv.to_array());
            col.push(color(h, 1.0 - nv.y));
        }
    }
    let v = |i: usize, j: usize| (j * n + i) as u32;
    let mut idx = Vec::with_capacity((n - 1) * (n - 1) * 6 + 24 * n);
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let (a, b, c, d) = (v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j + 1));
            idx.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    let depth = (s.spacing as f32 * 2.0).clamp(20.0, 800.0);
    let edges: [Vec<u32>; 4] = [
        (0..n).map(|i| v(i, 0)).collect(),
        (0..n).map(|j| v(n - 1, j)).collect(),
        (0..n).rev().map(|i| v(i, n - 1)).collect(),
        (0..n).rev().map(|j| v(0, j)).collect(),
    ];
    for e in edges {
        let base = pos.len() as u32;
        for &k in &e {
            let p = pos[k as usize];
            pos.push([p[0], p[1] - depth, p[2]]);
            nor.push(nor[k as usize]);
            col.push(col[k as usize]);
        }
        for q in 0..e.len() - 1 {
            let (a, b) = (e[q], e[q + 1]);
            let (a2, b2) = (base + q as u32, base + q as u32 + 1);
            idx.extend_from_slice(&[a, b, a2, b, b2, a2]);
        }
    }
    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nor)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx));
    (mesh, Heights { x_min: s.x_min, y_max: s.y_max, spacing: s.spacing, n, data: inner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_follow_mesh_triangles() {
        let h = Heights { x_min: 0.0, y_max: 100.0, spacing: 10.0, n: 2, data: vec![0.0, 10.0, 20.0, 40.0] };
        assert_eq!(h.at(0.0, 100.0), Some(0.0));
        assert_eq!(h.at(10.0, 90.0), Some(40.0));
        assert_eq!(h.at(5.0, 100.0), Some(5.0));
        assert!((h.at(7.5, 92.5).unwrap() - 32.5).abs() < 1e-4); // NE-SW diagonal split, lower-right triangle
        assert_eq!(h.at(-1.0, 100.0), None);
    }

    #[test]
    fn lattice_lonlats_match_direct_projection() {
        let g = GridSpec::default();
        let albers = Albers::new().unwrap();
        for p in [Patch::Tile { t: TileId::new(102, 352), lod: 0 }, Patch::Super { sx: 10, sy: 35 }] {
            let s = p.spec(&g);
            let ll = node_lonlats(&albers, &s);
            let m = s.n + 2;
            for (ii, jj) in [(0, 0), (m / 2, 7), (m - 1, m - 1), (3, m - 2)] {
                let want = albers.to_lonlat(s.x_min + (ii as f64 - 1.0) * s.spacing, s.y_max - (jj as f64 - 1.0) * s.spacing).unwrap();
                let got = ll[jj * m + ii];
                // Horizontal error in meters ≈ degrees × 111 km.
                let err = ((got.0 - want.0).abs() + (got.1 - want.1).abs()) * 111_000.0;
                assert!(err < s.spacing * 0.02, "{p:?} node ({ii},{jj}) off by {err} m");
            }
        }
    }

    #[test]
    fn california_has_a_few_hundred_super_tiles() {
        let n = california_supers(&GridSpec::default(), &Albers::new().unwrap()).len();
        assert!((250..=450).contains(&n), "{n}");
    }
}
