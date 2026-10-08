// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::numeric::median;

const CORNERS: [[usize; 3]; 8] =
    [[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0], [0, 0, 1], [1, 0, 1], [0, 1, 1], [1, 1, 1]];
const TETS: [[usize; 4]; 6] =
    [[0, 1, 3, 7], [0, 2, 3, 7], [0, 2, 6, 7], [0, 4, 6, 7], [0, 4, 5, 7], [0, 1, 5, 7]];
pub const FACE_BASE: i64 = 1 << 62;
pub const MAX_NODES: usize = 1 << 26;
pub const DEADBAND_FRAC: f64 = 1e-4;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PartitionError {
    #[error("{0}")]
    Invalid(String),
    #[error("the closed export grid needs {nodes} samples, above the {max_nodes}-sample bound")]
    GridTooLarge {
        nodes: u128,
        max_nodes: u128,
    },
}

fn invalid(m: &str) -> PartitionError {
    PartitionError::Invalid(m.to_string())
}



pub fn closed_grid_counts(
    lower: [f64; 3],
    upper: [f64; 3],
    spacing: [f64; 3],
) -> Result<([u128; 3], [f64; 3]), PartitionError> {
    if lower.iter().chain(upper.iter()).any(|v| !v.is_finite()) || (0..3).any(|a| upper[a] <= lower[a]) {
        return Err(invalid("the export box must be finite with hi > lo"));
    }
    if spacing.iter().any(|s| !s.is_finite() || *s <= 0.0) {
        return Err(invalid("the export spacing must be positive and finite"));
    }
    let mut cells = [0u128; 3];
    let mut steps = [0.0; 3];
    for a in 0..3 {
        let length = upper[a] - lower[a];
        let ratio = length / spacing[a];
        if !ratio.is_finite() {
            return Err(invalid("the export spacing is too small for the box"));
        }
        let count = (ratio - 1e-9).ceil().max(1.0);
        cells[a] = count as u128;
        steps[a] = length / count;
    }
    Ok((cells, steps))
}



pub fn closed_grid_axes(
    lower: [f64; 3],
    upper: [f64; 3],
    spacing: [f64; 3],
    max_nodes: Option<usize>,
) -> Result<([Vec<f64>; 3], Value), PartitionError> {
    let (cells, steps) = closed_grid_counts(lower, upper, spacing)?;
    let nodes: u128 = cells.iter().fold(1u128, |acc, &c| acc.saturating_mul(c.saturating_add(2)));
    let bound = max_nodes.map_or(MAX_NODES as u128 * 64, |m| m as u128);
    if nodes > bound {
        return Err(PartitionError::GridTooLarge { nodes, max_nodes: bound });
    }
    let cells: [usize; 3] = cells.map(|c| c as usize);
    let axes: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let mut v = Vec::with_capacity(cells[a] + 2);
        v.push(lower[a]);
        v.extend((0..cells[a]).map(|i| lower[a] + (i as f64 + 0.5) * steps[a]));
        v.push(upper[a]);
        v
    });
    let record = json!({
        "scheme": "cell_centred_with_box_face_nodes",
        "bbox_mm": [lower, upper],
        "cells": cells,
        "shape": [axes[0].len(), axes[1].len(), axes[2].len()],
        "nodes": nodes as u64,
        "spacing_mm": steps,
        "max_spacing_mm": steps.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "face_layer_spacing_mm": steps.map(|s| 0.5 * s),
    });
    Ok((axes, record))
}

