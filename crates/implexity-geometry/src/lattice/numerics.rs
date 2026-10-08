// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use crate::lattice::config::LatticeConfig;
use crate::scalar::Scalar;

#[derive(Clone, Debug, PartialEq)]
pub struct Grids {
    pub n: [usize; 3],
    pub h: f64,
    pub nc: [usize; 3],
    pub ci: [Vec<f64>; 3],
    pub xc: [Vec<f64>; 3],
    pub blur_radius: usize,
    pub k0: f64,
    pub spacing: [f64; 3],
}

impl Grids {
    #[must_use]
    pub fn cells(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }

    #[must_use]
    pub fn n_control(&self) -> usize {
        self.nc[0] * self.nc[1] * self.nc[2]
    }
}

fn py_round(x: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let r = x.round_ties_even() as i64;
    r
}

#[must_use]
pub fn build_grids(n: [usize; 3], spacing: [f64; 3], lat: &LatticeConfig) -> Grids {
    let h = spacing[0].min(spacing[1]).min(spacing[2]);
    #[allow(clippy::cast_precision_loss)]
    let lengths: [f64; 3] = std::array::from_fn(|a| n[a] as f64 * spacing[a]);
    let nc: [usize; 3] = std::array::from_fn(|a| {
        let v = py_round(lengths[a] / lat.control_dx[a]) + 1;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let v = v.max(2) as usize;
        v
    });
    let cells = n[0] * n[1] * n[2];
    let mut xc: [Vec<f64>; 3] =
        [Vec::with_capacity(cells), Vec::with_capacity(cells), Vec::with_capacity(cells)];
    let mut ci: [Vec<f64>; 3] =
        [Vec::with_capacity(cells), Vec::with_capacity(cells), Vec::with_capacity(cells)];
    for i in 0..n[0] {
        for j in 0..n[1] {
            for k in 0..n[2] {
                #[allow(clippy::cast_precision_loss)]
                let idx = [i as f64, j as f64, k as f64];
                for a in 0..3 {
                    let x = (idx[a] + 0.5) * spacing[a];
                    xc[a].push(x);
                    #[allow(clippy::cast_precision_loss)]
                    ci[a].push(x / lengths[a] * (nc[a] - 1) as f64);
                }
            }
        }
    }
    let blur = py_round(lat.period / (4.0 * h)).max(1);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let blur_radius = blur as usize;
    Grids { n, h, nc, ci, xc, blur_radius, k0: 2.0 * std::f64::consts::PI / lat.period, spacing }
}

#[derive(Clone, Debug)]
pub struct Interp {
    nc: [usize; 3],
    base: Vec<[usize; 3]>,
    frac: Vec<[f64; 3]>,
}

impl Interp {
    #[must_use]
    pub fn new(nc: [usize; 3], ci: &[Vec<f64>; 3]) -> Self {
        let cells = ci[0].len();
        let mut base = Vec::with_capacity(cells);
        let mut frac = Vec::with_capacity(cells);
        for c in 0..cells {
            let mut b = [0usize; 3];
            let mut f = [0.0; 3];
            for a in 0..3 {
                #[allow(clippy::cast_precision_loss)]
                let v = ci[a][c].clamp(0.0, (nc[a] - 1) as f64);
                #[allow(clippy::cast_precision_loss)]
                let lo = v.floor().clamp(0.0, (nc[a] - 2) as f64);
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let lo_i = lo as usize;
                b[a] = lo_i;
                f[a] = v - lo;
            }
            base.push(b);
            frac.push(f);
        }
        Self { nc, base, frac }
    }

    fn at(&self, x: usize, y: usize, z: usize) -> usize {
        (x * self.nc[1] + y) * self.nc[2] + z
    }

