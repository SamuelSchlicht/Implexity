// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use crate::error::LinalgError;

#[derive(Clone, Debug)]
pub struct KdTree {
    dim: usize,
    points: Vec<f64>,
    order: Vec<usize>,
    nodes: Vec<KdNode>,
}

#[derive(Clone, Debug)]
struct KdNode {
    start: usize,
    end: usize,
    split: Option<(usize, f64, usize, usize)>,
    lo: Vec<f64>,
    hi: Vec<f64>,
}

const LEAF_SIZE: usize = 16;

#[derive(PartialEq)]
struct Cand(f64, usize);
impl Eq for Cand {}
impl PartialOrd for Cand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Cand {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(self.1.cmp(&other.1))
    }
}

impl KdTree {


    pub fn new(points: &[f64], dim: usize) -> Result<Self, LinalgError> {
        if dim == 0 || !points.len().is_multiple_of(dim) {
            return Err(LinalgError::Shape(format!("{} coordinates for dimension {dim}", points.len())));
        }
        if !points.iter().all(|v| v.is_finite()) {
            return Err(LinalgError::NonFinite("k-d tree points must be finite".into()));
        }
        let n = points.len() / dim;
        let mut tree = Self { dim, points: points.to_vec(), order: (0..n).collect(), nodes: Vec::new() };
        if n > 0 {
            tree.build(0, n);
        }
        Ok(tree)
    }

    fn coord(&self, i: usize, a: usize) -> f64 {
        self.points[i * self.dim + a]
    }

    fn build(&mut self, start: usize, end: usize) -> usize {
        let dim = self.dim;
        let mut lo = vec![f64::INFINITY; dim];
        let mut hi = vec![f64::NEG_INFINITY; dim];
        for &i in &self.order[start..end] {
            for a in 0..dim {
                let v = self.points[i * dim + a];
                lo[a] = lo[a].min(v);
                hi[a] = hi[a].max(v);
            }
        }
        let id = self.nodes.len();
        self.nodes.push(KdNode { start, end, split: None, lo: lo.clone(), hi: hi.clone() });
        if end - start <= LEAF_SIZE {
            return id;
        }
        let axis = (0..dim)
            .max_by(|&p, &q| (hi[p] - lo[p]).total_cmp(&(hi[q] - lo[q])).then(q.cmp(&p)))
            .unwrap_or(0);
        if hi[axis] <= lo[axis] {
            return id;
        }
        let mid = start + (end - start) / 2;
        let pts = &self.points;
        self.order[start..end].select_nth_unstable_by(mid - start, |&p, &q| {
            pts[p * dim + axis].total_cmp(&pts[q * dim + axis]).then(p.cmp(&q))
        });
        let split = self.coord(self.order[mid], axis);
        let left = self.build(start, mid);
        let right = self.build(mid, end);
        self.nodes[id].split = Some((axis, split, left, right));
        id
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn box_dist2(node: &KdNode, q: &[f64]) -> f64 {
        let mut d = 0.0;
        for ((&qa, &lo), &hi) in q.iter().zip(&node.lo).zip(&node.hi) {
            let v = if qa < lo {
                lo - qa
            } else if qa > hi {
                qa - hi
            } else {
                0.0
            };
            d += v * v;
        }
        d
    }



    pub fn query(&self, q: &[f64], k: usize) -> Result<Vec<(f64, usize)>, LinalgError> {
        if q.len() != self.dim {
            return Err(LinalgError::Shape(format!(
                "query of dimension {} in a {}-d tree",
                q.len(),
                self.dim
            )));
        }
        let k = k.min(self.len());
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut best: BinaryHeap<Cand> = BinaryHeap::new();
        let mut stack = vec![0usize];
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if best.len() == k && best.peek().is_some_and(|w| Self::box_dist2(node, q) > w.0) {
                continue;
            }
            match node.split {
                None => {
                    for &i in &self.order[node.start..node.end] {
                        let d2: f64 = (0..self.dim).map(|a| (self.coord(i, a) - q[a]).powi(2)).sum();
                        let c = Cand(d2, i);
                        if best.len() < k {
                            best.push(c);
                        } else if best.peek().is_some_and(|w| c < *w) {
                            best.pop();
                            best.push(c);
                        }
                    }
                }
                Some((axis, split, left, right)) => {
                    let (near, far) = if q[axis] < split { (left, right) } else { (right, left) };
                    stack.push(far);
                    stack.push(near);
                }
            }
        }
        let mut out: Vec<(f64, usize)> = best.into_iter().map(|c| (c.0.sqrt(), c.1)).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        Ok(out)
    }
}

