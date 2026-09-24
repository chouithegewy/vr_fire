//! Bake outputs: binary glTF meshes, raw f32 heights, and per-tile JSON metadata.

use crate::crs::EPSG_5070;
use crate::grid::{Bounds, CELLS_PER_TILE, GridSpec, LOD_STRIDES, NODE_SPACING_M, NODES_PER_SIDE, TileId, WINDOWS_PER_TILE};
use crate::mesh::Mesh;
use crate::reproject::min_max;
use crate::store::TileEntry;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_INT: u32 = 5125;

/// Binary glTF 2.0: one mesh, one primitive (POSITION, NORMAL, TEXCOORD_0, u32 indices).
pub fn glb_bytes(mesh: &Mesh, name: &str) -> Vec<u8> {
    let nv = mesh.positions.len();
    let mut bin = Vec::with_capacity(nv * 32 + mesh.indices.len() * 4);
    mesh.positions.iter().flatten().for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    let normals_at = bin.len();
    mesh.normals.iter().flatten().for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    let uvs_at = bin.len();
    mesh.uvs.iter().flatten().for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    let indices_at = bin.len();
    mesh.indices.iter().for_each(|i| bin.extend_from_slice(&i.to_le_bytes()));

    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    let doc = json!({
        "asset": { "version": "2.0", "generator": concat!("vr_fire ", env!("CARGO_PKG_VERSION")) },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "name": name }],
        "meshes": [{ "name": name, "primitives": [{
            "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 }, "indices": 3, "mode": 4 }] }],
        "buffers": [{ "byteLength": bin.len() }],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": normals_at, "target": ARRAY_BUFFER },
            { "buffer": 0, "byteOffset": normals_at, "byteLength": uvs_at - normals_at, "target": ARRAY_BUFFER },
            { "buffer": 0, "byteOffset": uvs_at, "byteLength": indices_at - uvs_at, "target": ARRAY_BUFFER },
            { "buffer": 0, "byteOffset": indices_at, "byteLength": bin.len() - indices_at, "target": ELEMENT_ARRAY_BUFFER }
        ],
        "accessors": [
            { "bufferView": 0, "componentType": FLOAT, "count": nv, "type": "VEC3", "min": min, "max": max },
            { "bufferView": 1, "componentType": FLOAT, "count": nv, "type": "VEC3" },
            { "bufferView": 2, "componentType": FLOAT, "count": nv, "type": "VEC2" },
            { "bufferView": 3, "componentType": UNSIGNED_INT, "count": mesh.indices.len(), "type": "SCALAR" }
        ]
    });
    let mut json_chunk = serde_json::to_vec(&doc).expect("serializable");
    while json_chunk.len() % 4 != 0 {
        json_chunk.push(b' ');
    }
    let total = 12 + 8 + json_chunk.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_chunk);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

pub fn write_glb(path: &Path, mesh: &Mesh, name: &str) -> Result<()> {
    std::fs::write(path, glb_bytes(mesh, name)).with_context(|| format!("write {}", path.display()))
}

pub fn write_heights_f32(path: &Path, heights: &[f32]) -> Result<()> {
    let bytes: Vec<u8> = heights.iter().flat_map(|h| h.to_le_bytes()).collect();
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LodInfo {
    pub lod: usize,
    pub spacing_m: f64,
    pub nodes_per_side: usize,
    /// Relative to the bake output directory.
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileMetadata {
    pub tile: TileId,
    pub crs: String,
    pub grid_origin: [f64; 2],
    /// EPSG:5070 bounds (f64). Place tiles relative to a per-session floating origin.
    pub bounds: Bounds,
    pub vertical_datum: String,
    pub axes: String,
    pub node_spacing_m: f64,
    pub nodes_per_side: usize,
    pub ml_cells_per_side: i64,
    pub ml_windows_per_side: i64,
    pub lods: Vec<LodInfo>,
    /// Relative to the bake output directory; little-endian f32, row-major, north row first.
    pub heights_file: String,
    pub min_elevation_m: f32,
    pub max_elevation_m: f32,
    pub filled_samples: u32,
    pub sources: Vec<String>,
    pub pipeline_version: String,
}

impl TileMetadata {
    pub fn new(grid: &GridSpec, tile: TileId, entry: &TileEntry, heights: &[f32]) -> Self {
        let (lo, hi) = min_max(heights);
        Self {
            tile,
            crs: format!("EPSG:5070 ({EPSG_5070})"),
            grid_origin: [grid.origin_x, grid.origin_y],
            bounds: grid.tile_bounds(tile),
            vertical_datum: "NAVD88 meters".into(),
            axes: "glTF: +X east, +Y up (elevation m), +Z south; origin at tile NW corner (bounds.x_min, bounds.y_max)".into(),
            node_spacing_m: NODE_SPACING_M,
            nodes_per_side: NODES_PER_SIDE,
            ml_cells_per_side: CELLS_PER_TILE,
            ml_windows_per_side: WINDOWS_PER_TILE,
            lods: LOD_STRIDES
                .iter()
                .enumerate()
                .map(|(lod, &s)| LodInfo {
                    lod,
                    spacing_m: NODE_SPACING_M * s as f64,
                    nodes_per_side: (NODES_PER_SIDE - 1) / s + 1,
                    file: format!("lod{lod}/{tile}.glb"),
                })
                .collect(),
            heights_file: format!("{tile}.f32"),
            min_elevation_m: lo,
            max_elevation_m: hi,
            filled_samples: entry.filled_samples,
            sources: entry.sources.clone(),
            pipeline_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

pub fn write_metadata(path: &Path, meta: &TileMetadata) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(meta)?).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TileStatus;

    fn quad() -> Mesh {
        Mesh {
            positions: vec![[0.0, 1.0, 0.0], [10.0, 2.0, 0.0], [0.0, 3.0, 10.0], [10.0, 4.0, 10.0]],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            indices: vec![0, 2, 1, 1, 2, 3],
        }
    }

    #[test]
    fn glb_is_well_formed_and_aligned() {
        let bytes = glb_bytes(&quad(), "q");
        assert_eq!(&bytes[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize, bytes.len());
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(json_len % 4, 0);
        assert_eq!(&bytes[16..20], b"JSON");
        assert_eq!(&bytes[20 + json_len + 4..20 + json_len + 8], b"BIN\0");
    }

    #[test]
    fn glb_reloads_with_the_gltf_crate() {
        let m = quad();
        let (doc, buffers, _) = gltf::import_slice(glb_bytes(&m, "q")).unwrap();
        let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        assert_eq!(reader.read_positions().unwrap().collect::<Vec<_>>(), m.positions);
        assert_eq!(reader.read_normals().unwrap().collect::<Vec<_>>(), m.normals);
        assert_eq!(reader.read_tex_coords(0).unwrap().into_f32().collect::<Vec<_>>(), m.uvs);
        assert_eq!(reader.read_indices().unwrap().into_u32().collect::<Vec<_>>(), m.indices);
        let bb = prim.bounding_box();
        assert_eq!((bb.min, bb.max), ([0.0, 1.0, 0.0], [10.0, 4.0, 10.0]));
    }

    #[test]
    fn heights_are_little_endian_f32() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.f32");
        write_heights_f32(&p, &[1.5, -2.0]).unwrap();
        let b = std::fs::read(p).unwrap();
        assert_eq!(b, [1.5f32.to_le_bytes(), (-2.0f32).to_le_bytes()].concat());
    }

    #[test]
    fn metadata_describes_the_tile() {
        let grid = GridSpec::default();
        let t = TileId::new(812, 1440);
        let entry = TileEntry { status: TileStatus::Ok, error: None, min_elevation_m: Some(0.0), max_elevation_m: Some(0.0), filled_samples: 7, sources: vec!["n39w121".into()] };
        let heights: Vec<f32> = (0..NODES_PER_SIDE * NODES_PER_SIDE).map(|k| k as f32 / 1000.0).collect();
        let meta = TileMetadata::new(&grid, t, &entry, &heights);
        assert_eq!(meta.bounds, grid.tile_bounds(t));
        assert_eq!(meta.lods.len(), 4);
        assert_eq!(meta.lods[1].spacing_m, 30.0);
        assert_eq!(meta.lods[1].nodes_per_side, 126);
        assert_eq!(meta.lods[3].file, "lod3/812_1440.glb");
        assert_eq!(meta.heights_file, "812_1440.f32");
        assert_eq!((meta.ml_cells_per_side, meta.ml_windows_per_side), (125, 10));
        assert_eq!(meta.filled_samples, 7);
        assert_eq!(meta.max_elevation_m, heights[heights.len() - 1]);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.json");
        write_metadata(&p, &meta).unwrap();
        let back: TileMetadata = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(back, meta);
    }
}
