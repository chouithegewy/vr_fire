//! Prototype: how small can f32 terrain heights get? Benchmarks lossless and lossy codecs on
//! real 3DEP data. Usage: compress-lab <tile.tif>... (store tiles or a 3DEP COG crop).

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;
use vr_fire::dem::Raster;

struct Grid {
    w: usize,
    h: usize,
    z: Vec<f32>,
}

/// Encoded stream plus a decoder closure input.
struct Result {
    name: String,
    bytes: usize,
    max_err: f64,
    rms_err: f64,
    enc_ms: f64,
    dec_ms: f64,
}

fn zstd_c(b: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(b, 19).unwrap()
}
fn zstd_d(b: &[u8], cap: usize) -> Vec<u8> {
    zstd::bulk::decompress(b, cap).unwrap()
}
fn brotli_c(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = brotli::CompressorWriter::new(&mut out, 1 << 16, 11, 24);
        w.write_all(b).unwrap();
    }
    out
}
fn brotli_d(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    brotli::BrotliDecompress(&mut &b[..], &mut out).unwrap();
    out
}

/// Byte shuffle: all byte-0s, then byte-1s, ... (blosc SHUFFLE).
fn byte_shuffle(b: &[u8], width: usize) -> Vec<u8> {
    let n = b.len() / width;
    let mut o = vec![0u8; b.len()];
    for i in 0..n {
        for k in 0..width {
            o[k * n + i] = b[i * width + k];
        }
    }
    o
}
fn byte_unshuffle(b: &[u8], width: usize) -> Vec<u8> {
    let n = b.len() / width;
    let mut o = vec![0u8; b.len()];
    for i in 0..n {
        for k in 0..width {
            o[i * width + k] = b[k * n + i];
        }
    }
    o
}
/// Bit shuffle: bit planes of the element stream (bitshuffle/blosc BITSHUFFLE).
fn bit_shuffle(b: &[u8], width: usize) -> Vec<u8> {
    let n = b.len() / width;
    let bits = width * 8;
    let mut o = vec![0u8; b.len()];
    for bit in 0..bits {
        for i in 0..n {
            let v = (b[i * width + bit / 8] >> (bit % 8)) & 1;
            let idx = bit * n + i;
            o[idx / 8] |= v << (idx % 8);
        }
    }
    o
}
fn bit_unshuffle(b: &[u8], width: usize) -> Vec<u8> {
    let n = b.len() / width;
    let bits = width * 8;
    let mut o = vec![0u8; b.len()];
    for bit in 0..bits {
        for i in 0..n {
            let idx = bit * n + i;
            let v = (b[idx / 8] >> (idx % 8)) & 1;
            o[i * width + bit / 8] |= v << (bit % 8);
        }
    }
    o
}

fn f32_bytes(z: &[f32]) -> Vec<u8> {
    z.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn bytes_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}
fn unzigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// 2D Lorenzo predictor residuals: q - (W + N - NW). Terrain is locally planar, so residuals
/// cluster tightly around 0.
fn lorenzo(q: &[i32], w: usize) -> Vec<i32> {
    let at = |i: isize, j: isize| if i < 0 || j < 0 { 0 } else { q[j as usize * w + i as usize] };
    (0..q.len())
        .map(|k| {
            let (i, j) = ((k % w) as isize, (k / w) as isize);
            q[k] - (at(i - 1, j) + at(i, j - 1) - at(i - 1, j - 1))
        })
        .collect()
}
fn unlorenzo(r: &[i32], w: usize) -> Vec<i32> {
    let mut q = vec![0i32; r.len()];
    for k in 0..r.len() {
        let (i, j) = ((k % w) as isize, (k / w) as isize);
        let at = |q: &[i32], i: isize, j: isize| if i < 0 || j < 0 { 0 } else { q[j as usize * w + i as usize] };
        q[k] = r[k] + at(&q, i - 1, j) + at(&q, i, j - 1) - at(&q, i - 1, j - 1);
    }
    q
}
/// Row delta (the scheme in the brief): q[i] - q[i-1], restarting each row from the row above.
fn row_delta(q: &[i32], w: usize) -> Vec<i32> {
    (0..q.len()).map(|k| if k % w == 0 { if k == 0 { q[0] } else { q[k] - q[k - w] } } else { q[k] - q[k - 1] }).collect()
}
fn unrow_delta(r: &[i32], w: usize) -> Vec<i32> {
    let mut q = vec![0i32; r.len()];
    for k in 0..r.len() {
        q[k] = if k % w == 0 { if k == 0 { r[0] } else { r[k] + q[k - w] } } else { r[k] + q[k - 1] };
    }
    q
}