#[inline]
fn lo32(x: u64) -> u32 {
    u32::try_from(x & 0xFFFF_FFFF).unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Big {
    neg: bool,
    mag: Vec<u32>,
}

impl Big {
    fn zero() -> Self {
        Self { neg: false, mag: Vec::new() }
    }

    fn trim(mut self) -> Self {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
        self
    }

    fn from_f64(x: f64, emin: i32) -> Self {
        if x == 0.0 {
            return Self::zero();
        }
        let (m, e) = decompose(x);
        let shift = u32::try_from(e - emin).unwrap_or(0);
        let mut mag = vec![0u32; (shift / 32) as usize];
        let bits = shift % 32;
        let wide = u128::from(m) << bits;
        let mut w = wide;
        while w > 0 {
            mag.push(u32::try_from(w & 0xFFFF_FFFF).unwrap_or(0));
            w >>= 32;
        }
        Self { neg: x < 0.0, mag }.trim()
    }

    fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
        if a.len() != b.len() {
            return a.len().cmp(&b.len());
        }
        for (x, y) in a.iter().rev().zip(b.iter().rev()) {
            if x != y {
                return x.cmp(y);
            }
        }
        Ordering::Equal
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
        let mut carry = 0u64;
        for i in 0..a.len().max(b.len()) {
            let s = u64::from(*a.get(i).unwrap_or(&0)) + u64::from(*b.get(i).unwrap_or(&0)) + carry;
            out.push(lo32(s));
            carry = s >> 32;
        }
        if carry > 0 {
            out.push(lo32(carry));
        }
        out
    }

    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len());
        let mut borrow = 0i64;
        for (i, &ai) in a.iter().enumerate() {
            let mut d = i64::from(ai) - i64::from(*b.get(i).unwrap_or(&0)) - borrow;
            if d < 0 {
                d += 1 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out.push(u32::try_from(d).unwrap_or(0));
        }
        out
    }

    fn add(&self, o: &Self) -> Self {
        if self.neg == o.neg {
            return Self { neg: self.neg, mag: Self::add_mag(&self.mag, &o.mag) }.trim();
        }
        match Self::cmp_mag(&self.mag, &o.mag) {
            Ordering::Equal => Self::zero(),
            Ordering::Greater => Self { neg: self.neg, mag: Self::sub_mag(&self.mag, &o.mag) }.trim(),
            Ordering::Less => Self { neg: o.neg, mag: Self::sub_mag(&o.mag, &self.mag) }.trim(),
        }
    }

    fn neg(&self) -> Self {
        Self { neg: !self.neg && !self.mag.is_empty(), mag: self.mag.clone() }
    }

    fn sub(&self, o: &Self) -> Self {
        self.add(&o.neg())
    }

    fn mul(&self, o: &Self) -> Self {
        if self.mag.is_empty() || o.mag.is_empty() {
            return Self::zero();
        }
        let mut out = vec![0u64; self.mag.len() + o.mag.len() + 1];
        for (i, &a) in self.mag.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.mag.iter().enumerate() {
                let t = out[i + j] + u64::from(a) * u64::from(b) + carry;
                out[i + j] = t & 0xFFFF_FFFF;
                carry = t >> 32;
            }
            let mut k = i + o.mag.len();
            while carry > 0 {
                let t = out[k] + carry;
                out[k] = t & 0xFFFF_FFFF;
                carry = t >> 32;
                k += 1;
            }
        }
        Self { neg: self.neg != o.neg, mag: out.into_iter().map(lo32).collect() }.trim()
    }

    fn signum(&self) -> i32 {
        if self.mag.is_empty() {
            0
        } else if self.neg {
            -1
        } else {
            1
        }
    }
}

fn decompose(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    if exp == 0 { (frac, -1074) } else { (frac | (1u64 << 52), exp - 1075) }
}

