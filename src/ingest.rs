//! `ingest`: region → grid tiles → cached 3DEP sources → store height tiles + index.

use crate::crs::Albers;
use crate::dem::{EPSG_NAD83_GEOGRAPHIC, EPSG_WGS84_GEOGRAPHIC, Raster};
use crate::grid::{GridSpec, TILE_SIZE_M, TileId, TileRange};
use crate::region::{Region, tiles_in_region};
use crate::reproject::{Sources, min_max, resample_tile, source_cells_for_tile};
use crate::source::{Fetched, SourceCache, SourceCell};
use crate::store::{Store, StoreIndex, TileEntry, TileStatus};
use anyhow::{Result, anyhow, bail, ensure};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub struct IngestOptions {
    /// GeoJSON boundary; only tiles intersecting it are ingested.
    pub region: PathBuf,
    /// Optional further restriction to a rectangle of tiles.
    pub tiles: Option<TileRange>,
    pub cache_dir: PathBuf,
    pub store_dir: PathBuf,
    pub base_url: String,
    /// Re-ingest tiles that are already done.
    pub force: bool,
    pub download_threads: usize,
    /// Decoded 3DEP files kept in memory at once (~470 MB each).
    pub max_loaded_sources: usize,
    pub retry_delay: Duration,
}

#[derive(Debug, Default)]
pub struct IngestReport {
    pub written: usize,
    pub empty: usize,
    pub skipped: usize,
    pub failed: Vec<(TileId, String)>,
}

struct Job {
    tile: TileId,
    cells: Vec<SourceCell>,
}

pub fn run_ingest(opts: &IngestOptions) -> Result<IngestReport> {
    let grid = GridSpec::default();
    let albers = Albers::new()?;
    let store = Store::new(&opts.store_dir);
    let cache = SourceCache::new(&opts.cache_dir, &opts.base_url).with_retry_delay(opts.retry_delay);
    let mut index = store.load_index()?;
    if index.tiles.is_empty() {
        index.grid = grid;
    }
    ensure!(index.grid == grid, "store {} was built with a different grid origin", opts.store_dir.display());

    let mut report = IngestReport::default();
    let region = Region::load(&opts.region)?;
    let mut groups: BTreeMap<SourceCell, Vec<Job>> = BTreeMap::new();
    for tile in tiles_in_region(&grid, &albers, &region)? {
        if opts.tiles.is_some_and(|r| !r.contains(tile)) {
            continue;
        }
        if !opts.force && is_done(&index, &store, tile) {
            report.skipped += 1;
            continue;
        }
        let b = grid.tile_bounds(tile);
        let (lon, lat) = albers.to_lonlat(b.x_min + TILE_SIZE_M / 2.0, b.y_max - TILE_SIZE_M / 2.0)?;
        let cells = source_cells_for_tile(&grid, &albers, tile)?;
        groups.entry(SourceCell::containing(lon, lat)).or_default().push(Job { tile, cells });
    }
    let total: usize = groups.values().map(Vec::len).sum();
    eprintln!("ingest: {total} tiles to build, {} already done", report.skipped);

    let needed: BTreeSet<SourceCell> = groups.values().flatten().flat_map(|j| j.cells.iter().copied()).collect();
    let needed: Vec<SourceCell> = needed.into_iter().collect();
    eprintln!("ingest: ensuring {} 3DEP source files in {}", needed.len(), opts.cache_dir.display());
    let mut fetched: HashMap<SourceCell, Result<Fetched, String>> = cache
        .prefetch(&needed, opts.download_threads)
        .into_iter()
        .map(|(c, r)| (c, r.map_err(|e| format!("{e:#}"))))
        .collect();

    let mut sources = Sources::default();
    let mut done = 0;
    for (primary, jobs) in groups {
        let group_cells: BTreeSet<SourceCell> = jobs.iter().flat_map(|j| j.cells.iter().copied()).collect();
        for c in sources.cells() {
            if !group_cells.contains(&c) && sources.len() >= opts.max_loaded_sources {
                sources.remove(c);
            }
        }
        let to_load: Vec<(SourceCell, PathBuf)> = group_cells
            .iter()
            .filter(|c| !sources.contains(**c))
            .filter_map(|c| match fetched.get(c) {
                Some(Ok(Fetched::Present(p))) => Some((*c, p.clone())),
                _ => None,
            })
            .collect();
        let loaded: Vec<(SourceCell, Result<Raster>)> =
            to_load.par_iter().map(|(c, p)| (*c, load_source(&cache, *c, p))).collect();
        for (c, r) in loaded {
            match r {
                Ok(r) => sources.insert(c, Arc::new(r)),
                Err(e) => {
                    fetched.insert(c, Err(format!("{e:#}")));
                }
            }
        }

        let results: Vec<(TileId, Result<TileEntry>)> = jobs
            .par_iter()
            .map(|j| (j.tile, ingest_tile(&grid, &albers, &store, &sources, &fetched, j)))
            .collect();
        for (tile, result) in results {
            let entry = match result {
                Ok(e) => {
                    match e.status {
                        TileStatus::Ok => report.written += 1,
                        _ => report.empty += 1,
                    }
                    e
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    report.failed.push((tile, msg.clone()));
                    TileEntry { status: TileStatus::Failed, error: Some(msg), min_elevation_m: None, max_elevation_m: None, filled_samples: 0, sources: vec![] }
                }
            };
            index.set(tile, entry);
        }
        store.save_index(&index)?;
        done += jobs.len();
        eprintln!("ingest: [{done}/{total}] {} done", primary.name());
    }
    Ok(report)
}

