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

use crate::gas_continuous_layout::ContinuousGasRepresentation;
use crate::gas_continuous_mixed_solver::{ContinuousMixedStep, ContinuousMixedSystem};
use crate::gas_quadratic_network_solver::QuadraticGasNetworkSolvePolicy;
use implexity_core::{CaeError, CaeResult};
use implexity_solve::diagonal_graph::DiagonalGraph;
use implexity_solve::nonlinear_graph::{solve_linear, NonlinearGraphSystem};

pub fn accepted_inventory_jvp(
    system: &ContinuousMixedSystem,
    step: &ContinuousMixedStep,
    policy: &QuadraticGasNetworkSolvePolicy,
    physical_parameter_residual_direction: &[f64],
) -> CaeResult<Vec<f64>> {
    let current = system.pack(&step.inventory)?;
    if current.is_empty() {
        active_parameter_residual(system, physical_parameter_residual_direction)?;
        return Ok(vec![0.; step.inventory.len()]);
    }
    let constrained = system.build_constrained_graph(policy)?;
    let scaled = constrained.scaled_graph()?;
    let state = scaled.from_inner_state(&current)?;
    verify_accepted(&scaled, &state, step)?;
    let direction = active_parameter_residual(system, physical_parameter_residual_direction)?;
    if physical_interior_certificate(system, step, policy)?.is_some() {
        let raw = DiagonalGraph::new(
            system,
            constrained.scales().to_vec(),
            constrained.scales().to_vec(),
        )?;
        let raw_state = raw.from_inner_state(&current)?;
        let rhs = direction
            .iter()
            .zip(constrained.scales())
            .map(|(v, scale)| -v / scale)
            .collect::<Vec<_>>();
        let solved = solve_linear(&raw, &raw_state, &rhs, false, &policy.nonlinear)?;
        return system.unpack(&raw.to_inner_state(&solved.direction)?);
    }
    let graph_direction = constrained
        .physical_graph()
        .physical_residual_jvp(&current, &direction)?;
    let rhs = graph_direction
        .iter()
        .zip(constrained.scales())
        .map(|(v, scale)| -v / scale)
        .collect::<Vec<_>>();
    let solved = solve_linear(&scaled, &state, &rhs, false, &policy.nonlinear)?;
    system.unpack(&scaled.to_inner_state(&solved.direction)?)
}

pub fn accepted_physical_residual_vjp(
    system: &ContinuousMixedSystem,
    step: &ContinuousMixedStep,
    policy: &QuadraticGasNetworkSolvePolicy,
    inventory_bar: &[f64],
) -> CaeResult<Vec<f64>> {
    if inventory_bar.len() != system.network().layout().state_size()
        || inventory_bar.iter().any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(
            "mixed accepted inventory cotangent invalid",
        ));
    }
    let current = system.pack(&step.inventory)?;
    if current.is_empty() {
        return Ok(vec![0.; step.inventory.len()]);
    }
    let constrained = system.build_constrained_graph(policy)?;
    let scaled = constrained.scaled_graph()?;
    let state = scaled.from_inner_state(&current)?;
    verify_accepted(&scaled, &state, step)?;
    let rhs = system
        .active_columns()
        .iter()
        .zip(constrained.scales())
        .map(|(column, scale)| inventory_bar[*column] * scale)
        .collect::<Vec<_>>();
    if physical_interior_certificate(system, step, policy)?.is_some() {
        let raw = DiagonalGraph::new(
            system,
            constrained.scales().to_vec(),
            constrained.scales().to_vec(),
        )?;
        let raw_state = raw.from_inner_state(&current)?;
        let solved = solve_linear(&raw, &raw_state, &rhs, true, &policy.nonlinear)?;
        let physical_bar = solved
            .direction
            .iter()
            .zip(constrained.scales())
            .map(|(v, scale)| -v / scale)
            .collect::<Vec<_>>();
        return system.unpack(&physical_bar);
    }
    let solved = solve_linear(&scaled, &state, &rhs, true, &policy.nonlinear)?;
    let graph_bar = solved
        .direction
        .iter()
        .zip(constrained.scales())
        .map(|(v, scale)| -v / scale)
        .collect::<Vec<_>>();
    let physical_bar = constrained
        .physical_graph()
        .physical_residual_vjp(&current, &graph_bar)?;
    system.unpack(&physical_bar)
}

