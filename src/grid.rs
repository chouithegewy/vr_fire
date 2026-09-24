//! Fixed EPSG:5070 tile grid aligned to the NLCD/LANDFIRE 30 m grid.
//!
//! Tile (tx, ty) spans 3,750 m; tx grows east, ty grows south. Heights are sampled at
//! 376 × 376 nodes every 10 m, so adjacent tiles share (bit-identical) edge nodes and
//! every 30 m ML cell corner is a node.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const TILE_SIZE_M: f64 = 3750.0;
pub const NODE_SPACING_M: f64 = 10.0;
pub const NODES_PER_SIDE: usize = 376;
pub const CELL_SIZE_M: f64 = 30.0;
pub const CELLS_PER_TILE: i64 = 125;
/// 375 m ML windows per tile side, anchored at the tile origin (see spec open question 1).
pub const WINDOWS_PER_TILE: i64 = 10;
/// Node stride of each LOD over the 10 m grid: 10, 30, 150, 750 m.
pub const LOD_STRIDES: [usize; 4] = [1, 3, 15, 75];

/// NW corner of the NLCD CONUS raster; 30 m cell edges fall at 15 mod 30.
pub const NLCD_ORIGIN_X: f64 = -2_493_045.0;
pub const NLCD_ORIGIN_Y: f64 = 3_310_005.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridSpec {
    pub origin_x: f64,
    pub origin_y: f64,
}

impl Default for GridSpec {
    fn default() -> Self {
        Self { origin_x: NLCD_ORIGIN_X, origin_y: NLCD_ORIGIN_Y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileId {
    pub tx: i64,
    pub ty: i64,
}

impl TileId {
    pub fn new(tx: i64, ty: i64) -> Self {
        Self { tx, ty }
    }
    pub fn north(self) -> Self {
        Self::new(self.tx, self.ty - 1)
    }
    pub fn south(self) -> Self {
        Self::new(self.tx, self.ty + 1)
    }
    pub fn east(self) -> Self {
        Self::new(self.tx + 1, self.ty)
    }
    pub fn west(self) -> Self {
        Self::new(self.tx - 1, self.ty)
    }
}

impl fmt::Display for TileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.tx, self.ty)
    }
}

impl FromStr for TileId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (a, b) = s.split_once('_').ok_or_else(|| format!("expected 'tx_ty', got '{s}'"))?;
        let tx = a.parse().map_err(|_| format!("bad tx in '{s}'"))?;
        let ty = b.parse().map_err(|_| format!("bad ty in '{s}'"))?;
        Ok(Self::new(tx, ty))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
}

impl GridSpec {
    pub fn tile_bounds(&self, t: TileId) -> Bounds {
        let x_min = self.origin_x + TILE_SIZE_M * t.tx as f64;
        let y_max = self.origin_y - TILE_SIZE_M * t.ty as f64;
        Bounds { x_min, y_min: y_max - TILE_SIZE_M, x_max: x_min + TILE_SIZE_M, y_max }
    }

    /// EPSG:5070 coordinate of node (i, j) of a tile; i grows east, j grows south.
    pub fn node_xy(&self, t: TileId, i: usize, j: usize) -> (f64, f64) {
        let b = self.tile_bounds(t);
        (b.x_min + NODE_SPACING_M * i as f64, b.y_max - NODE_SPACING_M * j as f64)
    }

    pub fn tile_containing(&self, x: f64, y: f64) -> TileId {
        TileId::new(
            ((x - self.origin_x) / TILE_SIZE_M).floor() as i64,
            ((self.origin_y - y) / TILE_SIZE_M).floor() as i64,
        )
    }

    pub fn tiles_intersecting(&self, b: &Bounds) -> Vec<TileId> {
        let nw = self.tile_containing(b.x_min, b.y_max);
        let se = self.tile_containing(b.x_max, b.y_min);
        let mut out = Vec::new();
        for ty in nw.ty..=se.ty {
            for tx in nw.tx..=se.tx {
                out.push(TileId::new(tx, ty));
            }
        }
        out
    }

    /// Global 30 m ML cell (column east, row south, counted from the grid origin).
    pub fn cell_containing(&self, x: f64, y: f64) -> (i64, i64) {
        (
            ((x - self.origin_x) / CELL_SIZE_M).floor() as i64,
            ((self.origin_y - y) / CELL_SIZE_M).floor() as i64,
        )
    }
}

/// Global ML cell → (tile, cell within tile).
pub fn cell_to_tile(c: i64, r: i64) -> (TileId, (u32, u32)) {
    (
        TileId::new(c.div_euclid(CELLS_PER_TILE), r.div_euclid(CELLS_PER_TILE)),
        (c.rem_euclid(CELLS_PER_TILE) as u32, r.rem_euclid(CELLS_PER_TILE) as u32),
    )
}

/// Inclusive rectangle of tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRange {
    pub min: TileId,
    pub max: TileId,
}

impl TileRange {
    pub fn contains(&self, t: TileId) -> bool {
        (self.min.tx..=self.max.tx).contains(&t.tx) && (self.min.ty..=self.max.ty).contains(&t.ty)
    }

    pub fn tiles(&self) -> Vec<TileId> {
        let mut out = Vec::new();
        for ty in self.min.ty..=self.max.ty {
            for tx in self.min.tx..=self.max.tx {
                out.push(TileId::new(tx, ty));
            }
        }
        out
    }
}

