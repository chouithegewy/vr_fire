# Terrain Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A pure-Rust CLI (`vr_fire`) that bulk-ingests USGS 3DEP 10 m elevation for California into a tile store aligned to the ML team's 30 m EPSG:5070 grid, and bakes each 3,750 m tile into engine-agnostic `.glb` meshes (4 LODs) plus raw heights and metadata.

**Architecture:** Two stages over a fixed tile grid. `ingest` downloads 1°×1° 3DEP GeoTIFFs (resumable, cached), reprojects them onto each tile's 376×376 node grid (10 m) with `proj4rs` + bilinear sampling, and writes f32 GeoTIFF height tiles + `index.json` to `store/`. `bake` reads a height tile and its 4 neighbors, computes seamless normals, and writes glTF meshes at 10/30/150/750 m spacing with skirts, plus `.f32` heights and `.json` metadata to `tiles/`. All logic lives in `lib.rs` modules; `main.rs` is a thin `clap` CLI.

**Tech Stack:** Rust 2024 edition; `anyhow`, `clap` (derive), `proj4rs` 0.2, `tiff` 0.11, `ureq` 3, `rayon`, `serde`/`serde_json`; dev: `gltf` 1.4, `tempfile`.

**Spec:** `docs/superpowers/specs/2026-09-24-terrain-pipeline-design.md`

## Global Constraints

- Pure Rust dependencies only; no GDAL/PROJ/system libraries. `cargo build` must work on a clean machine with only a Rust toolchain.
- `cargo test` must not touch the network. Network tests are `#[ignore]` and run manually.
- Working CRS EPSG:5070 proj string (exact): `+proj=aea +lat_0=23 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +units=m +no_defs`
- Source geographic CRS NAD83 (EPSG:4269) proj string: `+proj=longlat +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +no_defs`. proj4rs works in radians for geographic coordinates.
- Grid origin (NW corner): x = −2,493,045, y = 3,310,005. Tile = 3,750 m. Nodes = 376 × 376 at 10 m, node-registered (`x_min + 10·i`, `y_max − 10·j`). `ty` increases southward.
- LOD strides over the 10 m node grid: `[1, 3, 15, 75]` → 10, 30, 150, 750 m.
- Source URL pattern: `https://prd-tnm.s3.amazonaws.com/StagedProducts/Elevation/13/TIFF/current/{name}/USGS_13_{name}.tif`, `name` = `n{lat_top:02}w{lon_west:03}` (e.g. `n39w121` covers lat 38–39, lon −121 to −120). Real files are ~460 MB, 10812², tiled, LZW, EPSG:4269, PixelIsArea, nodata `-999999`.
- Tile file stem is `{tx}_{ty}` everywhere (store, meshes, metadata).
- Output layout: `store/{tx}_{ty}.tif`, `store/index.json`, `tiles/lod{n}/{tx}_{ty}.glb`, `tiles/{tx}_{ty}.f32`, `tiles/{tx}_{ty}.json`.
- glTF axes: +X east, +Y elevation (m, NAVD88), +Z south (−Z north); origin at the tile's NW corner. u32 indices. Skirt depth 50 m.
- **Spec amendment (missing data):** nodes with no source data are set to **0.0 m (sea level)** and counted as `filled_samples`, instead of iterative neighbor averaging. Reason: 3DEP is void-filled on land, so missing data is almost always ocean or outside coverage, where sea level is correct; and a per-coordinate rule keeps shared tile edges bit-identical, whereas neighbor averaging depends on tile interiors and would create seams along the coast. A tile with no source data at all is `empty`.
- **Deferred:** the OpenTopography fallback (listed as optional in the spec) is not in this plan; `.env` stays untouched.

## Review Focus

1. **Tile straddling two 1° source cells** — heights must be continuous across the lon/lat boundary. Pinned by Task 6 `straddling_tile_is_seamless`.
2. **Coastal/ocean source cells (USGS returns 404)** — must become sea level, not a failure; fully-ocean tiles become `empty`. Pinned by Task 5 `missing_cell_is_remembered` and Task 6 `partially_covered_tile_fills_sea_level` / `tile_without_sources_is_empty`.
3. **Re-running after an interruption** — completed tiles are skipped and partial downloads resume. Pinned by Task 5 `download_resumes_after_truncation` and Task 8 `rerun_skips_completed_tiles`.
4. **Baking a tile whose neighbors are empty or absent** — one-sided normals, no panic. Pinned by Task 9 `normals_without_neighbors_use_one_sided_differences` and Task 11 `bake_edge_tile_without_neighbors`.
5. **Real 3DEP files exceed the `tiff` crate's default 256 MB decode limit** — reading must still succeed. Pinned by Task 4 `reads_rasters_larger_than_default_decoder_limit`.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Dependencies |
| `src/lib.rs` | Module declarations |
| `src/grid.rs` | Tile/node/cell math, `TileId`, `Bounds`, `GridSpec`, `TileRange` |
| `src/crs.rs` | EPSG:5070 ↔ NAD83 lon/lat (`Albers`), `LonLatBox` |
| `src/region.rs` | GeoJSON region polygons, point-in-polygon, `tiles_in_region` |
| `src/dem.rs` | `Raster`: GeoTIFF read/write, bilinear sampling |
| `src/source.rs` | `SourceCell` naming/coverage, `SourceCache` resumable download |
| `src/reproject.rs` | `Sources`, `resample_tile` → `TileHeights` |
| `src/store.rs` | Height-tile GeoTIFFs + `index.json` |
| `src/ingest.rs` | Ingest orchestration |
| `src/mesh.rs` | Normals, LOD grid mesh, skirts |
| `src/export.rs` | `.glb` writer, `.f32`, `TileMetadata` JSON |
| `src/bake.rs` | Bake orchestration |
| `src/main.rs` | CLI: `ingest`, `bake`, `locate` |
| `data/regions/california.geojson` | Simplified California boundary (Census TIGERweb) |
| `tests/common/mod.rs` | Synthetic 3DEP-like fixtures for integration tests |
| `tests/ingest.rs`, `tests/pipeline.rs` | Integration tests |
| `README.md` | Usage and conventions |

---

### Task 1: Scaffold and tile grid

**Files:**
- Modify: `Cargo.toml`, `src/main.rs`
- Create: `src/lib.rs`, `src/grid.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (`vr_fire::grid`):
  - consts `TILE_SIZE_M: f64 = 3750.0`, `NODE_SPACING_M: f64 = 10.0`, `NODES_PER_SIDE: usize = 376`, `CELL_SIZE_M: f64 = 30.0`, `CELLS_PER_TILE: i64 = 125`, `WINDOWS_PER_TILE: i64 = 10`, `LOD_STRIDES: [usize; 4] = [1, 3, 15, 75]`, `NLCD_ORIGIN_X`, `NLCD_ORIGIN_Y`
  - `struct GridSpec { origin_x: f64, origin_y: f64 }` (Default = NLCD origin) with `tile_bounds(&self, TileId) -> Bounds`, `node_xy(&self, TileId, i: usize, j: usize) -> (f64, f64)`, `tile_containing(&self, x: f64, y: f64) -> TileId`, `tiles_intersecting(&self, &Bounds) -> Vec<TileId>`, `cell_containing(&self, x: f64, y: f64) -> (i64, i64)`
  - `struct TileId { tx: i64, ty: i64 }` with `new`, `north`, `south`, `east`, `west`; `Display` = `"{tx}_{ty}"`; `FromStr` parses `"{tx}_{ty}"`
  - `struct Bounds { x_min, y_min, x_max, y_max: f64 }`
  - `fn cell_to_tile(c: i64, r: i64) -> (TileId, (u32, u32))`
  - `struct TileRange { min: TileId, max: TileId }` with `contains`, `tiles`; `FromStr` parses `"tx,ty"` or `"tx0,ty0..tx1,ty1"`

- [ ] **Step 1: Add dependencies and commit the existing scaffold**

The repo has one commit (the spec); `Cargo.toml`, `src/main.rs`, `.gitignore` are untracked. `.gitignore` already ignores `.env`, `/cache`, `/store`, `/tiles`, `/target`.

Run:
```bash
cargo add anyhow clap --features clap/derive
cargo add proj4rs@0.2 tiff@0.11 ureq@3 rayon serde --features serde/derive
cargo add serde_json
cargo add --dev gltf@1.4 tempfile
```

Replace `src/main.rs` with a placeholder that compiles (the real CLI arrives in Task 12):
```rust
fn main() {
    println!("vr_fire: see `cargo run -- --help` once the CLI lands");
}
```

Create `src/lib.rs`:
```rust
//! Terrain pipeline for the VR wildfire simulator: USGS 3DEP → EPSG:5070 height tiles → glTF meshes.

pub mod grid;
```

- [ ] **Step 2: Write the failing tests**

Create `src/grid.rs` with only the test module (so it fails to compile):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_tile_bounds() {
        let g = GridSpec::default();
        let b = g.tile_bounds(TileId::new(0, 0));
        assert_eq!(b.x_min, -2_493_045.0);
        assert_eq!(b.y_max, 3_310_005.0);
        assert_eq!(b.x_max - b.x_min, TILE_SIZE_M);
        assert_eq!(b.y_max - b.y_min, TILE_SIZE_M);
    }

    #[test]
    fn adjacent_tiles_share_edges_exactly() {
        let g = GridSpec::default();
        let t = TileId::new(812, 1440);
        assert_eq!(g.tile_bounds(t).x_max.to_bits(), g.tile_bounds(t.east()).x_min.to_bits());
        assert_eq!(g.tile_bounds(t).y_min.to_bits(), g.tile_bounds(t.south()).y_max.to_bits());
        for k in 0..NODES_PER_SIDE {
            let (ax, ay) = g.node_xy(t, NODES_PER_SIDE - 1, k);
            let (bx, by) = g.node_xy(t.east(), 0, k);
            assert_eq!((ax.to_bits(), ay.to_bits()), (bx.to_bits(), by.to_bits()));
            let (ax, ay) = g.node_xy(t, k, NODES_PER_SIDE - 1);
            let (bx, by) = g.node_xy(t.south(), k, 0);
            assert_eq!((ax.to_bits(), ay.to_bits()), (bx.to_bits(), by.to_bits()));
        }
    }

    #[test]
    fn every_third_node_is_an_nlcd_cell_corner() {
        // NLCD 30 m cell edges sit at 15 mod 30 in EPSG:5070.
        let g = GridSpec::default();
        let t = TileId::new(812, 1440);
        for k in (0..NODES_PER_SIDE).step_by(3) {
            let (x, y) = g.node_xy(t, k, k);
            assert_eq!(x.rem_euclid(CELL_SIZE_M), 15.0);
            assert_eq!(y.rem_euclid(CELL_SIZE_M), 15.0);
        }
    }

    #[test]
    fn tile_containing_round_trips_including_negative_ids() {
        let g = GridSpec::default();
        for t in [TileId::new(-3, -2), TileId::new(0, 0), TileId::new(812, 1440)] {
            let b = g.tile_bounds(t);
            assert_eq!(g.tile_containing((b.x_min + b.x_max) / 2.0, (b.y_min + b.y_max) / 2.0), t);
            assert_eq!(g.tile_containing(b.x_min, b.y_max), t, "NW corner belongs to the tile");
            let inner = Bounds { x_min: b.x_min + 1.0, y_min: b.y_min + 1.0, x_max: b.x_max - 1.0, y_max: b.y_max - 1.0 };
            assert_eq!(g.tiles_intersecting(&inner), vec![t]);
        }
    }

    #[test]
    fn tiles_intersecting_spans_rows_and_columns() {
        let g = GridSpec::default();
        let a = g.tile_bounds(TileId::new(10, 20));
        let z = g.tile_bounds(TileId::new(12, 21));
        let span = Bounds { x_min: a.x_min + 1.0, y_min: z.y_min + 1.0, x_max: z.x_max - 1.0, y_max: a.y_max - 1.0 };
        assert_eq!(g.tiles_intersecting(&span).len(), 3 * 2);
    }

    #[test]
    fn cells_map_to_tiles_with_euclidean_division() {
        assert_eq!(cell_to_tile(0, 0), (TileId::new(0, 0), (0, 0)));
        assert_eq!(cell_to_tile(125, 250), (TileId::new(1, 2), (0, 0)));
        assert_eq!(cell_to_tile(-1, 124), (TileId::new(-1, 0), (124, 124)));
        let g = GridSpec::default();
        let b = g.tile_bounds(TileId::new(1, 2));
        assert_eq!(g.cell_containing(b.x_min + 1.0, b.y_max - 1.0), (125, 250));
    }

    #[test]
    fn tile_id_display_and_parse() {
        let t = TileId::new(-3, 1440);
        assert_eq!(t.to_string(), "-3_1440");
        assert_eq!("-3_1440".parse::<TileId>().unwrap(), t);
        assert!("3,4".parse::<TileId>().is_err());
    }

    #[test]
    fn tile_range_parsing() {
        let r: TileRange = "812,1440..815,1443".parse().unwrap();
        assert_eq!(r.tiles().len(), 16);
        assert!(r.contains(TileId::new(813, 1441)));
        assert!(!r.contains(TileId::new(816, 1441)));
        let single: TileRange = "3,4".parse().unwrap();
        assert_eq!(single.tiles(), vec![TileId::new(3, 4)]);
        let reversed: TileRange = "5,6..3,4".parse().unwrap();
        assert_eq!(reversed.min, TileId::new(3, 4));
        assert!("garbage".parse::<TileRange>().is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib grid`
