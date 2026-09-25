//! Binary FBX 7.4 export of a terrain mesh, for engines that import FBX natively (Unity).
//!
//! Frame: tile meshes are built with x = east, y = up, z = south (right-handed, like glTF).
//! FBX is also right-handed Y-up, but Unity converts it to its left-handed frame by negating
//! X. We therefore write the mesh rotated 180° about Y (x, z) → (−x, −z): a proper rotation
//! (no mirroring, winding unchanged) that lands in Unity with +x = east, +z = north. Units
//! are metres (`UnitScaleFactor` 100 cm), so Unity imports at scale 1.
//!
//! Only what an importer needs is written: header extension, global settings, definitions,
//! one Geometry (vertices, triangles, per-vertex normals and UVs), one Model, connections.
//! Large arrays are zlib-compressed, as in files written by the Autodesk FBX SDK.

use crate::mesh::Mesh;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

const VERSION: u32 = 7400;
/// Arrays at least this many bytes are zlib-compressed.
const COMPRESS_FROM: usize = 128;

enum Prop<'a> {
    Bool(bool),
    I32(i32),
    I64(i64),
    F64(f64),
    Str(&'a str),
    Raw(&'a [u8]),
    ArrF64(Vec<f64>),
    ArrI32(Vec<i32>),
}

struct Node<'a> {
    name: &'a str,
    props: Vec<Prop<'a>>,
    children: Vec<Node<'a>>,
}

fn node<'a>(name: &'a str, props: Vec<Prop<'a>>, children: Vec<Node<'a>>) -> Node<'a> {
    Node { name, props, children }
}

fn leaf<'a>(name: &'a str, props: Vec<Prop<'a>>) -> Node<'a> {
    node(name, props, Vec::new())
}

// Header/footer constants as written by Blender's binary FBX exporter (accepted by Autodesk's SDK).
const FILE_ID: [u8; 16] = [0x28, 0xb3, 0x2a, 0xeb, 0xb6, 0x24, 0xcc, 0xc2, 0xbf, 0xc8, 0xb0, 0x2a, 0xa9, 0x2b, 0xfc, 0xf1];
const FOOT_ID: [u8; 16] = [0xfa, 0xbc, 0xab, 0x09, 0xd0, 0xc8, 0xd4, 0x66, 0xb1, 0x76, 0xfb, 0x83, 0x1c, 0xf7, 0x26, 0x7e];
const FOOT_MAGIC: [u8; 16] = [0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29, 0x0b];
const CREATION_TIME: &str = "1970-01-01 10:00:00:000";

fn write_array(out: &mut Vec<u8>, count: usize, bytes: &[u8]) {
    out.extend_from_slice(&(count as u32).to_le_bytes());
    if bytes.len() >= COMPRESS_FROM {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(bytes).expect("in-memory write");
        let packed = z.finish().expect("in-memory write");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&packed);
    } else {
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
}

