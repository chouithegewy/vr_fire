//! USGS 3DEP 1/3 arc-second (~10 m) seamless DEM, published as 1°×1° GeoTIFFs on S3:
//! cell naming, coverage, and a resumable on-disk download cache.

use crate::crs::LonLatBox;
use anyhow::{Result, anyhow, bail};
use rayon::prelude::*;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tiff::decoder::Decoder;

pub const USGS_13_BASE_URL: &str = "https://prd-tnm.s3.amazonaws.com/StagedProducts/Elevation/13/TIFF/current";
const MAX_ATTEMPTS: u32 = 5;

/// A 1°×1° cell named by its NW corner: `n39w121` covers lat [38, 39), lon [−121, −120).
/// Northern/western hemispheres only (CONUS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceCell {
    pub north: i32,
    pub west: i32,
}

impl SourceCell {
    pub fn containing(lon: f64, lat: f64) -> Self {
        Self { north: lat.floor() as i32 + 1, west: (-lon).ceil() as i32 }
    }

    pub fn name(&self) -> String {
        format!("n{:02}w{:03}", self.north, self.west)
    }

    pub fn file_name(&self) -> String {
        format!("USGS_13_{}.tif", self.name())
    }

    pub fn url(&self, base: &str) -> String {
        format!("{base}/{}/{}", self.name(), self.file_name())
    }

