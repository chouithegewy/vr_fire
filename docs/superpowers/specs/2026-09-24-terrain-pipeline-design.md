# Terrain Pipeline — Design Spec

- **Date:** 2026-09-24
- **Status:** Draft, awaiting review
- **Scope:** Sub-project 1 of the VR wildfire simulator prototype (terrain ingest + mesh bake)

## 1. Context

The VR wildfire simulator is rendered **server-side** and streamed to a VR headset as video.
A separate ML system predicts fire spread on **30 m cells** grouped into **375 m × 375 m windows**,
over **California** (possibly the contiguous US later).

This sub-project turns elevation data into engine-agnostic 3D terrain tiles that the server-side
renderer (Unity today, possibly another engine later) can load, and that line up exactly with the
ML grid so fire predictions can be overlaid on the terrain.

Out of scope here (separate sub-projects, separate specs):

- **Streaming benchmark** — end-to-end harness that scales scene load (terrain, fire, smoke) and
  stream settings (resolution, fps, codec, bitrate) together and reports whether the GPU, encoder,
  or network saturates first.
- Fire / smoke rendering, ML integration, tile serving, runtime LOD selection in the renderer.

## 2. Decisions

| Decision | Choice | Why |
|---|---|---|
| Language | Pure Rust (existing `vr_fire` crate), no system deps | `cargo build` works on every teammate's machine; no GDAL install |
| Elevation source | USGS 3DEP 1/3 arc-second (~10 m) seamless DEM, bulk 1°×1° GeoTIFFs from the public `prd-tnm` S3 bucket | Statewide/national coverage; no API key or quota. OpenTopography kept only as an optional fallback for ad-hoc areas |
| Working CRS | EPSG:5070 (NAD83 / CONUS Albers) | Standard CRS of CONUS 30 m products (NLCD, LANDFIRE); same datum as 3DEP (NAD83) so no datum shift |
| Grid alignment | Snap to NLCD/LANDFIRE 30 m grid (origin x = −2,493,045, y = 3,310,005) | Most likely grid for the ML team's 30 m inputs; configurable |
| Tiling | Fixed 3,750 m square tiles | LCM-friendly: 10×10 ML windows, 125×125 ML cells, 375 intervals at 10 m |
| Detail | 10 m base, 4 LODs, fixed tile grid | Server GPU budget is large but not unlimited; far terrain needs less detail |
| Mesh format | glTF binary (`.glb`) | Imports into Unity, Unreal, Godot, Blender, three.js |

## 3. Architecture

Two stages, both exposed through `lib.rs` (reusable by a future tile server) and a CLI binary:

```
vr_fire ingest --region california           # once: 3DEP 1° tiles → 10 m height tiles (store/)
vr_fire bake   --tiles 812,1440..815,1443    # on demand: height tiles → meshes + metadata (tiles/)
```

`ingest` is slow and IO-bound and runs once per region. `bake` is fast, runs per tile, and is cacheable.
Both parallelize across tiles with `rayon`.

### Modules

| Module | Responsibility | Depends on |
|---|---|---|
| `grid` | Tile / cell / window math: `(tx, ty)` ↔ EPSG:5070 bounds ↔ lat/lon bbox ↔ ML cell indices. Single source of truth for alignment. | `proj4rs` |
| `source` | Map an area to the covering 3DEP 1° tiles; resumable download into `cache/`; integrity check. | `ureq`, `grid` |
| `dem` | Read GeoTIFF → `Raster { data: Vec<f32>, width, height, geotransform, epsg, nodata }`. | `tiff` |
| `reproject` | Resample source rasters onto a tile's EPSG:5070 node grid. | `proj4rs`, `dem`, `grid` |
| `store` | Read/write 10 m height tiles as f32 GeoTIFF + index (including `empty` tiles). | `tiff`, `grid` |
| `mesh` | Height tile (+ neighbor edges) → `Mesh { positions, normals, uvs, indices }` at a given LOD, with skirts. | `store` |
| `export` | Write `.glb`, `.f32`, `tile.json`. | `gltf-json`, `serde_json` |
| `cli` | `clap` commands `ingest` and `bake`. | all |

## 4. Grid

- **CRS:** EPSG:5070. Grid origin `O = (Ox, Oy)` = NLCD CONUS NW corner (−2,493,045, 3,310,005);
  30 m cell edges therefore sit at 15 mod 30. Origin and CRS are config values.
- **Tile `(tx, ty)`:** `x_min = Ox + 3750·tx`, `x_max = x_min + 3750`,
  `y_max = Oy − 3750·ty`, `y_min = y_max − 3750`. `ty` grows southward (raster convention).
  Tile IDs are integers and never change.
- **ML cells:** 125 × 125 per tile. Global cell `(C, R)` → tile `(C div 125, R div 125)`,
  local `(C mod 125, R mod 125)`. Integer math only.
- **Sample nodes:** heights are sampled at nodes `(x_min + 10·i, y_max − 10·j)`, `i, j ∈ 0..=375`
  (376 × 376). Every 30 m cell corner is a node. Adjacent tiles sample identical coordinates on
  shared edges, so edge heights are bit-identical.
- **375 m windows:** anchored at tile origins (10 × 10 per tile). See Open Question 1.

## 5. Ingest

1. Enumerate tiles intersecting the region (California: a bundled simplified boundary polygon;
   tiles fully outside are skipped). Roughly 30–40k tiles.
