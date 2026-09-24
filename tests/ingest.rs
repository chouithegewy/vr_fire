mod common;

use common::*;
use vr_fire::crs::Albers;
use vr_fire::grid::{GridSpec, NODES_PER_SIDE, TileRange};
use vr_fire::ingest::run_ingest;
use vr_fire::source::SourceCell;
use vr_fire::store::{Store, TileStatus};

const CELL: SourceCell = SourceCell { north: 39, west: 121 };

#[test]
fn ingests_region_from_cached_source() {
    let dir = tempfile::tempdir().unwrap();
    write_plane_source(&dir.path().join("cache"), CELL);
    let opts = options(dir.path(), write_region(dir.path(), -120.7, 38.4, -120.6, 38.5));
    let report = run_ingest(&opts).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(report.written >= 9, "{report:?}");

    let store = Store::new(&opts.store_dir);
    let index = store.load_index().unwrap();
    let grid = GridSpec::default();
    let albers = Albers::new().unwrap();
    for t in index.tile_ids(TileStatus::Ok) {
        let entry = index.get(t).unwrap();
        assert_eq!(entry.sources, vec!["n39w121".to_string()]);
        assert_eq!(entry.filled_samples, 0);
        let h = store.read_tile(&grid, t).unwrap().unwrap();
        let (x, y) = grid.node_xy(t, 100, 200);
        let (lon, lat) = albers.to_lonlat(x, y).unwrap();
        assert!((h[200 * NODES_PER_SIDE + 100] as f64 - plane(lon, lat)).abs() < 1e-2);
    }
}

#[test]
fn rerun_skips_completed_tiles() {
    let dir = tempfile::tempdir().unwrap();
    write_plane_source(&dir.path().join("cache"), CELL);
    let mut opts = options(dir.path(), write_region(dir.path(), -120.7, 38.4, -120.6, 38.5));
    let first = run_ingest(&opts).unwrap();
    let second = run_ingest(&opts).unwrap();
    assert_eq!((second.written, second.skipped), (0, first.written + first.empty));
    opts.force = true;
    let forced = run_ingest(&opts).unwrap();
    assert_eq!(forced.written, first.written);
}

#[test]
fn tile_range_limits_the_work() {
    let dir = tempfile::tempdir().unwrap();
    write_plane_source(&dir.path().join("cache"), CELL);
    let mut opts = options(dir.path(), write_region(dir.path(), -120.7, 38.4, -120.6, 38.5));
    let all = Store::new(&opts.store_dir);
    let first = run_ingest(&opts).unwrap();
    let some = all.load_index().unwrap().tile_ids(TileStatus::Ok)[0];
    std::fs::remove_dir_all(&opts.store_dir).unwrap();
    opts.tiles = Some(TileRange { min: some, max: some });
    let report = run_ingest(&opts).unwrap();
    assert!(first.written > 1);
    assert_eq!(report.written, 1);
}

#[test]
fn unreachable_sources_fail_only_their_tiles() {
    let dir = tempfile::tempdir().unwrap();
    // No cached file and no server: every tile needing n39w121 must fail, and be recorded.
    let opts = options(dir.path(), write_region(dir.path(), -120.7, 38.4, -120.6, 38.5));
    let report = run_ingest(&opts).unwrap();
    assert_eq!(report.written, 0);
    assert!(!report.failed.is_empty());
    assert!(report.failed[0].1.contains("n39w121"), "{}", report.failed[0].1);
    let index = Store::new(&opts.store_dir).load_index().unwrap();
    assert_eq!(index.tile_ids(TileStatus::Failed).len(), report.failed.len());
}

/// Downloads one real ~460 MB 3DEP file. Run with: cargo test --release -- --ignored real_3dep
#[test]
#[ignore]
fn real_3dep_single_tile() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path(), "data/regions/california.geojson".into());
    opts.base_url = vr_fire::source::USGS_13_BASE_URL.into();
    let grid = GridSpec::default();
    let (x, y) = Albers::new().unwrap().from_lonlat(-120.65, 38.45).unwrap();
    let t = grid.tile_containing(x, y);
    opts.tiles = Some(TileRange { min: t, max: t });
    let report = run_ingest(&opts).unwrap();
    assert_eq!(report.written, 1, "{report:?}");
    let entry = Store::new(&opts.store_dir).load_index().unwrap().get(t).cloned().unwrap();
    // Sierra Nevada foothills near Placerville: roughly 500–1,500 m.
    assert!(entry.min_elevation_m.unwrap() > 200.0 && entry.max_elevation_m.unwrap() < 2000.0, "{entry:?}");
}
