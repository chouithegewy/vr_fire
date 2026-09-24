//! Single-band f32 GeoTIFF rasters: read (3DEP sources, store tiles), write, bilinear sampling.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype::Gray32Float};
use tiff::tags::Tag;

pub const EPSG_NAD83_GEOGRAPHIC: u16 = 4269;
pub const EPSG_WGS84_GEOGRAPHIC: u16 = 4326;
pub const EPSG_CONUS_ALBERS: u16 = 5070;

// GeoKey IDs and values from the GeoTIFF 1.0 spec.
const GT_MODEL_TYPE: u16 = 1024;
const GT_RASTER_TYPE: u16 = 1025;
const GEOGRAPHIC_TYPE: u16 = 2048;
const PROJECTED_CS_TYPE: u16 = 3072;
const MODEL_TYPE_PROJECTED: u16 = 1;
const MODEL_TYPE_GEOGRAPHIC: u16 = 2;
const RASTER_PIXEL_IS_AREA: u16 = 1;
const RASTER_PIXEL_IS_POINT: u16 = 2;

/// `origin_x`/`origin_y` is the outer top-left corner of pixel (0, 0), in CRS units.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub origin_x: f64,
    pub origin_y: f64,
    pub pixel_w: f64,
    pub pixel_h: f64,
    pub epsg: u16,
    pub nodata: Option<f32>,
}

impl Raster {
    pub fn read_geotiff(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut dec = Decoder::new(BufReader::new(file))
            .with_context(|| format!("not a TIFF: {}", path.display()))?
            .with_limits(Limits::unlimited());
        let (width, height) = dec.dimensions()?;
        let scale = dec.get_tag_f64_vec(Tag::ModelPixelScaleTag).context("missing ModelPixelScaleTag")?;
        let tie = dec.get_tag_f64_vec(Tag::ModelTiepointTag).context("missing ModelTiepointTag")?;
        let keys = dec.get_tag_u16_vec(Tag::GeoKeyDirectoryTag).context("missing GeoKeyDirectoryTag")?;
        let nodata = dec
            .get_tag_ascii_string(Tag::GdalNodata)
            .ok()
            .map(|s| s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).parse::<f32>())
            .transpose()
            .context("unparseable GDAL_NODATA")?;
        let epsg = geokey(&keys, PROJECTED_CS_TYPE)
            .or_else(|| geokey(&keys, GEOGRAPHIC_TYPE))
            .context("GeoKeys carry no EPSG code")?;
        let half = if geokey(&keys, GT_RASTER_TYPE) == Some(RASTER_PIXEL_IS_POINT) { 0.5 } else { 0.0 };
        let data = match dec.read_image() {
            Ok(DecodingResult::F32(v)) => v,
            Ok(_) => bail!("{}: only 32-bit float rasters are supported", path.display()),
            // The tiff crate's whole-image read rejects some valid 3DEP files (LZW blocks
            // without an end code); assemble those block by block instead.
            Err(e) => read_blocks(&mut dec, path, width as usize, height as usize)
                .with_context(|| format!("decode {} (whole-image read failed: {e})", path.display()))?,
        };
        let (pixel_w, pixel_h) = (scale[0], scale[1]);
        let (i, j, x, y) = (tie[0], tie[1], tie[3], tie[4]);
        Ok(Self {
            width: width as usize,
            height: height as usize,
            data,
            origin_x: x - (i + half) * pixel_w,
            origin_y: y + (j + half) * pixel_h,
            pixel_w,
            pixel_h,
            epsg,
            nodata,
        })
    }

    /// Deflate-compressed GeoTIFF. `point_registered` marks samples as grid nodes (PixelIsPoint).
    pub fn write_geotiff(&self, path: &Path, point_registered: bool) -> Result<()> {
        let mut out = BufWriter::new(File::create(path).with_context(|| format!("create {}", path.display()))?);
        {
            let mut enc = TiffEncoder::new(&mut out)?.with_compression(Compression::Deflate(DeflateLevel::Balanced));
            let mut img = enc.new_image::<Gray32Float>(self.width as u32, self.height as u32)?;
            let half = if point_registered { 0.5 } else { 0.0 };
            let geographic = matches!(self.epsg, EPSG_NAD83_GEOGRAPHIC | EPSG_WGS84_GEOGRAPHIC);
            let (model_type, cs_key) =
                if geographic { (MODEL_TYPE_GEOGRAPHIC, GEOGRAPHIC_TYPE) } else { (MODEL_TYPE_PROJECTED, PROJECTED_CS_TYPE) };
            let raster_type = if point_registered { RASTER_PIXEL_IS_POINT } else { RASTER_PIXEL_IS_AREA };
            img.encoder().write_tag(Tag::ModelPixelScaleTag, &[self.pixel_w, self.pixel_h, 0.0][..])?;
            img.encoder().write_tag(
                Tag::ModelTiepointTag,
                &[0.0, 0.0, 0.0, self.origin_x + half * self.pixel_w, self.origin_y - half * self.pixel_h, 0.0][..],
            )?;
            img.encoder().write_tag(
                Tag::GeoKeyDirectoryTag,
                &[1u16, 1, 0, 3, GT_MODEL_TYPE, 0, 1, model_type, GT_RASTER_TYPE, 0, 1, raster_type, cs_key, 0, 1, self.epsg][..],
            )?;
            if let Some(nd) = self.nodata {
                img.encoder().write_tag(Tag::GdalNodata, nd.to_string().as_str())?;
            }
            img.write_data(&self.data)?;
        }
        out.flush()?;
        Ok(())
    }

    /// Value at (col, row); None outside the raster, at nodata, or NaN.
    pub fn get(&self, col: i64, row: i64) -> Option<f32> {
        if col < 0 || row < 0 || col >= self.width as i64 || row >= self.height as i64 {
            return None;
        }
        let v = self.data[row as usize * self.width + col as usize];
        if v.is_nan() || Some(v) == self.nodata { None } else { Some(v) }
    }

    /// Bilinear sample at CRS coordinate (x, y). Missing neighbors are dropped and the
    /// remaining weights renormalized; None when no neighbor has data.
    pub fn sample_bilinear(&self, x: f64, y: f64) -> Option<f32> {
        let fx = (x - self.origin_x) / self.pixel_w - 0.5;
        let fy = (self.origin_y - y) / self.pixel_h - 0.5;
        let (c0, r0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - c0, fy - r0);
        let (c0, r0) = (c0 as i64, r0 as i64);
        let (mut sum, mut wsum) = (0.0f64, 0.0f64);
        for (dc, dr, w) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            if w == 0.0 {
                continue;
            }
            if let Some(v) = self.get(c0 + dc, r0 + dr) {
                sum += w * v as f64;
                wsum += w;
            }
        }
        (wsum > 0.0).then(|| (sum / wsum) as f32)
    }
}

