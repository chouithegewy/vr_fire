#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;
use vr_fire::dem::{EPSG_NAD83_GEOGRAPHIC, Raster};
use vr_fire::ingest::IngestOptions;
use vr_fire::source::SourceCell;

pub const PX: f64 = 0.01;

pub fn plane(lon: f64, lat: f64) -> f64 {
    1000.0 + 100.0 * (lon + 121.0) + 50.0 * (lat - 38.0)
}

/// A small 3DEP-like file (0.01° pixels, one-pixel overlap buffer) sampled from `plane`,
/// placed in the cache so ingest never needs the network.
pub fn write_plane_source(cache_dir: &Path, cell: SourceCell) {
    std::fs::create_dir_all(cache_dir).unwrap();
    let n = (1.0 / PX).round() as usize + 2;
    let origin_x = -(cell.west as f64) - PX;
    let origin_y = cell.north as f64 + PX;
    let mut data = Vec::with_capacity(n * n);
    for r in 0..n {
        for c in 0..n {
            data.push(plane(origin_x + (c as f64 + 0.5) * PX, origin_y - (r as f64 + 0.5) * PX) as f32);
        }
    }
    Raster { width: n, height: n, data, origin_x, origin_y, pixel_w: PX, pixel_h: PX, epsg: EPSG_NAD83_GEOGRAPHIC, nodata: Some(-999999.0) }
        .write_geotiff(&cache_dir.join(cell.file_name()), false)
        .unwrap();
}

pub fn write_region(dir: &Path, west: f64, south: f64, east: f64, north: f64) -> PathBuf {
    let path = dir.join("region.geojson");
    std::fs::write(
        &path,
        format!(r#"{{"type":"Polygon","coordinates":[[[{west},{south}],[{east},{south}],[{east},{north}],[{west},{north}],[{west},{south}]]]}}"#),
    )
    .unwrap();
    path
}

/// Options that never reach a real server: unknown cells fail fast against a closed port.
pub fn options(dir: &Path, region: PathBuf) -> IngestOptions {
    IngestOptions {
        region,
        tiles: None,
        cache_dir: dir.join("cache"),
        store_dir: dir.join("store"),
        base_url: "http://127.0.0.1:9".into(),
        force: false,
        download_threads: 2,
        max_loaded_sources: 4,
        retry_delay: Duration::ZERO,
    }
}