Expected: compile errors (`GridSpec`, `TileId`, … not found).

- [ ] **Step 4: Implement the grid**

Insert above the test module in `src/grid.rs`:
```rust
//! Fixed EPSG:5070 tile grid aligned to the NLCD/LANDFIRE 30 m grid.
//!
//! Tile (tx, ty) spans 3,750 m; tx grows east, ty grows south. Heights are sampled at
//! 376 × 376 nodes every 10 m, so adjacent tiles share (bit-identical) edge nodes and
//! every 30 m ML cell corner is a node.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const TILE_SIZE_M: f64 = 3750.0;
pub const NODE_SPACING_M: f64 = 10.0;
pub const NODES_PER_SIDE: usize = 376;
pub const CELL_SIZE_M: f64 = 30.0;
pub const CELLS_PER_TILE: i64 = 125;
/// 375 m ML windows per tile side, anchored at the tile origin (see spec open question 1).
pub const WINDOWS_PER_TILE: i64 = 10;
/// Node stride of each LOD over the 10 m grid: 10, 30, 150, 750 m.
pub const LOD_STRIDES: [usize; 4] = [1, 3, 15, 75];

/// NW corner of the NLCD CONUS raster; 30 m cell edges fall at 15 mod 30.
pub const NLCD_ORIGIN_X: f64 = -2_493_045.0;
pub const NLCD_ORIGIN_Y: f64 = 3_310_005.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridSpec {
    pub origin_x: f64,
    pub origin_y: f64,
}

impl Default for GridSpec {
    fn default() -> Self {
        Self { origin_x: NLCD_ORIGIN_X, origin_y: NLCD_ORIGIN_Y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileId {
    pub tx: i64,
    pub ty: i64,
}

impl TileId {
    pub fn new(tx: i64, ty: i64) -> Self {
        Self { tx, ty }
    }
    pub fn north(self) -> Self {
        Self::new(self.tx, self.ty - 1)
    }
    pub fn south(self) -> Self {
        Self::new(self.tx, self.ty + 1)
    }
    pub fn east(self) -> Self {
        Self::new(self.tx + 1, self.ty)
    }
    pub fn west(self) -> Self {
        Self::new(self.tx - 1, self.ty)
    }
}

impl fmt::Display for TileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.tx, self.ty)
    }
}

impl FromStr for TileId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (a, b) = s.split_once('_').ok_or_else(|| format!("expected 'tx_ty', got '{s}'"))?;
        let tx = a.parse().map_err(|_| format!("bad tx in '{s}'"))?;
        let ty = b.parse().map_err(|_| format!("bad ty in '{s}'"))?;
        Ok(Self::new(tx, ty))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
}

impl GridSpec {
    pub fn tile_bounds(&self, t: TileId) -> Bounds {
        let x_min = self.origin_x + TILE_SIZE_M * t.tx as f64;
        let y_max = self.origin_y - TILE_SIZE_M * t.ty as f64;
        Bounds { x_min, y_min: y_max - TILE_SIZE_M, x_max: x_min + TILE_SIZE_M, y_max }
    }

    /// EPSG:5070 coordinate of node (i, j) of a tile; i grows east, j grows south.
    pub fn node_xy(&self, t: TileId, i: usize, j: usize) -> (f64, f64) {
        let b = self.tile_bounds(t);
        (b.x_min + NODE_SPACING_M * i as f64, b.y_max - NODE_SPACING_M * j as f64)
    }

    pub fn tile_containing(&self, x: f64, y: f64) -> TileId {
        TileId::new(
            ((x - self.origin_x) / TILE_SIZE_M).floor() as i64,
            ((self.origin_y - y) / TILE_SIZE_M).floor() as i64,
        )
    }

    pub fn tiles_intersecting(&self, b: &Bounds) -> Vec<TileId> {
        let nw = self.tile_containing(b.x_min, b.y_max);
        let se = self.tile_containing(b.x_max, b.y_min);
        let mut out = Vec::new();
        for ty in nw.ty..=se.ty {
            for tx in nw.tx..=se.tx {
                out.push(TileId::new(tx, ty));
            }
        }
        out
    }

    /// Global 30 m ML cell (column east, row south, counted from the grid origin).
    pub fn cell_containing(&self, x: f64, y: f64) -> (i64, i64) {
        (
            ((x - self.origin_x) / CELL_SIZE_M).floor() as i64,
            ((self.origin_y - y) / CELL_SIZE_M).floor() as i64,
        )
    }
}

/// Global ML cell → (tile, cell within tile).
pub fn cell_to_tile(c: i64, r: i64) -> (TileId, (u32, u32)) {
    (
        TileId::new(c.div_euclid(CELLS_PER_TILE), r.div_euclid(CELLS_PER_TILE)),
        (c.rem_euclid(CELLS_PER_TILE) as u32, r.rem_euclid(CELLS_PER_TILE) as u32),
    )
}

/// Inclusive rectangle of tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRange {
    pub min: TileId,
    pub max: TileId,
}

impl TileRange {
    pub fn contains(&self, t: TileId) -> bool {
        (self.min.tx..=self.max.tx).contains(&t.tx) && (self.min.ty..=self.max.ty).contains(&t.ty)
    }

    pub fn tiles(&self) -> Vec<TileId> {
        let mut out = Vec::new();
        for ty in self.min.ty..=self.max.ty {
            for tx in self.min.tx..=self.max.tx {
                out.push(TileId::new(tx, ty));
            }
        }
        out
    }
}

impl FromStr for TileRange {
    type Err = String;
    /// "tx,ty" or "tx0,ty0..tx1,ty1" (corners in any order).
    fn from_str(s: &str) -> Result<Self, String> {
        fn corner(s: &str) -> Result<TileId, String> {
            let (a, b) = s.split_once(',').ok_or_else(|| format!("expected 'tx,ty', got '{s}'"))?;
            let tx = a.trim().parse().map_err(|_| format!("bad tx in '{s}'"))?;
            let ty = b.trim().parse().map_err(|_| format!("bad ty in '{s}'"))?;
            Ok(TileId::new(tx, ty))
        }
        let (a, b) = match s.split_once("..") {
            Some((a, b)) => (corner(a)?, corner(b)?),
            None => {
                let t = corner(s)?;
                (t, t)
            }
        };
        Ok(Self {
            min: TileId::new(a.tx.min(b.tx), a.ty.min(b.ty)),
            max: TileId::new(a.tx.max(b.tx), a.ty.max(b.ty)),
        })
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib grid`
Expected: 8 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/main.rs src/lib.rs src/grid.rs
git commit -m "feat: add EPSG:5070 tile grid aligned to NLCD 30 m cells"
```

---

### Task 2: CRS transforms

**Files:**
- Create: `src/crs.rs`
- Modify: `src/lib.rs` (add `pub mod crs;`)

**Interfaces:**
- Consumes: `grid::Bounds`.
- Produces (`vr_fire::crs`):
  - consts `EPSG_5070: &str`, `NAD83_GEOGRAPHIC: &str`
  - `struct LonLatBox { west, south, east, north: f64 }` with `padded(&self, deg: f64) -> LonLatBox`, `contains(&self, lon, lat) -> bool`
  - `struct Albers` (Send + Sync) with `new() -> Result<Albers>`, `to_lonlat(&self, x, y) -> Result<(f64, f64)>` (degrees), `from_lonlat(&self, lon, lat) -> Result<(f64, f64)>` (meters), `lonlat_bounds(&self, &Bounds) -> Result<LonLatBox>`, `albers_bounds(&self, &LonLatBox) -> Result<Bounds>`

- [ ] **Step 1: Write the failing tests**

Add `pub mod crs;` to `src/lib.rs`. Create `src/crs.rs`:
```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib crs`
Expected: compile errors (`Albers` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/crs.rs`:
```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib crs`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/crs.rs
git commit -m "feat: add EPSG:5070 <-> NAD83 transforms with pyproj-verified fixtures"
```

---

### Task 3: Region boundary and tile selection

**Files:**
- Create: `src/region.rs`, `data/regions/california.geojson`
- Modify: `src/lib.rs` (add `pub mod region;`)

**Interfaces:**
- Consumes: `grid::{GridSpec, TileId, TILE_SIZE_M}`, `crs::{Albers, LonLatBox}`.
- Produces (`vr_fire::region`):
  - `struct Region` with `load(&Path) -> Result<Region>`, `from_geojson(&str) -> Result<Region>`, `contains(&self, lon, lat) -> bool`, `bbox(&self) -> LonLatBox`
  - `fn tiles_in_region(&GridSpec, &Albers, &Region) -> Result<Vec<TileId>>` (sorted, unique)

- [ ] **Step 1: Download the California boundary**

```bash
mkdir -p data/regions
curl -sf -G "https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/State_County/MapServer/0/query" \
  --data-urlencode "where=STUSAB='CA'" --data-urlencode "outFields=NAME" \
  --data-urlencode "outSR=4326" --data-urlencode "maxAllowableOffset=0.005" \
  --data-urlencode "f=geojson" -o data/regions/california.geojson