/// Assemble a tiled f32 image one block at a time; blocks the tiff crate rejects are
/// decoded leniently from their raw bytes.
fn read_blocks(dec: &mut Decoder<BufReader<File>>, path: &Path, width: usize, height: usize) -> Result<Vec<f32>> {
    let (tw, th) = dec.chunk_dimensions();
    let (tw, th) = (tw as usize, th as usize);
    let offsets = dec.get_tag_u64_vec(Tag::TileOffsets).context("not a tiled TIFF")?;
    let counts = dec.get_tag_u64_vec(Tag::TileByteCounts)?;
    let predictor = dec.get_tag_unsigned::<u16>(Tag::Predictor).unwrap_or(1);
    let across = width.div_ceil(tw);
    let mut raw_file = File::open(path)?;
    let mut out = vec![0f32; width * height];
    for k in 0..offsets.len() {
        let block = match dec.read_chunk(k as u32) {
            Ok(DecodingResult::F32(v)) => {
                // The tiff crate crops edge blocks; re-pad to the full block width.
                let (dw, dh) = dec.chunk_data_dimensions(k as u32);
                let mut full = vec![0f32; tw * th];
                for r in 0..dh as usize {
                    full[r * tw..r * tw + dw as usize].copy_from_slice(&v[r * dw as usize..(r + 1) * dw as usize]);
                }
                full
            }
            _ => {
                use std::io::{Read, Seek, SeekFrom};
                let mut raw = vec![0u8; counts[k] as usize];
                raw_file.seek(SeekFrom::Start(offsets[k]))?;
                raw_file.read_exact(&mut raw)?;
                decode_lzw_f32_chunk(&raw, tw, th, predictor).with_context(|| format!("block {k}"))?
            }
        };
        let (bx, by) = (k % across, k / across);
        for r in 0..th {
            let y = by * th + r;
            if y >= height {
                break;
            }
            let x0 = bx * tw;
            let n = tw.min(width - x0);
            out[y * width + x0..y * width + x0 + n].copy_from_slice(&block[r * tw..r * tw + n]);
        }
    }
    Ok(out)
}

