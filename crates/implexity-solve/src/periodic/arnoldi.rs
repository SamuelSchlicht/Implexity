// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

use super::krylov::{dot, norm};
use crate::complex_spectral::{ComplexMatrix, c64};

pub const MIN_BASIS: usize = 26;
pub const STALL_WINDOW: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArnoldiOptions {
    pub wanted: usize,
    pub basis: Option<usize>,
    pub tolerance: f64,
    pub max_restarts: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RitzPair {
    pub value: c64,
    pub vector_re: Vec<f64>,
    pub vector_im: Vec<f64>,
    pub residual: f64,
}

#[derive(Clone, Debug)]
pub struct ArnoldiResult {
    pub pairs: Vec<RitzPair>,
    pub ritz_values: Vec<c64>,
    pub projected: DenseMatrix,
    pub products: usize,
    pub restarts: usize,
    pub invariant: bool,
    pub basis: usize,
}

pub(crate) fn modulus_order(values: &[c64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (values[a], values[b]);
        y.norm().total_cmp(&x.norm()).then(y.im.total_cmp(&x.im)).then(y.re.total_cmp(&x.re))
    });
    order
}

pub(crate) fn deterministic_vector(n: usize, seed: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let t = (i as f64 + 1.0) * 0.618_033_988_749_894_9 + seed as f64 * 0.414_213_562_373_095_1;
            1.0 + 0.5 * (7.3 * t).sin() + 0.25 * (2.9 * t + 0.7).cos()
        })
        .collect()
}

