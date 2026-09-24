//! Height tile → glTF-convention grid mesh at a given LOD, with skirts.
//!
//! Axes: +X east, +Y up (elevation), +Z south; origin at the tile's NW corner.

use crate::grid::{LOD_STRIDES, NODE_SPACING_M, NODES_PER_SIDE};

/// How far skirts drop below the edge, hiding cracks between tiles at different LODs.
pub const SKIRT_DEPTH_M: f32 = 50.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// A tile's 10 m heights plus its four edge neighbors (None when absent or empty).
pub struct Neighborhood<'a> {
    pub center: &'a [f32],
    pub north: Option<&'a [f32]>,
    pub south: Option<&'a [f32]>,
    pub east: Option<&'a [f32]>,
    pub west: Option<&'a [f32]>,
}

impl Neighborhood<'_> {
    /// Height at node (i, j), reaching one node across an edge into a neighbor. Edge nodes
    /// are shared, so our node −1 is the west tile's node 374 and our node 376 is the east tile's node 1.
    fn height(&self, i: isize, j: isize) -> Option<f32> {
        let n = NODES_PER_SIDE as isize;
        let at = |t: &[f32], i: isize, j: isize| t[(j * n + i) as usize];
        let inside = |k: isize| (0..n).contains(&k);
        match (inside(i), inside(j)) {
            (true, true) => Some(at(self.center, i, j)),
            (false, true) if i == -1 => self.west.map(|t| at(t, n - 2, j)),
            (false, true) if i == n => self.east.map(|t| at(t, 1, j)),
            (true, false) if j == -1 => self.north.map(|t| at(t, i, n - 2)),
            (true, false) if j == n => self.south.map(|t| at(t, i, 1)),
            _ => None,
        }
    }
}

/// Unit normals at every 10 m node, from central differences (one-sided where a neighbor is missing).
pub fn compute_normals(nb: &Neighborhood) -> Vec<[f32; 3]> {
    let n = NODES_PER_SIDE as isize;
    let d = NODE_SPACING_M as f32;
    let mut out = Vec::with_capacity((n * n) as usize);
    for j in 0..n {
        for i in 0..n {
            let h = nb.height(i, j).expect("center node");
            let dhdx = slope(nb.height(i - 1, j), h, nb.height(i + 1, j), d);
            let dhdz = slope(nb.height(i, j - 1), h, nb.height(i, j + 1), d);
            let len = (dhdx * dhdx + 1.0 + dhdz * dhdz).sqrt();
            out.push([-dhdx / len, 1.0 / len, -dhdz / len]);
        }
    }
    out
}

fn slope(before: Option<f32>, here: f32, after: Option<f32>, d: f32) -> f32 {
    match (before, after) {
        (Some(b), Some(a)) => (a - b) / (2.0 * d),
        (None, Some(a)) => (a - here) / d,
        (Some(b), None) => (here - b) / d,
        (None, None) => 0.0,
    }
}

/// Grid mesh taking every `LOD_STRIDES[lod]`-th node, plus skirts on all four edges.
pub fn build_mesh(heights: &[f32], normals: &[[f32; 3]], lod: usize) -> Mesh {
    let stride = LOD_STRIDES[lod];
    let n = (NODES_PER_SIDE - 1) / stride + 1;
    let step = NODE_SPACING_M as f32 * stride as f32;
    let mut m = Mesh::default();
    for r in 0..n {
        for c in 0..n {
            let k = r * stride * NODES_PER_SIDE + c * stride;
            m.positions.push([c as f32 * step, heights[k], r as f32 * step]);
            m.normals.push(normals[k]);
            m.uvs.push([c as f32 / (n - 1) as f32, r as f32 / (n - 1) as f32]);
        }
    }
    let v = |c: usize, r: usize| (r * n + c) as u32;
    for r in 0..n - 1 {
        for c in 0..n - 1 {
            let (a, b, cc, d) = (v(c, r), v(c + 1, r), v(c, r + 1), v(c + 1, r + 1));
            m.indices.extend_from_slice(&[a, cc, b, b, cc, d]);
        }
    }
    // Edges walked clockwise seen from above, so each skirt faces outward.
    let north: Vec<u32> = (0..n).map(|c| v(c, 0)).collect();
    let east: Vec<u32> = (0..n).map(|r| v(n - 1, r)).collect();
    let south: Vec<u32> = (0..n).rev().map(|c| v(c, n - 1)).collect();
    let west: Vec<u32> = (0..n).rev().map(|r| v(0, r)).collect();
    for edge in [north, east, south, west] {
        add_skirt(&mut m, &edge);
    }
    m
}