/// Residuals as u16 when they fit (they do for terrain), else u32; byte-shuffled.
fn pack_residuals(r: &[i32]) -> Vec<u8> {
    let z: Vec<u32> = r.iter().map(|&v| zigzag(v)).collect();
    let wide = z.iter().any(|&v| v > u16::MAX as u32);
    let mut out = vec![wide as u8];
    if wide {
        out.extend(byte_shuffle(&z.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(), 4));
    } else {
        out.extend(byte_shuffle(&z.iter().flat_map(|&v| (v as u16).to_le_bytes()).collect::<Vec<_>>(), 2));
    }
    out
}
fn unpack_residuals(b: &[u8]) -> Vec<i32> {
    if b[0] == 1 {
        byte_unshuffle(&b[1..], 4).chunks_exact(4).map(|c| unzigzag(u32::from_le_bytes(c.try_into().unwrap()))).collect()
    } else {
        byte_unshuffle(&b[1..], 2).chunks_exact(2).map(|c| unzigzag(u16::from_le_bytes(c.try_into().unwrap()) as u32)).collect()
    }
}

fn errors(a: &[f32], b: &[f32]) -> (f64, f64) {
    let (mut mx, mut ss) = (0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let d = (*x as f64 - *y as f64).abs();
        mx = mx.max(d);
        ss += d * d;
    }
    (mx, (ss / a.len() as f64).sqrt())
}

fn run(name: &str, g: &Grid, enc: impl Fn(&Grid) -> Vec<u8>, dec: impl Fn(&[u8]) -> Vec<f32>) -> Result {
    let t = Instant::now();
    let bytes = enc(g);
    let enc_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let back = dec(&bytes);
    let dec_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(back.len(), g.z.len(), "{name}: wrong decoded length");
    let (max_err, rms_err) = errors(&g.z, &back);
    Result { name: name.into(), bytes: bytes.len(), max_err, rms_err, enc_ms, dec_ms }
}

/// Quantize to a fixed step with an offset header; returns (offset, ints).
fn quantize(z: &[f32], step: f64) -> (f64, Vec<i32>) {
    let lo = z.iter().fold(f32::MAX, |a, &b| a.min(b)) as f64;
    (lo, z.iter().map(|&v| ((v as f64 - lo) / step).round() as i32).collect())
}
fn header(lo: f64, step: f64) -> Vec<u8> {
    [lo.to_le_bytes(), step.to_le_bytes()].concat()
}
fn read_header(b: &[u8]) -> (f64, f64) {
    (f64::from_le_bytes(b[0..8].try_into().unwrap()), f64::from_le_bytes(b[8..16].try_into().unwrap()))
}

