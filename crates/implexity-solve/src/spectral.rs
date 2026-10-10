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



use std::f64::consts::PI;

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseMatrix, eigh_generalized};

use crate::matrix::allclose;

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct EigenResult {
    pub eigenvalues: Vec<f64>,
    pub eigenvectors: DenseMatrix,
}



pub fn generalized_eigh(
    k: &DenseMatrix,
    m: &DenseMatrix,
    n_modes: Option<usize>,
    min_eigenvalue: f64,
) -> CaeResult<EigenResult> {
    if k.nrows != k.ncols || (m.nrows, m.ncols) != (k.nrows, k.ncols) {
        return Err(err("K and M must be equally sized square matrices"));
    }
    if !allclose(&k.data, &k.transpose().data, 1e-10, 1e-12) {
        return Err(err("K must be symmetric"));
    }
    if !allclose(&m.data, &m.transpose().data, 1e-10, 1e-12) {
        return Err(err("M must be symmetric"));
    }
    let (vals, vecs) =
        eigh_generalized(k, m).map_err(|e| err(format!("generalized eigenproblem failed: {e}")))?;
    let mut keep: Vec<usize> = (0..vals.len()).filter(|&i| vals[i] >= min_eigenvalue).collect();
    if let Some(nm) = n_modes {
        keep.truncate(nm);
    }
    let n = k.nrows;
    let mut v = DenseMatrix::zeros(n, keep.len());
    for (c, &j) in keep.iter().enumerate() {
        for i in 0..n {
            v.data[i * keep.len() + c] = vecs.get(i, j);
        }
    }
    Ok(EigenResult { eigenvalues: keep.iter().map(|&i| vals[i]).collect(), eigenvectors: v })
}



pub fn modal_assurance(
    reference: &DenseMatrix,
    candidate: &DenseMatrix,
    metric: Option<&DenseMatrix>,
) -> CaeResult<DenseMatrix> {
    if reference.nrows != candidate.nrows {
        return Err(err("mode matrices must have shape (dofs, modes)"));
    }
    let n = reference.nrows;
    let lin = |e: implexity_linalg::error::LinalgError| err(e.to_string());
    let w = metric.cloned().unwrap_or_else(|| DenseMatrix::identity(n));
    let at = reference.transpose();
    let atw = at.matmul(&w).map_err(lin)?;
    let num = atw.matmul(candidate).map_err(lin)?;
    let aa = atw.matmul(reference).map_err(lin)?;
    let bb = candidate.transpose().matmul(&w).map_err(lin)?.matmul(candidate).map_err(lin)?;
    let (ra, rb) = (reference.ncols, candidate.ncols);
    let mut out = DenseMatrix::zeros(ra, rb);
    for i in 0..ra {
        for j in 0..rb {
            let x = num.get(i, j).abs().powi(2);
            out.data[i * rb + j] =
                x / aa.get(i, i).max(f64::MIN_POSITIVE) / bb.get(j, j).max(f64::MIN_POSITIVE);
        }
    }
    Ok(out)
}



pub fn eigenvalue_design_gradient(
    eigenvalue: f64,
    eigenvector: &[f64],
    dk: &[DenseMatrix],
    dm: Option<&[DenseMatrix]>,
) -> CaeResult<Vec<f64>> {
    let n = eigenvector.len();
    if dk.iter().any(|d| (d.nrows, d.ncols) != (n, n)) {
        return Err(err("dK trailing dimensions must match the mode size"));
    }
    if let Some(dm) = dm
        && (dm.len() != dk.len() || dm.iter().any(|d| (d.nrows, d.ncols) != (n, n)))
    {
        return Err(err("dM must match dK"));
    }
    Ok(dk
        .iter()
        .enumerate()
        .map(|(k, d)| {
            let mut s = 0.0;
            for i in 0..n {
                for j in 0..n {
                    let mass = dm.map_or(0.0, |m| m[k].data[i * n + j]);
                    s += eigenvector[i] * (d.data[i * n + j] - eigenvalue * mass) * eigenvector[j];
                }
            }
            s
        })
        .collect())
}



pub fn frequency_gradient(eigenvalue: f64, gradient: &[f64]) -> CaeResult<Vec<f64>> {
    if eigenvalue <= 0.0 {
        return Err(err("frequency requires a positive eigenvalue"));
    }
    Ok(gradient.iter().map(|g| g / (4.0 * PI * eigenvalue.sqrt())).collect())
}



pub fn smooth_cluster_min(
    values: &[f64],
    gradients: &DenseMatrix,
    sharpness: f64,
) -> CaeResult<(f64, Vec<f64>)> {
    if gradients.nrows != values.len() || values.is_empty() {
        return Err(err("gradients must have one leading row per value"));
    }
    if sharpness <= 0.0 {
        return Err(err("sharpness must be positive"));
    }
    let m = values.iter().copied().fold(f64::INFINITY, f64::min);
    let wraw: Vec<f64> = values.iter().map(|x| (-sharpness * (x - m)).exp()).collect();
    let total: f64 = wraw.iter().sum();
    let value = m - total.ln() / sharpness;
    let cols = gradients.ncols;
    let mut grad = vec![0.0; cols];
    for (i, w) in wraw.iter().enumerate() {
        let wi = w / total;
        for (g, x) in grad.iter_mut().zip(&gradients.data[i * cols..(i + 1) * cols]) {
            *g += wi * x;
        }
    }
    Ok((value, grad))
}

