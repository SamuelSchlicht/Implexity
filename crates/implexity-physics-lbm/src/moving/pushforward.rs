// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use rayon::prelude::*;

use crate::d3q19::Grid;

pub const MAX_SUPPORT: usize = 32;

pub const NONE: u32 = u32::MAX;

#[inline]
#[must_use]
pub fn m4<S: Scalar>(s: S) -> S {
    let v = s.value();
    let a = if v < 0.0 { -s } else { s };
    let av = a.value();
    if av < 1.0 {
        (S::from_f64(4.0) - a * a * 6.0 + a * a * a * 3.0) / 6.0
    } else if av < 2.0 {
        let t = S::from_f64(2.0) - a;
        t * t * t / 6.0
    } else {
        S::zero()
    }
}

#[inline]
#[must_use]
pub fn m4_derivative<S: Scalar>(s: S) -> S {
    let v = s.value();
    let neg = v < 0.0;
    let a = if neg { -s } else { s };
    let av = a.value();
    let d = if av < 1.0 {
        -a * 2.0 + a * a * 1.5
    } else if av < 2.0 {
        let t = S::from_f64(2.0) - a;
        -(t * t) * 0.5
    } else {
        S::zero()
    };
    if neg { -d } else { d }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterpolationKernel {
    Cubic,
    Peskin4,
}

impl InterpolationKernel {
    pub fn as_str(self) -> &'static str {
        match self { Self::Cubic => "cubic", Self::Peskin4 => "peskin4" }
    }
    pub fn from_name(name: &str) -> CaeResult<Self> {
        match name { "cubic" => Ok(Self::Cubic), "peskin4" => Ok(Self::Peskin4), _ => Err(CaeError::contract("unknown interpolation kernel")) }
    }
}

pub fn peskin4<S: Scalar>(s: S) -> S {
 let r=if s.value()<0.0 {-s}else{s};let v=r.value();assert!(v.is_finite());
 if v<1.0 {(S::from_f64(3.0)-r*2.0+(S::one()+r*4.0-r*r*4.0).sqrt())/8.0}
 else if v<2.0 {(S::from_f64(5.0)-r*2.0-(S::from_f64(-7.0)+r*12.0-r*r*4.0).sqrt())/8.0}
 else {S::zero()}
}
pub fn peskin4_derivative<S: Scalar>(s: S) -> S {
 let neg=s.value()<0.0;let r=if neg {-s}else{s};let v=r.value();assert!(v.is_finite());
 let d=if v<1.0 {(-S::from_f64(2.0)+(S::from_f64(2.0)-r*4.0)/(S::one()+r*4.0-r*r*4.0).sqrt())/8.0}
 else if v<2.0 {(-S::from_f64(2.0)-(S::from_f64(6.0)-r*4.0)/(S::from_f64(-7.0)+r*12.0-r*r*4.0).sqrt())/8.0}
 else {S::zero()};if neg {-d}else{d}
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub shape: [usize; 3],
    pub spacing_m: f64,
    pub origin_m: [f64; 3],
    pub periodic: [bool; 3],
    pub width_cells: usize,
    pub mirrored: [[bool; 2]; 3],
    pub interpolation_kernel: InterpolationKernel,
}

impl Frame {

    pub fn new(
        shape: [usize; 3],
        spacing_m: f64,
        origin_m: [f64; 3],
        periodic: [bool; 3],
        width_cells: usize,
    ) -> CaeResult<Self> {
        if !(spacing_m.is_finite() && spacing_m > 0.0) || origin_m.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "push-forward frame requires a finite positive spacing and origin",
            ));
        }
        if width_cells == 0 || shape.contains(&0) {
            return Err(CaeError::contract("push-forward kernel width and lattice shape must be positive"));
        }
        for a in 0..3 {
            if periodic[a] && shape[a] > 1 && shape[a] < 4 * width_cells {
                return Err(CaeError::contract(format!(
                    "periodic axis {a} ({} cells) is shorter than the push-forward kernel support ({} cells)",
                    shape[a],
                    4 * width_cells
                )));
            }
            if !periodic[a] && shape[a] == 1 {
                return Err(CaeError::contract(format!(
                    "a one-cell lattice axis ({a}) must be periodic (planar) for the push-forward"
                )));
            }
        }
        Ok(Self { shape, spacing_m, origin_m, periodic, width_cells, mirrored: [[false; 2]; 3], interpolation_kernel: InterpolationKernel::Cubic })
    }

    pub fn with_kernel(mut self, kernel: InterpolationKernel) -> Self {
        self.interpolation_kernel = kernel;
        self
    }


    pub fn with_mirrors(mut self, mirrored: [[bool; 2]; 3]) -> CaeResult<Self> {
        for a in 0..3 {
            if (mirrored[a][0] || mirrored[a][1])
                && (self.periodic[a] || self.shape[a] < 4 * self.width_cells)
            {
                return Err(CaeError::contract(format!(
                    "push-forward mirror face on axis {a} needs a non-periodic axis of at least {} cells",
                    4 * self.width_cells
                )));
            }
        }
        self.mirrored = mirrored;
        Ok(self)
    }

    #[inline]
    #[must_use]
    pub fn planar(&self, a: usize) -> bool {
        self.shape[a] == 1 && self.periodic[a]
    }

    #[inline]
    #[must_use]
    pub fn support(&self, a: usize) -> usize {
        if self.planar(a) { 1 } else { 4 * self.width_cells }
    }

    #[must_use]
    pub fn grid(&self) -> Grid {
        Grid::new(self.shape)
    }

    #[inline]
    #[must_use]
    pub fn to_lattice<S: Scalar>(&self, x: [S; 3]) -> [S; 3] {
        std::array::from_fn(|a| (x[a] - self.origin_m[a]) / self.spacing_m - 0.5)
    }
}