fn write_prop(out: &mut Vec<u8>, p: &Prop) {
    match p {
        Prop::Bool(v) => out.extend_from_slice(&[b'C', *v as u8]),
        Prop::I32(v) => {
            out.push(b'I');
            out.extend_from_slice(&v.to_le_bytes());
        }
        Prop::I64(v) => {
            out.push(b'L');
            out.extend_from_slice(&v.to_le_bytes());
        }
        Prop::F64(v) => {
            out.push(b'D');
            out.extend_from_slice(&v.to_le_bytes());
        }
        Prop::Str(s) => {
            out.push(b'S');
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Prop::Raw(r) => {
            out.push(b'R');
            out.extend_from_slice(&(r.len() as u32).to_le_bytes());
            out.extend_from_slice(r);
        }
        Prop::ArrF64(a) => {
            out.push(b'd');
            let bytes: Vec<u8> = a.iter().flat_map(|v| v.to_le_bytes()).collect();
            write_array(out, a.len(), &bytes);
        }
        Prop::ArrI32(a) => {
            out.push(b'i');
            let bytes: Vec<u8> = a.iter().flat_map(|v| v.to_le_bytes()).collect();
            write_array(out, a.len(), &bytes);
        }
    }
}

/// Node record (7.4: 32-bit end offset, property count, property bytes, name), then
/// properties, children, and a null record when it has children or no properties.
fn write_node(out: &mut Vec<u8>, n: &Node) {
    let start = out.len();
    out.extend_from_slice(&[0; 12]);
    out.push(n.name.len() as u8);
    out.extend_from_slice(n.name.as_bytes());
    let props_at = out.len();
    for p in &n.props {
        write_prop(out, p);
    }
    let props_len = out.len() - props_at;
    for c in &n.children {
        write_node(out, c);
    }
    if !n.children.is_empty() || n.props.is_empty() {
        out.extend_from_slice(&[0; 13]);
    }
    let end = out.len() as u32;
    out[start..start + 4].copy_from_slice(&end.to_le_bytes());
    out[start + 4..start + 8].copy_from_slice(&(n.props.len() as u32).to_le_bytes());
    out[start + 8..start + 12].copy_from_slice(&(props_len as u32).to_le_bytes());
}

/// `P: name, type, label, flags, value` inside a Properties70 block.
fn p70<'a>(name: &'a str, kind: &'a str, label: &'a str, value: Prop<'a>) -> Node<'a> {
    leaf("P", vec![Prop::Str(name), Prop::Str(kind), Prop::Str(label), Prop::Str(""), value])
}

/// Encode the mesh as a binary FBX 7.4 file.
pub fn fbx_bytes(mesh: &Mesh, name: &str) -> Vec<u8> {
    const DOC_ID: i64 = 1_000_000;
    const GEOM_ID: i64 = 1_000_001;
    const MODEL_ID: i64 = 1_000_002;
    // 180° about Y: (x, y, z) → (−x, y, −z) for positions and normals (see module docs).
    let vertices: Vec<f64> = mesh.positions.iter().flat_map(|p| [-p[0] as f64, p[1] as f64, -p[2] as f64]).collect();
    let normals: Vec<f64> = mesh.normals.iter().flat_map(|n| [-n[0] as f64, n[1] as f64, -n[2] as f64]).collect();
    let uvs: Vec<f64> = mesh.uvs.iter().flat_map(|t| [t[0] as f64, t[1] as f64]).collect();
    // Each triangle's last index is stored as −(i + 1) to close the polygon.
    let polys: Vec<i32> = mesh.indices.iter().enumerate().map(|(k, &i)| if k % 3 == 2 { !(i as i32) } else { i as i32 }).collect();
    let geom_name = format!("{name}\x00\x01Geometry");
    let model_name = format!("{name}\x00\x01Model");
    let creator = concat!("vr_fire ", env!("CARGO_PKG_VERSION"));
    let i = Prop::I32;

    let nodes = vec![
        node(
            "FBXHeaderExtension",
            vec![],
            vec![
                leaf("FBXHeaderVersion", vec![i(1003)]),
                leaf("FBXVersion", vec![i(VERSION as i32)]),
                leaf("EncryptionType", vec![i(0)]),
                node(
                    "CreationTimeStamp",
                    vec![],
                    vec![
                        leaf("Version", vec![i(1000)]),
                        leaf("Year", vec![i(1970)]),
                        leaf("Month", vec![i(1)]),
                        leaf("Day", vec![i(1)]),
                        leaf("Hour", vec![i(10)]),
                        leaf("Minute", vec![i(0)]),
                        leaf("Second", vec![i(0)]),
                        leaf("Millisecond", vec![i(0)]),
                    ],
                ),
                leaf("Creator", vec![Prop::Str(creator)]),
            ],
        ),
        leaf("FileId", vec![Prop::Raw(&FILE_ID)]),
        leaf("CreationTime", vec![Prop::Str(CREATION_TIME)]),
        leaf("Creator", vec![Prop::Str(creator)]),
        node(
            "GlobalSettings",
            vec![],
            vec![
                leaf("Version", vec![i(1000)]),
                node(
                    "Properties70",
                    vec![],
                    vec![
                        p70("UpAxis", "int", "Integer", i(1)),
                        p70("UpAxisSign", "int", "Integer", i(1)),
                        p70("FrontAxis", "int", "Integer", i(2)),
                        p70("FrontAxisSign", "int", "Integer", i(1)),
                        p70("CoordAxis", "int", "Integer", i(0)),
                        p70("CoordAxisSign", "int", "Integer", i(1)),
                        p70("OriginalUpAxis", "int", "Integer", i(1)),
                        p70("OriginalUpAxisSign", "int", "Integer", i(1)),
                        p70("UnitScaleFactor", "double", "Number", Prop::F64(100.0)),
                        p70("OriginalUnitScaleFactor", "double", "Number", Prop::F64(100.0)),
                    ],
                ),
            ],
        ),
        node(
            "Documents",
            vec![],
            vec![
                leaf("Count", vec![i(1)]),
                node(
                    "Document",
                    vec![Prop::I64(DOC_ID), Prop::Str("Scene"), Prop::Str("Scene")],
                    vec![node("Properties70", vec![], vec![]), leaf("RootNode", vec![Prop::I64(0)])],
                ),
            ],
        ),
        leaf("References", vec![]),
        node(
            "Definitions",
            vec![],
            vec![
                leaf("Version", vec![i(100)]),
                leaf("Count", vec![i(3)]),
                node("ObjectType", vec![Prop::Str("GlobalSettings")], vec![leaf("Count", vec![i(1)])]),
                node("ObjectType", vec![Prop::Str("Geometry")], vec![leaf("Count", vec![i(1)])]),
                node("ObjectType", vec![Prop::Str("Model")], vec![leaf("Count", vec![i(1)])]),
            ],
        ),
        node(
            "Objects",
            vec![],
            vec![
                node(
                    "Geometry",
                    vec![Prop::I64(GEOM_ID), Prop::Str(&geom_name), Prop::Str("Mesh")],
                    vec![
                        leaf("Vertices", vec![Prop::ArrF64(vertices)]),
                        leaf("PolygonVertexIndex", vec![Prop::ArrI32(polys)]),
                        leaf("GeometryVersion", vec![i(124)]),
                        node(
                            "LayerElementNormal",
                            vec![i(0)],
                            vec![
                                leaf("Version", vec![i(101)]),
                                leaf("Name", vec![Prop::Str("")]),
                                leaf("MappingInformationType", vec![Prop::Str("ByVertice")]),
                                leaf("ReferenceInformationType", vec![Prop::Str("Direct")]),
                                leaf("Normals", vec![Prop::ArrF64(normals)]),
                            ],
                        ),
                        node(
                            "LayerElementUV",
                            vec![i(0)],
                            vec![
                                leaf("Version", vec![i(101)]),
                                leaf("Name", vec![Prop::Str("UVMap")]),
                                leaf("MappingInformationType", vec![Prop::Str("ByVertice")]),
                                leaf("ReferenceInformationType", vec![Prop::Str("Direct")]),
                                leaf("UV", vec![Prop::ArrF64(uvs)]),
                            ],
                        ),
                        node(
                            "Layer",
                            vec![i(0)],
                            vec![
                                leaf("Version", vec![i(100)]),
                                node(
                                    "LayerElement",
                                    vec![],
                                    vec![leaf("Type", vec![Prop::Str("LayerElementNormal")]), leaf("TypedIndex", vec![i(0)])],
                                ),
                                node(
                                    "LayerElement",
                                    vec![],
                                    vec![leaf("Type", vec![Prop::Str("LayerElementUV")]), leaf("TypedIndex", vec![i(0)])],
                                ),
                            ],
                        ),
                    ],
                ),
                node(
                    "Model",
                    vec![Prop::I64(MODEL_ID), Prop::Str(&model_name), Prop::Str("Mesh")],
                    vec![
                        leaf("Version", vec![i(232)]),
                        node("Properties70", vec![], vec![]),
                        leaf("MultiLayer", vec![i(0)]),
                        leaf("MultiTake", vec![i(0)]),
                        leaf("Shading", vec![Prop::Bool(true)]),
                        leaf("Culling", vec![Prop::Str("CullingOff")]),
                    ],
                ),
            ],
        ),
        node(
            "Connections",
            vec![],
            vec![
                leaf("C", vec![Prop::Str("OO"), Prop::I64(GEOM_ID), Prop::I64(MODEL_ID)]),
                leaf("C", vec![Prop::Str("OO"), Prop::I64(MODEL_ID), Prop::I64(0)]),
            ],
        ),
    ];

    let mut out = Vec::with_capacity(mesh.positions.len() * 40 + mesh.indices.len() * 4);
    out.extend_from_slice(b"Kaydara FBX Binary  \0\x1a\0");
    out.extend_from_slice(&VERSION.to_le_bytes());
    for n in &nodes {
        write_node(&mut out, n);
    }
    out.extend_from_slice(&[0; 13]);
    out.extend_from_slice(&FOOT_ID);
    out.extend_from_slice(&[0; 4]);
    let pad = match out.len() % 16 {
        0 => 16,
        r => 16 - r,
    };
    out.extend(std::iter::repeat_n(0u8, pad));
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&[0; 120]);
    out.extend_from_slice(&FOOT_MAGIC);
    out
}