fn min_exponent(values: &[f64]) -> i32 {
    values.iter().filter(|v| **v != 0.0).map(|&v| decompose(v).1).min().unwrap_or(0)
}

fn orient2(a: &[f64], b: &[f64], c: &[f64]) -> i32 {
    let (bx, by, cx, cy) = (b[0] - a[0], b[1] - a[1], c[0] - a[0], c[1] - a[1]);
    let det = bx * cy - by * cx;
    let perm = (bx * cy).abs() + (by * cx).abs();
    let bound = (3.0 + 16.0 * f64::EPSILON) * f64::EPSILON * perm;
    if det > bound {
        return 1;
    }
    if -det > bound {
        return -1;
    }
    let vals = [a[0], a[1], b[0], b[1], c[0], c[1]];
    let e = min_exponent(&vals);
    let g = |v: f64| Big::from_f64(v, e);
    let (ax, ay) = (g(a[0]), g(a[1]));
    let bx = g(b[0]).sub(&ax);
    let by = g(b[1]).sub(&ay);
    let cx = g(c[0]).sub(&ax);
    let cy = g(c[1]).sub(&ay);
    bx.mul(&cy).sub(&by.mul(&cx)).signum()
}

fn orient3(a: &[f64], b: &[f64], c: &[f64], d: &[f64]) -> i32 {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let w = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
    let det = u[0] * (v[1] * w[2] - v[2] * w[1]) - u[1] * (v[0] * w[2] - v[2] * w[0])
        + u[2] * (v[0] * w[1] - v[1] * w[0]);
    let perm = u[0].abs() * ((v[1] * w[2]).abs() + (v[2] * w[1]).abs())
        + u[1].abs() * ((v[0] * w[2]).abs() + (v[2] * w[0]).abs())
        + u[2].abs() * ((v[0] * w[1]).abs() + (v[1] * w[0]).abs());
    let bound = (7.0 + 56.0 * f64::EPSILON) * f64::EPSILON * perm;
    if det > bound {
        return 1;
    }
    if -det > bound {
        return -1;
    }
    let vals = [a[0], a[1], a[2], b[0], b[1], b[2], c[0], c[1], c[2], d[0], d[1], d[2]];
    let e = min_exponent(&vals);
    let g = |x: f64| Big::from_f64(x, e);
    let ab: Vec<Big> = (0..3).map(|k| g(b[k]).sub(&g(a[k]))).collect();
    let ac: Vec<Big> = (0..3).map(|k| g(c[k]).sub(&g(a[k]))).collect();
    let ad: Vec<Big> = (0..3).map(|k| g(d[k]).sub(&g(a[k]))).collect();
    let m0 = ac[1].mul(&ad[2]).sub(&ac[2].mul(&ad[1]));
    let m1 = ac[0].mul(&ad[2]).sub(&ac[2].mul(&ad[0]));
    let m2 = ac[0].mul(&ad[1]).sub(&ac[1].mul(&ad[0]));
    ab[0].mul(&m0).sub(&ab[1].mul(&m1)).add(&ab[2].mul(&m2)).signum()
}

fn in_sphere(dim: usize, s: &[&[f64]], e: &[f64]) -> f64 {
    if dim == 2 {
        let r: Vec<[f64; 3]> = s
            .iter()
            .map(|p| {
                let (x, y) = (p[0] - e[0], p[1] - e[1]);
                [x, y, x * x + y * y]
            })
            .collect();
        r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1]) - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
            + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0])
    } else {
        let r: Vec<[f64; 4]> = s
            .iter()
            .map(|p| {
                let (x, y, z) = (p[0] - e[0], p[1] - e[1], p[2] - e[2]);
                [x, y, z, x * x + y * y + z * z]
            })
            .collect();
        let det3 = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
            a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0])
        };
        let minor = |skip: usize| {
            let rows: Vec<[f64; 3]> = (0..4)
                .map(|i| {
                    let cols: Vec<f64> = (0..4).filter(|&c| c != skip).map(|c| r[i][c]).collect();
                    [cols[0], cols[1], cols[2]]
                })
                .collect();
            rows
        };
        let m = minor(3);
        let mut det = 0.0;
        for (i, ri) in r.iter().enumerate() {
            let others: Vec<[f64; 3]> = (0..4).filter(|&k| k != i).map(|k| m[k]).collect();
            let sign = if (i + 3) % 2 == 0 { 1.0 } else { -1.0 };
            det += sign * ri[3] * det3(others[0], others[1], others[2]);
        }
        -det
    }
}

