# viewer: California terrain + monster truck

A Bevy 0.19 app that shows all of California's terrain, zoomable from the whole state
down to 10 m, with a green monster truck you can drop anywhere and drive. It runs
natively and in the browser (WebGPU, with a WebGL2 fallback), and supports multiplayer
through a small WebSocket relay.

Live: https://chilos.dev/vr_fire/

## Run it

```bash
cargo run --release -p viewer
```

Terrain streams from USGS on demand, so the first view of an area takes a few seconds.

## Controls

**Map**

| Input | Action |
|---|---|
| Scroll | Zoom toward the cursor |
| Left-drag | Pan |
| Right-drag | Orbit |
| Click | Drop the truck there |
| Shift+click or F | Fetch 1 m lidar for that 3.75 km tile (about 30 s on first request, then cached) |
| C | Switch terrain between compressed tiles and raw USGS COG |
| 1–6 | Fly to Yosemite Valley, Lake Tahoe, Mt. Shasta, Death Valley, Placerville, Los Angeles |
| M | Back to your truck (from driving, M jumps to an aerial view over the truck) |

**Driving**

| Input | Action |
|---|---|
| W / S or ↑ / ↓ | Throttle / brake and reverse |
| A / D or ← / → | Steer (in the air: turn left or right) |
| Space | Mega boost (drains the boost bar, which refills over time) |
| W / S in the air | Pitch nose down / up (the truck also levels itself gently) |
| R | Reset upright |
| M or Esc | Back to the map |
| Mouse (pointer locked) | Look around; the camera swings back behind the truck when you drive and leave the mouse still |
| Scroll | Chase-camera distance |
| F | Fetch 1 m lidar for the tile under the truck |

While driving:
- A **minimap** in the corner shows California, the 20 km detailed-terrain window around
  you, and a dot for every player.
- **Waypoints** at the screen edge point to other players who are off-screen or more than
  2.5 km away, with their name and distance.
- A **network label** above each nearby truck shows live stats. Yours: round-trip time to
  the relay, messages and bytes per second up and down, and totals. Other trucks: update
  rate, age of their last update, jitter, and smoothing error.

In multiplayer, if another truck flips yours onto its roof, it's game over; press R to
respawn.

## How it works

**Terrain comes straight from USGS.** The USGS 3DEP 1/3 arc-second DEM is published as
cloud-optimized GeoTIFFs on S3, and the bucket allows cross-origin requests. The viewer
reads each file's header, then fetches only the 512×512 pieces it needs with HTTP range
requests. Each file has six resolution levels (about 10 m down to 320 m), so a zoomed-out
view downloads only coarse pieces. No terrain is hosted on our server (`src/cog.rs`).

**Compressed tiles (default).** `vr_fire pack` turns the 10 m store (all of California)
into `.vrh` patches at every level. Each patch is quantized (0.25–1 m steps), 2D-predicted
and Brotli-compressed (`vr_fire::codec`); a 10 m tile is ~49 KB versus ~570 KB as floats.
They're served from `/vr_fire/tiles/` (147,578 files, 1.7 GB). A patch without a file
(near the state line or coast) falls back to COG. Press **C** to compare with raw COG;
the HUD shows megabytes downloaded from each source.

**Level of detail** (`src/terrain.rs`, levels in `vr_fire::lod`). The finest level whose
node spacing is at least 0.4% of the view distance is used, so zooming in refines in
steps of 1.7–3× instead of big pops:

| Patch | Size | Node spacing | Used within about |
|---|---|---|---|
| Super tile | 37.5 km | 750 m | everywhere beyond 62.5 km |
| Grid tile | 3.75 km | 250 m | 62.5 km |
| Grid tile | 3.75 km | 150 m | 37.5 km |
| Grid tile | 3.75 km | 50 m | 12.5 km |
| Grid tile | 3.75 km | 30 m | 7.5 km |
| Grid tile | 3.75 km | 10 m | 2.5 km |
| Lidar tile (on request) | 3.75 km | 3.75 m | 15 km |

