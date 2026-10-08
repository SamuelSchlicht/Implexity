// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use crate::dense::{DenseMatrix, eigh};
use crate::error::LinalgError;
use crate::operator::{LinearOperator, axpy, dot, nrm2, scal};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LanczosOptions {
    pub tol: f64,
    pub max_restarts: Option<usize>,
    pub ncv: Option<usize>,
}

impl Default for LanczosOptions {
    fn default() -> Self {
        Self { tol: 0.0, max_restarts: None, ncv: None }
    }
}



pub fn smallest_eigenvalue(
    a: &dyn LinearOperator,
    opts: &LanczosOptions,
) -> Result<(f64, Vec<f64>), LinalgError> {
    let n = a.n();
    if n == 0 {
        return Err(LinalgError::Shape("eigenvalue of an empty operator".into()));
    }
    let m = opts.ncv.unwrap_or(20).min(n).max(1);
    if n <= m {
        let mut d = DenseMatrix::zeros(n, n);
        for j in 0..n {
            let mut e = vec![0.0; n];
            e[j] = 1.0;
            let c = a.matvec(&e)?;
            for (i, &ci) in c.iter().enumerate() {
                d.data[i * n + j] = ci;
            }
        }
        for i in 0..n {
            for j in 0..i {
                let s = 0.5 * (d.data[i * n + j] + d.data[j * n + i]);
                d.data[i * n + j] = s;
                d.data[j * n + i] = s;
            }
        }
        let (vals, vecs) = eigh(&d)?;
        return Ok((vals[0], (0..n).map(|i| vecs.get(i, 0)).collect()));
    }
    let tol = if opts.tol > 0.0 { opts.tol } else { f64::EPSILON };
    let max_restarts = opts.max_restarts.unwrap_or(10 * n);

    let mut v0: Vec<f64> = (0..n).map(|i| 1.0 + 0.5 * ((i as f64) * 0.618_033_988_749_895).fract()).collect();
    let nv = nrm2(&v0);
    scal(1.0 / nv, &mut v0);
    for _cycle in 0..max_restarts {
        let mut basis: Vec<Vec<f64>> = vec![v0.clone()];
        let mut alpha = Vec::with_capacity(m);
        let mut beta = Vec::with_capacity(m);
        let mut last_beta = 0.0;
        for j in 0..m {
            let mut w = a.matvec(&basis[j])?;
            let aj = dot(&w, &basis[j]);
            alpha.push(aj);

            for _ in 0..2 {
                for v in &basis {
                    let c = dot(v, &w);
                    axpy(-c, v, &mut w);
                }
            }
            let bj = nrm2(&w);
            last_beta = bj;
            if j + 1 == m || bj <= f64::EPSILON * aj.abs().max(1.0) {
                break;
            }
            beta.push(bj);
            scal(1.0 / bj, &mut w);
            basis.push(w);
        }
        let k = alpha.len();
        let mut t = DenseMatrix::zeros(k, k);
        for i in 0..k {
            t.data[i * k + i] = alpha[i];
            if i + 1 < k {
                t.data[i * k + i + 1] = beta[i];
                t.data[(i + 1) * k + i] = beta[i];
            }
        }
        let (theta, s) = eigh(&t)?;
        let th = theta[0];
        let sk = s.get(k - 1, 0);
        let mut y = vec![0.0; n];
        for (i, v) in basis.iter().enumerate().take(k) {
            axpy(s.get(i, 0), v, &mut y);
        }
        let ny = nrm2(&y);
        scal(1.0 / ny, &mut y);
        if (last_beta * sk).abs() <= tol * th.abs().max(f64::MIN_POSITIVE)
            || last_beta <= f64::EPSILON * th.abs().max(1.0)
        {
            return Ok((th, y));
        }
        v0 = y;
    }
    Err(LinalgError::NoConvergence("Lanczos did not converge to the smallest eigenvalue".into()))
}