/// Lenient decode of one LZW-compressed f32 block (`width × height`, little-endian file).
///
/// Some 3DEP files have LZW blocks without the end-of-information code. libtiff and GDAL
/// accept those; the tiff crate rejects them. This decodes until the block's full size is
/// produced, then undoes the TIFF predictor (1 = none, 3 = floating point).
pub fn decode_lzw_f32_chunk(raw: &[u8], width: usize, height: usize, predictor: u16) -> Result<Vec<f32>> {
    let row_bytes = width * 4;
    let mut buf = vec![0u8; row_bytes * height];
    let mut dec = weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let (mut inp, mut outp) = (0, 0);
    while outp < buf.len() {
        let res = dec.decode_bytes(&raw[inp..], &mut buf[outp..]);
        inp += res.consumed_in;
        outp += res.consumed_out;
        match res.status {
            Ok(weezl::LzwStatus::Ok) if res.consumed_in + res.consumed_out > 0 => {}
            // Input ran out (no end code) or end code reached: stop and check the size.
            _ => break,
        }
    }
    if outp != buf.len() {
        bail!("LZW block produced {outp} of {} bytes", buf.len());
    }
    Ok(match predictor {
        1 => buf.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
        3 => {
            // Floating-point predictor (TIFF Tech Note 3): each row holds the samples' bytes
            // split by significance (most significant first), then byte-wise differenced.
            let mut out = Vec::with_capacity(width * height);
            for row in buf.chunks_exact_mut(row_bytes) {
                for i in 1..row_bytes {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
                for i in 0..width {
                    out.push(f32::from_be_bytes([row[i], row[width + i], row[2 * width + i], row[3 * width + i]]));
                }
            }
            out
        }
        p => bail!("unsupported TIFF predictor {p}"),
    })
}

/// Value of a short GeoKey stored inline in the directory (TIFFTagLocation 0).
fn geokey(keys: &[u16], id: u16) -> Option<u16> {
    keys.as_chunks::<4>().0.iter().skip(1).find(|k| k[0] == id && k[1] == 0).map(|k| k[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: usize, height: usize, f: impl Fn(usize, usize) -> f32) -> Raster {
        Raster {
            width,
            height,
            data: (0..height).flat_map(|r| (0..width).map(move |c| (c, r))).map(|(c, r)| f(c, r)).collect(),
            origin_x: -121.0,
            origin_y: 39.0,
            pixel_w: 0.25,
            pixel_h: 0.25,
            epsg: EPSG_NAD83_GEOGRAPHIC,
            nodata: Some(-999999.0),
        }
    }

    #[test]
    fn round_trips_pixel_is_area() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.tif");
        let r = raster(4, 3, |c, r| (c * 10 + r) as f32);
        r.write_geotiff(&path, false).unwrap();
        assert_eq!(Raster::read_geotiff(&path).unwrap(), r);
    }

    #[test]
    fn round_trips_pixel_is_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.tif");
        let r = Raster { origin_x: -2_000_005.0, origin_y: 2_000_005.0, pixel_w: 10.0, pixel_h: 10.0, epsg: EPSG_CONUS_ALBERS, nodata: None, ..raster(5, 5, |c, r| (c + r) as f32) };
        r.write_geotiff(&path, true).unwrap();
        assert_eq!(Raster::read_geotiff(&path).unwrap(), r);
    }

    #[test]
    fn samples_pixel_centers_exactly_and_planes_bilinearly() {
        let r = raster(4, 4, |c, r| (100 * c + 7 * r) as f32);
        // Center of pixel (1, 2).
        assert_eq!(r.sample_bilinear(-121.0 + 1.5 * 0.25, 39.0 - 2.5 * 0.25), Some(114.0));
        // Midway between the centers of (1,1),(2,1),(1,2),(2,2).
        let v = r.sample_bilinear(-121.0 + 2.0 * 0.25, 39.0 - 2.0 * 0.25).unwrap();
        assert!((v - 160.5).abs() < 1e-4, "{v}"); // mean of 107, 207, 114, 214
    }

    #[test]
    fn nodata_neighbors_are_dropped_and_renormalized() {
        let mut r = raster(2, 2, |_, _| 10.0);
        r.data[3] = -999999.0;
        let v = r.sample_bilinear(-121.0 + 0.25, 39.0 - 0.25).unwrap();
        assert_eq!(v, 10.0);
        let all_nodata = raster(2, 2, |_, _| -999999.0);
        assert_eq!(all_nodata.sample_bilinear(-121.0 + 0.25, 39.0 - 0.25), None);
    }

    #[test]
    fn outside_the_raster_is_none() {
        let r = raster(4, 4, |_, _| 1.0);
        assert_eq!(r.sample_bilinear(-125.0, 39.0), None);
        assert_eq!(r.get(-1, 0), None);
        assert_eq!(r.get(0, 4), None);
    }

    #[test]
    fn reads_rasters_larger_than_default_decoder_limit() {
        // Real 3DEP tiles decode to ~467 MB; the tiff crate's default limit is 256 MB.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.tif");
        let side = 8200; // 8200² × 4 B ≈ 269 MB decoded; zeros compress to almost nothing.
        let r = raster(side, side, |_, _| 0.0);
        r.write_geotiff(&path, false).unwrap();
        let back = Raster::read_geotiff(&path).unwrap();
        assert_eq!((back.width, back.height), (side, side));
    }
}