#[derive(Clone, Debug)]
pub struct Stencils<S> {
    pub base: Vec<[usize; 3]>,
    pub w: Vec<S>,
    pub dw: Vec<S>,
    stride: usize,
}

impl<S: Scalar> Stencils<S> {

    pub fn new(frame: &Frame, xi: &[[S; 3]], gradients: bool) -> CaeResult<Self> {
        Self::new_with_kernel(frame, xi, gradients, frame.interpolation_kernel)
    }

    pub fn new_with_kernel(frame: &Frame, xi: &[[S; 3]], gradients: bool, kernel: InterpolationKernel) -> CaeResult<Self> {
        let h = frame.width_cells;
        let stride = 4 * h;
        let hf = h as f64;
        for (q, x) in xi.iter().enumerate() {
            for a in 0..3 {
                let v = x[a].value();
                if !v.is_finite() {
                    return Err(CaeError::contract(format!("push-forward point {q} is not finite")));
                }
                if frame.planar(a) || frame.periodic[a] {
                    continue;
                }
                let n = frame.shape[a] as f64;
                let [lo_m, hi_m] = frame.mirrored[a];
                if (lo_m && v < -0.5) || (hi_m && v > n - 0.5) {
                    return Err(CaeError::contract(format!(
                        "moving LBM admission refused (pushforward_support): point {q} at lattice coordinate {v:.6} \
                         on axis {a} lies beyond a symmetry face"
                    )));
                }
                if (!lo_m && v - 2.0 * hf <= -1.0) || (!hi_m && v + 2.0 * hf >= n) {
                    return Err(CaeError::contract(format!(
                        "moving LBM admission refused (pushforward_support): point {q} at lattice coordinate {v:.6} \
                         on axis {a} has kernel support outside the {}-cell lattice",
                        frame.shape[a]
                    )));
                }
            }
        }
        let per = 3 * stride;
        let mut base = vec![[0usize; 3]; xi.len()];
        let mut w = vec![S::zero(); xi.len() * per];
        let mut dw = if gradients { vec![S::zero(); xi.len() * per] } else { Vec::new() };
        let fill = |x: &[S; 3], base: &mut [usize; 3], w: &mut [S], mut dw: Option<&mut [S]>| {
            for a in 0..3 {
                if frame.planar(a) {
                    w[a * stride] = S::one();
                    continue;
                }
                let v = x[a].value();
                #[allow(clippy::cast_possible_truncation)]
                let b = v.floor() as i64;
                #[allow(clippy::cast_possible_wrap)]
                let first = b - 2 * h as i64 + 1;
                #[allow(clippy::cast_possible_wrap)]
                let n = frame.shape[a] as i64;
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                {
                    base[a] = first.rem_euclid(n) as usize;
                }

                #[allow(clippy::cast_possible_wrap)]
                let span = 4 * h as i64;
                let fold = if frame.mirrored[a][0] && first < 0 {
                    Some(0)
                } else if frame.mirrored[a][1] && first + span > n {
                    Some(n - span)
                } else {
                    None
                };
                if h == 1 && kernel == InterpolationKernel::Cubic {

                    #[allow(clippy::cast_precision_loss)]
                    let phi = x[a] - b as f64;
                    let r = S::one() - phi;
                    let (p2, r2) = (phi * phi, r * r);
                    let seg = &mut w[a * stride..a * stride + 4];
                    seg[0] = r2 * r / 6.0;
                    seg[1] = (p2 * (phi * 3.0 - 6.0) + 4.0) / 6.0;
                    seg[2] = (((-phi * 3.0 + 3.0) * phi + 3.0) * phi + 1.0) / 6.0;
                    seg[3] = p2 * phi / 6.0;
                    if let Some(d) = dw.as_deref_mut() {
                        let dseg = &mut d[a * stride..a * stride + 4];
                        dseg[0] = -r2 * 0.5;
                        dseg[1] = phi * (phi * 1.5 - 2.0);
                        dseg[2] = (-phi * 1.5 + 1.0) * phi + 0.5;
                        dseg[3] = p2 * 0.5;
                    }
                } else {
                    for t in 0..stride {
                        #[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
                        let c = (first + t as i64) as f64;
                        let r = (S::from_f64(c) - x[a]) / hf;
                        w[a * stride + t] = match kernel { InterpolationKernel::Cubic => m4(r), InterpolationKernel::Peskin4 => peskin4(r) } / hf;
                        if let Some(d) = dw.as_deref_mut() {
                            d[a * stride + t] = -match kernel { InterpolationKernel::Cubic => m4_derivative(r), InterpolationKernel::Peskin4 => peskin4_derivative(r) } / (hf * hf);
                        }
                    }
                }
                if let Some(start) = fold {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    {
                        base[a] = start as usize;
                    }
                    fold_axis(&mut w[a * stride..(a + 1) * stride], first, start, n);
                    if let Some(d) = dw.as_deref_mut() {
                        fold_axis(&mut d[a * stride..(a + 1) * stride], first, start, n);
                    }
                }
            }
        };
        if gradients {
            base.par_iter_mut()
                .zip(w.par_chunks_mut(per))
                .zip(dw.par_chunks_mut(per))
                .zip(xi.par_iter())
                .for_each(|(((b, w), d), x)| fill(x, b, w, Some(d)));
        } else {
            base.par_iter_mut()
                .zip(w.par_chunks_mut(per))
                .zip(xi.par_iter())
                .for_each(|((b, w), x)| fill(x, b, w, None));
        }
        let out = Self { base, w, dw, stride };
        Ok(out)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.base.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }

    #[inline]
    fn weight(&self, q: usize, a: usize, t: usize) -> S {
        self.w[(q * 3 + a) * self.stride + t]
    }

    #[inline]
    fn dweight(&self, q: usize, a: usize, t: usize) -> S {
        self.dw[(q * 3 + a) * self.stride + t]
    }
}

fn fold_axis<S: Scalar>(w: &mut [S], first: i64, start: i64, n: i64) {
    let len = w.len();
    let mut out = vec![S::zero(); len];
    for (t, v) in w.iter().enumerate() {
        #[allow(clippy::cast_possible_wrap)]
        let c = first + t as i64;
        let m = if c < 0 {
            -1 - c
        } else if c >= n {
            2 * n - 1 - c
        } else {
            c
        };
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let r = (m - start) as usize;
        out[r] += *v;
    }
    w.copy_from_slice(&out);
}

#[inline]
fn offset(frame: &Frame, a: usize, base: usize, c: usize) -> Option<usize> {
    if frame.planar(a) {
        return Some(0);
    }
    let n = frame.shape[a];
    let t = if frame.periodic[a] { (c + n - base) % n } else { c.wrapping_sub(base) };
    (t < 4 * frame.width_cells).then_some(t)
}

#[derive(Clone, Debug)]
pub struct BinGrid {
    start: Vec<u32>,
    points: Vec<u32>,
}

impl BinGrid {
    #[must_use]
    pub fn new<S: Scalar>(frame: &Frame, stencils: &Stencils<S>) -> Self {
        let grid = frame.grid();
        let n = grid.cells();
        let h = frame.width_cells;
        let bin_of =
            |q: usize| -> usize {
                let b = stencils.base[q];
                let c: [usize; 3] = std::array::from_fn(|a| {
                    if frame.planar(a) { 0 } else { (b[a] + 2 * h - 1) % frame.shape[a] }
                });
                grid.index(c[0], c[1], c[2])
            };
        let mut counts = vec![0u32; n + 1];
        let bins: Vec<usize> = (0..stencils.len()).map(bin_of).collect();
        for &b in &bins {
            counts[b + 1] += 1;
        }
        for i in 0..n {
            counts[i + 1] += counts[i];
        }
        let mut fill = counts.clone();
        let mut points = vec![0u32; stencils.len()];
        for (q, &b) in bins.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            {
                points[fill[b] as usize] = q as u32;
            }
            fill[b] += 1;
        }
        Self { start: counts, points }
    }

    #[inline]
    #[must_use]
    pub fn bin(&self, cell: usize) -> &[u32] {
        &self.points[self.start[cell] as usize..self.start[cell + 1] as usize]
    }

    #[must_use]
    pub fn occupied(&self) -> Vec<usize> {
        (0..self.start.len() - 1).filter(|&c| self.start[c + 1] > self.start[c]).collect()
    }
}

