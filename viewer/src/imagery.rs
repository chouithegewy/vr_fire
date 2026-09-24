//! Aerial imagery draped on the terrain: USDA/USGS orthoimagery (NAIP-based, public domain)
//! from The National Map's Web Mercator tile service, which allows cross-origin requests.
//!
//! A *mosaic* is a rectangle of whole 256 px Mercator tiles stitched into one texture (no
//! resampling). Terrain vertices get UVs computed from their lon/lat, so the Albers-grid
//! meshes and Mercator imagery line up exactly at vertices.
//!
//! - UV0 → the statewide atlas (zoom 8, ~480 m/px): every patch shows real imagery at once.
//! - UV1 → the patch's detail mosaic, swapped in when ready: zoom 12 (~30 m/px) per super
//!   tile for 150/250 m tiles, zoom 14 (~7.5 m/px) per tile for 50/30 m tiles, zoom 15
//!   (~3.8 m/px) for 10 m tiles, zoom 16 (~1.9 m/px) for lidar.
//!
//! Mip maps are built on the CPU so distant imagery doesn't shimmer; JPEG decoding runs on
//! a per-frame time budget; detail mosaics no patch uses are freed.

use bevy::asset::RenderAssetUsages;
use bevy::math::DVec2;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use bevy::image::{ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use vr_fire::crs::LonLatBox;

const BASE: &str = "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer/tile";
const TILE: u32 = 256;
pub const ATLAS_Z: u8 = 8;
const MAX_INFLIGHT: usize = 16;
const DECODE_BUDGET_MS: f64 = 2.5;
/// Seconds a detail mosaic may go unused before it's freed.
const EVICT_AFTER_S: f32 = 20.0;
/// Near-truck imagery: USGS NAIP (0.3–0.6 m source) exported straight into our EPSG:5070
/// grid as a 2560² JPEG over a 1.6 km square (~0.63 m/px, near NAIP's native 0.6 m in
/// California), re-centred when the truck gets
/// 400 m from its centre, fading out over the outer 120 m. The Mercator tile service stops
/// at zoom 16 (~1.9 m/px), which is why this uses the ImageServer's export instead.
const NAIP: &str = "https://imagery.nationalmap.gov/arcgis/rest/services/USGSNAIPImagery/ImageServer/exportImage";
const NEAR_PX: u32 = 2560;
const NEAR_HALF_M: f64 = 800.0;
const NEAR_RECENTER_M: f64 = 400.0;
const NEAR_FADE_M: f64 = 120.0;
const NEAR_RETRY_S: f32 = 10.0;

/// Terrain material: StandardMaterial plus the sharp near-truck image (`near.wgsl`).
pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, NearImagery>;

/// World X/Z → near-image UV: uv.x = u·(x, z, 1), uv.y = v·(x, z, 1);
/// u.w = 1 when enabled, v.w = edge fade width in UV units.
#[derive(Clone, Copy, Debug, Default, ShaderType, Reflect)]
pub struct NearParams {
    pub u: Vec4,
    pub v: Vec4,
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct NearImagery {
    #[uniform(100)]
    pub params: NearParams,
    #[texture(101)]
    #[sampler(102)]
    pub texture: Option<Handle<Image>>,
}

impl MaterialExtension for NearImagery {
    fn fragment_shader() -> ShaderRef {
        "embedded://viewer/near.wgsl".into()
    }
}

/// Web Mercator pixel coordinates at zoom `z` (256 px tiles).
pub fn merc_px(z: u8, lon: f64, lat: f64) -> (f64, f64) {
    let n = TILE as f64 * (1u64 << z) as f64;
    let lat = lat.to_radians();
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MosaicKey {
    pub z: u8,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl MosaicKey {
    pub fn covering(z: u8, b: &LonLatBox) -> Self {
        let (ax, ay) = merc_px(z, b.west, b.north);
        let (bx, by) = merc_px(z, b.east, b.south);
        Self { z, x0: (ax / TILE as f64) as u32, y0: (ay / TILE as f64) as u32, x1: (bx / TILE as f64) as u32, y1: (by / TILE as f64) as u32 }
    }
    fn size(&self) -> (u32, u32) {
        ((self.x1 - self.x0 + 1) * TILE, (self.y1 - self.y0 + 1) * TILE)
    }
    /// Texture coordinate of a lon/lat inside this mosaic.
    pub fn uv(&self, lon: f64, lat: f64) -> [f32; 2] {
        let (px, py) = merc_px(self.z, lon, lat);
        let (w, h) = self.size();
        [((px - (self.x0 * TILE) as f64) / w as f64) as f32, ((py - (self.y0 * TILE) as f64) / h as f64) as f32]
    }
    fn tiles(&self) -> impl Iterator<Item = (u8, u32, u32)> + '_ {
        (self.y0..=self.y1).flat_map(move |y| (self.x0..=self.x1).map(move |x| (self.z, x, y)))
    }
}

enum TileState {
    Fetching,
    Ready(Arc<Vec<u8>>),
    Missing,
}

enum MosaicState {
    Waiting,
    Building { rgba: Vec<u8>, next: usize },
    Ready(Handle<TerrainMaterial>),
}

/// Which mosaic (and so which material) a terrain patch uses for detail; None = atlas only.
#[derive(Component)]
pub struct Draped(pub Option<MosaicKey>);

#[derive(Resource)]
pub struct Imagery {
    tiles: HashMap<(u8, u32, u32), TileState>,
    queue: VecDeque<(u8, u32, u32)>,
    inflight: usize,
    tx: Sender<((u8, u32, u32), Option<Vec<u8>>)>,
    rx: Mutex<Receiver<((u8, u32, u32), Option<Vec<u8>>)>>,
    mosaics: HashMap<MosaicKey, MosaicState>,
    last_used: HashMap<MosaicKey, f32>,
    pub atlas: MosaicKey,
    pub atlas_material: Handle<TerrainMaterial>,
    atlas_done: bool,
    pub bytes: u64,
    since_apply: f32,
    /// Current near image (EPSG:5070 centre) and the centre being fetched.
    near: Option<(DVec2, Handle<Image>)>,
    near_pending: Option<DVec2>,
    near_tx: Sender<(DVec2, Option<Vec<u8>>)>,
    near_rx: Mutex<Receiver<(DVec2, Option<Vec<u8>>)>>,
    near_retry: f32,
    pub near_bytes: u64,
    /// Extension shared by every terrain material (updated when the near image moves).
    near_ext: NearImagery,
    near_dirty: bool,
    since_near: f32,
}

impl Imagery {
    pub fn new(atlas_bounds: &LonLatBox, mats: &mut Assets<TerrainMaterial>) -> Self {
        let (tx, rx) = channel();
        let (near_tx, near_rx) = channel();
        let atlas = MosaicKey::covering(ATLAS_Z, atlas_bounds);
        // Until the atlas arrives, patches show a neutral dry-grass tone.
        let atlas_material = mats.add(TerrainMaterial {
            base: StandardMaterial { base_color: Color::srgb(0.55, 0.52, 0.40), perceptual_roughness: 0.95, reflectance: 0.2, ..default() },
            extension: NearImagery::default(),
        });
        let mut me = Self {
            tiles: HashMap::new(),
            queue: VecDeque::new(),
            inflight: 0,
            tx,
            rx: Mutex::new(rx),
            mosaics: HashMap::new(),
            last_used: HashMap::new(),
            atlas,
            atlas_material,
            atlas_done: false,
            bytes: 0,
            since_apply: 0.0,
            near: None,
            near_pending: None,
            near_tx,
            near_rx: Mutex::new(near_rx),
            near_retry: 0.0,
            near_bytes: 0,
            near_ext: NearImagery::default(),
            near_dirty: false,
            since_near: 0.0,
        };
        me.request(atlas);
        me
    }

    fn request(&mut self, key: MosaicKey) {
        if self.mosaics.contains_key(&key) {
            return;
        }
        self.mosaics.insert(key, MosaicState::Waiting);
        for t in key.tiles() {
            if !self.tiles.contains_key(&t) {
                self.tiles.insert(t, TileState::Fetching);
                self.queue.push_back(t);
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.queue.len() + self.inflight
    }
}

fn fetch(tx: Sender<((u8, u32, u32), Option<Vec<u8>>)>, t: (u8, u32, u32)) {
    let url = format!("{BASE}/{}/{}/{}", t.0, t.2, t.1);
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        let bytes = match res {
            Ok(r) if r.status == 200 => Some(r.bytes),
            _ => None,
        };
        let _ = tx.send((t, bytes));
    });
}

fn decode_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let px = dec.decode().ok()?;
    (px.len() == (TILE * TILE * 4) as usize).then_some(px)
}

/// All mip levels of an RGBA8 image, concatenated (2×2 box filter).
fn with_mips(mut level: Vec<u8>, mut w: u32, mut h: u32) -> (Vec<u8>, u32) {
    let mut out = level.clone();
    let mut count = 1;
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let at = |xx: u32, yy: u32| level[((yy.min(h - 1) * w + xx.min(w - 1)) * 4 + c) as usize] as u32;
                    let (sx, sy) = (x * 2, y * 2);
                    next[((y * nw + x) * 4 + c) as usize] = ((at(sx, sy) + at(sx + 1, sy) + at(sx, sy + 1) + at(sx + 1, sy + 1) + 2) / 4) as u8;
                }
            }
        }
        out.extend_from_slice(&next);
        level = next;
        (w, h) = (nw, nh);
        count += 1;
    }
    (out, count)
}

