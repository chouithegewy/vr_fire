//! Streams USGS 3DEP 1/3" cloud-optimized GeoTIFFs straight from S3 with HTTP range
//! requests (the bucket allows CORS), so the browser fetches only the 512×512 pieces
//! of the overview level it needs. No server-side storage.

use bevy::prelude::*;
use std::collections::HashMap;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

const BASE: &str = "https://prd-tnm.s3.amazonaws.com/StagedProducts/Elevation/13/TIFF/current";
const HEADER_BYTES: u64 = 65536;
const NODATA: f32 = -999999.0;
pub const MAX_INFLIGHT: usize = 12;

/// 1°×1° 3DEP cell named by its NW corner (n39w121 covers lat 38–39, lon −121..−120).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cell {
    pub north: i32,
    pub west: i32,
}

impl Cell {
    pub fn containing(lon: f64, lat: f64) -> Self {
        Self { north: lat.floor() as i32 + 1, west: (-lon).ceil() as i32 }
    }
    fn name(&self) -> String {
        format!("n{:02}w{:03}", self.north, self.west)
    }
    fn url(&self) -> String {
        format!("{BASE}/{0}/USGS_13_{0}.tif", self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub cell: Cell,
    pub level: u8,
    pub tx: u32,
    pub ty: u32,
}

#[derive(Debug)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub tile: u32,
    pub across: u32,
    pub offsets: Vec<u64>,
    pub counts: Vec<u64>,
    /// Degrees per pixel.
    pub px: f64,
    pub py: f64,
}

#[derive(Debug)]
pub struct CellMeta {
    pub header: Arc<Vec<u8>>,
    pub levels: Vec<Level>,
    /// Outer NW corner of pixel (0,0), degrees.
    pub origin_lon: f64,
    pub origin_lat: f64,
}

pub enum CellState {
    Pending,
    Ready(Arc<CellMeta>),
    Missing,
}

pub struct TileData {
    pub w: u32,
    pub h: u32,
    pub data: Vec<f32>,
}

pub enum TileState {
    Queued,
    Pending,
    Ready(Arc<TileData>),
    Failed,
}

enum Msg {
    Header(Cell, Option<Vec<u8>>),
    Tile(TileKey, Option<Vec<u8>>),
}

#[derive(Resource)]
pub struct Cog {
    pub cells: HashMap<Cell, CellState>,
    pub tiles: HashMap<TileKey, TileState>,
    queue: Vec<TileKey>,
    inflight: usize,
    tx: Sender<Msg>,
    rx: Mutex<Receiver<Msg>>,
    pub bytes_fetched: u64,
}

impl Default for Cog {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            cells: HashMap::new(),
            tiles: HashMap::new(),
            queue: Vec::new(),
            inflight: 0,
            tx,
            rx: Mutex::new(rx),
            bytes_fetched: 0,
        }
    }
}

fn fetch_range(url: String, start: u64, end_inclusive: u64, on_done: impl FnOnce(Option<Vec<u8>>) + Send + 'static) {
    let mut req = ehttp::Request::get(url);
    req.headers.insert("Range", format!("bytes={start}-{end_inclusive}"));
    ehttp::fetch(req, move |res| {
        on_done(match res {
            Ok(r) if r.status == 200 || r.status == 206 => Some(r.bytes),
            _ => None,
        })
    });
}

impl Cog {
    pub fn inflight(&self) -> usize {
        self.inflight + self.queue.len()
    }

    /// Cell metadata if the header is loaded; requests it otherwise. `Some(None)` = no data (ocean).
    pub fn cell(&mut self, cell: Cell) -> Option<Option<Arc<CellMeta>>> {
        match self.cells.get(&cell) {
            Some(CellState::Ready(m)) => Some(Some(m.clone())),
            Some(CellState::Missing) => Some(None),
            Some(CellState::Pending) => None,
            None => {
                self.cells.insert(cell, CellState::Pending);
                self.inflight += 1;
                let tx = self.tx.clone();
                fetch_range(cell.url(), 0, HEADER_BYTES - 1, move |b| {
                    let _ = tx.send(Msg::Header(cell, b));
                });
                None
            }
        }
    }

    /// Tile data if loaded; queues the fetch otherwise. `Some(None)` = failed / no data.
    pub fn tile(&mut self, key: TileKey) -> Option<Option<Arc<TileData>>> {
        match self.tiles.get(&key) {
            Some(TileState::Ready(t)) => Some(Some(t.clone())),
            Some(TileState::Failed) => Some(None),
            Some(_) => None,
            None => {
                self.tiles.insert(key, TileState::Queued);
                self.queue.push(key);
                None
            }
        }
    }

