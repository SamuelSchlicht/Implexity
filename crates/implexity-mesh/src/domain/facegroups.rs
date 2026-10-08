// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::meshgen::blas_norm3;
use crate::MeshError;
use crate::bodyexport::SdfKernel;
use crate::numeric::{pairwise_sum, py_round_digits};
use crate::topology::{Tri, Vec3, cross};

pub const DEFAULT_ANGLE_DEG: f64 = 25.0;
pub const PLANAR_TOL_DEG: f64 = 1.0;
pub const FACET_AREA_FRAC: f64 = 0.02;
pub const MIN_AREA_FRAC: f64 = 0.002;

#[must_use]
pub fn tri_normals(v: &[Vec3], f: &[Tri]) -> (Vec<Vec3>, Vec<f64>) {
    let mut n = Vec::with_capacity(f.len());
    let mut a = Vec::with_capacity(f.len());
    for t in f {
        let (p, q, r) = (v[t[0]], v[t[1]], v[t[2]]);
        let c = cross([q[0] - p[0], q[1] - p[1], q[2] - p[2]], [r[0] - p[0], r[1] - p[1], r[2] - p[2]]);
        let twice = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        let d = twice.max(1e-300);
        n.push([c[0] / d, c[1] / d, c[2] / d]);
        a.push(0.5 * twice);
    }
    (n, a)
}

#[must_use]
pub fn edge_adjacency(f: &[Tri]) -> Vec<[usize; 2]> {
    let m = f.len();
    let mut e: Vec<([usize; 2], usize, usize)> = Vec::with_capacity(3 * m);
    for (s, (a, b)) in [(0usize, 1usize), (1, 2), (2, 0)].into_iter().enumerate() {
        for (i, t) in f.iter().enumerate() {
            let (x, y) = (t[a], t[b]);
            e.push(([x.min(y), x.max(y)], s * m + i, i));
        }
    }
    e.sort_by(|p, q| p.0.cmp(&q.0).then(p.1.cmp(&q.1)));
    e.windows(2).filter(|w| w[0].0 == w[1].0).map(|w| [w[0].2, w[1].2]).collect()
}

struct Uf {
    p: Vec<usize>,
}

impl Uf {
    fn find(&mut self, mut i: usize) -> usize {
        while self.p[i] != i {
            self.p[i] = self.p[self.p[i]];
            i = self.p[i];
        }
        i
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.p[rb] = ra;
        }
    }
}

fn einsum_dot(a: Vec3, b: Vec3) -> f64 {
    (a[0] * b[0] + a[2] * b[2]) + a[1] * b[1]
}

fn blas_dot(a: Vec3, b: Vec3) -> f64 {
    a[2].mul_add(b[2], a[1].mul_add(b[1], a[0] * b[0]))
}