fn active_parameter_residual(
    system: &ContinuousMixedSystem,
    direction: &[f64],
) -> CaeResult<Vec<f64>> {
    if direction.len() != system.network().layout().state_size()
        || direction.iter().any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(
            "mixed accepted parameter residual invalid",
        ));
    }
    let mut active = vec![false; direction.len()];
    for column in system.active_columns() {
        active[*column] = true;
    }
    if direction
        .iter()
        .zip(active)
        .any(|(v, active)| !active && *v != 0.)
    {
        return Err(CaeError::contract(
            "mixed closed event has no ordinary parameter derivative",
        ));
    }
    Ok(system
        .active_columns()
        .iter()
        .map(|c| direction[*c])
        .collect())
}

fn verify_accepted<S: NonlinearGraphSystem>(
    scaled: &S,
    state: &[f64],
    step: &ContinuousMixedStep,
) -> CaeResult<()> {
    let residual = scaled.residual(state)?;
    let norm = residual.iter().fold(0f64, |s, v| s.hypot(*v));
    let recorded = step.scaled_kkt_residual_norm;
    let state_norm = state.iter().fold(0f64, |s, v| s.hypot(*v));
    let allowance = 256. * f64::EPSILON * state_norm.max(1.);
    if !recorded.is_finite() || recorded < 0. || (norm - recorded).abs() > allowance {
        return Err(CaeError::contract(
            "mixed accepted KKT certificate differs from current graph",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct PhysicalInteriorCertificate {
    pub minimum_kinetic_coefficient_inventory: Option<f64>,
    pub maximum_relative_weak_backward_error: f64,
    pub maximum_relative_moment_backward_error: f64,
    pub numerical_scaled_residual_norm: f64,
    pub numerical_backward_error_bound: f64,
}

pub fn physical_interior_certificate(
    system: &ContinuousMixedSystem,
    step: &ContinuousMixedStep,
    policy: &QuadraticGasNetworkSolvePolicy,
) -> CaeResult<Option<PhysicalInteriorCertificate>> {
    policy.nonlinear.validate()?;
    if !policy.physical_moment_relative_tolerance.is_finite()
        || policy.physical_moment_relative_tolerance <= 0.
        || policy
            .physical_moment_absolute_tolerances
            .iter()
            .any(|v| !v.is_finite() || *v < 0.)
    {
        return Err(CaeError::contract(
            "mixed physical interior accuracy policy invalid",
        ));
    }
    let current = system.pack(&step.inventory)?;
    system.check_state(&current)?;
    let layout = system.network().layout();
    let basis = layout.basis();
    let mut minimum = None::<f64>;
    for cell in system.active_cells() {
        if layout.representation(*cell)? == ContinuousGasRepresentation::Kinetic {
            for coefficient in &step.inventory[layout.range(*cell)?] {
                if *coefficient == 0. {
                    return Ok(None);
                }
                if !coefficient.is_finite() || *coefficient < 0. {
                    return Err(CaeError::contract(
                        "mixed physical interior coefficient domain invalid",
                    ));
                }
                minimum = Some(minimum.map_or(*coefficient, |old| old.min(*coefficient)));
            }
        }
    }
    let evaluated = system.evaluate(&current)?;
    let rt = system.authored().gas_rt_m2_s2;
    let units = [1., rt.sqrt(), rt.sqrt(), rt.sqrt(), rt, rt, rt, rt, rt, rt];
    let mut maximum_weak = 0f64;
    let mut maximum_moment = 0f64;
    for cell in system.active_cells() {
        let range = layout.range(*cell)?;
        let residual = &evaluated.residual[range.clone()];
        let mass = layout.macro_inventory(&step.inventory, *cell)?[0]
            + layout.macro_inventory(&system.authored().previous_inventory, *cell)?[0];
        let (scales, moments) = match layout.representation(*cell)? {
            ContinuousGasRepresentation::Kinetic => {
                let sum = step.inventory[range.clone()]
                    .iter()
                    .zip(&system.authored().previous_inventory[range.clone()])
                    .map(|(a, b)| a + b)
                    .collect::<Vec<_>>();
                (
                    basis.weak_inventory(&sum)?,
                    basis.weak_moments(residual)?.to_vec(),
                )
            }
            ContinuousGasRepresentation::Continuum => (
                units[..4].iter().map(|u| mass * u).collect(),
                residual.to_vec(),
            ),
        };
        for (row, (value, scale)) in residual.iter().zip(scales).enumerate() {
            if !value.is_finite() || !scale.is_finite() || scale < 0. {
                return Err(CaeError::contract(
                    "mixed physical weak backward scale invalid",
                ));
            }
            if scale == 0. {
                if *value != 0. {
                    return Err(CaeError::contract(
                        "mixed physical zero weak scale has nonzero residual",
                    ));
                }
            } else {
                let relative = value.abs() / scale;
                if !relative.is_finite() || relative > policy.physical_moment_relative_tolerance {
                    return Err(CaeError::contract(format!("mixed physical interior weak backward error at cell {cell}, row {row}, relative {relative:.17e}, limit {:.17e}",policy.physical_moment_relative_tolerance)));
                }
                maximum_weak = maximum_weak.max(relative);
            }
        }
        for (row, value) in moments.iter().enumerate() {
            let scale = mass * units[row];
            let tolerance = policy.physical_moment_absolute_tolerances[row]
                + policy.physical_moment_relative_tolerance * scale;
            if !scale.is_finite() || !tolerance.is_finite() || scale < 0. || !value.is_finite() {
                return Err(CaeError::contract(
                    "mixed physical moment backward scale invalid",
                ));
            }
            if scale == 0. {
                if *value != 0. {
                    return Err(CaeError::contract(
                        "mixed physical zero moment scale has nonzero residual",
                    ));
                }
            } else {
                if value.abs() > tolerance {
                    return Err(CaeError::contract(
                        "mixed physical interior moment backward error exceeds accuracy",
                    ));
                }
                maximum_moment = maximum_moment.max(value.abs() / scale);
            }
        }
    }
    let constrained = system.build_constrained_graph(policy)?;
    let scales = constrained.scales();
    let residual = system.residual(&current)?;
    let mut residual_norm = 0f64;
    let mut state_norm = 0f64;
    for (row, ((value, state), scale)) in residual.iter().zip(&current).zip(scales).enumerate() {
        let r = value / scale;
        let x = state / scale;
        let tolerance =
            policy.nonlinear.absolute_tolerance + policy.nonlinear.relative_tolerance * x.abs();
        if !r.is_finite() || !x.is_finite() || !tolerance.is_finite() || r.abs() > tolerance {
            return Err(CaeError::contract(format!("mixed physical interior numerical backward error at row {row}, residual {r:.17e}, bound {tolerance:.17e}")));
        }
        residual_norm = residual_norm.hypot(r);
        state_norm = state_norm.hypot(x);
    }
    let bound =
        policy.nonlinear.absolute_tolerance + policy.nonlinear.relative_tolerance * state_norm;
    if !bound.is_finite() || residual_norm > bound {
        return Err(CaeError::contract(format!("mixed physical interior numerical norm backward error {residual_norm:.17e} exceeds {bound:.17e}")));
    }
    Ok(Some(PhysicalInteriorCertificate {
        minimum_kinetic_coefficient_inventory: minimum,
        maximum_relative_weak_backward_error: maximum_weak,
        maximum_relative_moment_backward_error: maximum_moment,
        numerical_scaled_residual_norm: residual_norm,
        numerical_backward_error_bound: bound,
    }))
}
