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

use crate::native_continuous_coupled_field::NativeContinuousCoupledField;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    field::LatticeKind,
    lattice::{D2Q9, D3Q19, D3Q27},
};
use implexity_solve::multirate_coupling::SubcycledField;
use std::collections::BTreeSet;

impl NativeContinuousCoupledField {
    pub fn initial_state_pullback(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        let c = &self.config;
        let bars = self.checkpoint.split(cotangent)?;
        if let Some(closed) = c.initial_closed_release_certificate.as_deref() {
            c.geometry
                .certify_initial_design_stratum_with_closed_release(
                    &c.initial_trace,
                    design,
                    closed,
                )?;
        } else {
            c.geometry
                .certify_initial_design_stratum(&c.initial_trace, design)?;
        }
        let bulk = c.bulk.initial_state(design)?;
        let stress_bars = match c.bulk.lattice() {
            LatticeKind::D2Q9 => self
                .ports
                .initial_stress_vjp::<9, D2Q9>(&bulk, bars.stress_trace_pa)?,
            LatticeKind::D3Q19 => self
                .ports
                .initial_stress_vjp::<19, D3Q19>(&bulk, bars.stress_trace_pa)?,
            LatticeKind::D3Q27 => self
                .ports
                .initial_stress_vjp::<27, D3Q27>(&bulk, bars.stress_trace_pa)?,
        };
        let bulk_bars = bars
            .bulk
            .iter()
            .zip(stress_bars)
            .map(|(a, b)| a + b)
            .collect::<Vec<_>>();
        let mut result = c.bulk.initial_state_vjp(design, &bulk_bars)?;
        let path = if let Some(closed) = c.initial_closed_release_certificate.as_deref() {
            c.geometry.assemble_geometry_path_with_closed_release(
                &c.initial_trace,
                &c.initial_trace,
                design,
                1.,
                c.bulk.config().macro_step_s,
                &c.boundaries,
                closed,
            )?
        } else {
            c.geometry.assemble_geometry_path(
                &c.initial_trace,
                &c.initial_trace,
                design,
                c.bulk.config().macro_step_s,
                &c.boundaries,
            )?
        };
        let endpoint = &path.old_geometry;
        path.certify_stationary_continuum_slots(&self.continuum_slots)?;
        let density = self.ports.initial_gas_inventory(
            &[1.],
            c.initial_density_kg_m3,
            c.initial_velocity_m_s,
            c.gas_rt_m2_s2,
        )?;
        let density = density[0].as_ref().ok_or_else(|| {
            CaeError::contract("positive initial native volume has no coefficient density")
        })?;
        let mut volume_bars = vec![0.; c.layout.slots()];
        for slot in 0..c.layout.slots() {
            let range = c.layout.range(slot)?;
            volume_bars[slot] = if self.kinetic_mask[slot] {
                bars.gas[range]
                    .iter()
                    .zip(density)
                    .map(|(b, f)| b * f)
                    .sum()
            } else {
                c.initial_density_kg_m3
                    * (bars.gas[range.start]
                        + (0..3)
                            .map(|k| c.initial_velocity_m_s[k] * bars.gas[range.start + 1 + k])
                            .sum::<f64>())
            };
        }
        if volume_bars.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "nonfinite initial native volume cotangent",
            ));
        }
        let mut cells = BTreeSet::new();
        for (component, &slot) in endpoint
            .refined
            .partition
            .components
            .iter()
            .zip(&endpoint.aggregated.component_slots)
        {
            if volume_bars[slot] != 0. {
                cells.insert(component.cell);
            }
        }
        let certificate = if let Some(closed) = c.initial_closed_release_certificate.as_deref() {
            c.geometry
                .certify_volume_point_with_closed_release(&c.initial_trace, design, closed)?
        } else {
            c.geometry.certify_volume_point(&c.initial_trace, design)?
        };
        for cell in cells {
            let support = c.geometry.volume_design_support_selected(
                &certificate,
                &[cell],
                &c.initial_trace,
                design,
                c.initial_closed_release_certificate.as_deref(),
            )?;
            for columns in support.chunks(48) {
                let local = if let Some(closed) = c.initial_closed_release_certificate.as_deref() {
                    c.geometry
                        .checked_volume_design_pullback_with_closed_release(
                            &certificate,
                            &[cell],
                            &c.initial_trace,
                            design,
                            columns,
                            &volume_bars,
                            closed,
                        )?
                } else {
                    c.geometry.checked_volume_design_pullback(
                        &certificate,
                        &[cell],
                        &c.initial_trace,
                        design,
                        columns,
                        &volume_bars,
                    )?
                };
                for (&column, value) in columns.iter().zip(local) {
                    result[column] += value;
                }
            }
        }
        if result.len() != design.len() || result.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "invalid native continuous initial pullback",
            ));
        }
        Ok(result)
    }
}
