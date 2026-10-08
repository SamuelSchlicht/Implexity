// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
