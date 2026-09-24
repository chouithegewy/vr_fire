//! EPSG:5070 (NAD83 / CONUS Albers) <-> NAD83 geographic lon/lat, via pure-Rust proj4rs.
//! 3DEP and EPSG:5070 share the NAD83 datum, so no datum shift is involved.

use crate::grid::Bounds;
use anyhow::{Result, anyhow};
use proj4rs::Proj;

pub const EPSG_5070: &str = "+proj=aea +lat_0=23 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +units=m +no_defs";
pub const NAD83_GEOGRAPHIC: &str = "+proj=longlat +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +no_defs";

/// Samples per box edge when converting boxes (edges are curved in the other CRS).
const EDGE_SAMPLES: usize = 33;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LonLatBox {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl LonLatBox {
    pub fn padded(&self, deg: f64) -> Self {
        Self { west: self.west - deg, south: self.south - deg, east: self.east + deg, north: self.north + deg }
    }
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        lon >= self.west && lon <= self.east && lat >= self.south && lat <= self.north
    }
}

pub struct Albers {
    albers: Proj,
    geographic: Proj,
}

impl Albers {
    pub fn new() -> Result<Self> {
        Ok(Self {
            albers: Proj::from_proj_string(EPSG_5070).map_err(|e| anyhow!("EPSG:5070 proj string: {e}"))?,
            geographic: Proj::from_proj_string(NAD83_GEOGRAPHIC).map_err(|e| anyhow!("NAD83 proj string: {e}"))?,
        })
    }

    /// EPSG:5070 meters → NAD83 (lon, lat) degrees.
    pub fn to_lonlat(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let mut p = (x, y, 0.0);
        proj4rs::transform::transform(&self.albers, &self.geographic, &mut p)
            .map_err(|e| anyhow!("EPSG:5070 -> lon/lat failed at ({x}, {y}): {e}"))?;
        Ok((p.0.to_degrees(), p.1.to_degrees()))
    }

    /// NAD83 (lon, lat) degrees → EPSG:5070 meters.
    pub fn from_lonlat(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let mut p = (lon.to_radians(), lat.to_radians(), 0.0);
        proj4rs::transform::transform(&self.geographic, &self.albers, &mut p)
            .map_err(|e| anyhow!("lon/lat -> EPSG:5070 failed at ({lon}, {lat}): {e}"))?;
        Ok((p.0, p.1))
    }

    /// Lon/lat box enclosing an EPSG:5070 rectangle.
    pub fn lonlat_bounds(&self, b: &Bounds) -> Result<LonLatBox> {
        let mut out = LonLatBox { west: f64::MAX, south: f64::MAX, east: f64::MIN, north: f64::MIN };
        for (x, y) in edge_samples(b.x_min, b.y_min, b.x_max, b.y_max) {
            let (lon, lat) = self.to_lonlat(x, y)?;
            out = LonLatBox { west: out.west.min(lon), south: out.south.min(lat), east: out.east.max(lon), north: out.north.max(lat) };
        }
        Ok(out)
    }

    /// EPSG:5070 rectangle enclosing a lon/lat box.
    pub fn albers_bounds(&self, b: &LonLatBox) -> Result<Bounds> {
        let mut out = Bounds { x_min: f64::MAX, y_min: f64::MAX, x_max: f64::MIN, y_max: f64::MIN };
        for (lon, lat) in edge_samples(b.west, b.south, b.east, b.north) {
            let (x, y) = self.from_lonlat(lon, lat)?;
            out = Bounds { x_min: out.x_min.min(x), y_min: out.y_min.min(y), x_max: out.x_max.max(x), y_max: out.y_max.max(y) };
        }
        Ok(out)
    }
}

/// Points along all four edges of an axis-aligned box, corners included.
fn edge_samples(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    let mut pts = Vec::with_capacity(4 * EDGE_SAMPLES);
    for k in 0..EDGE_SAMPLES {
        let t = k as f64 / (EDGE_SAMPLES - 1) as f64;
        let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        pts.extend([(x, y0), (x, y1), (x0, y), (x1, y)]);
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (lon, lat, x, y) generated with pyproj: Transformer.from_crs("EPSG:4269", "EPSG:5070", always_xy=True)
    const PYPROJ_FIXTURES: [(f64, f64, f64, f64); 5] = [
        (-120.8, 38.79, -2109889.7499, 2028245.4785),
        (-124.2, 41.75, -2294190.0346, 2425849.7967),
        (-114.6, 32.72, -1722432.8681, 1241150.5283),
        (-118.2437, 34.0522, -2019685.0673, 1458261.6851),
        (-96.0, 23.0, 0.0, 0.0),
    ];

    #[test]
    fn matches_pyproj_within_a_millimeter() {
        let a = Albers::new().unwrap();
        for (lon, lat, x, y) in PYPROJ_FIXTURES {
            let (px, py) = a.from_lonlat(lon, lat).unwrap();
            assert!((px - x).abs() < 1e-3 && (py - y).abs() < 1e-3, "({lon},{lat}) -> ({px},{py}), want ({x},{y})");
        }
    }

    #[test]
    fn round_trips() {
        let a = Albers::new().unwrap();
        for (lon, lat, _, _) in PYPROJ_FIXTURES {
            let (x, y) = a.from_lonlat(lon, lat).unwrap();
            let (lon2, lat2) = a.to_lonlat(x, y).unwrap();
            assert!((lon - lon2).abs() < 1e-9 && (lat - lat2).abs() < 1e-9);
        }
    }

    #[test]
    fn lonlat_bounds_enclose_the_whole_tile() {
        let a = Albers::new().unwrap();
        let (x, y) = a.from_lonlat(-120.8, 38.79).unwrap();
        let b = Bounds { x_min: x, y_min: y, x_max: x + 3750.0, y_max: y + 3750.0 };
        let ll = a.lonlat_bounds(&b).unwrap();
        for k in 0..=10 {
            for m in 0..=10 {
                let px = b.x_min + 375.0 * k as f64;
                let py = b.y_min + 375.0 * m as f64;
                let (lon, lat) = a.to_lonlat(px, py).unwrap();
                assert!(ll.padded(1e-9).contains(lon, lat), "({lon},{lat}) outside {ll:?}");
            }
        }
    }

    #[test]
    fn albers_bounds_enclose_the_lonlat_box() {
        let a = Albers::new().unwrap();
        let ll = LonLatBox { west: -124.5, south: 32.5, east: -114.1, north: 42.0 };
        let b = a.albers_bounds(&ll).unwrap();
        for (lon, lat) in [(-124.5, 42.0), (-114.1, 32.5), (-119.3, 42.0), (-119.3, 32.5)] {
            let (x, y) = a.from_lonlat(lon, lat).unwrap();
            assert!(x >= b.x_min && x <= b.x_max && y >= b.y_min && y <= b.y_max);
        }
    }
}
