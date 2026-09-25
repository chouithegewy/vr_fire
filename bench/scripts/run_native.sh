#!/usr/bin/env bash
# Alternate the Unity and native Bevy benchmarks ROUNDS times (default 2) so both see the same
# machine conditions. Both must present the same way: under XWayland (X11) either engine is held
# to the monitor refresh (144 fps here), so on a Wayland session Unity's SDL window is put on
# native Wayland like Bevy's (SDL_VIDEODRIVER=wayland).
set -euo pipefail
cd "$(dirname "$0")/../.."
ROUNDS=${ROUNDS:-2}
mkdir -p bench/results
cargo build --release -p terrain-bench
for r in $(seq 1 "$ROUNDS"); do
  ${WAYLAND_DISPLAY:+env SDL_VIDEODRIVER=wayland} unity/Build/Linux/vr_fire_bench.x86_64 -screen-width 1920 -screen-height 1080 -screen-fullscreen 0 \
    -benchOut "$PWD/bench/results/unity_linux_r$r.json" -logFile /dev/null >/dev/null 2>&1
  BENCH_OUT="$PWD/bench/results/bevy_native_r$r.json" ./target/release/terrain-bench >/dev/null 2>&1
  echo "round $r done"
done
