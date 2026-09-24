mod common;

use common::*;
use vr_fire::bake::{BakeOptions, run_bake};
use vr_fire::export::TileMetadata;
use vr_fire::grid::{GridSpec, NODES_PER_SIDE, TileId, TileRange};
use vr_fire::ingest::run_ingest;
use vr_fire::source::SourceCell;
use vr_fire::store::{Store, StoreIndex, TileEntry, TileStatus};

fn ingested() -> (tempfile::TempDir, Vec<TileId>) {
    let dir = tempfile::tempdir().unwrap();
    write_plane_source(&dir.path().join("cache"), SourceCell { north: 39, west: 121 });
    let opts = options(dir.path(), write_region(dir.path(), -120.7, 38.4, -120.6, 38.5));
    run_ingest(&opts).unwrap();
    let tiles = Store::new(&opts.store_dir).load_index().unwrap().tile_ids(TileStatus::Ok);
    (dir, tiles)
}

fn load(path: &std::path::Path) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let (doc, buffers, _) = gltf::import(path).unwrap();
    let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
    let r = prim.reader(|b| Some(&buffers[b.index()]));
    (r.read_positions().unwrap().collect(), r.read_normals().unwrap().collect())
}

#[test]
fn bakes_every_ok_tile_with_all_outputs() {
    let (dir, tiles) = ingested();
    let out = dir.path().join("tiles");
    let report = run_bake(&BakeOptions { store_dir: dir.path().join("store"), out_dir: out.clone(), tiles: None }).unwrap();
    assert_eq!(report.baked, tiles.len());
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    for t in &tiles {
        for (lod, n) in [(0, 376), (1, 126), (2, 26), (3, 6)] {
            let (positions, _) = load(&out.join(format!("lod{lod}/{t}.glb")));
            assert_eq!(positions.len(), n * n + 4 * n);
        }
        let raw = std::fs::read(out.join(format!("{t}.f32"))).unwrap();
        assert_eq!(raw.len(), NODES_PER_SIDE * NODES_PER_SIDE * 4);
        let meta: TileMetadata = serde_json::from_slice(&std::fs::read(out.join(format!("{t}.json"))).unwrap()).unwrap();
        assert_eq!(meta.bounds, GridSpec::default().tile_bounds(*t));
        assert_eq!(meta.sources, vec!["n39w121".to_string()]);
    }
}

#[test]
fn neighboring_baked_tiles_are_seamless() {
    let (dir, tiles) = ingested();
    let out = dir.path().join("tiles");
    run_bake(&BakeOptions { store_dir: dir.path().join("store"), out_dir: out.clone(), tiles: None }).unwrap();
    let pair = tiles.iter().find(|t| tiles.contains(&t.east())).expect("two horizontally adjacent tiles");
    let n = NODES_PER_SIDE;
    let (wp, wn) = load(&out.join(format!("lod0/{pair}.glb")));
    let (ep, en) = load(&out.join(format!("lod0/{}.glb", pair.east())));
    for r in 0..n {
        let (a, b) = (wp[r * n + n - 1], ep[r * n]);
        assert_eq!((a[0], a[1].to_bits(), a[2]), (b[0] + 3750.0, b[1].to_bits(), b[2]), "row {r}");
        // Corner rows may differ: their north/south neighbors are different tiles.
        if r > 0 && r < n - 1 {
            assert_eq!(wn[r * n + n - 1], en[r * n], "normal row {r}");
        }
    }
}

#[test]
fn bake_edge_tile_without_neighbors() {
    let (dir, tiles) = ingested();
    let lonely = *tiles.iter().find(|t| !tiles.contains(&t.north()) || !tiles.contains(&t.west())).unwrap();
    let report = run_bake(&BakeOptions {
        store_dir: dir.path().join("store"),
        out_dir: dir.path().join("tiles"),
        tiles: Some(TileRange { min: lonely, max: lonely }),
    })
    .unwrap();
    assert_eq!(report.baked, 1, "{report:?}");
}

#[test]
fn empty_and_unknown_tiles_are_reported() {
    let (dir, _) = ingested();
    let store = Store::new(dir.path().join("store"));
    let mut index: StoreIndex = store.load_index().unwrap();
    let empty = TileId::new(1, 1);
    index.set(empty, TileEntry { status: TileStatus::Empty, error: None, min_elevation_m: None, max_elevation_m: None, filled_samples: 0, sources: vec![] });
    store.save_index(&index).unwrap();
    let unknown = TileId::new(2, 1);
    let report = run_bake(&BakeOptions {
        store_dir: dir.path().join("store"),
        out_dir: dir.path().join("tiles"),
        tiles: Some(TileRange { min: empty, max: unknown }),
    })
    .unwrap();
    assert_eq!((report.baked, report.skipped_empty), (0, 1));
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].1.contains("vr_fire ingest"), "{}", report.failed[0].1);
}
