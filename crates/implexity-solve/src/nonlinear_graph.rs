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
use implexity_linalg::{
    error::LinalgError,
    krylov::{gmres, GmresCallbackType, GmresOptions},
    operator::FnOperator,
};

pub trait NonlinearGraphSystem {
    fn dimension(&self) -> usize;
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>>;
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>>;
    fn check_state(&self, state: &[f64]) -> CaeResult<()>;
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64>;
    fn newton_fixed_coordinates(
        &self,
        _state: &[f64],
        _residual: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        Ok(Vec::new())
    }
    fn reconstruct_newton_direction(
        &self,
        _state: &[f64],
        _residual: &[f64],
        direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        Ok(direction.to_vec())
    }
    fn fallback_direction(&self, _state: &[f64], _residual: &[f64]) -> CaeResult<Option<Vec<f64>>> {
        Ok(None)
    }
    fn preconditioner_product(
        &self,
        _state: &[f64],
        direction: &[f64],
        _transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        Ok(direction.to_vec())
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NonlinearGraphPolicy {
    pub relative_tolerance: f64,
    pub absolute_tolerance: f64,
    pub linear_relative_tolerance: f64,
    pub linear_absolute_tolerance: f64,
    pub maximum_newton_iterations: usize,
    pub maximum_linear_iterations: usize,
    pub krylov_restart: usize,
    pub maximum_backtracking_steps: usize,
    pub sufficient_decrease: f64,
}
impl Default for NonlinearGraphPolicy {
    fn default() -> Self {
        Self {
            relative_tolerance: 1e-9,
            absolute_tolerance: 1e-12,
            linear_relative_tolerance: 1e-10,
            linear_absolute_tolerance: 1e-14,
            maximum_newton_iterations: 30,
            maximum_linear_iterations: 200,
            krylov_restart: 40,
            maximum_backtracking_steps: 30,
            sufficient_decrease: 1e-4,
        }
    }
}
impl NonlinearGraphPolicy {
    pub fn validate(&self) -> CaeResult<()> {
        if !self.relative_tolerance.is_finite()
            || self.relative_tolerance <= 0.
            || !self.absolute_tolerance.is_finite()
            || self.absolute_tolerance < 0.
            || !self.linear_relative_tolerance.is_finite()
            || self.linear_relative_tolerance <= 0.
            || !self.linear_absolute_tolerance.is_finite()
            || self.linear_absolute_tolerance < 0.
            || !self.sufficient_decrease.is_finite()
            || self.sufficient_decrease <= 0.
            || self.sufficient_decrease >= 1.
            || self.maximum_newton_iterations == 0
            || self.maximum_linear_iterations == 0
            || self.krylov_restart == 0
            || self.maximum_backtracking_steps == 0
        {
            return Err(CaeError::contract("invalid nonlinear graph solver policy"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct NonlinearGraphSolution {
    pub state: Vec<f64>,
    pub residual_norm: f64,
    pub newton_iterations: usize,
    pub linear_iterations: usize,
}
#[derive(Clone, Debug)]
pub struct NonlinearGraphLinearSolution {
    pub direction: Vec<f64>,
    pub residual_norm: f64,
    pub iterations: usize,
}
fn checked(n: usize, values: Vec<f64>) -> CaeResult<Vec<f64>> {
    if values.len() != n || values.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("nonlinear graph vector shape or value"));
    }
    Ok(values)
}
fn norm(values: &[f64]) -> f64 {
    values.iter().fold(0f64, |sum, value| sum.hypot(*value))
}
pub fn solve_linear<S: NonlinearGraphSystem + ?Sized>(
    system: &S,
    state: &[f64],
    rhs: &[f64],
    transpose: bool,
    policy: &NonlinearGraphPolicy,
) -> CaeResult<NonlinearGraphLinearSolution> {
    policy.validate()?;
    let n = system.dimension();
    if n == 0
        || state.len() != n
        || rhs.len() != n
        || state.iter().chain(rhs).any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(
            "nonlinear graph linear input shape or value",
        ));
    }
    system.check_state(state)?;
    let op = FnOperator::new(n, |x: &[f64], y: &mut [f64]| {
        let v = system
            .jacobian_product(state, x, transpose)
            .and_then(|v| checked(n, v))
            .map_err(|e| LinalgError::Operator(e.to_string()))?;
        y.copy_from_slice(&v);
        Ok(())
    });
    let preconditioner = FnOperator::new(n, |x: &[f64], y: &mut [f64]| {
        let v = system
            .preconditioner_product(state, x, transpose)
            .and_then(|v| checked(n, v))
            .map_err(|e| LinalgError::Operator(e.to_string()))?;
        y.copy_from_slice(&v);
        Ok(())
    });
    let result = gmres(
        &op,
        rhs,
        None,
        Some(&preconditioner),
        &GmresOptions {
            rtol: policy.linear_relative_tolerance,
            atol: policy.linear_absolute_tolerance,
            restart: Some(policy.krylov_restart.min(n)),
            maxiter: Some(policy.maximum_linear_iterations),
            callback_type: GmresCallbackType::Legacy,
        },
    )
    .map_err(|e| CaeError::convergence(e.to_string()))?;
    let actual = checked(n, system.jacobian_product(state, &result.x, transpose)?)?;
    let defect = norm(
        &actual
            .iter()
            .zip(rhs)
            .map(|(a, b)| a - b)
            .collect::<Vec<_>>(),
    );
    let rhs_norm = norm(rhs);
    let tolerance = policy
        .linear_absolute_tolerance
        .max(policy.linear_relative_tolerance * rhs_norm);
    if result.info != 0 || !defect.is_finite() || !tolerance.is_finite() || defect > tolerance {
        return Err(CaeError::convergence(format!(
            "nonlinear graph linear residual {defect:e} exceeds {tolerance:e}"
        )));
    }
    Ok(NonlinearGraphLinearSolution {
        direction: result.x,
        residual_norm: defect,
        iterations: result.iterations,
    })
}

pub fn solve<S: NonlinearGraphSystem + ?Sized>(
    system: &S,
    initial: &[f64],
    policy: &NonlinearGraphPolicy,
) -> CaeResult<NonlinearGraphSolution> {
    policy.validate()?;
    let n = system.dimension();
    if n == 0 || initial.len() != n || initial.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("nonlinear graph initial shape or value"));
    }
    system.check_state(initial)?;
    let mut state = initial.to_vec();
    let mut residual = checked(n, system.residual(&state)?)?;
    let initial_norm = norm(&residual);
    let tolerance = policy
        .absolute_tolerance
        .max(policy.relative_tolerance * initial_norm);
    if !initial_norm.is_finite() || !tolerance.is_finite() {
        return Err(CaeError::contract(
            "nonlinear graph residual scale overflow",
        ));
    }
    let mut linear_iterations = 0;
    for iteration in 0..=policy.maximum_newton_iterations {
        let current_norm = norm(&residual);
        if current_norm <= tolerance {
            return Ok(NonlinearGraphSolution {
                state,
                residual_norm: current_norm,
                newton_iterations: iteration,
                linear_iterations,
            });
        }
        if iteration == policy.maximum_newton_iterations {
            break;
        }
        let rhs: Vec<_> = residual.iter().map(|v| -v).collect();
        let coordinates = system.newton_fixed_coordinates(&state, &residual)?;
        let linear = crate::fixed_coordinate_graph::solve_newton_with_fixed_coordinates(
            system,
            &state,
            &rhs,
            &coordinates,
            policy,
        )?;
        linear_iterations += linear.iterations;
        let direction = checked(
            n,
            system.reconstruct_newton_direction(&state, &residual, &linear.direction)?,
        )?;
        if direction != linear.direction {
            let actual = checked(n, system.jacobian_product(&state, &direction, false)?)?;
            let defect = norm(
                &actual
                    .iter()
                    .zip(&rhs)
                    .map(|(a, b)| a - b)
                    .collect::<Vec<_>>(),
            );
            let tolerance = policy
                .linear_absolute_tolerance
                .max(policy.linear_relative_tolerance * norm(&rhs));
            if !defect.is_finite() || defect > tolerance {
                return Err(CaeError::convergence(
                    "reconstructed nonlinear graph Newton direction violates linear tolerance",
                ));
            }
        }
        let search = |direction: &[f64]| -> CaeResult<Option<(Vec<f64>, Vec<f64>)>> {
            if direction.len() != n || direction.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract(
                    "nonlinear graph direction shape or value",
                ));
            }
            let mut fraction = system.admitted_step(&state, direction)?;
            if !fraction.is_finite() || fraction < 0. || fraction > 1. {
                return Err(CaeError::contract(
                    "invalid nonlinear graph admitted fraction",
                ));
            }
            if fraction == 0. {
                return Ok(None);
            }
            for _ in 0..policy.maximum_backtracking_steps {
                let trial: Vec<_> = state
                    .iter()
                    .zip(direction)
                    .map(|(x, d)| x + fraction * d)
                    .collect();
                if trial.iter().all(|v| v.is_finite()) && system.check_state(&trial).is_ok() {
                    if let Ok(next) = system.residual(&trial).and_then(|v| checked(n, v)) {
                        let next_norm = norm(&next);
                        if next_norm.is_finite()
                            && (next_norm <= tolerance
                                || next_norm
                                    <= (1. - policy.sufficient_decrease * fraction) * current_norm)
                        {
                            return Ok(Some((trial, next)));
                        }
                    }
                }
                fraction *= 0.5;
                if fraction == 0. {
                    break;
                }
            }
            Ok(None)
        };
        let mut accepted = search(&direction)?;
        if accepted.is_none() {
            if let Some(direction) = system.fallback_direction(&state, &residual)? {
                accepted = search(&direction)?;
            }
        }
        if let Some((trial, next)) = accepted {
            state = trial;
            residual = next;
        } else {
            return Err(CaeError::newton(
                "nonlinear graph Newton step did not reduce the residual",
            ));
        }
    }
    Err(CaeError::newton(
        "nonlinear graph Newton iteration limit reached",
    ))
}
