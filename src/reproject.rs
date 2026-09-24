//! Resample 3DEP source rasters (NAD83 lon/lat) onto a tile's EPSG:5070 node grid.

use crate::crs::Albers;
use crate::dem::Raster;
use crate::grid::{GridSpec, NODES_PER_SIDE, TileId};
use crate::source::SourceCell;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Height given to nodes with no source data (3DEP is void-filled on land, so this is ocean).
pub const SEA_LEVEL_M: f32 = 0.0;
/// Margin around a tile's lon/lat box when choosing source cells (~200 m).
pub const SOURCE_MARGIN_DEG: f64 = 0.002;

/// Loaded source rasters by cell. Cells absent from the map have no data (ocean).
#[derive(Default)]
pub struct Sources {
    rasters: HashMap<SourceCell, Arc<Raster>>,
}

impl Sources {
    pub fn insert(&mut self, c: SourceCell, r: Arc<Raster>) {
        self.rasters.insert(c, r);
    }
    pub fn remove(&mut self, c: SourceCell) {
        self.rasters.remove(&c);
    }
    pub fn contains(&self, c: SourceCell) -> bool {
        self.rasters.contains_key(&c)
    }
    pub fn cells(&self) -> Vec<SourceCell> {
        self.rasters.keys().copied().collect()
    }
    pub fn len(&self) -> usize {
        self.rasters.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rasters.is_empty()
    }

    /// Always samples the raster of the cell containing the point, so a given coordinate
    /// maps to the same value no matter which tile asks (keeps shared edges identical).
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f32> {
        self.rasters.get(&SourceCell::containing(lon, lat))?.sample_bilinear(lon, lat)
    }
}

pub struct TileHeights {
    pub tile: TileId,
    /// 376 × 376 heights (m, NAVD88), row-major, north row first.
    pub data: Vec<f32>,
    /// Nodes set to sea level because no source had data.
    pub filled: u32,
}

pub fn source_cells_for_tile(grid: &GridSpec, albers: &Albers, tile: TileId) -> Result<Vec<SourceCell>> {
    let ll = albers.lonlat_bounds(&grid.tile_bounds(tile))?.padded(SOURCE_MARGIN_DEG);
    Ok(SourceCell::covering(&ll))
}

/// Resample one tile. Returns None when no node has source data (tile is empty).
pub fn resample_tile(grid: &GridSpec, albers: &Albers, sources: &Sources, tile: TileId) -> Result<Option<TileHeights>> {
    let n = NODES_PER_SIDE;
    let mut data = Vec::with_capacity(n * n);
    let mut filled = 0u32;
    for j in 0..n {
        for i in 0..n {
            let (x, y) = grid.node_xy(tile, i, j);
            let (lon, lat) = albers.to_lonlat(x, y)?;
            match sources.sample(lon, lat) {
                Some(h) => data.push(h),
                None => {
                    data.push(SEA_LEVEL_M);
                    filled += 1;
                }
            }
        }
    }
    if filled as usize == n * n {
        return Ok(None);
    }
    Ok(Some(TileHeights { tile, data, filled }))
}

