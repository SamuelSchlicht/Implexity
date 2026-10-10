// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