#[cfg(test)]
mod lenient_tests {
    use super::*;

    /// n35w120 has LZW blocks without an end-of-information code; the tiff crate rejects
    /// them, GDAL accepts them. Every block must decode, matching the tiff crate wherever
    /// it succeeds. Skips when the cached file is absent.
    #[test]
    fn decodes_blocks_missing_the_lzw_end_code() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("cache/USGS_13_n35w120.tif");
        let Ok(file) = std::fs::read(&path) else { return };
        let mut dec = Decoder::new(std::io::Cursor::new(&file)).unwrap().with_limits(Limits::unlimited());
        let offsets = dec.get_tag_u64_vec(Tag::TileOffsets).unwrap();
        let counts = dec.get_tag_u64_vec(Tag::TileByteCounts).unwrap();
        let predictor = dec.get_tag_unsigned::<u16>(Tag::Predictor).unwrap_or(1);
        let (tw, th) = dec.chunk_dimensions();
        let (mut strict_failures, mut compared) = (0, 0);
        for (k, (&o, &n)) in offsets.iter().zip(&counts).enumerate() {
            let raw = &file[o as usize..(o + n) as usize];
            let lenient = decode_lzw_f32_chunk(raw, tw as usize, th as usize, predictor).unwrap();
            match dec.read_chunk(k as u32) {
                Ok(DecodingResult::F32(strict)) => {
                    let (dw, _) = dec.chunk_data_dimensions(k as u32);
                    // The tiff crate crops edge tiles to the image; compare the overlap.
                    for (r, row) in strict.chunks(dw as usize).enumerate() {
                        assert_eq!(row, &lenient[r * tw as usize..r * tw as usize + dw as usize], "block {k} row {r}");
                    }
                    compared += 1;
                }
                _ => strict_failures += 1,
            }
        }
        assert_eq!(strict_failures, 0);
        assert_eq!(compared, offsets.len());
        // The tiff crate's whole-image read fails on this file; read_geotiff must not.
        let r = Raster::read_geotiff(&path).unwrap();
        assert_eq!((r.width, r.height), (10812, 10812));
        let (c, row) = (5000usize, 7000usize);
        let (bx, by) = (c / tw as usize, row / th as usize);
        let k = by * (10812usize).div_ceil(tw as usize) + bx;
        let (o, n) = (offsets[k] as usize, counts[k] as usize);
        let block = decode_lzw_f32_chunk(&file[o..o + n], tw as usize, th as usize, predictor).unwrap();
        assert_eq!(r.data[row * 10812 + c], block[(row % th as usize) * tw as usize + c % tw as usize]);
    }
}
