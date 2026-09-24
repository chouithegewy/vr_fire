//! Region boundaries (GeoJSON lon/lat polygons) and the grid tiles that cover them.

use crate::crs::{Albers, LonLatBox};
use crate::grid::{GridSpec, TILE_SIZE_M, TileId};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

type Ring = Vec<(f64, f64)>;

pub struct Region {
    /// Each polygon is an outer ring followed by holes.
    polygons: Vec<Vec<Ring>>,
}

impl Region {
    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path).with_context(|| format!("read region {}", path.display()))?;
        Self::from_geojson(&s).with_context(|| format!("parse region {}", path.display()))
    }

    /// Accepts a FeatureCollection, Feature, Polygon or MultiPolygon.
    pub fn from_geojson(s: &str) -> Result<Self> {
        let v: Value = serde_json::from_str(s)?;
        let mut polygons = Vec::new();
        collect(&v, &mut polygons)?;
        if polygons.is_empty() {
            bail!("no Polygon or MultiPolygon geometry found");
        }
        Ok(Self { polygons })
    }

    /// Even-odd rule over each polygon's rings, so holes are excluded.
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        self.polygons
            .iter()
            .any(|rings| rings.iter().filter(|r| point_in_ring(r, lon, lat)).count() % 2 == 1)
    }

    pub fn bbox(&self) -> LonLatBox {
        let mut b = LonLatBox { west: f64::MAX, south: f64::MAX, east: f64::MIN, north: f64::MIN };
        for (lon, lat) in self.vertices() {
            b = LonLatBox { west: b.west.min(lon), south: b.south.min(lat), east: b.east.max(lon), north: b.north.max(lat) };
        }
        b
    }

    fn vertices(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.polygons.iter().flatten().flatten().copied()
    }
}

fn collect(v: &Value, out: &mut Vec<Vec<Ring>>) -> Result<()> {
    match v["type"].as_str() {
        Some("FeatureCollection") => {
            for f in v["features"].as_array().context("FeatureCollection without features")? {
                collect(f, out)?;
            }
        }
        Some("Feature") => collect(&v["geometry"], out)?,
        Some("Polygon") => out.push(parse_polygon(&v["coordinates"])?),
        Some("MultiPolygon") => {
            for p in v["coordinates"].as_array().context("MultiPolygon without coordinates")? {
                out.push(parse_polygon(p)?);
            }
        }
        other => bail!("unsupported GeoJSON type {other:?}"),
    }
    Ok(())
}

fn parse_polygon(v: &Value) -> Result<Vec<Ring>> {
    v.as_array()
        .context("polygon is not an array of rings")?
        .iter()
        .map(|ring| {
            ring.as_array()
                .context("ring is not an array")?
                .iter()
                .map(|p| {
                    let lon = p[0].as_f64().context("bad longitude")?;
                    let lat = p[1].as_f64().context("bad latitude")?;
                    Ok((lon, lat))
                })
                .collect()
        })
        .collect()
}

/// Ray casting (even-odd) test against one ring.
fn point_in_ring(ring: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut inside = false;
    let mut j = ring.len().wrapping_sub(1);
    for i in 0..ring.len() {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Tiles that intersect the region: any of a 5×5 probe lattice inside the region,
/// or any region vertex inside the tile (catches thin slivers and small islands).
pub fn tiles_in_region(grid: &GridSpec, albers: &Albers, region: &Region) -> Result<Vec<TileId>> {
    let candidates = grid.tiles_intersecting(&albers.albers_bounds(&region.bbox())?);
    let mut selected = BTreeSet::new();
    for t in candidates {
        let b = grid.tile_bounds(t);
        'probe: for sj in 0..=4 {
            for si in 0..=4 {
                let x = b.x_min + TILE_SIZE_M * si as f64 / 4.0;
                let y = b.y_max - TILE_SIZE_M * sj as f64 / 4.0;
                let (lon, lat) = albers.to_lonlat(x, y)?;
                if region.contains(lon, lat) {
                    selected.insert(t);
                    break 'probe;
                }
            }
        }
    }
    for (lon, lat) in region.vertices() {
        let (x, y) = albers.from_lonlat(lon, lat)?;
        selected.insert(grid.tile_containing(x, y));
    }
    Ok(selected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::GridSpec;

    const SQUARE: &str = r#"{"type":"Polygon","coordinates":[[[-121,38],[-120,38],[-120,39],[-121,39],[-121,38]]]}"#;

    #[test]
    fn polygon_contains() {
        let r = Region::from_geojson(SQUARE).unwrap();
        assert!(r.contains(-120.5, 38.5));
        assert!(!r.contains(-119.5, 38.5));
        assert_eq!(r.bbox(), LonLatBox { west: -121.0, south: 38.0, east: -120.0, north: 39.0 });
    }

    #[test]
    fn holes_are_excluded() {
        let r = Region::from_geojson(r#"{"type":"Polygon","coordinates":[
            [[0,0],[10,0],[10,10],[0,10],[0,0]],
            [[4,4],[6,4],[6,6],[4,6],[4,4]]]}"#).unwrap();
        assert!(r.contains(2.0, 2.0));
        assert!(!r.contains(5.0, 5.0));
    }

    #[test]
    fn feature_collection_of_multipolygons() {
        let r = Region::from_geojson(r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},
            "geometry":{"type":"MultiPolygon","coordinates":[
              [[[0,0],[1,0],[1,1],[0,1],[0,0]]],
              [[[5,5],[6,5],[6,6],[5,6],[5,5]]]]}}]}"#).unwrap();
        assert!(r.contains(0.5, 0.5));
        assert!(r.contains(5.5, 5.5));
        assert!(!r.contains(3.0, 3.0));
    }

    #[test]
    fn rejects_unsupported_geometry() {
        assert!(Region::from_geojson(r#"{"type":"Point","coordinates":[0,0]}"#).is_err());
    }

    #[test]
    fn small_region_selects_nearby_tiles_only() {
        let grid = GridSpec::default();
        let albers = Albers::new().unwrap();
        let r = Region::from_geojson(r#"{"type":"Polygon","coordinates":[[[-120.7,38.4],[-120.6,38.4],[-120.6,38.5],[-120.7,38.5],[-120.7,38.4]]]}"#).unwrap();
        let tiles = tiles_in_region(&grid, &albers, &r).unwrap();
        // 0.1° × 0.1° ≈ 8.7 km × 11.1 km → roughly 3–5 tiles per side.
        assert!((9..=30).contains(&tiles.len()), "{} tiles", tiles.len());
        let (x, y) = albers.from_lonlat(-120.65, 38.45).unwrap();
        assert!(tiles.contains(&grid.tile_containing(x, y)));
        let (x, y) = albers.from_lonlat(-120.4, 38.45).unwrap();
        assert!(!tiles.contains(&grid.tile_containing(x, y)));
    }

    #[test]
    fn california_boundary() {
        let r = Region::load(Path::new("data/regions/california.geojson")).unwrap();
        assert!(r.contains(-121.49, 38.58), "Sacramento");
        assert!(r.contains(-118.24, 34.05), "Los Angeles");
        assert!(!r.contains(-119.81, 39.53), "Reno, NV");
        assert!(!r.contains(-125.5, 37.0), "Pacific");
        let tiles = tiles_in_region(&GridSpec::default(), &Albers::new().unwrap(), &r).unwrap();
        // ~424,000 km² / 14.06 km² per tile ≈ 30k, plus boundary tiles.
        assert!((28_000..=38_000).contains(&tiles.len()), "{} tiles", tiles.len());
    }
}
