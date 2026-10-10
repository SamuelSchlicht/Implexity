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

use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::collections::BTreeSet;

pub struct MacroTransport<S> {
    pub gradients: Vec<[[S; 3]; 4]>,
    pub transport_viscosity_pa_s: Vec<S>,
    pub gradient_defined: Vec<bool>,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn solve<S: TransientGasScalar>(
    mut a: Vec<Vec<S>>,
    mut b: Vec<S>,
    limit: f64,
) -> CaeResult<Vec<S>> {
    let n = b.len();
    let mut largest = 0.0_f64;
    let mut smallest = f64::INFINITY;
    for k in 0..n {
        let pivot = (k..n)
            .max_by(|&i, &j| a[i][k].value().abs().total_cmp(&a[j][k].value().abs()))
            .unwrap();
        a.swap(k, pivot);
        b.swap(k, pivot);
        let v = a[k][k].value().abs();
        largest = largest.max(v);
        smallest = smallest.min(v);
        if !v.is_finite() || v == 0.0 || largest / smallest > limit {
            return Err(fail(
                "continuum gradient stencil is rank deficient or ill conditioned",
            ));
        }
        for i in k + 1..n {
            let f = a[i][k] / a[k][k];
            for j in k..n {
                let ak = a[k][j];
                a[i][j] = a[i][j] - f * ak;
            }
            let bk = b[k];
            b[i] = b[i] - f * bk;
        }
    }
    let mut x = vec![S::zero(); n];
    for i in (0..n).rev() {
        let r = (i + 1..n).fold(b[i], |s, j| s - a[i][j] * x[j]);
        x[i] = r / a[i][i];
    }
    Ok(x)
}

pub fn reconstruct<S: TransientGasScalar>(
    density: &[[S; 4]],
    neighbors: &[Vec<(usize, [S; 3])>],
    selected: &BTreeSet<usize>,
    axes: [bool; 3],
    maximum_condition_number: f64,
    molecular_viscosity_pa_s: S,
    step_s: S,
    wale: Option<(&implexity_physics_lbm::moving::wale::Wale, f64)>,
) -> CaeResult<MacroTransport<S>> {
    let n = density.len();
    if neighbors.len() != n
        || selected.iter().any(|&i| i >= n)
        || !maximum_condition_number.is_finite()
        || maximum_condition_number < 1.
        || !molecular_viscosity_pa_s.finite_first_order()
        || molecular_viscosity_pa_s.value() <= 0.
        || !step_s.finite_first_order()
        || step_s.value() <= 0.
        || density.iter().flatten().any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid macro transport inputs"));
    }
    if let Some((model, spacing)) = wale {
        model.validate()?;
        if !spacing.is_finite() || spacing <= 0. {
            return Err(fail("invalid macro transport filter spacing"));
        }
    }
    let ids = (0..3).filter(|&j| axes[j]).collect::<Vec<_>>();
    let d = ids.len();
    let mut grad = vec![[[S::zero(); 3]; 4]; n];
    let mut transport = vec![molecular_viscosity_pa_s; n];
    let mut defined = vec![false; n];
    for &i in selected {
        let rho = density[i][0];
        if rho.value() < 0. {
            return Err(fail("negative macro transport density"));
        }
        if rho.value() == 0. {
            if density[i]
                .iter()
                .any(|v| v.value() != 0. || v.has_direction())
            {
                return Err(fail(
                    "vacuum macro transport has no ordinary velocity-gradient derivative",
                ));
            }
            continue;
        }
        defined[i] = true;
        if d > 0 {
            let mut m = vec![vec![S::zero(); d]; d];
            let mut b = vec![vec![S::zero(); d]; 4];
            for &(j, dx) in &neighbors[i] {
                if j >= n || dx.iter().any(|v| !v.finite_first_order()) {
                    return Err(fail("invalid macro transport neighbor"));
                }
                if density[j][0].value() <= 0. {
                    return Err(fail("macro transport reconstruction adjacent to vacuum requires an admitted stencil"));
                }
                let rr = ids.iter().fold(S::zero(), |s, &k| s + dx[k] * dx[k]);
                if rr.value() <= 0. {
                    return Err(fail("coincident macro transport cell centroids"));
                }
                for x in 0..d {
                    for y in 0..d {
                        m[x][y] = m[x][y] + dx[ids[x]] * dx[ids[y]] / rr;
                    }
                    for q in 0..4 {
                        b[q][x] = b[q][x] + dx[ids[x]] * (density[j][q] - density[i][q]) / rr;
                    }
                }
            }
            for q in 0..4 {
                let g = solve(m.clone(), b[q].clone(), maximum_condition_number)?;
                for x in 0..d {
                    grad[i][q][ids[x]] = g[x];
                }
            }
        }
        if let Some((model, spacing)) = wale {
            let velocity_gradient = std::array::from_fn(|q| {
                std::array::from_fn(|j| {
                    (grad[i][q + 1][j] - density[i][q + 1] / rho * grad[i][0][j]) / rho
                })
            });
            let eddy = implexity_physics_lbm::moving::wale::physical_eddy_viscosity(
                model,
                &velocity_gradient,
                S::from_f64(spacing),
                step_s,
            )?;
            transport[i] = molecular_viscosity_pa_s + rho * eddy;
            if !transport[i].finite_first_order() {
                return Err(fail("macro WALE transport viscosity is not finite"));
            }
        }
    }
    Ok(MacroTransport {
        gradients: grad,
        transport_viscosity_pa_s: transport,
        gradient_defined: defined,
    })
}
