# Terrain data budget: tiles, triangles, bytes, and 1 m detail

Measured 2026-09-25/26 from the repository and the statewide store on the development machine.

## Tiles

- A tile is **3.75 km × 3.75 km (14.06 km²)** on the EPSG:5070 (NLCD-aligned) grid.
- It lines up with the fire model: **10 × 10 windows** of 375 m, or **125 × 125 cells** of 30 m.
- Heights are **376 × 376 nodes at 10 m** (edge rows are shared with neighbours).
- **California: 30,826 tiles**, all `ok` in `store/index.json`. That's about 433,500 km², slightly
  more than the state's 424,000 km², because boundary tiles overhang the coast and borders.
- For far views the viewer groups tiles into **370 super tiles** (37.5 km, 10 × 10 tiles each).

## Triangles

A patch with n × n nodes has 2(n−1)² surface triangles plus 8(n−1) skirt triangles. Unity's import
count for a 10 m tile (284,250) matches this.

**Viewer patches.** The mesh size depends only on the level, not the source: packed `.vrh`
and raw USGS COG (the C key) build identical meshes.

| Patch | Node spacing | Nodes per side | Triangles |
|---|---:|---:|---:|
| Tile | 10 m | 376 | 284,250 |
| Tile | 30 m | 126 | 32,250 |
| Tile | 50 m | 76 | 11,850 |
| Tile | 150 m | 26 | 1,450 |
| Tile | 250 m | 16 | 570 |
| Super tile (37.5 km) | 750 m | 51 | 5,400 |
| "1 m" lidar tile | 3.75 m | 1,001 | 2,008,000 |

**Baked meshes** (`vr_fire bake`, glb/fbx): lod0 10 m 284,250; lod1 30 m 32,250; lod2 150 m
1,450; lod3 750 m 90.

**Scenes:** the desktop benchmark (9 × 10 m tiles) is 2,558,250 triangles; the Quest build
(9 × 30 m tiles) is 290,250.

**Whole state at one level:** about 8.8 billion triangles at 10 m, 1.0 billion at 30 m,
45 million at 150 m, and 2.0 million for super tiles only. That's why levels are mixed by
distance.

**Quest 2 budget:** the rough guide is about 750k–1M triangles per frame at 72 Hz. The Quest
scene should fit, but it hasn't been measured on a headset. The 10 m tiles only fit next to the
viewer, and a lidar tile doesn't fit at all.

## Bytes

**Packed `.vrh` terrain** (compressed heights, the web viewer's source):

| Level | Files | Size |
|---|---:|---:|
| 10 m | 29,467 | 1,317 MB (~45 KB per tile) |
| 30 m | 29,467 | 271 MB |
| 50 m | 29,467 | 101 MB |
| 150 m | 29,467 | 19 MB |
| 250 m | 29,467 | 10 MB |
| Super tiles | 243 | 0.6 MB |
| **Total** | | **≈ 1.72 GB** |

- 29,467 of the 30,826 tiles are packed. Edge tiles missing neighbours are skipped, and the
  viewer uses COG there.
- All of it takes about 2.3 min at 100 Mbit/s. A real session fetches only what's near: an
  earlier drive used about 3 MB of terrain.

**The same terrain in other forms (10 m, whole state):**

| Format | Size |
|---|---:|
| Packed `.vrh` | 1.3 GB |
| Raw 32-bit float heights (`.f32`, 565 KB per tile) | 17.4 GB |
| Statewide store (`store/`, GeoTIFF) | 13 GB |
| `.fbx` meshes (4.2 MB per tile) | ~130 GB |
| `.glb` meshes (7.7 MB per tile) | ~237 GB |

Send compressed heights and build meshes on the device, not model files.

**Imagery** isn't included above. It streams from USGS's tile service, not our server, and costs
more than the terrain: an earlier drive used about 15–25 MB of imagery against about 3 MB of
terrain. Full-resolution statewide imagery runs to terabytes, so it's fetched per view.

**Server-rendered VR** sends no terrain to the headset, only video of both eyes. Bandwidth depends
on time and quality, not on how much of California you visit. Commercial Quest streaming (Air Link,
Virtual Desktop) typically runs at about 50–200 Mbit/s; at 100 Mbit/s that's about 45 GB per
headset per hour.

## 1 m lidar detail

The "1 m lidar" tiles aren't 1 m meshes. The `hires` service resamples OpenTopography's 1 m data to
a **3.75 m grid** (1,001 × 1,001 nodes), which gives 2,008,000 triangles. A true 1 m mesh would be
3,751 × 3,751 nodes, about **28 M triangles per tile**. Geometry doesn't scale, so the detail has
to move into textures and shaders.

**Option 1: baked normal map on a coarse mesh (near term).**
- Bake the 1 m lidar into a normal map (3,750 × 3,750, about 7 MB block-compressed) and put it on
  the 30 m mesh (32k triangles).
- Lighting shows every 1 m ridge, gully and road cut.
- Silhouettes and the truck's wheels still follow the 30 m surface.
- Best for mid and far views, and a good fit for the Quest. The viewer's ground-detail textures
  (`near.wgsl`) are a lighter version of the same idea.

**Option 2: height texture plus a GPU-displaced grid (the proper terrain renderer).**
- Heights live in textures (for example 16-bit, relative to the tile's minimum, 0.1 m steps).
- One reusable grid mesh around the camera, dense near and sparse far (clipmaps / CDLOD); the
  vertex shader reads each vertex's height.
- The triangle count is set by the screen, typically a few hundred thousand, **regardless of data
  resolution**. 1 m data costs memory and bandwidth (about 28 MB per tile on the GPU uncompressed),
  not triangles.
- Silhouettes and physics can use the true heights.
- This is how Unity Terrain and most large-world engines work. Bevy has nothing built in, so it
  would be custom WGSL.

**Not suitable:** tessellation shaders (unsupported in WebGPU/wgpu) and parallax occlusion mapping
(too expensive per pixel on a Quest).

**Plan:** prototype option 1 for the lidar tiles first (2 M → 32k triangles per lidar patch,
detail kept in lighting). Move to option 2 as the long-term renderer; it also fits server-side
streaming, where the server renders the full detail and the headset never sees the triangle count.