fn is_done(index: &StoreIndex, store: &Store, t: TileId) -> bool {
    match index.get(t).map(|e| e.status) {
        Some(TileStatus::Empty) => true,
        Some(TileStatus::Ok) => store.tile_path(t).exists(),
        _ => false,
    }
}

/// Decode a cached source; if it is corrupt, re-download once and try again.
fn load_source(cache: &SourceCache, c: SourceCell, path: &Path) -> Result<Raster> {
    let raster = match Raster::read_geotiff(path) {
        Ok(r) => r,
        Err(first) => {
            eprintln!("ingest: {} unreadable ({first:#}); downloading again", c.name());
            cache.invalidate(c)?;
            match cache.ensure(c)? {
                Fetched::Present(p) => Raster::read_geotiff(&p)?,
                Fetched::Missing => bail!("{} disappeared from USGS", c.name()),
            }
        }
    };
    if !matches!(raster.epsg, EPSG_NAD83_GEOGRAPHIC | EPSG_WGS84_GEOGRAPHIC) {
        bail!("{}: unexpected CRS EPSG:{} (expected 4269)", path.display(), raster.epsg);
    }
    Ok(raster)
}

fn ingest_tile(
    grid: &GridSpec,
    albers: &Albers,
    store: &Store,
    sources: &Sources,
    fetched: &HashMap<SourceCell, Result<Fetched, String>>,
    job: &Job,
) -> Result<TileEntry> {
    for c in &job.cells {
        if let Some(Err(e)) = fetched.get(c) {
            return Err(anyhow!("source {} unavailable: {e}", c.name()));
        }
    }
    let used: Vec<String> = job.cells.iter().filter(|c| sources.contains(**c)).map(|c| c.name()).collect();
    match resample_tile(grid, albers, sources, job.tile)? {
        None => Ok(TileEntry { status: TileStatus::Empty, error: None, min_elevation_m: None, max_elevation_m: None, filled_samples: 0, sources: used }),
        Some(h) => {
            store.write_tile(grid, &h)?;
            let (lo, hi) = min_max(&h.data);
            Ok(TileEntry { status: TileStatus::Ok, error: None, min_elevation_m: Some(lo), max_elevation_m: Some(hi), filled_samples: h.filled, sources: used })
        }
    }
}
