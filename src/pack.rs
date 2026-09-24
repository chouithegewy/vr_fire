//! `pack`: 10 m store tiles → compressed `.vrh` patches for every viewer level of detail.
//!
//! Each patch holds (n+2)² heights: the level's n×n nodes plus a one-node ring taken from
//! the neighbouring tiles, so the viewer computes seamless normals. A patch that needs a
//! tile missing from the store is skipped; the viewer then falls back to USGS COG for it.

use crate::codec::encode;
use crate::grid::{GridSpec, NODES_PER_SIDE, TileId};
use crate::lod::{OCEAN_M, SUPER_STEP, SUPER_STRIDE, SUPER_TILES, TILE_STEPS, TILE_STRIDES, super_nodes, super_path, tile_nodes, tile_path};
use crate::store::{Store, StoreIndex, TileStatus};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct PackOptions {
    pub store_dir: PathBuf,
    pub out_dir: PathBuf,
}

#[derive(Debug, Default)]
pub struct PackReport {
    pub files: usize,
    pub bytes: u64,
    pub skipped: usize,
}

const TILE_INTERVALS: i64 = (NODES_PER_SIDE - 1) as i64;

/// Heights at global 10 m node indices (gx east, gy south from the grid origin).
struct Nodes<'a> {
    store: &'a Store,
    index: &'a StoreIndex,
    grid: GridSpec,
    cache: Mutex<HashMap<TileId, Option<Arc<Vec<f32>>>>>,
}

impl Nodes<'_> {
    fn tile(&self, t: TileId) -> Option<Arc<Vec<f32>>> {
        if let Some(hit) = self.cache.lock().unwrap().get(&t) {
            return hit.clone();
        }
        let data = match self.index.get(t).map(|e| e.status) {
            // Store fills no-data with exactly 0.0 (sea level); the viewer wants OCEAN_M.
            Some(TileStatus::Ok) => self.store.read_tile(&self.grid, t).ok().flatten().map(|mut d| {
                d.iter_mut().filter(|h| **h == 0.0).for_each(|h| *h = OCEAN_M);
                Arc::new(d)
            }),
            Some(TileStatus::Empty) => Some(Arc::new(vec![OCEAN_M; NODES_PER_SIDE * NODES_PER_SIDE])),
            _ => None,
        };
        self.cache.lock().unwrap().insert(t, data.clone());
        data
    }

    fn at(&self, gx: i64, gy: i64) -> Option<f32> {
        let t = TileId::new(gx.div_euclid(TILE_INTERVALS), gy.div_euclid(TILE_INTERVALS));
        let (i, j) = (gx.rem_euclid(TILE_INTERVALS) as usize, gy.rem_euclid(TILE_INTERVALS) as usize);
        self.tile(t).map(|d| d[j * NODES_PER_SIDE + i])
    }

    /// (n+2)² nodes starting one stride before (gx0, gy0); None if any tile is missing.
    fn patch(&self, gx0: i64, gy0: i64, stride: i64, n: usize) -> Option<Vec<f32>> {
        let m = n + 2;
        let mut out = Vec::with_capacity(m * m);
        for j in 0..m as i64 {
            for i in 0..m as i64 {
                out.push(self.at(gx0 + (i - 1) * stride, gy0 + (j - 1) * stride)?);
            }
        }
        Some(out)
    }

    fn evict_rows_before(&self, ty: i64) {
        self.cache.lock().unwrap().retain(|t, _| t.ty >= ty);
    }
}

