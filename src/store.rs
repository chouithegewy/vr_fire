//! On-disk store of 10 m height tiles: `{tx}_{ty}.tif` (f32, EPSG:5070, PixelIsPoint,
//! Deflate — opens directly in QGIS) plus `index.json` recording every tile's status.

use crate::dem::{EPSG_CONUS_ALBERS, Raster};
use crate::grid::{GridSpec, NODE_SPACING_M, NODES_PER_SIDE, TileId};
use crate::reproject::TileHeights;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileStatus {
    Ok,
    Empty,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileEntry {
    pub status: TileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub min_elevation_m: Option<f32>,
    pub max_elevation_m: Option<f32>,
    pub filled_samples: u32,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoreIndex {
    pub grid: GridSpec,
    /// Keyed by `"{tx}_{ty}"`.
    pub tiles: BTreeMap<String, TileEntry>,
}

impl StoreIndex {
    pub fn get(&self, t: TileId) -> Option<&TileEntry> {
        self.tiles.get(&t.to_string())
    }
    pub fn set(&mut self, t: TileId, e: TileEntry) {
        self.tiles.insert(t.to_string(), e);
    }
    pub fn tile_ids(&self, status: TileStatus) -> Vec<TileId> {
        self.tiles.iter().filter(|(_, e)| e.status == status).filter_map(|(k, _)| k.parse().ok()).collect()
    }
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn tile_path(&self, t: TileId) -> PathBuf {
        self.root.join(format!("{t}.tif"))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    pub fn load_index(&self) -> Result<StoreIndex> {
        let p = self.index_path();
        if !p.exists() {
            return Ok(StoreIndex::default());
        }
        let s = fs::read_to_string(&p)?;
        serde_json::from_str(&s).with_context(|| format!("parse {}", p.display()))
    }

    /// Written to a temp file then renamed, so an interrupted ingest never leaves a torn index.
    pub fn save_index(&self, idx: &StoreIndex) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let tmp = self.root.join("index.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(idx)?)?;
        fs::rename(tmp, self.index_path())?;
        Ok(())
    }

    pub fn write_tile(&self, grid: &GridSpec, h: &TileHeights) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let b = grid.tile_bounds(h.tile);
        let raster = Raster {
            width: NODES_PER_SIDE,
            height: NODES_PER_SIDE,
            data: h.data.clone(),
            origin_x: b.x_min - NODE_SPACING_M / 2.0,
            origin_y: b.y_max + NODE_SPACING_M / 2.0,
            pixel_w: NODE_SPACING_M,
            pixel_h: NODE_SPACING_M,
            epsg: EPSG_CONUS_ALBERS,
            nodata: None,
        };
        let path = self.tile_path(h.tile);
        let tmp = path.with_extension("tif.tmp");
        raster.write_geotiff(&tmp, true)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    /// Heights of a stored tile, or None if the tile has no file.
    pub fn read_tile(&self, grid: &GridSpec, t: TileId) -> Result<Option<Vec<f32>>> {
        let path = self.tile_path(t);
        if !path.exists() {
            return Ok(None);
        }
        let r = Raster::read_geotiff(&path)?;
        let b = grid.tile_bounds(t);
        ensure!(
            r.width == NODES_PER_SIDE && r.height == NODES_PER_SIDE,
            "{}: expected {NODES_PER_SIDE}² nodes, got {}×{}",
            path.display(),
            r.width,
            r.height
        );
        ensure!(
            r.epsg == EPSG_CONUS_ALBERS
                && (r.origin_x + r.pixel_w / 2.0 - b.x_min).abs() < 1e-6
                && (r.origin_y - r.pixel_h / 2.0 - b.y_max).abs() < 1e-6,
            "{}: georeference does not match tile {t}",
            path.display()
        );
        Ok(Some(r.data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heights(tile: TileId) -> TileHeights {
        let n = NODES_PER_SIDE;
        TileHeights { tile, data: (0..n * n).map(|k| (k % 997) as f32 * 0.37 - 20.0).collect(), filled: 0 }
    }

    #[test]
    fn tile_round_trips_bit_identically() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let grid = GridSpec::default();
        let h = heights(TileId::new(812, 1440));
        store.write_tile(&grid, &h).unwrap();
        let back = store.read_tile(&grid, h.tile).unwrap().unwrap();
        assert!(back.iter().zip(&h.data).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn store_tiles_are_georeferenced_node_grids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let grid = GridSpec::default();
        let t = TileId::new(812, 1440);
        store.write_tile(&grid, &heights(t)).unwrap();
        let r = Raster::read_geotiff(&store.tile_path(t)).unwrap();
        let b = grid.tile_bounds(t);
        assert_eq!(r.epsg, EPSG_CONUS_ALBERS);
        // Pixel (0,0) is centered on the tile's NW corner node.
        assert_eq!(r.origin_x + r.pixel_w / 2.0, b.x_min);
        assert_eq!(r.origin_y - r.pixel_h / 2.0, b.y_max);
    }

    #[test]
    fn mismatched_georeference_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let grid = GridSpec::default();
        let t = TileId::new(812, 1440);
        store.write_tile(&grid, &heights(t)).unwrap();
        std::fs::copy(store.tile_path(t), store.tile_path(t.east())).unwrap();
        let err = store.read_tile(&grid, t.east()).unwrap_err();
        assert!(format!("{err:#}").contains("does not match"), "{err:#}");
    }

    #[test]
    fn absent_tile_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::new(dir.path()).read_tile(&GridSpec::default(), TileId::new(1, 1)).unwrap().is_none());
    }

    #[test]
    fn index_round_trips_and_defaults_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("store"));
        assert_eq!(store.load_index().unwrap(), StoreIndex::default());
        let mut idx = StoreIndex::default();
        let entry = TileEntry { status: TileStatus::Ok, error: None, min_elevation_m: Some(1.0), max_elevation_m: Some(9.0), filled_samples: 3, sources: vec!["n39w121".into()] };
        idx.set(TileId::new(-3, 7), entry.clone());
        idx.set(TileId::new(4, 5), TileEntry { status: TileStatus::Empty, ..entry.clone() });
        store.save_index(&idx).unwrap();
        let back = store.load_index().unwrap();
        assert_eq!(back, idx);
        assert_eq!(back.get(TileId::new(-3, 7)), Some(&entry));
        assert_eq!(back.tile_ids(TileStatus::Ok), vec![TileId::new(-3, 7)]);
    }
}
