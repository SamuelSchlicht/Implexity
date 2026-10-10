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

use crate::gas_quadratic_gaussian::QuadraticGaussianBasis;
use crate::gas_quadratic_velocity::QuadraticEntropyPolicy;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_core::{CaeError, CaeResult};

pub struct QuadraticGasBalance<S> {
    pub weak_residual: Vec<S>,
    pub physical_moment_residual: [S; 10],
    pub collision_coefficient_increment: Vec<S>,
    pub collision_thermal_bath_energy_j: S,
    pub face_ten_moment_packets: Vec<[S; 10]>,
}
pub fn coefficient_balance<S: TransientGasScalar>(
    gaussian: &QuadraticGaussianBasis,
    previous: &[S],
    current: &[S],
    volume_m3: S,
    gas_rt_m2_s2: S,
    molecular_viscosity_pa_s: S,
    step_s: S,
    face_weak_inventory_packets: &[Vec<S>],
    entropy_policy: &QuadraticEntropyPolicy,
) -> CaeResult<QuadraticGasBalance<S>> {
    let basis = gaussian.basis();
    let fail = |s| CaeError::contract(s);
    let n = basis.coefficient_count();
    if previous.len() != n
        || current.len() != n
        || previous
            .iter()
            .chain(current)
            .any(|v| !v.finite_first_order() || v.value() < 0.)
        || [volume_m3, gas_rt_m2_s2, molecular_viscosity_pa_s, step_s]
            .iter()
            .any(|v| !v.finite_first_order())
        || volume_m3.value() < 0.
        || gas_rt_m2_s2.value() <= 0.
        || molecular_viscosity_pa_s.value() <= 0.
        || step_s.value() <= 0.
        || face_weak_inventory_packets
            .iter()
            .any(|p| p.len() != n || p.iter().any(|v| !v.finite_first_order()))
    {
        return Err(fail("invalid continuous gas balance state or packet"));
    }
    let zero = S::zero();
    let mut increment = vec![zero; n];
    if volume_m3.value() == 0. {
        if volume_m3.has_direction()
            || previous
                .iter()
                .chain(current)
                .any(|v| v.value() != 0. || v.has_direction())
            || face_weak_inventory_packets
                .iter()
                .flatten()
                .any(|v| v.value() != 0. || v.has_direction())
        {
            return Err(fail(
                "closed continuous gas balance has inventory or unresolved event direction",
            ));
        }
    } else {
        let moments = basis.coefficient_moments(current)?;
        if moments[0].value() > 0. {
            let rho = moments[0] / volume_m3;
            let u = std::array::from_fn(|j| moments[j + 1] / moments[0]);
            let eq = gaussian
                .equilibrium(rho, u, gas_rt_m2_s2, entropy_policy)?
                .coefficient_density;
            let mut change = eq
                .iter()
                .zip(current)
                .map(|(g, a)| *g * volume_m3 - *a)
                .collect::<Vec<_>>();
            let eq_moments = gaussian.coefficient_moments(&eq)?;
            for _ in 0..2 {
                let mut error = [zero; 20];
                for (k, row) in basis.integrated_moment_rows().iter().enumerate() {
                    for j in 0..4 {
                        error[j] += change[k] * row[j];
                    }
                }
                let correction =
                    gaussian.moment_direction(&eq, eq_moments, error, entropy_policy)?;
                for k in 0..n {
                    change[k] -= correction[k];
                }
            }
            let frequency = rho * gas_rt_m2_s2 / molecular_viscosity_pa_s;
            for k in 0..n {
                increment[k] = step_s * frequency * change[k];
            }
        }
    }
    let mut difference = current
        .iter()
        .zip(previous)
        .zip(&increment)
        .map(|((a, b), d)| *a - *b - *d)
        .collect::<Vec<_>>();
    let mut residual = basis.mass_apply(&difference)?;
    let mut packets = Vec::with_capacity(face_weak_inventory_packets.len());
    for packet in face_weak_inventory_packets {
        for k in 0..n {
            residual[k] += packet[k];
        }
        packets.push(basis.weak_moments(packet)?);
    }
    difference.clear();
    let physical = basis.weak_moments(&residual)?;
    let mut collision_moments = [zero; 10];
    for (k, row) in basis.integrated_moment_rows().iter().enumerate() {
        for j in 0..10 {
            collision_moments[j] += increment[k] * row[j];
        }
    }
    let heat = -(collision_moments[4] + collision_moments[5] + collision_moments[6]) * 0.5;
    if residual
        .iter()
        .chain(physical.iter())
        .chain(increment.iter())
        .chain(std::iter::once(&heat))
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("continuous gas balance is nonfinite"));
    }
    Ok(QuadraticGasBalance {
        weak_residual: residual,
        physical_moment_residual: physical,
        collision_coefficient_increment: increment,
        collision_thermal_bath_energy_j: heat,
        face_ten_moment_packets: packets,
    })
}
