use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use vr_fire::bake::{BakeOptions, run_bake};
use vr_fire::crs::Albers;
use vr_fire::grid::{GridSpec, TileId, TileRange, cell_to_tile};
use vr_fire::ingest::{IngestOptions, run_ingest};
use vr_fire::source::USGS_13_BASE_URL;

#[derive(Parser)]
#[command(name = "vr_fire", about = "Terrain tiles for the VR wildfire simulator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download USGS 3DEP (10 m) and build height tiles in the store.
    Ingest {
        /// GeoJSON boundary of the area to ingest.
        #[arg(long, default_value = "data/regions/california.geojson")]
        region: PathBuf,
        /// Only these tiles: "tx,ty" or "tx0,ty0..tx1,ty1".
        #[arg(long, allow_hyphen_values = true)]
        tiles: Option<TileRange>,
        #[arg(long, default_value = "cache")]
        cache: PathBuf,
        #[arg(long, default_value = "store")]
        store: PathBuf,
        /// Rebuild tiles that are already done.
        #[arg(long)]
        force: bool,
        #[arg(long, default_value = USGS_13_BASE_URL)]
        base_url: String,
        #[arg(long, default_value_t = 4)]
        download_threads: usize,
        /// Decoded 3DEP files held in memory (~470 MB each).
        #[arg(long, default_value_t = 6)]
        max_loaded_sources: usize,
    },
    /// Turn store tiles into glb meshes (4 LODs), raw heights, and metadata.
    Bake {
        /// Only these tiles (default: every ok tile in the store).
        #[arg(long, allow_hyphen_values = true)]
        tiles: Option<TileRange>,
        #[arg(long, default_value = "store")]
        store: PathBuf,
        #[arg(long, default_value = "tiles")]
        out: PathBuf,
    },
    /// Show the tile and 30 m ML cell containing a lon/lat.
    Locate {
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        #[arg(long)]
        lat: f64,
    },
}

fn main() -> Result<ExitCode> {
    let failed = match Cli::parse().command {
        Command::Ingest { region, tiles, cache, store, force, base_url, download_threads, max_loaded_sources } => {
            let r = run_ingest(&IngestOptions {
                region,
                tiles,
                cache_dir: cache,
                store_dir: store,
                base_url,
                force,
                download_threads,
                max_loaded_sources,
                retry_delay: Duration::from_secs(2),
            })?;
            println!("ingest: {} written, {} empty, {} skipped, {} failed", r.written, r.empty, r.skipped, r.failed.len());
            r.failed
        }
        Command::Bake { tiles, store, out } => {
            let r = run_bake(&BakeOptions { store_dir: store, out_dir: out, tiles })?;
            println!("bake: {} baked, {} empty, {} failed", r.baked, r.skipped_empty, r.failed.len());
            r.failed
        }
        Command::Locate { lon, lat } => {
            locate(lon, lat)?;
            vec![]
        }
    };
    for (t, e) in &failed {
        eprintln!("  {t}: {e}");
    }
    Ok(if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn locate(lon: f64, lat: f64) -> Result<()> {
    let grid = GridSpec::default();
    let (x, y) = Albers::new()?.from_lonlat(lon, lat)?;
    let t = grid.tile_containing(x, y);
    let b = grid.tile_bounds(t);
    let (c, r) = grid.cell_containing(x, y);
    let (_, (lc, lr)) = cell_to_tile(c, r);
    println!("lon/lat      {lon}, {lat}");
    println!("EPSG:5070    {x:.2}, {y:.2}");
    println!("tile         {t}  (x {}..{}, y {}..{})", b.x_min, b.x_max, b.y_min, b.y_max);
    println!("ML cell      global ({c}, {r}), in tile ({lc}, {lr})");
    let (nw, se) = (TileId::new(t.tx - 1, t.ty - 1), TileId::new(t.tx + 1, t.ty + 1));
    println!("3×3 around   --tiles {},{}..{},{}", nw.tx, nw.ty, se.tx, se.ty);
    Ok(())
}
