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

use crate::gas_continuous_mixed::{ContinuousMixedNetwork, ContinuousMixedOutputBars};
use crate::gas_transient_kinetic::{TransientGasBoundary, TransientGasPacket};
use crate::gas_vjp::ReverseScalar;
use crate::native_continuous_coupled_field::{
    NativeContinuousCoupledField, NativeContinuousGeometryContext, NativeContinuousTick,
};
use crate::native_continuous_port_stage_vjp::NativeContinuousAcceptedStageCotangent;
use crate::native_gas_geometry_adapter::NativeGasShapeScope;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    field::{gas_tick::CoupledTickInput, LatticeKind},
    lattice::{D2Q9, D3Q19, D3Q27},
};
use implexity_solve::{
    multirate_coupling::{SubcycleCotangent, SubcycledField},
    time_stepper::StepParameters,
};
use std::collections::BTreeMap;

fn fail(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}
fn add(target: &mut [f64], source: &[f64]) -> CaeResult<()> {
    if target.len() != source.len() {
        return Err(fail("native continuous adjoint vector shape"));
    }
    for (a, b) in target.iter_mut().zip(source) {
        *a += b;
    }
    Ok(())
}
fn add_rows(target: &mut [(usize, Vec<f64>)], source: &[(usize, Vec<f64>)]) -> CaeResult<()> {
    for (cell, row) in source {
        let i = target
            .binary_search_by_key(cell, |v| v.0)
            .map_err(|_| fail("native continuous adjoint row owner absent"))?;
        add(&mut target[i].1, row)?;
    }
    Ok(())
}
fn scatter_rows(target: &mut [f64], cells: usize, source: &[(usize, Vec<f64>)]) -> CaeResult<()> {
    for (cell, row) in source {
        if *cell >= cells || row.len().checked_mul(cells) != Some(target.len()) {
            return Err(fail("native continuous adjoint population layout"));
        }
        for (q, value) in row.iter().enumerate() {
            target[q * cells + cell] += value;
        }
    }
    Ok(())
}
macro_rules! connector_call {
    ($owner:expr,$name:ident($($arg:expr),*)) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9=>$owner.config.connector.$name::<9,D2Q9>($($arg),*),
            LatticeKind::D3Q19=>$owner.config.connector.$name::<19,D3Q19>($($arg),*),
            LatticeKind::D3Q27=>$owner.config.connector.$name::<27,D3Q27>($($arg),*),
        }
    }
}
macro_rules! donor_call {
    ($owner:expr,$($arg:expr),*) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9=>$owner.ports.donor_coefficients_vjp::<9,D2Q9>($($arg),*),
            LatticeKind::D3Q19=>$owner.ports.donor_coefficients_vjp::<19,D3Q19>($($arg),*),
            LatticeKind::D3Q27=>$owner.ports.donor_coefficients_vjp::<27,D3Q27>($($arg),*),
        }
    }
}
macro_rules! stage_call {
    ($owner:expr,$($arg:expr),*) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9=>crate::native_continuous_port_stage_vjp::accepted_port_stage_vjp::<9,D2Q9>($($arg),*),
            LatticeKind::D3Q19=>crate::native_continuous_port_stage_vjp::accepted_port_stage_vjp::<19,D3Q19>($($arg),*),
            LatticeKind::D3Q27=>crate::native_continuous_port_stage_vjp::accepted_port_stage_vjp::<27,D3Q27>($($arg),*),
        }
    }
}
macro_rules! incoming_call {
    ($owner:expr,$($arg:expr),*) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9=>crate::native_gas_moment_vjp::incoming_moments_vjp::<9,D2Q9>($($arg),*),
            LatticeKind::D3Q19=>crate::native_gas_moment_vjp::incoming_moments_vjp::<19,D3Q19>($($arg),*),
            LatticeKind::D3Q27=>crate::native_gas_moment_vjp::incoming_moments_vjp::<27,D3Q27>($($arg),*),
        }
    }
}
#[derive(Clone)]
struct ReplayCheckpoint {
    tick: usize,
    bulk: Vec<f64>,
    gas: Vec<f64>,
    stress: Vec<f64>,
}
impl NativeContinuousCoupledField {
    fn replay_advance(
        &self,
        state: &mut ReplayCheckpoint,
        n: usize,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        context: Option<&NativeContinuousGeometryContext>,
    ) -> CaeResult<()> {
        let input = CoupledTickInput {
            macro_index: n,
            tick: state.tick + 1,
            previous: &state.bulk,
            start,
            end,
            design,
            time_scale: 1.,
        };
        let step =
            self.prepare_tick_with_closed_context(input, &state.gas, &state.stress, context)?;
        let native = self
            .config
            .bulk
            .coupled_tick_with_incoming(input, &step.incoming.incoming_rows)?;
        let stage = self.accepted_port_stage(&step, &native.state)?;
        state.tick += 1;
        state.bulk = native.state;
        state.gas = step.solved.inventory;
        state.stress = stage.stress_trace_pa;
        Ok(())
    }
    fn remapped_constant_inventory<S: crate::gas_geometry_time::GeometryTimeBase>(
        &self,
        path: &crate::native_gas_geometry_adapter::NativeGasGeometryPath<S>,
        old: &[f64],
    ) -> CaeResult<Vec<S>> {
        let c = &self.config;
        let mut inventory = vec![None; c.layout.slots()];
        for slot in 0..c.layout.slots() {
            if self.kinetic_mask[slot] && path.old_geometry.aggregated.active_slots[slot] {
                inventory[slot] = Some(
                    c.layout
                        .kinetic_inventory(old, slot)?
                        .iter()
                        .map(|v| S::from_f64(*v))
                        .collect(),
                );
            }
        }
        let remapped = c.geometry.layout.remap_sparse_coefficients(
            &path.old_geometry.refined,
            &path.current_geometry.refined,
            &inventory,
            c.layout.basis().coefficient_count(),
            &self.continuum_slots,
        )?;
        let mut result = old.iter().map(|v| S::from_f64(*v)).collect::<Vec<_>>();
        for slot in 0..c.layout.slots() {
            if self.kinetic_mask[slot] {
                let range = c.layout.range(slot)?;
                result[range.clone()].fill(S::zero());
                if let Some(row) = &remapped.coefficient_inventory[slot] {
                    result[range].copy_from_slice(row);
                }
            }
        }
        Ok(result)
    }
    fn remap_inventory_bars(
        &self,
        old: &[f64],
        step: &NativeContinuousTick,
        bars: &[f64],
    ) -> CaeResult<Vec<f64>> {
        let c = &self.config;
        let mut inventory = vec![None; c.layout.slots()];
        let mut output = vec![None; c.layout.slots()];
        for slot in 0..c.layout.slots() {
            if self.kinetic_mask[slot] {
                if step.geometry.old_geometry.aggregated.active_slots[slot] {
                    inventory[slot] = Some(c.layout.kinetic_inventory(old, slot)?.to_vec());
                }
                if step.geometry.current_geometry.aggregated.active_slots[slot] {
                    output[slot] = Some(c.layout.kinetic_inventory(bars, slot)?.to_vec());
                }
            }
        }
        let remapped = c.geometry.layout.remap_sparse_coefficients_transpose(
            &step.geometry.old_geometry.refined,
            &step.geometry.current_geometry.refined,
            &inventory,
            c.layout.basis().coefficient_count(),
            &self.continuum_slots,
            &output,
        )?;
        let mut result = bars.to_vec();
        for slot in 0..c.layout.slots() {
            if self.kinetic_mask[slot] {
                let range = c.layout.range(slot)?;
                result[range.clone()].fill(0.);
                if let Some(row) = &remapped.coefficient_inventory_bars[slot] {
                    result[range].copy_from_slice(row);
                }
            }
        }
        Ok(result)
    }
    pub fn adjoint(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        self.adjoint_with_closed_context(
            n, previous, start, end, p, state_bar, flux_bar, sample_bar, None,
        )
    }
    pub fn adjoint_with_closed_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        context: Option<&NativeContinuousGeometryContext>,
    ) -> CaeResult<SubcycleCotangent> {
        let owned = self.config.geometry.owner.capacity_owner().grid.cells.len();
        let columns = (0..start.len())
            .map(|i| (0, i))
            .chain((0..end.len()).map(|i| (1, i)))
            .chain((0..p.design.len()).map(|i| (2, i)))
            .collect::<Vec<_>>();
        let scopes = columns
            .chunks(48)
            .map(|chunk| NativeGasShapeScope {
                start_columns: chunk
                    .iter()
                    .filter_map(|(kind, i)| (*kind == 0).then_some(*i))
                    .collect(),
                end_columns: chunk
                    .iter()
                    .filter_map(|(kind, i)| (*kind == 1).then_some(*i))
                    .collect(),
                design_columns: chunk
                    .iter()
                    .filter_map(|(kind, i)| (*kind == 2).then_some(*i))
                    .collect(),
                maximum_owned_cells: owned,
            })
            .collect::<Vec<_>>();
        self.adjoint_scoped_geometry_with_closed_context(
            n, previous, start, end, p, state_bar, flux_bar, sample_bar, &scopes, context,
        )
    }
    pub fn adjoint_scoped_geometry(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        geometry_scopes: &[NativeGasShapeScope],
    ) -> CaeResult<SubcycleCotangent> {
        self.adjoint_scoped_geometry_with_closed_context(
            n,
            previous,
            start,
            end,
            p,
            state_bar,
            flux_bar,
            sample_bar,
            geometry_scopes,
            None,
        )
    }
    pub fn adjoint_scoped_geometry_with_closed_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        geometry_scopes: &[NativeGasShapeScope],
        context: Option<&NativeContinuousGeometryContext>,
    ) -> CaeResult<SubcycleCotangent> {
        let c = &self.config;
        if p.time_scale != 1.
            || previous.len() != self.state_size()
            || state_bar.len() != previous.len()
            || start.len() != c.bulk.trace_size()
            || end.len() != start.len()
            || flux_bar.len() != start.len()
            || p.design.len() != c.bulk.design_size()
            || sample_bar.len() != c.bulk.sample_names().len()
            || previous
                .iter()
                .chain(start)
                .chain(end)
                .chain(p.design)
                .chain(state_bar)
                .chain(flux_bar)
                .chain(sample_bar)
                .any(|v| !v.is_finite())
        {
            return Err(fail(
                "native continuous adjoint requires fixed clock and finite matching inputs",
            ));
        }
        match context {
            Some(NativeContinuousGeometryContext::Derivative(certificate)) => {
                certificate.validate_macro(n, p)?;
                certificate.validate_path(&c.geometry.kinematics, start, end, p.design)?;
            }
            Some(NativeContinuousGeometryContext::Trial(certificate)) => {
                certificate.validate_macro(n, p)?;
                certificate.validate_path(&c.geometry.kinematics, start, end, p.design)?;
            }
            Some(NativeContinuousGeometryContext::Primal(_)) => {
                return Err(fail("primal witness cannot select a native adjoint"))
            }
            None => {}
        }
        let mut geometry_columns = std::collections::BTreeSet::new();
        for scope in geometry_scopes {
            let count =
                scope.start_columns.len() + scope.end_columns.len() + scope.design_columns.len();
            if count == 0 || count > 48 {
                return Err(fail("native continuous adjoint geometry scope size"));
            }
            for (kind, columns, bound) in [
                (0, &scope.start_columns, start.len()),
                (1, &scope.end_columns, end.len()),
                (2, &scope.design_columns, p.design.len()),
            ] {
                for &column in columns {
                    if column >= bound || !geometry_columns.insert((kind, column)) {
                        return Err(fail(
                            "native continuous adjoint geometry columns must be valid and disjoint",
                        ));
                    }
                }
            }
        }
        let old = self.checkpoint.split(previous)?;
        let bars = self.checkpoint.split(state_bar)?;
        let mut out = SubcycleCotangent {
            previous: vec![0.; previous.len()],
            trace_start: vec![0.; start.len()],
            trace_end: vec![0.; end.len()],
            design: vec![0.; p.design.len()],
            time_scale: 0.,
        };
        let mut current = ReplayCheckpoint {
            tick: 0,
            bulk: old.bulk.to_vec(),
            gas: old.gas.to_vec(),
            stress: old.stress_trace_pa.to_vec(),
        };
        let ticks = c.bulk.config().substeps;
        let interval = ticks.div_ceil(4);
        let mut retained = vec![current.clone()];
        for tick in 1..=ticks {
            self.replay_advance(&mut current, n, start, end, p.design, context)?;
            if tick < ticks && tick % interval == 0 {
                retained.push(current.clone());
            }
        }
        if retained.len() > 4 {
            return Err(fail("native continuous replay checkpoint bound"));
        }
        drop(current);
        let mut bulk_bar = bars.bulk.to_vec();
        let mut gas_bar = bars.gas.to_vec();
        let mut stress_bar = bars.stress_trace_pa.to_vec();
        let dt = c.bulk.config().macro_step_s / ticks as f64;
        let gas_flux_bar = flux_bar
            .iter()
            .map(|v| v / ticks as f64)
            .collect::<Vec<_>>();
        for tick in (1..=ticks).rev() {
            let checkpoint = retained
                .iter()
                .rev()
                .find(|v| v.tick < tick)
                .ok_or_else(|| fail("native continuous replay checkpoint absent"))?;
            let mut before = checkpoint.clone();
            while before.tick + 1 < tick {
                self.replay_advance(&mut before, n, start, end, p.design, context)?;
            }
            let input = CoupledTickInput {
                macro_index: n,
                tick,
                previous: &before.bulk,
                start,
                end,
                design: p.design,
                time_scale: 1.,
            };
            let step =
                self.prepare_tick_with_closed_context(input, &before.gas, &before.stress, context)?;
            let native = c
                .bulk
                .coupled_tick_with_incoming(input, &step.incoming.incoming_rows)?;
            let post_rows = self
                .port_cells
                .iter()
                .map(|cell| {
                    (
                        *cell,
                        (0..(native.state.len() / c.bulk.config().solid_mask.len()))
                            .map(|q| native.state[q * c.bulk.config().solid_mask.len() + cell])
                            .collect(),
                    )
                })
                .collect::<Vec<_>>();
            let offsets = vec![[0.; 3]; c.connector.exchange().port_count()];
            let stage_bars = NativeContinuousAcceptedStageCotangent {
                stress_trace_pa: stress_bar,
                mass_momentum_change: vec![[0.; 4]; offsets.len()],
                energy_to_thermostat_j: vec![0.; offsets.len()],
            };
            let stage = stage_call!(
                self,
                &c.connector,
                &step.incoming.incoming_rows,
                &step.accepted_rates,
                &post_rows,
                &offsets,
                &stage_bars
            )?;
            scatter_rows(
                &mut bulk_bar,
                c.bulk.config().solid_mask.len(),
                &stage.accepted_post_collision_rows,
            )?;
            let mut native_bar = c.bulk.coupled_tick_with_incoming_adjoint(
                input,
                &step.incoming.incoming_rows,
                &bulk_bar,
                flux_bar,
                sample_bar,
            )?;
            add_rows(&mut native_bar.incoming, &stage.accepted_pre_collision_rows)?;
            let rate = c.bulk.coupled_tick_pre_collision_rates_adjoint(
                input,
                &step.incoming.incoming_rows,
                &stage.accepted_relaxation_rates,
            )?;
            add(&mut native_bar.native.previous, &rate.previous)?;
            add_rows(&mut native_bar.incoming, &rate.incoming)?;
            add(&mut out.trace_start, &native_bar.native.trace_start)?;
            add(&mut out.trace_end, &native_bar.native.trace_end)?;
            add(&mut out.design, &native_bar.native.design)?;
            bulk_bar = native_bar.native.previous;
            let reflection = c.bulk.coupled_tick_reflection(input, &self.links)?;
            let reflected = reflection
                .iter()
                .map(|(cell, j)| {
                    let mut v = [0.; 10];
                    v[1..4].copy_from_slice(j);
                    (*cell, v)
                })
                .collect::<Vec<_>>();
            let pulled = c.bulk.coupled_tick_incoming(input, &self.port_cells)?;
            let returned = incoming_call!(
                self,
                &c.connector,
                &pulled,
                &step.projected_packets,
                &reflected,
                &offsets,
                &native_bar.incoming,
                [0.; 10],
                0.
            )?;
            add(
                &mut bulk_bar,
                &c.bulk
                    .coupled_tick_incoming_adjoint(input, &returned.incoming_rows)?,
            )?;
            let reflected_bar = returned
                .reflection_moments
                .iter()
                .map(|(cell, j)| (*cell, [j[1], j[2], j[3]]))
                .collect::<Vec<_>>();
            add(
                &mut bulk_bar,
                &c.bulk
                    .coupled_tick_reflection_adjoint(input, &self.links, &reflected_bar)?
                    .previous,
            )?;
            let (reference_face_bar, mut reference_bar) = connector_call!(
                self,
                reference_moment_projection_vjp(&self.links, &returned.packet_moments)
            )?;
            let reference = match c.bulk.lattice() {
                LatticeKind::D2Q9 => c
                    .connector
                    .interface_reference_pressure::<f64, 9, D2Q9>(&before.bulk)?,
                LatticeKind::D3Q19 => c
                    .connector
                    .interface_reference_pressure::<f64, 19, D3Q19>(&before.bulk)?,
                LatticeKind::D3Q27 => c
                    .connector
                    .interface_reference_pressure::<f64, 27, D3Q27>(&before.bulk)?,
            };
            for face in &step.geometry.faces {
                if let TransientGasBoundary::Port { port } = face.boundary {
                    reference_bar += (0..3)
                        .map(|j| face.area_m2 * face.normal[j] * dt * reference_face_bar[port][j])
                        .sum::<f64>();
                }
            }
            let pressure_rows =
                connector_call!(self, interface_reference_pressure_vjp(reference_bar))?;
            scatter_rows(
                &mut bulk_bar,
                c.bulk.config().solid_mask.len(),
                &pressure_rows,
            )?;
            let mut output = ContinuousMixedOutputBars {
                residual: vec![0.; c.layout.state_size()],
                boundary_moments: BTreeMap::new(),
                boundary_wall_work_j: BTreeMap::new(),
                boundary_wall_heat_j: BTreeMap::new(),
            };
            for packet in &step.solved.evaluated.boundary_packets {
                if let TransientGasBoundary::Port { port } =
                    step.geometry.faces[packet.face].boundary
                {
                    output
                        .boundary_moments
                        .insert(packet.face, returned.packet_moments[port]);
                }
            }
            let packets = wall_packets(&step);
            let at = interpolate(start, end, (tick - 1) as f64 / ticks as f64);
            let bt = interpolate(start, end, tick as f64 / ticks as f64);
            for stage in &step.geometry.wall_stages {
                let impulses = stage
                    .face_indices
                    .iter()
                    .map(|f| {
                        let packet = packets
                            .iter()
                            .find(|(i, _)| i == f)
                            .ok_or_else(|| fail("native continuous wall packet absent"))?;
                        let face = &step.geometry.faces[*f];
                        Ok(crate::reference_traction::adjust_wall_reference(
                            packet.1.momentum_kg_m_s,
                            face.normal.map(|v| v * face.area_m2 * dt),
                            face.wall_velocity_m_s,
                            c.reference_pressure_pa,
                        )?
                        .momentum_impulse_kg_m_s)
                    })
                    .collect::<CaeResult<Vec<_>>>()?;
                let wall = stage.transfer.scatter_adjoint(
                    &impulses,
                    &at,
                    &bt,
                    dt,
                    &gas_flux_bar,
                    [0.; 3],
                    0.,
                )?;
                for (face, bar) in stage.face_indices.iter().zip(wall.impulses_n_s) {
                    let entry = output.boundary_moments.entry(*face).or_insert([0.; 10]);
                    for j in 0..3 {
                        entry[j + 1] += bar[j];
                    }
                }
            }
            let packed = step.system.pack(&step.solved.inventory)?;
            let direct = step.system.output_vjp(&packed, &output)?;
            add(&mut gas_bar, &direct.current_inventory)?;
            output.residual = crate::gas_continuous_mixed_implicit::accepted_physical_residual_vjp(
                &step.system,
                &step.solved,
                &c.solve_policy,
                &gas_bar,
            )?;
            let mixed = step.system.output_vjp(&packed, &output)?;
            let donor = donor_call!(
                self,
                &before.bulk,
                &before.stress,
                &offsets,
                &mixed.port_coefficient_density
            )?;
            add(&mut bulk_bar, &donor.populations)?;
            stress_bar = donor.stress_trace_pa;
            gas_bar = self.remap_inventory_bars(&before.gas, &step, &mixed.previous_inventory)?;
            let selected = (0..c.layout.slots())
                .filter(|s| step.geometry.current_geometry.aggregated.active_slots[*s])
                .collect::<Vec<_>>();
            for scope in geometry_scopes {
                let evaluate =
                    |path: &crate::native_gas_geometry_adapter::NativeGasGeometryPath<
                        ReverseScalar,
                    >,
                     trace_a: &[ReverseScalar],
                     trace_b: &[ReverseScalar],
                     _design: &[ReverseScalar]| {
                        let remapped = self.remapped_constant_inventory(path, &before.gas)?;
                        path.validate_wall_anchor_route(&step.geometry)?;
                        let network = self.prepare_network_for_path(path, ReverseScalar::from_f64(dt))?;
                        let current = step
                            .solved
                            .inventory
                            .iter()
                            .map(|v| ReverseScalar::from_f64(*v))
                            .collect::<Vec<_>>();
                        let donors = step
                            .system
                            .authored()
                            .port_coefficient_density
                            .iter()
                            .map(|row| row.iter().map(|v| ReverseScalar::from_f64(*v)).collect())
                            .collect::<Vec<_>>();
                        let mut objective = network.weighted_selected_output(
                            &current,
                            &remapped,
                            &donors,
                            ReverseScalar::from_f64(c.gas_rt_m2_s2),
                            ReverseScalar::from_f64(c.viscosity_pa_s),
                            &selected,
                            &output,
                        )?;
                        let wall_packets = packets
                            .iter()
                            .map(|(i, p)| (*i, lift_packet(p)))
                            .collect::<Vec<_>>();
                        let wall = path.scatter_wall_packets_with_reference(
                            &wall_packets,
                            trace_a,
                            trace_b,
                            ReverseScalar::from_f64(c.reference_pressure_pa),
                        )?;
                        for (force, bar) in wall.load.force_flux_n.iter().zip(&gas_flux_bar) {
                            objective += *force * *bar;
                        }
                        for face in &path.faces {
                            if let TransientGasBoundary::Port { port } = face.boundary {
                                for j in 0..3 {
                                    objective += face.area_m2
                                        * face.normal[j]
                                        * dt
                                        * reference
                                        * reference_face_bar[port][j];
                                }
                            }
                        }
                        Ok(objective)
                    };
                let a = (tick - 1) as f64 / ticks as f64;
                let b = tick as f64 / ticks as f64;
                match context {
                    None => {
                        let geometry = c.geometry.checked_path_pullback(
                            &at,
                            &bt,
                            p.design,
                            dt,
                            &c.boundaries,
                            scope,
                            false,
                            evaluate,
                        )?;
                        for j in 0..start.len() {
                            out.trace_start[j] += (1. - a) * geometry.shape.start[j]
                                + (1. - b) * geometry.shape.end[j];
                            out.trace_end[j] +=
                                a * geometry.shape.start[j] + b * geometry.shape.end[j];
                        }
                        add(&mut out.design, &geometry.shape.design)?;
                    }
                    Some(NativeContinuousGeometryContext::Derivative(certificate)) => {
                        let law = c.closed_contact_law.as_ref().ok_or_else(|| {
                            fail("native adjoint geometry lacks its contact owner")
                        })?;
                        let sample = std::sync::Arc::new(certificate.sample_child(law, a, b)?);
                        let geometry = c.geometry.checked_path_pullback_with_original_macro_receipt(
                            &c.geometry,
                            start,
                            end,
                            p.design,
                            p.time_scale,
                            dt,
                            &c.boundaries,
                            scope,
                            false,
                            sample,
                            |receipt| {
                                self.derivative_birth_stages(n, tick, receipt)?;
                                Ok(())
                            },
                            |receipt, path, trace_a, trace_b, design| {
                                let stages = self.derivative_birth_stages(n, tick, receipt)?;
                                self.consume_birth_stages(path, &stages)?;
                                let previous = before.gas.iter().map(|v| ReverseScalar::from_f64(*v)).collect::<Vec<_>>();
                                self.admit_birth_inventory(path, &previous)?;
                                evaluate(path, trace_a, trace_b, design)
                            },
                        )?;
                        add(&mut out.trace_start, &geometry.shape.start)?;
                        add(&mut out.trace_end, &geometry.shape.end)?;
                        add(&mut out.design, &geometry.shape.design)?;
                    }
                    Some(NativeContinuousGeometryContext::Trial(certificate)) => {
                        let law = c
                            .closed_contact_law
                            .as_ref()
                            .ok_or_else(|| fail("native trial adjoint lacks its contact owner"))?;
                        let sample = std::sync::Arc::new(certificate.sample_child(law, a, b)?);
                        let geometry = c.geometry.checked_trial_path_pullback_with_original_macro_receipt(
                            &c.geometry,
                            start,
                            end,
                            p.design,
                            p.time_scale,
                            dt,
                            &c.boundaries,
                            scope,
                            false,
                            sample,
                            |receipt| {
                                self.trial_birth_stages(n, tick, receipt)?;
                                Ok(())
                            },
                            |receipt, path, trace_a, trace_b, design| {
                                let stages = self.trial_birth_stages(n, tick, receipt)?;
                                self.consume_birth_stages(path, &stages)?;
                                let previous = before.gas.iter().map(|v| ReverseScalar::from_f64(*v)).collect::<Vec<_>>();
                                self.admit_birth_inventory(path, &previous)?;
                                evaluate(path, trace_a, trace_b, design)
                            },
                        )?;
                        add(&mut out.trace_start, &geometry.shape.start)?;
                        add(&mut out.trace_end, &geometry.shape.end)?;
                        add(&mut out.design, &geometry.shape.design)?;
                    }
                    Some(NativeContinuousGeometryContext::Primal(_)) => {
                        return Err(fail("primal witness cannot select a native adjoint"))
                    }
                }
            }
        }
        out.previous = self.checkpoint.pack(&bulk_bar, &gas_bar, &stress_bar)?;
        if out
            .previous
            .iter()
            .chain(&out.trace_start)
            .chain(&out.trace_end)
            .chain(&out.design)
            .any(|v| !v.is_finite())
        {
            return Err(fail("native continuous adjoint overflow"));
        }
        Ok(out)
    }
}
fn interpolate(start: &[f64], end: &[f64], weight: f64) -> Vec<f64> {
    start
        .iter()
        .zip(end)
        .map(|(s, e)| s + (e - s) * weight)
        .collect()
}
fn wall_packets(step: &NativeContinuousTick) -> Vec<(usize, TransientGasPacket<f64>)> {
    step.solved
        .evaluated
        .boundary_packets
        .iter()
        .map(|p| {
            (
                p.face,
                TransientGasPacket {
                    mass_kg: p.moments[0],
                    momentum_kg_m_s: [p.moments[1], p.moments[2], p.moments[3]],
                    kinetic_energy_j: 0.5 * (p.moments[4] + p.moments[5] + p.moments[6]),
                    wall_work_j: p.wall_work_j,
                    wall_heat_j: p.wall_heat_j,
                },
            )
        })
        .collect()
}
fn lift_packet(p: &TransientGasPacket<f64>) -> TransientGasPacket<ReverseScalar> {
    TransientGasPacket {
        mass_kg: ReverseScalar::from_f64(p.mass_kg),
        momentum_kg_m_s: p.momentum_kg_m_s.map(ReverseScalar::from_f64),
        kinetic_energy_j: ReverseScalar::from_f64(p.kinetic_energy_j),
        wall_work_j: ReverseScalar::from_f64(p.wall_work_j),
        wall_heat_j: ReverseScalar::from_f64(p.wall_heat_j),
    }
}