pub fn write_fbx(path: &Path, mesh: &Mesh, name: &str) -> Result<()> {
    std::fs::write(path, fbx_bytes(mesh, name)).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
pub(crate) mod read {
    //! Minimal binary FBX reader for tests.
    use std::io::Read;

    #[derive(Debug, Clone, PartialEq)]
    pub enum P {
        I16(i16),
        Bool(bool),
        I32(i32),
        I64(i64),
        F32(f32),
        F64(f64),
        Str(Vec<u8>),
        Raw(Vec<u8>),
        ArrF64(Vec<f64>),
        ArrI32(Vec<i32>),
        Other,
    }

    #[derive(Debug, Clone)]
    pub struct N {
        pub name: String,
        pub props: Vec<P>,
        pub children: Vec<N>,
    }

    impl N {
        pub fn child(&self, name: &str) -> Option<&N> {
            self.children.iter().find(|c| c.name == name)
        }
        pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a N> {
            self.children.iter().filter(move |c| c.name == name)
        }
    }

    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    fn array(b: &[u8], o: &mut usize, width: usize) -> Vec<u8> {
        let (count, enc, len) = (u32_at(b, *o) as usize, u32_at(b, *o + 4), u32_at(b, *o + 8) as usize);
        let data = &b[*o + 12..*o + 12 + len];
        *o += 12 + len;
        let raw = if enc == 1 {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).unwrap();
            out
        } else {
            data.to_vec()
        };
        assert_eq!(raw.len(), count * width, "array length");
        raw
    }

    fn prop(b: &[u8], o: &mut usize) -> P {
        let t = b[*o];
        *o += 1;
        let take = |o: &mut usize, n: usize| {
            let s = &b[*o..*o + n];
            *o += n;
            s.to_vec()
        };
        match t {
            b'Y' => P::I16(i16::from_le_bytes(take(o, 2).try_into().unwrap())),
            b'C' => P::Bool(take(o, 1)[0] != 0),
            b'I' => P::I32(i32::from_le_bytes(take(o, 4).try_into().unwrap())),
            b'F' => P::F32(f32::from_le_bytes(take(o, 4).try_into().unwrap())),
            b'D' => P::F64(f64::from_le_bytes(take(o, 8).try_into().unwrap())),
            b'L' => P::I64(i64::from_le_bytes(take(o, 8).try_into().unwrap())),
            b'S' | b'R' => {
                let n = u32_at(b, *o) as usize;
                *o += 4;
                let s = take(o, n);
                if t == b'S' { P::Str(s) } else { P::Raw(s) }
            }
            b'd' => P::ArrF64(array(b, o, 8).chunks(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect()),
            b'i' => P::ArrI32(array(b, o, 4).chunks(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()),
            b'f' | b'l' | b'b' => {
                let w = match t {
                    b'f' => 4,
                    b'l' => 8,
                    _ => 1,
                };
                array(b, o, w);
                P::Other
            }
            other => panic!("unknown property type {other}"),
        }
    }

    /// Parse one node at `o`; None at a null record.
    fn node(b: &[u8], o: &mut usize) -> Option<N> {
        let end = u32_at(b, *o) as usize;
        let nprops = u32_at(b, *o + 4) as usize;
        let name_len = b[*o + 12] as usize;
        if end == 0 {
            *o += 13;
            return None;
        }
        let name = String::from_utf8(b[*o + 13..*o + 13 + name_len].to_vec()).unwrap();
        *o += 13 + name_len;
        let props = (0..nprops).map(|_| prop(b, o)).collect();
        let mut children = Vec::new();
        while *o < end {
            match node(b, o) {
                Some(c) => children.push(c),
                None => break,
            }
        }
        assert_eq!(*o, end, "node {name} ends at its end offset");
        Some(N { name, props, children })
    }

    /// (version, top-level nodes, offset just past the top-level null record).
    pub fn parse(b: &[u8]) -> (u32, Vec<N>, usize) {
        assert_eq!(&b[..21], b"Kaydara FBX Binary  \0");
        assert_eq!(&b[21..23], &[0x1a, 0x00]);
        let version = u32_at(b, 23);
        let mut o = 27;
        let mut nodes = Vec::new();
        while let Some(n) = node(b, &mut o) {
            nodes.push(n);
        }
        (version, nodes, o)
    }
}

#[cfg(test)]
mod tests {
    use super::read::{N, P, parse};
    use super::*;

    /// A 2×2-quad patch with a bump in its north-east (x = 20 m, z = 0) corner.
    fn patch() -> Mesh {
        let mut m = Mesh::default();
        for r in 0..3 {
            for c in 0..3 {
                let h = if (c, r) == (2, 0) { 50.0 } else { 10.0 };
                m.positions.push([c as f32 * 10.0, h, r as f32 * 10.0]);
                m.normals.push([0.0, 1.0, 0.0]);
                m.uvs.push([c as f32 / 2.0, r as f32 / 2.0]);
            }
        }
        for r in 0..2u32 {
            for c in 0..2u32 {
                let v = |c: u32, r: u32| r * 3 + c;
                m.indices.extend_from_slice(&[v(c, r), v(c, r + 1), v(c + 1, r), v(c + 1, r), v(c, r + 1), v(c + 1, r + 1)]);
            }
        }
        m
    }

    fn top<'a>(nodes: &'a [N], name: &str) -> &'a N {
        nodes.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("missing {name}"))
    }

    fn p70<'a>(n: &'a N, key: &str) -> &'a P {
        let props = n.child("Properties70").expect("Properties70");
        let p = props.all("P").find(|p| p.props[0] == P::Str(key.as_bytes().to_vec())).unwrap_or_else(|| panic!("P {key}"));
        p.props.last().unwrap()
    }

    #[test]
    fn file_has_the_fbx_7400_header_nodes_and_footer() {
        let b = fbx_bytes(&patch(), "tile");
        let (version, nodes, end) = parse(&b);
        assert_eq!(version, 7400);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        for want in ["FBXHeaderExtension", "FileId", "CreationTime", "Creator", "GlobalSettings", "Documents", "References", "Definitions", "Objects", "Connections"] {
            assert!(names.contains(&want), "missing {want}: {names:?}");
        }
        // Footer: id, zero padding to 16-byte alignment, version, 120 zeros, magic.
        let tail = &b[end..];
        assert!(tail.ends_with(&[0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29, 0x0b]));
        let v_at = b.len() - 16 - 120 - 4;
        assert_eq!(v_at % 16, 0, "version word is 16-byte aligned");
        assert_eq!(u32::from_le_bytes(b[v_at..v_at + 4].try_into().unwrap()), 7400);
    }

    #[test]
    fn global_settings_say_y_up_metres() {
        let (_, nodes, _) = parse(&fbx_bytes(&patch(), "tile"));
        let g = top(&nodes, "GlobalSettings");
        assert_eq!(p70(g, "UpAxis"), &P::I32(1));
        assert_eq!(p70(g, "FrontAxis"), &P::I32(2));
        assert_eq!(p70(g, "CoordAxis"), &P::I32(0));
        assert_eq!(p70(g, "UnitScaleFactor"), &P::F64(100.0));
    }

    #[test]
    fn geometry_holds_rotated_vertices_triangles_normals_and_uvs() {
        let m = patch();
        let (_, nodes, _) = parse(&fbx_bytes(&m, "tile"));
        let objects = top(&nodes, "Objects");
        let geom = objects.child("Geometry").expect("Geometry");
        assert_eq!(geom.props[1], P::Str(b"tile\x00\x01Geometry".to_vec()));
        let P::ArrF64(v) = &geom.child("Vertices").unwrap().props[0] else { panic!() };
        assert_eq!(v.len(), m.positions.len() * 3);
        // Rotated 180° about Y: the NE bump (x 20, z 0) sits at (−20, 50, −0).
        let bump = 2;
        assert_eq!([v[bump * 3], v[bump * 3 + 1], v[bump * 3 + 2]], [-20.0, 50.0, -0.0]);
        let P::ArrI32(pvi) = &geom.child("PolygonVertexIndex").unwrap().props[0] else { panic!() };
        assert_eq!(pvi.len(), m.indices.len());
        let decoded: Vec<u32> = pvi.iter().map(|&i| if i < 0 { !i as u32 } else { i as u32 }).collect();
        assert_eq!(decoded, m.indices);
        assert!(pvi.iter().enumerate().all(|(k, &i)| (i < 0) == (k % 3 == 2)), "every third index closes a triangle");
        let normals = geom.child("LayerElementNormal").unwrap();
        let P::ArrF64(n) = &normals.child("Normals").unwrap().props[0] else { panic!() };
        assert_eq!(n.len(), m.normals.len() * 3);
        let uvs = geom.child("LayerElementUV").unwrap();
        let P::ArrF64(uv) = &uvs.child("UV").unwrap().props[0] else { panic!() };
        assert_eq!(uv.len(), m.uvs.len() * 2);
        assert_eq!(uvs.child("MappingInformationType").unwrap().props[0], P::Str(b"ByVertice".to_vec()));
        let layer = geom.child("Layer").unwrap();
        assert_eq!(layer.all("LayerElement").count(), 2);
    }

    #[test]
    fn model_is_connected_to_geometry_and_scene_root() {
        let (_, nodes, _) = parse(&fbx_bytes(&patch(), "tile"));
        let objects = top(&nodes, "Objects");
        let P::I64(geom_id) = objects.child("Geometry").unwrap().props[0] else { panic!() };
        let model = objects.child("Model").unwrap();
        let P::I64(model_id) = model.props[0] else { panic!() };
        assert_eq!(model.props[2], P::Str(b"Mesh".to_vec()));
        let links: Vec<(i64, i64)> = top(&nodes, "Connections")
            .all("C")
            .map(|c| match (&c.props[1], &c.props[2]) {
                (P::I64(a), P::I64(b)) => (*a, *b),
                _ => panic!(),
            })
            .collect();
        assert!(links.contains(&(geom_id, model_id)));
        assert!(links.contains(&(model_id, 0)));
    }

    #[test]
    fn a_full_tile_is_compressed_and_round_trips() {
        let n = 376usize;
        let heights: Vec<f32> = (0..n * n).map(|i| ((i % n) as f32 * 0.37).sin() * 30.0 + (i / n) as f32 * 0.1).collect();
        let normals = vec![[0.0, 1.0, 0.0]; n * n];
        let m = crate::mesh::build_mesh(&heights, &normals, 0);
        let b = fbx_bytes(&m, "big");
        let (_, nodes, _) = parse(&b);
        let geom = top(&nodes, "Objects").child("Geometry").unwrap();
        let P::ArrF64(v) = &geom.child("Vertices").unwrap().props[0] else { panic!() };
        assert_eq!(v.len(), m.positions.len() * 3);
        let raw = m.positions.len() * 8 * 3 + m.normals.len() * 8 * 3 + m.uvs.len() * 8 * 2 + m.indices.len() * 4;
        assert!(b.len() < raw, "compressed: {} < {raw}", b.len());
    }
}
