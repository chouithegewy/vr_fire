//! vr_fire hires service: on request, fetches USGS 1 m lidar for one grid tile from
//! OpenTopography, resamples it onto the tile's EPSG:5070 grid at 3.75 m (for meshes) and at
//! 1 m (for normal maps), and serves both as `.vrh` patches. The OpenTopography API key never
//! leaves the server.
//!
//! Behind nginx at /vr_fire/hires/:
//!   GET /hires/{tx}_{ty}.vrh     3.75 m patch  → 200 | 202 processing | 404 no lidar here |
//!   GET /hires/{tx}_{ty}_1m.vrh  1 m patch        503 busy (retry) | 429 daily cap reached
//! One OpenTopography download builds both; the 1 m file is written first.
//!
//! Env: OPENTOPOGRAPHY_API_KEY (required), PORT (8797), HIRES_CACHE (./hires-cache),
//!      HIRES_DAILY_CAP (150).

use anyhow::{Context, Result, anyhow, bail};
use proj4rs::Proj;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Response, Server};
use vr_fire::codec::encode;
use vr_fire::crs::Albers;
use vr_fire::dem::Raster;
use vr_fire::grid::{GridSpec, TileId};
use vr_fire::lod::{HIRES_FINE_SPACING_M, HIRES_SPACING_M, HIRES_STEP, hires_fine_nodes, hires_fine_path, hires_nodes, hires_path};

/// Lidar must cover at least this much of the tile, else we report "no lidar here".
const MIN_COVERAGE: f64 = 0.5;
/// Tiles are ~1–2 GB of RAM-heavy work on a small VPS: one at a time.
const MAX_JOBS: usize = 1;

struct State {
    running: HashSet<TileId>,
    day: u64,
    calls_today: u32,
}

/// NAD83 / WGS84 UTM EPSG code → zone.
fn utm_zone(epsg: u16) -> Option<u32> {
    match epsg {
        26901..=26923 => Some((epsg - 26900) as u32),
        6330..=6348 => Some((epsg - 6329) as u32), // NAD83(2011) UTM zone z N = 6329 + z
        32601..=32660 => Some((epsg - 32600) as u32),
        _ => None,
    }
}

fn utm_proj(zone: u32) -> Result<Proj> {
    Proj::from_proj_string(&format!("+proj=utm +zone={zone} +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +units=m +no_defs"))
        .map_err(|e| anyhow!("UTM zone {zone}: {e}"))
}

/// Heights at `nodes`² tile nodes `spacing` apart, plus a one-node ring, sampled from a UTM
/// lidar raster. NaN where the lidar has no data.
fn resample(raster: &Raster, zone: u32, grid: &GridSpec, albers: &Albers, t: TileId, spacing: f64, nodes: usize) -> Result<Vec<f32>> {
    let utm = utm_proj(zone)?;
    let geo = Proj::from_proj_string(vr_fire::crs::NAD83_GEOGRAPHIC).map_err(|e| anyhow!("{e}"))?;
    let b = grid.tile_bounds(t);
    let m = nodes + 2;
    let mut out = Vec::with_capacity(m * m);
    for j in 0..m {
        for i in 0..m {
            let x = b.x_min + (i as f64 - 1.0) * spacing;
            let y = b.y_max - (j as f64 - 1.0) * spacing;
            let (lon, lat) = albers.to_lonlat(x, y)?;
            let mut p = (lon.to_radians(), lat.to_radians(), 0.0);
            proj4rs::transform::transform(&geo, &utm, &mut p).map_err(|e| anyhow!("{e}"))?;
            out.push(raster.sample_bilinear(p.0, p.1).unwrap_or(f32::NAN));
        }
    }
    Ok(out)
}