#[derive(Clone, Debug)]
pub struct Delaunay {
    dim: usize,
    points: Vec<f64>,
    pub simplices: Vec<Vec<usize>>,
    pub neighbors: Vec<Vec<Option<usize>>>,
    pub coplanar: Vec<usize>,
    transforms: Vec<(Vec<f64>, Vec<f64>)>,
}

struct Builder<'a> {
    dim: usize,
    pts: &'a [f64],
    sup: Vec<f64>,
    n: usize,
    simp: Vec<[usize; 4]>,
    nbr: Vec<[usize; 4]>,
    alive: Vec<bool>,
    last: usize,
}

const NONE: usize = usize::MAX;

impl Builder<'_> {
    fn p(&self, i: usize) -> &[f64] {
        if i < self.n {
            &self.pts[i * self.dim..(i + 1) * self.dim]
        } else {
            &self.sup[(i - self.n) * self.dim..(i - self.n + 1) * self.dim]
        }
    }

    fn orient(&self, v: &[usize]) -> i32 {
        if self.dim == 2 {
            orient2(self.p(v[0]), self.p(v[1]), self.p(v[2]))
        } else {
            orient3(self.p(v[0]), self.p(v[1]), self.p(v[2]), self.p(v[3]))
        }
    }

    fn orient_replaced(&self, t: usize, i: usize, q: usize) -> i32 {
        let mut v = self.simp[t];
        v[i] = q;
        self.orient(&v[..=self.dim])
    }

    fn contains(&self, t: usize, q: usize) -> bool {
        (0..=self.dim).all(|i| self.orient_replaced(t, i, q) >= 0)
    }

    fn locate(&self, q: usize) -> usize {
        let mut t = self.last;
        if !self.alive[t] {
            t = self.alive.iter().rposition(|&a| a).unwrap_or(0);
        }
        let limit = 4 * self.simp.len() + 16;
        'walk: for _ in 0..limit {
            for i in 0..=self.dim {
                if self.orient_replaced(t, i, q) < 0 {
                    let nb = self.nbr[t][i];
                    if nb == NONE {
                        break 'walk;
                    }
                    t = nb;
                    continue 'walk;
                }
            }
            return t;
        }
        (0..self.simp.len()).find(|&s| self.alive[s] && self.contains(s, q)).unwrap_or(t)
    }

    fn insert(&mut self, q: usize) -> bool {
        let start = self.locate(q);
        if (0..=self.dim).any(|i| {
            let v = self.simp[start][i];
            self.p(v) == self.p(q)
        }) {
            return false;
        }
        let d = self.dim;
        let mut in_cavity = std::collections::BTreeSet::new();
        in_cavity.insert(start);
        let mut queue = vec![start];
        while let Some(t) = queue.pop() {
            for i in 0..=d {
                let nb = self.nbr[t][i];
                if nb == NONE || in_cavity.contains(&nb) {
                    continue;
                }
                let verts: Vec<&[f64]> = (0..=d).map(|k| self.p(self.simp[nb][k])).collect();
                if in_sphere(d, &verts, self.p(q)) > 0.0 {
                    in_cavity.insert(nb);
                    queue.push(nb);
                }
            }
        }

        loop {
            let mut grow = Vec::new();
            for &t in &in_cavity {
                for i in 0..=d {
                    let nb = self.nbr[t][i];
                    if nb != NONE && in_cavity.contains(&nb) {
                        continue;
                    }
                    if self.orient_replaced(t, i, q) <= 0 && nb != NONE {
                        grow.push(nb);
                    }
                }
            }
            if grow.is_empty() {
                break;
            }
            for g in grow {
                in_cavity.insert(g);
            }
        }

        let mut faces: BTreeMap<Vec<usize>, (usize, usize)> = BTreeMap::new();
        let cavity: Vec<usize> = in_cavity.iter().copied().collect();
        for &t in &cavity {
            for i in 0..=d {
                let nb = self.nbr[t][i];
                if nb != NONE && in_cavity.contains(&nb) {
                    continue;
                }
                let mut v = self.simp[t];
                v[i] = q;
                let id = self.simp.len();
                self.simp.push(v);
                self.alive.push(true);
                let mut links = [NONE; 4];
                links[i] = nb;
                if nb != NONE {

                    if let Some(k) = (0..=d).find(|&k| self.nbr[nb][k] == t) {
                        self.nbr[nb][k] = id;
                    }
                }
                self.nbr.push(links);
                for j in 0..=d {
                    if j == i {
                        continue;
                    }
                    let mut key: Vec<usize> = (0..=d).filter(|&k| k != j && k != i).map(|k| v[k]).collect();
                    key.sort_unstable();
                    if let Some((other, oslot)) = faces.remove(&key) {
                        self.nbr[id][j] = other;
                        self.nbr[other][oslot] = id;
                    } else {
                        faces.insert(key, (id, j));
                    }
                }
                self.last = id;
            }
        }
        for t in cavity {
            self.alive[t] = false;
        }
        true
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]                             
fn quantize_1023(t: f64) -> u64 {
    (t * 1023.0).round().clamp(0.0, 1023.0) as u64
}

