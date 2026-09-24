//! `.vrh` height-patch codec: fixed-step quantization, 2D Lorenzo prediction, zigzag,
//! byte-shuffle, Brotli. See `compress-lab/` for how this was chosen (~8–18× vs f32).
//!
//! Layout: `b"VRH1"`, side: u16, step: f32, base: i32, wide: u8, then the Brotli stream of
//! byte-shuffled zigzag residuals (u16 each, or u32 when `wide`). All little-endian.

use anyhow::{Result, bail, ensure};
use std::io::Write;

const MAGIC: &[u8; 4] = b"VRH1";
const HEADER: usize = 4 + 2 + 4 + 4 + 1;

fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}
fn unzigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// Encode `side × side` heights (row-major) quantized to `step` metres.
pub fn encode(heights: &[f32], side: usize, step: f32) -> Vec<u8> {
    assert_eq!(heights.len(), side * side);
    let q: Vec<i32> = heights.iter().map(|&h| (h / step).round() as i32).collect();
    let base = q.iter().copied().min().unwrap_or(0);
    let at = |i: usize, j: usize| q[j * side + i] - base;
    let mut res = Vec::with_capacity(q.len());
    for j in 0..side {
        for i in 0..side {
            let pred = match (i, j) {
                (0, 0) => 0,
                (_, 0) => at(i - 1, 0),
                (0, _) => at(0, j - 1),
                _ => at(i - 1, j) + at(i, j - 1) - at(i - 1, j - 1),
            };
            res.push(zigzag(at(i, j) - pred));
        }
    }
    let wide = res.iter().any(|&r| r > u16::MAX as u32);
    let width = if wide { 4 } else { 2 };
    let n = res.len();
    let mut shuffled = vec![0u8; n * width];
    for (k, r) in res.iter().enumerate() {
        for (b, byte) in r.to_le_bytes()[..width].iter().enumerate() {
            shuffled[b * n + k] = *byte;
        }
    }
    let mut out = Vec::with_capacity(HEADER + n);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(side as u16).to_le_bytes());
    out.extend_from_slice(&step.to_le_bytes());
    out.extend_from_slice(&base.to_le_bytes());
    out.push(wide as u8);
    {
        let mut w = brotli::CompressorWriter::new(&mut out, 1 << 16, 11, 22);
        w.write_all(&shuffled).unwrap();
    }
    out
}

/// Decode to (side, heights).
pub fn decode(bytes: &[u8]) -> Result<(usize, Vec<f32>)> {
    ensure!(bytes.len() >= HEADER && &bytes[..4] == MAGIC, "not a VRH1 height patch");
    let side = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let step = f32::from_le_bytes(bytes[6..10].try_into().unwrap());
    let base = i32::from_le_bytes(bytes[10..14].try_into().unwrap());
    let width = if bytes[14] == 1 { 4 } else { 2 };
    let n = side * side;
    let mut shuffled = Vec::with_capacity(n * width);
    brotli::BrotliDecompress(&mut &bytes[HEADER..], &mut shuffled)?;
    if shuffled.len() != n * width {
        bail!("corrupt patch: {} residual bytes, expected {}", shuffled.len(), n * width);
    }
    let mut q = vec![0i32; n];
    for k in 0..n {
        let mut b = [0u8; 4];
        for (w, byte) in b[..width].iter_mut().enumerate() {
            *byte = shuffled[w * n + k];
        }
        let r = unzigzag(u32::from_le_bytes(b));
        let (i, j) = (k % side, k / side);
        let pred = match (i, j) {
            (0, 0) => 0,
            (_, 0) => q[k - 1],
            (0, _) => q[k - side],
            _ => q[k - 1] + q[k - side] - q[k - side - 1],
        };
        q[k] = r + pred;
    }
    Ok((side, q.iter().map(|&v| (v + base) as f32 * step).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terrain(side: usize) -> Vec<f32> {
        (0..side * side)
            .map(|k| {
                let (x, y) = ((k % side) as f32, (k / side) as f32);
                800.0 + 120.0 * (x * 0.031).sin() + 90.0 * (y * 0.047).cos() + 3.0 * (x * 0.9 + y * 1.3).sin()
            })
            .collect()
    }

    #[test]
    fn round_trips_within_half_a_step() {
        for step in [0.1f32, 0.25, 1.0] {
            let h = terrain(130);
            let bytes = encode(&h, 130, step);
            let (side, back) = decode(&bytes).unwrap();
            assert_eq!(side, 130);
            let max = h.iter().zip(&back).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(max <= step / 2.0 + 1e-3, "step {step}: max err {max}");
        }
    }

    #[test]
    fn compresses_smooth_terrain_well() {
        let h = terrain(378);
        let raw = h.len() * 4;
        let packed = encode(&h, 378, 0.25).len();
        assert!(packed * 6 < raw, "{packed} bytes vs {raw} raw");
    }

    #[test]
    fn handles_cliffs_needing_wide_residuals() {
        // Ocean fill next to a 2,000 m cliff: residuals exceed 16 bits at 1 cm.
        let mut h = vec![-300.0f32; 40 * 40];
        for (k, v) in h.iter_mut().enumerate() {
            if k % 40 > 20 {
                *v = 2000.0;
            }
        }
        let (_, back) = decode(&encode(&h, 40, 0.01)).unwrap();
        assert!(h.iter().zip(&back).all(|(a, b)| (a - b).abs() <= 0.006));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"nope").is_err());
        assert!(decode(b"VRH1\x05").is_err());
    }
}