**1 m lidar on request** (`../hires/`). Shift+click asks the `hires` service on the
server for that tile. The service calls OpenTopography for USGS 1 m lidar, resamples it
to 3.75 m on our grid, and caches the `.vrh`. The viewer polls and keeps running
meanwhile; the HUD shows progress. The API key stays on the server. Tile IDs only, one
job at a time, 150 OpenTopography calls a day at most.

- Grid tiles are the same EPSG:5070 tiles the pipeline produces, aligned to the ML team's
  30 m grid.
- A super tile is replaced by its 100 children only once all of them are ready, so there
  are no holes.
- Skirts hide cracks between neighbors at different detail.
- Meshes are built within a 4 ms per-frame budget so streaming doesn't cause stutter.
- To keep that fast, only a coarse lattice of points is projected; the rest are
  interpolated (under 2% of node spacing, checked by a test).

**Truck** (`src/truck.rs`) is built from primitives in code. Physics is custom and
arcade-style, running at a fixed 120 Hz:
- four raycast spring-damper wheels on the same terrain triangles that are drawn
- 4-wheel steering, tire grip limited to a friction circle
- body-corner contacts so a flipped truck slides on its roof
- air control, and the boost thrust

Rendering interpolates between physics steps.

**Precision.** World coordinates are meters relative to a movable origin. Dropping the
truck re-centers the world on it, so 32-bit floats stay precise under the wheels.

**Multiplayer** (`src/net.rs`, `../relay/`):
- Each client simulates only its own truck and sends its pose at 20 Hz, using absolute
  EPSG:5070 coordinates.
- The relay tags each message with a player ID and forwards it to everyone else.
- Other trucks are smoothed toward their latest pose. Tall beacons make them findable
  from the map.
- Truck-to-truck contact pushes only your own truck.
- Being upside down for 1.5 s within 4 s of touching another truck is game over.

## Environment variables (native only)

| Variable | Effect |
|---|---|
| `VR_FIRE_RELAY` | Relay URL (default `wss://chilos.dev/vr_fire/ws`), e.g. `ws://127.0.0.1:8795` for a local relay |
| `VR_FIRE_AUTOPILOT_PLACE=1..6` | Which place the autopilot flies to (default 5, Placerville) |
| `VR_FIRE_AUTOPILOT=<dir>` | Scripted demo: flies to Placerville, drops the truck, drives and boosts, saves screenshots to `<dir>`, then exits. Turns off vsync. |

To test multiplayer locally, run `PORT=8795 cargo run --release -p relay`, then start two
viewers with `VR_FIRE_RELAY=ws://127.0.0.1:8795`.

## Web build and deploy

```bash
scripts/deploy_viewer.sh
```

The script does four things:
- builds the WebGPU and WebGL2 variants, and runs `wasm-bindgen` on each
- gzips the wasm; only the `.gz` is uploaded, and nginx serves it with
  `gzip_static always`
- builds the relay as a static musl binary
- copies everything to chilos.dev and restarts the relay

`web/index.html` loads the WebGPU build when the browser offers a WebGPU adapter,
otherwise WebGL2.

One-time server setup is in `deploy/`:
- `nginx-vr_fire.conf`: the static location plus the WebSocket proxy at `/vr_fire/ws`
- `vr-fire-relay.service`: the systemd unit for the relay

Needs `rustup target add wasm32-unknown-unknown x86_64-unknown-linux-musl` and
`wasm-bindgen-cli` matching the `wasm-bindgen` version in `Cargo.lock`.

## Tests

```bash
cargo test --release -p viewer
```

- COG piece decoding is checked against a full-file decode of the real cached 3DEP file.
  It's skipped if `../cache/USGS_13_n39w121.tif` is absent.
- Height lookup follows the mesh triangles.
- Lattice-interpolated lon/lat is checked against direct projection.
- The California super-tile count is checked.

## Known limitations

- The WebGL2 build is deployed but hasn't been tested in a browser. The WebGPU build has
  been tested in Chrome.
- Everything outside California renders as ocean.
- The wasm is 55 MB raw (12 MB gzipped). Turning off unused Bevy features would shrink it
  a lot.
- Player names are random (`trucker-NNNN`).
- Decoding a newly arrived USGS piece isn't time-budgeted, so it can cause a small
  one-frame hitch in the browser.