fn newell(points: &[[f64; 3]]) -> [f64; 3] {
    let n = points.len();
    let mut s = [0.0; 3];
    for i in 0..n {
        let p = points[i];
        let q = points[(i + 1) % n];
        s[0] += (p[1] - q[1]) * (p[2] + q[2]);
        s[1] += (p[2] - q[2]) * (p[0] + q[0]);
        s[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    s
}

fn case_polygons(case: usize) -> Vec<Vec<(usize, usize)>> {
    let inside: Vec<usize> = (0..4).filter(|j| case >> j & 1 == 1).collect();
    let outside: Vec<usize> = (0..4).filter(|j| case >> j & 1 == 0).collect();
    match inside.len() {
        0 | 4 => vec![],
        1 => {
            let a = inside[0];
            vec![vec![(a, outside[0]), (a, outside[1]), (a, outside[2])]]
        }
        3 => {
            let a = outside[0];
            vec![vec![(inside[0], a), (inside[1], a), (inside[2], a)]]
        }
        _ => {
            let (a, b) = (inside[0], inside[1]);
            let (c, d) = (outside[0], outside[1]);
            vec![vec![(a, c), (a, d), (b, d), (b, c)]]
        }
    }
}

type MarchEntry = (usize, usize);

struct Tables {
    march: Vec<Vec<Vec<Vec<MarchEntry>>>>,
    faces: [[[usize; 3]; 4]; 6],
    sides: [[Option<(usize, usize)>; 4]; 6],
}

fn tables() -> Tables {
    let mut march = Vec::with_capacity(6);
    let mut faces = [[[0usize; 3]; 4]; 6];
    let mut sides = [[None; 4]; 6];
    for (t, chain) in TETS.iter().enumerate() {
        let pts: Vec<[f64; 3]> = chain.iter().map(|&c| CORNERS[c].map(|v| v as f64)).collect();
        let mut by_case = vec![Vec::new(); 15];
        for (case, slot) in by_case.iter_mut().enumerate().take(15).skip(1) {
            let outside: Vec<usize> = (0..4).filter(|j| case >> j & 1 == 0).collect();
            for mut poly in case_polygons(case) {
                let mids: Vec<[f64; 3]> = poly
                    .iter()
                    .map(|&(i, j)| std::array::from_fn(|a| f64::midpoint(pts[i][a], pts[j][a])))
                    .collect();
                let m = mids.len() as f64;
                let mid_mean: [f64; 3] = std::array::from_fn(|a| mids.iter().map(|p| p[a]).sum::<f64>() / m);
                let out_mean: [f64; 3] = std::array::from_fn(|a| {
                    outside.iter().map(|&j| pts[j][a]).sum::<f64>() / outside.len() as f64
                });
                let rf: [f64; 3] = std::array::from_fn(|a| out_mean[a] - mid_mean[a]);
                let nw = newell(&mids);
                if nw[0] * rf[0] + nw[1] * rf[1] + nw[2] * rf[2] < 0.0 {
                    poly.reverse();
                }
                let entry = poly
                    .iter()
                    .map(|&(i, j)| {
                        let (lo, hi) = if chain[i] < chain[j] { (i, j) } else { (j, i) };
                        (lo, chain[hi] - chain[lo])
                    })
                    .collect();
                slot.push(entry);
            }
        }
        march.push(by_case);
        for opposite in 0..4 {
            let mut tri: Vec<usize> = (0..4).filter(|&j| j != opposite).collect();
            let fp: Vec<[f64; 3]> = tri.iter().map(|&j| pts[j]).collect();
            let fmean: [f64; 3] = std::array::from_fn(|a| fp.iter().map(|p| p[a]).sum::<f64>() / 3.0);
            let out: [f64; 3] = std::array::from_fn(|a| fmean[a] - pts[opposite][a]);
            let nw = newell(&fp);
            if nw[0] * out[0] + nw[1] * out[1] + nw[2] * out[2] < 0.0 {
                tri = vec![tri[0], tri[2], tri[1]];
            }
            faces[t][opposite] = [tri[0], tri[1], tri[2]];
            let mut side = None;
            for axis in 0..3 {
                let bits: Vec<usize> = tri.iter().map(|&j| (chain[j] >> axis) & 1).collect();
                if bits.iter().all(|&b| b == bits[0]) {
                    side = Some((axis, bits[0]));
                }
            }
            sides[t][opposite] = side;
        }
    }
    Tables { march, faces, sides }
}

struct Context<'a> {
    sx: usize,
    sy: usize,
    nodes: i64,
    functions: &'a [Vec<f64>],
    count: i64,
    face_keys: Vec<[i64; 5]>,
    face_index: HashMap<[i64; 5], usize>,
    weights_cache: HashMap<i64, (Vec<i64>, Vec<f64>)>,
    values_cache: HashMap<(i64, i64), f64>,
    cross_cache: HashMap<(i64, i64, i64), i64>,
}

type Poly = Vec<i64>;
type Face = (Poly, Option<(i64, i64)>);

impl<'a> Context<'a> {
    fn new(shape: [usize; 3], functions: &'a [Vec<f64>]) -> Self {
        let nodes = shape[0] * shape[1] * shape[2];
        Self {
            sx: shape[1] * shape[2],
            sy: shape[2],
            nodes: nodes as i64,
            functions,
            count: functions.len() as i64,
            face_keys: Vec::new(),
            face_index: HashMap::new(),
            weights_cache: HashMap::new(),
            values_cache: HashMap::new(),
            cross_cache: HashMap::new(),
        }
    }

    fn edge_code(&self, a: i64, b: i64, g: i64) -> Result<i64, PartitionError> {
        let (a, b) = if a > b { (b, a) } else { (a, b) };
        let diff = b - a;
        let (sx, sy) = (self.sx as i64, self.sy as i64);
        let dx = i64::from(diff >= sx);
        let rest = diff - dx * sx;
        let dy = i64::from(rest >= sy);
        let dz = rest - dy * sy;
        if !(dz == 0 || dz == 1) || diff <= 0 {
            return Err(invalid("internal: edge between non-adjacent nodes"));
        }
        let d = dx | (dy << 1) | (dz << 2);
        Ok(self.nodes + ((a * 8 + d) * self.count + g))
    }

    fn other(&self, a: i64, d: i64) -> i64 {
        a + (d & 1) * self.sx as i64 + ((d >> 1) & 1) * self.sy as i64 + ((d >> 2) & 1)
    }

    fn decode(&self, code: i64) -> (Vec<i64>, Vec<i64>) {
        if code < self.nodes {
            return (vec![code], vec![]);
        }
        if code < FACE_BASE {
            let e = code - self.nodes;
            let g = e % self.count;
            let ad = e / self.count;
            let a = ad / 8;
            return (vec![a, self.other(a, ad % 8)], vec![g]);
        }
        let k = self.face_keys[(code - FACE_BASE) as usize];
        (vec![k[0], k[1], k[2]], vec![k[3], k[4]])
    }

    fn face_code(&mut self, support: &[i64], functions: &[i64]) -> i64 {
        let mut s = support.to_vec();
        s.sort_unstable();
        let mut f = functions.to_vec();
        f.sort_unstable();
        let key = [s[0], s[1], s[2], f[0], f[1]];
        let index = if let Some(&i) = self.face_index.get(&key) {
            i
        } else {
            let i = self.face_keys.len();
            self.face_keys.push(key);
            self.face_index.insert(key, i);
            i
        };
        FACE_BASE + index as i64
    }

    fn g(&self, g: i64, node: i64) -> f64 {
        self.functions[g as usize][node as usize]
    }

    fn weights(&mut self, code: i64) -> (Vec<i64>, Vec<f64>) {
        if let Some(c) = self.weights_cache.get(&code) {
            return c.clone();
        }
        let (support, functions) = self.decode(code);
        let result = match support.len() {
            1 => (support, vec![1.0]),
            2 => {
                let (a, b) = (support[0], support[1]);
                let (ga, gb) = (self.g(functions[0], a), self.g(functions[0], b));
                #[allow(clippy::float_cmp)]
                let s = if ga == gb { 0.5 } else { ga / (ga - gb) };
                let s = s.clamp(0.0, 1.0);
                (support, vec![1.0 - s, s])
            }
            _ => {
                let u: Vec<f64> = support.iter().map(|&n| self.g(functions[0], n)).collect();
                let v: Vec<f64> = support.iter().map(|&n| self.g(functions[1], n)).collect();
                (support, face_weights(&u, &v).to_vec())
            }
        };
        self.weights_cache.insert(code, result.clone());
        result
    }

    fn value(&mut self, code: i64, g: i64) -> f64 {
        if let Some(&v) = self.values_cache.get(&(code, g)) {
            return v;
        }
        let (_support, functions) = self.decode(code);
        let value = if functions.contains(&g) {
            0.0
        } else {
            let (nodes, lam) = self.weights(code);
            let mut v = 0.0;
            for (n, w) in nodes.iter().zip(&lam) {
                v += w * self.g(g, *n);
            }
            v
        };
        self.values_cache.insert((code, g), value);
        value
    }

    fn negative(&mut self, code: i64, g: i64) -> bool {
        self.value(code, g) < 0.0
    }

    fn crossing(&mut self, u: i64, v: i64, g: i64) -> Result<i64, PartitionError> {
        let key = if u < v { (u, v, g) } else { (v, u, g) };
        if let Some(&c) = self.cross_cache.get(&key) {
            return Ok(c);
        }
        let (su, fu) = self.decode(u);
        let (sv, fv) = self.decode(v);
        let mut support: Vec<i64> = su.iter().chain(sv.iter()).copied().collect();
        support.sort_unstable();
        support.dedup();
        let mut functions: Vec<i64> = fu.iter().filter(|x| fv.contains(x)).copied().collect();
        if !functions.contains(&g) {
            functions.push(g);
        }
        functions.sort_unstable();
        functions.dedup();
        let code = if support.len() == 2 && functions.len() == 1 {
            self.edge_code(support[0], support[1], g)?
        } else if support.len() == 3 && functions.len() == 2 {
            self.face_code(&support, &functions)
        } else {
            return Err(invalid(
                "degenerate partition: two sampled level sets coincide on a tetrahedron edge; perturb the \
                 thresholds or change the export resolution",
            ));
        };
        self.cross_cache.insert(key, code);
        Ok(code)
    }
}

fn face_weights(u: &[f64], v: &[f64]) -> [f64; 3] {
    let c0 = u[1] * v[2] - u[2] * v[1];
    let c1 = u[2] * v[0] - u[0] * v[2];
    let c2 = u[0] * v[1] - u[1] * v[0];
    let total = c0 + c1 + c2;
    let third = 1.0 / 3.0;
    if total == 0.0 || !total.is_finite() {
        return [third; 3];
    }
    let lam = [(c0 / total).max(0.0), (c1 / total).max(0.0), (c2 / total).max(0.0)];
    let norm = lam[0] + lam[1] + lam[2];
    if norm <= 0.0 {
        return [third; 3];
    }
    [lam[0] / norm, lam[1] / norm, lam[2] / norm]
}

#[allow(clippy::type_complexity)]
fn split_polygon(
    ctx: &mut Context<'_>,
    verts: &[i64],
    g: i64,
) -> Result<(Option<Poly>, Option<Poly>, Option<(i64, i64)>), PartitionError> {
    let neg: Vec<bool> = verts.iter().map(|&v| ctx.negative(v, g)).collect();
    if neg.iter().all(|&b| b) {
        return Ok((Some(verts.to_vec()), None, None));
    }
    if !neg.iter().any(|&b| b) {
        return Ok((None, Some(verts.to_vec()), None));
    }
    let (mut negative, mut positive) = (Vec::new(), Vec::new());
    let (mut exit, mut entry) = (None, None);
    let n = verts.len();
    for i in 0..n {
        let (u, w) = (verts[i], verts[(i + 1) % n]);
        let (nu, nw) = (neg[i], neg[(i + 1) % n]);
        if nu {
            negative.push(u);
        } else {
            positive.push(u);
        }
        if nu != nw {
            let x = ctx.crossing(u, w, g)?;
            negative.push(x);
            positive.push(x);
            if nu {
                exit = Some(x);
            } else {
                entry = Some(x);
            }
        }
    }
    let cut = match (exit, entry) {
        (Some(e), Some(n)) => Some((e, n)),
        _ => None,
    };
    Ok((Some(negative), Some(positive), cut))
}

#[allow(clippy::type_complexity)]
fn split_polytope(
    ctx: &mut Context<'_>,
    faces: &[Face],
    g: i64,
) -> Result<(Option<Vec<Face>>, Option<Vec<Face>>), PartitionError> {
    let (mut negative, mut positive) = (Vec::new(), Vec::new());

    let mut cap: Vec<(i64, i64)> = Vec::new();
    for (verts, tag) in faces {
        let (a, b, cut) = split_polygon(ctx, verts, g)?;
        if let Some(a) = a {
            negative.push((a, *tag));
        }
        if let Some(b) = b {
            positive.push((b, *tag));
        }
        if let Some((exit, entry)) = cut {
            if cap.iter().any(|(k, _)| *k == entry) {
                return Err(invalid("internal: non-convex polytope split"));
            }
            cap.push((entry, exit));
        }
    }
    if negative.is_empty() {
        return Ok((None, Some(positive)));
    }
    if positive.is_empty() {
        return Ok((Some(negative), None));
    }
    let lookup = |k: i64| cap.iter().find(|(e, _)| *e == k).map(|(_, x)| *x);
    let start = cap.first().map(|(k, _)| *k).ok_or_else(|| invalid("internal: open polytope cut"))?;
    let mut cycle = vec![start];
    loop {
        let last = *cycle.last().unwrap_or(&start);
        let nxt = lookup(last).ok_or_else(|| invalid("internal: open polytope cut"))?;
        if nxt == start {
            break;
        }
        cycle.push(nxt);
        if cycle.len() > cap.len() {
            return Err(invalid("internal: polytope cut is not one cycle"));
        }
    }
    if cycle.len() != cap.len() {
        return Err(invalid("internal: polytope cut is not one cycle"));
    }
    let mut reversed = cycle.clone();
    reversed.reverse();
    negative.push((cycle, Some((g, -1))));
    positive.push((reversed, Some((g, 1))));
    Ok((Some(negative), Some(positive)))
}

fn changes(values: &[f64]) -> bool {
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    lo < 0.0 && 0.0 <= hi
}

fn fan(verts: &[i64], nodes: i64) -> Vec<[i64; 3]> {
    let n = verts.len();
    let apex = verts.iter().position(|&v| v < nodes).unwrap_or(0);
    let order: Vec<i64> = verts[apex..].iter().chain(verts[..apex].iter()).copied().collect();
    (1..n.saturating_sub(1)).map(|i| [order[0], order[i], order[i + 1]]).collect()
}

#[derive(Clone, Debug)]
pub struct PartitionMesh {
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[usize; 3]>,
    pub inner: Vec<i64>,
    pub outer: Vec<i64>,
    pub regions: usize,
    pub stats: Value,
}

pub type WeldedSurface = (Vec<[f32; 3]>, Vec<[usize; 3]>, usize);

impl PartitionMesh {


    pub fn surface(&self, region_ids: &[i64]) -> Result<WeldedSurface, PartitionError> {
        let mut member = vec![false; self.regions + 2];
        for &rid in region_ids {
            if rid < 0 || rid as usize >= self.regions {
                return Err(PartitionError::Invalid(format!("unknown region id {rid}")));
            }
            member[rid as usize + 1] = true;
        }
        let mut tri = Vec::new();
        for (i, t) in self.triangles.iter().enumerate() {
            let inside = member[(self.inner[i] + 1) as usize];
            let outside = member[(self.outer[i] + 1) as usize];
            if inside != outside {
                tri.push(if outside { [t[0], t[2], t[1]] } else { *t });
            }
        }
        Ok(weld_float32(&self.vertices, &tri))
    }
}

fn f32_key(v: [f32; 3]) -> [u8; 12] {
    let mut k = [0u8; 12];
    for (i, c) in v.iter().enumerate() {
        k[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
    }
    k
}

#[must_use]
pub fn weld_float32(vertices: &[[f64; 3]], triangles: &[[usize; 3]]) -> WeldedSurface {
    if triangles.is_empty() {
        return (Vec::new(), Vec::new(), 0);
    }
    let mut used: Vec<usize> = triangles.iter().flatten().copied().collect();
    used.sort_unstable();
    used.dedup();
    let pos_in_used = |v: usize| used.binary_search(&v).unwrap_or(0);
    let v32: Vec<[f32; 3]> = used.iter().map(|&i| vertices[i].map(|c| c as f32)).collect();
    let keys: Vec<[u8; 12]> = v32.iter().map(|v| f32_key(*v)).collect();
    let mut order: Vec<usize> = (0..v32.len()).collect();
    order.sort_by(|&a, &b| keys[a].cmp(&keys[b]).then(a.cmp(&b)));
    let mut weld = vec![0usize; v32.len()];
    let mut compact: Vec<[f32; 3]> = Vec::new();
    let mut last: Option<[u8; 12]> = None;
    for &i in &order {
        if last != Some(keys[i]) {
            compact.push(v32[i]);
            last = Some(keys[i]);
        }
        weld[i] = compact.len() - 1;
    }
    let mut tri: Vec<[usize; 3]> = Vec::with_capacity(triangles.len());
    let mut dropped = 0;
    for t in triangles {
        let w = t.map(|v| weld[pos_in_used(v)]);
        if w[0] != w[1] && w[1] != w[2] && w[2] != w[0] {
            tri.push(w);
        } else {
            dropped += 1;
        }
    }
    let mut used2: Vec<usize> = tri.iter().flatten().copied().collect();
    used2.sort_unstable();
    used2.dedup();
    let verts = used2.iter().map(|&i| compact[i]).collect();
    let tris = tri.iter().map(|t| t.map(|v| used2.binary_search(&v).unwrap_or(0))).collect();
    (verts, tris, dropped)
}

fn deadband(values: &[f64], shape: [usize; 3]) -> (Vec<f64>, Value) {
    let mut g = values.to_vec();
    let strides = [shape[1] * shape[2], shape[2], 1];
    let mut steps = Vec::new();
    for axis in 0..3 {
        let s = strides[axis];
        for (idx, &lo) in g.iter().enumerate() {
            if (idx / s) % shape[axis] == shape[axis] - 1 {
                continue;
            }
            let hi = g[idx + s];
            if (lo < 0.0) != (hi < 0.0) {
                steps.push((hi - lo).abs());
            }
        }
    }
    if steps.is_empty() {
        return (g, json!({"eps": 0.0, "nodes_moved": 0}));
    }
    let eps = DEADBAND_FRAC * median(&steps);
    let mut moved = 0usize;
    for v in &mut g {
        if v.abs() < eps {
            *v = if *v < 0.0 { -eps } else { eps };
            moved += 1;
        }
    }
    (g, json!({"eps": eps, "nodes_moved": moved}))
}




#[allow(clippy::too_many_lines)]
pub fn mesh_partition(
    axes: &[Vec<f64>; 3],
    solid: &[f64],
    phase: Option<&[f64]>,
    thresholds: &[f64],
) -> Result<PartitionMesh, PartitionError> {
    if axes.iter().any(|a| a.len() < 3 || a.windows(2).any(|p| p[1] - p[0] <= 0.0)) {
        return Err(invalid("axes must be three increasing arrays of >= 3 nodes"));
    }
    let shape = [axes[0].len(), axes[1].len(), axes[2].len()];
    let nodes = shape[0] * shape[1] * shape[2];
    if nodes > MAX_NODES {
        return Err(PartitionError::Invalid(format!("the partition grid exceeds {MAX_NODES} nodes")));
    }
    if solid.len() != nodes || solid.iter().any(|v| !v.is_finite()) {
        return Err(invalid("solid samples must be finite on the grid"));
    }
    if thresholds.iter().any(|t| !t.is_finite())
        || thresholds.windows(2).any(|p| p[1] - p[0] <= 1e-9 * 1f64.max(p[0].abs()).max(p[1].abs()))
    {
        return Err(invalid("thresholds must be finite and strictly increasing"));
    }
    let phase = if thresholds.is_empty() {
        None
    } else {
        match phase {
            Some(p) if p.len() == nodes && p.iter().all(|v| v.is_finite()) => Some(p),
            _ => return Err(invalid("phase samples must be finite on the grid")),
        }
    };
    let mut raw: Vec<Vec<f64>> = vec![solid.to_vec()];
    if let Some(p) = phase {
        for t in thresholds {
            raw.push(p.iter().map(|v| v - t).collect());
        }
    }
    let mut functions = Vec::with_capacity(raw.len());
    let mut deadbands = Vec::with_capacity(raw.len());
    for values in &raw {
        let (pushed, record) = deadband(values, shape);
        functions.push(pushed);
        deadbands.push(record);
    }
    drop(raw);
    let tables = tables();
    let mut ctx = Context::new(shape, &functions);
    let count = functions.len() as i64;
    let regions = thresholds.len() + 2;
    let negative: Vec<bool> = functions[0].iter().map(|&v| v < 0.0).collect();
    let mut category = vec![0i64; nodes];
    for h in &functions[1..] {
        for (c, &v) in category.iter_mut().zip(h) {
            *c += i64::from(v >= 0.0);
        }
    }
    let region: Vec<i64> = (0..nodes).map(|i| if negative[i] { 1 + category[i] } else { 0 }).collect();
    let (sx, sy) = (ctx.sx, ctx.sy);
    let offsets: [usize; 8] =
        std::array::from_fn(|c| CORNERS[c][0] * sx + CORNERS[c][1] * sy + CORNERS[c][2]);
    let (nx, ny, nz) = (shape[0], shape[1], shape[2]);

    let mut cells: Vec<usize> = Vec::new();
    for i in 0..nx - 1 {
        for j in 0..ny - 1 {
            for k in 0..nz - 1 {
                let base = i * sx + j * sy + k;
                let mut any_neg = false;
                let mut all_neg = true;
                let (mut cmin, mut cmax) = (i64::MAX, i64::MIN);
                for &o in &offsets {
                    let n = base + o;
                    any_neg |= negative[n];
                    all_neg &= negative[n];
                    cmin = cmin.min(category[n]);
                    cmax = cmax.max(category[n]);
                }
                if (any_neg && !all_neg) || (all_neg && cmin != cmax) {
                    cells.push(base);
                }
            }
        }
    }
    let mut stats = serde_json::Map::new();
    stats.insert("active_cells".into(), json!(cells.len()));

    let mut codes: Vec<[i64; 3]> = Vec::new();
    let mut inner: Vec<i64> = Vec::new();
    let mut outer: Vec<i64> = Vec::new();
    let mut python: Vec<([i64; 3], i64, i64)> = Vec::new();
    let nodes_i = nodes as i64;

    let march = |t: usize,
                 batch: &[([usize; 4], usize, i64, i64)],
                 g: i64,
                 codes: &mut Vec<[i64; 3]>,
                 inner: &mut Vec<i64>,
                 outer: &mut Vec<i64>| {
        for case in 1..15 {
            let sel: Vec<&([usize; 4], usize, i64, i64)> = batch.iter().filter(|b| b.1 == case).collect();
            if sel.is_empty() {
                continue;
            }
            for poly in &tables.march[t][case] {
                let row = |nd: &[usize; 4], k: usize| {
                    let (lo, d) = poly[k];
                    nodes_i + ((nd[lo] as i64 * 8 + d as i64) * count + g)
                };
                for b in &sel {
                    codes.push([row(&b.0, 0), row(&b.0, 1), row(&b.0, 2)]);
                    inner.push(b.2);
                    outer.push(b.3);
                }
                if poly.len() == 4 {
                    for b in &sel {
                        codes.push([row(&b.0, 0), row(&b.0, 2), row(&b.0, 3)]);
                        inner.push(b.2);
                        outer.push(b.3);
                    }
                }
            }
        }
    };

    let mut mixed: Vec<(usize, [usize; 4])> = Vec::new();
    for (t, chain) in TETS.iter().enumerate() {
        let mut pure_f = Vec::new();
        let mut pure_h: Vec<[usize; 4]> = Vec::new();
        for &base in &cells {
            let nd: [usize; 4] = std::array::from_fn(|q| base + offsets[chain[q]]);
            let tneg: [bool; 4] = nd.map(|n| negative[n]);
            let tcat: [i64; 4] = nd.map(|n| category[n]);
            let f_change = tneg.iter().any(|&b| b) && !tneg.iter().all(|&b| b);
            let c_change = tcat.iter().min() != tcat.iter().max();
            if f_change && !c_change {
                let case = tneg.iter().enumerate().map(|(q, &b)| usize::from(b) << q).sum();
                pure_f.push((nd, case, 1 + tcat[0], 0));
            }
            if tneg.iter().all(|&b| b) && c_change {
                pure_h.push(nd);
            }
            if f_change && c_change {
                mixed.push((t, nd));
            }
        }
        march(t, &pure_f, 0, &mut codes, &mut inner, &mut outer);
        if !pure_h.is_empty() {
            for (i, function) in functions.iter().enumerate().skip(1) {
                let batch: Vec<([usize; 4], usize, i64, i64)> = pure_h
                    .iter()
                    .filter_map(|nd| {
                        let hneg: [bool; 4] = nd.map(|n| function[n] < 0.0);
                        let crossing = hneg.iter().any(|&b| b) && !hneg.iter().all(|&b| b);
                        crossing.then(|| {
                            let case = hneg.iter().enumerate().map(|(q, &b)| usize::from(b) << q).sum();
                            (*nd, case, i as i64, i as i64 + 1)
                        })
                    })
                    .collect();
                if !batch.is_empty() {
                    march(t, &batch, i as i64, &mut codes, &mut inner, &mut outer);
                }
            }
        }
    }
    stats.insert("mixed_tetrahedra".into(), json!(mixed.len()));
    for (t, nd) in &mixed {
        mixed_tetrahedron(&mut ctx, &tables, *t, nd, &mut python)?;
    }

    let mut mixed_box = 0usize;
    for axis in 0..3 {
        for value in 0..2 {
            let index = if value == 0 { 0 } else { shape[axis] - 2 };
            let on = cells_on_side(shape, axis, index);
            let side_base: Vec<usize> = on.iter().map(|c| c[0] * sx + c[1] * sy + c[2]).collect();
            for (t, chain) in TETS.iter().enumerate() {
                for f in 0..4 {
                    if tables.sides[t][f] != Some((axis, value)) {
                        continue;
                    }
                    let local = tables.faces[t][f];
                    let mut nonuniform = Vec::new();
                    for &b in &side_base {
                        let tri: [usize; 3] = local.map(|j| b + offsets[chain[j]]);
                        let reg = tri.map(|n| region[n]);
                        if reg[0] == reg[1] && reg[1] == reg[2] {
                            codes.push(tri.map(|n| n as i64));
                            inner.push(reg[0]);
                            outer.push(-1);
                        } else {
                            nonuniform.push(tri);
                        }
                    }
                    for tri in nonuniform {
                        mixed_box += 1;
                        mixed_box_triangle(&mut ctx, &tri.map(|n| n as i64), &mut python)?;
                    }
                }
            }
        }
    }
    stats.insert("mixed_box_triangles".into(), json!(mixed_box));
    stats.insert("deadband".into(), Value::Array(deadbands));
    for (c, i, o) in python {
        codes.push(c);
        inner.push(i);
        outer.push(o);
    }
    let mut unique: Vec<i64> = codes.iter().flatten().copied().collect();
    unique.sort_unstable();
    unique.dedup();
    let vertices = positions(&ctx, axes, &functions, &unique);
    let triangles: Vec<[usize; 3]> =
        codes.iter().map(|c| c.map(|v| unique.binary_search(&v).unwrap_or(0))).collect();
    stats.insert("interface_triangles".into(), json!(codes.len()));
    stats.insert("symbolic_vertices".into(), json!(unique.len()));
    stats.insert("face_vertices".into(), json!(ctx.face_keys.len()));
    Ok(PartitionMesh { vertices, triangles, inner, outer, regions, stats: Value::Object(stats) })
}

fn cells_on_side(shape: [usize; 3], axis: usize, index: usize) -> Vec<[usize; 3]> {
    let ranges: [Vec<usize>; 3] =
        std::array::from_fn(|a| if a == axis { vec![index] } else { (0..shape[a] - 1).collect() });
    let mut out = Vec::with_capacity(ranges[0].len() * ranges[1].len() * ranges[2].len());
    for &i in &ranges[0] {
        for &j in &ranges[1] {
            for &k in &ranges[2] {
                out.push([i, j, k]);
            }
        }
    }
    out
}

fn mixed_tetrahedron(
    ctx: &mut Context<'_>,
    tables: &Tables,
    t: usize,
    tet_nodes: &[usize; 4],
    out: &mut Vec<([i64; 3], i64, i64)>,
) -> Result<(), PartitionError> {
    let faces: Vec<Face> = tables.faces[t]
        .iter()
        .map(|local| (local.iter().map(|&j| tet_nodes[j] as i64).collect(), None))
        .collect();
    let (solid, _complement) = split_polytope(ctx, &faces, 0)?;
    let Some(solid) = solid else { return Ok(()) };
    let mut cells: Vec<(Vec<Face>, i64)> = vec![(solid, 0)];
    for g in 1..ctx.count {
        let values: Vec<f64> = tet_nodes.iter().map(|&n| ctx.g(g, n as i64)).collect();
        if !changes(&values) {
            if values.iter().copied().fold(f64::INFINITY, f64::min) >= 0.0 {
                cells = cells.into_iter().map(|(c, above)| (c, above + 1)).collect();
            }
            continue;
        }
        let mut split = Vec::new();
        for (cell, above) in cells {
            let (neg, pos) = split_polytope(ctx, &cell, g)?;
            if let Some(neg) = neg {
                split.push((neg, above));
            }
            if let Some(pos) = pos {
                split.push((pos, above + 1));
            }
        }
        cells = split;
    }
    for (cell, above) in cells {
        for (verts, tag) in cell {
            let Some((g, side)) = tag else { continue };
            if side != -1 {
                continue;
            }
            let inner = 1 + above;
            let outer = if g == 0 { 0 } else { inner + 1 };
            for tri in fan(&verts, ctx.nodes) {
                out.push((tri, inner, outer));
            }
        }
    }
    Ok(())
}

fn mixed_box_triangle(
    ctx: &mut Context<'_>,
    tri_nodes: &[i64; 3],
    out: &mut Vec<([i64; 3], i64, i64)>,
) -> Result<(), PartitionError> {
    let (negative, positive, cut) = split_polygon(ctx, tri_nodes, 0)?;
    let mut pieces: Vec<(Poly, i64)> = Vec::new();
    if let Some(negative) = &negative {
        let mut parts: Vec<(Poly, i64)> = vec![(negative.clone(), 0)];
        for g in 1..ctx.count {
            let values: Vec<f64> = tri_nodes.iter().map(|&n| ctx.g(g, n)).collect();
            if !changes(&values) {
                if values.iter().copied().fold(f64::INFINITY, f64::min) >= 0.0 {
                    parts = parts.into_iter().map(|(p, above)| (p, above + 1)).collect();
                }
                continue;
            }
            let mut split = Vec::new();
            for (part, above) in parts {
                let (a, b, _cut) = split_polygon(ctx, &part, g)?;
                if let Some(a) = a {
                    split.push((a, above));
                }
                if let Some(b) = b {
                    split.push((b, above + 1));
                }
            }
            parts = split;
        }
        pieces.extend(parts.into_iter().map(|(p, above)| (p, 1 + above)));
    }
    if let Some(mut positive) = positive {
        if negative.is_some() && pieces.len() > 1 {
            let cut = cut.ok_or_else(|| invalid("internal: complement part lacks the solid cut"))?;
            positive = with_cut_chain(ctx, positive, &pieces, cut)?;
        }
        pieces.push((positive, 0));
    }
    for (verts, region) in pieces {
        for tri in fan(&verts, ctx.nodes) {
            out.push((tri, region, -1));
        }
    }
    Ok(())
}

fn with_cut_chain(
    ctx: &Context<'_>,
    positive: Poly,
    pieces: &[(Poly, i64)],
    cut: (i64, i64),
) -> Result<Poly, PartitionError> {
    let (exit, entry) = cut;
    let mut step: HashMap<i64, i64> = HashMap::new();
    for (verts, _region) in pieces {
        let n = verts.len();
        for i in 0..n {
            let (u, w) = (verts[i], verts[(i + 1) % n]);
            if ctx.decode(u).1.contains(&0) && ctx.decode(w).1.contains(&0) {
                step.insert(u, w);
            }
        }
    }
    let mut chain = vec![exit];
    while *chain.last().unwrap_or(&entry) != entry {
        let nxt = step.get(chain.last().unwrap_or(&entry)).copied();
        match nxt {
            Some(n) if chain.len() <= step.len() + 1 => chain.push(n),
            _ => return Err(invalid("internal: broken solid-boundary chain")),
        }
    }
    let interior: Vec<i64> = chain[1..chain.len() - 1].to_vec();
    if interior.is_empty() {
        return Ok(positive);
    }
    let n = positive.len();
    for i in 0..n {
        if positive[i] == entry && positive[(i + 1) % n] == exit {
            let mut out: Vec<i64> = positive[..=i].to_vec();
            out.extend(interior.iter().rev());
            out.extend_from_slice(&positive[i + 1..]);
            return Ok(out);
        }
    }
    Err(invalid("internal: complement part lacks the solid cut"))
}

fn positions(
    ctx: &Context<'_>,
    axes: &[Vec<f64>; 3],
    functions: &[Vec<f64>],
    codes: &[i64],
) -> Vec<[f64; 3]> {
    let (sx, sy) = (ctx.sx as i64, ctx.sy as i64);
    let node_xyz = |id: i64| {
        let i = id / sx;
        let j = (id % sx) / sy;
        let k = id % sy;
        [axes[0][i as usize], axes[1][j as usize], axes[2][k as usize]]
    };
    codes
        .iter()
        .map(|&code| {
            if code < ctx.nodes {
                node_xyz(code)
            } else if code < FACE_BASE {
                let e = code - ctx.nodes;
                let g = (e % ctx.count) as usize;
                let ad = e / ctx.count;
                let a = ad / 8;
                let d = ad % 8;
                let b = a + (d & 1) * sx + ((d >> 1) & 1) * sy + ((d >> 2) & 1);
                let (ga, gb) = (functions[g][a as usize], functions[g][b as usize]);
                let den = ga - gb;
                let s = if den == 0.0 { 0.5 } else { ga / den };
                let s = s.clamp(0.0, 1.0);
                let (xa, xb) = (node_xyz(a), node_xyz(b));
                std::array::from_fn(|q| xa[q] + s * (xb[q] - xa[q]))
            } else {
                let k = ctx.face_keys[(code - FACE_BASE) as usize];
                let (a, b, c, g1, g2) = (k[0], k[1], k[2], k[3] as usize, k[4] as usize);
                let u = [functions[g1][a as usize], functions[g1][b as usize], functions[g1][c as usize]];
                let v = [functions[g2][a as usize], functions[g2][b as usize], functions[g2][c as usize]];
                let mut lam =
                    [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let total = lam[0] + lam[1] + lam[2];
                let third = 1.0 / 3.0;
                if total == 0.0 || !total.is_finite() {
                    lam = [third; 3];
                } else {
                    lam = lam.map(|l| l / total);
                }
                lam = lam.map(|l| l.max(0.0));
                let norm = lam[0] + lam[1] + lam[2];
                lam = if norm > 0.0 { lam.map(|l| l / norm) } else { [third; 3] };
                let (pa, pb, pc) = (node_xyz(a), node_xyz(b), node_xyz(c));
                std::array::from_fn(|q| lam[0] * pa[q] + lam[1] * pb[q] + lam[2] * pc[q])
            }
        })
        .collect()
}

fn canonical_triangles(tris: &[[[f32; 3]; 3]]) -> (Vec<[usize; 3]>, Vec<bool>) {
    let keys: Vec<[u8; 12]> = tris.iter().flatten().map(|v| f32_key(*v)).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    sorted.dedup();
    let ids: Vec<usize> = keys.iter().map(|k| sorted.binary_search(k).unwrap_or(0)).collect();
    let mut out = Vec::with_capacity(tris.len());
    let mut parity = Vec::with_capacity(tris.len());
    for t in ids.chunks_exact(3) {
        let first = (0..3).min_by_key(|&q| (t[q], q)).unwrap_or(0);
        let a = t[first];
        let b = t[(first + 1) % 3];
        let c = t[(first + 2) % 3];
        parity.push(b < c);
        out.push([a, b.min(c), b.max(c)]);
    }
    (out, parity)
}

#[must_use]
pub fn superposition_check(parts: &[Vec<[[f32; 3]; 3]>], whole: Option<&[[[f32; 3]; 3]]>) -> Value {
    let counts: Vec<usize> = parts.iter().map(Vec::len).collect();
    let mut joined: Vec<[[f32; 3]; 3]> = parts.iter().flatten().copied().collect();
    let n_parts = joined.len();
    let mut owner: Vec<usize> =
        parts.iter().enumerate().flat_map(|(i, p)| std::iter::repeat_n(i, p.len())).collect();
    if let Some(w) = whole {
        joined.extend_from_slice(w);
        owner.extend(std::iter::repeat_n(parts.len(), w.len()));
    }
    let (keys, parity) = canonical_triangles(&joined);
    let (pk, pp) = (&keys[..n_parts], &parity[..n_parts]);

    let mut groups: std::collections::BTreeMap<[usize; 3], Vec<usize>> = std::collections::BTreeMap::new();
    for (i, k) in pk.iter().enumerate() {
        groups.entry(*k).or_default().push(i);
    }
    let (mut paired, mut single, mut bad, mut same_part_pairs) = (0usize, 0usize, 0usize, 0usize);
    let mut remainder: Vec<[usize; 4]> = Vec::new();
    for members in groups.values() {
        let signed: i64 = members.iter().map(|&i| if pp[i] { 1 } else { -1 }).sum();
        if members.len() == 2 && signed == 0 {
            paired += 1;
            if owner[members[0]] == owner[members[1]] {
                same_part_pairs += 1;
            }
        } else if members.len() == 1 {
            single += 1;
            let i = members[0];
            remainder.push([pk[i][0], pk[i][1], pk[i][2], usize::from(pp[i])]);
        } else {
            bad += 1;
        }
    }
    let mut result = serde_json::Map::new();
    result.insert("part_triangles".into(), json!(counts));
    result.insert("shared_interface_triangles".into(), json!(paired));
    result.insert("unpaired_triangles".into(), json!(single));
    result.insert("conflicting_triangles".into(), json!(bad));
    result.insert("interfaces_within_one_part".into(), json!(same_part_pairs));
    let mut ok = bad == 0 && same_part_pairs == 0;
    if let Some(w) = whole {
        let mut wk: Vec<[usize; 4]> = (n_parts..n_parts + w.len())
            .map(|i| [keys[i][0], keys[i][1], keys[i][2], usize::from(parity[i])])
            .collect();
        let whole_len = wk.len();
        wk.sort_unstable();
        wk.dedup();
        remainder.sort_unstable();
        let rlen = remainder.len();
        remainder.dedup();
        let equal = rlen == remainder.len()
            && remainder.len() == wk.len()
            && wk.len() == whole_len
            && remainder == wk;
        result.insert("whole_triangles".into(), json!(whole_len));
        result.insert("remainder_equals_whole".into(), json!(equal));
        ok = ok && equal;
    }
    result.insert("passed".into(), json!(ok));
    Value::Object(result)
}

