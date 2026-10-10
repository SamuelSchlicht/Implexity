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

use crate::gas_transient_kinetic::{TransientGasBoundary, TransientGasPacket};
use crate::native_continuous_coupled_field::NativeContinuousCoupledField;
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    field::{
        LatticeKind,
        gas_tick::{CoupledTickDirection, CoupledTickInput},
    },
    lattice::{D2Q9, D3Q19, D3Q27},
};
use implexity_solve::{
    multirate_coupling::{SubcycleTangent, SubcycledField},
    time_stepper::StepParameters,
};
macro_rules! port_call {
    ($owner:expr,$name:ident($($arg:expr),*)) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9=>$owner.ports.$name::<Dual<1>,9,D2Q9>($($arg),*),
            LatticeKind::D3Q19=>$owner.ports.$name::<Dual<1>,19,D3Q19>($($arg),*),
            LatticeKind::D3Q27=>$owner.ports.$name::<Dual<1>,27,D3Q27>($($arg),*),
        }
    }
}
macro_rules! connector_call {
    ($owner:expr,$name:ident($($arg:expr),*)) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9=>$owner.config.connector.$name::<Dual<1>,9,D2Q9>($($arg),*),
            LatticeKind::D3Q19=>$owner.config.connector.$name::<Dual<1>,19,D3Q19>($($arg),*),
            LatticeKind::D3Q27=>$owner.config.connector.$name::<Dual<1>,27,D3Q27>($($arg),*),
        }
    }
}
impl NativeContinuousCoupledField {
    pub fn tangent(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        previous_direction: &[f64],
        start_direction: &[f64],
        end_direction: &[f64],
        design_direction: &[f64],
        time_direction: f64,
    ) -> CaeResult<SubcycleTangent> {
        self.tangent_with_closed_context(
            n,
            previous,
            start,
            end,
            p,
            previous_direction,
            start_direction,
            end_direction,
            design_direction,
            time_direction,
            None,
        )
    }
    pub(crate) fn tangent_with_closed_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        previous_direction: &[f64],
        start_direction: &[f64],
        end_direction: &[f64],
        design_direction: &[f64],
        time_direction: f64,
        context: Option<&crate::native_continuous_coupled_field::NativeContinuousGeometryContext>,
    ) -> CaeResult<SubcycleTangent> {
        let c = &self.config;
        if previous_direction.len() != previous.len()
            || start_direction.len() != start.len()
            || end_direction.len() != end.len()
            || design_direction.len() != p.design.len()
            || start.len() != c.bulk.trace_size()
            || end.len() != start.len()
            || p.design.len() != c.bulk.design_size()
            || p.time_scale != 1.
            || time_direction != 0.
            || previous_direction
                .iter()
                .chain(start_direction)
                .chain(end_direction)
                .chain(design_direction)
                .chain(start)
                .chain(end)
                .chain(p.design)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "native continuous tangent requires fixed clock and matching finite directions",
            ));
        }
        match context {
            Some(
                crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Derivative(
                    certificate,
                ),
            ) => {
                certificate.validate_macro(n, p)?;
                certificate.validate_path(&c.geometry.kinematics, start, end, p.design)?;
            }
            Some(
                crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Trial(
                    certificate,
                ),
            ) => {
                certificate.validate_macro(n, p)?;
                certificate.validate_path(&c.geometry.kinematics, start, end, p.design)?;
            }
            Some(
                crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Primal(_),
            ) => {
                return Err(CaeError::contract(
                    "primal witness cannot select a native tangent",
                ));
            }
            None => {}
        }
        let macro_start = start
            .iter()
            .zip(start_direction)
            .map(|(&v, &d)| Dual::new(v, [d]))
            .collect::<Vec<_>>();
        let macro_end = end
            .iter()
            .zip(end_direction)
            .map(|(&v, &d)| Dual::new(v, [d]))
            .collect::<Vec<_>>();
        let old = self.checkpoint.split(previous)?;
        let direction = self.checkpoint.split(previous_direction)?;
        let mut bulk = old.bulk.to_vec();
        let mut dbulk = direction.bulk.to_vec();
        let mut gas = old
            .gas
            .iter()
            .zip(direction.gas)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect::<Vec<_>>();
        let mut stress = old
            .stress_trace_pa
            .iter()
            .zip(direction.stress_trace_pa)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect::<Vec<_>>();
        let design = p
            .design
            .iter()
            .zip(design_direction)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect::<Vec<_>>();
        let ticks = c.bulk.config().substeps;
        let dt = c.bulk.config().macro_step_s / ticks as f64;
        let mut flux = vec![0.; start.len()];
        let mut samples = vec![0.; c.bulk.sample_names().len()];
        for tick in 1..=ticks {
            let a = (tick - 1) as f64 / ticks as f64;
            let b = tick as f64 / ticks as f64;
            let trace_at = |weight: f64| {
                start
                    .iter()
                    .zip(end)
                    .zip(start_direction.iter().zip(end_direction))
                    .map(|((s, e), (ds, de))| {
                        Dual::new(s + (e - s) * weight, [ds + (de - ds) * weight])
                    })
                    .collect::<Vec<_>>()
            };
            let at = trace_at(a);
            let bt = trace_at(b);
            let input = CoupledTickInput {
                macro_index: n,
                tick,
                previous: &bulk,
                start,
                end,
                design: p.design,
                time_scale: 1.,
            };
            let primal_gas = gas.iter().map(|v| v.re).collect::<Vec<_>>();
            let primal_stress = stress.iter().map(|v| v.re).collect::<Vec<_>>();
            let primal =
                self.prepare_tick_with_closed_context(input, &primal_gas, &primal_stress, context)?;
            let path = match context {
                None => c.geometry.assemble_geometry_path(&at,&bt,&design,Dual::constant(dt),&c.boundaries)?,
                Some(crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Derivative(certificate)) => {
                    let law=c.closed_contact_law.as_ref().ok_or_else(||CaeError::contract("native tangent geometry lacks its contact owner"))?;
                    let sample=Arc::new(certificate.sample_child(law,a,b)?);
                    let receipt=c.geometry.project_and_issue_closed_child(&macro_start,&macro_end,&design,Dual::constant(p.time_scale),sample.clone())?;
                    let stages=self.derivative_birth_stages(n,tick,receipt)?;
                    let mut path=crate::native_gas_geometry_adapter::NativeGasGeometryAdapter::assemble_retained_birth_release_path(&c.geometry,&macro_start,&macro_end,&design,Dual::constant(p.time_scale),Dual::constant(dt),&c.boundaries,sample.clone())?;
                    self.consume_birth_stages(&mut path,&stages)?;path
                }
                Some(crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Trial(certificate)) => {
                    let law=c.closed_contact_law.as_ref().ok_or_else(||CaeError::contract("native trial tangent lacks its contact owner"))?;
                    let sample=Arc::new(certificate.sample_child(law,a,b)?);
                    let receipt=c.geometry.project_and_issue_closed_trial_child(&macro_start,&macro_end,&design,Dual::constant(p.time_scale),sample.clone())?;
                    let stages=self.trial_birth_stages(n,tick,receipt)?;
                    let mut path=crate::native_gas_geometry_adapter::NativeGasGeometryAdapter::assemble_retained_birth_trial_path(&c.geometry,&macro_start,&macro_end,&design,Dual::constant(p.time_scale),Dual::constant(dt),&c.boundaries,sample.clone())?;
                    self.consume_birth_stages(&mut path,&stages)?;path
                }
                Some(crate::native_continuous_coupled_field::NativeContinuousGeometryContext::Primal(_)) => return Err(CaeError::contract("primal witness cannot select a native tangent")),
            };
            path.certify_stationary_continuum_slots(&self.continuum_slots)?;
            self.admit_birth_inventory(&path,&gas)?;
            if path.birth_route_identity!=primal.geometry.birth_route_identity{return Err(CaeError::contract("native tangent birth route differs from accepted primal"));}
            c.layout
                .validate(&gas, &path.old_geometry.aggregated.volumes_m3)?;
            let sparse = (0..c.layout.slots())
                .map(|slot| {
                    if self.kinetic_mask[slot] && path.old_geometry.aggregated.active_slots[slot] {
                        Ok(Some(c.layout.kinetic_inventory(&gas, slot)?.to_vec()))
                    } else {
                        Ok(None)
                    }
                })
                .collect::<CaeResult<Vec<_>>>()?;
            let remap = c.geometry.layout.remap_sparse_coefficients(
                &path.old_geometry.refined,
                &path.current_geometry.refined,
                &sparse,
                c.layout.basis().coefficient_count(),
                &self.continuum_slots,
            )?;
            let mut remapped = gas.clone();
            for slot in 0..c.layout.slots() {
                if self.kinetic_mask[slot] {
                    let range = c.layout.range(slot)?;
                    remapped[range.clone()].fill(Dual::constant(0.));
                    if let Some(row) = &remap.coefficient_inventory[slot] {
                        remapped[range].copy_from_slice(row);
                    }
                }
            }
            let bulk_dual = bulk
                .iter()
                .zip(&dbulk)
                .map(|(v, d)| Dual::new(*v, [*d]))
                .collect::<Vec<_>>();
            let offsets = vec![[Dual::constant(0.); 3]; c.connector.exchange().port_count()];
            let donors = port_call!(self, donor_coefficients(&bulk_dual, &stress, &offsets))?;
            path.validate_wall_anchor_route(&primal.geometry)?;
            let network = self.prepare_network_for_path(&path, Dual::constant(dt))?;
            let current_constant = primal
                .solved
                .inventory
                .iter()
                .map(|v| Dual::constant(*v))
                .collect::<Vec<_>>();
            let parameter = network.evaluate(
                &current_constant,
                &remapped,
                &donors,
                Dual::constant(c.gas_rt_m2_s2),
                Dual::constant(c.viscosity_pa_s),
            )?;
            let rhs = parameter
                .residual
                .iter()
                .map(|v| v.eps[0])
                .collect::<Vec<_>>();
            let dgas = crate::gas_continuous_mixed_implicit::accepted_inventory_jvp(
                &primal.system,
                &primal.solved,
                &c.solve_policy,
                &rhs,
            )?;
            gas = primal
                .solved
                .inventory
                .iter()
                .zip(dgas)
                .map(|(v, d)| Dual::new(*v, [d]))
                .collect();
            let evaluated = network.evaluate(
                &gas,
                &remapped,
                &donors,
                Dual::constant(c.gas_rt_m2_s2),
                Dual::constant(c.viscosity_pa_s),
            )?;
            let mut packets = vec![[Dual::constant(0.); 10]; offsets.len()];
            let mut wall_packets = Vec::new();
            for packet in &evaluated.boundary_packets {
                if let TransientGasBoundary::Port { port } = path.faces[packet.face].boundary {
                    for j in 0..10 {
                        packets[port][j] += packet.moments[j];
                    }
                }
                wall_packets.push((
                    packet.face,
                    TransientGasPacket {
                        mass_kg: packet.moments[0],
                        momentum_kg_m_s: [packet.moments[1], packet.moments[2], packet.moments[3]],
                        kinetic_energy_j: (packet.moments[4]
                            + packet.moments[5]
                            + packet.moments[6])
                            * Dual::constant(0.5),
                        wall_work_j: packet.wall_work_j,
                        wall_heat_j: packet.wall_heat_j,
                    },
                ));
            }
            let wall = path.scatter_wall_packets_with_reference(
                &wall_packets,
                &at,
                &bt,
                Dual::constant(c.reference_pressure_pa),
            )?;
            for (f, g) in flux.iter_mut().zip(wall.load.force_flux_n) {
                *f += g.eps[0] / ticks as f64;
            }
            let reference = connector_call!(self, interface_reference_pressure(&bulk_dual))?;
            let mut reference_faces = vec![[Dual::constant(0.); 3]; packets.len()];
            for face in &path.faces {
                if let TransientGasBoundary::Port { port } = face.boundary {
                    for j in 0..3 {
                        reference_faces[port][j] += reference * face.area_m2 * face.normal[j] * dt;
                    }
                }
            }
            let projected = connector_call!(
                self,
                reference_moment_projection(&packets, &reference_faces, &self.links, reference)
            )?;
            let reflected = c.bulk.coupled_tick_reflection(input, &self.links)?;
            let dr = c
                .bulk
                .coupled_tick_reflection_tangent(input, &self.links, &dbulk, 0.)?;
            let reflection = reflected
                .iter()
                .zip(&dr)
                .map(|((cell, j), (_, dj))| {
                    let mut moment = [Dual::constant(0.); 10];
                    for k in 0..3 {
                        moment[k + 1] = Dual::new(j[k], [dj[k]]);
                    }
                    (*cell, moment)
                })
                .collect::<Vec<_>>();
            let rows = c.bulk.coupled_tick_incoming(input, &self.port_cells)?;
            let drows = c
                .bulk
                .coupled_tick_incoming_tangent(input, &self.port_cells, &dbulk)?;
            let rows_dual = rows
                .iter()
                .zip(&drows)
                .map(|((cell, row), (_, d))| {
                    (
                        *cell,
                        row.iter()
                            .zip(d)
                            .map(|(v, d)| Dual::new(*v, [*d]))
                            .collect(),
                    )
                })
                .collect::<Vec<_>>();
            let incoming = port_call!(
                self,
                incoming(&rows_dual, &projected.packets, &reflection, &offsets)
            )?;
            let accepted_rows = incoming
                .incoming_rows
                .iter()
                .map(|(cell, r)| (*cell, r.iter().map(|v| v.re).collect::<Vec<_>>()))
                .collect::<Vec<_>>();
            let accepted_drows = incoming
                .incoming_rows
                .iter()
                .map(|(cell, r)| (*cell, r.iter().map(|v| v.eps[0]).collect::<Vec<_>>()))
                .collect::<Vec<_>>();
            let rates =
                c.bulk
                    .coupled_tick_pre_collision_rates(input, &accepted_rows, &self.port_cells)?;
            let drates = c.bulk.coupled_tick_pre_collision_rates_tangent(
                input,
                &accepted_rows,
                &self.port_cells,
                &dbulk,
                &accepted_drows,
                0.,
            )?;
            let rates_dual = rates
                .iter()
                .zip(drates)
                .map(|((cell, v), (_, d))| (*cell, Dual::new(*v, [d])))
                .collect::<Vec<_>>();
            let native_direction = c.bulk.coupled_tick_with_incoming_tangent(
                input,
                CoupledTickDirection {
                    previous: &dbulk,
                    start: start_direction,
                    end: end_direction,
                    design: design_direction,
                    time_scale: 0.,
                },
                &accepted_rows,
                &accepted_drows,
            )?;
            let native = c.bulk.coupled_tick_with_incoming(input, &accepted_rows)?;
            let post = native
                .state
                .iter()
                .zip(&native_direction.state)
                .map(|(v, d)| Dual::new(*v, [*d]))
                .collect::<Vec<_>>();
            stress = port_call!(
                self,
                accepted_stage(&incoming.incoming_rows, &rates_dual, &post, &offsets)
            )?
            .stress_trace_pa;
            for (f, g) in flux.iter_mut().zip(native_direction.flux) {
                *f += g;
            }
            for (s, g) in samples.iter_mut().zip(native_direction.samples) {
                *s += g;
            }
            bulk = native.state;
            dbulk = native_direction.state;
        }
        let state = self.checkpoint.pack(
            &dbulk,
            &gas.iter().map(|v| v.eps[0]).collect::<Vec<_>>(),
            &stress.iter().map(|v| v.eps[0]).collect::<Vec<_>>(),
        )?;
        Ok(SubcycleTangent {
            state,
            flux,
            samples,
        })
    }
}