    pub fn pump(&mut self) {
        let msgs: Vec<Msg> = self.rx.lock().unwrap().try_iter().collect();
        for m in msgs {
            self.inflight -= 1;
            match m {
                Msg::Header(cell, bytes) => {
                    let state = bytes
                        .and_then(|b| {
                            self.bytes_fetched += b.len() as u64;
                            parse_header(b).map_err(|e| warn!("{}: bad header: {e}", cell.name())).ok()
                        })
                        .map(|m| CellState::Ready(Arc::new(m)))
                        .unwrap_or(CellState::Missing);
                    self.cells.insert(cell, state);
                }
                Msg::Tile(key, bytes) => {
                    let meta = match self.cells.get(&key.cell) {
                        Some(CellState::Ready(m)) => m.clone(),
                        _ => continue,
                    };
                    let state = bytes
                        .and_then(|b| {
                            self.bytes_fetched += b.len() as u64;
                            decode_tile(&meta, key, b).map_err(|e| warn!("decode {key:?}: {e}")).ok()
                        })
                        .map(|t| TileState::Ready(Arc::new(t)))
                        .unwrap_or(TileState::Failed);
                    self.tiles.insert(key, state);
                }
            }
        }
        while self.inflight < MAX_INFLIGHT && !self.queue.is_empty() {
            let key = self.queue.remove(0);
            let Some(CellState::Ready(meta)) = self.cells.get(&key.cell) else {
                self.tiles.insert(key, TileState::Failed);
                continue;
            };
            let lv = &meta.levels[key.level as usize];
            let idx = (key.ty * lv.across + key.tx) as usize;
            let (off, len) = (lv.offsets[idx], lv.counts[idx]);
            if len == 0 {
                self.tiles.insert(key, TileState::Failed);
                continue;
            }
            self.tiles.insert(key, TileState::Pending);
            self.inflight += 1;
            let tx = self.tx.clone();
            fetch_range(key.cell.url(), off, off + len - 1, move |b| {
                let _ = tx.send(Msg::Tile(key, b));
            });
        }
    }

    /// Height at lon/lat from `level`; None if a needed piece isn't loaded yet (it gets requested).
    /// Returns Some(None) where there is no data (ocean / outside coverage).
    pub fn sample(&mut self, lon: f64, lat: f64, level: u8) -> Option<Option<f32>> {
        let cell = Cell::containing(lon, lat);
        let meta = match self.cell(cell)? {
            Some(m) => m,
            None => return Some(None),
        };
        let lv = &meta.levels[(level as usize).min(meta.levels.len() - 1)];
        let fx = (lon - meta.origin_lon) / lv.px - 0.5;
        let fy = (meta.origin_lat - lat) / lv.py - 0.5;
        let (c0, r0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - c0, fy - r0);
        let (c0, r0) = (c0 as i64, r0 as i64);
        let mut sum = 0.0;
        let mut wsum = 0.0;
        let mut missing = false;
        for (dc, dr, w) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
            if w <= 0.0 {
                continue;
            }
            let (c, r) = (c0 + dc, r0 + dr);
            if c < 0 || r < 0 || c >= lv.width as i64 || r >= lv.height as i64 {
                continue;
            }
            let key = TileKey { cell, level, tx: c as u32 / lv.tile, ty: r as u32 / lv.tile };
            match self.tile(key) {
                None => missing = true,
                Some(None) => {}
                Some(Some(t)) => {
                    let (lc, lr) = (c as u32 % lv.tile, r as u32 % lv.tile);
                    if lc < t.w && lr < t.h {
                        let v = t.data[(lr * t.w + lc) as usize];
                        if v != NODATA && v.is_finite() {
                            sum += w * v as f64;
                            wsum += w;
                        }
                    }
                }
            }
        }
        if missing {
            return None;
        }
        Some((wsum > 0.0).then(|| (sum / wsum) as f32))
    }
}

