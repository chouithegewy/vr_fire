//! HUD diagnostics line: graphics backend, GPU, render size and frame-time percentiles.

/// The `p`-th percentile (0..=1) of `samples`, nearest-rank; `None` when empty.
pub fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = (p * v.len() as f64).ceil() as usize;
    Some(v[rank.clamp(1, v.len()) - 1])
}

/// Friendly backend name: wgpu reports `BrowserWebGpu` / `Gl` on the web.
pub fn backend_name(backend: &str, web: bool) -> &str {
    match (backend, web) {
        ("BrowserWebGpu", _) => "WebGPU",
        ("Gl", true) => "WebGL2",
        _ => backend,
    }
}

/// e.g. `WebGL2 | GPU: (hidden by browser) | 1920x1080 | frame p50 8.3 p99 12.1 max 20.4 ms (120 frames)`.
pub fn line(backend: &str, gpu: &str, size: (u32, u32), frames_ms: &[f64]) -> String {
    let gpu = if gpu.trim().is_empty() { "(hidden by browser)" } else { gpu.trim() };
    let mut s = format!("{backend} | GPU: {gpu} | {}x{}", size.0, size.1);
    if let (Some(p50), Some(p99), Some(max)) = (percentile(frames_ms, 0.5), percentile(frames_ms, 0.99), percentile(frames_ms, 1.0)) {
        s += &format!(" | frame p50 {p50:.1} p99 {p99:.1} max {max:.1} ms ({} frames)", frames_ms.len());
    }
    if backend == "WebGL2" {
        s += " | no WebGPU adapter: check chrome://gpu";
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let ms: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        assert_eq!(percentile(&ms, 0.5), Some(50.0));
        assert_eq!(percentile(&ms, 0.99), Some(99.0));
        assert_eq!(percentile(&ms, 1.0), Some(100.0));
        assert_eq!(percentile(&[7.0], 0.5), Some(7.0));
        assert_eq!(percentile(&[], 0.5), None);
    }

    #[test]
    fn line_names_the_backend_and_hidden_gpu() {
        assert_eq!(backend_name("Gl", true), "WebGL2");
        assert_eq!(backend_name("BrowserWebGpu", true), "WebGPU");
        assert_eq!(backend_name("Vulkan", false), "Vulkan");
        let l = line("WebGL2", " ", (1280, 720), &[8.0, 9.0, 30.0]);
        assert_eq!(l, "WebGL2 | GPU: (hidden by browser) | 1280x720 | frame p50 9.0 p99 30.0 max 30.0 ms (3 frames) | no WebGPU adapter: check chrome://gpu");
        assert_eq!(line("Vulkan", "Arc B580", (800, 600), &[]), "Vulkan | GPU: Arc B580 | 800x600");
    }
}