fn write(out_dir: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let path = out_dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn run_pack(opts: &PackOptions) -> Result<PackReport> {
    let store = Store::new(&opts.store_dir);
    let index = store.load_index()?;
    let nodes = Nodes { store: &store, index: &index, grid: index.grid, cache: Mutex::default() };
    let mut rows: BTreeMap<i64, Vec<TileId>> = BTreeMap::new();
    for t in index.tile_ids(TileStatus::Ok) {
        rows.entry(t.ty).or_default().push(t);
    }
    let total: usize = rows.values().map(Vec::len).sum();
    let report = Mutex::new(PackReport::default());
    let emit = |rel: String, patch: Option<Vec<f32>>, side: usize, step: f32| -> Result<()> {
        let mut r = report.lock().unwrap();
        match patch {
            Some(h) => {
                drop(r);
                let bytes = encode(&h, side, step);
                write(&opts.out_dir, &rel, &bytes)?;
                let mut r = report.lock().unwrap();
                r.files += 1;
                r.bytes += bytes.len() as u64;
            }
            None => r.skipped += 1,
        }
        Ok(())
    };
    let mut done = 0;
    for (&ty, tiles) in &rows {
        nodes.evict_rows_before(ty - 1);
        tiles.par_iter().try_for_each(|t| -> Result<()> {
            for (lod, &stride) in TILE_STRIDES.iter().enumerate() {
                let n = tile_nodes(lod);
                let patch = nodes.patch(t.tx * TILE_INTERVALS, t.ty * TILE_INTERVALS, stride, n);
                emit(tile_path(lod, t.tx, t.ty), patch, n + 2, TILE_STEPS[lod])?;
            }
            Ok(())
        })?;
        done += tiles.len();
        if ty % 10 == 0 {
            eprintln!("pack: tiles {done}/{total}");
        }
    }
    // Super tiles containing any stored tile, one super row at a time.
    let mut supers: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for t in rows.values().flatten() {
        let (sx, sy) = (t.tx.div_euclid(SUPER_TILES), t.ty.div_euclid(SUPER_TILES));
        let row = supers.entry(sy).or_default();
        if !row.contains(&sx) {
            row.push(sx);
        }
    }
    nodes.evict_rows_before(i64::MAX);
    for (&sy, sxs) in &supers {
        nodes.evict_rows_before(sy * SUPER_TILES - 1);
        sxs.par_iter().try_for_each(|&sx| -> Result<()> {
            let n = super_nodes();
            let span = SUPER_TILES * TILE_INTERVALS;
            let patch = nodes.patch(sx * span, sy * span, SUPER_STRIDE, n);
            emit(super_path(sx, sy), patch, n + 2, SUPER_STEP)
        })?;
    }
    Ok(report.into_inner().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode;
    use crate::reproject::TileHeights;

    /// Global 10 m node height: a gentle plane, with node (10, 10) of each tile "filled" (0.0).
    fn height(gx: i64, gy: i64) -> f32 {
        500.0 + gx as f32 * 0.02 - gy as f32 * 0.01
    }

    fn write_store(dir: &std::path::Path, tiles: impl Iterator<Item = TileId>) {
        let store = Store::new(dir);
        let grid = GridSpec::default();
        let mut index = StoreIndex::default();
        for t in tiles {
            let data = (0..NODES_PER_SIDE * NODES_PER_SIDE)
                .map(|k| {
                    let (i, j) = ((k % NODES_PER_SIDE) as i64, (k / NODES_PER_SIDE) as i64);
                    if (i, j) == (10, 10) { 0.0 } else { height(t.tx * 375 + i, t.ty * 375 + j) }
                })
                .collect();
            store.write_tile(&grid, &TileHeights { tile: t, data, filled: 1 }).unwrap();
            index.set(t, crate::store::TileEntry { status: TileStatus::Ok, error: None, min_elevation_m: None, max_elevation_m: None, filled_samples: 1, sources: vec![] });
        }
        store.save_index(&index).unwrap();
    }

    #[test]
    fn packs_every_level_with_neighbour_rings() {
        let dir = tempfile::tempdir().unwrap();
        let tiles = (0..3).flat_map(|ty| (0..3).map(move |tx| TileId::new(tx, ty)));
        write_store(&dir.path().join("store"), tiles);
        let out = dir.path().join("packed");
        let report = run_pack(&PackOptions { store_dir: dir.path().join("store"), out_dir: out.clone() }).unwrap();
        // Only the centre tile has all its ring neighbours; edge tiles fall back to COG.
        assert_eq!(report.files, TILE_STRIDES.len());
        for (lod, &stride) in TILE_STRIDES.iter().enumerate() {
            let (side, h) = decode(&std::fs::read(out.join(tile_path(lod, 1, 1))).unwrap()).unwrap();
            let n = tile_nodes(lod);
            assert_eq!(side, n + 2);
            for (i, j) in [(0usize, 0usize), (1, 1), (n, n), (n + 1, 3)] {
                let (gx, gy) = (375 + (i as i64 - 1) * stride, 375 + (j as i64 - 1) * stride);
                let want = height(gx, gy);
                assert!((h[j * side + i] - want).abs() <= TILE_STEPS[lod], "lod {lod} node ({i},{j})");
            }
        }
        assert!(!out.join(tile_path(0, 0, 0)).exists());
    }

    #[test]
    fn filled_samples_become_ocean() {
        let dir = tempfile::tempdir().unwrap();
        write_store(&dir.path().join("store"), (0..3).flat_map(|ty| (0..3).map(move |tx| TileId::new(tx, ty))));
        let out = dir.path().join("packed");
        run_pack(&PackOptions { store_dir: dir.path().join("store"), out_dir: out.clone() }).unwrap();
        let (side, h) = decode(&std::fs::read(out.join(tile_path(0, 1, 1))).unwrap()).unwrap();
        // Store node (10,10) of tile (1,1) is ring-offset by one.
        assert!((h[11 * side + 11] - OCEAN_M).abs() < 0.2);
    }

    #[test]
    fn packs_super_tiles() {
        let dir = tempfile::tempdir().unwrap();
        // Super tile (1,1) covers tiles 10..=19; its ring needs tiles 9 and 20 too.
        write_store(&dir.path().join("store"), (9..=20).flat_map(|ty| (9..=20).map(move |tx| TileId::new(tx, ty))));
        let out = dir.path().join("packed");
        run_pack(&PackOptions { store_dir: dir.path().join("store"), out_dir: out.clone() }).unwrap();
        let (side, h) = decode(&std::fs::read(out.join(super_path(1, 1))).unwrap()).unwrap();
        assert_eq!(side, super_nodes() + 2);
        let (gx, gy) = (3750 + 20 * 75, 3750 + 30 * 75);
        assert!((h[31 * side + 21] - height(gx, gy)).abs() <= SUPER_STEP);
    }
}
