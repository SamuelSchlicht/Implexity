// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![allow(clippy::cast_possible_wrap)]

use serde::Serialize;
use serde_json::{Value, json};

use crate::numeric::pairwise_sum;

pub type Vec3 = [f64; 3];
pub type Tri = [usize; 3];

#[inline]
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
#[must_use]
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[inline]
#[must_use]
pub fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
#[must_use]
pub fn norm(a: Vec3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}


#[must_use]
pub fn weld_exact(vertices: &[Vec3], faces: &[Tri]) -> (Vec<Vec3>, Vec<Tri>, usize) {
    let mut order: Vec<usize> = (0..vertices.len()).collect();
    let key_cmp = |a: &Vec3, b: &Vec3| {
        a[0].partial_cmp(&b[0])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal))
            .then(a[2].partial_cmp(&b[2]).unwrap_or(std::cmp::Ordering::Equal))
    };
    order.sort_by(|&i, &j| key_cmp(&vertices[i], &vertices[j]).then(i.cmp(&j)));
    let mut inverse = vec![0usize; vertices.len()];
    let mut unique: Vec<Vec3> = Vec::with_capacity(vertices.len());
    for &i in &order {
        let v = vertices[i];
        let same = unique.last().is_some_and(|u| key_cmp(u, &v) == std::cmp::Ordering::Equal);
        if !same {
            unique.push(v);
        }
        inverse[i] = unique.len() - 1;
    }
    let mut out = Vec::with_capacity(faces.len());
    let mut dropped = 0;
    for f in faces {
        let t = [inverse[f[0]], inverse[f[1]], inverse[f[2]]];
        if t[0] != t[1] && t[1] != t[2] && t[2] != t[0] {
            out.push(t);
        } else {
            dropped += 1;
        }
    }
    (unique, out, dropped)
}

#[must_use]
pub fn triple_products(vertices: &[Vec3], faces: &[Tri]) -> Vec<f64> {
    faces.iter().map(|f| dot(vertices[f[0]], cross(vertices[f[1]], vertices[f[2]]))).collect()
}

#[must_use]
pub fn signed_volume(vertices: &[Vec3], faces: &[Tri]) -> f64 {
    pairwise_sum(&triple_products(vertices, faces)) / 6.0
}

#[must_use]
pub fn areas(vertices: &[Vec3], faces: &[Tri]) -> Vec<f64> {
    faces
        .iter()
        .map(|f| {
            let a = sub(vertices[f[1]], vertices[f[0]]);
            let b = sub(vertices[f[2]], vertices[f[0]]);
            0.5 * norm(cross(a, b))
        })
        .collect()
}

fn directed_edges(faces: &[Tri]) -> Vec<[usize; 2]> {
    let mut e = Vec::with_capacity(faces.len() * 3);
    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
        e.extend(faces.iter().map(|f| [f[a], f[b]]));
    }
    e
}