    #[must_use]
    pub fn forward(&self, f: &[f64]) -> Vec<f64> {
        self.base
            .iter()
            .zip(&self.frac)
            .map(|(b, fr)| {
                let (x0, y0, z0) = (b[0], b[1], b[2]);
                let (x1, y1, z1) = (x0 + 1, y0 + 1, z0 + 1);
                let (fx, fy, fz) = (fr[0], fr[1], fr[2]);
                let g = |a, bb, c| f[self.at(a, bb, c)];
                let c00 = g(x0, y0, z0) * (1.0 - fx) + g(x1, y0, z0) * fx;
                let c01 = g(x0, y0, z1) * (1.0 - fx) + g(x1, y0, z1) * fx;
                let c10 = g(x0, y1, z0) * (1.0 - fx) + g(x1, y1, z0) * fx;
                let c11 = g(x0, y1, z1) * (1.0 - fx) + g(x1, y1, z1) * fx;
                let c0 = c00 * (1.0 - fy) + c10 * fy;
                let c1 = c01 * (1.0 - fy) + c11 * fy;
                c0 * (1.0 - fz) + c1 * fz
            })
            .collect()
    }

    #[must_use]
    pub fn transpose(&self, bar: &[f64]) -> Vec<f64> {
        let mut out = vec![0.0; self.nc[0] * self.nc[1] * self.nc[2]];
        for ((b, fr), g) in self.base.iter().zip(&self.frac).zip(bar) {
            if *g == 0.0 {
                continue;
            }
            let (fx, fy, fz) = (fr[0], fr[1], fr[2]);
            for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                    for (dz, wz) in [(0, 1.0 - fz), (1, fz)] {
                        out[self.at(b[0] + dx, b[1] + dy, b[2] + dz)] += g * wx * wy * wz;
                    }
                }
            }
        }
        out
    }
}

pub fn trilerp_at<S: Scalar>(f: &[S], shape: [usize; 3], ci: [S; 3]) -> S {
    let mut lo = [0usize; 3];
    let mut fr = [S::cst(0.0); 3];
    for a in 0..3 {
        #[allow(clippy::cast_precision_loss)]
        let c = ci[a].clip_c(0.0, shape[a] as f64 - 1.0);
        #[allow(clippy::cast_precision_loss)]
        let l = c.val().floor().clamp(0.0, shape[a] as f64 - 2.0);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let li = l as usize;
        lo[a] = li;
        fr[a] = c - l;
    }
    let at = |x: usize, y: usize, z: usize| f[(x * shape[1] + y) * shape[2] + z];
    let (x0, y0, z0) = (lo[0], lo[1], lo[2]);
    let (x1, y1, z1) = (x0 + 1, y0 + 1, z0 + 1);
    let (fx, fy, fz) = (fr[0], fr[1], fr[2]);
    let one = S::cst(1.0);
    let c00 = at(x0, y0, z0) * (one - fx) + at(x1, y0, z0) * fx;
    let c01 = at(x0, y0, z1) * (one - fx) + at(x1, y0, z1) * fx;
    let c10 = at(x0, y1, z0) * (one - fx) + at(x1, y1, z0) * fx;
    let c11 = at(x0, y1, z1) * (one - fx) + at(x1, y1, z1) * fx;
    let c0 = c00 * (one - fy) + c10 * fy;
    let c1 = c01 * (one - fy) + c11 * fy;
    c0 * (one - fz) + c1 * fz
}

pub fn trilerp_const<S: Scalar>(f: &[f64], shape: [usize; 3], ci: [S; 3]) -> S {
    let mut lo = [0usize; 3];
    let mut fr = [S::cst(0.0); 3];
    for a in 0..3 {
        #[allow(clippy::cast_precision_loss)]
        let c = ci[a].clip_c(0.0, shape[a] as f64 - 1.0);
        #[allow(clippy::cast_precision_loss)]
        let l = c.val().floor().clamp(0.0, shape[a] as f64 - 2.0);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let li = l as usize;
        lo[a] = li;
        fr[a] = c - l;
    }
    let at = |x: usize, y: usize, z: usize| f[(x * shape[1] + y) * shape[2] + z];
    let (x0, y0, z0) = (lo[0], lo[1], lo[2]);
    let (x1, y1, z1) = (x0 + 1, y0 + 1, z0 + 1);
    let (fx, fy, fz) = (fr[0], fr[1], fr[2]);
    let one = S::cst(1.0);
    let c00 = (one - fx) * at(x0, y0, z0) + fx * at(x1, y0, z0);
    let c01 = (one - fx) * at(x0, y0, z1) + fx * at(x1, y0, z1);
    let c10 = (one - fx) * at(x0, y1, z0) + fx * at(x1, y1, z0);
    let c11 = (one - fx) * at(x0, y1, z1) + fx * at(x1, y1, z1);
    let c0 = c00 * (one - fy) + c10 * fy;
    let c1 = c01 * (one - fy) + c11 * fy;
    c0 * (one - fz) + c1 * fz
}

