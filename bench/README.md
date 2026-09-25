# Terrain rendering benchmark: Unity vs Bevy (native and wasm)

The same scene and camera path in three runtimes, to compare engines before committing to one
for VR.

- **Scene** ([scene.json](scene.json)): 9 tiles at 10 m (a 3×3 block of 3.75 km tiles near
  Placerville, 2,558,250 triangles), 1920×1080, 4× MSAA, no shadows, no tonemapping or HDR,
  vsync off.
- **Path:** a 5 s warm-up, then one 30 s orbit (4 km radius) 300 m above the highest point,
  looking at the block centre.
- **Metric:** frame time over the 30 s (mean/p50/p95/p99/max). Unity also reports its GPU
  frame time (FrameTimingManager).

| Runtime | Source | Output |
|---|---|---|
| Unity 6.3 (URP, Vulkan) | [../unity](../unity) (`VrFireBatch.All`) | `results/unity_linux*.json` |
| Bevy 0.19 native (Vulkan) | [bevy/](bevy) (`cargo run --release -p terrain-bench`) | `results/bevy_native*.json` |
| Bevy 0.19 wasm (WebGPU, WebGL2) | [bevy/web](bevy/web), [scripts/run_web.py](scripts/run_web.py) | `results/bevy_wasm_*.json` |

## Reproduce

```sh
cargo run --release -- bake --tiles 101,351..103,353 --format both --out bench/tiles
~/Unity/Hub/Editor/6000.3.25f1/Editor/Unity -batchmode -nographics -quit -projectPath unity \
   -executeMethod VrFire.EditorTools.VrFireBatch.All
bench/scripts/run_native.sh              # Unity and Bevy native, alternating (ROUNDS=2)
# wasm: build both variants, then run them in Chrome
cargo build --release -p terrain-bench --target wasm32-unknown-unknown --features webgpu
cargo build --release -p terrain-bench --target wasm32-unknown-unknown --target-dir target/webgl2
for v in webgpu webgl2; do d=target; [ $v = webgl2 ] && d=target/webgl2
  wasm-bindgen --target web --no-typescript --out-name terrain_bench --out-dir bench/bevy/web/$v \
    $d/wasm32-unknown-unknown/release/terrain-bench.wasm; done
python bench/scripts/run_web.py          # needs playwright + Chrome
```

## Results (2026-09-25)

Intel Arc B580, Mesa 26.2.2, Ubuntu (GNOME Wayland), 144 Hz monitor. All runs rendered the
same view; the screenshots are saved next to the JSON.

| Runtime | Graphics API | Mean fps | p50 ms | p95 ms | p99 ms | Runs |
|---|---|---:|---:|---:|---:|---|
| Unity 6.3 URP (Wayland) | Vulkan | 999 | 1.00 | 1.07 | 1.09 | 3, identical (GPU p50 0.99 ms) |
| Bevy 0.19 native (Wayland) | Vulkan | 536–544 | 1.78–1.81 | 2.13–2.18 | 2.38–2.45 | 3 |
| Bevy 0.19 wasm, Chrome (headless) | WebGPU | 575 | 1.60 | 2.10 | 3.20 | 1 |
| Bevy 0.19 wasm, Chrome (headless) | WebGL2 | 125 | 8.00 | 8.30 | 8.50 | 1, looks capped |

### Reading these numbers

- **Unity is about 1.8× faster per frame here.** Both are far inside any VR budget on this GPU
  (11.1 ms at 90 Hz). Likely contributors, not yet measured separately: Unity reorders
  imported meshes for the vertex cache, and URP Lit with no extra features is lighter than
  Bevy's clustered-forward `StandardMaterial`. Unity's GPU time equals its frame time, so it
  is GPU-bound; Bevy doesn't report GPU time in this harness, so its CPU/GPU split is
  unknown.
- **Unity's 999 fps sits right at 1.00 ms.** Its GPU time (0.99 ms) says that's real GPU
  work, but a hidden 1 kHz limiter can't be completely ruled out. Unity reaches 1,000 fps
  exactly and the p95 is only 1.07 ms.
- **XWayland caps both engines at the monitor refresh.** Run as X11 windows (Unity's default
  on Linux), Unity and Bevy are both held at exactly 144 fps with vsync off. The table uses
  native Wayland for both (`SDL_VIDEODRIVER=wayland` for Unity).
- **Headless Chrome doesn't present to a screen,** so the WebGPU number measures rendering
  without compositing and isn't directly comparable with the native windows. The WebGL2 run
  is pinned at 8.0–8.5 ms, which looks like a frame cap rather than render cost. Treat both
  browser numbers as a sanity check, not a comparison.

### What this means for VR

A PC-rendered or server-streamed headset (the plan for this project) renders two eyes at
90–120 Hz, roughly 2× the pixels of 1080p per eye on current headsets. At 1–2 ms for this
scene on a mid-range desktop GPU, either engine has plenty of headroom for terrain; the budget
will go to fire effects, vegetation and video encoding. A standalone Quest is a different
class of GPU: it needs the coarser LOD tiles and should be benchmarked on the device
(the Unity project already has the Android and OpenXR packages).
