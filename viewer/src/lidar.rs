//! 1 m lidar normal maps (prototype, docs/terrain-data-budget.md "Option 1").
//!
//! A lidar tile's mesh is drawn at 10 m (284k triangles instead of 2 M at 3.75 m), and its
//! lighting uses a normal map baked from the 1 m heights served by the `hires` service
//! (`/hires/{tx}_{ty}_1m.vrh`). One map is active at a time: the lidar tile under the truck,
//! else the most recently loaded one. The imagery module carries it in the shared terrain
//! material extension, like the near image.

use crate::imagery::Imagery;
use crate::terrain::{HiresState, Terrain};
use crate::truck::Truck;
use crate::WorldOrigin;
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor, ImageAddressMode, ImageFilterMode};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Mutex;
use vr_fire::grid::TileId;
use vr_fire::lod;

/// Normal maps kept in memory (each ~37 MB with mips at 3751²).
const MAX_MAPS: usize = 3;
const RETRY_S: f32 = 5.0;

enum MapState {
    /// Request in flight, or waiting `f32` seconds before asking again (server still building).
    Fetching,
    Waiting(f32),
    Baking(Task<Result<(Image, f32), String>>),
    /// Ready, with the time it became ready (for evicting the oldest).
    Ready(Handle<Image>, f64),
    Failed,
}

#[derive(Resource)]
pub struct Lidar {
    maps: HashMap<TileId, MapState>,
    tx: Sender<(TileId, u16, Vec<u8>)>,
    rx: Mutex<Receiver<(TileId, u16, Vec<u8>)>>,
    pub bytes: u64,
}

impl Lidar {
    /// Whether any normal map is baked and ready.
    pub fn any_ready(&self) -> bool {
        self.maps.values().any(|st| matches!(st, MapState::Ready(..)))
    }
}

impl Default for Lidar {
    fn default() -> Self {
        let (tx, rx) = channel();
        Lidar { maps: HashMap::new(), tx, rx: Mutex::new(rx), bytes: 0 }
    }
}

fn fetch(tx: Sender<(TileId, u16, Vec<u8>)>, t: TileId) {
    let url = format!("{}/{}", crate::terrain::hires_base(), lod::hires_fine_path(t.tx, t.ty));
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        let (status, bytes) = match res {
            Ok(r) => (r.status, r.bytes),
            Err(_) => (0, Vec::new()),
        };
        let _ = tx.send((t, status, bytes));
    });
}