fn spatial_order(points: &[f64], dim: usize, n: usize) -> Vec<usize> {
    let mut lo = vec![f64::INFINITY; dim];
    let mut hi = vec![f64::NEG_INFINITY; dim];
    for i in 0..n {
        for a in 0..dim {
            lo[a] = lo[a].min(points[i * dim + a]);
            hi[a] = hi[a].max(points[i * dim + a]);
        }
    }
    let key = |i: usize| -> u64 {
        let mut k = 0u64;
        let q: Vec<u64> = (0..dim)
            .map(|a| {
                let span = hi[a] - lo[a];
                let t = if span > 0.0 { (points[i * dim + a] - lo[a]) / span } else { 0.0 };
                quantize_1023(t)
            })
            .collect();
        for bit in (0..10).rev() {
            for &c in &q {
                k = (k << 1) | ((c >> bit) & 1);
            }
        }
        k
    };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (key(i), i));
    order
}

impl Delaunay {


    #[allow(clippy::too_many_lines)]
    pub fn new(points: &[f64], dim: usize) -> Result<Self, LinalgError> {
        if !(dim == 2 || dim == 3) || !points.len().is_multiple_of(dim) {
            return Err(LinalgError::Shape(format!("Delaunay supports 2-D and 3-D points, got dim {dim}")));
        }
        if !points.iter().all(|v| v.is_finite()) {
            return Err(LinalgError::NonFinite("Delaunay points must be finite".into()));
        }
        let n = points.len() / dim;
        if n < dim + 1 {
            return Err(LinalgError::Shape(format!("{n} points cannot span a {dim}-simplex")));
        }
        let mut lo = vec![f64::INFINITY; dim];
        let mut hi = vec![f64::NEG_INFINITY; dim];
        for i in 0..n {
            for a in 0..dim {
                lo[a] = lo[a].min(points[i * dim + a]);
                hi[a] = hi[a].max(points[i * dim + a]);
            }
        }
        let center: Vec<f64> = (0..dim).map(|a| 0.5 * (lo[a] + hi[a])).collect();
        let span = (0..dim).map(|a| hi[a] - lo[a]).fold(0.0, f64::max).max(1.0);
        let big = 1e4 * span;

        let sup: Vec<f64> = if dim == 2 {
            vec![
                center[0] - 3.0 * big,
                center[1] - 3.0 * big,
                center[0] + 3.0 * big,
                center[1] - 3.0 * big,
                center[0],
                center[1] + 3.0 * big,
            ]
        } else {
            vec![
                center[0] - 3.0 * big,
                center[1] - 3.0 * big,
                center[2] - 3.0 * big,
                center[0] + 3.0 * big,
                center[1] - 3.0 * big,
                center[2] - 3.0 * big,
                center[0],
                center[1] + 3.0 * big,
                center[2] - 3.0 * big,
                center[0],
                center[1],
                center[2] + 3.0 * big,
            ]
        };
        let root: [usize; 4] = if dim == 2 { [n, n + 1, n + 2, NONE] } else { [n, n + 1, n + 2, n + 3] };
        let mut b = Builder {
            dim,
            pts: points,
            sup,
            n,
            simp: vec![root],
            nbr: vec![[NONE; 4]],
            alive: vec![true],
            last: 0,
        };
        if b.orient(&root[..=dim]) < 0 {
            b.simp[0].swap(0, 1);
        }
        let mut coplanar = Vec::new();
        for q in spatial_order(points, dim, n) {
            if !b.insert(q) {
                coplanar.push(q);
            }
        }
        coplanar.sort_unstable();

        let keep: Vec<usize> =
            (0..b.simp.len()).filter(|&t| b.alive[t] && b.simp[t][..=dim].iter().all(|&v| v < n)).collect();
        if keep.is_empty() {
            return Err(LinalgError::Singular(
                "QH6154 initial simplex is flat: points are degenerate".into(),
            ));
        }
        let mut new_id = vec![NONE; b.simp.len()];
        for (k, &t) in keep.iter().enumerate() {
            new_id[t] = k;
        }
        let simplices: Vec<Vec<usize>> = keep.iter().map(|&t| b.simp[t][..=dim].to_vec()).collect();
        let neighbors: Vec<Vec<Option<usize>>> = keep
            .iter()
            .map(|&t| {
                (0..=dim)
                    .map(|i| b.nbr[t][i])
                    .map(|nb| if nb == NONE || new_id[nb] == NONE { None } else { Some(new_id[nb]) })
                    .collect()
            })
            .collect();
        let mut transforms = Vec::with_capacity(simplices.len());
        for s in &simplices {
            let r: Vec<f64> = points[s[dim] * dim..(s[dim] + 1) * dim].to_vec();

            let mut t = vec![0.0; dim * dim];
            for k in 0..dim {
                for a in 0..dim {
                    t[a * dim + k] = points[s[k] * dim + a] - r[a];
                }
            }

            let tinv = crate::dense::inv(&crate::dense::DenseMatrix::new(dim, dim, t)?)
                .map_or_else(|_| vec![f64::NAN; dim * dim], |m| m.data);
            transforms.push((tinv, r));
        }
        Ok(Self { dim, points: points.to_vec(), simplices, neighbors, coplanar, transforms })
    }