fn unique_counts(mut rows: Vec<[usize; 2]>) -> Vec<([usize; 2], usize)> {
    rows.sort_unstable();
    let mut out: Vec<([usize; 2], usize)> = Vec::new();
    for r in rows {
        match out.last_mut() {
            Some((k, c)) if *k == r => *c += 1,
            _ => out.push((r, 1)),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrientationReport {
    pub directed_edges: usize,
    pub directed_edges_seen_twice: usize,
    pub undirected_edges: usize,
    pub undirected_edges_not_shared_by_two: usize,
    pub consistent: bool,
}

#[must_use]
pub fn orientation_report(faces: &[Tri]) -> OrientationReport {
    let e = directed_edges(faces);
    let directed = unique_counts(e.clone());
    let twice = directed.iter().filter(|(_, c)| *c > 1).count();
    let undirected = unique_counts(e.iter().map(|r| [r[0].min(r[1]), r[0].max(r[1])]).collect());
    OrientationReport {
        directed_edges: e.len(),
        directed_edges_seen_twice: twice,
        undirected_edges: undirected.len(),
        undirected_edges_not_shared_by_two: undirected.iter().filter(|(_, c)| *c != 2).count(),
        consistent: twice == 0,
    }
}

impl OrientationReport {
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Clone, Debug)]
pub struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self { parent: (0..n).collect() }
    }

    pub fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    pub fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

#[must_use]
pub fn connected_components(n: usize, edges: impl IntoIterator<Item = [usize; 2]>) -> (usize, Vec<usize>) {
    let mut uf = UnionFind::new(n);
    for [a, b] in edges {
        uf.union(a, b);
    }
    let mut label_of_root = vec![usize::MAX; n];
    let mut labels = vec![0; n];
    let mut count = 0;
    for (i, label) in labels.iter_mut().enumerate() {
        let r = uf.find(i);
        if label_of_root[r] == usize::MAX {
            label_of_root[r] = count;
            count += 1;
        }
        *label = label_of_root[r];
    }
    (count, labels)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Topology {
    pub vertices: usize,
    pub edges: usize,
    pub faces: usize,
    pub euler_characteristic: i64,
    pub components: usize,
    pub boundary_edges: usize,
    pub boundary_loops: usize,
    pub nonmanifold_edges: usize,
    pub genus: f64,
    #[serde(skip)]
    pub labels: Vec<usize>,
}

impl Topology {
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[must_use]
pub fn topology_fast(vertices: &[Vec3], faces: &[Tri]) -> Topology {
    let nv = vertices.len();
    let nf = faces.len();
    let undirected =
        unique_counts(directed_edges(faces).into_iter().map(|r| [r[0].min(r[1]), r[0].max(r[1])]).collect());
    let ne = undirected.len();
    let chi = nv as i64 - ne as i64 + nf as i64;
    let boundary: Vec<[usize; 2]> = undirected.iter().filter(|(_, c)| *c == 1).map(|(k, _)| *k).collect();
    let nonmanifold = undirected.iter().filter(|(_, c)| *c > 2).count();
    let (ncomp, labels) = connected_components(nv, undirected.iter().map(|(k, _)| *k));
    let mut nloop = 0;
    if !boundary.is_empty() {
        let (_n, lb) = connected_components(nv, boundary.iter().copied());
        let mut seen: Vec<usize> = boundary.iter().flat_map(|e| [lb[e[0]], lb[e[1]]]).collect();
        seen.sort_unstable();
        seen.dedup();
        nloop = seen.len();
    }
    let genus = (2.0 * ncomp as f64 - chi as f64 - nloop as f64) / 2.0;
    Topology {
        vertices: nv,
        edges: ne,
        faces: nf,
        euler_characteristic: chi,
        components: ncomp,
        boundary_edges: boundary.len(),
        boundary_loops: nloop,
        nonmanifold_edges: nonmanifold,
        genus,
        labels,
    }
}

#[must_use]
pub fn face_components(faces: &[Tri], nv: usize) -> (Vec<usize>, usize, Vec<usize>) {
    let (count, labels) = connected_components(nv, directed_edges(faces));
    (faces.iter().map(|f| labels[f[0]]).collect(), count, labels)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ComponentRow {
    pub component: usize,
    pub triangles: usize,
    pub area_mm2: f64,
    pub volume_mm3: f64,
    pub euler_characteristic: i64,
    pub genus: f64,
    pub boundary_edges: usize,
    pub nonmanifold_edges: usize,
}

#[must_use]
pub fn component_table(
    vertices: &[Vec3],
    faces: &[Tri],
    limit: usize,
) -> (Vec<ComponentRow>, Vec<usize>, Vec<usize>, Vec<f64>) {
    let (cid, ncomp, cvert) = face_components(faces, vertices.len());
    let ar = areas(vertices, faces);
    let vol: Vec<f64> = triple_products(vertices, faces).into_iter().map(|t| t / 6.0).collect();
    let undirected =
        unique_counts(directed_edges(faces).into_iter().map(|r| [r[0].min(r[1]), r[0].max(r[1])]).collect());
    let mut n_v = vec![0i64; ncomp];
    for &c in &cvert {
        n_v[c] += 1;
    }
    let mut n_e = vec![0i64; ncomp];
    let mut n_b = vec![0usize; ncomp];
    let mut n_nm = vec![0usize; ncomp];
    for (k, c) in &undirected {
        let comp = cvert[k[0]];
        n_e[comp] += 1;
        if *c == 1 {
            n_b[comp] += 1;
        }
        if *c > 2 {
            n_nm[comp] += 1;
        }
    }
    let mut n_f = vec![0i64; ncomp];
    let mut area_c = vec![0.0; ncomp];
    let mut vol_c = vec![0.0; ncomp];
    for (i, &c) in cid.iter().enumerate() {
        n_f[c] += 1;
        area_c[c] += ar[i];
        vol_c[c] += vol[i];
    }
    let mut order: Vec<usize> = (0..ncomp).collect();
    order.sort_by(|&a, &b| vol_c[b].abs().partial_cmp(&vol_c[a].abs()).unwrap_or(std::cmp::Ordering::Equal));
    let rows = order
        .iter()
        .take(limit)
        .map(|&c| {
            let chi = n_v[c] - n_e[c] + n_f[c];
            ComponentRow {
                component: c,
                triangles: usize::try_from(n_f[c]).unwrap_or(0),
                area_mm2: area_c[c] * 1e6,
                volume_mm3: vol_c[c] * 1e9,
                euler_characteristic: chi,
                genus: (2 - chi) as f64 / 2.0,
                boundary_edges: n_b[c],
                nonmanifold_edges: n_nm[c],
            }
        })
        .collect();
    (rows, cid, cvert, vol_c)
}

#[must_use]
pub fn surface_report(vertices: &[[f32; 3]], faces: &[Tri]) -> Value {
    if faces.is_empty() {
        return json!({"triangles": 0, "vertices": 0, "empty": true,
            "watertight": false, "boundary_edges": 0,
            "nonmanifold_edges": 0, "orientation_consistent": true,
            "volume_mm3": 0.0, "area_mm2": 0.0, "bbox_mm": Value::Null});
    }
    let v: Vec<Vec3> = vertices.iter().map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]).collect();
    let nv = faces.iter().flat_map(|f| f.iter().copied()).max().unwrap_or(0) + 1;
    let e = directed_edges(faces);
    let directed = unique_counts(e.clone());
    let undirected = unique_counts(e.iter().map(|r| [r[0].min(r[1]), r[0].max(r[1])]).collect());
    let boundary = undirected.iter().filter(|(_, c)| *c == 1).count();
    let nonmanifold = undirected.iter().filter(|(_, c)| *c > 2).count();
    let volume = signed_volume(&v, faces);
    let area = 0.5 * pairwise_sum(&cross_norms(&v, faces));
    let (components, _) = connected_components(nv, e.iter().copied());
    let chi = nv as i64 - undirected.len() as i64 + faces.len() as i64;
    let closed = boundary == 0 && nonmanifold == 0;
    let consistent = directed.iter().all(|(_, c)| *c == 1);
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &v {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    json!({
        "triangles": faces.len(), "vertices": nv, "empty": false,
        "undirected_edges": undirected.len(),
        "boundary_edges": boundary, "nonmanifold_edges": nonmanifold,
        "every_edge_shared_by_two_triangles": undirected.iter().all(|(_, c)| *c == 2),
        "orientation_consistent": consistent,
        "watertight": closed && consistent,
        "components": components,
        "euler_characteristic": chi,
        "genus_total": if closed { json!((2 * components as i64 - chi) as f64 / 2.0) } else { Value::Null },
        "volume_mm3": volume, "area_mm2": area,
        "bbox_mm": [lo, hi],
    })
}

fn cross_norms(v: &[Vec3], faces: &[Tri]) -> Vec<f64> {
    faces
        .iter()
        .map(|f| {
            let a = v[f[0]];
            norm(cross(sub(v[f[1]], a), sub(v[f[2]], a)))
        })
        .collect()
}