/// Fill NaN gaps by repeatedly averaging valid 4-neighbours. Returns the valid fraction
/// before filling.
fn fill_gaps(h: &mut [f32], side: usize) -> f64 {
    let valid = h.iter().filter(|v| v.is_finite()).count();
    if valid == 0 {
        return 0.0;
    }
    loop {
        let mut changed = false;
        let snapshot = h.to_vec();
        for k in 0..h.len() {
            if snapshot[k].is_finite() {
                continue;
            }
            let (i, j) = (k % side, k / side);
            let mut sum = 0.0;
            let mut n = 0;
            for (di, dj) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let (a, b) = (i as i64 + di, j as i64 + dj);
                if a >= 0 && b >= 0 && (a as usize) < side && (b as usize) < side {
                    let v = snapshot[b as usize * side + a as usize];
                    if v.is_finite() {
                        sum += v;
                        n += 1;
                    }
                }
            }
            if n > 0 {
                h[k] = sum / n as f32;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    valid as f64 / h.len() as f64
}

/// Fill NaN nodes of a fine ringed grid by bilinear interpolation of an already gap-filled
/// coarse ringed grid covering the same tile (cheap, unlike repeated neighbour averaging on
/// ~14 M nodes).
fn fill_from_coarse(fine: &mut [f32], fine_side: usize, fine_spacing: f64, coarse: &[f32], coarse_side: usize, coarse_spacing: f64) {
    let last = (coarse_side - 1) as f64;
    for (k, v) in fine.iter_mut().enumerate() {
        if v.is_finite() {
            continue;
        }
        let (i, j) = ((k % fine_side) as f64, (k / fine_side) as f64);
        // Metres from the tile's NW corner → coarse index (both grids carry a one-node ring).
        let u = (((i - 1.0) * fine_spacing) / coarse_spacing + 1.0).clamp(0.0, last);
        let w = (((j - 1.0) * fine_spacing) / coarse_spacing + 1.0).clamp(0.0, last);
        let (i0, j0) = ((u.floor() as usize).min(coarse_side - 2), (w.floor() as usize).min(coarse_side - 2));
        let (fu, fw) = ((u - i0 as f64) as f32, (w - j0 as f64) as f32);
        let at = |a: usize, b: usize| coarse[b * coarse_side + a];
        let top = at(i0, j0) * (1.0 - fu) + at(i0 + 1, j0) * fu;
        let bot = at(i0, j0 + 1) * (1.0 - fu) + at(i0 + 1, j0 + 1) * fu;
        *v = top * (1.0 - fw) + bot * fw;
    }
}

/// Which patch a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    /// 3.75 m, for meshes.
    Coarse,
    /// 1 m, for normal maps.
    Fine,
}

fn patch_path(t: TileId, res: Res) -> String {
    match res {
        Res::Coarse => hires_path(t.tx, t.ty),
        Res::Fine => hires_fine_path(t.tx, t.ty),
    }
}

enum Outcome {
    Ready,
    NoLidar(String),
}

fn build(t: TileId, key: &str, cache: &Path) -> Result<Outcome> {
    let grid = GridSpec::default();
    let albers = Albers::new()?;
    let ll = albers.lonlat_bounds(&grid.tile_bounds(t))?.padded(0.0015);
    let url = format!(
        "https://portal.opentopography.org/API/usgsdem?datasetName=USGS1m&south={}&north={}&west={}&east={}&outputFormat=GTiff&API_Key={key}",
        ll.south, ll.north, ll.west, ll.east
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent.get(&url).call().context("OpenTopography request")?;
    let status = resp.status().as_u16();
    let body = resp.into_body().with_config().limit(1 << 30).read_to_vec()?;
    let is_tiff = body.starts_with(b"II*\0") || body.starts_with(b"MM\0*");
    if !is_tiff {
        let text = String::from_utf8_lossy(&body[..body.len().min(300)]).trim().to_string();
        let msg = if text.is_empty() { "no USGS 1 m lidar for this tile".to_string() } else { text };
        if status == 200 || status == 204 || status == 400 || msg.to_lowercase().contains("no data") {
            return Ok(Outcome::NoLidar(msg));
        }
        bail!("OpenTopography HTTP {status}: {msg}");
    }
    let tmp = cache.join(format!("{t}.download.tif"));
    std::fs::write(&tmp, &body)?;
    drop(body);
    let raster = Raster::read_geotiff(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let raster = raster?;
    let zone = utm_zone(raster.epsg).with_context(|| format!("unexpected lidar CRS EPSG:{}", raster.epsg))?;
    let mut h = resample(&raster, zone, &grid, &albers, t, HIRES_SPACING_M, hires_nodes())?;
    let side = hires_nodes() + 2;
    let coverage = fill_gaps(&mut h, side);
    if coverage < MIN_COVERAGE {
        return Ok(Outcome::NoLidar(format!("1 m lidar covers only {:.0}% of this tile", coverage * 100.0)));
    }
    // 1 m grid from the same download, gaps filled from the 3.75 m grid. Written first, so a
    // 3.75 m file on disk always has its 1 m partner.
    let fine_side = hires_fine_nodes() + 2;
    let mut fine = resample(&raster, zone, &grid, &albers, t, HIRES_FINE_SPACING_M, hires_fine_nodes())?;
    drop(raster);
    fill_from_coarse(&mut fine, fine_side, HIRES_FINE_SPACING_M, &h, side, HIRES_SPACING_M);
    write_atomic(&cache.join(hires_fine_path(t.tx, t.ty)), &encode(&fine, fine_side, HIRES_STEP))?;
    drop(fine);
    write_atomic(&cache.join(hires_path(t.tx, t.ty)), &encode(&h, side, HIRES_STEP))?;
    Ok(Outcome::Ready)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path.with_extension("tmp"), bytes)?;
    std::fs::rename(path.with_extension("tmp"), path)?;
    Ok(())
}

fn parse(url: &str) -> Option<(TileId, Res)> {
    let name = url.strip_prefix("/hires/")?.strip_suffix(".vrh")?;
    let (name, res) = match name.strip_suffix("_1m") {
        Some(n) => (n, Res::Fine),
        None => (name, Res::Coarse),
    };
    let (a, b) = name.split_once('_')?;
    let t = TileId::new(a.parse().ok()?, b.parse().ok()?);
    // California's grid tiles sit well inside this box; reject anything else.
    ((0..1000).contains(&t.tx) && (0..1000).contains(&t.ty)).then_some((t, res))
}

fn text(code: u16, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body).with_status_code(code).with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
}

fn main() -> Result<()> {
    let key = std::env::var("OPENTOPOGRAPHY_API_KEY").context("OPENTOPOGRAPHY_API_KEY not set")?;
    let port = std::env::var("PORT").unwrap_or_else(|_| "8797".into());
    let cache = PathBuf::from(std::env::var("HIRES_CACHE").unwrap_or_else(|_| "hires-cache".into()));
    let cap: u32 = std::env::var("HIRES_DAILY_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(150);
    std::fs::create_dir_all(&cache)?;
    let server = Server::http(format!("127.0.0.1:{port}")).map_err(|e| anyhow!("{e}"))?;
    eprintln!("hires on 127.0.0.1:{port}, cache {}", cache.display());
    let state = Arc::new(Mutex::new(State { running: HashSet::new(), day: 0, calls_today: 0 }));
    for req in server.incoming_requests() {
        let Some((t, res)) = parse(req.url()) else {
            let _ = req.respond(text(400, "expected /hires/{tx}_{ty}.vrh or /hires/{tx}_{ty}_1m.vrh"));
            continue;
        };
        // Tiles built before the 1 m patch existed have only the 3.75 m file; a 1 m request
        // rebuilds them (one OpenTopography call, like any first request).
        let file = cache.join(patch_path(t, res));
        let none = cache.join(hires_path(t.tx, t.ty)).with_extension("none");
        if let Ok(bytes) = std::fs::read(&file) {
            let resp = Response::from_data(bytes)
                .with_header(Header::from_bytes("Content-Type", "application/octet-stream").unwrap())
                .with_header(Header::from_bytes("Cache-Control", "public, max-age=86400").unwrap());
            let _ = req.respond(resp);
            continue;
        }
        if let Ok(msg) = std::fs::read_to_string(&none) {
            let _ = req.respond(text(404, &msg));
            continue;
        }
        let mut st = state.lock().unwrap();
        if st.running.contains(&t) {
            let _ = req.respond(text(202, "processing"));
            continue;
        }
        if st.running.len() >= MAX_JOBS {
            let _ = req.respond(text(503, "busy with another tile, retry shortly"));
            continue;
        }
        let day = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() / 86_400;
        if st.day != day {
            st.day = day;
            st.calls_today = 0;
        }
        if st.calls_today >= cap {
            let _ = req.respond(text(429, "daily OpenTopography request cap reached"));
            continue;
        }
        st.calls_today += 1;
        st.running.insert(t);
        drop(st);
        let _ = req.respond(text(202, "processing"));
        let (key, cache, state) = (key.clone(), cache.clone(), state.clone());
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            match build(t, &key, &cache) {
                Ok(Outcome::Ready) => eprintln!("{t}: ready in {:.0}s", started.elapsed().as_secs_f32()),
                Ok(Outcome::NoLidar(msg)) => {
                    eprintln!("{t}: no lidar ({msg})");
                    let _ = std::fs::write(cache.join(hires_path(t.tx, t.ty)).with_extension("none"), msg);
                }
                Err(e) => eprintln!("{t}: failed, will retry on next request: {e:#}"),
            }
            state.lock().unwrap().running.remove(&t);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vr_fire::dem::EPSG_NAD83_GEOGRAPHIC;

    #[test]
    fn parses_both_patch_urls() {
        assert_eq!(parse("/hires/102_352.vrh"), Some((TileId::new(102, 352), Res::Coarse)));
        assert_eq!(parse("/hires/102_352_1m.vrh"), Some((TileId::new(102, 352), Res::Fine)));
        assert_eq!(parse("/hires/102_352_2m.vrh"), None);
        assert_eq!(parse("/hires/5000_1.vrh"), None);
    }

    #[test]
    fn fine_gaps_are_filled_from_the_coarse_grid() {
        // Coarse 3.75 m grid (with its one-node ring) holding h = x + 2·y metres from the NW corner.
        let (cs, cn) = (3.75, 5usize);
        let cm = cn + 2;
        let coarse: Vec<f32> = (0..cm * cm).map(|k| {
            let (x, y) = (((k % cm) as f64 - 1.0) * cs, ((k / cm) as f64 - 1.0) * cs);
            (x + 2.0 * y) as f32
        }).collect();
        let (fs, fnodes) = (1.0, 12usize);
        let fm = fnodes + 2;
        let mut fine = vec![f32::NAN; fm * fm];
        fine[0] = 42.0; // a real sample stays
        fill_from_coarse(&mut fine, fm, fs, &coarse, cm, cs);
        assert_eq!(fine[0], 42.0);
        for (i, j) in [(1usize, 1usize), (5, 3), (12, 12)] {
            let (x, y) = ((i as f64 - 1.0) * fs, (j as f64 - 1.0) * fs);
            assert!((fine[j * fm + i] as f64 - (x + 2.0 * y)).abs() < 1e-3, "node ({i},{j})");
        }
    }

    #[test]
    fn utm_zones() {
        assert_eq!(utm_zone(26910), Some(10));
        assert_eq!(utm_zone(26911), Some(11));
        assert_eq!(utm_zone(6339), Some(10));
        assert_eq!(utm_zone(32611), Some(11));
        assert_eq!(utm_zone(EPSG_NAD83_GEOGRAPHIC), None);
    }

    #[test]
    fn fills_gaps_from_neighbours() {
        let mut h = vec![1.0, f32::NAN, 3.0, f32::NAN, f32::NAN, f32::NAN, 7.0, f32::NAN, 9.0];
        let valid = fill_gaps(&mut h, 3);
        assert!((valid - 4.0 / 9.0).abs() < 1e-9);
        assert!(h.iter().all(|v| v.is_finite()));
        assert_eq!(h[1], 2.0);
    }

    #[test]
    fn resamples_a_utm_plane_onto_the_tile_grid() {
        let grid = GridSpec::default();
        let albers = Albers::new().unwrap();
        let (x, y) = albers.from_lonlat(-120.65, 38.45).unwrap();
        let t = grid.tile_containing(x, y);
        // A 1 m raster in UTM 10N covering the tile, holding z = 100 + 0.01·E − 0.02·N (km-scale plane).
        let ll = albers.lonlat_bounds(&grid.tile_bounds(t)).unwrap().padded(0.002);
        let utm = utm_proj(10).unwrap();
        let geo = Proj::from_proj_string(vr_fire::crs::NAD83_GEOGRAPHIC).unwrap();
        let corner = |lon: f64, lat: f64| {
            let mut p = (lon.to_radians(), lat.to_radians(), 0.0);
            proj4rs::transform::transform(&geo, &utm, &mut p).unwrap();
            (p.0, p.1)
        };
        let (e0, n1) = corner(ll.west, ll.north);
        let (e1, n0) = corner(ll.east, ll.south);
        let (e0, n1) = (e0.min(corner(ll.west, ll.south).0) - 50.0, n1.max(corner(ll.east, ll.north).1) + 50.0);
        let (w, hgt) = ((e1 - e0 + 100.0) as usize / 4, (n1 - n0 + 100.0) as usize / 4);
        let plane = |e: f64, n: f64| 100.0 + 0.01 * (e - e0) - 0.02 * (n - n0);
        let data = (0..w * hgt).map(|k| plane(e0 + ((k % w) as f64 + 0.5) * 4.0, n1 - ((k / w) as f64 + 0.5) * 4.0) as f32).collect();
        let raster = Raster { width: w, height: hgt, data, origin_x: e0, origin_y: n1, pixel_w: 4.0, pixel_h: 4.0, epsg: 26910, nodata: None };
        let h = resample(&raster, 10, &grid, &albers, t, HIRES_SPACING_M, hires_nodes()).unwrap();
        let m = hires_nodes() + 2;
        // The 1 m grid uses the same mapping (a small corner of it, to keep the test fast).
        let fine = resample(&raster, 10, &grid, &albers, t, 1.0, 20).unwrap();
        let b = grid.tile_bounds(t);
        for (i, j) in [(1usize, 1usize), (15, 9)] {
            let (lon, lat) = albers.to_lonlat(b.x_min + (i as f64 - 1.0), b.y_max - (j as f64 - 1.0)).unwrap();
            let (e, n) = corner(lon, lat);
            assert!((fine[j * 22 + i] as f64 - plane(e, n)).abs() < 0.05, "1 m node ({i},{j})");
        }
        assert_eq!(h.len(), m * m);
        for (i, j) in [(0, 0), (500, 500), (m - 1, 7)] {
            let b = grid.tile_bounds(t);
            let (lon, lat) = albers.to_lonlat(b.x_min + (i as f64 - 1.0) * HIRES_SPACING_M, b.y_max - (j as f64 - 1.0) * HIRES_SPACING_M).unwrap();
            let (e, n) = corner(lon, lat);
            assert!((h[j * m + i] as f64 - plane(e, n)).abs() < 0.05, "node ({i},{j})");
        }
    }
}