pub fn min_max(heights: &[f32]) -> (f32, f32) {
    heights.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &h| (lo.min(h), hi.max(h)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem::EPSG_NAD83_GEOGRAPHIC;

    const PX: f64 = 0.01;

    fn plane(lon: f64, lat: f64) -> f64 {
        1000.0 + 100.0 * (lon + 121.0) + 50.0 * (lat - 38.0)
    }

    /// Like a real 3DEP file: the 1° cell plus a one-pixel overlap buffer on every side.
    fn plane_raster(cell: SourceCell) -> Arc<Raster> {
        let n = (1.0 / PX).round() as usize + 2;
        let origin_x = -(cell.west as f64) - PX;
        let origin_y = cell.north as f64 + PX;
        let mut data = Vec::with_capacity(n * n);
        for r in 0..n {
            for c in 0..n {
                data.push(plane(origin_x + (c as f64 + 0.5) * PX, origin_y - (r as f64 + 0.5) * PX) as f32);
            }
        }
        Arc::new(Raster { width: n, height: n, data, origin_x, origin_y, pixel_w: PX, pixel_h: PX, epsg: EPSG_NAD83_GEOGRAPHIC, nodata: None })
    }

    fn tile_at(lon: f64, lat: f64) -> (GridSpec, Albers, TileId) {
        let grid = GridSpec::default();
        let albers = Albers::new().unwrap();
        let (x, y) = albers.from_lonlat(lon, lat).unwrap();
        let t = grid.tile_containing(x, y);
        (grid, albers, t)
    }

    fn assert_matches_plane(grid: &GridSpec, albers: &Albers, h: &TileHeights) {
        for j in (0..NODES_PER_SIDE).step_by(25) {
            for i in (0..NODES_PER_SIDE).step_by(25) {
                let (x, y) = grid.node_xy(h.tile, i, j);
                let (lon, lat) = albers.to_lonlat(x, y).unwrap();
                let got = h.data[j * NODES_PER_SIDE + i];
                assert!((got as f64 - plane(lon, lat)).abs() < 1e-2, "node ({i},{j}): {got} vs {}", plane(lon, lat));
            }
        }
    }

    #[test]
    fn resamples_a_plane_exactly() {
        let (grid, albers, t) = tile_at(-120.5, 38.5);
        let mut s = Sources::default();
        let cell = SourceCell { north: 39, west: 121 };
        s.insert(cell, plane_raster(cell));
        let h = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        assert_eq!(h.data.len(), NODES_PER_SIDE * NODES_PER_SIDE);
        assert_eq!(h.filled, 0);
        assert_matches_plane(&grid, &albers, &h);
    }

    #[test]
    fn straddling_tile_is_seamless() {
        let (grid, albers, t) = tile_at(-120.0, 38.5);
        let cells = source_cells_for_tile(&grid, &albers, t).unwrap();
        assert!(cells.contains(&SourceCell { north: 39, west: 121 }) && cells.contains(&SourceCell { north: 39, west: 120 }));
        let mut s = Sources::default();
        for c in cells {
            s.insert(c, plane_raster(c));
        }
        let h = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        assert_eq!(h.filled, 0);
        assert_matches_plane(&grid, &albers, &h);
    }

    #[test]
    fn partially_covered_tile_fills_sea_level() {
        let (grid, albers, t) = tile_at(-120.0, 38.5);
        let mut s = Sources::default();
        let west = SourceCell { north: 39, west: 121 };
        s.insert(west, plane_raster(west)); // the eastern cell is "ocean"
        let h = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        assert!(h.filled > 0 && (h.filled as usize) < NODES_PER_SIDE * NODES_PER_SIDE);
        assert!(h.data.contains(&SEA_LEVEL_M));
    }

    #[test]
    fn tile_without_sources_is_empty() {
        let (grid, albers, t) = tile_at(-120.5, 38.5);
        assert!(resample_tile(&grid, &albers, &Sources::default(), t).unwrap().is_none());
    }

    #[test]
    fn adjacent_tiles_have_bit_identical_edges() {
        let (grid, albers, t) = tile_at(-120.5, 38.5);
        let mut s = Sources::default();
        let cell = SourceCell { north: 39, west: 121 };
        s.insert(cell, plane_raster(cell));
        let a = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        let b = resample_tile(&grid, &albers, &s, t.east()).unwrap().unwrap();
        let n = NODES_PER_SIDE;
        for j in 0..n {
            assert_eq!(a.data[j * n + n - 1].to_bits(), b.data[j * n].to_bits(), "row {j}");
        }
    }

    #[test]
    fn min_max_of_heights() {
        assert_eq!(min_max(&[3.0, -1.5, 7.25]), (-1.5, 7.25));
    }
}
