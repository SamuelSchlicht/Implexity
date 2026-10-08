// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

pub const WIDTH: usize = 8;

pub trait DifferentiableResidual: Send + Sync {
    fn residual<S: Scalar>(&self, u: &[S], x: &[S]) -> Vec<S>;
}

pub trait DifferentiableResponse: Send + Sync {
    fn value<S: Scalar>(&self, u: &[S], x: &[S]) -> S;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argument {
    State,
    Design,
}

fn lift(v: &[f64]) -> Vec<Dual<WIDTH>> {
    v.iter().map(|&x| Dual::constant(x)).collect()
}

fn seed(values: &mut [Dual<WIDTH>], start: usize, end: usize) {
    for (j, s) in values.iter_mut().enumerate() {
        s.eps = [0.0; WIDTH];
        if (start..end).contains(&j) {
            s.eps[j - start] = 1.0;
        }
    }
}



pub fn evaluate<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
    let out = r.residual::<f64>(u, x);
    if out.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::convergence("residual contains non-finite values"));
    }
    Ok(out)
}

#[must_use]
pub fn jacobian<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64], wrt: Argument) -> DenseMatrix {
    let (mut du, mut dx) = (lift(u), lift(x));
    let cols = if wrt == Argument::State { u.len() } else { x.len() };
    let mut out: Option<DenseMatrix> = None;
    for pass in 0..cols.div_ceil(WIDTH).max(1) {
        let start = pass * WIDTH;
        let end = (start + WIDTH).min(cols);
        match wrt {
            Argument::State => seed(&mut du, start, end),
            Argument::Design => seed(&mut dx, start, end),
        }
        let y = r.residual(&du, &dx);
        let m = out.get_or_insert_with(|| DenseMatrix::zeros(y.len(), cols));
        for (i, yi) in y.iter().enumerate() {
            for j in start..end {
                m.data[i * cols + j] = yi.eps[j - start];
            }
        }
    }
    out.unwrap_or_else(|| DenseMatrix::zeros(0, cols))
}

#[must_use]
pub fn jvp_state<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64], v: &[f64]) -> CaeResult<Vec<f64>> {
    if v.len() != u.len() {
        return Err(CaeError::contract("state direction length differs from state length"));
    }
    let du: Vec<Dual<1>> = u.iter().zip(v).map(|(&a, &b)| Dual::new(a, [b])).collect();
    let dx: Vec<Dual<1>> = x.iter().map(|&a| Dual::constant(a)).collect();
    Ok(r.residual(&du, &dx).iter().map(|y| y.eps[0]).collect())
}

#[must_use]
pub fn vjp<R: DifferentiableResidual>(r: &R, u: &[f64], x: &[f64], w: &[f64], wrt: Argument) -> CaeResult<Vec<f64>> {
    let (mut du, mut dx) = (lift(u), lift(x));
    let cols = if wrt == Argument::State { u.len() } else { x.len() };
    let mut out = vec![0.0; cols];
    for pass in 0..cols.div_ceil(WIDTH).max(1) {
        let start = pass * WIDTH;
        let end = (start + WIDTH).min(cols);
        match wrt {
            Argument::State => seed(&mut du, start, end),
            Argument::Design => seed(&mut dx, start, end),
        }
        let y = r.residual(&du, &dx);
        if y.len() != w.len() {
            return Err(CaeError::contract("residual cotangent length differs from residual length"));
        }
        for (k, o) in out[start..end].iter_mut().enumerate() {
            *o = y.iter().zip(w).map(|(yi, wi)| wi * yi.eps[k]).sum();
        }
    }
    Ok(out)
}

#[must_use]
pub fn gradient<J: DifferentiableResponse>(j: &J, u: &[f64], x: &[f64], wrt: Argument) -> Vec<f64> {
    let (mut du, mut dx) = (lift(u), lift(x));
    let cols = if wrt == Argument::State { u.len() } else { x.len() };
    let mut out = vec![0.0; cols];
    for pass in 0..cols.div_ceil(WIDTH) {
        let start = pass * WIDTH;
        let end = (start + WIDTH).min(cols);
        match wrt {
            Argument::State => seed(&mut du, start, end),
            Argument::Design => seed(&mut dx, start, end),
        }
        let y = j.value(&du, &dx);
        out[start..end].copy_from_slice(&y.eps[..end - start]);
    }
    out
}

#[must_use]
pub fn response_value<J: DifferentiableResponse>(j: &J, u: &[f64], x: &[f64]) -> f64 {
    j.value::<f64>(u, x)
}