fn idx(n: [usize; 3], i: usize, j: usize, k: usize) -> usize {
    (i * n[1] + j) * n[2] + k
}

#[must_use]
pub fn grad_center(f: &[f64], n: [usize; 3], spacing: [f64; 3], axis: usize) -> Vec<f64> {
    use rayon::prelude::*;
    let axis = axis.min(2);
    let den = 2.0 * spacing[axis];
    let mut out = vec![0.0; f.len()];
    let plane = n[1] * n[2];
    if plane == 0 {
        return out;
    }
    let [n0, n1, n2] = n;

    out.par_chunks_mut(plane).take(n0).enumerate().for_each(|(i, row)| match axis {
        0 => {
            let (ih, il) = ((i + 1).min(n0 - 1), i.saturating_sub(1));
            let (fh, fl) = (&f[ih * plane..(ih + 1) * plane], &f[il * plane..(il + 1) * plane]);
            for ((o, a), b) in row.iter_mut().zip(fh).zip(fl) {
                *o = (a - b) / den;
            }
        }
        1 => {
            for j in 0..n1 {
                let (jh, jl) = ((j + 1).min(n1 - 1), j.saturating_sub(1));
                let fh = &f[i * plane + jh * n2..i * plane + (jh + 1) * n2];
                let fl = &f[i * plane + jl * n2..i * plane + (jl + 1) * n2];
                for ((o, a), b) in row[j * n2..(j + 1) * n2].iter_mut().zip(fh).zip(fl) {
                    *o = (a - b) / den;
                }
            }
        }
        _ => {
            for j in 0..n1 {
                let r = &f[i * plane + j * n2..i * plane + (j + 1) * n2];
                for (k, o) in row[j * n2..(j + 1) * n2].iter_mut().enumerate() {
                    *o = (r[(k + 1).min(n2 - 1)] - r[k.saturating_sub(1)]) / den;
                }
            }
        }
    });
    out
}

pub fn grad_center_t(bar: &[f64], n: [usize; 3], spacing: [f64; 3], axis: usize, acc: &mut [f64]) {
    let den = 2.0 * spacing[axis];
    for i in 0..n[0] {
        for j in 0..n[1] {
            for k in 0..n[2] {
                let g = bar[idx(n, i, j, k)] / den;
                if g == 0.0 {
                    continue;
                }
                let p = [i, j, k];
                let mut hi = p;
                let mut lo = p;
                hi[axis] = (p[axis] + 1).min(n[axis] - 1);
                lo[axis] = p[axis].saturating_sub(1);
                acc[idx(n, hi[0], hi[1], hi[2])] += g;
                acc[idx(n, lo[0], lo[1], lo[2])] -= g;
            }
        }
    }
}