pub(crate) fn hashed_vector(n: usize, seed: usize) -> Vec<f64> {
    let mix = |mut z: u64| {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    #[allow(clippy::cast_precision_loss)]
    let scale = 1.0 / (1u64 << 53) as f64;
    (0..n)
        .map(|i| {
            let z = mix(mix(seed as u64) ^ (i as u64));
            #[allow(clippy::cast_precision_loss)]
            let u = (z >> 11) as f64 * scale;
            2.0 * u - 1.0
        })
        .collect()
}

fn orthogonalize(w: &mut [f64], basis: &[Vec<f64>]) -> Vec<f64> {
    let mut h = vec![0.0; basis.len()];
    for _pass in 0..2 {
        let coefficients: Vec<f64> = basis.iter().map(|v| dot(w, v)).collect();
        for (c, v) in coefficients.iter().zip(basis) {
            for (wk, vk) in w.iter_mut().zip(v) {
                *wk -= c * vk;
            }
        }
        for (hi, c) in h.iter_mut().zip(&coefficients) {
            *hi += c;
        }
    }
    h
}

struct Factorization {
    n: usize,
    m: usize,
    basis: Vec<Vec<f64>>,
    h: Vec<f64>,
    residual: Vec<f64>,
    products: usize,
    seeds: usize,
    invariant: bool,
}

impl Factorization {
    fn h(&self, i: usize, j: usize) -> f64 {
        self.h[i * self.m + j]
    }

    fn h_norm(&self) -> f64 {
        self.h.iter().map(|v| v * v).sum::<f64>().sqrt()
    }

    fn fresh_vector(&mut self) -> CaeResult<Vec<f64>> {
        for attempt in 0..8 {
            self.seeds += 1;

            let mut v = if attempt == 0 {
                deterministic_vector(self.n, self.seeds)
            } else {
                hashed_vector(self.n, self.seeds)
            };
            let before = norm(&v);
            orthogonalize(&mut v, &self.basis);
            let after = norm(&v);
            if after > 1e-8 * before {
                return Ok(v.into_iter().map(|x| x / after).collect());
            }
        }
        Err(CaeError::convergence("Arnoldi could not extend its basis with an independent vector"))
    }

    fn extend<F>(&mut self, matvec: &mut F) -> CaeResult<()>
    where
        F: FnMut(&[f64]) -> CaeResult<Vec<f64>>,
    {
        while self.basis.len() < self.m {
            let j = self.basis.len() - 1;
            let mut w = matvec(&self.basis[j])?;
            self.products += 1;
            if w.len() != self.n || !w.iter().all(|v| v.is_finite()) {
                return Err(CaeError::convergence(
                    "Arnoldi operator product is not a finite vector of the order",
                ));
            }
            let coefficients = orthogonalize(&mut w, &self.basis);
            for (i, c) in coefficients.iter().enumerate() {
                self.h[i * self.m + j] = *c;
            }
            let beta = norm(&w);
            let scale = coefficients.iter().map(|c| c.abs()).fold(beta, f64::max);
            if beta <= 64.0 * f64::EPSILON * scale || beta == 0.0 {

                self.h[(j + 1) * self.m + j] = 0.0;
                let v = self.fresh_vector()?;
                self.basis.push(v);
            } else {
                self.h[(j + 1) * self.m + j] = beta;
                self.basis.push(w.into_iter().map(|x| x / beta).collect());
            }
        }
        let j = self.m - 1;
        let mut w = matvec(&self.basis[j])?;
        self.products += 1;
        if w.len() != self.n || !w.iter().all(|v| v.is_finite()) {
            return Err(CaeError::convergence(
                "Arnoldi operator product is not a finite vector of the order",
            ));
        }
        let coefficients = orthogonalize(&mut w, &self.basis);
        for (i, c) in coefficients.iter().enumerate() {
            self.h[i * self.m + j] = *c;
        }
        let beta = norm(&w);
        let scale = coefficients.iter().map(|c| c.abs()).fold(beta, f64::max);
        if self.m == self.n || beta <= 64.0 * f64::EPSILON * scale {
            self.invariant = true;
            self.residual = vec![0.0; self.n];
        } else {
            self.invariant = false;
            self.residual = w;
        }
        Ok(())
    }

    fn apply_rotation_rows(&mut self, k: usize, c: f64, s: f64, from: usize) {
        let m = self.m;
        for col in from..m {
            let (a, b) = (self.h[k * m + col], self.h[(k + 1) * m + col]);
            self.h[k * m + col] = c * a + s * b;
            self.h[(k + 1) * m + col] = -s * a + c * b;
        }
    }

    fn apply_rotation_cols(&mut self, k: usize, c: f64, s: f64, to: usize, q: &mut [f64]) {
        let m = self.m;
        for row in 0..=to.min(m - 1) {
            let (a, b) = (self.h[row * m + k], self.h[row * m + k + 1]);
            self.h[row * m + k] = c * a + s * b;
            self.h[row * m + k + 1] = -s * a + c * b;
        }
        for row in 0..m {
            let (a, b) = (q[row * m + k], q[row * m + k + 1]);
            q[row * m + k] = c * a + s * b;
            q[row * m + k + 1] = -s * a + c * b;
        }
    }

    fn real_shift(&mut self, mu: f64, q: &mut [f64]) {
        let m = self.m;
        let mut x = self.h(0, 0) - mu;
        let mut y = self.h(1, 0);
        for k in 0..m - 1 {
            let (c, s) = if y == 0.0 {
                (1.0, 0.0)
            } else {
                let r = x.hypot(y);
                (x / r, y / r)
            };
            self.apply_rotation_rows(k, c, s, k.saturating_sub(1));
            self.apply_rotation_cols(k, c, s, k + 2, q);
            if k > 0 {
                self.h[(k + 1) * m + k - 1] = 0.0;
            }
            if k + 2 < m {
                x = self.h(k + 1, k);
                y = self.h(k + 2, k);
            }
        }
    }

    fn double_shift(&mut self, mu: c64, q: &mut [f64]) {
        let m = self.m;
        if m < 3 {

            self.real_shift(mu.re, q);
            return;
        }
        let s = 2.0 * mu.re;
        let t = mu.norm_sqr();
        let mut x = self.h(0, 0) * self.h(0, 0) + self.h(0, 1) * self.h(1, 0) - s * self.h(0, 0) + t;
        let mut y = self.h(1, 0) * (self.h(0, 0) + self.h(1, 1) - s);
        let mut z = self.h(1, 0) * self.h(2, 1);
        for k in 0..m - 2 {
            let alpha = (x * x + y * y + z * z).sqrt();
            if alpha > 0.0 {
                let sign = if x >= 0.0 { 1.0 } else { -1.0 };
                let v = [x + sign * alpha, y, z];
                let vv = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
                let from = k.saturating_sub(1);
                for col in from..m {
                    let d = v[0] * self.h[k * m + col]
                        + v[1] * self.h[(k + 1) * m + col]
                        + v[2] * self.h[(k + 2) * m + col];
                    let f = 2.0 * d / vv;
                    for (r, vr) in v.iter().enumerate() {
                        self.h[(k + r) * m + col] -= f * vr;
                    }
                }
                let to = (k + 3).min(m - 1);
                for row in 0..=to {
                    let d = self.h[row * m + k] * v[0]
                        + self.h[row * m + k + 1] * v[1]
                        + self.h[row * m + k + 2] * v[2];
                    let f = 2.0 * d / vv;
                    for (r, vr) in v.iter().enumerate() {
                        self.h[row * m + k + r] -= f * vr;
                    }
                }
                for row in 0..m {
                    let d = q[row * m + k] * v[0] + q[row * m + k + 1] * v[1] + q[row * m + k + 2] * v[2];
                    let f = 2.0 * d / vv;
                    for (r, vr) in v.iter().enumerate() {
                        q[row * m + k + r] -= f * vr;
                    }
                }
                if k > 0 {
                    self.h[(k + 1) * m + k - 1] = 0.0;
                    self.h[(k + 2) * m + k - 1] = 0.0;
                }
            }
            x = self.h(k + 1, k);
            y = self.h(k + 2, k);
            if k + 3 < m {
                z = self.h(k + 3, k);
            }
        }

        let k = m - 2;
        let (c, sn) = if y == 0.0 {
            (1.0, 0.0)
        } else {
            let r = x.hypot(y);
            (x / r, y / r)
        };
        self.apply_rotation_rows(k, c, sn, k.saturating_sub(1));
        self.apply_rotation_cols(k, c, sn, m - 1, q);
        if k > 0 {
            self.h[(k + 1) * m + k - 1] = 0.0;
        }

        for i in 2..m {
            for j in 0..i - 1 {
                self.h[i * m + j] = 0.0;
            }
        }
    }

    fn grow(&mut self, m_new: usize) {
        let (m, rows) = (self.m, self.basis.len());
        let mut h = vec![0.0; m_new * m_new];
        for i in 0..rows.min(m) {
            h[i * m_new..i * m_new + m].copy_from_slice(&self.h[i * m..i * m + m]);
        }
        self.h = h;
        self.m = m_new;
    }

    fn restart(&mut self, k: usize, shifts: &[c64]) -> CaeResult<()> {
        let m = self.m;
        let mut q = vec![0.0; m * m];
        for i in 0..m {
            q[i * m + i] = 1.0;
        }
        let mut applied = 0usize;
        let budget = m - k;
        let mut index = 0;
        while index < shifts.len() {
            let mu = shifts[index];
            let real = mu.im.abs() <= 1e-12 * mu.norm().max(1e-300);
            if real {
                if applied + 1 > budget {
                    break;
                }
                self.real_shift(mu.re, &mut q);
                applied += 1;
                index += 1;
            } else {
                if applied + 2 > budget {
                    break;
                }
                self.double_shift(mu, &mut q);
                applied += 2;

                index += if shifts.get(index + 1).is_some_and(|p| (*p - mu.conj()).norm() <= 1e-8 * mu.norm())
                {
                    2
                } else {
                    1
                };
            }
        }
        let beta_k = self.h(k, k - 1);
        let sigma = q[(m - 1) * m + k - 1];
        let n = self.n;
        let mut new_basis = Vec::with_capacity(m);
        for col in 0..=k {
            let mut v = vec![0.0; n];
            for (row, b) in self.basis.iter().enumerate() {
                let c = q[row * m + col];
                if c != 0.0 {
                    for (vi, bi) in v.iter_mut().zip(b) {
                        *vi += c * bi;
                    }
                }
            }
            new_basis.push(v);
        }
        let mut f: Vec<f64> = new_basis[k].iter().map(|v| v * beta_k).collect();
        for (fi, ri) in f.iter_mut().zip(&self.residual) {
            *fi += sigma * ri;
        }
        new_basis.truncate(k);
        let mut h = vec![0.0; m * m];
        for i in 0..k {
            for j in 0..k {
                h[i * m + j] = self.h(i, j);
            }
        }
        self.h = h;
        self.basis = new_basis;

        orthogonalize(&mut f, &self.basis);
        let beta = norm(&f);
        let scale = self.h_norm().max(f64::MIN_POSITIVE);
        if beta <= 64.0 * f64::EPSILON * scale || beta == 0.0 {
            self.h[k * m + k - 1] = 0.0;
            let v = self.fresh_vector()?;
            self.basis.push(v);
        } else {
            self.h[k * m + k - 1] = beta;
            self.basis.push(f.into_iter().map(|x| x / beta).collect());
        }
        Ok(())
    }
}

struct RitzSystem {
    values: Vec<c64>,
    vectors: ComplexMatrix,
    order: Vec<usize>,
}



pub(crate) fn real_eigen(a: &DenseMatrix) -> CaeResult<(Vec<c64>, ComplexMatrix)> {
    let e = implexity_linalg::dense::eig(a)
        .map_err(|e| CaeError::convergence(format!("small eigenproblem failed: {e}")))?;
    let n = e.n;
    Ok((e.values, ComplexMatrix { nrows: n, ncols: n, data: e.right }))
}

fn ritz(f: &Factorization) -> CaeResult<RitzSystem> {
    let m = f.m;
    let h = DenseMatrix::new(m, m, f.h.clone())
        .map_err(|e| CaeError::contract(format!("Arnoldi projected matrix: {e}")))?;
    let (values, vectors) = real_eigen(&h)?;
    let order = modulus_order(&values);
    Ok(RitzSystem { values, vectors, order })
}

fn wanted_count(values: &[c64], order: &[usize], wanted: usize) -> usize {
    if wanted >= order.len() {
        return order.len();
    }
    let last = values[order[wanted - 1]];
    let next = values[order[wanted]];
    let pair =
        last.im.abs() > 1e-12 * last.norm() && (next - last.conj()).norm() <= 1e-8 * last.norm().max(1e-300);
    if pair { wanted + 1 } else { wanted }
}



#[allow(clippy::too_many_lines)]
pub fn dominant_eigenpairs<F>(
    n: usize,
    options: &ArnoldiOptions,
    start: Option<&[f64]>,
    mut matvec: F,
) -> CaeResult<ArnoldiResult>
where
    F: FnMut(&[f64]) -> CaeResult<Vec<f64>>,
{
    if n == 0 || options.wanted == 0 || options.wanted > n {
        return Err(CaeError::contract(format!(
            "Arnoldi needs 1 ≤ wanted ≤ order (wanted {}, order {n})",
            options.wanted
        )));
    }
    if !(options.tolerance.is_finite() && options.tolerance > 0.0) {
        return Err(CaeError::contract("Arnoldi tolerance must be finite and positive"));
    }
    let m = options.basis.unwrap_or((2 * options.wanted + 10).max(MIN_BASIS)).max(options.wanted + 2).min(n);
    let max_basis = (4 * m).max(64).min(n);
    let mut v0 = match start {
        Some(s) if s.len() == n && s.iter().all(|v| v.is_finite()) && norm(s) > 0.0 => s.to_vec(),
        Some(_) => {
            return Err(CaeError::contract(
                "Arnoldi start vector must be a finite nonzero vector of the order",
            ));
        }
        None => deterministic_vector(n, 0),
    };
    let nv = norm(&v0);
    for v in &mut v0 {
        *v /= nv;
    }
    let mut f = Factorization {
        n,
        m,
        basis: vec![v0],
        h: vec![0.0; m * m],
        residual: vec![0.0; n],
        products: 0,
        seeds: 0,
        invariant: false,
    };
    f.extend(&mut matvec)?;
    let mut restarts = 0usize;

    let mut window_start = f64::INFINITY;
    loop {
        let m = f.m;
        let system = ritz(&f)?;
        let k = wanted_count(&system.values, &system.order, options.wanted);
        let beta = norm(&f.residual);
        let h_norm = f.h_norm();
        let floor = f64::EPSILON.powf(2.0 / 3.0) * h_norm;
        let residual_of = |idx: usize| beta * system.vectors.get(m - 1, idx).norm();
        let converged = f.invariant
            || system.order[..k]
                .iter()
                .all(|&i| residual_of(i) <= options.tolerance * system.values[i].norm().max(floor));
        if converged || restarts >= options.max_restarts {
            if !converged {
                let worst = system.order[..k]
                    .iter()
                    .map(|&i| residual_of(i) / system.values[i].norm().max(floor))
                    .fold(0.0, f64::max);
                return Err(CaeError::convergence(format!(
                    "Arnoldi did not converge: relative Ritz residual {worst:.3e} above {:.3e} after {restarts} restarts (basis {m})",
                    options.tolerance
                )));
            }
            let mut pairs = Vec::with_capacity(k);
            for &i in &system.order[..k] {
                let mut re = vec![0.0; n];
                let mut im = vec![0.0; n];
                for (row, b) in f.basis.iter().enumerate() {
                    let y = system.vectors.get(row, i);
                    for ((r, s), bi) in re.iter_mut().zip(im.iter_mut()).zip(b) {
                        *r += y.re * bi;
                        *s += y.im * bi;
                    }
                }
                normalize_complex(&mut re, &mut im);
                pairs.push(RitzPair {
                    value: system.values[i],
                    vector_re: re,
                    vector_im: im,
                    residual: residual_of(i),
                });
            }
            let projected = DenseMatrix::new(m, m, f.h.clone())
                .map_err(|e| CaeError::contract(format!("Arnoldi projected matrix: {e}")))?;
            return Ok(ArnoldiResult {
                pairs,
                ritz_values: system.order.iter().map(|&i| system.values[i]).collect(),
                projected,
                products: f.products,
                restarts,
                invariant: f.invariant,
                basis: f.m,
            });
        }
        let worst = system.order[..k]
            .iter()
            .map(|&i| residual_of(i) / system.values[i].norm().max(floor))
            .fold(0.0, f64::max);
        let shifts: Vec<c64> = system.order[k..].iter().map(|&i| system.values[i]).collect();
        f.restart(k, &shifts)?;
        if restarts.is_multiple_of(STALL_WINDOW) {
            if restarts > 0 && worst > 0.5 * window_start && f.m < max_basis {
                f.grow((f.m + (f.m / 2).max(10)).min(max_basis));
            }
            window_start = worst;
        }
        f.extend(&mut matvec)?;
        restarts += 1;
    }
}

pub(crate) fn normalize_complex(re: &mut [f64], im: &mut [f64]) {
    let total = (dot(re, re) + dot(im, im)).sqrt();
    if total == 0.0 {
        return;
    }
    let mut big = 0;
    let mut best = -1.0;
    for i in 0..re.len() {
        let a = re[i].hypot(im[i]);
        if a > best {
            best = a;
            big = i;
        }
    }
    let phase = c64::new(re[big], -im[big]) / best;
    for i in 0..re.len() {
        let v = c64::new(re[i], im[i]) * phase / total;
        re[i] = v.re;
        im[i] = v.im;
    }
}

