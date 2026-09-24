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
        let data = match dec.read_image().with_context(|| format!("decode {}", path.display()))? {
            DecodingResult::F32(v) => v,
            _ => bail!("{}: only 32-bit float rasters are supported", path.display()),
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