2. For each tile, compute its lat/lon bbox (+ margin) and the covering 3DEP 1° tiles
   (named by NW corner, e.g. `n39w121`); download any missing ones into `cache/` (resumable).
3. **Reproject:** for each node, inverse-project EPSG:5070 → NAD83 lon/lat with `proj4rs`,
   locate the source pixel via that file's own geotransform (handles 3DEP's overlap buffer),
   bilinear-sample. Nodes near a 1° boundary sample from whichever source contains them.
4. **Nodata:** if some of the 4 source pixels are nodata, use the weight-normalized valid ones;
   if all are nodata, mark missing and fill afterwards by iterative neighbor averaging.
   Tiles with no valid data (open ocean) are recorded as `empty` and not written.
5. Write `store/{tx}_{ty}.tif` (f32, Deflate, GeoTIFF tags) and update `store/index.json`
   (status, min/max elevation, filled-sample count, source file names).

Elevations are NAVD88 meters, passed through unchanged. Ingest is idempotent: existing store
tiles are skipped unless `--force`.

Performance: ~4×10⁹ projections for California, parallel over tiles; expected tens of minutes on a
desktop. If projection dominates, project a coarse lattice and interpolate (sub-mm error) —
only if profiling shows it's needed.

## 6. Bake

### LODs

Spacing must divide 3,750 m and be a multiple of 10 m, so each LOD is an exact subset of LOD 0.

| LOD | Spacing | Nodes | Triangles (excl. skirts) |
|---|---|---|---|
| 0 | 10 m | 376² | 281,250 |
| 1 | 30 m | 126² | 31,250 |
| 2 | 150 m | 26² | 1,250 |
| 3 | 750 m | 6² | 50 |

- Coarser LODs **subsample** (every k-th node), preserving bit-identical shared edges at every LOD
  and putting LOD 1 vertices exactly on ML cell corners.
- **Skirts:** each edge gets a vertical strip dropping 50 m below the edge vertices, hiding cracks
  between neighbors at different LODs.

### Mesh attributes

- **Positions:** tile-local, origin at tile NW corner, glTF convention: +X = east,
  +Y = elevation (absolute NAVD88 m), −Z = north. All magnitudes < ~4.5 km → sub-mm float32 precision.
- **Normals:** central differences on the full 10 m field, using neighbor tiles' edge rows
  (from `store`) at borders so shading is seamless; coarse LODs take normals from the 10 m field
  at their node positions. Missing neighbor (store edge / empty) → one-sided difference.
- **UV0:** 0→1 across the tile (u east, v south). ML cell `(c, r)` occupies
  `[c/125, (c+1)/125] × [r/125, (r+1)/125]`, so a 125×125 fire texture maps with no remapping.
- **Indices:** u32 (LOD 0 exceeds 65,535 vertices).
- No mesh compression (server-side rendering loads from local disk).

### Outputs

| Path | Content |
|---|---|
| `tiles/lod{n}/{tx}_{ty}.glb` | Mesh per LOD |
| `tiles/{tx}_{ty}.f32` | Raw 376×376 little-endian f32 heights, row-major north→south |
| `tiles/{tx}_{ty}.json` | Tile ID, EPSG:5070 bounds (f64), grid origin, CRS, LOD spacings, min/max elevation, filled-sample count, source files, pipeline version |

**Placement in the renderer:** use a per-session floating origin; tile world position =
`(x_min − scene_x, 0, −(y_max − scene_y))` computed in f64, then cast. Raw EPSG:5070 values
(~2×10⁶ m) must never go into float32 vertex data.

## 7. Error handling

- Network: retries with backoff; partial downloads resume; a failed source tile fails only the
  tiles that need it, and they are reported at the end (exit code non-zero).
- Corrupt/unreadable GeoTIFF: delete from cache, re-download once, then fail that tile.
- Unexpected source CRS (not EPSG:4269/4326): hard error naming the file.
- `bake` on a tile missing from the store: error; on an `empty` tile: skip with a notice.

## 8. Testing

- `grid`: tile ↔ bounds ↔ cells round trips; adjacent tiles share edge node coordinates exactly.
- `reproject`: EPSG:5070 ↔ lon/lat against reference points generated once with `pyproj`
  (committed as fixtures); resampling a synthetic plane/slope raster within 1 mm; nodata fill.
- `mesh`: vertex/triangle counts per LOD; adjacent tiles have bit-identical edge vertices at every
  LOD; normals are unit length.
- `export`: `.glb` reloads with the `gltf` crate; bounds, index ranges, attribute counts correct.
- End-to-end on one real 3DEP 1° tile: `#[ignore]` (network), run manually.
- Manual: open a baked tile in Blender / a glTF viewer; open a store tile in QGIS over a basemap.

## 9. Open questions

1. **375 m windows vs 30 m cells.** 375 / 30 = 12.5, so window edges alternately bisect cells.
   The ML team must choose (e.g. 360 m = 12 cells, 390 m = 13 cells, or resampling). Terrain work
   is not blocked; only window anchoring in `grid` changes.
2. **ML grid CRS/origin.** Assumed EPSG:5070 on the NLCD/LANDFIRE grid. Confirm with the ML team;
   both are config values.
3. **Higher resolution.** 3DEP 1 m lidar covers much of California. Could replace LOD 0 near the
   fire later; not in this scope.
4. **Background terrain.** Cesium for Unity (streamed global terrain) could supply distant
   terrain instead of LOD 2–3 tiles. Decide during renderer work.
