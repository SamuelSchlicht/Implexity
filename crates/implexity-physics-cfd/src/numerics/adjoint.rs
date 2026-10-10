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


use std::sync::Arc;

use crate::error::{CfdError, CfdResult};
use crate::numerics::preconditioner::SaddlePointLduPreconditioner;
use crate::numerics::solver::{SaddlePointKrylovSolver, SaddlePointSolveResult};
use crate::numerics::system::SaddlePointSystem;

pub type ResidualDesignVjp<'a> = &'a dyn Fn(&[f64], &[f64]) -> CfdResult<Vec<f64>>;

#[derive(Debug, Clone)]
pub struct DiscreteAdjointResult {
    pub adjoint: Vec<f64>,
    pub total_design_gradient: Vec<f64>,
    pub adjoint_solve: SaddlePointSolveResult,
    pub residual_design_vjp: Vec<f64>,
    pub objective_partial_design: Vec<f64>,
}



pub fn solve_discrete_adjoint(
    system: &Arc<SaddlePointSystem>,
    state: &[f64],
    objective_state_gradient: &[f64],
    residual_design_vjp: ResidualDesignVjp<'_>,
    solver: &SaddlePointKrylovSolver,
    objective_partial_design: Option<&[f64]>,
    preconditioner: Option<&SaddlePointLduPreconditioner>,
) -> CfdResult<DiscreteAdjointResult> {
    if state.len() != system.size() || objective_state_gradient.len() != system.size() {
        return Err(CfdError::Contract("State or objective-state gradient has an invalid size.".into()));
    }
    if !state.iter().all(|v| v.is_finite()) || !objective_state_gradient.iter().all(|v| v.is_finite()) {
        return Err(CfdError::Contract("State or objective-state gradient is non-finite.".into()));
    }
    let adjoint_solve = solver.solve(system, Some(objective_state_gradient), true, None, preconditioner)?;
    let vjp = residual_design_vjp(&adjoint_solve.state, state)?;
    let partial = match objective_partial_design {
        None => vec![0.0; vjp.len()],
        Some(p) => {
            if p.len() != vjp.len() {
                return Err(CfdError::Contract(
                    "The direct objective-design derivative has the wrong shape.".into(),
                ));
            }
            p.to_vec()
        }
    };
    let gradient: Vec<f64> = partial.iter().zip(&vjp).map(|(a, b)| a - b).collect();
    if !vjp.iter().all(|v| v.is_finite()) || !gradient.iter().all(|v| v.is_finite()) {
        return Err(CfdError::Contract("The assembled topology gradient is non-finite.".into()));
    }
    Ok(DiscreteAdjointResult {
        adjoint: adjoint_solve.state.clone(),
        total_design_gradient: gradient,
        adjoint_solve,
        residual_design_vjp: vjp,
        objective_partial_design: partial,
    })
}
