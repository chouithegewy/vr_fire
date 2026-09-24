//! Level-of-detail terrain for all of California.
//!
//! Patches: 37.5 km super tiles (750 m nodes) cover the state; near the camera each splits
//! into its 100 grid tiles (3.75 km, aligned to the ML 30 m grid) at 250/150/50/30/10 m
//! (`vr_fire::lod`), picked so node spacing stays about 0.4% of view distance: zooming in
//! refines in small steps instead of big pops.
//!
//! Heights come from one of two sources, toggled with C:
//! - **compressed**: pre-packed `.vrh` patches (quantized + 2D predictor + Brotli, ~8–18×
//!   smaller than f32) served next to the app; any patch without a file falls back to COG;
//! - **COG**: USGS 3DEP cloud-optimized GeoTIFF pieces fetched straight from S3.
//!
//! Meshes are built on a per-frame time budget so streaming never blows a 120 fps frame.

use crate::cog::{Cell, Cog, TileKey};
use crate::{Anchor, WorldOrigin};
use bevy::asset::RenderAssetUsages;
use bevy::math::DVec2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use vr_fire::crs::{Albers, LonLatBox};
use vr_fire::grid::{GridSpec, NODE_SPACING_M, TILE_SIZE_M, TileId};
use vr_fire::lod::{self, OCEAN_M, SUPER_STRIDE, SUPER_TILES, TILE_STRIDES};
use vr_fire::region::Region;

const SUPER_SIZE: f64 = TILE_SIZE_M * SUPER_TILES as f64;
/// Node spacing ÷ view distance: the finest level with spacing ≥ distance × K is used.
const K: f64 = 0.004;
/// Super tiles split into grid tiles when closer than this (coarsest tile level / K).
const SPLIT_DIST: f64 = 250.0 / K;
const FRAME_BUDGET_MS: f64 = 4.0;
const LATTICE: usize = 25;
const MAX_PACKED_INFLIGHT: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Patch {
    Super { sx: i64, sy: i64 },
    Tile { t: TileId, lod: u8 },
    /// On-demand 3.75 m tile from USGS 1 m lidar (via the `hires` service).
    Hires { t: TileId },
}

/// Progress of an on-demand lidar tile.
#[derive(Clone, Debug, PartialEq)]
pub enum HiresState {
    /// Server is fetching/processing; poll again after `next_poll` seconds.
    Waiting { age: f32, next_poll: f32, polling: bool },
    Building,
    Ready,
    Unavailable(String),
}

/// How close a lidar tile must be to replace the regular levels.
const HIRES_SHOW_DIST: f64 = 15_000.0;

struct Spec {
    x_min: f64,
    y_max: f64,
    spacing: f64,
    n: usize,
    /// COG overview level whose pixels best match `spacing`.
    level: u8,
}

/// COG overview levels are ~10, 20, 40, 80, 160, 320 m.
fn cog_level(spacing: f64) -> u8 {
    match spacing as u32 {
        0..=15 => 0,
        16..=35 => 1,
        36..=90 => 2,
        91..=200 => 3,
        201..=400 => 4,
        _ => 5,
    }
}

impl Patch {
    fn spec(&self, g: &GridSpec) -> Spec {
        match *self {
            Patch::Super { sx, sy } => {
                let spacing = NODE_SPACING_M * SUPER_STRIDE as f64;
                Spec {
                    x_min: g.origin_x + SUPER_SIZE * sx as f64,
                    y_max: g.origin_y - SUPER_SIZE * sy as f64,
                    spacing,
                    n: lod::super_nodes(),
                    level: cog_level(spacing),
                }
            }
            Patch::Tile { t, lod } => {
                let b = g.tile_bounds(t);
                let spacing = lod::tile_spacing(lod as usize);
                Spec { x_min: b.x_min, y_max: b.y_max, spacing, n: lod::tile_nodes(lod as usize), level: cog_level(spacing) }
            }
            Patch::Hires { t } => {
                let b = g.tile_bounds(t);
                Spec { x_min: b.x_min, y_max: b.y_max, spacing: lod::HIRES_SPACING_M, n: lod::hires_nodes(), level: 0 }
            }
        }
    }

