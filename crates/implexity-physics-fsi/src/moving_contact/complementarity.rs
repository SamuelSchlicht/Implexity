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

use implexity_core::{CaeError, CaeResult};
#[derive(Clone, Copy)]
pub enum OriginBranch {
    Closed,
    Open,
    Symmetric,
}
pub fn residual_partials(
    gap: f64,
    multiplier: f64,
    gap_scale: f64,
    force_scale: f64,
    origin: OriginBranch,
) -> CaeResult<(f64, f64, f64)> {
    if ![gap, multiplier, gap_scale, force_scale]
        .iter()
        .all(|v| v.is_finite())
        || gap_scale <= 0.
        || force_scale <= 0.
    {
        return Err(CaeError::contract("invalid complementarity inputs"));
    }
    let a = gap / gap_scale;
    let b = multiplier / force_scale;
    let r = a.hypot(b);
    let (da, db) = if r == 0. {
        match origin {
            OriginBranch::Closed => (-1., 0.),
            OriginBranch::Open => (0., -1.),
            OriginBranch::Symmetric => (1. / 2f64.sqrt() - 1., 1. / 2f64.sqrt() - 1.),
        }
    } else {
        (a / r - 1., b / r - 1.)
    };
    let result = (r - a - b, da / gap_scale, db / force_scale);
    if ![a, b, r, result.0, result.1, result.2]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(CaeError::contract("contact normalized overflow"));
    }
    Ok(result)
}

pub fn origin_direction(
    matrix: &implexity_linalg::dense::DenseMatrix,
    residual: &[f64],
    gap_gradient: &[f64],
    gap_scale: f64,
    force_scale: f64,
) -> CaeResult<(Vec<f64>, implexity_linalg::dense::DenseMatrix, &'static str)> {
    use implexity_linalg::dense::DenseLu;
    let n = residual.len();
    if n < 2
        || matrix.nrows != n
        || matrix.ncols != n
        || gap_gradient.len() != n
        || gap_gradient[n - 1] != 0.
        || ![gap_scale, force_scale]
            .iter()
            .all(|x| x.is_finite() && *x > 0.)
        || residual
            .iter()
            .chain(gap_gradient)
            .chain(&matrix.data)
            .any(|x| !x.is_finite())
    {
        return Err(CaeError::contract("invalid origin predictor inputs"));
    }
    let rhs: Vec<_> = residual.iter().map(|x| -x).collect();
    for closed in [true, false] {
        let mut candidate = matrix.clone();
        for j in 0..n {
            candidate.data[(n - 1) * n + j] = if closed {
                -gap_gradient[j] / gap_scale
            } else if j == n - 1 {
                -1. / force_scale
            } else {
                0.
            };
        }
        let Ok(factor) = DenseLu::new(&candidate) else {
            continue;
        };
        let Ok(direction) = factor.solve(&rhs, 1, false) else {
            continue;
        };
        if direction.iter().any(|x| !x.is_finite()) {
            continue;
        }
        let predicted_gap: f64 = gap_gradient
            .iter()
            .zip(&direction)
            .map(|(g, d)| g * d)
            .sum();
        if !predicted_gap.is_finite() {
            continue;
        }

        if (closed && direction[n - 1] >= 0.) || (!closed && predicted_gap >= 0.) {
            return Ok((direction, candidate, if closed { "closed" } else { "open" }));
        }
    }
    Err(CaeError::contract(
        "no cone-consistent contact origin predictor",
    ))
}