fn weighted_normal(normals: &[Vec3], areas: &[f64], members: impl Iterator<Item = usize>) -> Vec3 {
    let mut s = [0.0; 3];
    for i in members {
        for k in 0..3 {
            s[k] += normals[i][k] * areas[i];
        }
    }
    let l = blas_norm3(s).max(1e-300);
    s.map(|x| x / l)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupOptions {
    pub angle_deg: f64,
    pub min_area_frac: f64,
    pub planar_tol_deg: f64,
    pub facet_area_frac: f64,
}

impl Default for GroupOptions {
    fn default() -> Self {
        Self {
            angle_deg: DEFAULT_ANGLE_DEG,
            min_area_frac: MIN_AREA_FRAC,
            planar_tol_deg: PLANAR_TOL_DEG,
            facet_area_frac: FACET_AREA_FRAC,
        }
    }
}

fn unique_inverse(root: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let ids: Vec<usize> = root.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let pos: BTreeMap<usize, usize> = ids.iter().enumerate().map(|(k, &v)| (v, k)).collect();
    (ids, root.iter().map(|r| pos[r]).collect())
}

fn bincount(inv: &[usize], w: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    for (i, &k) in inv.iter().enumerate() {
        out[k] += w[i];
    }
    out
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn build_face_groups(v: &[Vec3], f: &[Tri], opts: &GroupOptions) -> (Vec<usize>, Vec<Value>) {
    let m = f.len();
    let (normals, areas) = tri_normals(v, f);
    let pairs = edge_adjacency(f);
    let cosang: Vec<f64> = pairs.iter().map(|p| einsum_dot(normals[p[0]], normals[p[1]])).collect();
    let deg = std::f64::consts::PI / 180.0;
    let mut uf = Uf { p: (0..m).collect() };
    let planar = (opts.planar_tol_deg * deg).cos();
    for (p, &c) in pairs.iter().zip(&cosang) {
        if c >= planar {
            uf.union(p[0], p[1]);
        }
    }
    let patch: Vec<usize> = (0..m).map(|i| uf.find(i)).collect();
    let (pids, pinv) = unique_inverse(&patch);
    let parea = bincount(&pinv, &areas, pids.len());
    let total = pairwise_sum(&areas);
    let facet_like: Vec<bool> = pinv.iter().map(|&k| parea[k] <= opts.facet_area_frac * total).collect();
    let smooth = (opts.angle_deg * deg).cos();
    for (p, &c) in pairs.iter().zip(&cosang) {
        if c >= smooth && facet_like[p[0]] && facet_like[p[1]] {
            uf.union(p[0], p[1]);
        }
    }
    let mut root: Vec<usize> = (0..m).map(|i| uf.find(i)).collect();
    for _ in 0..8 {
        let (ids, inv) = unique_inverse(&root);
        let ga = bincount(&inv, &areas, ids.len());
        let small: Vec<usize> =
            ids.iter().zip(&ga).filter(|(_, a)| **a < opts.min_area_frac * total).map(|(i, _)| *i).collect();
        if small.is_empty() || ids.len() == 1 {
            break;
        }
        let small_set: BTreeSet<usize> = small.iter().copied().collect();
        let mut moved = false;
        for &s in &small {
            let mine: Vec<usize> = (0..m).filter(|&i| root[i] == s).collect();
            let adj: Vec<&[usize; 2]> =
                pairs.iter().filter(|p| (root[p[0]] == s) ^ (root[p[1]] == s)).collect();
            if adj.is_empty() {
                continue;
            }
            let others: BTreeSet<usize> =
                adj.iter().map(|p| if root[p[0]] == s { root[p[1]] } else { root[p[0]] }).collect();
            let mut cand: Vec<usize> = others.iter().copied().filter(|o| !small_set.contains(o)).collect();
            if cand.is_empty() {
                cand = others.into_iter().collect();
            }
            let mn = weighted_normal(&normals, &areas, mine.iter().copied());
            let (mut best, mut bd) = (None, -2.0);
            for o in cand {
                let on = weighted_normal(&normals, &areas, (0..m).filter(|&i| root[i] == o));
                let d = blas_dot(mn, on);
                if d > bd {
                    bd = d;
                    best = Some(o);
                }
            }
            if let Some(b) = best {
                for &i in &mine {
                    root[i] = b;
                }
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    let (ids, inv) = unique_inverse(&root);
    let ga = bincount(&inv, &areas, ids.len());
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by(|&a, &b| (-ga[a]).total_cmp(&(-ga[b])));
    let mut rank = vec![0usize; ids.len()];
    for (r, &k) in order.iter().enumerate() {
        rank[k] = r;
    }
    let labels: Vec<usize> = inv.iter().map(|&k| rank[k]).collect();
    let cent: Vec<Vec3> =
        f.iter().map(|t| std::array::from_fn(|k| (v[t[0]][k] + v[t[1]][k] + v[t[2]][k]) / 3.0)).collect();
    let mut groups = Vec::with_capacity(ids.len());
    for g in 0..ids.len() {
        let members: Vec<usize> = (0..m).filter(|&i| labels[i] == g).collect();
        let nrm = weighted_normal(&normals, &areas, members.iter().copied());
        let mind = members.iter().map(|&i| blas_dot(normals[i], nrm)).fold(f64::INFINITY, f64::min);
        let spread = mind.clamp(-1.0, 1.0).acos() * (180.0 / std::f64::consts::PI);
        let ma: Vec<f64> = members.iter().map(|&i| areas[i]).collect();
        let area = pairwise_sum(&ma);
        let mut c = [0.0; 3];
        for &i in &members {
            for k in 0..3 {
                c[k] += cent[i][k] * areas[i];
            }
        }
        let w = area.max(1e-300);
        groups.push(json!({
            "id": g, "n_tris": members.len(), "area_m2": area, "normal": nrm,
            "normal_spread_deg": py_round_digits(spread, 2), "centroid_m": c.map(|x| x / w),
        }));
    }
    (labels, groups)
}

pub struct DomainView<'a> {
    pub v: &'a [Vec3],
    pub f: &'a [Tri],
    pub sdf: &'a [f64],
    pub hard: &'a [f64],
    pub mask: &'a [f64],
    pub shape: [usize; 3],
    pub origin: Vec3,
    pub h: f64,
}

impl<'a> DomainView<'a> {
    #[must_use]
    pub fn from_fields(d: &'a implexity_geometry::domain_sdf::DomainFields) -> Self {
        Self {
            v: &d.mesh.v,
            f: &d.mesh.f,
            sdf: &d.sdf,
            hard: &d.hard,
            mask: &d.mask,
            shape: d.grid.counts,
            origin: d.grid.origin,
            h: d.grid.h,
        }
    }
}

#[must_use]
pub fn element_centres(shape: [usize; 3], origin: Vec3, h: f64) -> Vec<Vec3> {
    #[allow(clippy::cast_precision_loss)]
    let ax: [Vec<f64>; 3] =
        std::array::from_fn(|a| (0..shape[a]).map(|i| origin[a] + (i as f64 + 0.5) * h).collect());
    let mut out = Vec::with_capacity(shape.iter().product());
    for &x in &ax[0] {
        for &y in &ax[1] {
            for &z in &ax[2] {
                out.push([x, y, z]);
            }
        }
    }
    out
}


pub fn rasterise_groups(
    dom: &DomainView<'_>,
    labels: &[usize],
    band_width: Option<f64>,
    kernel: &dyn SdfKernel,
) -> Result<BTreeMap<usize, Vec<f64>>, MeshError> {
    let band = band_width.filter(|b| *b != 0.0).unwrap_or(dom.h);
    let idx: Vec<usize> =
        (0..dom.sdf.len()).filter(|&i| dom.sdf[i].abs() <= band && dom.hard[i] > 0.0).collect();
    let mut out = BTreeMap::new();
    if idx.is_empty() {
        return Ok(out);
    }
    let centres = element_centres(dom.shape, dom.origin, dom.h);
    let p: Vec<Vec3> = idx.iter().map(|&i| centres[i]).collect();
    let groups: BTreeSet<usize> = labels.iter().copied().collect();
    for g in groups {
        let fg: Vec<Tri> = dom.f.iter().zip(labels).filter(|(_, l)| **l == g).map(|(t, _)| *t).collect();
        let (d, _tri) = kernel.unsigned_distance(dom.v, &fg, &p)?;
        let sel: Vec<usize> = (0..idx.len()).filter(|&k| d[k] <= band * (1.0 + 1e-12)).collect();
        if sel.is_empty() {
            continue;
        }
        let mut m = vec![0.0; dom.sdf.len()];
        for k in sel {
            m[idx[k]] = dom.mask[idx[k]];
        }
        out.insert(g, m);
    }
    Ok(out)
}

#[must_use]
pub fn footprint_area(mask: &[f64], shape: [usize; 3], axis: usize, h: f64) -> f64 {
    let dims: Vec<usize> = (0..3).filter(|&a| a != axis).collect();
    let (n0, n1) = (shape[dims[0]], shape[dims[1]]);
    let mut cols = Vec::with_capacity(n0 * n1);
    for i in 0..n0 {
        for j in 0..n1 {
            let mut best = f64::NEG_INFINITY;
            for k in 0..shape[axis] {
                let mut ijk = [0usize; 3];
                ijk[dims[0]] = i;
                ijk[dims[1]] = j;
                ijk[axis] = k;
                let v = mask[(ijk[0] * shape[1] + ijk[1]) * shape[2] + ijk[2]];
                if v > best || v.is_nan() {
                    best = v;
                }
            }
            cols.push(best);
        }
    }
    pairwise_sum(&cols) * h * h
}
