// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::error::LinalgError;
use crate::lu::SparseLu;
use crate::sparse::{CscMatrix, CsrMatrix};


pub trait LinearOperator {
    fn n(&self) -> usize;



    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError>;



    fn matvec(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        let mut y = vec![0.0; self.n()];
        self.apply(x, &mut y)?;
        Ok(y)
    }



    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        xs.iter().map(|x| self.matvec(x)).collect()
    }
}

impl<T: LinearOperator + ?Sized> LinearOperator for &T {
    fn n(&self) -> usize {
        (**self).n()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        (**self).apply(x, y)
    }
    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        (**self).apply_block(xs)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Identity(pub usize);

impl LinearOperator for Identity {
    fn n(&self) -> usize {
        self.0
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        check(self.0, x, y)?;
        y.copy_from_slice(x);
        Ok(())
    }
}

pub struct FnOperator<F> {
    n: usize,
    f: F,
}

impl<F> FnOperator<F>
where
    F: Fn(&[f64], &mut [f64]) -> Result<(), LinalgError>,
{
    pub const fn new(n: usize, f: F) -> Self {
        Self { n, f }
    }
}

impl<F> LinearOperator for FnOperator<F>
where
    F: Fn(&[f64], &mut [f64]) -> Result<(), LinalgError>,
{
    fn n(&self) -> usize {
        self.n
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        check(self.n, x, y)?;
        (self.f)(x, y)
    }
}

fn check(n: usize, x: &[f64], y: &[f64]) -> Result<(), LinalgError> {
    if x.len() == n && y.len() == n {
        Ok(())
    } else {
        Err(LinalgError::Shape(format!("operator of order {n} applied to {} → {}", x.len(), y.len())))
    }
}

impl LinearOperator for CsrMatrix {
    fn n(&self) -> usize {
        self.nrows()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        if self.nrows() != self.ncols() {
            return Err(LinalgError::Shape("operator must be square".into()));
        }
        self.matvec_into(x, y)
    }
}

impl LinearOperator for CscMatrix {
    fn n(&self) -> usize {
        self.nrows()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        if self.nrows() != self.ncols() {
            return Err(LinalgError::Shape("operator must be square".into()));
        }
        check(self.nrows(), x, y)?;
        y.copy_from_slice(&self.matvec(x)?);
        Ok(())
    }
}

impl LinearOperator for SparseLu {
    fn n(&self) -> usize {
        SparseLu::n(self)
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        check(SparseLu::n(self), x, y)?;
        y.copy_from_slice(x);
        self.solve_in_place(y)
    }
    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        solve_block(self, xs, false)
    }
}



pub fn solve_block(lu: &SparseLu, xs: &[Vec<f64>], transpose: bool) -> Result<Vec<Vec<f64>>, LinalgError> {
    let n = SparseLu::n(lu);
    if xs.iter().any(|x| x.len() != n) {
        return Err(LinalgError::Shape(format!(
            "operator of order {n} applied to a block of another length"
        )));
    }
    if n == 0 {
        return Ok(xs.to_vec());
    }
    let mut block: Vec<f64> = xs.iter().flatten().copied().collect();
    lu.solve_many_in_place(&mut block, xs.len(), transpose)?;
    Ok(block.chunks_exact(n).map(<[f64]>::to_vec).collect())
}

#[inline]
#[must_use]
pub fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[must_use]
pub fn nrm2(x: &[f64]) -> f64 {
    let scale = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    if scale == 0.0 || !scale.is_finite() {
        return if scale.is_nan() || x.iter().any(|v| v.is_nan()) { f64::NAN } else { scale };
    }
    if (1e-150..1e150).contains(&scale) {
        return x.iter().map(|v| v * v).sum::<f64>().sqrt();
    }
    let s: f64 = x.iter().map(|v| (v / scale) * (v / scale)).sum();
    scale * s.sqrt()
}

#[inline]
pub fn axpy(a: f64, x: &[f64], y: &mut [f64]) {
    for (yi, xi) in y.iter_mut().zip(x) {
        *yi += a * xi;
    }
}

#[inline]
pub fn scal(a: f64, x: &mut [f64]) {
    for v in x {
        *v *= a;
    }
}