    #[must_use]
    pub fn dim(&self) -> usize {
        self.dim
    }

    #[must_use]
    pub fn points(&self) -> &[f64] {
        &self.points
    }



    pub fn barycentric(&self, s: usize, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        let (tinv, r) =
            self.transforms.get(s).ok_or_else(|| LinalgError::Shape(format!("simplex {s} out of range")))?;
        if x.len() != self.dim {
            return Err(LinalgError::Shape("point dimension mismatch".into()));
        }
        let d = self.dim;
        let mut c: Vec<f64> = (0..d).map(|i| (0..d).map(|j| tinv[i * d + j] * (x[j] - r[j])).sum()).collect();
        let last = 1.0 - c.iter().sum::<f64>();
        c.push(last);
        Ok(c)
    }

    #[must_use]
    pub fn transform(&self, s: usize) -> Option<(&[f64], &[f64])> {
        self.transforms.get(s).map(|(t, r)| (t.as_slice(), r.as_slice()))
    }



    pub fn find_simplex(&self, x: &[f64]) -> Result<Option<usize>, LinalgError> {
        if x.len() != self.dim {
            return Err(LinalgError::Shape("point dimension mismatch".into()));
        }
        let eps = 100.0 * f64::EPSILON;

        let mut s = 0usize;
        for _ in 0..(4 * self.simplices.len() + 16) {
            let c = self.barycentric(s, x)?;
            let (k, &m) = c.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).unwrap_or((0, &0.0));
            if m >= -eps {
                return Ok(Some(s));
            }
            match self.neighbors[s][k] {
                Some(nb) => s = nb,
                None => break,
            }
        }
        for s in 0..self.simplices.len() {
            if self.barycentric(s, x)?.iter().all(|&v| v >= -eps) {
                return Ok(Some(s));
            }
        }
        Ok(None)
    }
}

