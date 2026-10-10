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