    fn packed_path(&self) -> String {
        match *self {
            Patch::Super { sx, sy } => lod::super_path(sx, sy),
            Patch::Tile { t, lod } => lod::tile_path(lod as usize, t.tx, t.ty),
            Patch::Hires { t } => lod::hires_path(t.tx, t.ty),
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
    /// Compressed source: waiting for the `.vrh` download.
    Fetch { requested: bool },
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

/// Where terrain heights come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Compressed,
    Cog,
}

#[derive(Resource)]
pub struct Terrain {
    pub albers: Albers,
    pub grid: GridSpec,
    pub source: Source,
    supers: Vec<(i64, i64)>,
    built: HashMap<Patch, Built>,
    jobs: Vec<Job>,
    material: Handle<StandardMaterial>,
    since_update: f32,
    pub stats: (usize, usize),
    /// Bytes of `.vrh` patches downloaded, and patches that fell back to COG.
    pub packed_bytes: u64,
    pub packed_hits: usize,
    pub packed_misses: usize,
    packed_inflight: usize,
    tx: Sender<(Patch, Option<Vec<u8>>)>,
    rx: Mutex<Receiver<(Patch, Option<Vec<u8>>)>>,
    packed: HashMap<Patch, Option<Vec<u8>>>,
    pub hires: HashMap<TileId, HiresState>,
    htx: Sender<(TileId, u16, Vec<u8>)>,
    hrx: Mutex<Receiver<(TileId, u16, Vec<u8>)>>,
}

pub fn california_supers(grid: &GridSpec, albers: &Albers) -> Vec<(i64, i64)> {
    let region = Region::from_geojson(include_str!("../../data/regions/california.geojson")).unwrap();
    let bb: LonLatBox = region.bbox();
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

/// Root URL of the packed `.vrh` tiles.
fn packed_base() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = std::env::var("VR_FIRE_TILES") {
        return url;
    }
    if cfg!(target_arch = "wasm32") { "tiles".into() } else { "https://chilos.dev/vr_fire/tiles".into() }
}

impl Terrain {
    pub fn new(material: Handle<StandardMaterial>) -> Self {
        let grid = GridSpec::default();
        let albers = Albers::new().unwrap();
        let supers = california_supers(&grid, &albers);
        info!("California: {} super tiles", supers.len());
        let (tx, rx) = channel();
        let (htx, hrx) = channel();
        Self {
            albers,
            grid,
            source: Source::Compressed,
            supers,
            built: HashMap::new(),
            jobs: Vec::new(),
            material,
            since_update: 1.0,
            stats: (0, 0),
            packed_bytes: 0,
            packed_hits: 0,
            packed_misses: 0,
            packed_inflight: 0,
            tx,
            rx: Mutex::new(rx),
            packed: HashMap::new(),
            hires: HashMap::new(),
            htx,
            hrx: Mutex::new(hrx),
        }
    }

    /// Terrain height at an EPSG:5070 point from the finest built patch.
    pub fn height_at(&self, x: f64, y: f64) -> Option<(f32, u8)> {
        let t = self.grid.tile_containing(x, y);
        if let Some(h) = self.built.get(&Patch::Hires { t }).and_then(|b| b.heights.at(x, y)) {
            return Some((h, 0));
        }
        for lod in 0..TILE_STRIDES.len() as u8 {
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

    /// Switch height source and rebuild everything from it.
    pub fn set_source(&mut self, source: Source, commands: &mut Commands) {
        if self.source == source {
            return;
        }
        self.source = source;
        // Lidar tiles don't depend on the source: keep them.
        let all: Vec<(Patch, Built)> = self.built.drain().collect();
        for (p, b) in all {
            if matches!(p, Patch::Hires { .. }) {
                self.built.insert(p, b);
            } else {
                commands.entity(b.entity).despawn();
            }
        }
        self.jobs.clear();
        self.packed.clear();
        self.since_update = 1.0;
    }

    /// Ask the `hires` service for 1 m lidar under tile `t` (no-op if already requested).
    pub fn request_hires(&mut self, t: TileId) {
        self.hires.entry(t).or_insert(HiresState::Waiting { age: 0.0, next_poll: 0.0, polling: false });
    }

    fn new_job(&self, patch: Patch, prio: f64) -> Job {
        let stage = match self.source {
            Source::Compressed => Stage::Fetch { requested: false },
            Source::Cog => Stage::Plan,
        };
        Job { patch, prio, stage, ll: Vec::new(), heights: Vec::new() }
    }
}

fn dist_to_rect(px: f64, py: f64, x0: f64, y1: f64, size: f64, alt: f64) -> f64 {
    let dx = (x0 - px).max(0.0).max(px - (x0 + size));
    let dy = ((y1 - size) - py).max(0.0).max(py - y1);
    (dx * dx + dy * dy + alt * alt).sqrt()
}

/// Finest tile level whose node spacing is at least distance × K.
fn tile_lod_for(dist: f64) -> u8 {
    (0..TILE_STRIDES.len()).find(|&l| lod::tile_spacing(l) >= dist * K).unwrap_or(TILE_STRIDES.len() - 1) as u8
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
    let levels = TILE_STRIDES.len() as u8;

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
        for j in 0..SUPER_TILES {
            for i in 0..SUPER_TILES {
                let t = TileId::new(sx * SUPER_TILES + i, sy * SUPER_TILES + j);
                let b = g.tile_bounds(t);
                let et = dist_to_rect(cx, cy, b.x_min, b.y_max, TILE_SIZE_M, alt);
                let lod = tile_lod_for(et);
                if et < HIRES_SHOW_DIST && terrain.built.contains_key(&Patch::Hires { t }) {
                    children.push(Patch::Hires { t });
                    continue;
                }
                wanted.insert(Patch::Tile { t, lod }, et);
                // Display the desired level if built, else the nearest built one (finer first).
                let mut order: Vec<u8> = (0..levels).collect();
                order.sort_by_key(|&l| ((l as i32 - lod as i32).abs(), l));
                match order.iter().map(|&l| Patch::Tile { t, lod: l }).find(|p| terrain.built.contains_key(p)) {
                    Some(p) => children.push(p),
                    None => all_ready = false,
                }
            }
        }
        if all_ready {
            show.extend(children.iter().copied());
        } else {
            show.insert(sp);
        }
        for c in &children {
            wanted.entry(*c).or_insert(f64::MAX); // keep the displayed fallback alive
        }
    }

    // Lidar tiles are never evicted; they're only drawn while their super tile is split.
    for (t, st) in &terrain.hires {
        if matches!(st, HiresState::Building | HiresState::Ready) {
            wanted.insert(Patch::Hires { t: *t }, f64::MAX);
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
    let new: Vec<Job> = wanted
        .iter()
        .filter(|(p, prio)| **prio != f64::MAX && !terrain.built.contains_key(p) && !queued.contains(p))
        .map(|(p, prio)| terrain.new_job(*p, *prio))
        .collect();
    terrain.jobs.extend(new);
    for j in terrain.jobs.iter_mut() {
        if let Some(p) = wanted.get(&j.patch) {
            j.prio = *p;
        }
    }
    terrain.jobs.sort_by(|a, b| a.prio.total_cmp(&b.prio));
    terrain.packed.retain(|p, _| wanted.contains_key(p));
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
        let bj = (jj / LATTICE).min(k - 2);
        let fj = (jj as f64 - (bj * LATTICE) as f64) / LATTICE as f64;
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

fn request_packed(terrain: &mut Terrain, patch: Patch) {
    terrain.packed_inflight += 1;
    let tx = terrain.tx.clone();
    let url = format!("{}/{}", packed_base(), patch.packed_path());
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        let bytes = match res {
            Ok(r) if r.status == 200 => Some(r.bytes),
            _ => None,
        };
        let _ = tx.send((patch, bytes));
    });
}

/// Advance jobs within the frame budget; spawn finished meshes.
pub fn run_jobs(
    time: Res<Time>,
    mut terrain: ResMut<Terrain>,
    mut cog: ResMut<Cog>,
    origin: Res<WorldOrigin>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    cog.pump();
    let start = Instant::now();
    let terrain = &mut *terrain;
    poll_hires(terrain, time.delta_secs());
    let lidar: Vec<_> = terrain.hrx.lock().unwrap().try_iter().collect();
    for (t, status, bytes) in lidar {
        let state = match status {
            200 => match vr_fire::codec::decode(&bytes) {
                Ok((side, h)) if side == lod::hires_nodes() + 2 => {
                    terrain.packed_bytes += bytes.len() as u64;
                    terrain.jobs.insert(0, Job { patch: Patch::Hires { t }, prio: -1.0, stage: Stage::Build, ll: Vec::new(), heights: h });
                    HiresState::Building
                }
                _ => HiresState::Unavailable("corrupt lidar patch".into()),
            },
            202 | 503 => HiresState::Waiting { age: terrain.hires_age(t), next_poll: 3.0, polling: false },
            404 => HiresState::Unavailable(String::from_utf8_lossy(&bytes).chars().take(80).collect()),
            429 => HiresState::Unavailable("daily lidar request cap reached, try tomorrow".into()),
            _ => HiresState::Waiting { age: terrain.hires_age(t), next_poll: 6.0, polling: false },
        };
        terrain.hires.insert(t, state);
    }
    let arrived: Vec<_> = terrain.rx.lock().unwrap().try_iter().collect();
    for (patch, bytes) in arrived {
        terrain.packed_inflight -= 1;
        if let Some(b) = &bytes {
            terrain.packed_bytes += b.len() as u64;
        }
        terrain.packed.insert(patch, bytes);
    }
    let mut planning = 0;
    let mut i = 0;
    while i < terrain.jobs.len() {
        if start.elapsed().as_secs_f64() * 1e3 > FRAME_BUDGET_MS {
            break;
        }
        let spec = terrain.jobs[i].patch.spec(&terrain.grid);
        match &terrain.jobs[i].stage {
            Stage::Fetch { requested } => {
                let patch = terrain.jobs[i].patch;
                if let Some(result) = terrain.packed.remove(&patch) {
                    let decoded = result.and_then(|b| vr_fire::codec::decode(&b).ok()).filter(|(side, _)| *side == spec.n + 2);
                    let job = &mut terrain.jobs[i];
                    match decoded {
                        Some((_, h)) => {
                            terrain.packed_hits += 1;
                            job.heights = h;
                            job.stage = Stage::Build;
                        }
                        None => {
                            terrain.packed_misses += 1;
                            job.stage = Stage::Plan; // no packed file here: use COG
                        }
                    }
                } else if !requested && terrain.packed_inflight < MAX_PACKED_INFLIGHT {
                    request_packed(terrain, patch);
                    terrain.jobs[i].stage = Stage::Fetch { requested: true };
                    i += 1;
                } else {
                    i += 1;
                }
            }
            Stage::Plan => {
                if planning > 48 {
                    i += 1;
                    continue;
                }
                planning += 1;
                let job = &mut terrain.jobs[i];
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
                let next = if !cells.is_empty() {
                    let headers_done = cells.iter().all(|c| cog.cells.get(c).is_some_and(|s| !matches!(s, crate::cog::CellState::Pending)));
                    headers_done.then_some(Stage::Plan)
                } else {
                    let keys = keys.clone();
                    keys.iter().all(|k| cog.tile(*k).is_some()).then_some(Stage::Build)
                };
                let building = matches!(next, Some(Stage::Build));
                if let Some(stage) = next {
                    terrain.jobs[i].stage = stage;
                }
                if !building {
                    i += 1;
                }
            }
            Stage::Build => {
                let m = spec.n + 2;
                let job = &mut terrain.jobs[i];
                // COG path samples row by row within the budget; packed patches arrive complete.
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
                let entity = commands
                    .spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(terrain.material.clone()),
                        Anchor(DVec2::new(spec.x_min, spec.y_max)),
                        Transform::from_translation(crate::world_pos(&origin, spec.x_min, spec.y_max, 0.0)),
                        Visibility::Hidden,
                    ))
                    .id();
                if let Patch::Hires { t } = job.patch {
                    terrain.hires.insert(t, HiresState::Ready);
                }
                terrain.built.insert(job.patch, Built { entity, heights: Arc::new(heights) });
            }
        }
    }
}

impl Terrain {
    fn hires_age(&self, t: TileId) -> f32 {
        match self.hires.get(&t) {
            Some(HiresState::Waiting { age, .. }) => *age,
            _ => 0.0,
        }
    }
}

fn hires_base() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = std::env::var("VR_FIRE_HIRES") {
        return url;
    }
    if cfg!(target_arch = "wasm32") { "hires".into() } else { "https://chilos.dev/vr_fire/hires".into() }
}

/// Poll the `hires` service for every waiting lidar tile.
fn poll_hires(terrain: &mut Terrain, dt: f32) {
    let mut due = Vec::new();
    for (t, st) in terrain.hires.iter_mut() {
        if let HiresState::Waiting { age, next_poll, polling } = st {
            *age += dt;
            *next_poll -= dt;
            if !*polling && *next_poll <= 0.0 {
                *polling = true;
                due.push(*t);
            }
        }
    }
    for t in due {
        let tx = terrain.htx.clone();
        let url = format!("{}/{}", hires_base(), lod::hires_path(t.tx, t.ty));
        ehttp::fetch(ehttp::Request::get(url), move |res| {
            let _ = match res {
                Ok(r) => tx.send((t, r.status, r.bytes)),
                Err(e) => tx.send((t, 0, e.into_bytes())),
            };
        });
    }
}

fn color(h: f32, slope: f32) -> [f32; 4] {
    let ramp: [(f32, [f32; 3]); 7] = [
        (-200.0, [0.30, 0.34, 0.36]),
        (-50.0, [0.76, 0.70, 0.50]),
        (150.0, [0.50, 0.55, 0.28]),
        (900.0, [0.17, 0.33, 0.13]),
        (2200.0, [0.28, 0.33, 0.20]),
        (3000.0, [0.45, 0.43, 0.40]),
        (3700.0, [0.95, 0.96, 0.98]),
    ];
    let mut c = ramp[ramp.len() - 1].1;
    if h < ramp[0].0 {
        c = ramp[0].1;
    } else {
        for w in ramp.windows(2) {
            if h < w[1].0 {
                let t = ((h - w[0].0) / (w[1].0 - w[0].0)).clamp(0.0, 1.0);
                c = [0, 1, 2].map(|k| w[0].1[k] + (w[1].1[k] - w[0].1[k]) * t);
                break;
            }
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
        // Lower-right triangle (b, c, d): 40 − 20·0.25 − 30·0.25. Bilinear would give 17.5.
        assert!((h.at(7.5, 92.5).unwrap() - 27.5).abs() < 1e-4);
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

    #[test]
    fn detail_steps_smoothly_with_distance() {
        let lods: Vec<u8> = [1_000.0, 5_000.0, 10_000.0, 20_000.0, 50_000.0, 100_000.0].map(tile_lod_for).to_vec();
        assert_eq!(lods, vec![0, 1, 2, 3, 4, 4]);
        // Every level is used somewhere, and each step coarsens by at most 3×.
        for l in 1..TILE_STRIDES.len() {
            assert!(lod::tile_spacing(l) / lod::tile_spacing(l - 1) <= 3.0);
        }
    }
}
