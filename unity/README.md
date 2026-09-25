# vr_fire Unity project

Unity 6.3 LTS (6000.3.25f1) project for the VR simulator, driven entirely from the command
line. It imports the terrain tiles baked by the Rust pipeline, checks them, builds a benchmark
scene and builds a Linux player. The project uses URP and has OpenXR and XR Plug-in Management
installed, so it's ready for a VR target.

## Why FBX

Unity imports FBX, OBJ and DAE natively; `.glb` needs the glTFast package (included here as
`com.unity.cloud.gltfast`, so glb works too). For assets imported in the Editor, FBX is the
native path, so the pipeline can write it:

```sh
cargo run --release -- bake --tiles 101,351..103,353 --format fbx   # or --format both
```

`src/fbx.rs` writes binary FBX 7.4 with zlib-compressed arrays (a 10 m tile is 4.2 MB vs
7.7 MB for glb). Units are metres. Unity flips X when it converts FBX to its left-handed frame,
so the pipeline writes the mesh rotated 180° about Y; in Unity a tile has its NW corner at
the origin, +x east, +z north, and isn't mirrored. `VrFireBatch.Verify` checks this against the
raw heights for every imported tile.

For streaming terrain in VR, don't use model files at all: send the raw `.f32`/`.vrh`
heights and build meshes at runtime, as the Bevy viewer does. FBX is for Editor workflows and
fixed scenes such as this benchmark.

## Headless build

```sh
U=~/Unity/Hub/Editor/6000.3.25f1/Editor/Unity
cargo run --release -- bake --tiles 101,351..103,353 --format both --out bench/tiles
$U -batchmode -nographics -quit -projectPath unity \
   -executeMethod VrFire.EditorTools.VrFireBatch.All -logFile unity/Logs/batch.log
```

`All` runs these steps, each also callable on its own with `-executeMethod`:

| Method | Does |
|---|---|
| `ImportTiles` | Copies `bench/tiles/lod0/*.fbx` into `Assets/VrFire/Tiles` (import settings: file scale in metres, imported normals, 32-bit indices, no materials, colliders or animation) |
| `Verify` | For each tile, checks 32-bit indices and that the four corner heights match the `.f32` grid (orientation and scale) |
| `BuildScene` | URP asset (4× MSAA, no HDR, no shadows), sun, flat ambient, benchmark camera, tiles placed from `bench/scene.json` |
| `PrepareXR` | Creates the OpenXR settings asset (the Editor UI normally does this; without it batch builds fail with "Please build again") |
| `BuildLinux` | Linux player in `Build/Linux/vr_fire_bench.x86_64` (Vulkan, OpenGL fallback) |

Options: `-tiles <dir>`, `-heights <dir>`, `-scene <scene.json>`, `-out <player path>`.

Lines prefixed `VRFIRE` in the log report progress; a `VRFIRE FAIL` exits with code 1.

## Running the benchmark player

```sh
SDL_VIDEODRIVER=wayland unity/Build/Linux/vr_fire_bench.x86_64 -screen-width 1920 -screen-height 1080 \
  -screen-fullscreen 0 -benchOut $PWD/bench/results/unity.json -benchShot $PWD/bench/results/unity.png
```

On a Wayland desktop, use `SDL_VIDEODRIVER=wayland`. Under XWayland (the default), both
Unity and Bevy are held to the monitor refresh rate even with vsync off. See
[../bench/README.md](../bench/README.md) for the comparison with Bevy.

## Towards VR

- OpenXR 1.16.1 and XR Plug-in Management 4.6.1 are installed, but no loader is enabled, so
  builds are flat-screen. To run on a headset, enable OpenXR under Project Settings > XR Plug-in
  Management (Standalone for PC VR / Quest Link, Android for a standalone Quest). Then add the
  interaction profile for your controllers.
- The Android module is installed, so a standalone Quest build is one `BuildTarget.Android`
  away. Budget there is much tighter than on a desktop GPU: the 2.5 M-triangle 10 m scene
  here is a desktop load; a Quest wants LOD tiles (30 m and coarser) and single-pass
  instanced stereo.
- Keep URP: it's Unity's supported pipeline for Quest and single-pass instanced rendering.