fn make_image(rgba: Vec<u8>, w: u32, h: u32) -> Image {
    let (data, levels) = with_mips(rgba, w, h);
    let mut image = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        vec![0; (w * h * 4) as usize],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    image
}

/// Fetch tiles, stitch mosaics within the frame budget, and publish finished textures.
pub fn run(mut imagery: ResMut<Imagery>, mut images: ResMut<Assets<Image>>, mut mats: ResMut<Assets<TerrainMaterial>>) {
    let im = &mut *imagery;
    let arrived: Vec<_> = im.rx.lock().unwrap().try_iter().collect();
    for (t, bytes) in arrived {
        im.inflight -= 1;
        let state = match bytes {
            Some(b) => {
                im.bytes += b.len() as u64;
                TileState::Ready(Arc::new(b))
            }
            None => TileState::Missing,
        };
        im.tiles.insert(t, state);
    }
    // The atlas goes first; then nearest-requested detail.
    while im.inflight < MAX_INFLIGHT {
        let Some(t) = im.queue.pop_front() else { break };
        im.inflight += 1;
        fetch(im.tx.clone(), t);
    }
    let start = Instant::now();
    let keys: Vec<MosaicKey> = im.mosaics.keys().copied().collect();
    for key in keys {
        if start.elapsed().as_secs_f64() * 1e3 > DECODE_BUDGET_MS {
            break;
        }
        let ready_to_build = matches!(im.mosaics.get(&key), Some(MosaicState::Waiting))
            && key.tiles().all(|t| !matches!(im.tiles.get(&t), Some(TileState::Fetching) | None));
        if ready_to_build {
            let (w, h) = key.size();
            // Grey-brown where a tile is missing (outside imagery coverage).
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            rgba.chunks_exact_mut(4).for_each(|p| p.copy_from_slice(&[128, 120, 100, 255]));
            im.mosaics.insert(key, MosaicState::Building { rgba, next: 0 });
        }
        let Some(MosaicState::Building { rgba, next }) = im.mosaics.get_mut(&key) else { continue };
        let tiles: Vec<_> = key.tiles().collect();
        let (w, _) = key.size();
        while *next < tiles.len() && start.elapsed().as_secs_f64() * 1e3 < DECODE_BUDGET_MS {
            let (z, x, y) = tiles[*next];
            if let Some(TileState::Ready(jpeg)) = im.tiles.get(&(z, x, y)) {
                if let Some(px) = decode_jpeg(jpeg) {
                    let (ox, oy) = ((x - key.x0) * TILE, (y - key.y0) * TILE);
                    for row in 0..TILE {
                        let dst = (((oy + row) * w + ox) * 4) as usize;
                        let src = (row * TILE * 4) as usize;
                        rgba[dst..dst + (TILE * 4) as usize].copy_from_slice(&px[src..src + (TILE * 4) as usize]);
                    }
                }
            }
            *next += 1;
        }
        if *next < tiles.len() {
            continue;
        }
        let Some(MosaicState::Building { rgba, .. }) = im.mosaics.remove(&key) else { unreachable!() };
        let (w, h) = key.size();
        let image = images.add(make_image(rgba, w, h));
        if key == im.atlas {
            if let Some(mut m) = mats.get_mut(&im.atlas_material) {
                m.base.base_color = Color::WHITE;
                m.base.base_color_texture = Some(image);
            }
            im.atlas_done = true;
            im.mosaics.insert(key, MosaicState::Ready(im.atlas_material.clone()));
        } else {
            let material = mats.add(TerrainMaterial {
                base: StandardMaterial {
                    base_color_texture: Some(image),
                    base_color_channel: bevy::mesh::UvChannel::Uv1,
                    perceptual_roughness: 0.95,
                    reflectance: 0.2,
                    ..default()
                },
                extension: im.near_ext.clone(),
            });
            im.mosaics.insert(key, MosaicState::Ready(material));
        }
    }
}

