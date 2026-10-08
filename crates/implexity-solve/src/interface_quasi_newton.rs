// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

fn axpy(alpha: f64, x: &[f64], y: &mut [f64]) {
    for (yi, xi) in y.iter_mut().zip(x) {
        *yi += alpha * xi;
    }
}

fn check_finite(label: &str, v: &[f64], size: usize) -> CaeResult<()> {
    if v.len() != size {
        return Err(err(format!("{label}: expected length {size}, got {}", v.len())));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(err(format!("{label}: values must be finite")));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct Pair {
    image: Vec<f64>,
    correction: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
struct Basis {
    q: Vec<Vec<f64>>,
    r: Vec<f64>,
    kept: Vec<usize>,
}

impl Basis {
    fn rank(&self) -> usize {
        self.q.len()
    }

    fn coefficients(&self, x: &[f64]) -> Vec<f64> {
        let k = self.rank();
        let mut c: Vec<f64> = self.q.iter().map(|q| dot(q, x)).collect();
        for i in (0..k).rev() {
            let row = &self.r[i * k..(i + 1) * k];
            let tail: f64 = row[i + 1..].iter().zip(&c[i + 1..]).map(|(r, cj)| r * cj).sum();
            c[i] = (c[i] - tail) / row[i];
        }
        c
    }

    fn transpose_combination(&self, z: &[f64], size: usize) -> Vec<f64> {
        let k = self.rank();
        let mut w = z.to_vec();
        for i in 0..k {
            let head: f64 = w[..i].iter().enumerate().map(|(j, wj)| self.r[j * k + i] * wj).sum();
            w[i] = (w[i] - head) / self.r[i * k + i];
        }
        let mut out = vec![0.0; size];
        for (q, &wi) in self.q.iter().zip(&w) {
            axpy(wi, q, &mut out);
        }
        out
    }
}


#[derive(Clone, Debug)]
pub struct SecantInverse {
    size: usize,
    max_columns: usize,
    reuse_blocks: usize,
    filter: f64,
    closed: Vec<Vec<Pair>>,
    open: Vec<Pair>,
    basis: Basis,
}

impl SecantInverse {


    pub fn new(size: usize, reuse_blocks: usize, filter: f64) -> CaeResult<Self> {
        if size == 0 {
            return Err(err("secant inverse: vector size must be positive"));
        }
        if !filter.is_finite() || !(0.0..1.0).contains(&filter) {
            return Err(err("secant inverse: filter must be finite and in [0, 1)"));
        }
        Ok(Self {
            size,
            max_columns: size,
            reuse_blocks,
            filter,
            closed: Vec::new(),
            open: Vec::new(),
            basis: Basis::default(),
        })
    }

    #[must_use]
    pub fn with_max_columns(mut self, max_columns: usize) -> Self {
        self.max_columns = max_columns.clamp(1, self.size);
        self.trim();
        self.rebuild();
        self
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    #[must_use]
    pub fn pairs(&self) -> usize {
        self.open.len() + self.closed.iter().map(Vec::len).sum::<usize>()
    }

    #[must_use]
    pub fn rank(&self) -> usize {
        self.basis.rank()
    }




    pub fn push(&mut self, s: &[f64], y: &[f64], b_y: &[f64]) -> CaeResult<()> {
        check_finite("secant pair s", s, self.size)?;
        check_finite("secant pair y", y, self.size)?;
        check_finite("secant pair B y", b_y, self.size)?;
        if y.iter().all(|v| *v == 0.0) {
            return Ok(());
        }
        let correction: Vec<f64> = s.iter().zip(b_y).map(|(a, b)| a - b).collect();
        self.open.push(Pair { image: y.to_vec(), correction });
        self.trim();
        self.rebuild();
        Ok(())
    }

    pub fn end_block(&mut self) {
        let open = std::mem::take(&mut self.open);
        if !open.is_empty() {
            self.closed.push(open);
        }
        if self.closed.len() > self.reuse_blocks {
            let drop = self.closed.len() - self.reuse_blocks;
            self.closed.drain(..drop);
        }
        self.rebuild();
    }

    pub fn clear(&mut self) {
        self.closed.clear();
        self.open.clear();
        self.basis = Basis::default();
    }



    pub fn apply(&self, x: &[f64], b_x: &[f64]) -> CaeResult<Vec<f64>> {
        check_finite("secant inverse operand", x, self.size)?;
        check_finite("secant inverse base action", b_x, self.size)?;
        let c = self.basis.coefficients(x);
        let newest = self.newest_first();
        let mut out = b_x.to_vec();
        for (&index, &ci) in self.basis.kept.iter().zip(&c) {
            axpy(ci, &newest[index].correction, &mut out);
        }
        Ok(out)
    }



    pub fn apply_transpose(&self, y: &[f64], bt_y: &[f64]) -> CaeResult<Vec<f64>> {
        check_finite("secant inverse transpose operand", y, self.size)?;
        check_finite("secant inverse transpose base action", bt_y, self.size)?;
        let newest = self.newest_first();
        let z: Vec<f64> = self.basis.kept.iter().map(|&index| dot(&newest[index].correction, y)).collect();
        let mut out = self.basis.transpose_combination(&z, self.size);
        for (o, b) in out.iter_mut().zip(bt_y) {
            *o += b;
        }
        Ok(out)
    }

    fn newest_first(&self) -> Vec<&Pair> {
        self.open.iter().rev().chain(self.closed.iter().rev().flat_map(|block| block.iter().rev())).collect()
    }

    fn trim(&mut self) {
        while self.pairs() > self.max_columns {
            if let Some(first) = self.closed.first_mut() {
                first.remove(0);
                if first.is_empty() {
                    self.closed.remove(0);
                }
            } else {
                self.open.remove(0);
            }
        }
    }

    fn rebuild(&mut self) {
        let newest = self.newest_first();
        let mut basis = Basis::default();
        let mut r_cols: Vec<Vec<f64>> = Vec::new();
        for (index, pair) in newest.iter().enumerate() {
            let original = norm(&pair.image);
            if original == 0.0 || !original.is_finite() {
                continue;
            }
            let mut v = pair.image.clone();
            let mut coeffs = vec![0.0; basis.q.len()];
            for _pass in 0..2 {
                for (i, q) in basis.q.iter().enumerate() {
                    let h = dot(q, &v);
                    coeffs[i] += h;
                    axpy(-h, q, &mut v);
                }
            }
            let rest = norm(&v);
            if rest <= self.filter * original || rest == 0.0 {
                continue;
            }
            for vi in &mut v {
                *vi /= rest;
            }
            coeffs.push(rest);
            basis.q.push(v);
            basis.kept.push(index);
            r_cols.push(coeffs);
        }
        let k = basis.q.len();
        basis.r = vec![0.0; k * k];
        for (j, col) in r_cols.iter().enumerate() {
            for (i, &value) in col.iter().enumerate() {
                basis.r[i * k + j] = value;
            }
        }
        self.basis = basis;
    }
}


#[derive(Clone, Debug)]
pub struct IqnIls {
    inverse: SecantInverse,
    initial_relaxation: f64,
    last: Option<(Vec<f64>, Vec<f64>)>,
    updates: usize,
}

impl IqnIls {


    pub fn new(size: usize, reuse_steps: usize, filter: f64) -> CaeResult<Self> {
        Ok(Self {
            inverse: SecantInverse::new(size, reuse_steps, filter)?,
            initial_relaxation: 0.5,
            last: None,
            updates: 0,
        })
    }



    pub fn with_initial_relaxation(mut self, omega: f64) -> CaeResult<Self> {
        if !omega.is_finite() || omega <= 0.0 || omega > 1.0 {
            return Err(err("IQN-ILS initial relaxation must be in (0, 1]"));
        }
        self.initial_relaxation = omega;
        Ok(self)
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.inverse.size()
    }

    #[must_use]
    pub fn columns(&self) -> usize {
        self.inverse.rank()
    }

    #[must_use]
    pub fn updates(&self) -> usize {
        self.updates
    }



    pub fn update(&mut self, residual: &[f64], trace: &[f64]) -> CaeResult<Vec<f64>> {
        let size = self.size();
        check_finite("IQN-ILS residual", residual, size)?;
        check_finite("IQN-ILS trace", trace, size)?;
        if let Some((last_r, last_d)) = &self.last {
            let dr: Vec<f64> = residual.iter().zip(last_r).map(|(a, b)| a - b).collect();
            let dd: Vec<f64> = trace.iter().zip(last_d).map(|(a, b)| a - b).collect();
            let minus_dr: Vec<f64> = dr.iter().map(|v| -v).collect();
            self.inverse.push(&dd, &dr, &minus_dr)?;
        }
        self.last = Some((residual.to_vec(), trace.to_vec()));
        self.updates += 1;
        let next = if self.inverse.rank() == 0 {
            trace.iter().zip(residual).map(|(d, r)| d + self.initial_relaxation * r).collect()
        } else {
            let h_r = self.apply(residual)?;
            trace.iter().zip(&h_r).map(|(d, h)| d - h).collect()
        };
        Ok(next)
    }



    pub fn apply(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        let minus: Vec<f64> = x.iter().map(|v| -v).collect();
        self.inverse.apply(x, &minus)
    }



    pub fn apply_transpose(&self, y: &[f64]) -> CaeResult<Vec<f64>> {
        let minus: Vec<f64> = y.iter().map(|v| -v).collect();
        self.inverse.apply_transpose(y, &minus)
    }

    pub fn end_step(&mut self) {
        self.inverse.end_block();
        self.last = None;
    }

    pub fn reset(&mut self) {
        self.inverse.clear();
        self.last = None;
    }
}

