// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseLu;
use implexity_linalg::lu::{Parallelism, SparseLu};

use crate::factorization::{symbolic_for,Factorization};
use crate::local_condensation::LocalEliminationPartition;
use crate::matrix::Jacobian;

pub(super) fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub(super) fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

pub(super) fn norm_inf(a: &[f64]) -> f64 {
    a.iter().fold(0.0_f64, |m, v| m.max(v.abs()))
}

pub(super) fn axpy(alpha: f64, x: &[f64], y: &mut [f64]) {
    for (yi, xi) in y.iter_mut().zip(x) {
        *yi += alpha * xi;
    }
}

pub(super) fn add_into(y: &mut [f64], x: &[f64]) {
    for (yi, xi) in y.iter_mut().zip(x) {
        *yi += xi;
    }
}

pub(super) fn sub_into(y: &mut [f64], x: &[f64]) {
    for (yi, xi) in y.iter_mut().zip(x) {
        *yi -= xi;
    }
}

pub(super) fn negated(x: &[f64]) -> Vec<f64> {
    x.iter().map(|v| -v).collect()
}

pub(super) enum CurrentFactor {
    Dense(DenseLu),
    Sparse(Box<SparseLu>),
    Partitioned(Box<Factorization>),
}

impl CurrentFactor {
    pub(super) fn new(jacobian: &Jacobian, size: usize, groups:Option<Vec<Vec<usize>>>) -> CaeResult<Self> {
        if jacobian.shape() != (size, size) {
            return Err(CaeError::contract(format!(
                "multirate coupling: field B current Jacobian has shape {:?}, expected ({size}, {size})",
                jacobian.shape()
            )));
        }
        if let Some(groups)=groups.filter(|g|!g.is_empty()) {
            let partition=LocalEliminationPartition::new(size,groups).map_err(|e|CaeError::contract(e.to_string()))?;
            let factor=Factorization::correction_with_partition(jacobian.clone(),size,None,Some(&partition))?;
            return Ok(Self::Partitioned(Box::new(factor)));
        }
        let singular = |e: implexity_linalg::error::LinalgError| {
            CaeError::newton(format!(
                "multirate coupling: field B step Jacobian could not be factorized: {e}"
            ))
        };
        match jacobian {
            Jacobian::Dense(m) => {
                if m.data.iter().any(|v| !v.is_finite()) {
                    return Err(CaeError::newton("multirate coupling: field B step Jacobian is not finite"));
                }
                Ok(Self::Dense(DenseLu::new(m).map_err(singular)?))
            }
            Jacobian::Csr(_) | Jacobian::Csc(_) => {
                let csc = match jacobian {
                    Jacobian::Csc(m) => m.clone(),
                    _ => jacobian.to_csr()?.to_csc(),
                };
                if !csc.is_finite() {
                    return Err(CaeError::newton("multirate coupling: field B step Jacobian is not finite"));
                }
                let symbolic = symbolic_for(&csc).map_err(singular)?;
                let lu = symbolic.factor(&csc, Parallelism::Sequential).map_err(singular)?;
                Ok(Self::Sparse(Box::new(lu)))
            }
            Jacobian::Operator(_) => Err(CaeError::contract(
                "multirate coupling: field B must provide an assembled dense/sparse current Jacobian",
            )),
        }
    }

    pub(super) fn solve(&self, b: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let lin = |e: implexity_linalg::error::LinalgError| CaeError::contract(e.to_string());
        let x = match self {
            Self::Dense(lu) => lu.solve(b, 1, transpose).map_err(lin)?,
            Self::Partitioned(factor)=>factor.solve(b,transpose)?.0,
            Self::Sparse(lu) => {
                if transpose {
                    lu.solve_transpose(b).map_err(lin)?
                } else {
                    lu.solve(b).map_err(lin)?
                }
            }
        };
        if x.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::newton(
                "multirate coupling: field B step solve produced non-finite values",
            ));
        }
        Ok(x)
    }
}


pub(super) trait KrylovSystem {
    fn aux_len(&self) -> usize;
    fn matvec(&mut self, x: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)>;
    fn precondition(&mut self, x: &[f64]) -> CaeResult<Vec<f64>>;
}

