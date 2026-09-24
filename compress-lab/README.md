# compress-lab: how small can terrain heights get?

A benchmark of lossless and lossy ways to compress f32 terrain height grids, run on real
USGS 3DEP data. It came out of asking how little data we'd need to store or send terrain
tiles.

## Run it

```bash
cargo run --release -p compress-lab -- store/*.tif
```

Any single-band f32 GeoTIFF works: the pipeline's 10 m store tiles, or a crop of a 3DEP
file. It prints a Markdown table.

## Methods

**Lossless:**
- raw f32
- f32 + zstd
- f32 with byte-shuffle or bit-shuffle, then zstd (the Blosc / bitshuffle trick)

**Lossy:**
- **u16 range quantization + row delta + zstd or Brotli.** Heights are mapped linearly
  from [min, max] onto 65,535 steps, then each is stored as its difference from its
  left neighbor.
- **Fixed-step quantization + 2D predictor + zstd or Brotli.** Heights are rounded to a
  fixed step (1 cm to 1 m). Each one is predicted from its west, north and northwest
  neighbors (west + north − northwest), and only the error is stored. Terrain is locally
  close to flat, so most errors are tiny.

In both lossy methods, residuals are zigzag-encoded, packed as u16 when they fit, and
byte-shuffled before the entropy coder. Maximum error is exactly half the step size,
whatever the terrain relief. Every method is decoded again and checked for error.

## Results

The 9 store tiles around Placerville (376×376 each, 1.27 M samples, 5.09 MB raw f32),
single core:

| Method | Size | Ratio | Bits/sample | Max error | Decode MB/s |
|---|---:|---:|---:|---:|---:|
| raw f32 | 5090 KB | 1.0× | 32.00 | 0 | 12252 |
| f32 + zstd19 | 4157 KB | 1.2× | 26.14 | 0 | 724 |
| f32 byteshuffle + zstd19 | 2839 KB | 1.8× | 17.85 | 0 | 1205 |
| f32 bitshuffle + zstd19 | 3024 KB | 1.7× | 19.01 | 0 | 136 |
| u16 range-quant + row delta + zstd19 | 1618 KB | 3.1× | 10.17 | 3 mm | 862 |
| u16 range-quant + row delta + brotli11 | 1591 KB | 3.2× | 10.00 | 3 mm | 249 |
| 0.01 m quant + 2D predictor + brotli11 | 1173 KB | 4.3× | 7.37 | 5 mm | 312 |
| 0.1 m quant + 2D predictor + brotli11 | 651 KB | 7.8× | 4.09 | 5 cm | 313 |
| 0.25 m quant + 2D predictor + brotli11 | 470 KB | 10.8× | 2.96 | 12.5 cm | 318 |
| 0.5 m quant + 2D predictor + brotli11 | 357 KB | 14.2× | 2.25 | 25 cm | 323 |
| 1 m quant + 2D predictor + brotli11 | 283 KB | 18.0× | 1.78 | 50 cm | 325 |

zstd19 lands within about 4% of Brotli 11 on every row and decodes 2–3× faster; the full
table prints both.

## Takeaways

- **Quantization is the biggest win, and a fixed step beats range scaling.** A 1 cm step
  already beats u16 range scaling at a similar error (4.3× vs 3.2×).
- **The 2D predictor beats row deltas.** It uses both neighboring directions, so residuals
  cluster tighter around zero.
- **Pick the step by what the data can support.** 3DEP's vertical accuracy is on the order
  of a meter, so 0.1–0.5 m steps (8–14×) lose nothing meaningful, and 1 m gives 18×.
- **Decode speed isn't a bottleneck:** 250–900 MB/s on one core is far faster than any
  network. Brotli can also be served with `Content-Encoding: br`, so a browser decodes
  it natively with no wasm code.
- **Further gains:** a context-modeling entropy coder, or error-bounded wavelet
  compressors (SZ3, zfp), would add roughly another 20–30% at the same error.
  That's diminishing returns.