/// Request detail mosaics for visible patches, swap their material in when ready, and free
/// detail mosaics nothing uses.
pub fn apply(
    time: Res<Time>,
    mut imagery: ResMut<Imagery>,
    mut patches: Query<(&Draped, &ViewVisibility, &mut MeshMaterial3d<TerrainMaterial>)>,
) {
    let im = &mut *imagery;
    let now = time.elapsed_secs();
    im.since_apply += time.delta_secs();
    if im.since_apply < 0.25 {
        return;
    }
    im.since_apply = 0.0;
    let mut used = HashSet::new();
    for (draped, visible, mut material) in &mut patches {
        let Some(key) = draped.0 else { continue };
        used.insert(key);
        if !visible.get() {
            continue;
        }
        im.last_used.insert(key, now);
        match im.mosaics.get(&key) {
            None => im.request(key),
            Some(MosaicState::Ready(m)) if material.0 != *m => material.0 = m.clone(),
            _ => {}
        }
    }
    let atlas = im.atlas;
    let stale: Vec<MosaicKey> = im
        .mosaics
        .keys()
        .filter(|k| **k != atlas)
        .filter(|k| !used.contains(k) || now - im.last_used.get(k).copied().unwrap_or(0.0) > EVICT_AFTER_S)
        .copied()
        .collect();
    for k in stale {
        // Patches still pointing at it fall back to the atlas.
        if let Some(MosaicState::Ready(m)) = im.mosaics.remove(&k) {
            for (draped, _, mut material) in &mut patches {
                if draped.0 == Some(k) && material.0 == m {
                    material.0 = im.atlas_material.clone();
                }
            }
        }
        im.last_used.remove(&k);
    }
    // Keep only JPEGs a live mosaic still needs (bounded memory).
    let live: HashSet<(u8, u32, u32)> = im.mosaics.keys().flat_map(|k| k.tiles().collect::<Vec<_>>()).collect();
    im.tiles.retain(|t, s| live.contains(t) || matches!(s, TileState::Fetching));
}

