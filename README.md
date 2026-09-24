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