#[must_use]
pub fn box_blur3(f: &[f64], n: [usize; 3], r: usize) -> Vec<f64> {
    if r == 0 {
        return f.to_vec();
    }
    let w = 2 * r + 1;
    let p = [n[0] + 2 * r, n[1] + 2 * r, n[2] + 2 * r];

    let mut s = vec![0.0; p[0] * p[1] * p[2]];
    for i in 0..p[0] {
        let si = (i.saturating_sub(r)).min(n[0] - 1);
        for j in 0..p[1] {
            let sj = (j.saturating_sub(r)).min(n[1] - 1);
            for k in 0..p[2] {
                let sk = (k.saturating_sub(r)).min(n[2] - 1);
                s[idx(p, i, j, k)] = f[idx(n, si, sj, sk)];
            }
        }
    }

    for i in 1..p[0] {
        for j in 0..p[1] {
            for k in 0..p[2] {
                s[idx(p, i, j, k)] += s[idx(p, i - 1, j, k)];
            }
        }
    }
    for i in 0..p[0] {
        for j in 1..p[1] {
            for k in 0..p[2] {
                s[idx(p, i, j, k)] += s[idx(p, i, j - 1, k)];
            }
        }
    }
    for i in 0..p[0] {
        for j in 0..p[1] {
            for k in 1..p[2] {
                s[idx(p, i, j, k)] += s[idx(p, i, j, k - 1)];
            }
        }
    }
    let sp = |i: usize, j: usize, k: usize| -> f64 {
        if i == 0 || j == 0 || k == 0 { 0.0 } else { s[idx(p, i - 1, j - 1, k - 1)] }
    };
    #[allow(clippy::cast_precision_loss)]
    let vol = (w * w * w) as f64;
    let mut out = vec![0.0; f.len()];
    for i in 0..n[0] {
        for j in 0..n[1] {
            for k in 0..n[2] {
                let (a1, b1, c1) = (i + w, j + w, k + w);
                let (a0, b0, c0) = (i, j, k);
                let win = sp(a1, b1, c1) - sp(a0, b1, c1) - sp(a1, b0, c1) - sp(a1, b1, c0)
                    + sp(a0, b0, c1)
                    + sp(a0, b1, c0)
                    + sp(a1, b0, c0)
                    - sp(a0, b0, c0);
                out[idx(n, i, j, k)] = win / vol;
            }
        }
    }
    out
}

#[must_use]
pub fn box_blur3_t(bar: &[f64], n: [usize; 3], r: usize) -> Vec<f64> {
    if r == 0 {
        return bar.to_vec();
    }
    let w = 2 * r + 1;
    #[allow(clippy::cast_precision_loss)]
    let vol = (w * w * w) as f64;

    let mut cur = bar.iter().map(|b| b / vol).collect::<Vec<f64>>();
    for axis in 0..3 {
        let mut next = vec![0.0; cur.len()];
        for i in 0..n[0] {
            for j in 0..n[1] {
                for k in 0..n[2] {
                    let g = cur[idx(n, i, j, k)];
                    if g == 0.0 {
                        continue;
                    }
                    let p = [i, j, k];
                    for d in 0..w {
                        #[allow(clippy::cast_possible_wrap)]
                        let s = p[axis] as isize + d as isize - r as isize;
                        #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
                        let s = s.clamp(0, n[axis] as isize - 1) as usize;
                        let mut q = p;
                        q[axis] = s;
                        next[idx(n, q[0], q[1], q[2])] += g;
                    }
                }
            }
        }
        cur = next;
    }
    cur
}

#[must_use]
pub fn cumintegrate(f: &[f64], n: [usize; 3], h: f64, axis: usize) -> Vec<f64> {
    let mut total = f.to_vec();
    for i in 0..n[0] {
        for j in 0..n[1] {
            for k in 0..n[2] {
                let p = [i, j, k];
                if p[axis] == 0 {
                    continue;
                }
                let mut q = p;
                q[axis] -= 1;
                total[idx(n, i, j, k)] += total[idx(n, q[0], q[1], q[2])];
            }
        }
    }
    total.iter().zip(f).map(|(t, x)| h * (t - 0.5 * x)).collect()
}

#[must_use]
pub fn cumintegrate_t(bar: &[f64], n: [usize; 3], h: f64, axis: usize) -> Vec<f64> {
    let mut rev = bar.to_vec();
    for i in (0..n[0]).rev() {
        for j in (0..n[1]).rev() {
            for k in (0..n[2]).rev() {
                let p = [i, j, k];
                if p[axis] + 1 >= n[axis] {
                    continue;
                }
                let mut q = p;
                q[axis] += 1;
                rev[idx(n, i, j, k)] += rev[idx(n, q[0], q[1], q[2])];
            }
        }
    }
    rev.iter().zip(bar).map(|(s, b)| h * (s - 0.5 * b)).collect()
}