pub(super) struct KrylovOutcome {
    pub(super) x: Vec<f64>,
    pub(super) aux: Vec<f64>,
    pub(super) iterations: usize,
    pub(super) relative_residual: f64,
    pub(super) converged: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct KrylovLimits {
    pub(super) rtol: f64,
    pub(super) restart: usize,
    pub(super) max_iterations: usize,
}

fn orthogonalize(v: &[Vec<f64>], w: &mut [f64]) -> Vec<f64> {
    let mut col = vec![0.0; v.len() + 1];
    for _pass in 0..2 {
        for (i, vi) in v.iter().enumerate() {
            let hij = dot(vi, w);
            col[i] += hij;
            axpy(-hij, vi, w);
        }
    }
    col
}

fn back_substitute(h: &[Vec<f64>], g: &[f64]) -> Vec<f64> {
    let k = g.len();
    let mut y = g.to_vec();
    for i in (0..k).rev() {
        let mut value = y[i];
        for (j, hj) in h.iter().enumerate().take(k).skip(i + 1) {
            value -= hj[i] * y[j];
        }
        let diag = h[i][i];
        y[i] = if diag == 0.0 { 0.0 } else { value / diag };
    }
    y
}

#[allow(clippy::many_single_char_names)]
pub(super) fn fgmres(
    system: &mut dyn KrylovSystem,
    b: &[f64],
    limits: KrylovLimits,
) -> CaeResult<KrylovOutcome> {
    let n = b.len();
    if b.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("coupled Krylov solve: right-hand side must be finite"));
    }
    let bnorm = norm(b);
    let mut x = vec![0.0; n];
    let mut aux = vec![0.0; system.aux_len()];
    if bnorm == 0.0 {
        return Ok(KrylovOutcome { x, aux, iterations: 0, relative_residual: 0.0, converged: true });
    }
    let target = limits.rtol * bnorm;
    let restart = limits.restart.clamp(1, n.max(1));
    let mut total = 0usize;
    let mut first = true;
    loop {
        let r: Vec<f64> = if first {
            b.to_vec()
        } else {
            let (ax, gx) = system.matvec(&x)?;
            if ax.len() != n || gx.len() != aux.len() {
                return Err(CaeError::contract(
                    "coupled Krylov solve: operator returned a vector of the wrong length",
                ));
            }
            aux = gx;
            b.iter().zip(&ax).map(|(p, q)| p - q).collect()
        };
        first = false;
        let beta = norm(&r);
        if !beta.is_finite() {
            return Err(CaeError::convergence("coupled Krylov solve: residual became non-finite"));
        }
        if beta <= target {
            return Ok(KrylovOutcome {
                x,
                aux,
                iterations: total,
                relative_residual: beta / bnorm,
                converged: true,
            });
        }
        if total >= limits.max_iterations {
            return Ok(KrylovOutcome {
                x,
                aux,
                iterations: total,
                relative_residual: beta / bnorm,
                converged: false,
            });
        }
        let mut v: Vec<Vec<f64>> = Vec::with_capacity(restart + 1);
        let mut z: Vec<Vec<f64>> = Vec::with_capacity(restart);
        v.push(r.iter().map(|e| e / beta).collect());
        let mut h: Vec<Vec<f64>> = Vec::with_capacity(restart);
        let mut givens: Vec<(f64, f64)> = Vec::with_capacity(restart);
        let mut g = vec![0.0; restart + 1];
        g[0] = beta;
        let mut k = 0usize;
        while k < restart && total < limits.max_iterations {
            let zk = system.precondition(&v[k])?;
            let (mut w, _) = system.matvec(&zk)?;
            if w.len() != n || zk.len() != n {
                return Err(CaeError::contract(
                    "coupled Krylov solve: operator returned a vector of the wrong length",
                ));
            }
            total += 1;
            let mut col = orthogonalize(&v, &mut w);
            let hnext = norm(&w);
            col[k + 1] = hnext;
            for (i, &(c, s)) in givens.iter().enumerate() {
                let (a0, a1) = (col[i], col[i + 1]);
                col[i] = c * a0 + s * a1;
                col[i + 1] = -s * a0 + c * a1;
            }
            let (a, bb) = (col[k], col[k + 1]);
            let rho = a.hypot(bb);
            let (c, s) = if rho == 0.0 { (1.0, 0.0) } else { (a / rho, bb / rho) };
            col[k] = rho;
            col[k + 1] = 0.0;
            givens.push((c, s));
            g[k + 1] = -s * g[k];
            g[k] *= c;
            h.push(col);
            z.push(zk);
            k += 1;
            if hnext == 0.0 || !hnext.is_finite() {
                break;
            }
            v.push(w.iter().map(|e| e / hnext).collect());
            if g[k].abs() <= 0.5 * target {
                break;
            }
        }
        let y = back_substitute(&h, &g[..k]);
        for (zi, &yi) in z.iter().zip(&y) {
            axpy(yi, zi, &mut x);
        }
    }
}