    pub fn covering(b: &LonLatBox) -> Vec<Self> {
        let nw = Self::containing(b.west, b.north);
        let se = Self::containing(b.east, b.south);
        let mut out = Vec::new();
        for north in (se.north..=nw.north).rev() {
            for west in (se.west..=nw.west).rev() {
                out.push(Self { north, west });
            }
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Present(PathBuf),
    /// USGS has no file for this cell (open ocean or outside coverage).
    Missing,
}

enum Download {
    Complete,
    NotFound,
}

pub struct SourceCache {
    dir: PathBuf,
    base_url: String,
    retry_delay: Duration,
}

impl SourceCache {
    pub fn new(dir: impl Into<PathBuf>, base_url: impl Into<String>) -> Self {
        Self { dir: dir.into(), base_url: base_url.into(), retry_delay: Duration::from_secs(2) }
    }

    /// Base delay between retries; attempt k waits k × delay.
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    pub fn path(&self, c: SourceCell) -> PathBuf {
        self.dir.join(c.file_name())
    }

    fn missing_marker(&self, c: SourceCell) -> PathBuf {
        self.dir.join(format!("{}.missing", c.file_name()))
    }

    /// Make sure the cell's file is cached, downloading (or resuming) it if needed.
    pub fn ensure(&self, c: SourceCell) -> Result<Fetched> {
        fs::create_dir_all(&self.dir)?;
        if self.missing_marker(c).exists() {
            return Ok(Fetched::Missing);
        }
        let path = self.path(c);
        if path.exists() {
            if is_readable_tiff(&path) {
                return Ok(Fetched::Present(path));
            }
            fs::remove_file(&path)?; // corrupt cache entry: download it again
        }
        match self.download(&c.url(&self.base_url), &path)? {
            Download::NotFound => {
                fs::write(self.missing_marker(c), b"")?;
                Ok(Fetched::Missing)
            }
            Download::Complete if is_readable_tiff(&path) => Ok(Fetched::Present(path)),
            Download::Complete => {
                fs::remove_file(&path)?;
                bail!("downloaded {} is not a readable TIFF", path.display())
            }
        }
    }

    /// Drop a cached file (e.g. it failed to decode) so the next `ensure` downloads it again.
    pub fn invalidate(&self, c: SourceCell) -> Result<()> {
        match fs::remove_file(self.path(c)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// `ensure` every cell, `threads` downloads at a time.
    pub fn prefetch(&self, cells: &[SourceCell], threads: usize) -> Vec<(SourceCell, Result<Fetched>)> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build().expect("thread pool");
        pool.install(|| cells.par_iter().map(|&c| (c, self.ensure(c))).collect())
    }

    fn download(&self, url: &str, dest: &Path) -> Result<Download> {
        let part = dest.with_extension("tif.part");
        let mut last_err = anyhow!("no attempts made");
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(self.retry_delay * attempt);
            }
            let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            let mut req = ureq::get(url);
            if have > 0 {
                req = req.header("Range", format!("bytes={have}-"));
            }
            let resp = match req.call() {
                Ok(r) => r,
                Err(ureq::Error::StatusCode(404)) => return Ok(Download::NotFound),
                Err(ureq::Error::StatusCode(416)) if have > 0 => {
                    // Range starts at EOF: the partial file is already complete.
                    fs::rename(&part, dest)?;
                    return Ok(Download::Complete);
                }
                Err(e) => {
                    last_err = anyhow!(e);
                    continue;
                }
            };
            let mut file = if resp.status().as_u16() == 206 {
                OpenOptions::new().append(true).open(&part)?
            } else {
                File::create(&part)?
            };
            match io::copy(&mut resp.into_body().into_reader(), &mut file) {
                Ok(_) => {
                    file.sync_all()?;
                    drop(file);
                    fs::rename(&part, dest)?;
                    return Ok(Download::Complete);
                }
                Err(e) => last_err = anyhow!(e).context("download interrupted"),
            }
        }
        Err(last_err.context(format!("failed to download {url} after {MAX_ATTEMPTS} attempts")))
    }
}

/// Cheap validity check: the TIFF header and first IFD parse. Full decoding happens at ingest.
fn is_readable_tiff(path: &Path) -> bool {
    File::open(path)
        .ok()
        .and_then(|f| Decoder::new(BufReader::new(f)).ok())
        .and_then(|mut d| d.dimensions().ok())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem::{EPSG_NAD83_GEOGRAPHIC, Raster};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MISSING: SourceCell = SourceCell { north: 10, west: 10 };

    /// Minimal HTTP server: honors `Range: bytes=N-`, 404s for the MISSING cell,
    /// and optionally cuts the first response off halfway through the body.
    fn serve(body: Vec<u8>, truncate_first: bool) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut path, mut start) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if path.is_empty() {
                        path = line.split(' ').nth(1).unwrap_or("").to_string();
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        start = v.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                if path.contains(&MISSING.name()) {
                    let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                let rest = &body[start..];
                let head = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        start, body.len() - 1, body.len(), rest.len()
                    )
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len())
                };
                let _ = stream.write_all(head.as_bytes());
                let send = if truncate_first && n == 0 { &rest[..rest.len() / 2] } else { rest };
                let _ = stream.write_all(send);
            }
        });
        (url, requests)
    }

    /// Bytes of a small but valid GeoTIFF whose data doesn't compress away.
    fn tiff_bytes() -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.tif");
        Raster {
            width: 64,
            height: 64,
            data: (0..64 * 64).map(|i| ((i * 7919) % 1000) as f32).collect(),
            origin_x: -121.0,
            origin_y: 39.0,
            pixel_w: 1.0 / 64.0,
            pixel_h: 1.0 / 64.0,
            epsg: EPSG_NAD83_GEOGRAPHIC,
            nodata: None,
        }
        .write_geotiff(&path, false)
        .unwrap();
        std::fs::read(path).unwrap()
    }

    fn cache(dir: &Path, url: &str) -> SourceCache {
        SourceCache::new(dir, url).with_retry_delay(Duration::ZERO)
    }

    #[test]
    fn cell_naming_matches_usgs() {
        let c = SourceCell::containing(-120.8, 38.79);
        assert_eq!(c, SourceCell { north: 39, west: 121 });
        assert_eq!(
            c.url(USGS_13_BASE_URL),
            "https://prd-tnm.s3.amazonaws.com/StagedProducts/Elevation/13/TIFF/current/n39w121/USGS_13_n39w121.tif"
        );
        // Cells are half-open: the south and west edges belong to the cell.
        assert_eq!(SourceCell::containing(-120.0, 38.0), SourceCell { north: 39, west: 120 });
    }

    #[test]
    fn covering_spans_all_cells() {
        let cells = SourceCell::covering(&LonLatBox { west: -121.2, south: 38.9, east: -120.9, north: 39.1 });
        assert_eq!(cells.len(), 4);
        assert!(cells.contains(&SourceCell { north: 40, west: 122 }));
        assert!(cells.contains(&SourceCell { north: 39, west: 121 }));
    }

    #[test]
    fn downloads_then_serves_from_cache() {
        let body = tiff_bytes();
        let (url, requests) = serve(body.clone(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cell = SourceCell { north: 39, west: 121 };
        assert_eq!(c.ensure(cell).unwrap(), Fetched::Present(c.path(cell)));
        assert_eq!(std::fs::read(c.path(cell)).unwrap(), body);
        c.ensure(cell).unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1, "second ensure must hit the cache");
    }

    #[test]
    fn download_resumes_after_truncation() {
        let body = tiff_bytes();
        let (url, requests) = serve(body.clone(), true);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cell = SourceCell { north: 39, west: 121 };
        assert_eq!(c.ensure(cell).unwrap(), Fetched::Present(c.path(cell)));
        assert_eq!(std::fs::read(c.path(cell)).unwrap(), body);
        assert_eq!(requests.load(Ordering::SeqCst), 2, "one truncated + one ranged request");
    }

    #[test]
    fn missing_cell_is_remembered() {
        let (url, requests) = serve(tiff_bytes(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        assert_eq!(c.ensure(MISSING).unwrap(), Fetched::Missing);
        assert_eq!(c.ensure(MISSING).unwrap(), Fetched::Missing);
        assert_eq!(requests.load(Ordering::SeqCst), 1, "404 is cached as a .missing marker");
    }

    #[test]
    fn corrupt_cache_entry_is_redownloaded() {
        let body = tiff_bytes();
        let (url, _) = serve(body.clone(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cell = SourceCell { north: 39, west: 121 };
        std::fs::write(c.path(cell), b"not a tiff").unwrap();
        c.ensure(cell).unwrap();
        assert_eq!(std::fs::read(c.path(cell)).unwrap(), body);
    }

    #[test]
    fn unreachable_server_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), "http://127.0.0.1:9");
        let err = c.ensure(SourceCell { north: 39, west: 121 }).unwrap_err();
        assert!(format!("{err:#}").contains("after 5 attempts"), "{err:#}");
    }

    #[test]
    fn prefetch_reports_every_cell() {
        let (url, _) = serve(tiff_bytes(), false);
        let dir = tempfile::tempdir().unwrap();
        let c = cache(dir.path(), &url);
        let cells = [SourceCell { north: 39, west: 121 }, SourceCell { north: 39, west: 120 }, MISSING];
        let results = c.prefetch(&cells, 2);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, r)| r.is_ok()));
    }
}
