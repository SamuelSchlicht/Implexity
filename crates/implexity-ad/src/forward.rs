// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use rayon::prelude::*;

use crate::dual::Dual;
use crate::error::AdError;
use crate::hyperdual::HyperDual;

#[derive(Clone, Debug, PartialEq)]
pub struct Jacobian {
    pub value: Vec<f64>,
    pub matrix: Vec<f64>,
    pub rows: usize,
    pub cols: usize,
}

impl Jacobian {


    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        assert!(i < self.rows && j < self.cols, "Jacobian index out of range");
        self.matrix[i * self.cols + j]
    }
}




pub fn jacobian<const N: usize, F>(f: F, x: &[f64]) -> Result<Jacobian, AdError>
where
    F: Fn(&[Dual<N>]) -> Vec<Dual<N>>,
{
    const { assert!(N > 0, "Dual width N must be positive") };
    let n = x.len();
    let mut seeded: Vec<Dual<N>> = x.iter().map(|&v| Dual::constant(v)).collect();
    let passes = n.div_ceil(N).max(1);
    let mut value = Vec::new();
    let mut matrix = Vec::new();
    let mut rows = None;
    for pass in 0..passes {
        let start = pass * N;
        let end = (start + N).min(n);
        for (j, s) in seeded.iter_mut().enumerate() {
            s.eps = [0.0; N];
            if (start..end).contains(&j) {
                s.eps[j - start] = 1.0;
            }
        }
        let out = f(&seeded);
        let m = *rows.get_or_insert(out.len());
        if out.len() != m {
            return Err(AdError::Shape(format!(
                "jacobian: output length changed from {m} to {} between passes",
                out.len()
            )));
        }
        if pass == 0 {
            value = out.iter().map(|d| d.re).collect();
            matrix = vec![0.0; m * n];
        }
        for (i, y) in out.iter().enumerate() {
            for j in start..end {
                matrix[i * n + j] = y.eps[j - start];
            }
        }
    }
    let rows = rows.unwrap_or(0);
    Ok(Jacobian { value, matrix, rows, cols: n })
}



pub fn gradient<const N: usize, F>(f: F, x: &[f64]) -> Result<(f64, Vec<f64>), AdError>
where
    F: Fn(&[Dual<N>]) -> Dual<N>,
{
    let jac = jacobian::<N, _>(|xs| vec![f(xs)], x)?;
    Ok((jac.value[0], jac.matrix))
}



pub fn jvp<F>(f: F, x: &[f64], v: &[f64]) -> Result<(Vec<f64>, Vec<f64>), AdError>
where
    F: Fn(&[Dual<1>]) -> Vec<Dual<1>>,
{
    if v.len() != x.len() {
        return Err(AdError::Shape(format!("jvp: tangent length {} != input length {}", v.len(), x.len())));
    }
    let xs: Vec<Dual<1>> = x.iter().zip(v).map(|(&a, &b)| Dual::new(a, [b])).collect();
    let out = f(&xs);
    Ok((out.iter().map(|d| d.re).collect(), out.iter().map(|d| d.eps[0]).collect()))
}


#[must_use]
pub fn hessian<F>(f: F, x: &[f64]) -> (f64, Vec<f64>, Vec<f64>)
where
    F: Fn(&[HyperDual]) -> HyperDual,
{
    let n = x.len();
    let mut xs: Vec<HyperDual> = x.iter().map(|&v| HyperDual::constant(v)).collect();
    let mut grad = vec![0.0; n];
    let mut hess = vec![0.0; n * n];
    let mut value = f(&xs).re;
    for i in 0..n {
        for j in i..n {
            for (k, s) in xs.iter_mut().enumerate() {
                s.e1 = if k == i { 1.0 } else { 0.0 };
                s.e2 = if k == j { 1.0 } else { 0.0 };
                s.e12 = 0.0;
            }
            let y = f(&xs);
            value = y.re;
            if j == i {
                grad[i] = y.e1;
            }
            hess[i * n + j] = y.e12;
            hess[j * n + i] = y.e12;
        }
    }
    (value, grad, hess)
}



pub fn hessian_vector_product<F>(f: F, x: &[f64], v: &[f64]) -> Result<Vec<f64>, AdError>
where
    F: Fn(&[HyperDual]) -> HyperDual,
{
    let n = x.len();
    if v.len() != n {
        return Err(AdError::Shape(format!("hvp: direction length {} != input length {n}", v.len())));
    }
    let mut xs: Vec<HyperDual> = x.iter().zip(v).map(|(&a, &d)| HyperDual::new(a, d, 0.0, 0.0)).collect();
    let mut out = vec![0.0; n];
    for (j, o) in out.iter_mut().enumerate() {
        for (k, s) in xs.iter_mut().enumerate() {
            s.e2 = if k == j { 1.0 } else { 0.0 };
        }
        *o = f(&xs).e12;
    }
    Ok(out)
}




pub fn element_jacobians<const N: usize, K>(
    n_elements: usize,
    n_in: usize,
    n_out: usize,
    inputs: &[f64],
    kernel: K,
) -> Result<(Vec<f64>, Vec<f64>), AdError>
where
    K: Fn(usize, &[Dual<N>], &mut [Dual<N>]) + Sync,
{
    if n_in > N {
        return Err(AdError::Shape(format!("element_jacobians: {n_in} inputs exceed dual width {N}")));
    }
    if inputs.len() != n_elements * n_in {
        return Err(AdError::Shape(format!(
            "element_jacobians: {} input values for {n_elements} elements of {n_in}",
            inputs.len()
        )));
    }
    let mut values = vec![0.0; n_elements * n_out];
    let mut jacs = vec![0.0; n_elements * n_out * n_in];
    if n_out == 0 || n_elements == 0 {
        return Ok((values, jacs));
    }
    if n_in == 0 {
        values.par_chunks_mut(n_out).enumerate().for_each(|(e, val)| {
            let mut ys = vec![Dual::<N>::default(); n_out];
            kernel(e, &[], &mut ys);
            for (v, y) in val.iter_mut().zip(&ys) {
                *v = y.re;
            }
        });
        return Ok((values, jacs));
    }
    values.par_chunks_mut(n_out).zip(jacs.par_chunks_mut(n_out * n_in)).enumerate().for_each_init(
        || (Vec::with_capacity(n_in), vec![Dual::<N>::default(); n_out]),
        |(xs, ys), (e, (val, jac))| {
            xs.clear();
            xs.extend(
                inputs[e * n_in..(e + 1) * n_in].iter().enumerate().map(|(k, &v)| Dual::variable(v, k)),
            );
            ys.fill(Dual::default());
            kernel(e, xs, ys);
            for (i, y) in ys.iter().enumerate() {
                val[i] = y.re;
                jac[i * n_in..(i + 1) * n_in].copy_from_slice(&y.eps[..n_in]);
            }
        },
    );
    Ok((values, jacs))
}
