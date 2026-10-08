// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

use super::arnoldi::{deterministic_vector, hashed_vector, real_eigen};
use super::krylov::{dot, norm};
use crate::complex_spectral::{ComplexMatrix, c64};

pub(crate) fn coefficients(basis: &[Vec<f64>], v: &[f64]) -> Vec<f64> {
    basis.iter().map(|b| dot(b, v)).collect()
}

pub(crate) fn combine(basis: &[Vec<f64>], c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    for (b, ci) in basis.iter().zip(c) {
        for (o, bi) in out.iter_mut().zip(b) {
            *o += ci * bi;
        }
    }
    out
}

pub(crate) fn project_out(basis: &[Vec<f64>], v: &[f64]) -> Vec<f64> {
    let mut out = v.to_vec();
    for _pass in 0..2 {
        for b in basis {
            let c = dot(b, &out);
            for (o, bi) in out.iter_mut().zip(b) {
                *o -= c * bi;
            }
        }
    }
    out
}

pub(crate) fn orthonormalize(columns: &[Vec<f64>], n: usize) -> CaeResult<Vec<Vec<f64>>> {
    if columns.len() > n {
        return Err(CaeError::contract(format!(
            "a subspace of dimension {} exceeds the order {n}",
            columns.len()
        )));
    }
    let mut basis: Vec<Vec<f64>> = Vec::with_capacity(columns.len());
    let mut seed = 1000usize;
    for column in columns {
        let mut candidate = column.clone();
        let mut accepted = false;
        for _attempt in 0..16 {
            let before = norm(&candidate);
            let v = project_out(&basis, &candidate);
            let after = norm(&v);
            if before > 0.0 && after > 1e-10 * before && after.is_finite() {
                basis.push(v.into_iter().map(|x| x / after).collect());
                accepted = true;
                break;
            }
            seed += 1;

            candidate = hashed_vector(n, seed);
        }
        if !accepted {
            return Err(CaeError::convergence("could not complete an orthonormal subspace basis"));
        }
    }
    Ok(basis)
}

pub(crate) fn deterministic_basis(n: usize, p: usize) -> CaeResult<Vec<Vec<f64>>> {
    let columns: Vec<Vec<f64>> = (0..p).map(|k| deterministic_vector(n, 100 + k)).collect();
    orthonormalize(&columns, n)
}

pub(crate) fn projected(basis: &[Vec<f64>], images: &[Vec<f64>]) -> DenseMatrix {
    let p = basis.len();
    let mut h = DenseMatrix::zeros(p, p);
    for (i, b) in basis.iter().enumerate() {
        for (j, w) in images.iter().enumerate() {
            h.data[i * p + j] = dot(b, w);
        }
    }
    h
}

pub(crate) struct SpectralInverse {
    basis: Vec<Vec<f64>>,
    map: DenseMatrix,
}

impl SpectralInverse {
    pub(crate) fn new(basis: Vec<Vec<f64>>, k: &DenseMatrix, floor: f64) -> CaeResult<Self> {
        let p = basis.len();
        if p == 0 {
            return Ok(Self { basis, map: DenseMatrix::zeros(0, 0) });
        }
        let (values, right) = real_eigen(k)?;
        let inverse = right.solve(&ComplexMatrix::identity(p))?;
        let mut map = DenseMatrix::zeros(p, p);
        let one = c64::new(1.0, 0.0);
        for (l, value) in values.iter().enumerate() {
            let gap = one - *value;
            let f = if gap.norm() >= floor { one / gap } else { one };
            for i in 0..p {
                let yi = right.get(i, l) * f;
                for j in 0..p {
                    map.data[i * p + j] += (yi * inverse.get(l, j)).re;
                }
            }
        }
        if !map.data.iter().all(|v| v.is_finite()) {
            return Err(CaeError::convergence("Newton–Picard subspace preconditioner is not finite"));
        }
        Ok(Self { basis, map })
    }

    pub(crate) fn apply(&self, v: &[f64]) -> Vec<f64> {
        let n = v.len();
        let c = coefficients(&self.basis, v);
        let mut out = v.to_vec();
        let p = self.basis.len();
        for (i, b) in self.basis.iter().enumerate() {
            let fc: f64 = (0..p).map(|j| self.map.data[i * p + j] * c[j]).sum();
            let coefficient = fc - c[i];
            for (o, bi) in out.iter_mut().zip(b) {
                *o += coefficient * bi;
            }
        }
        debug_assert_eq!(out.len(), n);
        out
    }
}

