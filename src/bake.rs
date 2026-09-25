//! `bake`: store height tiles → glb and/or FBX meshes (all LODs), raw heights, and metadata.

use crate::export::{TileMetadata, write_glb, write_heights_f32, write_metadata};
use crate::fbx::write_fbx;
use crate::grid::{LOD_STRIDES, TileId, TileRange};
use crate::mesh::{Neighborhood, build_mesh, compute_normals};
use crate::store::{Store, StoreIndex, TileStatus};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

pub struct BakeOptions {
    pub store_dir: PathBuf,
    pub out_dir: PathBuf,
    /// None bakes every `ok` tile in the store.
    pub tiles: Option<TileRange>,
    pub format: MeshFormat,
}

/// Mesh file format: glTF binary (Bevy, Blender, three.js) and/or FBX (Unity's native import).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "pipeline", derive(clap::ValueEnum))]
pub enum MeshFormat {
    #[default]
    Glb,
    Fbx,
    Both,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BakeOutcome {
    Baked,
    Empty,
}

#[derive(Debug, Default)]
pub struct BakeReport {
    pub baked: usize,
    pub skipped_empty: usize,
    pub failed: Vec<(TileId, String)>,
}

pub fn bake_tile(store: &Store, index: &StoreIndex, tile: TileId, out_dir: &Path, format: MeshFormat) -> Result<BakeOutcome> {
    let grid = index.grid;
    let entry = index
        .get(tile)
        .with_context(|| format!("tile {tile} is not in the store; run `vr_fire ingest --tiles {},{}` first", tile.tx, tile.ty))?;
    match entry.status {
        TileStatus::Empty => return Ok(BakeOutcome::Empty),
        TileStatus::Failed => bail!("tile {tile} failed ingest: {}", entry.error.as_deref().unwrap_or("unknown error")),
        TileStatus::Ok => {}
    }
    let center = store
        .read_tile(&grid, tile)?
        .with_context(|| format!("tile {tile} is in the index but {} is missing", store.tile_path(tile).display()))?;
    let north = store.read_tile(&grid, tile.north())?;
    let south = store.read_tile(&grid, tile.south())?;
    let east = store.read_tile(&grid, tile.east())?;
    let west = store.read_tile(&grid, tile.west())?;
    let normals = compute_normals(&Neighborhood {
        center: &center,
        north: north.as_deref(),
        south: south.as_deref(),
        east: east.as_deref(),
        west: west.as_deref(),
    });
    for lod in 0..LOD_STRIDES.len() {
        let dir = out_dir.join(format!("lod{lod}"));
        fs::create_dir_all(&dir)?;
        let mesh = build_mesh(&center, &normals, lod);
        let name = format!("tile_{tile}_lod{lod}");
        if format != MeshFormat::Fbx {
            write_glb(&dir.join(format!("{tile}.glb")), &mesh, &name)?;
        }
        if format != MeshFormat::Glb {
            write_fbx(&dir.join(format!("{tile}.fbx")), &mesh, &name)?;
        }
    }
    write_heights_f32(&out_dir.join(format!("{tile}.f32")), &center)?;
    write_metadata(&out_dir.join(format!("{tile}.json")), &TileMetadata::new(&grid, tile, entry, &center))?;
    Ok(BakeOutcome::Baked)
}

pub fn run_bake(opts: &BakeOptions) -> Result<BakeReport> {
    let store = Store::new(&opts.store_dir);
    let index = store.load_index()?;
    let tiles = match opts.tiles {
        Some(r) => r.tiles(),
        None => index.tile_ids(TileStatus::Ok),
    };
    fs::create_dir_all(&opts.out_dir)?;
    eprintln!("bake: {} tiles → {}", tiles.len(), opts.out_dir.display());
    let results: Vec<(TileId, Result<BakeOutcome>)> =
        tiles.par_iter().map(|&t| (t, bake_tile(&store, &index, t, &opts.out_dir, opts.format))).collect();
    let mut report = BakeReport::default();
    for (t, r) in results {
        match r {
            Ok(BakeOutcome::Baked) => report.baked += 1,
            Ok(BakeOutcome::Empty) => report.skipped_empty += 1,
            Err(e) => report.failed.push((t, format!("{e:#}"))),
        }
    }
    Ok(report)
}
