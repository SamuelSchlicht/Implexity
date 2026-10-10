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

use crate::gas_vjp::{ReverseScalar, scoped_pullback};
use crate::native_gas_port_coupling::NativeGasPortConnector;
use crate::native_gas_stress_trace::NativeGasStressTrace;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::lattice::Lattice;
use std::sync::Arc;

pub struct NativeContinuousAcceptedStageCotangent {
    pub stress_trace_pa: Vec<f64>,
    pub mass_momentum_change: Vec<[f64; 4]>,
    pub energy_to_thermostat_j: Vec<f64>,
}
pub struct NativeContinuousAcceptedStageBars {
    pub accepted_pre_collision_rows: Vec<(usize, Vec<f64>)>,
    pub accepted_post_collision_rows: Vec<(usize, Vec<f64>)>,
    pub accepted_relaxation_rates: Vec<(usize, f64)>,
    pub velocity_offsets_m_s: Vec<[f64; 3]>,
}
pub fn accepted_port_stage_vjp<const Q: usize, L: Lattice<Q>>(
    connector: &NativeGasPortConnector,
    accepted_pre_collision_rows: &[(usize, Vec<f64>)],
    accepted_relaxation_rates: &[(usize, f64)],
    accepted_post_collision_rows: &[(usize, Vec<f64>)],
    velocity_offsets_m_s: &[[f64; 3]],
    bars: &NativeContinuousAcceptedStageCotangent,
) -> CaeResult<NativeContinuousAcceptedStageBars> {
    let ports = connector.exchange().port_count();
    if bars.stress_trace_pa.len() != 6 * ports
        || bars.mass_momentum_change.len() != ports
        || bars.energy_to_thermostat_j.len() != ports
        || bars
            .stress_trace_pa
            .iter()
            .chain(bars.mass_momentum_change.iter().flatten())
            .chain(&bars.energy_to_thermostat_j)
            .any(|v| !v.is_finite())
    {
        return Err(CaeError::contract("accepted port stage cotangent invalid"));
    }
    let stress = NativeGasStressTrace::new(
        Arc::new(connector.exchange().clone()),
        connector.reference_pressure_pa(),
    )?;
    stress.accepted_pre_collision_trace::<f64, Q, L>(
        accepted_pre_collision_rows,
        accepted_relaxation_rates,
    )?;
    connector.collision_exchange::<f64, Q, L>(
        accepted_pre_collision_rows,
        accepted_post_collision_rows,
        velocity_offsets_m_s,
    )?;
    let mut result = NativeContinuousAcceptedStageBars {
        accepted_pre_collision_rows: accepted_pre_collision_rows
            .iter()
            .map(|(c, r)| (*c, vec![0.; r.len()]))
            .collect(),
        accepted_post_collision_rows: accepted_post_collision_rows
            .iter()
            .map(|(c, r)| (*c, vec![0.; r.len()]))
            .collect(),
        accepted_relaxation_rates: accepted_relaxation_rates
            .iter()
            .map(|(c, _)| (*c, 0.))
            .collect(),
        velocity_offsets_m_s: vec![[0.; 3]; ports],
    };
    for (port_index, port) in connector.exchange().ports().iter().enumerate() {
        let mut indices = port
            .iter()
            .map(|(cell, _)| {
                accepted_pre_collision_rows
                    .binary_search_by_key(cell, |row| row.0)
                    .map_err(|_| CaeError::contract("accepted port stage cell absent"))
            })
            .collect::<CaeResult<Vec<_>>>()?;
        indices.sort_unstable();
        let cells = indices
            .iter()
            .map(|i| accepted_pre_collision_rows[*i].0)
            .collect::<Vec<_>>();
        let local = connector.port_subset(&[port_index])?;
        let local_stress = NativeGasStressTrace::new(
            Arc::new(local.exchange().clone()),
            local.reference_pressure_pa(),
        )?;
        let n = cells.len();
        let mut input = Vec::with_capacity(n * (2 * Q + 1) + 3);
        for &i in &indices {
            input.extend(&accepted_pre_collision_rows[i].1);
        }
        for &i in &indices {
            input.extend(&accepted_post_collision_rows[i].1);
        }
        for &i in &indices {
            input.push(accepted_relaxation_rates[i].1);
        }
        input.extend(velocity_offsets_m_s[port_index]);
        let gradient = scoped_pullback(&input, |x| {
            let pre = cells
                .iter()
                .enumerate()
                .map(|(i, c)| (*c, x[i * Q..(i + 1) * Q].to_vec()))
                .collect::<Vec<_>>();
            let post = cells
                .iter()
                .enumerate()
                .map(|(i, c)| (*c, x[n * Q + i * Q..n * Q + (i + 1) * Q].to_vec()))
                .collect::<Vec<_>>();
            let rates = cells
                .iter()
                .enumerate()
                .map(|(i, c)| (*c, x[2 * n * Q + i]))
                .collect::<Vec<_>>();
            let offsets = [std::array::from_fn(|a| x[2 * n * Q + n + a])];
            let trace =
                local_stress.accepted_pre_collision_trace::<ReverseScalar, Q, L>(&pre, &rates)?;
            let collision =
                local.collision_exchange::<ReverseScalar, Q, L>(&pre, &post, &offsets)?;
            let mut objective = ReverseScalar::zero();
            for (a, v) in trace.into_iter().enumerate() {
                objective += v * bars.stress_trace_pa[6 * port_index + a];
            }
            for a in 0..4 {
                objective +=
                    collision.mass_momentum_change[0][a] * bars.mass_momentum_change[port_index][a];
            }
            objective +=
                collision.energy_to_thermostat_j[0] * bars.energy_to_thermostat_j[port_index];
            Ok(objective)
        })?;
        for (j, &i) in indices.iter().enumerate() {
            for q in 0..Q {
                result.accepted_pre_collision_rows[i].1[q] += gradient[j * Q + q];
                result.accepted_post_collision_rows[i].1[q] += gradient[n * Q + j * Q + q];
            }
            result.accepted_relaxation_rates[i].1 += gradient[2 * n * Q + j];
        }
        for a in 0..3 {
            result.velocity_offsets_m_s[port_index][a] += gradient[2 * n * Q + n + a];
        }
    }
    if result
        .accepted_pre_collision_rows
        .iter()
        .flat_map(|r| &r.1)
        .chain(
            result
                .accepted_post_collision_rows
                .iter()
                .flat_map(|r| &r.1),
        )
        .chain(result.accepted_relaxation_rates.iter().map(|r| &r.1))
        .chain(result.velocity_offsets_m_s.iter().flatten())
        .any(|v| !v.is_finite())
    {
        return Err(CaeError::contract("accepted port stage pullback overflow"));
    }
    Ok(result)
}
