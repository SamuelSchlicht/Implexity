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

use crate::gas_transient_kinetic::{
    TransientGasPolicy, TransientGasScalar, TransientVelocityGrid, moments,
    velocity_equilibrium_density,
};
use implexity_core::{CaeError, CaeResult};

pub struct WeakForceIncrement<S> {
    pub inventory_increment_kg: Vec<S>,
    pub mechanical_work_j: S,
    pub kinetic_energy_increment_j: S,
    pub heat_bath_energy_j: S,
}

pub fn entropy_tangent_force<S: TransientGasScalar>(
    grid: &TransientVelocityGrid,
    inventory_kg: &[S],
    volume_m3: S,
    gas_rt_m2_s2: S,
    step_s: S,
    force_density_n_m3: [S; 3],
    policy: &TransientGasPolicy,
) -> CaeResult<WeakForceIncrement<S>> {
    let fail = || CaeError::contract("invalid entropy-tangent weak-force state");
    if inventory_kg.len() != grid.velocity_m_s.len()
        || inventory_kg
            .iter()
            .any(|q| !q.finite_first_order() || q.value() < 0.)
        || [volume_m3, gas_rt_m2_s2, step_s]
            .iter()
            .any(|x| !x.finite_first_order() || x.value() <= 0.)
        || force_density_n_m3.iter().any(|f| !f.finite_first_order())
    {
        return Err(fail());
    }
    let m = moments(inventory_kg, grid);
    if !m.mass_kg.finite_first_order() || m.mass_kg.value() <= 0. {
        return Err(CaeError::contract(
            "entropy-tangent force requires positive resolved gas mass",
        ));
    }
    let u = m.momentum_kg_m_s.map(|j| j / m.mass_kg);
    let g = velocity_equilibrium_density(grid, m.mass_kg / volume_m3, u, gas_rt_m2_s2, policy)?;
    let mut covariance = [[S::zero(); 3]; 3];
    for (c, g) in grid.velocity_m_s.iter().zip(&g) {
        for i in 0..3 {
            for j in 0..3 {
                covariance[i][j] += *g * volume_m3 / m.mass_kg
                    * (S::from_f64(c[i]) - u[i])
                    * (S::from_f64(c[j]) - u[j]);
            }
        }
    }
    let a = covariance;
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if !det.finite_first_order() || det.value() <= 0. {
        return Err(fail());
    }
    let inverse = [
        [
            a[1][1] * a[2][2] - a[1][2] * a[2][1],
            a[0][2] * a[2][1] - a[0][1] * a[2][2],
            a[0][1] * a[1][2] - a[0][2] * a[1][1],
        ],
        [
            a[1][2] * a[2][0] - a[1][0] * a[2][2],
            a[0][0] * a[2][2] - a[0][2] * a[2][0],
            a[0][2] * a[1][0] - a[0][0] * a[1][2],
        ],
        [
            a[1][0] * a[2][1] - a[1][1] * a[2][0],
            a[0][1] * a[2][0] - a[0][0] * a[2][1],
            a[0][0] * a[1][1] - a[0][1] * a[1][0],
        ],
    ];
    let impulse = force_density_n_m3.map(|f| f * step_s * volume_m3);
    let coefficient: [S; 3] = std::array::from_fn(|i| {
        (0..3).map(|j| inverse[i][j] * impulse[j]).sum::<S>() / (det * m.mass_kg)
    });
    let increment: Vec<S> = grid
        .velocity_m_s
        .iter()
        .zip(&g)
        .map(|(c, g)| {
            *g * volume_m3
                * (0..3)
                    .map(|i| (S::from_f64(c[i]) - u[i]) * coefficient[i])
                    .sum::<S>()
        })
        .collect();
    let work = (0..3).map(|i| impulse[i] * u[i]).sum::<S>();
    let energy = moments(&increment, grid).kinetic_energy_j;
    let heat = work - energy;
    if increment.iter().any(|q| !q.finite_first_order())
        || [work, energy, heat].iter().any(|q| !q.finite_first_order())
    {
        return Err(fail());
    }
    Ok(WeakForceIncrement {
        inventory_increment_kg: increment,
        mechanical_work_j: work,
        kinetic_energy_increment_j: energy,
        heat_bath_energy_j: heat,
    })
}