/// World X/Z → near-image UV. The image is exported in EPSG:5070, the same grid as the
/// world, so the map is exact: u = (x + origin.x − x_min)/size, v = (y_max − origin.y + z)/size.
fn near_params(centre: DVec2, origin: DVec2) -> NearParams {
    let size = 2.0 * NEAR_HALF_M;
    let (x_min, y_max) = (centre.x - NEAR_HALF_M, centre.y + NEAR_HALF_M);
    let s = (1.0 / size) as f32;
    NearParams {
        u: Vec4::new(s, 0.0, ((origin.x - x_min) / size) as f32, 1.0),
        v: Vec4::new(0.0, s, ((y_max - origin.y) / size) as f32, (NEAR_FADE_M / size) as f32),
    }
}

fn fetch_near(tx: Sender<(DVec2, Option<Vec<u8>>)>, centre: DVec2) {
    let (x0, y0, x1, y1) = (centre.x - NEAR_HALF_M, centre.y - NEAR_HALF_M, centre.x + NEAR_HALF_M, centre.y + NEAR_HALF_M);
    let url = format!("{NAIP}?bbox={x0:.0},{y0:.0},{x1:.0},{y1:.0}&bboxSR=5070&imageSR=5070&size={NEAR_PX},{NEAR_PX}&format=jpg&f=image");
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        let bytes = match res {
            Ok(r) if r.status == 200 && r.bytes.starts_with(&[0xFF, 0xD8]) => Some(r.bytes),
            _ => None,
        };
        let _ = tx.send((centre, bytes));
    });
}