#[inline]
fn neighbours(frame: &Frame, a: usize, c: usize, lo_shift: i64) -> ([usize; MAX_SUPPORT], usize) {
    let mut out = [0usize; MAX_SUPPORT];
    if frame.planar(a) {
        return (out, 1);
    }
    #[allow(clippy::cast_possible_wrap)]
    let (n, h, ci) = (frame.shape[a] as i64, frame.width_cells as i64, c as i64);
    let mut len = 0;
    for k in 0..4 * h {
        let v = ci - 2 * h + lo_shift + k;
        let w = if frame.periodic[a] { v.rem_euclid(n) } else { v };
        if (0..n).contains(&w) {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            {
                out[len] = w as usize;
            }
            len += 1;
        }
    }
    (out, len)
}

#[must_use]
pub fn support_cells(frame: &Frame, bins: &[&BinGrid]) -> Vec<u32> {
    let grid = frame.grid();
    let mut mark = vec![false; grid.cells()];
    for b in bins {
        for cell in b.occupied() {
            let c = grid.coords(cell);
            let (nx, lx) = neighbours(frame, 0, c[0], 1);
            let (ny, ly) = neighbours(frame, 1, c[1], 1);
            let (nz, lz) = neighbours(frame, 2, c[2], 1);
            for &i in &nx[..lx] {
                for &j in &ny[..ly] {
                    for &k in &nz[..lz] {
                        mark[grid.index(i, j, k)] = true;
                    }
                }
            }
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    (0..grid.cells()).filter(|&c| mark[c]).map(|c| c as u32).collect()
}

#[must_use]
pub fn cell_gather<S: Scalar>(
    frame: &Frame,
    bins: &BinGrid,
    stencils: &Stencils<S>,
    a: &[S],
    vec: Option<&[[S; 3]]>,
    cells: &[u32],
) -> (Vec<S>, Vec<[S; 3]>) {
    let grid = frame.grid();
    cells
        .par_iter()
        .map(|&cell| {
            let c = grid.coords(cell as usize);
            let (nx, lx) = neighbours(frame, 0, c[0], 0);
            let (ny, ly) = neighbours(frame, 1, c[1], 0);
            let (nz, lz) = neighbours(frame, 2, c[2], 0);
            let mut d = S::zero();
            let mut m = [S::zero(); 3];
            for &i in &nx[..lx] {
                for &j in &ny[..ly] {
                    for &k in &nz[..lz] {
                        let bin = bins.bin(grid.index(i, j, k));
                        let Some(&first) = bin.first() else { continue };

                        let base = stencils.base[first as usize];
                        let (Some(tx), Some(ty), Some(tz)) = (
                            offset(frame, 0, base[0], c[0]),
                            offset(frame, 1, base[1], c[1]),
                            offset(frame, 2, base[2], c[2]),
                        ) else {
                            continue;
                        };
                        for &q in bin {
                            let q = q as usize;
                            let k_w = stencils.weight(q, 0, tx)
                                * stencils.weight(q, 1, ty)
                                * stencils.weight(q, 2, tz);
                            let w = a[q] * k_w;
                            d += w;
                            if let Some(v) = vec {
                                for e in 0..3 {
                                    m[e] += w * v[q][e];
                                }
                            }
                        }
                    }
                }
            }
            (d, m)
        })
        .unzip()
}

#[derive(Clone, Copy, Debug)]
pub struct PointGather<S> {
    pub ks: S,
    pub kv: [S; 3],
    pub gs: [S; 3],
    pub gv: [[S; 3]; 3],
}

#[must_use]
pub fn point_gather<S: Scalar>(
    frame: &Frame,
    stencils: &Stencils<S>,
    slot: &[u32],
    scalar: Option<&[S]>,
    vector: Option<&[[S; 3]]>,
    gradients: bool,
) -> Vec<PointGather<S>> {

    let bx = SupportBox::of(frame, &[stencils]);
    let dense_s: Vec<S> = match scalar {
        Some(v) => (0..bx.cells())
            .into_par_iter()
            .map(|r| {
                let s = slot[bx.lattice_cell(frame, r)];
                if s == NONE { S::zero() } else { v[s as usize] }
            })
            .collect(),
        None => Vec::new(),
    };
    let dense_v: Vec<[S; 3]> = match vector {
        Some(v) => (0..bx.cells())
            .into_par_iter()
            .map(|r| {
                let s = slot[bx.lattice_cell(frame, r)];
                if s == NONE { [S::zero(); 3] } else { v[s as usize] }
            })
            .collect(),
        None => Vec::new(),
    };
    let (with_s, with_v) = (scalar.is_some(), vector.is_some());
    let sup = [frame.support(0), frame.support(1), frame.support(2)];
    let planar = [frame.planar(0), frame.planar(1), frame.planar(2)];
    (0..stencils.len())
        .into_par_iter()
        .map(|q| {
            let b = stencils.base[q];
            let r = [bx.rel(frame, 0, b[0]), bx.rel(frame, 1, b[1]), bx.rel(frame, 2, b[2])];
            let mut out = PointGather {
                ks: S::zero(),
                kv: [S::zero(); 3],
                gs: [S::zero(); 3],
                gv: [[S::zero(); 3]; 3],
            };
            let mut ys = [0usize; MAX_SUPPORT];
            let mut zs = [0usize; MAX_SUPPORT];
            for t in 0..sup[1] {
                ys[t] = bx.wrap(frame, 1, r[1] + t);
            }
            for t in 0..sup[2] {
                zs[t] = bx.wrap(frame, 2, r[2] + t);
            }
            let dweight = |a: usize, t: usize| {
                if gradients && !planar[a] { stencils.dweight(q, a, t) } else { S::zero() }
            };
            for tx in 0..sup[0] {
                let x = bx.wrap(frame, 0, r[0] + tx);
                let (wx, dx) = (stencils.weight(q, 0, tx), dweight(0, tx));
                for ty in 0..sup[1] {
                    let (wy, dy) = (stencils.weight(q, 1, ty), dweight(1, ty));
                    let row = (x * bx.len[1] + ys[ty]) * bx.len[2];
                    for tz in 0..sup[2] {
                        let cell = row + zs[tz];
                        let wz = stencils.weight(q, 2, tz);
                        let kw = wx * wy * wz;
                        if with_s {
                            out.ks += kw * dense_s[cell];
                        }
                        if with_v {
                            let vv = dense_v[cell];
                            for e in 0..3 {
                                out.kv[e] += kw * vv[e];
                            }
                        }
                        if gradients {
                            let dz = dweight(2, tz);
                            let grad = [dx * wy * wz, wx * dy * wz, wx * wy * dz];
                            for d in 0..3 {
                                if with_s {
                                    out.gs[d] += grad[d] * dense_s[cell];
                                }
                                if with_v {
                                    let vv = dense_v[cell];
                                    for e in 0..3 {
                                        out.gv[d][e] += grad[d] * vv[e];
                                    }
                                }
                            }
                        }
                    }
                }
            }
            out
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct PointSet<S> {
    pub xi: Vec<[S; 3]>,
    pub a: Vec<S>,
}

#[derive(Clone, Debug)]
pub struct Endpoint<S> {
    pub points: PointSet<S>,
    pub stencils: Stencils<S>,
    bins: std::sync::OnceLock<BinGrid>,
}

impl<S: Scalar> Endpoint<S> {

    pub fn new(frame: &Frame, points: PointSet<S>, gradients: bool) -> CaeResult<Self> {
        let stencils = Stencils::new(frame, &points.xi, gradients)?;
        Ok(Self { points, stencils, bins: std::sync::OnceLock::new() })
    }

    #[must_use]
    pub fn bins(&self, frame: &Frame) -> &BinGrid {
        self.bins.get_or_init(|| BinGrid::new(frame, &self.stencils))
    }
}

#[derive(Clone, Debug)]
pub struct Occupancy<S> {
    pub cells: Vec<u32>,
    pub slot: Vec<u32>,
    pub d: [Vec<S>; 2],
    pub m: [Vec<[S; 3]>; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportBox {
    pub lo: [usize; 3],
    pub len: [usize; 3],
    pub full: [bool; 3],
}

impl SupportBox {
    #[must_use]
    pub fn of<S: Scalar>(frame: &Frame, sets: &[&Stencils<S>]) -> Self {
        let mut lo = [0usize; 3];
        let mut len = [1usize; 3];
        let mut full = [false; 3];
        for a in 0..3 {
            if frame.planar(a) {
                continue;
            }
            let mut mn = usize::MAX;
            let mut mx = 0usize;
            for st in sets {
                for b in &st.base {
                    mn = mn.min(b[a]);
                    mx = mx.max(b[a]);
                }
            }
            let n = frame.shape[a];
            if mn == usize::MAX {
                len[a] = 0;
            } else if frame.periodic[a] && mx + 4 * frame.width_cells - mn > n {
                len[a] = n;
                full[a] = true;
            } else {
                lo[a] = mn;
                len[a] = mx + 4 * frame.width_cells - mn;
                if !frame.periodic[a] {
                    len[a] = len[a].min(n - mn);
                }
            }
        }
        Self { lo, len, full }
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        self.len[0] * self.len[1] * self.len[2]
    }

    #[inline]
    fn rel(&self, frame: &Frame, a: usize, base: usize) -> usize {
        if frame.planar(a) || self.full[a] {
            if frame.planar(a) { 0 } else { base }
        } else {
            base - self.lo[a]
        }
    }

    #[inline]
    fn wrap(&self, _frame: &Frame, a: usize, r: usize) -> usize {
        if self.full[a] { r % self.len[a] } else { r }
    }

    #[must_use]
    pub fn index_of(&self, frame: &Frame, cell: usize) -> Option<usize> {
        let c = frame.grid().coords(cell);
        let mut r = [0usize; 3];
        for a in 0..3 {
            r[a] = if frame.planar(a) || self.full[a] {
                if frame.planar(a) { 0 } else { c[a] }
            } else {
                let n = frame.shape[a];
                let t = if frame.periodic[a] {
                    (c[a] + n - self.lo[a]) % n
                } else {
                    c[a].wrapping_sub(self.lo[a])
                };
                if t >= self.len[a] {
                    return None;
                }
                t
            };
        }
        Some((r[0] * self.len[1] + r[1]) * self.len[2] + r[2])
    }

    #[must_use]
    pub fn lattice_cell(&self, frame: &Frame, index: usize) -> usize {
        let z = index % self.len[2];
        let y = (index / self.len[2]) % self.len[1];
        let x = index / (self.len[1] * self.len[2]);
        let c = [x, y, z];
        let g: [usize; 3] = std::array::from_fn(|a| (c[a] + self.lo[a]) % frame.shape[a]);
        frame.grid().index(g[0], g[1], g[2])
    }
}

#[must_use]
pub fn scatter<S: Scalar>(
    frame: &Frame,
    bx: &SupportBox,
    stencils: &Stencils<S>,
    a: &[S],
    v: Option<&[[S; 3]]>,
) -> (Vec<S>, Vec<[S; 3]>) {
    let cells = bx.cells();
    let mut d = vec![S::zero(); cells];
    let mut m = vec![[S::zero(); 3]; if v.is_some() { cells } else { 0 }];
    if cells == 0 || stencils.is_empty() {
        return (d, m);
    }
    let sup = [frame.support(0), frame.support(1), frame.support(2)];
    let width = (8 * frame.width_cells).max(sup[0]);
    let lx = bx.len[0];
    let slabs = lx.div_ceil(width).max(1);
    let plane = bx.len[1] * bx.len[2];
    let planes = width + sup[0] - 1;

    let mut counts = vec![0usize; slabs + 1];
    let slab_of: Vec<usize> =
        stencils.base.iter().map(|b| (bx.rel(frame, 0, b[0]) / width).min(slabs - 1)).collect();
    for &s in &slab_of {
        counts[s + 1] += 1;
    }
    for s in 0..slabs {
        counts[s + 1] += counts[s];
    }
    let mut fill = counts.clone();
    let mut order = vec![0usize; stencils.len()];
    for (q, &s) in slab_of.iter().enumerate() {
        order[fill[s]] = q;
        fill[s] += 1;
    }
    let with_v = v.is_some();
    let buffers: Vec<(Vec<S>, Vec<[S; 3]>)> = (0..slabs)
        .into_par_iter()
        .map(|sl| {
            let mut bd = vec![S::zero(); planes * plane];
            let mut bm = vec![[S::zero(); 3]; if with_v { planes * plane } else { 0 }];
            let x0 = sl * width;
            let mut ys = [0usize; MAX_SUPPORT];
            let mut zs = [0usize; MAX_SUPPORT];
            let mut wzs = [S::zero(); MAX_SUPPORT];
            for &q in &order[counts[sl]..counts[sl + 1]] {
                let b = stencils.base[q];
                let r = [bx.rel(frame, 0, b[0]), bx.rel(frame, 1, b[1]), bx.rel(frame, 2, b[2])];
                let aq = a[q];
                for t in 0..sup[1] {
                    ys[t] = bx.wrap(frame, 1, r[1] + t);
                }
                for t in 0..sup[2] {
                    zs[t] = bx.wrap(frame, 2, r[2] + t);
                    wzs[t] = stencils.weight(q, 2, t);
                }

                let straight = zs[sup[2] - 1] == zs[0] + sup[2] - 1;
                let vq = v.map(|v| v[q]);
                for tx in 0..sup[0] {
                    let xl = r[0] + tx - x0;
                    let wx = aq * stencils.weight(q, 0, tx);
                    for ty in 0..sup[1] {
                        let wxy = wx * stencils.weight(q, 1, ty);
                        let row = (xl * bx.len[1] + ys[ty]) * bx.len[2];
                        if straight {
                            let lo = row + zs[0];
                            let dz = &mut bd[lo..lo + sup[2]];
                            for (o, wz) in dz.iter_mut().zip(&wzs[..sup[2]]) {
                                *o += wxy * *wz;
                            }
                            if let Some(vq) = vq {
                                let mz = &mut bm[lo..lo + sup[2]];
                                for (e, wz) in mz.iter_mut().zip(&wzs[..sup[2]]) {
                                    let w = wxy * *wz;
                                    e[0] += w * vq[0];
                                    e[1] += w * vq[1];
                                    e[2] += w * vq[2];
                                }
                            }
                        } else {
                            for tz in 0..sup[2] {
                                let z = zs[tz];
                                let w = wxy * wzs[tz];
                                bd[row + z] += w;
                                if let Some(vq) = vq {
                                    let e = &mut bm[row + z];
                                    e[0] += w * vq[0];
                                    e[1] += w * vq[1];
                                    e[2] += w * vq[2];
                                }
                            }
                        }
                    }
                }
            }
            (bd, bm)
        })
        .collect();
    let periodic_x = bx.full[0];
    let sources = |p: usize| -> Vec<(usize, usize)> {

        let mut out = Vec::with_capacity(3);
        for sl in 0..slabs {
            let x0 = sl * width;
            let mut k = 0;
            loop {
                let target = p + k * lx;
                if target >= x0 {
                    let xl = target - x0;
                    if xl >= planes {
                        break;
                    }
                    out.push((sl, xl));
                }
                if !periodic_x {
                    break;
                }
                k += 1;
            }
        }
        out
    };
    d.par_chunks_mut(plane).enumerate().for_each(|(p, row)| {
        for (sl, xl) in sources(p) {
            let src = &buffers[sl].0[xl * plane..(xl + 1) * plane];
            for (o, s) in row.iter_mut().zip(src) {
                *o += *s;
            }
        }
    });
    if with_v {
        m.par_chunks_mut(plane).enumerate().for_each(|(p, row)| {
            for (sl, xl) in sources(p) {
                let src = &buffers[sl].1[xl * plane..(xl + 1) * plane];
                for (o, s) in row.iter_mut().zip(src) {
                    for e in 0..3 {
                        o[e] += s[e];
                    }
                }
            }
        });
    }
    (d, m)
}

#[must_use]
pub fn occupancy<S: Scalar>(frame: &Frame, ends: [&Endpoint<S>; 2], v: &[[S; 3]]) -> Occupancy<S> {
    let bx = SupportBox::of(frame, &[&ends[0].stencils, &ends[1].stencils]);
    let (d0, m0) = scatter(frame, &bx, &ends[0].stencils, &ends[0].points.a, Some(v));
    let (d1, m1) = scatter(frame, &bx, &ends[1].stencils, &ends[1].points.a, Some(v));
    let mut active: Vec<(usize, usize)> = (0..bx.cells())
        .filter(|&r| d0[r].value() > 0.0 || d1[r].value() > 0.0)
        .map(|r| (bx.lattice_cell(frame, r), r))
        .collect();
    active.sort_unstable();
    let mut cells = Vec::with_capacity(active.len());
    let mut d = [Vec::with_capacity(active.len()), Vec::with_capacity(active.len())];
    let mut m = [Vec::with_capacity(active.len()), Vec::with_capacity(active.len())];
    for &(c, r) in &active {
        #[allow(clippy::cast_possible_truncation)]
        cells.push(c as u32);
        d[0].push(d0[r]);
        d[1].push(d1[r]);
        m[0].push(m0[r]);
        m[1].push(m1[r]);
    }
    let mut slot = vec![NONE; frame.grid().cells()];
    for (r, &c) in cells.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        {
            slot[c as usize] = r as u32;
        }
    }
    Occupancy { cells, slot, d, m }
}

#[derive(Clone, Debug)]
pub struct PointBar<S> {
    pub xi: Vec<[S; 3]>,
    pub a: Vec<S>,
}

#[must_use]
pub fn endpoint_vjp<S: Scalar>(
    frame: &Frame,
    end: &Endpoint<S>,
    slot: &[u32],
    v: &[[S; 3]],
    d_bar: &[S],
    m_bar: &[[S; 3]],
    v_bar: &mut [[S; 3]],
) -> PointBar<S> {
    let g = point_gather(frame, &end.stencils, slot, Some(d_bar), Some(m_bar), true);
    let mut xi = vec![[S::zero(); 3]; g.len()];
    let mut a = vec![S::zero(); g.len()];
    for q in 0..g.len() {
        let aq = end.points.a[q];
        let vq = v[q];
        let pg = &g[q];
        a[q] = pg.ks + pg.kv[0] * vq[0] + pg.kv[1] * vq[1] + pg.kv[2] * vq[2];
        for d in 0..3 {
            xi[q][d] = aq * (pg.gs[d] + pg.gv[d][0] * vq[0] + pg.gv[d][1] * vq[1] + pg.gv[d][2] * vq[2]);
            v_bar[q][d] += aq * pg.kv[d];
        }
    }
    PointBar { xi, a }
}

#[must_use]
pub fn endpoint_impulses<S: Scalar>(
    frame: &Frame,
    end: &Endpoint<S>,
    slot: &[u32],
    g: &[[S; 3]],
) -> Vec<[S; 3]> {

    let bx = SupportBox::of(frame, &[&end.stencils]);
    let dense: Vec<[S; 3]> = (0..bx.cells())
        .into_par_iter()
        .map(|r| {
            let s = slot[bx.lattice_cell(frame, r)];
            if s == NONE { [S::zero(); 3] } else { g[s as usize] }
        })
        .collect();
    let sup = [frame.support(0), frame.support(1), frame.support(2)];
    let st = &end.stencils;
    (0..st.len())
        .into_par_iter()
        .map(|q| {
            let b = st.base[q];
            let r = [bx.rel(frame, 0, b[0]), bx.rel(frame, 1, b[1]), bx.rel(frame, 2, b[2])];
            let mut acc = [S::zero(); 3];
            let mut ys = [0usize; MAX_SUPPORT];
            let mut zs = [0usize; MAX_SUPPORT];
            let mut wzs = [S::zero(); MAX_SUPPORT];
            for t in 0..sup[1] {
                ys[t] = bx.wrap(frame, 1, r[1] + t);
            }
            for t in 0..sup[2] {
                zs[t] = bx.wrap(frame, 2, r[2] + t);
                wzs[t] = st.weight(q, 2, t);
            }
            for tx in 0..sup[0] {
                let x = bx.wrap(frame, 0, r[0] + tx);
                let wx = st.weight(q, 0, tx);
                for ty in 0..sup[1] {
                    let y = ys[ty];
                    let wxy = wx * st.weight(q, 1, ty);
                    let row = (x * bx.len[1] + y) * bx.len[2];
                    for tz in 0..sup[2] {
                        let z = zs[tz];
                        let w = wxy * wzs[tz];
                        let gv = dense[row + z];
                        acc[0] += w * gv[0];
                        acc[1] += w * gv[1];
                        acc[2] += w * gv[2];
                    }
                }
            }
            let a = end.points.a[q];
            [a * acc[0], a * acc[1], a * acc[2]]
        })
        .collect()
}

#[must_use]
pub fn endpoint_impulses_vjp<S: Scalar>(
    frame: &Frame,
    end: &Endpoint<S>,
    slot: &[u32],
    cells: &[u32],
    g: &[[S; 3]],
    i_bar: &[[S; 3]],
) -> (PointBar<S>, Vec<[S; 3]>) {

    let bx = SupportBox::of(frame, &[&end.stencils]);
    let (_, dense) = scatter(frame, &bx, &end.stencils, &end.points.a, Some(i_bar));
    let g_bar: Vec<[S; 3]> = cells
        .par_iter()
        .map(|&c| bx.index_of(frame, c as usize).map_or([S::zero(); 3], |r| dense[r]))
        .collect();
    let pg = point_gather(frame, &end.stencils, slot, None, Some(g), true);
    let mut xi = vec![[S::zero(); 3]; pg.len()];
    let mut a = vec![S::zero(); pg.len()];
    for q in 0..pg.len() {
        let ib = i_bar[q];
        a[q] = pg[q].kv[0] * ib[0] + pg[q].kv[1] * ib[1] + pg[q].kv[2] * ib[2];
        for d in 0..3 {
            xi[q][d] =
                end.points.a[q] * (pg[q].gv[d][0] * ib[0] + pg[q].gv[d][1] * ib[1] + pg[q].gv[d][2] * ib[2]);
        }
    }
    (PointBar { xi, a }, g_bar)
}