fn add_skirt(m: &mut Mesh, edge: &[u32]) {
    let base = m.positions.len() as u32;
    for &e in edge {
        let e = e as usize;
        let [x, y, z] = m.positions[e];
        m.positions.push([x, y - SKIRT_DEPTH_M, z]);
        m.normals.push(m.normals[e]);
        m.uvs.push(m.uvs[e]);
    }
    for k in 0..edge.len() - 1 {
        let (a, b) = (edge[k], edge[k + 1]);
        let (a2, b2) = (base + k as u32, base + k as u32 + 1);
        m.indices.extend_from_slice(&[a, b, a2, b, b2, a2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = NODES_PER_SIDE;

    /// Heights from a function of tile-local meters (x east, z south).
    fn field(f: impl Fn(f32, f32) -> f32) -> Vec<f32> {
        (0..N * N).map(|k| f((k % N) as f32 * 10.0, (k / N) as f32 * 10.0)).collect()
    }

    fn alone(center: &[f32]) -> Neighborhood<'_> {
        Neighborhood { center, north: None, south: None, east: None, west: None }
    }

    fn tri_normal(m: &Mesh, t: usize) -> [f32; 3] {
        let [a, b, c] = [0, 1, 2].map(|k| m.positions[m.indices[3 * t + k] as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    fn unit(v: [f32; 3]) -> [f32; 3] {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / l, v[1] / l, v[2] / l]
    }

    #[test]
    fn vertex_and_triangle_counts_per_lod() {
        let h = field(|_, _| 0.0);
        let normals = compute_normals(&alone(&h));
        for (lod, n) in [(0, 376), (1, 126), (2, 26), (3, 6)] {
            let m = build_mesh(&h, &normals, lod);
            assert_eq!(m.positions.len(), n * n + 4 * n, "lod {lod}");
            assert_eq!(m.normals.len(), m.positions.len());
            assert_eq!(m.uvs.len(), m.positions.len());
            assert_eq!(m.indices.len() / 3, 2 * (n - 1) * (n - 1) + 8 * (n - 1), "lod {lod}");
            assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
        }
    }

    #[test]
    fn top_surface_faces_up_and_spans_the_tile() {
        let h = field(|_, _| 100.0);
        let m = build_mesh(&h, &compute_normals(&alone(&h)), 1);
        let n = 126;
        for t in 0..2 * (n - 1) * (n - 1) {
            assert!(tri_normal(&m, t)[1] > 0.0, "triangle {t} faces down");
        }
        assert_eq!(m.positions[0], [0.0, 100.0, 0.0]);
        assert_eq!(m.positions[n * n - 1], [3750.0, 100.0, 3750.0]);
        assert_eq!(m.uvs[0], [0.0, 0.0]);
        assert_eq!(m.uvs[n * n - 1], [1.0, 1.0]);
        assert_eq!(m.normals[0], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn skirts_hang_down_and_face_outward() {
        let h = field(|_, _| 100.0);
        let m = build_mesh(&h, &compute_normals(&alone(&h)), 2);
        let n = 26;
        let first_skirt_tri = 2 * (n - 1) * (n - 1);
        for t in first_skirt_tri..m.indices.len() / 3 {
            let nrm = tri_normal(&m, t);
            let c = [0, 1, 2].map(|k| m.positions[m.indices[3 * t + k] as usize]);
            let (cx, cz) = ((c[0][0] + c[1][0] + c[2][0]) / 3.0 - 1875.0, (c[0][2] + c[1][2] + c[2][2]) / 3.0 - 1875.0);
            assert!(nrm[0] * cx + nrm[2] * cz > 0.0, "skirt triangle {t} faces inward");
        }
        assert!(m.positions[n * n..].iter().all(|p| p[1] == 100.0 - SKIRT_DEPTH_M));
    }

    #[test]
    fn normals_follow_slopes() {
        let east = field(|x, _| 0.1 * x);
        for nrm in compute_normals(&alone(&east)) {
            assert!(close(nrm, unit([-0.1, 1.0, 0.0])), "{nrm:?}");
        }
        let south = field(|_, z| 0.2 * z);
        for nrm in compute_normals(&alone(&south)) {
            assert!(close(nrm, unit([0.0, 1.0, -0.2])), "{nrm:?}");
        }
    }

    #[test]
    fn normals_without_neighbors_use_one_sided_differences() {
        // A crease at the east edge: one-sided differences must not panic and stay unit length.
        let h = field(|x, _| if x >= 3740.0 { 50.0 } else { 0.0 });
        for nrm in compute_normals(&alone(&h)) {
            let l = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
            assert!((l - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn edge_normals_reach_into_neighbors() {
        let center = field(|_, _| 0.0);
        let east = field(|x, _| if x == 10.0 { 20.0 } else { 0.0 }); // east node 1 = 20 m
        let nb = Neighborhood { center: &center, north: None, south: None, east: Some(&east), west: None };
        let normals = compute_normals(&nb);
        // At center node 375: dh/dx = (20 − 0) / 20 m = 1.
        assert!(close(normals[100 * N + N - 1], unit([-1.0, 1.0, 0.0])));
        assert_eq!(normals[100 * N + N - 2], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn adjacent_tiles_meet_exactly_at_every_lod() {
        // Global field sampled for two tiles sharing an edge.
        let g = |x: f32, z: f32| (x * 0.01).sin() * 40.0 + z * 0.05;
        let west = field(|x, z| g(x, z));
        let east = field(|x, z| g(x + 3750.0, z));
        let wn = compute_normals(&Neighborhood { center: &west, north: None, south: None, east: Some(&east), west: None });
        let en = compute_normals(&Neighborhood { center: &east, north: None, south: None, east: None, west: Some(&west) });
        for (lod, &stride) in LOD_STRIDES.iter().enumerate() {
            let (a, b) = (build_mesh(&west, &wn, lod), build_mesh(&east, &en, lod));
            let n = (N - 1) / stride + 1;
            for r in 0..n {
                let (pa, pb) = (a.positions[r * n + n - 1], b.positions[r * n]);
                assert_eq!((pa[0], pa[1].to_bits(), pa[2]), (pb[0] + 3750.0, pb[1].to_bits(), pb[2]), "lod {lod} row {r}");
                assert_eq!(a.normals[r * n + n - 1], b.normals[r * n], "lod {lod} row {r}");
            }
        }
    }
}
