#!/usr/bin/env bash
# Build the Bevy viewer for the web (WebGPU + WebGL2 fallback) and the relay, then deploy to
# chilos.dev/vr_fire. First-time server setup (nginx location, systemd unit) is in viewer/deploy/.
set -euo pipefail
cd "$(dirname "$0")/.."
HOST=${HOST:-thehomiedavid@chilos.dev}
rm -rf dist && mkdir -p dist/webgpu dist/webgl2
cargo build --release -p viewer --target wasm32-unknown-unknown --features webgpu
cargo build --release -p viewer --target wasm32-unknown-unknown --target-dir target/webgl2
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section \
  --out-dir dist/webgpu target/wasm32-unknown-unknown/release/viewer.wasm
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section \
  --out-dir dist/webgl2 target/webgl2/wasm32-unknown-unknown/release/viewer.wasm
cp viewer/web/index.html dist/
# Ship only the gzipped wasm (nginx: gzip_static always) to save server disk.
for v in webgpu webgl2; do gzip -9 dist/$v/viewer_bg.wasm; done
# Server binaries are static musl builds (the VPS has an older glibc); ring needs clang for musl.
export CC_x86_64_unknown_linux_musl=clang
cargo build --release -p relay -p hires --target x86_64-unknown-linux-musl
scp -r dist/index.html dist/webgpu dist/webgl2 "$HOST:/var/www/chilos.dev/vr_fire/"
scp target/x86_64-unknown-linux-musl/release/relay "$HOST:vr_fire/relay.new"
scp target/x86_64-unknown-linux-musl/release/hires "$HOST:vr_fire/hires.new"
ssh "$HOST" 'mv vr_fire/relay.new vr_fire/relay && mv vr_fire/hires.new vr_fire/hires && sudo systemctl restart vr-fire-relay vr-fire-hires'
# Compressed terrain (vr_fire pack → packed/): upload with UPLOAD_TILES=1 (~2 GB, 147k files).
if [ "${UPLOAD_TILES:-0}" = 1 ]; then
  tar cf - -C packed . | ssh "$HOST" 'mkdir -p /var/www/chilos.dev/vr_fire/tiles && tar xf - -C /var/www/chilos.dev/vr_fire/tiles'
fi
echo "deployed: https://chilos.dev/vr_fire/"
