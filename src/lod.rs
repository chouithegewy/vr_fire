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
