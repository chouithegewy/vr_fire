//! Level-of-detail layout for streamed/packed viewer patches, shared by `vr_fire pack` and
//! the viewer. Every spacing is a multiple of the 10 m node grid and divides the 3,750 m
//! tile, so each level is an exact subset of the 10 m store nodes.

use crate::grid::{NODE_SPACING_M, TILE_SIZE_M};

/// Node strides over the 10 m grid for tile levels 0..: 10, 30, 50, 150, 250 m.
/// Ratios of 1.7–3× between levels keep zooming smooth.
pub const TILE_STRIDES: [i64; 5] = [1, 3, 5, 15, 25];
/// Grid tiles per super-tile side (37.5 km super tiles).
pub const SUPER_TILES: i64 = 10;
/// Super-tile node stride: 750 m.
pub const SUPER_STRIDE: i64 = 75;
/// Quantization step (m) per tile level; max error is half of it.
pub const TILE_STEPS: [f32; 5] = [0.25, 0.25, 0.5, 0.5, 0.5];
pub const SUPER_STEP: f32 = 1.0;
/// Height used for "no data" (ocean): far below the viewer's water plane, and below
/// Death Valley (−86 m) so dry basins never flood.
pub const OCEAN_M: f32 = -300.0;

/// Nodes per side for a tile level (without the normal ring).
pub fn tile_nodes(lod: usize) -> usize {
    (TILE_SIZE_M / NODE_SPACING_M) as usize / TILE_STRIDES[lod] as usize + 1
}

pub fn tile_spacing(lod: usize) -> f64 {
    NODE_SPACING_M * TILE_STRIDES[lod] as f64
}

pub fn super_nodes() -> usize {
    (TILE_SIZE_M * SUPER_TILES as f64 / NODE_SPACING_M) as usize / SUPER_STRIDE as usize + 1
}

/// On-demand lidar patches (`hires` service): one grid tile at 3.75 m, from USGS 1 m lidar.
pub const HIRES_SPACING_M: f64 = 3.75;
pub const HIRES_STEP: f32 = 0.1;

pub fn hires_nodes() -> usize {
    (TILE_SIZE_M / HIRES_SPACING_M) as usize + 1
}

pub fn hires_path(tx: i64, ty: i64) -> String {
    format!("{tx}_{ty}.vrh")
}

/// The same lidar tile at 1 m (for normal maps and, later, height textures).
pub const HIRES_FINE_SPACING_M: f64 = 1.0;

pub fn hires_fine_nodes() -> usize {
    (TILE_SIZE_M / HIRES_FINE_SPACING_M) as usize + 1
}

pub fn hires_fine_path(tx: i64, ty: i64) -> String {
    format!("{tx}_{ty}_1m.vrh")
}

/// Bilinearly resample a ringed height grid (`nodes`² tile nodes `spacing` apart plus a
/// one-node ring) to another spacing over the same tile. Returns `(dst_nodes + 2)²` values.
pub fn resample_ringed(src: &[f32], src_nodes: usize, src_spacing: f64, dst_nodes: usize, dst_spacing: f64) -> Vec<f32> {
    let (sm, dm) = (src_nodes + 2, dst_nodes + 2);
    let last = (sm - 1) as f64;
    let at = |i: usize, j: usize| src[j * sm + i];
    let mut out = Vec::with_capacity(dm * dm);
    for j in 0..dm {
        for i in 0..dm {
            // Metres from the NW corner → source index (both grids carry a one-node ring).
            let u = (((i as f64 - 1.0) * dst_spacing) / src_spacing + 1.0).clamp(0.0, last);
            let w = (((j as f64 - 1.0) * dst_spacing) / src_spacing + 1.0).clamp(0.0, last);
            let (i0, j0) = ((u.floor() as usize).min(sm - 2), (w.floor() as usize).min(sm - 2));
            let (fu, fw) = ((u - i0 as f64) as f32, (w - j0 as f64) as f32);
            let top = at(i0, j0) * (1.0 - fu) + at(i0 + 1, j0) * fu;
            let bot = at(i0, j0 + 1) * (1.0 - fu) + at(i0 + 1, j0 + 1) * fu;
            out.push(top * (1.0 - fw) + bot * fw);
        }
    }
    out
}

/// Packed file path relative to the tile root, e.g. `t0/102_352.vrh`, `s/10_35.vrh`.
pub fn tile_path(lod: usize, tx: i64, ty: i64) -> String {
    format!("t{lod}/{tx}_{ty}.vrh")
}

pub fn super_path(sx: i64, sy: i64) -> String {
    format!("s/{sx}_{sy}.vrh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_a_ringed_plane_is_exact() {
        // h = 3 + 0.5·east − 0.25·south (metres from the tile's NW corner), incl. the ring.
        let plane = |e: f64, s: f64| (3.0 + 0.5 * e - 0.25 * s) as f32;
        let (sn, ss) = (11usize, 3.75);
        let sm = sn + 2;
        let src: Vec<f32> = (0..sm * sm).map(|k| plane(((k % sm) as f64 - 1.0) * ss, ((k / sm) as f64 - 1.0) * ss)).collect();
        let (dn, ds) = (5usize, 10.0);
        let dst = resample_ringed(&src, sn, ss, dn, ds);
        let dm = dn + 2;
        assert_eq!(dst.len(), dm * dm);
        for j in 0..dm {
            for i in 0..dm {
                let want = plane((i as f64 - 1.0) * ds, (j as f64 - 1.0) * ds);
                // The ring past the source's extent is clamped; check only covered nodes.
                if (i as f64 - 1.0) * ds <= (sn as f64) * ss && (j as f64 - 1.0) * ds <= (sn as f64) * ss && i > 0 && j > 0 {
                    assert!((dst[j * dm + i] - want).abs() < 1e-3, "node ({i},{j}): {} vs {want}", dst[j * dm + i]);
                }
            }
        }
    }

    #[test]
    fn levels_are_exact_subsets_of_the_10m_grid() {
        assert_eq!((0..5).map(tile_nodes).collect::<Vec<_>>(), vec![376, 126, 76, 26, 16]);
        assert_eq!((0..5).map(tile_spacing).collect::<Vec<_>>(), vec![10.0, 30.0, 50.0, 150.0, 250.0]);
        for s in TILE_STRIDES {
            assert_eq!(375 % s, 0);
        }
        assert_eq!(super_nodes(), 51);
        assert_eq!(tile_path(2, -3, 7), "t2/-3_7.vrh");
        assert_eq!(hires_nodes(), 1001);
    }
}