impl FromStr for TileRange {
    type Err = String;
    /// "tx,ty" or "tx0,ty0..tx1,ty1" (corners in any order).
    fn from_str(s: &str) -> Result<Self, String> {
        fn corner(s: &str) -> Result<TileId, String> {
            let (a, b) = s.split_once(',').ok_or_else(|| format!("expected 'tx,ty', got '{s}'"))?;
            let tx = a.trim().parse().map_err(|_| format!("bad tx in '{s}'"))?;
            let ty = b.trim().parse().map_err(|_| format!("bad ty in '{s}'"))?;
            Ok(TileId::new(tx, ty))
        }
        let (a, b) = match s.split_once("..") {
            Some((a, b)) => (corner(a)?, corner(b)?),
            None => {
                let t = corner(s)?;
                (t, t)
            }
        };
        Ok(Self {
            min: TileId::new(a.tx.min(b.tx), a.ty.min(b.ty)),
            max: TileId::new(a.tx.max(b.tx), a.ty.max(b.ty)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_tile_bounds() {
        let g = GridSpec::default();
        let b = g.tile_bounds(TileId::new(0, 0));
        assert_eq!(b.x_min, -2_493_045.0);
        assert_eq!(b.y_max, 3_310_005.0);
        assert_eq!(b.x_max - b.x_min, TILE_SIZE_M);
        assert_eq!(b.y_max - b.y_min, TILE_SIZE_M);
    }

    #[test]
    fn adjacent_tiles_share_edges_exactly() {
        let g = GridSpec::default();
        let t = TileId::new(812, 1440);
        assert_eq!(g.tile_bounds(t).x_max.to_bits(), g.tile_bounds(t.east()).x_min.to_bits());
        assert_eq!(g.tile_bounds(t).y_min.to_bits(), g.tile_bounds(t.south()).y_max.to_bits());
        for k in 0..NODES_PER_SIDE {
            let (ax, ay) = g.node_xy(t, NODES_PER_SIDE - 1, k);
            let (bx, by) = g.node_xy(t.east(), 0, k);
            assert_eq!((ax.to_bits(), ay.to_bits()), (bx.to_bits(), by.to_bits()));
            let (ax, ay) = g.node_xy(t, k, NODES_PER_SIDE - 1);
            let (bx, by) = g.node_xy(t.south(), k, 0);
            assert_eq!((ax.to_bits(), ay.to_bits()), (bx.to_bits(), by.to_bits()));
        }
    }

    #[test]
    fn every_third_node_is_an_nlcd_cell_corner() {
        // NLCD 30 m cell edges sit at 15 mod 30 in EPSG:5070.
        let g = GridSpec::default();
        let t = TileId::new(812, 1440);
        for k in (0..NODES_PER_SIDE).step_by(3) {
            let (x, y) = g.node_xy(t, k, k);
            assert_eq!(x.rem_euclid(CELL_SIZE_M), 15.0);
            assert_eq!(y.rem_euclid(CELL_SIZE_M), 15.0);
        }
    }

    #[test]
    fn tile_containing_round_trips_including_negative_ids() {
        let g = GridSpec::default();
        for t in [TileId::new(-3, -2), TileId::new(0, 0), TileId::new(812, 1440)] {
            let b = g.tile_bounds(t);
            assert_eq!(g.tile_containing((b.x_min + b.x_max) / 2.0, (b.y_min + b.y_max) / 2.0), t);
            assert_eq!(g.tile_containing(b.x_min, b.y_max), t, "NW corner belongs to the tile");
            let inner = Bounds { x_min: b.x_min + 1.0, y_min: b.y_min + 1.0, x_max: b.x_max - 1.0, y_max: b.y_max - 1.0 };
            assert_eq!(g.tiles_intersecting(&inner), vec![t]);
        }
    }

    #[test]
    fn tiles_intersecting_spans_rows_and_columns() {
        let g = GridSpec::default();
        let a = g.tile_bounds(TileId::new(10, 20));
        let z = g.tile_bounds(TileId::new(12, 21));
        let span = Bounds { x_min: a.x_min + 1.0, y_min: z.y_min + 1.0, x_max: z.x_max - 1.0, y_max: a.y_max - 1.0 };
        assert_eq!(g.tiles_intersecting(&span).len(), 3 * 2);
    }

    #[test]
    fn cells_map_to_tiles_with_euclidean_division() {
        assert_eq!(cell_to_tile(0, 0), (TileId::new(0, 0), (0, 0)));
        assert_eq!(cell_to_tile(125, 250), (TileId::new(1, 2), (0, 0)));
        assert_eq!(cell_to_tile(-1, 124), (TileId::new(-1, 0), (124, 124)));
        let g = GridSpec::default();
        let b = g.tile_bounds(TileId::new(1, 2));
        assert_eq!(g.cell_containing(b.x_min + 1.0, b.y_max - 1.0), (125, 250));
    }

    #[test]
    fn tile_id_display_and_parse() {
        let t = TileId::new(-3, 1440);
        assert_eq!(t.to_string(), "-3_1440");
        assert_eq!("-3_1440".parse::<TileId>().unwrap(), t);
        assert!("3,4".parse::<TileId>().is_err());
    }

    #[test]
    fn tile_range_parsing() {
        let r: TileRange = "812,1440..815,1443".parse().unwrap();
        assert_eq!(r.tiles().len(), 16);
        assert!(r.contains(TileId::new(813, 1441)));
        assert!(!r.contains(TileId::new(816, 1441)));
        let single: TileRange = "3,4".parse().unwrap();
        assert_eq!(single.tiles(), vec![TileId::new(3, 4)]);
        let reversed: TileRange = "5,6..3,4".parse().unwrap();
        assert_eq!(reversed.min, TileId::new(3, 4));
        assert!("garbage".parse::<TileRange>().is_err());
    }
}
