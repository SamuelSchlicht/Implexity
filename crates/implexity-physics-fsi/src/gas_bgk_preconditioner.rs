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
    TransientGasPolicy, TransientVelocityGrid, moments, velocity_equilibrium_density,
};
use implexity_ad::Dual;
use implexity_core::{CaeError, CaeResult};

#[derive(Clone, Debug)]
pub struct BgkCollisionPreconditioner {
    velocity_m_s: Vec<[f64; 3]>,
    equilibrium_columns: Vec<[f64; 4]>,
    nonequilibrium_kg: Vec<f64>,
    inverse_diagonal: f64,
    equilibrium_factor: f64,
    density_factor: f64,
}
impl BgkCollisionPreconditioner {
    pub fn new(
        grid: &TransientVelocityGrid,
        current_inventory_kg: &[f64],
        volume_m3: f64,
        gas_rt_m2_s2: f64,
        viscosity_pa_s: f64,
        step_s: f64,
        policy: &TransientGasPolicy,
    ) -> CaeResult<Self> {
        if current_inventory_kg.len() != grid.velocity_m_s.len()
            || current_inventory_kg
                .iter()
                .any(|q| !q.is_finite() || *q < 0.)
            || ![volume_m3, gas_rt_m2_s2, viscosity_pa_s, step_s]
                .iter()
                .all(|v| v.is_finite() && *v > 0.)
        {
            return Err(CaeError::contract("invalid local BGK preconditioner input"));
        }
        let m = moments(current_inventory_kg, grid);
        if !m.mass_kg.is_finite()
            || m.mass_kg <= 0.
            || m.momentum_kg_m_s.iter().any(|j| !j.is_finite())
        {
            return Err(CaeError::contract(
                "local BGK preconditioner requires positive resolved mass",
            ));
        }
        let lift = |value: f64, index: usize| {
            let mut direction = [0.; 4];
            direction[index] = 1.;
            Dual::new(value, direction)
        };
        let mass = lift(m.mass_kg, 0);
        let momentum = std::array::from_fn(|axis| lift(m.momentum_kg_m_s[axis], axis + 1));
        let equilibrium = velocity_equilibrium_density(
            grid,
            mass / volume_m3,
            momentum.map(|j| j / mass),
            Dual::constant(gas_rt_m2_s2),
            policy,
        )?;
        let gamma = gas_rt_m2_s2 / (viscosity_pa_s * volume_m3);
        let delta = step_s * gamma * m.mass_kg;
        let inverse_diagonal = 1. / (1. + delta);
        let equilibrium_factor = delta * inverse_diagonal;
        let density_factor = step_s * gamma * inverse_diagonal;
        let equilibrium_columns = equilibrium
            .iter()
            .map(|g| g.eps.map(|v| v * volume_m3))
            .collect::<Vec<_>>();
        let nonequilibrium_kg = equilibrium
            .iter()
            .zip(current_inventory_kg)
            .map(|(g, q)| g.re * volume_m3 - q)
            .collect::<Vec<_>>();
        if ![
            gamma,
            delta,
            inverse_diagonal,
            equilibrium_factor,
            density_factor,
        ]
        .iter()
        .all(|v| v.is_finite())
            || equilibrium_columns
                .iter()
                .flatten()
                .chain(&nonequilibrium_kg)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("local BGK preconditioner overflow"));
        }
        Ok(Self {
            velocity_m_s: grid.velocity_m_s.clone(),
            equilibrium_columns,
            nonequilibrium_kg,
            inverse_diagonal,
            equilibrium_factor,
            density_factor,
        })
    }
    pub fn apply(&self, direction: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if direction.len() != self.velocity_m_s.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "local BGK preconditioner direction shape or value",
            ));
        }
        let mut out = Vec::with_capacity(direction.len());
        if transpose {
            let mut projected = [0.; 4];
            for (columns, value) in self.equilibrium_columns.iter().zip(direction) {
                for j in 0..4 {
                    projected[j] += columns[j] * value;
                }
            }
            let density = self
                .nonequilibrium_kg
                .iter()
                .zip(direction)
                .map(|(q, v)| q * v)
                .sum::<f64>();
            for (velocity, value) in self.velocity_m_s.iter().zip(direction) {
                out.push(
                    self.inverse_diagonal * value
                        + self.equilibrium_factor
                            * (projected[0]
                                + (0..3).map(|j| velocity[j] * projected[j + 1]).sum::<f64>())
                        + self.density_factor * density,
                );
            }
        } else {
            let mut moment = [0.; 4];
            for (velocity, value) in self.velocity_m_s.iter().zip(direction) {
                moment[0] += value;
                for j in 0..3 {
                    moment[j + 1] += velocity[j] * value;
                }
            }
            for ((columns, value), w) in self
                .equilibrium_columns
                .iter()
                .zip(direction)
                .zip(&self.nonequilibrium_kg)
            {
                out.push(
                    self.inverse_diagonal * value
                        + self.equilibrium_factor
                            * (0..4).map(|j| columns[j] * moment[j]).sum::<f64>()
                        + self.density_factor * w * moment[0],
                );
            }
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "local BGK preconditioner action overflow",
            ));
        }
        Ok(out)
    }
}
