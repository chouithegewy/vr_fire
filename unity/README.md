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

## Meta Quest build (OpenXR)

```sh
U=~/Unity/Hub/Editor/6000.3.25f1/Editor/Unity
$U -batchmode -nographics -quit -projectPath unity -buildTarget Android \
   -executeMethod VrFire.EditorTools.VrFireQuest.Setup -logFile unity/Logs/quest-setup.log
$U -batchmode -nographics -quit -projectPath unity -buildTarget Android \
   -executeMethod VrFire.EditorTools.VrFireQuest.Build -logFile unity/Logs/quest-build.log
```

It takes two invocations. `Setup` switches the project to the Input System, which only reaches
the Editor's compiled scripts after a restart. Building in the same session fails with "script
class layout is incompatible between the editor and the player".

| Step | Does |
|---|---|
| `ImportTiles` | `bench/tiles/lod1/*.fbx` (30 m, ~31k triangles per tile, 285k for the block) into `Assets/VrFire/TilesLod1` |
| `ConfigureAndroid` | `dev.chilos.vrfire`; IL2CPP, ARM64, Vulkan, linear colour, min API 32, ASTC; Input System + old input |
| `EnableOpenXR` | OpenXR loader for Android, Meta Quest support, Oculus Touch interaction profile, single-pass instanced (multiview) |
| `BuildScene` | `Quest.unity`: the 9 tiles with mesh colliders, sun, XR rig at the block centre (head tracked by `TrackedPoseDriver`) |
| `BuildApk` | `Build/Quest/vr_fire_quest.apk` (47 MB) |

The APK targets Quest, Quest 2, Pro, 3 and 3S (`com.oculus.supportedDevices`), launches as a VR
app, and needs Unity's OpenJDK 17 module
(`unityhub --headless install-modules --version 6000.3.25f1 -m android-open-jdk-17.0.18+8`).

**Controls** (`QuestRig`): left stick moves where you're looking; right stick left/right snap
turns 30°; right stick up/down climbs or descends. You can't go below the terrain.

**Install:** turn on developer mode for the headset in the Meta Horizon phone app, connect it
by USB and allow debugging in the headset, then

```sh
~/Unity/Hub/Editor/6000.3.25f1/Editor/Data/PlaybackEngines/AndroidPlayer/SDK/platform-tools/adb \
  install -r unity/Build/Quest/vr_fire_quest.apk
```

It appears under Library > Unknown Sources.

## Towards VR

- OpenXR is enabled for Android (the Quest build above). Standalone (Linux) has no loader, so
  the desktop benchmark stays flat-screen; enable OpenXR for Standalone to use PC VR / Quest
  Link.
- A standalone Quest's GPU budget is much tighter than a desktop's: the 2.5 M-triangle 10 m
  scene is a desktop load, so the Quest build uses the 30 m tiles (285k triangles). It hasn't
  been profiled on a headset yet.
- Keep URP: it's Unity's supported pipeline for Quest and single-pass instanced rendering.