head -c 200 data/regions/california.geojson
```
Expected: ~9.5 KB file starting with `{"type":"FeatureCollection"` and containing a `MultiPolygon` (mainland + Channel Islands).

- [ ] **Step 2: Write the failing tests**

Add `pub mod region;` to `src/lib.rs`. Create `src/region.rs`:
```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib region`
Expected: compile errors (`Region` not found).

- [ ] **Step 4: Implement**

Insert above the tests in `src/region.rs`:
```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib region`
Expected: 6 passed. If `california_boundary`'s tile count falls outside 28k–38k, print the count, sanity-check it against the state's area (~424,000 km² / 14.06 km²), and adjust the bounds only if the count is plausible.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/region.rs data/regions/california.geojson
git commit -m "feat: select grid tiles covering a GeoJSON region (California boundary)"
```

---

### Task 4: GeoTIFF rasters

**Files:**
- Create: `src/dem.rs`
- Modify: `src/lib.rs` (add `pub mod dem;`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces (`vr_fire::dem`):
  - consts `EPSG_NAD83_GEOGRAPHIC: u16 = 4269`, `EPSG_WGS84_GEOGRAPHIC: u16 = 4326`, `EPSG_CONUS_ALBERS: u16 = 5070`
  - `struct Raster { width: usize, height: usize, data: Vec<f32>, origin_x: f64, origin_y: f64, pixel_w: f64, pixel_h: f64, epsg: u16, nodata: Option<f32> }` — `origin_*` is the **outer top-left corner** of pixel (0,0); pixel centers are at `origin_x + (c + 0.5)·pixel_w`, `origin_y − (r + 0.5)·pixel_h`
  - `Raster::read_geotiff(&Path) -> Result<Raster>` (handles PixelIsArea and PixelIsPoint, no size limit)
  - `Raster::write_geotiff(&self, &Path, point_registered: bool) -> Result<()>` (Deflate)
  - `Raster::get(&self, col: i64, row: i64) -> Option<f32>` (None outside/nodata/NaN)
  - `Raster::sample_bilinear(&self, x: f64, y: f64) -> Option<f32>`

- [ ] **Step 1: Write the failing tests**

Add `pub mod dem;` to `src/lib.rs`. Create `src/dem.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: usize, height: usize, f: impl Fn(usize, usize) -> f32) -> Raster {
        Raster {
            width,
            height,
            data: (0..height).flat_map(|r| (0..width).map(move |c| (c, r))).map(|(c, r)| f(c, r)).collect(),
            origin_x: -121.0,
            origin_y: 39.0,
            pixel_w: 0.25,
            pixel_h: 0.25,
            epsg: EPSG_NAD83_GEOGRAPHIC,
            nodata: Some(-999999.0),
        }
    }

    #[test]
    fn round_trips_pixel_is_area() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.tif");
        let r = raster(4, 3, |c, r| (c * 10 + r) as f32);
        r.write_geotiff(&path, false).unwrap();
        assert_eq!(Raster::read_geotiff(&path).unwrap(), r);
    }

    #[test]
    fn round_trips_pixel_is_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.tif");
        let r = Raster { origin_x: -2_000_005.0, origin_y: 2_000_005.0, pixel_w: 10.0, pixel_h: 10.0, epsg: EPSG_CONUS_ALBERS, nodata: None, ..raster(5, 5, |c, r| (c + r) as f32) };
        r.write_geotiff(&path, true).unwrap();
        assert_eq!(Raster::read_geotiff(&path).unwrap(), r);
    }

    #[test]
    fn samples_pixel_centers_exactly_and_planes_bilinearly() {
        let r = raster(4, 4, |c, r| (100 * c + 7 * r) as f32);
        // Center of pixel (1, 2).
        assert_eq!(r.sample_bilinear(-121.0 + 1.5 * 0.25, 39.0 - 2.5 * 0.25), Some(114.0));
        // Midway between the centers of (1,1),(2,1),(1,2),(2,2).
        let v = r.sample_bilinear(-121.0 + 2.0 * 0.25, 39.0 - 2.0 * 0.25).unwrap();
        assert!((v - 160.5).abs() < 1e-4, "{v}"); // mean of 107, 207, 114, 214
    }

    #[test]
    fn nodata_neighbors_are_dropped_and_renormalized() {
        let mut r = raster(2, 2, |_, _| 10.0);
        r.data[3] = -999999.0;
        let v = r.sample_bilinear(-121.0 + 0.25, 39.0 - 0.25).unwrap();
        assert_eq!(v, 10.0);
        let all_nodata = raster(2, 2, |_, _| -999999.0);
        assert_eq!(all_nodata.sample_bilinear(-121.0 + 0.25, 39.0 - 0.25), None);
    }

    #[test]
    fn outside_the_raster_is_none() {
        let r = raster(4, 4, |_, _| 1.0);
        assert_eq!(r.sample_bilinear(-125.0, 39.0), None);
        assert_eq!(r.get(-1, 0), None);
        assert_eq!(r.get(0, 4), None);
    }

    #[test]
    fn reads_rasters_larger_than_default_decoder_limit() {
        // Real 3DEP tiles decode to ~467 MB; the tiff crate's default limit is 256 MB.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.tif");
        let side = 8200; // 8200² × 4 B ≈ 269 MB decoded; zeros compress to almost nothing.
        let r = raster(side, side, |_, _| 0.0);
        r.write_geotiff(&path, false).unwrap();
        let back = Raster::read_geotiff(&path).unwrap();
        assert_eq!((back.width, back.height), (side, side));
    }
}
```
- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib dem`
Expected: compile errors (`Raster` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/dem.rs`:
```rust
//! Single-band f32 GeoTIFF rasters: read (3DEP sources, store tiles), write, bilinear sampling.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype::Gray32Float};
use tiff::tags::Tag;

pub const EPSG_NAD83_GEOGRAPHIC: u16 = 4269;
pub const EPSG_WGS84_GEOGRAPHIC: u16 = 4326;
pub const EPSG_CONUS_ALBERS: u16 = 5070;

// GeoKey IDs and values from the GeoTIFF 1.0 spec.
const GT_MODEL_TYPE: u16 = 1024;
const GT_RASTER_TYPE: u16 = 1025;
const GEOGRAPHIC_TYPE: u16 = 2048;
const PROJECTED_CS_TYPE: u16 = 3072;
const MODEL_TYPE_PROJECTED: u16 = 1;
const MODEL_TYPE_GEOGRAPHIC: u16 = 2;
const RASTER_PIXEL_IS_AREA: u16 = 1;
const RASTER_PIXEL_IS_POINT: u16 = 2;

/// `origin_x`/`origin_y` is the outer top-left corner of pixel (0, 0), in CRS units.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub origin_x: f64,
    pub origin_y: f64,
    pub pixel_w: f64,
    pub pixel_h: f64,
    pub epsg: u16,
    pub nodata: Option<f32>,
}

impl Raster {
    pub fn read_geotiff(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut dec = Decoder::new(BufReader::new(file))
            .with_context(|| format!("not a TIFF: {}", path.display()))?
            .with_limits(Limits::unlimited());
        let (width, height) = dec.dimensions()?;
        let scale = dec.get_tag_f64_vec(Tag::ModelPixelScaleTag).context("missing ModelPixelScaleTag")?;
        let tie = dec.get_tag_f64_vec(Tag::ModelTiepointTag).context("missing ModelTiepointTag")?;
        let keys = dec.get_tag_u16_vec(Tag::GeoKeyDirectoryTag).context("missing GeoKeyDirectoryTag")?;
        let nodata = dec
            .get_tag_ascii_string(Tag::GdalNodata)
            .ok()
            .map(|s| s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).parse::<f32>())
            .transpose()
            .context("unparseable GDAL_NODATA")?;
        let epsg = geokey(&keys, PROJECTED_CS_TYPE)
            .or_else(|| geokey(&keys, GEOGRAPHIC_TYPE))
            .context("GeoKeys carry no EPSG code")?;
        let half = if geokey(&keys, GT_RASTER_TYPE) == Some(RASTER_PIXEL_IS_POINT) { 0.5 } else { 0.0 };
        let data = match dec.read_image().with_context(|| format!("decode {}", path.display()))? {
            DecodingResult::F32(v) => v,
            _ => bail!("{}: only 32-bit float rasters are supported", path.display()),
        };
        let (pixel_w, pixel_h) = (scale[0], scale[1]);
        let (i, j, x, y) = (tie[0], tie[1], tie[3], tie[4]);
        Ok(Self {
            width: width as usize,
            height: height as usize,
            data,
            origin_x: x - (i + half) * pixel_w,
            origin_y: y + (j + half) * pixel_h,
            pixel_w,
            pixel_h,
            epsg,
            nodata,
        })
    }

    /// Deflate-compressed GeoTIFF. `point_registered` marks samples as grid nodes (PixelIsPoint).
    pub fn write_geotiff(&self, path: &Path, point_registered: bool) -> Result<()> {
        let mut out = BufWriter::new(File::create(path).with_context(|| format!("create {}", path.display()))?);
        {
            let mut enc = TiffEncoder::new(&mut out)?.with_compression(Compression::Deflate(DeflateLevel::Balanced));
            let mut img = enc.new_image::<Gray32Float>(self.width as u32, self.height as u32)?;
            let half = if point_registered { 0.5 } else { 0.0 };
            let geographic = matches!(self.epsg, EPSG_NAD83_GEOGRAPHIC | EPSG_WGS84_GEOGRAPHIC);
            let (model_type, cs_key) =
                if geographic { (MODEL_TYPE_GEOGRAPHIC, GEOGRAPHIC_TYPE) } else { (MODEL_TYPE_PROJECTED, PROJECTED_CS_TYPE) };
            let raster_type = if point_registered { RASTER_PIXEL_IS_POINT } else { RASTER_PIXEL_IS_AREA };
            img.encoder().write_tag(Tag::ModelPixelScaleTag, &[self.pixel_w, self.pixel_h, 0.0][..])?;
            img.encoder().write_tag(
                Tag::ModelTiepointTag,
                &[0.0, 0.0, 0.0, self.origin_x + half * self.pixel_w, self.origin_y - half * self.pixel_h, 0.0][..],
            )?;
            img.encoder().write_tag(
                Tag::GeoKeyDirectoryTag,
                &[1u16, 1, 0, 3, GT_MODEL_TYPE, 0, 1, model_type, GT_RASTER_TYPE, 0, 1, raster_type, cs_key, 0, 1, self.epsg][..],
            )?;
            if let Some(nd) = self.nodata {
                img.encoder().write_tag(Tag::GdalNodata, nd.to_string().as_str())?;
            }
            img.write_data(&self.data)?;
        }
        out.flush()?;
        Ok(())
    }

    /// Value at (col, row); None outside the raster, at nodata, or NaN.
    pub fn get(&self, col: i64, row: i64) -> Option<f32> {
        if col < 0 || row < 0 || col >= self.width as i64 || row >= self.height as i64 {
            return None;
        }
        let v = self.data[row as usize * self.width + col as usize];
        if v.is_nan() || Some(v) == self.nodata { None } else { Some(v) }
    }

    /// Bilinear sample at CRS coordinate (x, y). Missing neighbors are dropped and the
    /// remaining weights renormalized; None when no neighbor has data.
    pub fn sample_bilinear(&self, x: f64, y: f64) -> Option<f32> {
        let fx = (x - self.origin_x) / self.pixel_w - 0.5;
        let fy = (self.origin_y - y) / self.pixel_h - 0.5;
        let (c0, r0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - c0, fy - r0);
        let (c0, r0) = (c0 as i64, r0 as i64);
        let (mut sum, mut wsum) = (0.0f64, 0.0f64);
        for (dc, dr, w) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            if w == 0.0 {
                continue;
            }
            if let Some(v) = self.get(c0 + dc, r0 + dr) {
                sum += w * v as f64;
                wsum += w;
            }
        }
        (wsum > 0.0).then(|| (sum / wsum) as f32)
    }
}

/// Value of a short GeoKey stored inline in the directory (TIFFTagLocation 0).
fn geokey(keys: &[u16], id: u16) -> Option<u16> {
    keys.chunks_exact(4).skip(1).find(|k| k[0] == id && k[1] == 0).map(|k| k[3])
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib dem`
Expected: 6 passed (`reads_rasters_larger_than_default_decoder_limit` takes a few seconds in debug; run `cargo test --release --lib dem` if it is slow).

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/dem.rs
git commit -m "feat: read/write GeoTIFF rasters with bilinear nodata-aware sampling"
```

---

### Task 5: 3DEP source cells and resumable cache

**Files:**
- Create: `src/source.rs`
- Modify: `src/lib.rs` (add `pub mod source;`)

**Interfaces:**
- Consumes: `crs::LonLatBox`; `dem::Raster` (tests only, to build a valid TIFF body).
- Produces (`vr_fire::source`):
  - const `USGS_13_BASE_URL: &str`
  - `struct SourceCell { north: i32, west: i32 }` (Copy, Ord, Hash) with `containing(lon, lat) -> SourceCell`, `name() -> String` (`"n39w121"`), `file_name() -> String`, `url(&self, base: &str) -> String`, `covering(&LonLatBox) -> Vec<SourceCell>`
  - `enum Fetched { Present(PathBuf), Missing }`
  - `struct SourceCache` with `new(dir, base_url) -> SourceCache`, `with_retry_delay(Duration) -> SourceCache`, `path(&self, SourceCell) -> PathBuf`, `ensure(&self, SourceCell) -> Result<Fetched>`, `invalidate(&self, SourceCell) -> Result<()>`, `prefetch(&self, &[SourceCell], threads: usize) -> Vec<(SourceCell, Result<Fetched>)>`

- [ ] **Step 1: Write the failing tests**

Add `pub mod source;` to `src/lib.rs`. Create `src/source.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem::{EPSG_NAD83_GEOGRAPHIC, Raster};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MISSING: SourceCell = SourceCell { north: 10, west: 10 };

    /// Minimal HTTP server: honors `Range: bytes=N-`, 404s for the MISSING cell,
    /// and optionally cuts the first response off halfway through the body.
    fn serve(body: Vec<u8>, truncate_first: bool) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut path, mut start) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if path.is_empty() {
                        path = line.split(' ').nth(1).unwrap_or("").to_string();
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        start = v.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                if path.contains(&MISSING.name()) {
                    let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let rest = &body[start..];
                let head = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        start, body.len() - 1, body.len(), rest.len()
                    )
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len())
                };
                let _ = stream.write_all(head.as_bytes());
                let send = if truncate_first && n == 0 { &rest[..rest.len() / 2] } else { rest };
                let _ = stream.write_all(send);
            }
        });
        (url, requests)
    }

    /// Bytes of a small but valid GeoTIFF whose data doesn't compress away.
    fn tiff_bytes() -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.tif");
        Raster {
            width: 64,
            height: 64,
            data: (0..64 * 64).map(|i| ((i * 7919) % 1000) as f32).collect(),
            origin_x: -121.0,
            origin_y: 39.0,
            pixel_w: 1.0 / 64.0,
            pixel_h: 1.0 / 64.0,
            epsg: EPSG_NAD83_GEOGRAPHIC,
            nodata: None,
        }
        .write_geotiff(&path, false)
        .unwrap();
        std::fs::read(path).unwrap()
    }

    fn cache(dir: &Path, url: &str) -> SourceCache {
        SourceCache::new(dir, url).with_retry_delay(Duration::ZERO)
    }

    #[test]
    fn cell_naming_matches_usgs() {
        let c = SourceCell::containing(-120.8, 38.79);
        assert_eq!(c, SourceCell { north: 39, west: 121 });
        assert_eq!(
            c.url(USGS_13_BASE_URL),
            "https://prd-tnm.s3.amazonaws.com/StagedProducts/Elevation/13/TIFF/current/n39w121/USGS_13_n39w121.tif"
        );
        // Cells are half-open: the south and west edges belong to the cell.
        assert_eq!(SourceCell::containing(-120.0, 38.0), SourceCell { north: 39, west: 120 });
    }

    #[test]
    fn covering_spans_all_cells() {
        let cells = SourceCell::covering(&LonLatBox { west: -121.2, south: 38.9, east: -120.9, north: 39.1 });
        assert_eq!(cells.len(), 4);
        assert!(cells.contains(&SourceCell { north: 40, west: 122 }));
        assert!(cells.contains(&SourceCell { north: 39, west: 121 }));
    }

    #[test]
    fn downloads_then_serves_from_cache() {
        let body = tiff_bytes();
        let (url, requests) = serve(body.clone(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cell = SourceCell { north: 39, west: 121 };
        assert_eq!(c.ensure(cell).unwrap(), Fetched::Present(c.path(cell)));
        assert_eq!(std::fs::read(c.path(cell)).unwrap(), body);
        c.ensure(cell).unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1, "second ensure must hit the cache");
    }

    #[test]
    fn download_resumes_after_truncation() {
        let body = tiff_bytes();
        let (url, requests) = serve(body.clone(), true);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cell = SourceCell { north: 39, west: 121 };
        assert_eq!(c.ensure(cell).unwrap(), Fetched::Present(c.path(cell)));
        assert_eq!(std::fs::read(c.path(cell)).unwrap(), body);
        assert_eq!(requests.load(Ordering::SeqCst), 2, "one truncated + one ranged request");
    }

    #[test]
    fn missing_cell_is_remembered() {
        let (url, requests) = serve(tiff_bytes(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        assert_eq!(c.ensure(MISSING).unwrap(), Fetched::Missing);
        assert_eq!(c.ensure(MISSING).unwrap(), Fetched::Missing);
        assert_eq!(requests.load(Ordering::SeqCst), 1, "404 is cached as a .missing marker");
    }

    #[test]
    fn corrupt_cache_entry_is_redownloaded() {
        let body = tiff_bytes();
        let (url, _) = serve(body.clone(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cell = SourceCell { north: 39, west: 121 };
        std::fs::write(c.path(cell), b"not a tiff").unwrap();
        c.ensure(cell).unwrap();
        assert_eq!(std::fs::read(c.path(cell)).unwrap(), body);
    }

    #[test]
    fn unreachable_server_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), "http://127.0.0.1:9");
        let err = c.ensure(SourceCell { north: 39, west: 121 }).unwrap_err();
        assert!(format!("{err:#}").contains("after 5 attempts"), "{err:#}");
    }

    #[test]
    fn prefetch_reports_every_cell() {
        let (url, _) = serve(tiff_bytes(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cells = [SourceCell { north: 39, west: 121 }, SourceCell { north: 39, west: 120 }, MISSING];
        let results = c.prefetch(&cells, 2);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, r)| r.is_ok()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib source`
Expected: compile errors (`SourceCell` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/source.rs`:
```rust
//! USGS 3DEP 1/3 arc-second (~10 m) seamless DEM, published as 1°×1° GeoTIFFs on S3:
//! cell naming, coverage, and a resumable on-disk download cache.

use crate::crs::LonLatBox;
use anyhow::{Result, anyhow, bail};
use rayon::prelude::*;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tiff::decoder::Decoder;

pub const USGS_13_BASE_URL: &str = "https://prd-tnm.s3.amazonaws.com/StagedProducts/Elevation/13/TIFF/current";
const MAX_ATTEMPTS: u32 = 5;

/// A 1°×1° cell named by its NW corner: `n39w121` covers lat [38, 39), lon [−121, −120).
/// Northern/western hemispheres only (CONUS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceCell {
    pub north: i32,
    pub west: i32,
}

impl SourceCell {
    pub fn containing(lon: f64, lat: f64) -> Self {
        Self { north: lat.floor() as i32 + 1, west: (-lon).ceil() as i32 }
    }

    pub fn name(&self) -> String {
        format!("n{:02}w{:03}", self.north, self.west)
    }

    pub fn file_name(&self) -> String {
        format!("USGS_13_{}.tif", self.name())
    }

    pub fn url(&self, base: &str) -> String {
        format!("{base}/{}/{}", self.name(), self.file_name())
    }

    pub fn covering(b: &LonLatBox) -> Vec<Self> {
        let nw = Self::containing(b.west, b.north);
        let se = Self::containing(b.east, b.south);
        let mut out = Vec::new();
        for north in (se.north..=nw.north).rev() {
            for west in (se.west..=nw.west).rev() {
                out.push(Self { north, west });
            }
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Present(PathBuf),
    /// USGS has no file for this cell (open ocean or outside coverage).
    Missing,
}

enum Download {
    Complete,
    NotFound,
}

pub struct SourceCache {
    dir: PathBuf,
    base_url: String,
    retry_delay: Duration,
}

impl SourceCache {
    pub fn new(dir: impl Into<PathBuf>, base_url: impl Into<String>) -> Self {
        Self { dir: dir.into(), base_url: base_url.into(), retry_delay: Duration::from_secs(2) }
    }

    /// Base delay between retries; attempt k waits k × delay.
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    pub fn path(&self, c: SourceCell) -> PathBuf {
        self.dir.join(c.file_name())
    }

    fn missing_marker(&self, c: SourceCell) -> PathBuf {
        self.dir.join(format!("{}.missing", c.file_name()))
    }

    /// Make sure the cell's file is cached, downloading (or resuming) it if needed.
    pub fn ensure(&self, c: SourceCell) -> Result<Fetched> {
        fs::create_dir_all(&self.dir)?;
        if self.missing_marker(c).exists() {
            return Ok(Fetched::Missing);
        }
        let path = self.path(c);
        if path.exists() {
            if is_readable_tiff(&path) {
                return Ok(Fetched::Present(path));
            }
            fs::remove_file(&path)?; // corrupt cache entry: download it again
        }
        match self.download(&c.url(&self.base_url), &path)? {
            Download::NotFound => {
                fs::write(self.missing_marker(c), b"")?;
                Ok(Fetched::Missing)
            }
            Download::Complete if is_readable_tiff(&path) => Ok(Fetched::Present(path)),
            Download::Complete => {
                fs::remove_file(&path)?;
                bail!("downloaded {} is not a readable TIFF", path.display())
            }
        }
    }

    /// Drop a cached file (e.g. it failed to decode) so the next `ensure` downloads it again.
    pub fn invalidate(&self, c: SourceCell) -> Result<()> {
        match fs::remove_file(self.path(c)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// `ensure` every cell, `threads` downloads at a time.
    pub fn prefetch(&self, cells: &[SourceCell], threads: usize) -> Vec<(SourceCell, Result<Fetched>)> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build().expect("thread pool");
        pool.install(|| cells.par_iter().map(|&c| (c, self.ensure(c))).collect())
    }

    fn download(&self, url: &str, dest: &Path) -> Result<Download> {
        let part = dest.with_extension("tif.part");
        let mut last_err = anyhow!("no attempts made");
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(self.retry_delay * attempt);
            }
            let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            let mut req = ureq::get(url);
            if have > 0 {
                req = req.header("Range", format!("bytes={have}-"));
            }
            let resp = match req.call() {
                Ok(r) => r,
                Err(ureq::Error::StatusCode(404)) => return Ok(Download::NotFound),
                Err(ureq::Error::StatusCode(416)) if have > 0 => {
                    // Range starts at EOF: the partial file is already complete.
                    fs::rename(&part, dest)?;
                    return Ok(Download::Complete);
                }
                Err(e) => {
                    last_err = anyhow!(e);
                    continue;
                }
            };
            let mut file = if resp.status().as_u16() == 206 {
                OpenOptions::new().append(true).open(&part)?
            } else {
                File::create(&part)?
            };
            match io::copy(&mut resp.into_body().into_reader(), &mut file) {
                Ok(_) => {
                    file.sync_all()?;
                    drop(file);
                    fs::rename(&part, dest)?;
                    return Ok(Download::Complete);
                }
                Err(e) => last_err = anyhow!(e).context("download interrupted"),
            }
        }
        Err(last_err.context(format!("failed to download {url} after {MAX_ATTEMPTS} attempts")))
    }
}

/// Cheap validity check: the TIFF header and first IFD parse. Full decoding happens at ingest.
fn is_readable_tiff(path: &Path) -> bool {
    File::open(path)
        .ok()
        .and_then(|f| Decoder::new(BufReader::new(f)).ok())
        .and_then(|mut d| d.dimensions().ok())
        .is_some()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib source`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/source.rs
git commit -m "feat: resumable cached download of USGS 3DEP 1-degree source tiles"
```

---

### Task 6: Reprojection onto tile nodes

**Files:**
- Create: `src/reproject.rs`
- Modify: `src/lib.rs` (add `pub mod reproject;`)

**Interfaces:**
- Consumes: `grid::{GridSpec, TileId, NODES_PER_SIDE}`, `crs::Albers`, `dem::Raster`, `source::SourceCell`.
- Produces (`vr_fire::reproject`):
  - consts `SEA_LEVEL_M: f32 = 0.0`, `SOURCE_MARGIN_DEG: f64 = 0.002`
  - `struct Sources` (Default) with `insert(&mut self, SourceCell, Arc<Raster>)`, `remove(&mut self, SourceCell)`, `contains(&self, SourceCell) -> bool`, `cells(&self) -> Vec<SourceCell>`, `len(&self) -> usize`, `sample(&self, lon, lat) -> Option<f32>`
  - `struct TileHeights { tile: TileId, data: Vec<f32>, filled: u32 }` (`data` is 376×376 row-major, north row first)
  - `fn source_cells_for_tile(&GridSpec, &Albers, TileId) -> Result<Vec<SourceCell>>`
  - `fn resample_tile(&GridSpec, &Albers, &Sources, TileId) -> Result<Option<TileHeights>>` (None = empty)
  - `fn min_max(&[f32]) -> (f32, f32)`

- [ ] **Step 1: Write the failing tests**

Add `pub mod reproject;` to `src/lib.rs`. Create `src/reproject.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem::EPSG_NAD83_GEOGRAPHIC;

    const PX: f64 = 0.01;

    fn plane(lon: f64, lat: f64) -> f64 {
        1000.0 + 100.0 * (lon + 121.0) + 50.0 * (lat - 38.0)
    }

    /// Like a real 3DEP file: the 1° cell plus a one-pixel overlap buffer on every side.
    fn plane_raster(cell: SourceCell) -> Arc<Raster> {
        let n = (1.0 / PX).round() as usize + 2;
        let origin_x = -(cell.west as f64) - PX;
        let origin_y = cell.north as f64 + PX;
        let mut data = Vec::with_capacity(n * n);
        for r in 0..n {
            for c in 0..n {
                data.push(plane(origin_x + (c as f64 + 0.5) * PX, origin_y - (r as f64 + 0.5) * PX) as f32);
            }
        }
        Arc::new(Raster { width: n, height: n, data, origin_x, origin_y, pixel_w: PX, pixel_h: PX, epsg: EPSG_NAD83_GEOGRAPHIC, nodata: None })
    }

    fn tile_at(lon: f64, lat: f64) -> (GridSpec, Albers, TileId) {
        let grid = GridSpec::default();
        let albers = Albers::new().unwrap();
        let (x, y) = albers.from_lonlat(lon, lat).unwrap();
        let t = grid.tile_containing(x, y);
        (grid, albers, t)
    }

    fn assert_matches_plane(grid: &GridSpec, albers: &Albers, h: &TileHeights) {
        for j in (0..NODES_PER_SIDE).step_by(25) {
            for i in (0..NODES_PER_SIDE).step_by(25) {
                let (x, y) = grid.node_xy(h.tile, i, j);
                let (lon, lat) = albers.to_lonlat(x, y).unwrap();
                let got = h.data[j * NODES_PER_SIDE + i];
                assert!((got as f64 - plane(lon, lat)).abs() < 1e-2, "node ({i},{j}): {got} vs {}", plane(lon, lat));
            }
        }
    }

    #[test]
    fn resamples_a_plane_exactly() {
        let (grid, albers, t) = tile_at(-120.5, 38.5);
        let mut s = Sources::default();
        let cell = SourceCell { north: 39, west: 121 };
        s.insert(cell, plane_raster(cell));
        let h = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        assert_eq!(h.data.len(), NODES_PER_SIDE * NODES_PER_SIDE);
        assert_eq!(h.filled, 0);
        assert_matches_plane(&grid, &albers, &h);
    }

    #[test]
    fn straddling_tile_is_seamless() {
        let (grid, albers, t) = tile_at(-120.0, 38.5);
        let cells = source_cells_for_tile(&grid, &albers, t).unwrap();
        assert!(cells.contains(&SourceCell { north: 39, west: 121 }) && cells.contains(&SourceCell { north: 39, west: 120 }));
        let mut s = Sources::default();
        for c in cells {
            s.insert(c, plane_raster(c));
        }
        let h = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        assert_eq!(h.filled, 0);
        assert_matches_plane(&grid, &albers, &h);
    }

    #[test]
    fn partially_covered_tile_fills_sea_level() {
        let (grid, albers, t) = tile_at(-120.0, 38.5);
        let mut s = Sources::default();
        let west = SourceCell { north: 39, west: 121 };
        s.insert(west, plane_raster(west)); // the eastern cell is "ocean"
        let h = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        assert!(h.filled > 0 && (h.filled as usize) < NODES_PER_SIDE * NODES_PER_SIDE);
        assert!(h.data.contains(&SEA_LEVEL_M));
    }

    #[test]
    fn tile_without_sources_is_empty() {
        let (grid, albers, t) = tile_at(-120.5, 38.5);
        assert!(resample_tile(&grid, &albers, &Sources::default(), t).unwrap().is_none());
    }

    #[test]
    fn adjacent_tiles_have_bit_identical_edges() {
        let (grid, albers, t) = tile_at(-120.5, 38.5);
        let mut s = Sources::default();
        let cell = SourceCell { north: 39, west: 121 };
        s.insert(cell, plane_raster(cell));
        let a = resample_tile(&grid, &albers, &s, t).unwrap().unwrap();
        let b = resample_tile(&grid, &albers, &s, t.east()).unwrap().unwrap();
        let n = NODES_PER_SIDE;
        for j in 0..n {
            assert_eq!(a.data[j * n + n - 1].to_bits(), b.data[j * n].to_bits(), "row {j}");
        }
    }

    #[test]
    fn min_max_of_heights() {
        assert_eq!(min_max(&[3.0, -1.5, 7.25]), (-1.5, 7.25));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib reproject`
Expected: compile errors (`Sources` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/reproject.rs`:
```rust
//! Resample 3DEP source rasters (NAD83 lon/lat) onto a tile's EPSG:5070 node grid.

use crate::crs::Albers;
use crate::dem::Raster;
use crate::grid::{GridSpec, NODES_PER_SIDE, TileId};
use crate::source::SourceCell;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Height given to nodes with no source data (3DEP is void-filled on land, so this is ocean).
pub const SEA_LEVEL_M: f32 = 0.0;
/// Margin around a tile's lon/lat box when choosing source cells (~200 m).
pub const SOURCE_MARGIN_DEG: f64 = 0.002;

/// Loaded source rasters by cell. Cells absent from the map have no data (ocean).
#[derive(Default)]
pub struct Sources {
    rasters: HashMap<SourceCell, Arc<Raster>>,
}

impl Sources {
    pub fn insert(&mut self, c: SourceCell, r: Arc<Raster>) {
        self.rasters.insert(c, r);
    }
    pub fn remove(&mut self, c: SourceCell) {
        self.rasters.remove(&c);
    }
    pub fn contains(&self, c: SourceCell) -> bool {
        self.rasters.contains_key(&c)
    }
    pub fn cells(&self) -> Vec<SourceCell> {
        self.rasters.keys().copied().collect()
    }
    pub fn len(&self) -> usize {
        self.rasters.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rasters.is_empty()
    }

    /// Always samples the raster of the cell containing the point, so a given coordinate
    /// maps to the same value no matter which tile asks (keeps shared edges identical).
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f32> {
        self.rasters.get(&SourceCell::containing(lon, lat))?.sample_bilinear(lon, lat)
    }
}

pub struct TileHeights {
    pub tile: TileId,
    /// 376 × 376 heights (m, NAVD88), row-major, north row first.
    pub data: Vec<f32>,
    /// Nodes set to sea level because no source had data.
    pub filled: u32,
}

pub fn source_cells_for_tile(grid: &GridSpec, albers: &Albers, tile: TileId) -> Result<Vec<SourceCell>> {
    let ll = albers.lonlat_bounds(&grid.tile_bounds(tile))?.padded(SOURCE_MARGIN_DEG);
    Ok(SourceCell::covering(&ll))
}

/// Resample one tile. Returns None when no node has source data (tile is empty).
pub fn resample_tile(grid: &GridSpec, albers: &Albers, sources: &Sources, tile: TileId) -> Result<Option<TileHeights>> {
    let n = NODES_PER_SIDE;
    let mut data = Vec::with_capacity(n * n);
    let mut filled = 0u32;
    for j in 0..n {
        for i in 0..n {
            let (x, y) = grid.node_xy(tile, i, j);
            let (lon, lat) = albers.to_lonlat(x, y)?;
            match sources.sample(lon, lat) {
                Some(h) => data.push(h),
                None => {
                    data.push(SEA_LEVEL_M);
                    filled += 1;
                }
            }
        }
    }
    if filled as usize == n * n {
        return Ok(None);
    }
    Ok(Some(TileHeights { tile, data, filled }))
}

pub fn min_max(heights: &[f32]) -> (f32, f32) {
    heights.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &h| (lo.min(h), hi.max(h)))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib reproject`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/reproject.rs
git commit -m "feat: resample 3DEP sources onto EPSG:5070 tile nodes"
```

---

### Task 7: Height-tile store

**Files:**
- Create: `src/store.rs`
- Modify: `src/lib.rs` (add `pub mod store;`)

**Interfaces:**
- Consumes: `grid::{GridSpec, TileId, NODES_PER_SIDE, NODE_SPACING_M}`, `dem::{Raster, EPSG_CONUS_ALBERS}`, `reproject::TileHeights`.
- Produces (`vr_fire::store`):
  - `enum TileStatus { Ok, Empty, Failed }` (serde lowercase)
  - `struct TileEntry { status: TileStatus, error: Option<String>, min_elevation_m: Option<f32>, max_elevation_m: Option<f32>, filled_samples: u32, sources: Vec<String> }`
  - `struct StoreIndex { grid: GridSpec, tiles: BTreeMap<String, TileEntry> }` (Default) with `get(&self, TileId) -> Option<&TileEntry>`, `set(&mut self, TileId, TileEntry)`, `tile_ids(&self, TileStatus) -> Vec<TileId>`
  - `struct Store` with `new(root) -> Store`, `tile_path(&self, TileId) -> PathBuf`, `load_index(&self) -> Result<StoreIndex>`, `save_index(&self, &StoreIndex) -> Result<()>`, `write_tile(&self, &GridSpec, &TileHeights) -> Result<()>`, `read_tile(&self, &GridSpec, TileId) -> Result<Option<Vec<f32>>>`

- [ ] **Step 1: Write the failing tests**

Add `pub mod store;` to `src/lib.rs`. Create `src/store.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn heights(tile: TileId) -> TileHeights {
        let n = NODES_PER_SIDE;
        TileHeights { tile, data: (0..n * n).map(|k| (k % 997) as f32 * 0.37 - 20.0).collect(), filled: 0 }
    }

    #[test]
    fn tile_round_trips_bit_identically() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let grid = GridSpec::default();
        let h = heights(TileId::new(812, 1440));
        store.write_tile(&grid, &h).unwrap();
        let back = store.read_tile(&grid, h.tile).unwrap().unwrap();
        assert!(back.iter().zip(&h.data).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn store_tiles_are_georeferenced_node_grids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let grid = GridSpec::default();
        let t = TileId::new(812, 1440);
        store.write_tile(&grid, &heights(t)).unwrap();
        let r = Raster::read_geotiff(&store.tile_path(t)).unwrap();
        let b = grid.tile_bounds(t);
        assert_eq!(r.epsg, EPSG_CONUS_ALBERS);
        // Pixel (0,0) is centered on the tile's NW corner node.
        assert_eq!(r.origin_x + r.pixel_w / 2.0, b.x_min);
        assert_eq!(r.origin_y - r.pixel_h / 2.0, b.y_max);
    }

    #[test]
    fn mismatched_georeference_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let grid = GridSpec::default();
        let t = TileId::new(812, 1440);
        store.write_tile(&grid, &heights(t)).unwrap();
        std::fs::copy(store.tile_path(t), store.tile_path(t.east())).unwrap();
        let err = store.read_tile(&grid, t.east()).unwrap_err();
        assert!(format!("{err:#}").contains("does not match"), "{err:#}");
    }

    #[test]
    fn absent_tile_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::new(dir.path()).read_tile(&GridSpec::default(), TileId::new(1, 1)).unwrap().is_none());
    }

    #[test]
    fn index_round_trips_and_defaults_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("store"));
        assert_eq!(store.load_index().unwrap(), StoreIndex::default());
        let mut idx = StoreIndex::default();
        let entry = TileEntry { status: TileStatus::Ok, error: None, min_elevation_m: Some(1.0), max_elevation_m: Some(9.0), filled_samples: 3, sources: vec!["n39w121".into()] };
        idx.set(TileId::new(-3, 7), entry.clone());
        idx.set(TileId::new(4, 5), TileEntry { status: TileStatus::Empty, ..entry.clone() });
        store.save_index(&idx).unwrap();
        let back = store.load_index().unwrap();
        assert_eq!(back, idx);
        assert_eq!(back.get(TileId::new(-3, 7)), Some(&entry));
        assert_eq!(back.tile_ids(TileStatus::Ok), vec![TileId::new(-3, 7)]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib store`
Expected: compile errors (`Store` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/store.rs`:
```rust
//! On-disk store of 10 m height tiles: `{tx}_{ty}.tif` (f32, EPSG:5070, PixelIsPoint,
//! Deflate — opens directly in QGIS) plus `index.json` recording every tile's status.

use crate::dem::{EPSG_CONUS_ALBERS, Raster};
use crate::grid::{GridSpec, NODE_SPACING_M, NODES_PER_SIDE, TileId};
use crate::reproject::TileHeights;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileStatus {
    Ok,
    Empty,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileEntry {
    pub status: TileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub min_elevation_m: Option<f32>,
    pub max_elevation_m: Option<f32>,
    pub filled_samples: u32,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoreIndex {
    pub grid: GridSpec,
    /// Keyed by `"{tx}_{ty}"`.
    pub tiles: BTreeMap<String, TileEntry>,
}

impl StoreIndex {
    pub fn get(&self, t: TileId) -> Option<&TileEntry> {
        self.tiles.get(&t.to_string())
    }
    pub fn set(&mut self, t: TileId, e: TileEntry) {
        self.tiles.insert(t.to_string(), e);
    }
    pub fn tile_ids(&self, status: TileStatus) -> Vec<TileId> {
        self.tiles.iter().filter(|(_, e)| e.status == status).filter_map(|(k, _)| k.parse().ok()).collect()
    }
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn tile_path(&self, t: TileId) -> PathBuf {
        self.root.join(format!("{t}.tif"))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    pub fn load_index(&self) -> Result<StoreIndex> {
        let p = self.index_path();
        if !p.exists() {
            return Ok(StoreIndex::default());
        }
        let s = fs::read_to_string(&p)?;
        serde_json::from_str(&s).with_context(|| format!("parse {}", p.display()))
    }

    /// Written to a temp file then renamed, so an interrupted ingest never leaves a torn index.
    pub fn save_index(&self, idx: &StoreIndex) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let tmp = self.root.join("index.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(idx)?)?;
        fs::rename(tmp, self.index_path())?;
        Ok(())
    }

    pub fn write_tile(&self, grid: &GridSpec, h: &TileHeights) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let b = grid.tile_bounds(h.tile);
        let raster = Raster {
            width: NODES_PER_SIDE,
            height: NODES_PER_SIDE,
            data: h.data.clone(),
            origin_x: b.x_min - NODE_SPACING_M / 2.0,
            origin_y: b.y_max + NODE_SPACING_M / 2.0,
            pixel_w: NODE_SPACING_M,
            pixel_h: NODE_SPACING_M,
            epsg: EPSG_CONUS_ALBERS,
            nodata: None,
        };
        let path = self.tile_path(h.tile);
        let tmp = path.with_extension("tif.tmp");
        raster.write_geotiff(&tmp, true)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    /// Heights of a stored tile, or None if the tile has no file.
    pub fn read_tile(&self, grid: &GridSpec, t: TileId) -> Result<Option<Vec<f32>>> {
        let path = self.tile_path(t);
        if !path.exists() {
            return Ok(None);
        }
        let r = Raster::read_geotiff(&path)?;
        let b = grid.tile_bounds(t);
        ensure!(
            r.width == NODES_PER_SIDE && r.height == NODES_PER_SIDE,
            "{}: expected {NODES_PER_SIDE}² nodes, got {}×{}",
            path.display(),
            r.width,
            r.height
        );
        ensure!(
            r.epsg == EPSG_CONUS_ALBERS
                && (r.origin_x + r.pixel_w / 2.0 - b.x_min).abs() < 1e-6
                && (r.origin_y - r.pixel_h / 2.0 - b.y_max).abs() < 1e-6,
            "{}: georeference does not match tile {t}",
            path.display()
        );
        Ok(Some(r.data))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib store`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/store.rs
git commit -m "feat: height-tile store with GeoTIFF tiles and JSON index"
```

---

### Task 8: Ingest orchestration

**Files:**
- Create: `src/ingest.rs`, `tests/common/mod.rs`, `tests/ingest.rs`
- Modify: `src/lib.rs` (add `pub mod ingest;`)

**Interfaces:**
- Consumes: `grid::{GridSpec, TileId, TileRange}`, `crs::Albers`, `region::{Region, tiles_in_region}`, `dem::{Raster, EPSG_NAD83_GEOGRAPHIC, EPSG_WGS84_GEOGRAPHIC}`, `source::{SourceCache, SourceCell, Fetched}`, `reproject::{Sources, resample_tile, source_cells_for_tile, min_max}`, `store::{Store, StoreIndex, TileEntry, TileStatus}`.
- Produces (`vr_fire::ingest`):
  - `struct IngestOptions { region: PathBuf, tiles: Option<TileRange>, cache_dir: PathBuf, store_dir: PathBuf, base_url: String, force: bool, download_threads: usize, max_loaded_sources: usize, retry_delay: Duration }`
  - `struct IngestReport { written: usize, empty: usize, skipped: usize, failed: Vec<(TileId, String)> }`
  - `fn run_ingest(&IngestOptions) -> Result<IngestReport>`
- Test fixtures (`tests/common/mod.rs`, used again in Task 11): `plane(lon, lat) -> f64`, `write_plane_source(cache_dir, SourceCell)`, `write_region(dir, west, south, east, north) -> PathBuf`, `options(dir, region) -> IngestOptions`

- [ ] **Step 1: Write the shared test fixtures**

Create `tests/common/mod.rs`:
```rust
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
```

- [ ] **Step 2: Write the failing integration tests**

Add `pub mod ingest;` to `src/lib.rs`. Create `tests/ingest.rs`:
```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test ingest`
Expected: compile errors (`vr_fire::ingest` has no `IngestOptions`/`run_ingest`).

- [ ] **Step 4: Implement**

Create `src/ingest.rs`:
```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test ingest`
Expected: 4 passed, 1 ignored. (`unreachable_sources_fail_only_their_tiles` retries 5× against a closed port with zero delay; it should take well under a second.)

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/ingest.rs tests/common/mod.rs tests/ingest.rs
git commit -m "feat: ingest region tiles from cached 3DEP sources into the store"
```

---

### Task 9: Mesh generation

**Files:**
- Create: `src/mesh.rs`
- Modify: `src/lib.rs` (add `pub mod mesh;`)

**Interfaces:**
- Consumes: `grid::{NODES_PER_SIDE, NODE_SPACING_M, LOD_STRIDES}`.
- Produces (`vr_fire::mesh`):
  - const `SKIRT_DEPTH_M: f32 = 50.0`
  - `struct Mesh { positions: Vec<[f32; 3]>, normals: Vec<[f32; 3]>, uvs: Vec<[f32; 2]>, indices: Vec<u32> }` (Default)
  - `struct Neighborhood<'a> { center: &'a [f32], north: Option<&'a [f32]>, south: Option<&'a [f32]>, east: Option<&'a [f32]>, west: Option<&'a [f32]> }`
  - `fn compute_normals(&Neighborhood) -> Vec<[f32; 3]>` (376² unit normals)
  - `fn build_mesh(heights: &[f32], normals: &[[f32; 3]], lod: usize) -> Mesh`

- [ ] **Step 1: Write the failing tests**

Add `pub mod mesh;` to `src/lib.rs`. Create `src/mesh.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = NODES_PER_SIDE;

    /// Heights from a function of tile-local meters (x east, z south).
    fn field(f: impl Fn(f32, f32) -> f32) -> Vec<f32> {
        (0..N * N).map(|k| f((k % N) as f32 * 10.0, (k / N) as f32 * 10.0)).collect()
    }

    fn alone(center: &[f32]) -> Neighborhood<'_> {
        Neighborhood { center, north: None, south: None, east: None, west: None }
    }

    fn tri_normal(m: &Mesh, t: usize) -> [f32; 3] {
        let [a, b, c] = [0, 1, 2].map(|k| m.positions[m.indices[3 * t + k] as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    fn unit(v: [f32; 3]) -> [f32; 3] {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / l, v[1] / l, v[2] / l]
    }

    #[test]
    fn vertex_and_triangle_counts_per_lod() {
        let h = field(|_, _| 0.0);
        let normals = compute_normals(&alone(&h));
        for (lod, n) in [(0, 376), (1, 126), (2, 26), (3, 6)] {
            let m = build_mesh(&h, &normals, lod);
            assert_eq!(m.positions.len(), n * n + 4 * n, "lod {lod}");
            assert_eq!(m.normals.len(), m.positions.len());
            assert_eq!(m.uvs.len(), m.positions.len());
            assert_eq!(m.indices.len() / 3, 2 * (n - 1) * (n - 1) + 8 * (n - 1), "lod {lod}");
            assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
        }
    }

    #[test]
    fn top_surface_faces_up_and_spans_the_tile() {
        let h = field(|_, _| 100.0);
        let m = build_mesh(&h, &compute_normals(&alone(&h)), 1);
        let n = 126;
        for t in 0..2 * (n - 1) * (n - 1) {
            assert!(tri_normal(&m, t)[1] > 0.0, "triangle {t} faces down");
        }
        assert_eq!(m.positions[0], [0.0, 100.0, 0.0]);
        assert_eq!(m.positions[n * n - 1], [3750.0, 100.0, 3750.0]);
        assert_eq!(m.uvs[0], [0.0, 0.0]);
        assert_eq!(m.uvs[n * n - 1], [1.0, 1.0]);
        assert_eq!(m.normals[0], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn skirts_hang_down_and_face_outward() {
        let h = field(|_, _| 100.0);
        let m = build_mesh(&h, &compute_normals(&alone(&h)), 2);
        let n = 26;
        let first_skirt_tri = 2 * (n - 1) * (n - 1);
        for t in first_skirt_tri..m.indices.len() / 3 {
            let nrm = tri_normal(&m, t);
            let c = [0, 1, 2].map(|k| m.positions[m.indices[3 * t + k] as usize]);
            let (cx, cz) = ((c[0][0] + c[1][0] + c[2][0]) / 3.0 - 1875.0, (c[0][2] + c[1][2] + c[2][2]) / 3.0 - 1875.0);
            assert!(nrm[0] * cx + nrm[2] * cz > 0.0, "skirt triangle {t} faces inward");
        }
        assert!(m.positions[n * n..].iter().all(|p| p[1] == 100.0 - SKIRT_DEPTH_M));
    }

    #[test]
    fn normals_follow_slopes() {
        let east = field(|x, _| 0.1 * x);
        for nrm in compute_normals(&alone(&east)) {
            assert!(close(nrm, unit([-0.1, 1.0, 0.0])), "{nrm:?}");
        }
        let south = field(|_, z| 0.2 * z);
        for nrm in compute_normals(&alone(&south)) {
            assert!(close(nrm, unit([0.0, 1.0, -0.2])), "{nrm:?}");
        }
    }

    #[test]
    fn normals_without_neighbors_use_one_sided_differences() {
        // A crease at the east edge: one-sided differences must not panic and stay unit length.
        let h = field(|x, _| if x >= 3740.0 { 50.0 } else { 0.0 });
        for nrm in compute_normals(&alone(&h)) {
            let l = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
            assert!((l - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn edge_normals_reach_into_neighbors() {
        let center = field(|_, _| 0.0);
        let east = field(|x, _| if x == 10.0 { 20.0 } else { 0.0 }); // east node 1 = 20 m
        let nb = Neighborhood { center: &center, north: None, south: None, east: Some(&east), west: None };
        let normals = compute_normals(&nb);
        // At center node 375: dh/dx = (20 − 0) / 20 m = 1.
        assert!(close(normals[100 * N + N - 1], unit([-1.0, 1.0, 0.0])));
        assert_eq!(normals[100 * N + N - 2], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn adjacent_tiles_meet_exactly_at_every_lod() {
        // Global field sampled for two tiles sharing an edge.
        let g = |x: f32, z: f32| (x * 0.01).sin() * 40.0 + z * 0.05;
        let west = field(|x, z| g(x, z));
        let east = field(|x, z| g(x + 3750.0, z));
        let wn = compute_normals(&Neighborhood { center: &west, north: None, south: None, east: Some(&east), west: None });
        let en = compute_normals(&Neighborhood { center: &east, north: None, south: None, east: None, west: Some(&west) });
        for (lod, &stride) in LOD_STRIDES.iter().enumerate() {
            let (a, b) = (build_mesh(&west, &wn, lod), build_mesh(&east, &en, lod));
            let n = (N - 1) / stride + 1;
            for r in 0..n {
                let (pa, pb) = (a.positions[r * n + n - 1], b.positions[r * n]);
                assert_eq!((pa[0], pa[1].to_bits(), pa[2]), (pb[0] + 3750.0, pb[1].to_bits(), pb[2]), "lod {lod} row {r}");
                assert_eq!(a.normals[r * n + n - 1], b.normals[r * n], "lod {lod} row {r}");
            }
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib mesh`
Expected: compile errors (`Mesh` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/mesh.rs`:
```rust
//! Height tile → glTF-convention grid mesh at a given LOD, with skirts.
//!
//! Axes: +X east, +Y up (elevation), +Z south; origin at the tile's NW corner.

use crate::grid::{LOD_STRIDES, NODE_SPACING_M, NODES_PER_SIDE};

/// How far skirts drop below the edge, hiding cracks between tiles at different LODs.
pub const SKIRT_DEPTH_M: f32 = 50.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// A tile's 10 m heights plus its four edge neighbors (None when absent or empty).
pub struct Neighborhood<'a> {
    pub center: &'a [f32],
    pub north: Option<&'a [f32]>,
    pub south: Option<&'a [f32]>,
    pub east: Option<&'a [f32]>,
    pub west: Option<&'a [f32]>,
}

impl Neighborhood<'_> {
    /// Height at node (i, j), reaching one node across an edge into a neighbor. Edge nodes
    /// are shared, so our node −1 is the west tile's node 374 and our node 376 is the east tile's node 1.
    fn height(&self, i: isize, j: isize) -> Option<f32> {
        let n = NODES_PER_SIDE as isize;
        let at = |t: &[f32], i: isize, j: isize| t[(j * n + i) as usize];
        let inside = |k: isize| (0..n).contains(&k);
        match (inside(i), inside(j)) {
            (true, true) => Some(at(self.center, i, j)),
            (false, true) if i == -1 => self.west.map(|t| at(t, n - 2, j)),
            (false, true) if i == n => self.east.map(|t| at(t, 1, j)),
            (true, false) if j == -1 => self.north.map(|t| at(t, i, n - 2)),
            (true, false) if j == n => self.south.map(|t| at(t, i, 1)),
            _ => None,
        }
    }
}

/// Unit normals at every 10 m node, from central differences (one-sided where a neighbor is missing).
pub fn compute_normals(nb: &Neighborhood) -> Vec<[f32; 3]> {
    let n = NODES_PER_SIDE as isize;
    let d = NODE_SPACING_M as f32;
    let mut out = Vec::with_capacity((n * n) as usize);
    for j in 0..n {
        for i in 0..n {
            let h = nb.height(i, j).expect("center node");
            let dhdx = slope(nb.height(i - 1, j), h, nb.height(i + 1, j), d);
            let dhdz = slope(nb.height(i, j - 1), h, nb.height(i, j + 1), d);
            let len = (dhdx * dhdx + 1.0 + dhdz * dhdz).sqrt();
            out.push([-dhdx / len, 1.0 / len, -dhdz / len]);
        }
    }
    out
}

fn slope(before: Option<f32>, here: f32, after: Option<f32>, d: f32) -> f32 {
    match (before, after) {
        (Some(b), Some(a)) => (a - b) / (2.0 * d),
        (None, Some(a)) => (a - here) / d,
        (Some(b), None) => (here - b) / d,
        (None, None) => 0.0,
    }
}

/// Grid mesh taking every `LOD_STRIDES[lod]`-th node, plus skirts on all four edges.
pub fn build_mesh(heights: &[f32], normals: &[[f32; 3]], lod: usize) -> Mesh {
    let stride = LOD_STRIDES[lod];
    let n = (NODES_PER_SIDE - 1) / stride + 1;
    let step = NODE_SPACING_M as f32 * stride as f32;
    let mut m = Mesh::default();
    for r in 0..n {
        for c in 0..n {
            let k = r * stride * NODES_PER_SIDE + c * stride;
            m.positions.push([c as f32 * step, heights[k], r as f32 * step]);
            m.normals.push(normals[k]);
            m.uvs.push([c as f32 / (n - 1) as f32, r as f32 / (n - 1) as f32]);
        }
    }
    let v = |c: usize, r: usize| (r * n + c) as u32;
    for r in 0..n - 1 {
        for c in 0..n - 1 {
            let (a, b, cc, d) = (v(c, r), v(c + 1, r), v(c, r + 1), v(c + 1, r + 1));
            m.indices.extend_from_slice(&[a, cc, b, b, cc, d]);
        }
    }
    // Edges walked clockwise seen from above, so each skirt faces outward.
    let north: Vec<u32> = (0..n).map(|c| v(c, 0)).collect();
    let east: Vec<u32> = (0..n).map(|r| v(n - 1, r)).collect();
    let south: Vec<u32> = (0..n).rev().map(|c| v(c, n - 1)).collect();
    let west: Vec<u32> = (0..n).rev().map(|r| v(0, r)).collect();
    for edge in [north, east, south, west] {
        add_skirt(&mut m, &edge);
    }
    m
}

fn add_skirt(m: &mut Mesh, edge: &[u32]) {
    let base = m.positions.len() as u32;
    for &e in edge {
        let e = e as usize;
        let [x, y, z] = m.positions[e];
        m.positions.push([x, y - SKIRT_DEPTH_M, z]);
        m.normals.push(m.normals[e]);
        m.uvs.push(m.uvs[e]);
    }
    for k in 0..edge.len() - 1 {
        let (a, b) = (edge[k], edge[k + 1]);
        let (a2, b2) = (base + k as u32, base + k as u32 + 1);
        m.indices.extend_from_slice(&[a, b, a2, b, b2, a2]);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib mesh`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/mesh.rs
git commit -m "feat: LOD grid meshes with seamless normals and outward skirts"
```

---

### Task 10: Export (glb, raw heights, metadata)

**Files:**
- Create: `src/export.rs`
- Modify: `src/lib.rs` (add `pub mod export;`)

**Interfaces:**
- Consumes: `mesh::Mesh`, `grid::{GridSpec, TileId, Bounds, LOD_STRIDES, NODE_SPACING_M, NODES_PER_SIDE, CELLS_PER_TILE, WINDOWS_PER_TILE}`, `store::TileEntry`, `reproject::min_max`, `crs::EPSG_5070`.
- Produces (`vr_fire::export`):
  - `fn glb_bytes(&Mesh, name: &str) -> Vec<u8>`, `fn write_glb(&Path, &Mesh, name: &str) -> Result<()>`
  - `fn write_heights_f32(&Path, &[f32]) -> Result<()>` (little-endian, row-major, north row first)
  - `struct LodInfo { lod: usize, spacing_m: f64, nodes_per_side: usize, file: String }`
  - `struct TileMetadata { tile, crs, grid_origin, bounds, vertical_datum, axes, node_spacing_m, nodes_per_side, ml_cells_per_side, ml_windows_per_side, lods, heights_file, min_elevation_m, max_elevation_m, filled_samples, sources, pipeline_version }` with `TileMetadata::new(&GridSpec, TileId, &TileEntry, heights: &[f32]) -> TileMetadata`
  - `fn write_metadata(&Path, &TileMetadata) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

Add `pub mod export;` to `src/lib.rs`. Create `src/export.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TileStatus;

    fn quad() -> Mesh {
        Mesh {
            positions: vec![[0.0, 1.0, 0.0], [10.0, 2.0, 0.0], [0.0, 3.0, 10.0], [10.0, 4.0, 10.0]],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            indices: vec![0, 2, 1, 1, 2, 3],
        }
    }

    #[test]
    fn glb_is_well_formed_and_aligned() {
        let bytes = glb_bytes(&quad(), "q");
        assert_eq!(&bytes[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize, bytes.len());
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(json_len % 4, 0);
        assert_eq!(&bytes[16..20], b"JSON");
        assert_eq!(&bytes[20 + json_len + 4..20 + json_len + 8], b"BIN\0");
    }

    #[test]
    fn glb_reloads_with_the_gltf_crate() {
        let m = quad();
        let (doc, buffers, _) = gltf::import_slice(glb_bytes(&m, "q")).unwrap();
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        assert_eq!(reader.read_positions().unwrap().collect::<Vec<_>>(), m.positions);
        assert_eq!(reader.read_normals().unwrap().collect::<Vec<_>>(), m.normals);
        assert_eq!(reader.read_tex_coords(0).unwrap().into_f32().collect::<Vec<_>>(), m.uvs);
        assert_eq!(reader.read_indices().unwrap().into_u32().collect::<Vec<_>>(), m.indices);
        let bb = prim.bounding_box();
        assert_eq!((bb.min, bb.max), ([0.0, 1.0, 0.0], [10.0, 4.0, 10.0]));
    }

    #[test]
    fn heights_are_little_endian_f32() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.f32");
        write_heights_f32(&p, &[1.5, -2.0]).unwrap();
        let b = std::fs::read(p).unwrap();
        assert_eq!(b, [1.5f32.to_le_bytes(), (-2.0f32).to_le_bytes()].concat());
    }

    #[test]
    fn metadata_describes_the_tile() {
        let grid = GridSpec::default();
        let t = TileId::new(812, 1440);
        let entry = TileEntry { status: TileStatus::Ok, error: None, min_elevation_m: Some(0.0), max_elevation_m: Some(0.0), filled_samples: 7, sources: vec!["n39w121".into()] };
        let heights: Vec<f32> = (0..NODES_PER_SIDE * NODES_PER_SIDE).map(|k| k as f32 / 1000.0).collect();
        let meta = TileMetadata::new(&grid, t, &entry, &heights);
        assert_eq!(meta.bounds, grid.tile_bounds(t));
        assert_eq!(meta.lods.len(), 4);
        assert_eq!(meta.lods[1].spacing_m, 30.0);
        assert_eq!(meta.lods[1].nodes_per_side, 126);
        assert_eq!(meta.lods[3].file, "lod3/812_1440.glb");
        assert_eq!(meta.heights_file, "812_1440.f32");
        assert_eq!((meta.ml_cells_per_side, meta.ml_windows_per_side), (125, 10));
        assert_eq!(meta.filled_samples, 7);
        assert_eq!(meta.max_elevation_m, heights[heights.len() - 1]);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.json");
        write_metadata(&p, &meta).unwrap();
        let back: TileMetadata = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(back, meta);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib export`
Expected: compile errors (`glb_bytes` not found).

- [ ] **Step 3: Implement**

Insert above the tests in `src/export.rs`:
```rust
//! Bake outputs: binary glTF meshes, raw f32 heights, and per-tile JSON metadata.

use crate::crs::EPSG_5070;
use crate::grid::{Bounds, CELLS_PER_TILE, GridSpec, LOD_STRIDES, NODE_SPACING_M, NODES_PER_SIDE, TileId, WINDOWS_PER_TILE};
use crate::mesh::Mesh;
use crate::reproject::min_max;
use crate::store::TileEntry;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_INT: u32 = 5125;

/// Binary glTF 2.0: one mesh, one primitive (POSITION, NORMAL, TEXCOORD_0, u32 indices).
pub fn glb_bytes(mesh: &Mesh, name: &str) -> Vec<u8> {
    let nv = mesh.positions.len();
    let mut bin = Vec::with_capacity(nv * 32 + mesh.indices.len() * 4);
    mesh.positions.iter().flatten().for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    let normals_at = bin.len();
    mesh.normals.iter().flatten().for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    let uvs_at = bin.len();
    mesh.uvs.iter().flatten().for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    let indices_at = bin.len();
    mesh.indices.iter().for_each(|i| bin.extend_from_slice(&i.to_le_bytes()));

    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    let doc = json!({
        "asset": { "version": "2.0", "generator": concat!("vr_fire ", env!("CARGO_PKG_VERSION")) },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "name": name }],
        "meshes": [{ "name": name, "primitives": [{
            "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 }, "indices": 3, "mode": 4 }] }],
        "buffers": [{ "byteLength": bin.len() }],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": normals_at, "target": ARRAY_BUFFER },
            { "buffer": 0, "byteOffset": normals_at, "byteLength": uvs_at - normals_at, "target": ARRAY_BUFFER },
            { "buffer": 0, "byteOffset": uvs_at, "byteLength": indices_at - uvs_at, "target": ARRAY_BUFFER },
            { "buffer": 0, "byteOffset": indices_at, "byteLength": bin.len() - indices_at, "target": ELEMENT_ARRAY_BUFFER }
        ],
        "accessors": [
            { "bufferView": 0, "componentType": FLOAT, "count": nv, "type": "VEC3", "min": min, "max": max },
            { "bufferView": 1, "componentType": FLOAT, "count": nv, "type": "VEC3" },
            { "bufferView": 2, "componentType": FLOAT, "count": nv, "type": "VEC2" },
            { "bufferView": 3, "componentType": UNSIGNED_INT, "count": mesh.indices.len(), "type": "SCALAR" }
        ]
    });
    let mut json_chunk = serde_json::to_vec(&doc).expect("serializable");
    while json_chunk.len() % 4 != 0 {
        json_chunk.push(b' ');
    }
    let total = 12 + 8 + json_chunk.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_chunk);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

pub fn write_glb(path: &Path, mesh: &Mesh, name: &str) -> Result<()> {
    std::fs::write(path, glb_bytes(mesh, name)).with_context(|| format!("write {}", path.display()))
}

pub fn write_heights_f32(path: &Path, heights: &[f32]) -> Result<()> {
    let bytes: Vec<u8> = heights.iter().flat_map(|h| h.to_le_bytes()).collect();
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LodInfo {
    pub lod: usize,
    pub spacing_m: f64,
    pub nodes_per_side: usize,
    /// Relative to the bake output directory.
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileMetadata {
    pub tile: TileId,
    pub crs: String,
    pub grid_origin: [f64; 2],
    /// EPSG:5070 bounds (f64). Place tiles relative to a per-session floating origin.
    pub bounds: Bounds,
    pub vertical_datum: String,
    pub axes: String,
    pub node_spacing_m: f64,
    pub nodes_per_side: usize,
    pub ml_cells_per_side: i64,
    pub ml_windows_per_side: i64,
    pub lods: Vec<LodInfo>,
    /// Relative to the bake output directory; little-endian f32, row-major, north row first.
    pub heights_file: String,
    pub min_elevation_m: f32,
    pub max_elevation_m: f32,
    pub filled_samples: u32,
    pub sources: Vec<String>,
    pub pipeline_version: String,
}

impl TileMetadata {
    pub fn new(grid: &GridSpec, tile: TileId, entry: &TileEntry, heights: &[f32]) -> Self {
        let (lo, hi) = min_max(heights);
        Self {
            tile,
            crs: format!("EPSG:5070 ({EPSG_5070})"),
            grid_origin: [grid.origin_x, grid.origin_y],
            bounds: grid.tile_bounds(tile),
            vertical_datum: "NAVD88 meters".into(),
            axes: "glTF: +X east, +Y up (elevation m), +Z south; origin at tile NW corner (bounds.x_min, bounds.y_max)".into(),
            node_spacing_m: NODE_SPACING_M,
            nodes_per_side: NODES_PER_SIDE,
            ml_cells_per_side: CELLS_PER_TILE,
            ml_windows_per_side: WINDOWS_PER_TILE,
            lods: LOD_STRIDES
                .iter()
                .enumerate()
                .map(|(lod, &s)| LodInfo {
                    lod,
                    spacing_m: NODE_SPACING_M * s as f64,
                    nodes_per_side: (NODES_PER_SIDE - 1) / s + 1,
                    file: format!("lod{lod}/{tile}.glb"),
                })
                .collect(),
            heights_file: format!("{tile}.f32"),
            min_elevation_m: lo,
            max_elevation_m: hi,
            filled_samples: entry.filled_samples,
            sources: entry.sources.clone(),
            pipeline_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

pub fn write_metadata(path: &Path, meta: &TileMetadata) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(meta)?).with_context(|| format!("write {}", path.display()))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib export`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/export.rs
git commit -m "feat: export glb meshes, raw heights, and tile metadata"
```

---

### Task 11: Bake orchestration

**Files:**
- Create: `src/bake.rs`, `tests/pipeline.rs`
- Modify: `src/lib.rs` (add `pub mod bake;`)

**Interfaces:**
- Consumes: `store::{Store, StoreIndex, TileStatus}`, `mesh::{Neighborhood, compute_normals, build_mesh}`, `export::{write_glb, write_heights_f32, write_metadata, TileMetadata}`, `grid::{TileId, TileRange, LOD_STRIDES}`; test fixtures from `tests/common/mod.rs` (Task 8).
- Produces (`vr_fire::bake`):
  - `struct BakeOptions { store_dir: PathBuf, out_dir: PathBuf, tiles: Option<TileRange> }`
  - `enum BakeOutcome { Baked, Empty }`
  - `struct BakeReport { baked: usize, skipped_empty: usize, failed: Vec<(TileId, String)> }`
  - `fn bake_tile(&Store, &StoreIndex, TileId, out_dir: &Path) -> Result<BakeOutcome>`
  - `fn run_bake(&BakeOptions) -> Result<BakeReport>`

- [ ] **Step 1: Write the failing integration tests**

Add `pub mod bake;` to `src/lib.rs`. Create `tests/pipeline.rs`:
```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test pipeline`
Expected: compile errors (`vr_fire::bake` items not found).

- [ ] **Step 3: Implement**

Create `src/bake.rs`:
```rust
//! `bake`: store height tiles → glb meshes (all LODs), raw heights, and metadata.

use crate::export::{TileMetadata, write_glb, write_heights_f32, write_metadata};
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

pub fn bake_tile(store: &Store, index: &StoreIndex, tile: TileId, out_dir: &Path) -> Result<BakeOutcome> {
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
        write_glb(&dir.join(format!("{tile}.glb")), &build_mesh(&center, &normals, lod), &format!("tile_{tile}_lod{lod}"))?;
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
        tiles.par_iter().map(|&t| (t, bake_tile(&store, &index, t, &opts.out_dir))).collect();
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test pipeline`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/bake.rs tests/pipeline.rs
git commit -m "feat: bake store tiles into LOD glb meshes, heights, and metadata"
```

---

### Task 12: CLI, README, and real-data smoke run

**Files:**
- Modify: `src/main.rs`
- Create: `README.md`

**Interfaces:**
- Consumes: `ingest::{IngestOptions, run_ingest}`, `bake::{BakeOptions, run_bake}`, `grid::{GridSpec, TileRange, cell_to_tile}`, `crs::Albers`, `source::USGS_13_BASE_URL`.
- Produces: the `vr_fire` binary with subcommands `ingest`, `bake`, `locate`.

- [ ] **Step 1: Implement the CLI**

Replace `src/main.rs`:
```rust
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
```

- [ ] **Step 2: Verify the CLI builds and runs**

Run: `cargo run --release -- locate --lon -120.8 --lat 38.79`
Expected: prints `EPSG:5070    -2109889.75, 2028245.48` (matches the pyproj fixture), a tile id, an ML cell, and a `--tiles` range.

Run: `cargo run --release -- --help` and `cargo run --release -- ingest --help`
Expected: the three subcommands and their flags are listed.

- [ ] **Step 3: Write the README**

Create `README.md`:
````markdown
# vr_fire — terrain pipeline

Builds terrain tiles for the VR wildfire simulator from USGS 3DEP 10 m elevation,
aligned to the ML team's 30 m grid (EPSG:5070, NLCD/LANDFIRE-aligned).
Design: `docs/superpowers/specs/2026-09-24-terrain-pipeline-design.md`.

## Usage

```bash
# Which tile is this place in?
cargo run --release -- locate --lon -120.8 --lat 38.79

# Ingest the 3×3 block around it (downloads the covering 3DEP 1° files, ~460 MB each, into cache/)
cargo run --release -- ingest --tiles 101,340..103,342

# Ingest all of California (~55 source files, tens of GB; resumable — just re-run)
cargo run --release -- ingest

# Bake meshes for those tiles (or omit --tiles to bake everything in the store)
cargo run --release -- bake --tiles 101,340..103,342
```

## Outputs

| Path | Content |
|---|---|
| `store/{tx}_{ty}.tif` | 376×376 f32 heights (m, NAVD88), EPSG:5070 GeoTIFF, node-registered. Opens in QGIS. |
| `store/index.json` | Status (`ok`/`empty`/`failed`), elevation range, sources per tile |
| `tiles/lod{0..3}/{tx}_{ty}.glb` | Meshes at 10 / 30 / 150 / 750 m spacing, with 50 m skirts |
| `tiles/{tx}_{ty}.f32` | Raw heights, little-endian f32, row-major, north row first |
| `tiles/{tx}_{ty}.json` | Bounds, CRS, LOD list, provenance |

## Conventions

- Tiles are 3,750 m square: 10×10 ML windows (375 m), 125×125 ML cells (30 m).
- Mesh axes follow glTF: +X east, +Y up, +Z south; origin at the tile's NW corner.
- Place tiles relative to a per-session floating origin using the f64 `bounds` in the JSON:
  world position = `(x_min − origin_x, 0, origin_y − y_max)`. Never put raw EPSG:5070
  coordinates (~2×10⁶ m) into float32 vertex data.
- UV0 spans the tile; ML cell (c, r) covers `[c/125, (c+1)/125] × [r/125, (r+1)/125]`.
- Nodes with no 3DEP data (ocean) are sea level (0 m); tiles with none at all are `empty`.

## Tests

```bash
cargo test                                          # offline
cargo test --release -- --ignored real_3dep         # downloads one real 3DEP file
```
````

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all unit and integration tests pass; 1 ignored.

Run: `cargo clippy --all-targets`
Expected: no errors (fix any warnings introduced by this plan's code).

- [ ] **Step 5: Real-data smoke run (manual, needs network, ~1 GB download)**

```bash
cargo run --release -- locate --lon -120.65 --lat 38.45
# Use the printed "3×3 around" range below:
cargo run --release -- ingest --tiles <range>
cargo run --release -- bake --tiles <range>
ls tiles/lod0 | wc -l          # expect 9
```
Then check by eye:
- Open `tiles/lod0/<center>.glb` in Blender (File → Import → glTF 2.0) or https://gltf-viewer.donmccurdy.com — expect Sierra foothill terrain with ~500–1,500 m relief, no holes.
- Import all 9 LOD 0 tiles, offsetting each by its `bounds` from the JSON relative to one origin. There should be no visible seams.
- Open `store/<center>.tif` in QGIS over an OpenStreetMap basemap. The hillshade should line up with the mapped ridges and rivers.

Record the results (any seams, misalignment, timing per tile) in the PR/commit message.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs README.md
git commit -m "feat: vr_fire CLI (ingest, bake, locate) and README"
```