/// Decode a 1 m `.vrh` and bake its normal map with mips into a GPU image. Also returns the
/// seconds it took (decode + normals + mips), which matters on wasm and a Quest.
fn bake(bytes: Vec<u8>) -> Result<(Image, f32), String> {
    let started = web_time::Instant::now();
    let (side, h) = vr_fire::codec::decode(&bytes).map_err(|e| format!("decode: {e}"))?;
    if side != lod::hires_fine_nodes() + 2 {
        return Err(format!("unexpected 1 m patch side {side}"));
    }
    let w = side - 2;
    let (data, levels) = mips(&normal_map(&h, side, lod::HIRES_FINE_SPACING_M as f32), w);
    let mut image = Image::new(
        Extent3d { width: w as u32, height: w as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data.into_iter().map(|v| v as u8).collect(),
        TextureFormat::Rg8Snorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    Ok((image, started.elapsed().as_secs_f32()))
}

/// Fetch and bake normal maps for lidar tiles, and choose the active one.
pub fn run(
    time: Res<Time>,
    terrain: Res<Terrain>,
    truck: Res<Truck>,
    origin: Res<WorldOrigin>,
    mut lidar: ResMut<Lidar>,
    mut images: ResMut<Assets<Image>>,
    mut im: ResMut<Imagery>,
) {
    let lidar = &mut *lidar;
    let now = time.elapsed_secs_f64();
    // A/B runs: VR_FIRE_LIDAR_NORMALS=off keeps the 10 m lidar mesh but skips the normal map.
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("VR_FIRE_LIDAR_NORMALS").is_ok_and(|v| v == "off") {
        im.set_lidar(None);
        return;
    }
    // New lidar tiles (their 3.75 m patch is built): ask for the 1 m heights.
    for (t, st) in &terrain.hires {
        if matches!(st, HiresState::Ready) && !lidar.maps.contains_key(t) {
            lidar.maps.insert(*t, MapState::Fetching);
            fetch(lidar.tx.clone(), *t);
        }
    }
    let arrived: Vec<_> = lidar.rx.lock().unwrap().try_iter().collect();
    for (t, status, bytes) in arrived {
        let next = match status {
            200 => {
                lidar.bytes += bytes.len() as u64;
                info!("lidar {t}: 1 m heights {:.1} MB, baking normal map", bytes.len() as f64 / 1e6);
                MapState::Baking(AsyncComputeTaskPool::get().spawn(async move { bake(bytes) }))
            }
            202 | 503 => MapState::Waiting(RETRY_S),
            404 => MapState::Failed,
            _ => MapState::Waiting(RETRY_S * 2.0),
        };
        lidar.maps.insert(t, next);
    }
    let dt = time.delta_secs();
    let mut refetch = Vec::new();
    for (t, st) in lidar.maps.iter_mut() {
        match st {
            MapState::Waiting(left) => {
                *left -= dt;
                if *left <= 0.0 {
                    refetch.push(*t);
                }
            }
            MapState::Baking(task) => {
                if let Some(result) = check_ready(task) {
                    *st = match result {
                        Ok((image, secs)) => {
                            info!("lidar {t}: normal map ready ({secs:.2} s to decode and bake)");
                            MapState::Ready(images.add(image), now)
                        }
                        Err(e) => {
                            warn!("lidar {t}: {e}");
                            MapState::Failed
                        }
                    };
                }
            }
            _ => {}
        }
    }
    for t in refetch {
        lidar.maps.insert(t, MapState::Fetching);
        fetch(lidar.tx.clone(), t);
    }
    // Keep the newest maps only.
    let mut ready: Vec<(TileId, f64)> =
        lidar.maps.iter().filter_map(|(t, st)| if let MapState::Ready(_, at) = st { Some((*t, *at)) } else { None }).collect();
    ready.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (t, _) in ready.iter().skip(MAX_MAPS) {
        if let Some(MapState::Ready(h, _)) = lidar.maps.remove(t) {
            images.remove(&h);
        }
    }
    // Active map: the tile under the truck if it has one, else the newest.
    let under = truck.active.then(|| terrain.grid.tile_containing(origin.0.x + truck.body.pos.x as f64, origin.0.y - truck.body.pos.z as f64));
    let pick = under.filter(|t| matches!(lidar.maps.get(t), Some(MapState::Ready(..)))).or_else(|| ready.first().map(|r| r.0));
    let active = pick.and_then(|t| match lidar.maps.get(&t) {
        Some(MapState::Ready(h, _)) => Some((t, h.clone())),
        _ => None,
    });
    im.set_lidar(active);
}

/// World-space normal map from a ringed height grid (`side`² nodes including a one-node ring,
/// `spacing` metres apart; rows run south, columns east). One texel per interior node, two
/// signed 8-bit channels: the normal's east (x) and south (z) components; the up component is
/// rebuilt in the shader.
pub fn normal_map(heights: &[f32], side: usize, spacing: f32) -> Vec<i8> {
    let n = side - 2;
    let h = |i: usize, j: usize| heights[j * side + i];
    let mut out = Vec::with_capacity(n * n * 2);
    for j in 1..=n {
        for i in 1..=n {
            let dx = (h(i + 1, j) - h(i - 1, j)) / (2.0 * spacing); // rise per metre east
            let dz = (h(i, j + 1) - h(i, j - 1)) / (2.0 * spacing); // rise per metre south
            push_normal(&mut out, Vec3::new(-dx, 1.0, -dz).normalize());
        }
    }
    out
}

fn push_normal(out: &mut Vec<i8>, n: Vec3) {
    out.push((n.x * 127.0).round().clamp(-127.0, 127.0) as i8);
    out.push((n.z * 127.0).round().clamp(-127.0, 127.0) as i8);
}

fn texel(t: &[i8], k: usize) -> Vec3 {
    let (x, z) = (t[2 * k] as f32 / 127.0, t[2 * k + 1] as f32 / 127.0);
    Vec3::new(x, (1.0 - x * x - z * z).max(0.0).sqrt(), z)
}

/// Full mip chain for a square two-channel signed normal map (base first). Each level averages
/// 2×2 normals (rebuilding y) and renormalises. Returns the bytes and the level count.
pub fn mips(base: &[i8], w: usize) -> (Vec<i8>, u32) {
    let mut all = base.to_vec();
    let (mut prev, mut pw, mut levels) = (base.to_vec(), w, 1u32);
    while pw > 1 {
        let nw = pw / 2;
        let mut next = Vec::with_capacity(nw * nw * 2);
        for y in 0..nw {
            for x in 0..nw {
                let mut sum = Vec3::ZERO;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((2 * x + dx).min(pw - 1), (2 * y + dy).min(pw - 1));
                    sum += texel(&prev, sy * pw + sx);
                }
                push_normal(&mut next, sum.normalize_or(Vec3::Y));
            }
        }
        all.extend_from_slice(&next);
        prev = next;
        pw = nw;
        levels += 1;
    }
    (all, levels)
}

/// Affine world X/Z → texture UV for a lidar tile whose NW corner is (`x_min`, `y_max`) in
/// EPSG:5070, `size` metres across, sampled with one texel per node (`texels` per side): node
/// centres land on texel centres. Same form as the near image: uv = (u·(x, z, 1), v·(x, z, 1)),
/// u.w = 1 (enabled).
pub fn lidar_params(x_min: f64, y_max: f64, size: f64, texels: usize, origin: bevy::math::DVec2) -> (Vec4, Vec4) {
    let s = (texels as f64 - 1.0) / texels as f64 / size;
    let half = 0.5 / texels as f64;
    // World x = east − origin.x, world z = origin.y − north (south is +z).
    let u = Vec4::new(s as f32, 0.0, ((origin.x - x_min) * s + half) as f32, 1.0);
    let v = Vec4::new(0.0, s as f32, ((y_max - origin.y) * s + half) as f32, 0.0);
    (u, v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::DVec2;

    fn ringed(side: usize, f: impl Fn(f32, f32) -> f32, spacing: f32) -> Vec<f32> {
        (0..side * side).map(|k| f(((k % side) as f32 - 1.0) * spacing, ((k / side) as f32 - 1.0) * spacing)).collect()
    }

    fn decode(t: &[i8], k: usize) -> Vec3 {
        let (x, z) = (t[2 * k] as f32 / 127.0, t[2 * k + 1] as f32 / 127.0);
        Vec3::new(x, (1.0 - x * x - z * z).max(0.0).sqrt(), z)
    }

    #[test]
    fn flat_ground_points_straight_up() {
        let t = normal_map(&ringed(7, |_, _| 12.0, 1.0), 7, 1.0);
        assert_eq!(t.len(), 5 * 5 * 2);
        assert!(t.iter().all(|&v| v == 0));
    }

    #[test]
    fn slopes_tilt_the_normal_downhill() {
        // Rising 0.5 m per metre east: normal ∝ (−0.5, 1, 0).
        let t = normal_map(&ringed(7, |e, _| 0.5 * e, 1.0), 7, 1.0);
        let n = decode(&t, 12);
        let want = Vec3::new(-0.5, 1.0, 0.0).normalize();
        assert!((n - want).length() < 0.02, "{n} vs {want}");
        // Rising 0.5 m per metre south: normal ∝ (0, 1, −0.5) (z is south in the world).
        let t = normal_map(&ringed(7, |_, s| 0.5 * s, 2.0), 7, 2.0);
        let n = decode(&t, 12);
        let want = Vec3::new(0.0, 1.0, -0.5).normalize();
        assert!((n - want).length() < 0.02, "{n} vs {want}");
    }

    #[test]
    fn mip_chain_goes_down_to_one_texel_and_keeps_the_average_direction() {
        let t = normal_map(&ringed(7, |e, _| 0.5 * e, 1.0), 7, 1.0); // 5×5, uniform slope
        let (all, levels) = mips(&t, 5);
        assert_eq!(levels, 3); // 5 → 2 → 1
        assert_eq!(all.len(), 2 * (25 + 4 + 1));
        let last = decode(&all[all.len() - 2..], 0);
        assert!((last - Vec3::new(-0.5, 1.0, 0.0).normalize()).length() < 0.03);
    }

    #[test]
    fn node_centres_land_on_texel_centres() {
        let origin = DVec2::new(-2_000_000.0, 1_900_000.0);
        let (x_min, y_max, size, texels) = (-2_001_000.0, 1_901_000.0, 3750.0, 3751);
        let (u, v) = lidar_params(x_min, y_max, size, texels, origin);
        let uv = |east: f64, north: f64| {
            let (x, z) = ((east - origin.x) as f32, (origin.y - north) as f32);
            (u.x * x + u.y * z + u.z, v.x * x + v.y * z + v.z)
        };
        let half = 0.5 / texels as f32;
        let (a, b) = uv(x_min, y_max); // NW node → centre of texel (0, 0)
        assert!((a - half).abs() < 1e-5 && (b - half).abs() < 1e-5, "{a} {b}");
        let (a, b) = uv(x_min + size, y_max - size); // SE node → centre of the last texel
        assert!((a - (1.0 - half)).abs() < 1e-5 && (b - (1.0 - half)).abs() < 1e-5, "{a} {b}");
        assert_eq!(u.w, 1.0);
    }
}