fn parse_header(bytes: Vec<u8>) -> Result<CellMeta, String> {
    let header = Arc::new(bytes);
    let mut dec = Decoder::new(Cursor::new(header.as_slice())).map_err(|e| e.to_string())?;
    let scale = dec.get_tag_f64_vec(Tag::ModelPixelScaleTag).map_err(|e| e.to_string())?;
    let tie = dec.get_tag_f64_vec(Tag::ModelTiepointTag).map_err(|e| e.to_string())?;
    let mut levels = Vec::new();
    let mut w0 = 0.0;
    loop {
        let (width, height) = dec.dimensions().map_err(|e| e.to_string())?;
        let (tile, _) = dec.chunk_dimensions();
        let offsets = dec.get_tag_u64_vec(Tag::TileOffsets).map_err(|e| e.to_string())?;
        let counts = dec.get_tag_u64_vec(Tag::TileByteCounts).map_err(|e| e.to_string())?;
        if levels.is_empty() {
            w0 = width as f64;
        }
        let f = w0 / width as f64;
        levels.push(Level { width, height, tile, across: width.div_ceil(tile), offsets, counts, px: scale[0] * f, py: scale[1] * f });
        if !dec.more_images() {
            break;
        }
        dec.next_image().map_err(|e| e.to_string())?;
    }
    Ok(CellMeta {
        origin_lon: tie[3] - tie[0] * scale[0],
        origin_lat: tie[4] + tie[1] * scale[1],
        header,
        levels,
    })
}

/// The header plus one tile's bytes, placed at their file offsets.
struct Sparse {
    header: Arc<Vec<u8>>,
    at: u64,
    data: Vec<u8>,
    pos: u64,
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (src, base) = if self.pos < self.header.len() as u64 {
            (self.header.as_slice(), 0)
        } else if self.pos >= self.at && self.pos < self.at + self.data.len() as u64 {
            (self.data.as_slice(), self.at)
        } else {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "byte range not fetched"));
        };
        let start = (self.pos - base) as usize;
        let n = buf.len().min(src.len() - start);
        buf[..n].copy_from_slice(&src[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.pos = match p {
            SeekFrom::Start(o) => o,
            SeekFrom::Current(d) => (self.pos as i64 + d) as u64,
            SeekFrom::End(_) => return Err(io::Error::other("no end")),
        };
        Ok(self.pos)
    }
}

fn decode_tile(meta: &CellMeta, key: TileKey, bytes: Vec<u8>) -> Result<TileData, String> {
    let lv = &meta.levels[key.level as usize];
    let idx = key.ty * lv.across + key.tx;
    let at = lv.offsets[idx as usize];
    let sparse = Sparse { header: meta.header.clone(), at, data: bytes, pos: 0 };
    let mut dec = Decoder::new(sparse).map_err(|e| e.to_string())?.with_limits(Limits::unlimited());
    dec.seek_to_image(key.level as usize).map_err(|e| e.to_string())?;
    let (_, h) = dec.chunk_data_dimensions(idx);
    match dec.read_chunk(idx).map_err(|e| e.to_string())? {
        DecodingResult::F32(data) => {
            let w = (data.len() as u32) / h.max(1);
            Ok(TileData { w, h, data })
        }
        _ => Err("not f32".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode pieces of the real cached 3DEP file through the sparse path and compare
    /// with a full decode. Skips when the cache file is absent.
    #[test]
    fn sparse_decode_matches_full_file() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cache/USGS_13_n39w121.tif");
        let Ok(file) = std::fs::read(&path) else { return };
        let meta = parse_header(file[..HEADER_BYTES as usize].to_vec()).unwrap();
        assert_eq!(meta.levels.len(), 6);
        assert_eq!(meta.levels[0].width, 10812);
        for (level, tx, ty) in [(0u8, 3u32, 7u32), (5, 0, 0), (2, 5, 5)] {
            let lv = &meta.levels[level as usize];
            let idx = (ty * lv.across + tx) as usize;
            let (o, n) = (lv.offsets[idx] as usize, lv.counts[idx] as usize);
            let key = TileKey { cell: Cell { north: 39, west: 121 }, level, tx, ty };
            let t = decode_tile(&meta, key, file[o..o + n].to_vec()).unwrap();
            // Reference: full-file decoder, same chunk.
            let mut dec = Decoder::new(Cursor::new(&file)).unwrap().with_limits(Limits::unlimited());
            dec.seek_to_image(level as usize).unwrap();
            let DecodingResult::F32(want) = dec.read_chunk(idx as u32).unwrap() else { panic!() };
            assert_eq!(t.data, want, "level {level} tile ({tx},{ty})");
            assert!(t.data.iter().any(|v| *v > 100.0 && *v < 3000.0));
        }
    }
}