/// Keep a sharp NAIP image centred on the truck and push its placement to every terrain material.
pub fn near(
    time: Res<Time>,
    truck: Res<crate::truck::Truck>,
    origin: Res<crate::WorldOrigin>,
    mut imagery: ResMut<Imagery>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<TerrainMaterial>>,
) {
    let im = &mut *imagery;
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("VR_FIRE_NEAR").is_ok_and(|v| v == "off") {
        return; // A/B comparisons
    }
    let arrived: Vec<_> = im.near_rx.lock().unwrap().try_iter().collect();
    for (centre, bytes) in arrived {
        im.near_pending = None;
        let decoded = bytes.as_ref().and_then(|b| {
            use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
            let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
            let px = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(b.as_slice()), options).decode().ok()?;
            (px.len() == (NEAR_PX * NEAR_PX * 4) as usize).then_some(px)
        });
        match decoded {
            Some(px) => {
                info!("near imagery ready at {centre:?} ({} KB)", bytes.as_ref().map_or(0, |b| b.len()) / 1000);
                im.near_bytes += bytes.map_or(0, |b| b.len() as u64);
                im.near = Some((centre, images.add(make_image(px, NEAR_PX, NEAR_PX))));
            }
            // No NAIP here or the export failed: show no overlay rather than a fill colour.
            None => {
                warn!("near imagery unavailable at {centre:?}; retrying in {NEAR_RETRY_S} s");
                im.near = None;
                im.near_retry = NEAR_RETRY_S;
            }
        }
        im.near_dirty = true;
    }
    im.near_retry -= time.delta_secs();
    if truck.active && im.near_pending.is_none() && im.near_retry <= 0.0 {
        let p = truck.body.pos;
        let here = DVec2::new(origin.0.x + p.x as f64, origin.0.y - p.z as f64);
        if im.near.as_ref().is_none_or(|n| n.0.distance(here) > NEAR_RECENTER_M) {
            im.near_pending = Some(here);
            fetch_near(im.near_tx.clone(), here);
        }
    }
    if !(im.near_dirty || origin.is_changed()) {
        return;
    }
    im.near_dirty = false;
    im.near_ext = match &im.near {
        Some((centre, image)) if truck.active => NearImagery { params: near_params(*centre, origin.0), texture: Some(image.clone()) },
        _ => NearImagery::default(),
    };
    // Materials created later copy `near_ext` (see `run`), so updating the existing ones is enough.
    for (_, m) in mats.iter_mut() {
        m.extension = im.near_ext.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_matches_known_tile() {
        // Standard slippy-map formula, computed independently in Python: zoom 15 tile (5401, 12587).
        let (px, py) = merc_px(15, -120.66, 38.45);
        assert_eq!(((px / 256.0) as u32, (py / 256.0) as u32), (5401, 12587));
    }

    #[test]
    fn uvs_span_the_mosaic() {
        let b = LonLatBox { west: -121.0, south: 38.0, east: -120.0, north: 39.0 };
        let k = MosaicKey::covering(12, &b);
        let nw = k.uv(-121.0, 39.0);
        let se = k.uv(-120.0, 38.0);
        assert!(nw.iter().all(|v| (0.0..=1.0).contains(v)) && se.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(se[0] > nw[0] && se[1] > nw[1], "north is up, east is right");
    }

    #[test]
    fn near_uv_is_exact_in_the_albers_world() {
        let centre = DVec2::new(-2_109_000.0, 1_988_000.0);
        let origin = DVec2::new(-2_110_234.0, 1_988_567.0);
        let p = near_params(centre, origin);
        let uv = |x: f64, y: f64| {
            let (wx, wz) = ((x - origin.x) as f32, (origin.y - y) as f32);
            [p.u.x * wx + p.u.y * wz + p.u.z, p.v.x * wx + p.v.y * wz + p.v.z]
        };
        let nw = uv(centre.x - NEAR_HALF_M, centre.y + NEAR_HALF_M);
        let se = uv(centre.x + NEAR_HALF_M, centre.y - NEAR_HALF_M);
        let c = uv(centre.x, centre.y);
        assert!(nw.iter().all(|v| v.abs() < 1e-4), "{nw:?}");
        assert!(se.iter().all(|v| (v - 1.0).abs() < 1e-4), "{se:?}");
        assert!(c.iter().all(|v| (v - 0.5).abs() < 1e-4), "{c:?}");
    }

    #[test]
    fn mips_go_down_to_one_pixel() {
        let (data, levels) = with_mips(vec![200; 8 * 4 * 4], 8, 4);
        assert_eq!(levels, 4); // 8x4, 4x2, 2x1, 1x1
        assert_eq!(data.len(), (32 + 8 + 2 + 1) * 4);
        assert!(data.iter().all(|&v| v == 200));
    }
}
