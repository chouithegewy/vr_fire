//! Offline evidence for the 2026-09-25 review. These assertions confirm defects,
//! not correct behavior; they should fail when the corresponding defects are fixed.
use vr_fire::codec::{decode, encode};
use vr_fire::dem::{EPSG_CONUS_ALBERS, Raster};
use vr_fire::grid::{GridSpec, NODES_PER_SIDE, TileId};
use vr_fire::lod::{OCEAN_M, tile_path};
use vr_fire::pack::{PackOptions, run_pack};
use vr_fire::reproject::TileHeights;
use vr_fire::store::{Store, StoreIndex, TileEntry, TileStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = encode(&[1.0, 2.0, 3.0, 4.0], 2, 0.25);
    bytes[6..10].copy_from_slice(&f32::NAN.to_le_bytes());
    let (_, heights) = decode(&bytes)?;
    assert!(heights.iter().all(|h| h.is_nan()));
    println!("F06 reproduced: decoder accepts a NaN quantization step and returns four NaN heights");

    let scratch = tempfile::tempdir()?;
    let grid = GridSpec::default();
    let tile = TileId::new(1, 1);
    let n = NODES_PER_SIDE;
    let store = Store::new(scratch.path().join("store"));
    store.write_tile(&grid, &TileHeights { tile, data: vec![0.0; n * n], filled: 0 })?;
    let mut index = StoreIndex::default();
    for ty in 0..3 {
        for tx in 0..3 {
            let t = TileId::new(tx, ty);
            index.set(t, TileEntry {
                status: if t == tile { TileStatus::Ok } else { TileStatus::Empty },
                error: None,
                min_elevation_m: Some(0.0),
                max_elevation_m: Some(0.0),
                filled_samples: 0,
                sources: vec!["synthetic-valid-zero".into()],
            });
        }
    }
    store.save_index(&index)?;
    let opts = PackOptions { store_dir: scratch.path().join("store"), out_dir: scratch.path().join("packed") };
    run_pack(&opts)?;
    let packed_path = opts.out_dir.join(tile_path(0, 1, 1));
    let (side, heights) = decode(&std::fs::read(&packed_path)?)?;
    assert_eq!(heights[side + 1], OCEAN_M);
    println!("F03 reproduced: valid 0.0 m sample with filled_samples=0 becomes {} m after packing", heights[side + 1]);

    index.tiles.get_mut("1_1").unwrap().status = TileStatus::Failed;
    store.save_index(&index)?;
    let result = run_pack(&opts)?;
    assert_eq!(result.files, 0);
    assert!(packed_path.exists());
    println!("F07 reproduced: repacking a failed tile writes zero files but retains its old t0/1_1.vrh");

    let bounds = grid.tile_bounds(tile);
    let wrong_scale = Raster {
        width: n, height: n, data: vec![42.0; n * n],
        origin_x: bounds.x_min - 10.0, origin_y: bounds.y_max + 10.0,
        pixel_w: 20.0, pixel_h: 20.0, epsg: EPSG_CONUS_ALBERS, nodata: None,
    };
    wrong_scale.write_geotiff(&store.tile_path(tile), true)?;
    assert!(store.read_tile(&grid, tile)?.is_some());
    println!("F08 reproduced: Store::read_tile accepts 20 m pixels for a required 10 m tile");
    println!("4/4 bounded offline defect probes reproduced; temporary data is automatically removed.");
    Ok(())
}
