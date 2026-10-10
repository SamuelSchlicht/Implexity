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

use crate::gas_transient_kinetic::solve_scalar;
pub use crate::gas_transient_kinetic::{
    velocity_equilibrium_density, TransientGasBoundary, TransientGasInputs,
    TransientGasPolicy, TransientGasScalar, TransientGasSolution, TransientGasFace, TransientVelocityGrid,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{lattice::Lattice, port_exchange::PortExchange};

#[derive(Clone, Debug)]
pub struct KineticPortStep<S> {
    pub bulk_populations: Vec<S>,
    pub gas: TransientGasSolution<S>,
    pub raw_bulk_increments_kg_kg_m_s: Vec<[S; 4]>,
    pub exchange_mass_momentum_defect: [S; 4],
    pub bulk_projection_heat_j: S,
}

pub fn advance<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
    exchange: &PortExchange,
    bulk_populations: &[S],
    port_velocity_offset_m_s: &[[S; 3]],
    reference_pressure_pa: f64,
    inputs: &TransientGasInputs<S>,
    policy: &TransientGasPolicy,
) -> CaeResult<KineticPortStep<S>> {
    let fail = |s: &str| CaeError::contract(s);
    if port_velocity_offset_m_s.len() != exchange.port_count()
        || port_velocity_offset_m_s
            .iter()
            .flatten()
            .any(|v| !v.finite_first_order())
    {
        return Err(fail("kinetic port velocity offset shape or value"));
    }
    let states = exchange.macroscopic_states::<S, Q, L>(bulk_populations, reference_pressure_pa)?;
    let mut configured = inputs.clone();
    configured.port_density_kg_m3.clear();
    for (state, offset) in states.iter().zip(port_velocity_offset_m_s) {
        let native_rt = state.absolute_pressure_pa / state.density_kg_m3;
        let mismatch = inputs.gas_rt_m2_s2 - native_rt;
        let tolerance = 128. * f64::EPSILON * inputs.gas_rt_m2_s2.value().abs();
        if !mismatch.finite_first_order()
            || mismatch.value().abs() > tolerance
            || mismatch.maximum_direction() > tolerance
        {
            return Err(fail(
                "kinetic and bulk thermodynamic scales or directions differ",
            ));
        }
        let velocity =
            std::array::from_fn(|axis| state.population_velocity_m_s[axis] + offset[axis]);
        configured
            .port_density_kg_m3
            .push(velocity_equilibrium_density(
                &inputs.grid,
                state.density_kg_m3,
                velocity,
                inputs.gas_rt_m2_s2,
                policy,
            )?);
    }
    let gas = solve_scalar(&configured, policy)?;
    let mut increments = vec![[S::zero(); 4]; exchange.port_count()];
    let mut emitted_energy = S::zero();
    for (face, packet) in configured.faces.iter().zip(&gas.packets) {
        if let TransientGasBoundary::Port { port } = face.boundary {
            let increment = increments
                .get_mut(port)
                .ok_or_else(|| fail("kinetic face bulk port absent"))?;
            increment[0] += packet.mass_kg;
            for axis in 0..3 {
                increment[axis + 1] += packet.momentum_kg_m_s[axis];
            }
            emitted_energy += packet.kinetic_energy_j;
        }
    }
    let raw_increments = increments
        .iter()
        .zip(port_velocity_offset_m_s)
        .map(|(packet, offset)| {
            let mut raw = *packet;
            for axis in 0..3 {
                raw[axis + 1] -= packet[0] * offset[axis];
            }
            raw
        })
        .collect::<Vec<_>>();
    let next = exchange.apply::<S, Q, L>(bulk_populations, &raw_increments)?;
    let actual =
        exchange.exchanged_moments::<S, Q, L>(bulk_populations, &next, port_velocity_offset_m_s)?;
    let defect = std::array::from_fn(|axis| {
        actual[axis]
            - increments
                .iter()
                .map(|p| p[axis])
                .fold(S::zero(), |a, b| a + b)
    });
    let heat = emitted_energy - actual[4];
    if defect
        .iter()
        .chain([&heat])
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("kinetic bulk exchange balance overflow"));
    }
    Ok(KineticPortStep {
        bulk_populations: next,
        gas,
        raw_bulk_increments_kg_kg_m_s: raw_increments,
        exchange_mass_momentum_defect: defect,
        bulk_projection_heat_j: heat,
    })
}