fn benchmark(g: &Grid) -> Vec<Result> {
    let n = g.z.len() * 4;
    let w = g.w;
    let mut out = vec![
        run("raw f32", g, |g| f32_bytes(&g.z), bytes_f32),
        run("f32 + zstd19", g, |g| zstd_c(&f32_bytes(&g.z)), |b| bytes_f32(&zstd_d(b, n))),
        run("f32 byteshuffle + zstd19", g, |g| zstd_c(&byte_shuffle(&f32_bytes(&g.z), 4)), |b| {
            bytes_f32(&byte_unshuffle(&zstd_d(b, n), 4))
        }),
        run("f32 bitshuffle + zstd19", g, |g| zstd_c(&bit_shuffle(&f32_bytes(&g.z), 4)), |b| {
            bytes_f32(&bit_unshuffle(&zstd_d(b, n), 4))
        }),
    ];
    // The brief's pipeline: u16 over [min, max] → row delta → zstd / brotli.
    for (label, codec) in [("zstd19", 0), ("brotli11", 1)] {
        out.push(run(&format!("u16 range-quant + row delta + {label}"), g, |g| {
            let lo = g.z.iter().fold(f32::MAX, |a, &b| a.min(b)) as f64;
            let hi = g.z.iter().fold(f32::MIN, |a, &b| a.max(b)) as f64;
            let step = ((hi - lo) / 65535.0).max(1e-6);
            let (lo, q) = quantize(&g.z, step);
            let body = pack_residuals(&row_delta(&q, w));
            let c = if codec == 0 { zstd_c(&body) } else { brotli_c(&body) };
            [header(lo, step), c].concat()
        }, |b| {
            let (lo, step) = read_header(b);
            let body = if codec == 0 { zstd_d(&b[16..], n * 2) } else { brotli_d(&b[16..]) };
            unrow_delta(&unpack_residuals(&body), w).iter().map(|&q| (lo + q as f64 * step) as f32).collect()
        }));
    }
    // Fixed-step quantization + 2D Lorenzo predictor: error is bounded by step/2 regardless of relief.
    for step in [0.01, 0.1, 0.25, 0.5, 1.0] {
        for (label, codec) in [("zstd19", 0), ("brotli11", 1)] {
            out.push(run(&format!("{step} m quant + 2D Lorenzo + {label}"), g, |g| {
                let (lo, q) = quantize(&g.z, step);
                let body = pack_residuals(&lorenzo(&q, w));
                let c = if codec == 0 { zstd_c(&body) } else { brotli_c(&body) };
                [header(lo, step), c].concat()
            }, |b| {
                let (lo, step) = read_header(b);
                let body = if codec == 0 { zstd_d(&b[16..], n * 2) } else { brotli_d(&b[16..]) };
                unlorenzo(&unpack_residuals(&body), w).iter().map(|&q| (lo + q as f64 * step) as f32).collect()
            }));
        }
    }
    out
}

fn main() {
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    assert!(!paths.is_empty(), "usage: compress-lab <tile.tif>...");
    // Concatenate all tiles row-wise into one benchmark set per method (sum over files).
    let mut totals: Vec<Result> = Vec::new();
    let mut samples = 0usize;
    for p in &paths {
        let r = Raster::read_geotiff(p).unwrap();
        samples += r.data.len();
        let g = Grid { w: r.width, h: r.height, z: r.data };
        let _ = g.h;
        for (k, res) in benchmark(&g).into_iter().enumerate() {
            if let Some(t) = totals.get_mut(k) {
                t.bytes += res.bytes;
                t.max_err = t.max_err.max(res.max_err);
                t.rms_err = (t.rms_err.powi(2) + res.rms_err.powi(2)).sqrt(); // combined below
                t.enc_ms += res.enc_ms;
                t.dec_ms += res.dec_ms;
            } else {
                totals.push(res);
            }
        }
    }
    let raw = (samples * 4) as f64;
    println!("{} files, {} samples, raw {:.2} MB\n", paths.len(), samples, raw / 1e6);
    println!("| method | size | ratio | bits/sample | max err (m) | enc MB/s | dec MB/s |");
    println!("|---|---:|---:|---:|---:|---:|---:|");
    for t in &totals {
        println!(
            "| {} | {:.1} KB | {:.1}× | {:.2} | {:.3} | {:.0} | {:.0} |",
            t.name,
            t.bytes as f64 / 1e3,
            raw / t.bytes as f64,
            t.bytes as f64 * 8.0 / samples as f64,
            t.max_err,
            raw / 1e6 / (t.enc_ms / 1e3),
            raw / 1e6 / (t.dec_ms / 1e3),
        );
    }
}
