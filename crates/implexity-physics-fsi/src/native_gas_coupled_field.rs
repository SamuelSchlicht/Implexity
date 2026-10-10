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

use crate::gas_multiscale_network::MultiscaleNetworkPolicy;
use crate::gas_transient_kinetic::TransientVelocityGrid;
use crate::native_gas_geometry_adapter::{NativeGasExteriorBoundary, NativeGasGeometryAdapter};
use crate::native_gas_port_coupling::NativeGasPortConnector;
use implexity_physics_lbm::moving::field::AnyMovingLbm;
use implexity_solve::nonlinear_graph::NonlinearGraphPolicy;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct NativeGasMixedConfig {
    pub continuum_slots: Vec<usize>,
    pub kinetic_subdomain: Option<crate::gas_mixed_solver::MixedKineticSubdomainPolicy>,
    pub continuum_policy: crate::gas_continuum_fv::ContinuumFvPolicy,
    pub lift_policy: crate::gas_continuum_lift::ContinuumGasPolicy,
}
pub struct NativeGasCoupledFieldConfig {
    pub bulk: Arc<AnyMovingLbm>,
    pub geometry: Arc<NativeGasGeometryAdapter>,
    pub connector: Arc<NativeGasPortConnector>,
    pub boundaries: BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
    pub velocity_grid: TransientVelocityGrid,
    pub transport_policy: MultiscaleNetworkPolicy,
    pub nonlinear_policy: NonlinearGraphPolicy,
    pub mixed: Option<NativeGasMixedConfig>,
    pub initial_trace: Vec<f64>,
    pub initial_density_kg_m3: f64,
    pub initial_velocity_m_s: [f64; 3],
    pub gas_rt_m2_s2: f64,
    pub viscosity_pa_s: f64,
    pub reference_pressure_pa: f64,
}

pub struct NativeGasCoupledField {
    config: NativeGasCoupledFieldConfig,
    names: Vec<String>,
    bulk_state_size: usize,
    gas_state_size: usize,
    mixed_layout: Option<crate::gas_mixed_network::MixedGasLayout>,
}

use crate::gas_multiscale_network::physical_residual_scalar;
use crate::gas_multiscale_solver::MultiscaleNetworkSystem;
use crate::gas_transient_kinetic::{TransientGasBoundary, TransientGasPacket};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    field::{LatticeKind, gas_tick::CoupledTickInput},
    lattice::{D2Q9, D3Q19, D3Q27},
};
use implexity_solve::{
    multirate_coupling::{SubcycleCotangent, SubcycleRecord, SubcycleTangent, SubcycledField},
    nonlinear_graph::{NonlinearGraphSystem, solve},
    time_stepper::{StepDiagnostics, StepParameters},
};

impl NativeGasCoupledField {
    pub fn new(config: NativeGasCoupledFieldConfig) -> CaeResult<Self> {
        if config.initial_trace.len() != config.bulk.trace_size()
            || config
                .initial_trace
                .iter()
                .chain(&config.initial_velocity_m_s)
                .any(|v| !v.is_finite())
            || [
                config.initial_density_kg_m3,
                config.gas_rt_m2_s2,
                config.viscosity_pa_s,
                config.reference_pressure_pa,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.)
            || config.geometry.layout.slots() == 0
            || config.geometry.layout.state_size() / config.geometry.layout.slots().max(1)
                != config.velocity_grid.velocity_m_s.len()
            || config.velocity_grid.velocity_m_s.is_empty()
            || config.connector.exchange().port_count() == 0
        {
            return Err(CaeError::contract(
                "invalid native gas coupled field configuration",
            ));
        }
        let bulk = config.bulk.config();
        let grid = &config.geometry.owner.capacity_owner().grid;
        if grid.origin_m != bulk.origin_m
            || grid.spacing_m != bulk.spacing_m
            || grid.shape != bulk.shape
            || grid.cells != config.connector.exchange().excluded_cells()
        {
            return Err(CaeError::contract(
                "native gas geometry and bulk ownership grids differ",
            ));
        }
        let pressure = bulk.density_kg_m3 * config.gas_rt_m2_s2;
        if !pressure.is_finite()
            || (pressure - config.reference_pressure_pa).abs()
                > 128. * f64::EPSILON * pressure.abs().max(config.reference_pressure_pa)
        {
            return Err(CaeError::contract(
                "native gas temperature and reference pressure differ",
            ));
        }
        config
            .connector
            .exchange()
            .validate_native_mask(&bulk.solid_mask)?;
        config.connector.exchange().validate_native_scales(
            bulk.spacing_m,
            bulk.density_kg_m3,
            bulk.macro_step_s / bulk.substeps as f64,
        )?;
        if config
            .connector
            .exchange()
            .ports()
            .iter()
            .any(|p| p.len() != 1 || p[0].1 != 1.)
        {
            return Err(CaeError::contract(
                "native gas coupling requires one exchange port per bulk cell",
            ));
        }
        if config.boundaries.values().any(|b|matches!(b,NativeGasExteriorBoundary::Port{port} if *port>=config.connector.exchange().port_count())) {
            return Err(CaeError::contract("native gas boundary port is outside connector"));
        }
        let mixed_layout = if let Some(mixed) = &config.mixed {
            let slots = config.geometry.layout.slots();
            let mut mask = vec![false; slots];
            for &i in &mixed.continuum_slots {
                if i >= slots || mask[i] {
                    return Err(CaeError::contract(
                        "invalid or duplicate native continuum slot",
                    ));
                }
                mask[i] = true;
            }
            if mixed.continuum_slots.is_empty() {
                return Err(CaeError::contract(
                    "mixed native gas requires explicit certified continuum slots",
                ));
            }
            Some(crate::gas_mixed_network::MixedGasLayout::new(
                mask,
                &vec![true; slots],
                config.velocity_grid.velocity_m_s.len(),
            )?)
        } else {
            None
        };
        let gas_state_size = mixed_layout
            .as_ref()
            .map_or(config.geometry.layout.state_size(), |l| l.dimension());
        if config
            .bulk
            .state_size()
            .checked_add(gas_state_size)
            .is_none()
        {
            return Err(CaeError::contract("native gas state size overflow"));
        }
        let bulk_state_size = config.bulk.state_size();
        let names = config.bulk.sample_names().to_vec();
        Ok(Self {
            config,
            names,
            bulk_state_size,
            gas_state_size,
            mixed_layout,
        })
    }
    fn admit(
        &self,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        if previous.len() != self.state_size()
            || start.len() != self.trace_size()
            || end.len() != self.trace_size()
            || p.design.len() != self.design_size()
            || previous
                .iter()
                .chain(start)
                .chain(end)
                .chain(p.design)
                .any(|v| !v.is_finite())
            || p.time_scale != 1.
        {
            return Err(CaeError::contract(
                "native gas coupling requires matched fixed physical time and finite input dimensions",
            ));
        }
        Ok(())
    }
    fn port_distributions<S: crate::gas_transient_kinetic::TransientGasScalar>(
        &self,
        state: &[S],
        offsets: &[[S; 3]],
    ) -> CaeResult<Vec<Vec<S>>> {
        let c = &self.config;
        match c.bulk.lattice() {
            LatticeKind::D2Q9 => c.connector.port_distributions::<S, 9, D2Q9>(
                state,
                offsets,
                &c.velocity_grid,
                S::from_f64(c.gas_rt_m2_s2),
                &c.transport_policy.kinetic,
            ),
            LatticeKind::D3Q19 => c.connector.port_distributions::<S, 19, D3Q19>(
                state,
                offsets,
                &c.velocity_grid,
                S::from_f64(c.gas_rt_m2_s2),
                &c.transport_policy.kinetic,
            ),
            LatticeKind::D3Q27 => c.connector.port_distributions::<S, 27, D3Q27>(
                state,
                offsets,
                &c.velocity_grid,
                S::from_f64(c.gas_rt_m2_s2),
                &c.transport_policy.kinetic,
            ),
        }
    }
    fn kinetic_projection<S: crate::gas_transient_kinetic::TransientGasScalar>(
        &self,
        gas: &[S],
    ) -> CaeResult<Vec<S>> {
        if let Some(layout) = &self.mixed_layout {
            layout.kinetic_projection(gas)
        } else {
            Ok(gas.to_vec())
        }
    }
    fn totals(&self, gas: &[f64]) -> CaeResult<[f64; 5]> {
        if let Some(layout) = &self.mixed_layout {
            mixed_gas_totals(layout, gas, &self.config.velocity_grid, self.config.gas_rt_m2_s2)
        } else {
            Ok(gas_totals(gas, &self.config.velocity_grid))
        }
    }
    fn gas_scaling(
        &self,
        inputs: &crate::gas_multiscale_network::MultiscaleNetworkInputs<f64>,
    ) -> CaeResult<Vec<f64>> {
        let weights = crate::gas_transient_kinetic::velocity_equilibrium_density(
            &inputs.gas.grid,
            1.,
            [0.; 3],
            inputs.gas.gas_rt_m2_s2,
            &self.config.transport_policy.kinetic,
        )?;
        let volume_scale = self.config.bulk.config().spacing_m.powi(3);
        let scales = inputs
            .gas
            .volume_m3
            .iter()
            .filter(|v| **v > 0.)
            .flat_map(|v| weights.iter().map(move |w| v / volume_scale * w))
            .collect::<Vec<_>>();
        if scales.iter().any(|s| !s.is_finite() || *s <= 0.) {
            return Err(CaeError::contract(
                "native gas numerical scaling is not finite and positive",
            ));
        }
        Ok(scales)
    }

    fn reference_packets<S: crate::gas_transient_kinetic::TransientGasScalar>(
        &self,
        inputs: &crate::gas_multiscale_network::MultiscaleNetworkInputs<S>,
        bulk: &[S],
        packets: &[TransientGasPacket<S>],
        links: &[implexity_physics_lbm::moving::field::gas_tick::CoupledInterfaceLink],
    ) -> CaeResult<crate::native_gas_port_coupling::NativeGasReferencePackets<S>> {
        let connector = &self.config.connector;
        let pressure = match self.config.bulk.lattice() {
            LatticeKind::D2Q9 => connector.interface_reference_pressure::<S, 9, D2Q9>(bulk)?,
            LatticeKind::D3Q19 => connector.interface_reference_pressure::<S, 19, D3Q19>(bulk)?,
            LatticeKind::D3Q27 => connector.interface_reference_pressure::<S, 27, D3Q27>(bulk)?,
        };
        let mut reference = vec![[S::zero(); 3]; packets.len()];
        for face in &inputs.gas.faces {
            if let TransientGasBoundary::Port { port } = face.boundary {
                for k in 0..3 {
                    reference[port][k] +=
                        pressure * face.area_m2 * face.normal[k] * inputs.gas.step_s;
                }
            }
        }
        match self.config.bulk.lattice() {
            LatticeKind::D2Q9 => {
                connector.reference_projection::<S, 9, D2Q9>(packets, &reference, links, pressure)
            }
            LatticeKind::D3Q19 => {
                connector.reference_projection::<S, 19, D3Q19>(packets, &reference, links, pressure)
            }
            LatticeKind::D3Q27 => {
                connector.reference_projection::<S, 27, D3Q27>(packets, &reference, links, pressure)
            }
        }
    }

    fn incoming<S: crate::gas_transient_kinetic::TransientGasScalar>(
        &self,
        rows: &[(usize, Vec<S>)],
        packets: &[TransientGasPacket<S>],
        reflections: &[(usize, [S; 3])],
        offsets: &[[S; 3]],
    ) -> CaeResult<crate::native_gas_port_coupling::NativeGasPortIncoming<S>> {
        match self.config.bulk.lattice() {
            LatticeKind::D2Q9 => self.config.connector.replace_incoming::<S, 9, D2Q9>(
                rows,
                packets,
                reflections,
                offsets,
            ),
            LatticeKind::D3Q19 => self.config.connector.replace_incoming::<S, 19, D3Q19>(
                rows,
                packets,
                reflections,
                offsets,
            ),
            LatticeKind::D3Q27 => self.config.connector.replace_incoming::<S, 27, D3Q27>(
                rows,
                packets,
                reflections,
                offsets,
            ),
        }
    }
}
impl SubcycledField for NativeGasCoupledField {
    fn state_size(&self) -> usize {
        self.bulk_state_size + self.gas_state_size
    }
    fn trace_size(&self) -> usize {
        self.config.bulk.trace_size()
    }
    fn design_size(&self) -> usize {
        self.config.bulk.design_size()
    }
    fn sample_names(&self) -> &[String] {
        &self.names
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        let c = &self.config;
        let mut state = c.bulk.initial_state(design)?;
        let gas = c.geometry.initial_inventory(
            &c.initial_trace,
            design,
            c.initial_density_kg_m3,
            c.initial_velocity_m_s,
            c.gas_rt_m2_s2,
            &c.velocity_grid,
            &c.transport_policy.kinetic,
        )?;
        state.extend(if let Some(layout) = &self.mixed_layout {
            layout.pack(&gas, &c.velocity_grid.velocity_m_s)?
        } else {
            gas
        });
        if state.len() != self.state_size() {
            return Err(CaeError::contract(
                "native gas initial state layout mismatch",
            ));
        }
        Ok(state)
    }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        use implexity_ad::Scalar;
        if design.len() != self.design_size()
            || cotangent.len() != self.state_size()
            || design.iter().chain(cotangent).any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("native gas initial-state cotangent shape or value"));
        }
        let c = &self.config;
        let mut bar = c.bulk.initial_state_vjp(design, &cotangent[..self.bulk_state_size])?;
        let density = crate::gas_transient_kinetic::velocity_equilibrium_density(
            &c.velocity_grid, c.initial_density_kg_m3, c.initial_velocity_m_s,
            c.gas_rt_m2_s2, &c.transport_policy.kinetic,
        )?;
        let gas_bar = &cotangent[self.bulk_state_size..];
        let mut volume_bar = vec![0.; c.geometry.layout.slots()];
        for (cell, volume) in volume_bar.iter_mut().enumerate() {
            if let Some(layout) = &self.mixed_layout {
                let Some(offset) = layout.cell_offset(cell) else { continue; };
                for (k, &q) in density.iter().enumerate() {
                    let coefficient = if layout.continuum_cells()[cell] {
                        gas_bar[offset] + (0..3).map(|a| gas_bar[offset + a + 1] * c.velocity_grid.velocity_m_s[k][a]).sum::<f64>()
                    } else { gas_bar[offset + k] };
                    *volume += q * coefficient;
                }
            } else {
                *volume = density.iter().zip(&gas_bar[cell * density.len()..(cell + 1) * density.len()])
                    .map(|(q, b)| q * b).sum();
            }
        }
        let geometry_bar = crate::gas_vjp::scoped_pullback(design, |parameters| {
            let trace: Vec<_> = c.initial_trace.iter().copied().map(crate::gas_vjp::ReverseScalar::from_f64).collect();
            let endpoint = c.geometry.endpoint(&trace, parameters)?;
            let mut objective = crate::gas_vjp::ReverseScalar::from_f64(0.);
            for (cell, (&volume, &weight)) in endpoint.aggregated.volumes_m3.iter().zip(&volume_bar).enumerate() {
                if self.mixed_layout.as_ref().is_some_and(|layout| layout.cell_offset(cell).is_none())
                    && (volume.value() != 0. || crate::gas_gap_mass::GasMassScalar::has_direction(volume)) {
                    return Err(CaeError::contract("initial gas design crosses an inactive state slot"));
                }
                objective += volume * weight;
            }
            Ok(objective)
        })?;
        if bar.len() != geometry_bar.len() {
            return Err(CaeError::contract("native gas initial-state design gradient shape"));
        }
        for (value, geometry) in bar.iter_mut().zip(geometry_bar) { *value += geometry; }
        if bar.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("native gas initial-state gradient overflow"));
        }
        Ok(bar)
    }
    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        self.admit(previous, start, end, p)?;
        let c = &self.config;
        let cfg = c.bulk.config();
        let ticks = cfg.substeps;
        let dt = cfg.macro_step_s / ticks as f64;
        let ports = c.connector.exchange().port_count();
        let offsets = vec![cfg.body_acceleration_m_s2.map(|a| a * dt * 0.5); ports];
        let cells = c
            .connector
            .exchange()
            .ports()
            .iter()
            .map(|p| p[0].0)
            .collect::<Vec<_>>();
        let mut cells = cells;
        cells.sort_unstable();
        cells.dedup();
        let links = c
            .bulk
            .coupled_interface_links(c.connector.exchange().excluded_cells())?;
        let mut session = c.bulk.coupled_macro_session(n, start, end, p.design, 1.)?;
        let initial_bulk = &previous[..self.bulk_state_size];
        let mut bulk = initial_bulk.to_vec();
        let mut gas = previous[self.bulk_state_size..].to_vec();
        let mut gas_flux = vec![0.; self.trace_size()];
        let mut gas_work = 0.;
        let mut projection_heat = 0.;
        let mut reference_work = 0.;
        let mut bath_heat = 0.;
        let mut maximum_mass_exchange_defect: f64 = 0.;
        let mut maximum_momentum_exchange_defect: f64 = 0.;
        let mut maximum_reference_projection_defect: f64 = 0.;
        let initial_moments = self.totals(&gas)?;
        let mut boundary_moments = [0.; 5];
        let mut external_wall_impulse = [0.; 3];
        let mut wall_heat = 0.;
        let mut diagnostics = StepDiagnostics::default();
        let mut maximum_physical_residual_kg: f64 = 0.;
        let mut maximum_continuum_momentum_residual: f64 = 0.;
        let mut continuum_energy_remainder = 0.;
        for tick in 1..=ticks {
            let a = (tick - 1) as f64 / ticks as f64;
            let b = tick as f64 / ticks as f64;
            let at: Vec<_> = start
                .iter()
                .zip(end)
                .map(|(s, e)| s + (e - s) * a)
                .collect();
            let bt: Vec<_> = start
                .iter()
                .zip(end)
                .map(|(s, e)| s + (e - s) * b)
                .collect();
            let input = CoupledTickInput {
                macro_index: n,
                tick,
                previous: &bulk,
                start,
                end,
                design: p.design,
                time_scale: 1.,
            };
            let rows = c.bulk.coupled_tick_incoming(input, &cells)?;
            let port_density = self.port_distributions(&bulk, &offsets)?;
            let previous_kinetic = self.kinetic_projection(&gas)?;
            let assembly = c.geometry.assemble_with_boundaries(
                &at,
                &bt,
                p.design,
                dt,
                &previous_kinetic,
                &c.boundaries,
                port_density,
                c.velocity_grid.clone(),
                c.gas_rt_m2_s2,
                c.viscosity_pa_s,
                &c.transport_policy.kinetic,
            )?;
            let residual = if let (Some(layout), Some(mixed)) = (&self.mixed_layout, &c.mixed) {
                assembly.certify_stationary_continuum_slots(&mixed.continuum_slots)?;
                let previous_mixed = layout
                    .with_kinetic_projection(&gas, &assembly.inputs.gas.previous_inventory_kg)?;
                let policy = crate::gas_mixed_network::MixedGasPolicy {
                    network: c.transport_policy.clone(),
                    continuum: mixed.continuum_policy.clone(),
                    lift: mixed.lift_policy.clone(),
                };
                let system = crate::gas_mixed_solver::MixedGasSystem::new(
                    assembly.inputs.clone(),
                    layout.clone(),
                    policy,
                    previous_mixed.clone(),
                    cfg.density_kg_m3,
                )?
                .with_kinetic_subdomain(mixed.kinetic_subdomain.clone())?;
                let solved = solve(&system, &system.pack(&previous_mixed)?, &c.nonlinear_policy)?;
                diagnostics.newton_iterations += solved.newton_iterations;
                diagnostics.krylov_iterations += solved.linear_iterations;
                diagnostics.residual_norm = diagnostics.residual_norm.max(solved.residual_norm);
                gas = system.unpack(&solved.state)?;
                let result = system.physical_result(&solved.state)?;
                continuum_energy_remainder +=
                    result.continuum.equilibrium_energy_balance_residual_j;
                let mut mass_residual = vec![];
                for cell in 0..assembly.inputs.gas.volume_m3.len() {
                    let o = layout
                        .cell_offset(cell)
                        .ok_or_else(|| CaeError::contract("missing mixed checkpoint slot"))?;
                    if layout.continuum_cells()[cell] {
                        mass_residual.push(result.inventory_residual[o]);
                        for j in 1..4 {
                            maximum_continuum_momentum_residual =
                                maximum_continuum_momentum_residual
                                    .max(result.inventory_residual[o + j].abs());
                        }
                    } else {
                        mass_residual.extend_from_slice(
                            &result.inventory_residual[o..o + layout.population_count()],
                        );
                    }
                }
                crate::gas_multiscale_network::MultiscaleNetworkResidual {
                    cells: result.kinetic_cells,
                    inventory_residual_kg: mass_residual,
                    face_packets: result.face_packets.into_iter().enumerate().collect(),
                    collision_heat_bath_energy_j: result.kinetic_collision_bath_energy_j,
                    forcing_work_j: vec![],
                    forcing_thermal_bath_energy_j: vec![],
                }
            } else {
                let mass_scale = vec![
                    cfg.density_kg_m3 * cfg.spacing_m.powi(3);
                    assembly.inputs.gas.volume_m3.len()
                ];
                let system = MultiscaleNetworkSystem::new(
                    assembly.inputs.clone(),
                    c.transport_policy.clone(),
                    mass_scale,
                )?;
                if system.dimension() == 0 {
                    gas = vec![0.; self.gas_state_size];
                } else {
                    let scaling = self.gas_scaling(&assembly.inputs)?;
                    let scaled = implexity_solve::diagonal_graph::DiagonalGraph::new(
                        &system,
                        scaling.clone(),
                        scaling,
                    )?;
                    let initial = scaled.from_inner_state(
                        &system.pack(&assembly.inputs.gas.previous_inventory_kg)?,
                    )?;
                    let solved = solve(&scaled, &initial, &c.nonlinear_policy)?;
                    diagnostics.newton_iterations += solved.newton_iterations;
                    diagnostics.krylov_iterations += solved.linear_iterations;
                    diagnostics.residual_norm = diagnostics.residual_norm.max(solved.residual_norm);
                    gas = system.unpack(&scaled.to_inner_state(&solved.state)?)?;
                }
                physical_residual_scalar(&assembly.inputs, &gas, &c.transport_policy)?
            };
            maximum_physical_residual_kg = maximum_physical_residual_kg.max(
                residual
                    .inventory_residual_kg
                    .iter()
                    .map(|r| r.abs())
                    .fold(0., f64::max),
            );
            let mut packets = vec![
                TransientGasPacket {
                    mass_kg: 0.,
                    momentum_kg_m_s: [0.; 3],
                    kinetic_energy_j: 0.,
                    wall_work_j: 0.,
                    wall_heat_j: 0.
                };
                ports
            ];
            for (face, packet) in &residual.face_packets {
                if let TransientGasBoundary::Port { port } =
                    assembly.inputs.gas.faces[*face].boundary
                {
                    let dst = &mut packets[port];
                    dst.mass_kg += packet.mass_kg;
                    dst.kinetic_energy_j += packet.kinetic_energy_j;
                    for k in 0..3 {
                        dst.momentum_kg_m_s[k] += packet.momentum_kg_m_s[k];
                    }
                }
            }
            for (face, packet) in &residual.face_packets {
                if !matches!(
                    assembly.inputs.gas.faces[*face].boundary,
                    TransientGasBoundary::Interior { .. }
                ) {
                    boundary_moments[0] += packet.mass_kg;
                    boundary_moments[4] += packet.kinetic_energy_j;
                    for k in 0..3 {
                        boundary_moments[k + 1] += packet.momentum_kg_m_s[k];
                    }
                    wall_heat += packet.wall_heat_j;
                    if assembly.external_wall_faces.contains(face) {
                        for k in 0..3 {
                            external_wall_impulse[k] += packet.momentum_kg_m_s[k];
                        }
                    }
                }
            }
            let wall = assembly.scatter_wall_packets_with_reference(
                &residual.face_packets,
                &at,
                &bt,
                c.reference_pressure_pa,
            )?;
            for (v, f) in gas_flux.iter_mut().zip(wall.load.force_flux_n) {
                *v += f / ticks as f64;
            }
            gas_work += wall.load.wall_work_j;
            reference_work += wall.reference_work_j;
            bath_heat += residual.collision_heat_bath_energy_j.iter().sum::<f64>();
            let reflected = c.bulk.coupled_tick_reflection(input, &links)?;
            let reference = self.reference_packets(&assembly.inputs, &bulk, &packets, &links)?;
            maximum_reference_projection_defect = maximum_reference_projection_defect.max(
                reference
                    .reference_momentum_defect_kg_m_s
                    .iter()
                    .map(|v| v.abs())
                    .fold(0., f64::max),
            );
            let packets = reference.packets;
            let incoming = self.incoming(&rows, &packets, &reflected, &offsets)?;
            maximum_mass_exchange_defect =
                maximum_mass_exchange_defect.max(incoming.exchange_mass_momentum_defect[0].abs());
            maximum_momentum_exchange_defect = maximum_momentum_exchange_defect.max(
                incoming.exchange_mass_momentum_defect[1..]
                    .iter()
                    .map(|v| v.abs())
                    .fold(0., f64::max),
            );
            projection_heat += incoming.projection_heat_j;
            bulk = session.tick_with_incoming(&bulk, &incoming.incoming_rows)?;
        }
        let mut native = session.finish(initial_bulk, bulk)?;
        for (f, g) in native.flux.iter_mut().zip(gas_flux) {
            *f += g;
        }
        native.pairing += gas_work;
        native
            .ledger
            .insert("gas_mass_kg".into(), self.totals(&gas)?[0]);
        if self.mixed_layout.is_some() {
            native.ledger.insert(
                "gas_continuum_equilibrium_energy_remainder_J".into(),
                continuum_energy_remainder,
            );
            native.ledger.insert(
                "gas_continuum_maximum_momentum_residual_kg_m_s".into(),
                maximum_continuum_momentum_residual,
            );
        }
        native
            .ledger
            .insert("gas_scaled_residual_norm".into(), diagnostics.residual_norm);
        native.ledger.insert(
            "gas_physical_residual_maximum_kg".into(),
            maximum_physical_residual_kg,
        );
        native
            .ledger
            .insert("gas_interface_work_J".into(), gas_work);
        native
            .ledger
            .insert("gas_reference_work_J".into(), reference_work);
        native
            .ledger
            .insert("gas_collision_bath_energy_J".into(), bath_heat);
        native
            .ledger
            .insert("gas_bulk_projection_heat_J".into(), projection_heat);
        native.ledger.insert(
            "gas_bulk_mass_exchange_maximum_defect_kg".into(),
            maximum_mass_exchange_defect,
        );
        native.ledger.insert(
            "gas_bulk_momentum_exchange_maximum_defect_kg_m_s".into(),
            maximum_momentum_exchange_defect,
        );
        native.ledger.insert(
            "gas_reference_projection_maximum_momentum_defect_kg_m_s".into(),
            maximum_reference_projection_defect,
        );
        let final_moments = self.totals(&gas)?;
        native.ledger.insert(
            "gas_mass_conservation_defect_kg".into(),
            final_moments[0] - initial_moments[0] + boundary_moments[0],
        );
        native.ledger.insert(
            "gas_energy_conservation_defect_J".into(),
            final_moments[4] - initial_moments[4] + boundary_moments[4] + bath_heat,
        );
        native.ledger.insert("gas_wall_heat_J".into(), wall_heat);
        for (k, axis) in ["x", "y", "z"].iter().enumerate() {
            native.ledger.insert(
                format!("gas_momentum_conservation_defect_{axis}_kg_m_s"),
                final_moments[k + 1] - initial_moments[k + 1] + boundary_moments[k + 1],
            );
            native.ledger.insert(
                format!("gas_external_wall_impulse_{axis}_N_s"),
                external_wall_impulse[k],
            );
        }
        native.state.extend(gas);
        Ok(SubcycleRecord {
            state: native.state,
            flux: native.flux,
            samples: native.samples,
            pairing: native.pairing,
            ledger: native.ledger,
            diagnostics,
        })
    }
    fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        dp: &[f64],
        ds: &[f64],
        de: &[f64],
        dd: Option<&[f64]>,
        dtime: f64,
    ) -> CaeResult<SubcycleTangent> {
        use implexity_ad::Dual;
        use implexity_physics_lbm::moving::field::gas_tick::CoupledTickDirection;
        self.admit(previous, start, end, p)?;
        let zero = vec![0.; self.design_size()];
        let dd = dd.unwrap_or(&zero);
        if dp.len() != previous.len()
            || ds.len() != start.len()
            || de.len() != end.len()
            || dd.len() != p.design.len()
            || dp
                .iter()
                .chain(ds)
                .chain(de)
                .chain(dd)
                .any(|v| !v.is_finite())
            || dtime != 0.
        {
            return Err(CaeError::contract(
                "native gas tangent requires a fixed physical clock and finite matching dimensions",
            ));
        }
        let c = &self.config;
        let cfg = c.bulk.config();
        if c.bulk.blocking_saturation(start, p.design)?.max_fill != 0.
            || c.bulk.blocking_saturation(end, p.design)?.max_fill != 0.
        {
            return Err(CaeError::contract(
                "native gas tangent requires exclusive whole-body traction ownership without native PSM occupancy",
            ));
        }
        let ticks = cfg.substeps;
        let dt = cfg.macro_step_s / ticks as f64;
        let ports = c.connector.exchange().port_count();
        let offsets = vec![
            cfg.body_acceleration_m_s2
                .map(|a| Dual::constant(a * dt * 0.5));
            ports
        ];
        let mut cells = c
            .connector
            .exchange()
            .ports()
            .iter()
            .map(|p| p[0].0)
            .collect::<Vec<_>>();
        cells.sort_unstable();
        cells.dedup();
        let links = c
            .bulk
            .coupled_interface_links(c.connector.exchange().excluded_cells())?;
        let mut bulk = previous[..self.bulk_state_size].to_vec();
        let mut dbulk = dp[..self.bulk_state_size].to_vec();
        let mut gas = previous[self.bulk_state_size..]
            .iter()
            .zip(&dp[self.bulk_state_size..])
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect::<Vec<_>>();
        let design = p
            .design
            .iter()
            .zip(dd)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect::<Vec<_>>();
        let mut flux = vec![0.; self.trace_size()];
        let mut samples = vec![0.; self.names.len()];
        let nv = c.velocity_grid.velocity_m_s.len();
        for tick in 1..=ticks {
            let a = (tick - 1) as f64 / ticks as f64;
            let b = tick as f64 / ticks as f64;
            let at = start
                .iter()
                .zip(end)
                .zip(ds.iter().zip(de))
                .map(|((s, e), (ds, de))| Dual::new(s + (e - s) * a, [ds + (de - ds) * a]))
                .collect::<Vec<_>>();
            let bt = start
                .iter()
                .zip(end)
                .zip(ds.iter().zip(de))
                .map(|((s, e), (ds, de))| Dual::new(s + (e - s) * b, [ds + (de - ds) * b]))
                .collect::<Vec<_>>();
            let input = CoupledTickInput {
                macro_index: n,
                tick,
                previous: &bulk,
                start,
                end,
                design: p.design,
                time_scale: 1.,
            };
            let rows = c.bulk.coupled_tick_incoming(input, &cells)?;
            let drows = c
                .bulk
                .coupled_tick_incoming_tangent(input, &cells, &dbulk)?;
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
            let bulk_dual = bulk
                .iter()
                .zip(&dbulk)
                .map(|(v, d)| Dual::new(*v, [*d]))
                .collect::<Vec<_>>();
            let port_density = self.port_distributions(&bulk_dual, &offsets)?;
            let previous_kinetic = self.kinetic_projection(&gas)?;
            let assembly = c.geometry.assemble_with_boundaries(
                &at,
                &bt,
                &design,
                Dual::constant(dt),
                &previous_kinetic,
                &c.boundaries,
                port_density,
                c.velocity_grid.clone(),
                Dual::constant(c.gas_rt_m2_s2),
                Dual::constant(c.viscosity_pa_s),
                &c.transport_policy.kinetic,
            )?;
            let residual = if let (Some(layout), Some(mixed)) = (&self.mixed_layout, &c.mixed) {
                assembly.certify_stationary_continuum_slots(&mixed.continuum_slots)?;
                let previous_mixed = layout
                    .with_kinetic_projection(&gas, &assembly.inputs.gas.previous_inventory_kg)?;
                let inputs = primal_inputs(&assembly.inputs);
                let policy = crate::gas_mixed_network::MixedGasPolicy {
                    network: c.transport_policy.clone(),
                    continuum: mixed.continuum_policy.clone(),
                    lift: mixed.lift_policy.clone(),
                };
                let previous_primal = previous_mixed.iter().map(|q| q.re).collect::<Vec<_>>();
                let system = crate::gas_mixed_solver::MixedGasSystem::new(
                    inputs,
                    layout.clone(),
                    policy.clone(),
                    previous_primal.clone(),
                    cfg.density_kg_m3,
                )?
                .with_kinetic_subdomain(mixed.kinetic_subdomain.clone())?;
                let solved = solve(
                    &system,
                    &system.pack(&previous_primal)?,
                    &c.nonlinear_policy,
                )?;
                let primal = system.unpack(&solved.state)?;
                let constant = primal
                    .iter()
                    .map(|q| Dual::constant(*q))
                    .collect::<Vec<_>>();
                let parameter = crate::gas_mixed_network::physical_residual_scalar(
                    &assembly.inputs,
                    layout,
                    &constant,
                    &previous_mixed,
                    &policy,
                )?;
                let rhs = system.pack(
                    &parameter
                        .inventory_residual
                        .iter()
                        .map(|r| -r.eps[0])
                        .collect::<Vec<_>>(),
                )?;
                let tangent = implexity_solve::nonlinear_graph::solve_linear(
                    &system,
                    &solved.state,
                    &rhs,
                    false,
                    &c.nonlinear_policy,
                )?;
                let direction = system.unpack(&tangent.direction)?;
                gas = primal
                    .iter()
                    .zip(direction)
                    .map(|(&q, d)| Dual::new(q, [d]))
                    .collect();
                let result = crate::gas_mixed_network::physical_residual_scalar(
                    &assembly.inputs,
                    layout,
                    &gas,
                    &previous_mixed,
                    &policy,
                )?;
                crate::gas_multiscale_network::MultiscaleNetworkResidual {
                    cells: result.kinetic_cells,
                    inventory_residual_kg: result.inventory_residual,
                    face_packets: result.face_packets.into_iter().enumerate().collect(),
                    collision_heat_bath_energy_j: result.kinetic_collision_bath_energy_j,
                    forcing_work_j: vec![],
                    forcing_thermal_bath_energy_j: vec![],
                }
            } else {
                let inputs = primal_inputs(&assembly.inputs);
                let scale = cfg.density_kg_m3 * cfg.spacing_m.powi(3);
                let system = MultiscaleNetworkSystem::new(
                    inputs.clone(),
                    c.transport_policy.clone(),
                    vec![scale; inputs.gas.volume_m3.len()],
                )?;
                if system.dimension() == 0 {
                    gas = vec![Dual::constant(0.); self.gas_state_size];
                } else {
                    let scaling = self.gas_scaling(&inputs)?;
                    let scaled = implexity_solve::diagonal_graph::DiagonalGraph::new(
                        &system,
                        scaling.clone(),
                        scaling.clone(),
                    )?;
                    let solved = solve(
                        &scaled,
                        &scaled
                            .from_inner_state(&system.pack(&inputs.gas.previous_inventory_kg)?)?,
                        &c.nonlinear_policy,
                    )?;
                    let primal = system.unpack(&scaled.to_inner_state(&solved.state)?)?;
                    let constant = primal
                        .iter()
                        .map(|v| Dual::constant(*v))
                        .collect::<Vec<_>>();
                    let parameter =
                        physical_residual_scalar(&assembly.inputs, &constant, &c.transport_policy)?;
                    let mut rhs = vec![];
                    for (cell, volume) in inputs.gas.volume_m3.iter().enumerate() {
                        if *volume > 0. {
                            rhs.extend(
                                parameter.inventory_residual_kg[cell * nv..(cell + 1) * nv]
                                    .iter()
                                    .map(|r| -r.eps[0] / scale),
                            );
                        }
                    }
                    for (r, w) in rhs.iter_mut().zip(&scaling) {
                        *r /= w;
                    }
                    let tangent = implexity_solve::nonlinear_graph::solve_linear(
                        &scaled,
                        &solved.state,
                        &rhs,
                        false,
                        &c.nonlinear_policy,
                    )?;
                    let mut direction = vec![0.; primal.len()];
                    let mut offset = 0;
                    for (cell, volume) in inputs.gas.volume_m3.iter().enumerate() {
                        if *volume > 0. {
                            for k in 0..nv {
                                direction[cell * nv + k] =
                                    tangent.direction[offset + k] * scaling[offset + k] * scale;
                            }
                            offset += nv;
                        }
                    }
                    gas = primal
                        .iter()
                        .zip(direction)
                        .map(|(v, d)| Dual::new(*v, [d]))
                        .collect();
                }
                physical_residual_scalar(&assembly.inputs, &gas, &c.transport_policy)?
            };
            let mut packets = vec![
                TransientGasPacket {
                    mass_kg: Dual::constant(0.),
                    momentum_kg_m_s: [Dual::constant(0.); 3],
                    kinetic_energy_j: Dual::constant(0.),
                    wall_work_j: Dual::constant(0.),
                    wall_heat_j: Dual::constant(0.)
                };
                ports
            ];
            for (face, packet) in &residual.face_packets {
                if let TransientGasBoundary::Port { port } =
                    assembly.inputs.gas.faces[*face].boundary
                {
                    let dst = &mut packets[port];
                    dst.mass_kg += packet.mass_kg;
                    dst.kinetic_energy_j += packet.kinetic_energy_j;
                    for k in 0..3 {
                        dst.momentum_kg_m_s[k] += packet.momentum_kg_m_s[k];
                    }
                }
            }
            let wall = assembly.scatter_wall_packets_with_reference(
                &residual.face_packets,
                &at,
                &bt,
                Dual::constant(c.reference_pressure_pa),
            )?;
            for (v, g) in flux.iter_mut().zip(wall.load.force_flux_n) {
                *v += g.eps[0] / ticks as f64;
            }
            let reflected = c.bulk.coupled_tick_reflection(input, &links)?;
            let dreflected = c
                .bulk
                .coupled_tick_reflection_tangent(input, &links, &dbulk, 0.)?;
            let reflected_dual = reflected
                .iter()
                .zip(dreflected)
                .map(|((cell, r), (_, dr))| {
                    (*cell, std::array::from_fn(|k| Dual::new(r[k], [dr[k]])))
                })
                .collect::<Vec<_>>();
            let reference =
                self.reference_packets(&assembly.inputs, &bulk_dual, &packets, &links)?;
            let packets = reference.packets;
            let incoming = self.incoming(&rows_dual, &packets, &reflected_dual, &offsets)?;
            let primal_rows = incoming
                .incoming_rows
                .iter()
                .map(|(c, row)| (*c, row.iter().map(|v| v.re).collect()))
                .collect::<Vec<_>>();
            let direction_rows = incoming
                .incoming_rows
                .iter()
                .map(|(c, row)| (*c, row.iter().map(|v| v.eps[0]).collect()))
                .collect::<Vec<_>>();
            let native_tangent = c.bulk.coupled_tick_with_incoming_tangent(
                input,
                CoupledTickDirection {
                    previous: &dbulk,
                    start: ds,
                    end: de,
                    design: dd,
                    time_scale: 0.,
                },
                &primal_rows,
                &direction_rows,
            )?;
            let native = c.bulk.coupled_tick_with_incoming(input, &primal_rows)?;
            for (v, f) in flux.iter_mut().zip(native_tangent.flux) {
                *v += f;
            }
            for (v, s) in samples.iter_mut().zip(native_tangent.samples) {
                *v += s;
            }
            bulk = native.state;
            dbulk = native_tangent.state;
        }
        dbulk.extend(gas.iter().map(|g| g.eps[0]));
        Ok(SubcycleTangent {
            state: dbulk,
            flux,
            samples,
        })
    }
    fn subcycle_adjoint(
        &self,
        _n: usize,
        _previous: &[f64],
        _start: &[f64],
        _end: &[f64],
        _p: StepParameters<'_>,
        _sb: &[f64],
        _fb: &[f64],
        _ob: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        Err(CaeError::contract(
            "native gas coupled adjoint has not been qualified",
        ))
    }
}

fn primal_inputs(
    a: &crate::gas_multiscale_network::MultiscaleNetworkInputs<implexity_ad::Dual<1>>,
) -> crate::gas_multiscale_network::MultiscaleNetworkInputs {
    let v = |x: implexity_ad::Dual<1>| x.re;
    crate::gas_multiscale_network::MultiscaleNetworkInputs {
        gas: crate::gas_transient_kinetic::TransientGasInputs {
            volume_m3: a.gas.volume_m3.iter().copied().map(v).collect(),
            previous_volume_m3: a.gas.previous_volume_m3.iter().copied().map(v).collect(),
            previous_inventory_kg: a.gas.previous_inventory_kg.iter().copied().map(v).collect(),
            faces: a
                .gas
                .faces
                .iter()
                .map(|f| crate::gas_transient_kinetic::TransientGasFace {
                    left: f.left,
                    boundary: f.boundary.clone(),
                    area_m2: v(f.area_m2),
                    normal: f.normal.map(v),
                    ale_velocity_m_s: f.ale_velocity_m_s.map(v),
                    wall_velocity_m_s: f.wall_velocity_m_s.map(v),
                })
                .collect(),
            port_density_kg_m3: a
                .gas
                .port_density_kg_m3
                .iter()
                .map(|p| p.iter().copied().map(v).collect())
                .collect(),
            grid: a.gas.grid.clone(),
            gas_rt_m2_s2: v(a.gas.gas_rt_m2_s2),
            viscosity_pa_s: v(a.gas.viscosity_pa_s),
            step_s: v(a.gas.step_s),
        },
        force_density_n_m3: a
            .force_density_n_m3
            .as_ref()
            .map(|p| p.iter().map(|p| p.map(v)).collect()),
        left_centroid_to_face_m: a.left_centroid_to_face_m.iter().map(|p| p.map(v)).collect(),
        right_centroid_to_face_m: a
            .right_centroid_to_face_m
            .iter()
            .map(|p| p.map(|p| p.map(v)))
            .collect(),
    }
}

fn mixed_gas_totals(
    layout: &crate::gas_mixed_network::MixedGasLayout,
    gas: &[f64],
    grid: &TransientVelocityGrid,
    gas_rt_m2_s2: f64,
) -> CaeResult<[f64; 5]> {
    if gas.len() != layout.dimension() || grid.velocity_m_s.len() != layout.population_count() {
        return Err(CaeError::contract("mixed gas totals shape mismatch"));
    }
    let mut total = [0.; 5];
    for (cell, &continuum) in layout.continuum_cells().iter().enumerate() {
        if continuum { continue; }
        let Some(offset) = layout.cell_offset(cell) else { continue; };
        let q = &gas[offset..offset + layout.population_count()];
        if q.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(CaeError::contract("negative or nonfinite mixed kinetic inventory"));
        }
        let m = crate::gas_transient_kinetic::moments(q, grid);
        total[0] += m.mass_kg;
        for k in 0..3 { total[k + 1] += m.momentum_kg_m_s[k]; }
        total[4] += m.kinetic_energy_j;
    }
    for (cell, &continuum) in layout.continuum_cells().iter().enumerate() {
        if !continuum { continue; }
        let offset = layout.cell_offset(cell).ok_or_else(|| CaeError::contract("continuum checkpoint has no state offset"))?;
        let m = &gas[offset..offset + 4];
        if m[0] <= 0.0 || m.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("continuum checkpoint mass must remain positive and moments finite"));
        }
        total[0] += m[0];
        for j in 0..3 { total[j + 1] += m[j + 1]; }
        total[4] += (m[1] * m[1] + m[2] * m[2] + m[3] * m[3]) / m[0] * 0.5
            + 1.5 * gas_rt_m2_s2 * m[0];
    }
    Ok(total)
}

fn gas_totals(q: &[f64], grid: &TransientVelocityGrid) -> [f64; 5] {
    let mut total = [0.; 5];
    for cell in q.chunks_exact(grid.velocity_m_s.len()) {
        let m = crate::gas_transient_kinetic::moments(cell, grid);
        total[0] += m.mass_kg;
        for k in 0..3 {
            total[k + 1] += m.momentum_kg_m_s[k];
        }
        total[4] += m.kinetic_energy_j;
    }
    total
}

enum NativeGasAdjointSolve {
    Molecular {
        system: MultiscaleNetworkSystem,
        scaling: Vec<f64>,
        state: Vec<f64>,
        volumes: Vec<f64>,
        mass_scale: f64,
        populations: usize,
    },
    Mixed {
        system: crate::gas_mixed_solver::MixedGasSystem,
        state: Vec<f64>,
    },
}
impl NativeGasAdjointSolve {
    fn residual_cotangent(
        &self,
        gas_bar: &[f64],
        policy: &NonlinearGraphPolicy,
    ) -> CaeResult<Vec<f64>> {
        match self {
            Self::Mixed { system, state } => {
                let rhs = system.pack_cotangent(gas_bar)?;
                let lambda = implexity_solve::nonlinear_graph::solve_linear(
                    system, state, &rhs, true, policy,
                )?;
                Ok(system
                    .unpack_cotangent(&lambda.direction)?
                    .into_iter()
                    .map(|v| -v)
                    .collect())
            }
            Self::Molecular {
                system,
                scaling,
                state,
                volumes,
                mass_scale,
                populations,
            } => {
                let scaled = implexity_solve::diagonal_graph::DiagonalGraph::new(
                    system,
                    scaling.clone(),
                    scaling.clone(),
                )?;
                let mut rhs = vec![];
                let mut o = 0;
                for (cell, &v) in volumes.iter().enumerate() {
                    if v > 0.0 {
                        for k in 0..*populations {
                            rhs.push(gas_bar[cell * populations + k] * mass_scale * scaling[o + k]);
                        }
                        o += populations;
                    }
                }
                let lambda = implexity_solve::nonlinear_graph::solve_linear(
                    &scaled, state, &rhs, true, policy,
                )?;
                let mut bars = vec![0.; gas_bar.len()];
                let mut o = 0;
                for (cell, &v) in volumes.iter().enumerate() {
                    if v > 0.0 {
                        for k in 0..*populations {
                            bars[cell * populations + k] =
                                -lambda.direction[o + k] / scaling[o + k] / mass_scale;
                        }
                        o += populations;
                    }
                }
                Ok(bars)
            }
        }
    }
}
impl NativeGasCoupledField {
    fn fixed_state_gas_output_vjp(
        &self,
        inputs: &crate::gas_multiscale_network::MultiscaleNetworkInputs,
        topology: &crate::gas_multiscale_network::MultiscaleNetworkTopology,
        gas: &[f64],
        previous: &[f64],
        bars: &crate::gas_vjp::NetworkOutputBars,
    ) -> CaeResult<crate::gas_vjp::NetworkInputBars> {
        if let (Some(layout), Some(mixed)) = (&self.mixed_layout, &self.config.mixed) {
            let policy = crate::gas_mixed_network::MixedGasPolicy {
                network: self.config.transport_policy.clone(),
                continuum: mixed.continuum_policy.clone(),
                lift: mixed.lift_policy.clone(),
            };
            let b = crate::gas_mixed_vjp::fixed_state_output_vjp(
                inputs,
                layout,
                gas,
                previous,
                &crate::gas_mixed_vjp::MixedGasOutputBars {
                    inventory_residual: bars.inventory_residual_kg.clone(),
                    face_packets: bars.face_packets.clone(),
                },
                &policy,
            )?;
            Ok(crate::gas_vjp::NetworkInputBars {
                current_inventory_kg: b.current_inventory,
                previous_inventory_kg: b.previous_inventory,
                port_density_kg_m3: b.port_density_kg_m3,
            })
        } else {
            crate::gas_vjp::output_vjp(inputs, topology, gas, bars, &self.config.transport_policy)
        }
    }
}
impl NativeGasCoupledField {
    pub fn fixed_geometry_state_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        trace: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.admit(previous, trace, trace, p)?;
        let c = &self.config;
        let cfg = c.bulk.config();
        if cfg.substeps != 1
            || c.bulk.blocking_saturation(trace, p.design)?.max_fill != 0.
            || state_bar.len() != self.state_size()
            || flux_bar.len() != self.trace_size()
            || sample_bar.len() != self.names.len()
            || state_bar
                .iter()
                .chain(flux_bar)
                .chain(sample_bar)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "fixed-geometry gas state transpose requires one native tick and exclusive whole-body traction ownership",
            ));
        }
        let dt = cfg.macro_step_s;
        let ports = c.connector.exchange().port_count();
        let offsets = vec![cfg.body_acceleration_m_s2.map(|a| 0.5 * a * dt); ports];
        let bulk = &previous[..self.bulk_state_size];
        let oldgas = &previous[self.bulk_state_size..];
        let input = CoupledTickInput {
            macro_index: n,
            tick: 1,
            previous: bulk,
            start: trace,
            end: trace,
            design: p.design,
            time_scale: 1.,
        };
        let mut cells: Vec<_> = c
            .connector
            .exchange()
            .ports()
            .iter()
            .map(|p| p[0].0)
            .collect();
        cells.sort_unstable();
        cells.dedup();
        let links = c
            .bulk
            .coupled_interface_links(c.connector.exchange().excluded_cells())?;
        let rows = c.bulk.coupled_tick_incoming(input, &cells)?;
        let reflected = c.bulk.coupled_tick_reflection(input, &links)?;
        let density = self.port_distributions(bulk, &offsets)?;
        let old_kinetic = self.kinetic_projection(oldgas)?;
        let assembly = c.geometry.assemble_with_boundaries(
            trace,
            trace,
            p.design,
            dt,
            &old_kinetic,
            &c.boundaries,
            density,
            c.velocity_grid.clone(),
            c.gas_rt_m2_s2,
            c.viscosity_pa_s,
            &c.transport_policy.kinetic,
        )?;
        if assembly.inputs.gas.previous_inventory_kg != old_kinetic {
            return Err(CaeError::contract(
                "fixed-geometry gas transpose requires identity state-slot remapping",
            ));
        }
        let (gas, residual, adjoint) = if let (Some(layout), Some(mixed)) =
            (&self.mixed_layout, &c.mixed)
        {
            assembly.certify_stationary_continuum_slots(&mixed.continuum_slots)?;
            let policy = crate::gas_mixed_network::MixedGasPolicy {
                network: c.transport_policy.clone(),
                continuum: mixed.continuum_policy.clone(),
                lift: mixed.lift_policy.clone(),
            };
            let system = crate::gas_mixed_solver::MixedGasSystem::new(
                assembly.inputs.clone(),
                layout.clone(),
                policy,
                oldgas.to_vec(),
                cfg.density_kg_m3,
            )?
            .with_kinetic_subdomain(mixed.kinetic_subdomain.clone())?;
            let solved = solve(&system, &system.pack(oldgas)?, &c.nonlinear_policy)?;
            let gas = system.unpack(&solved.state)?;
            let result = system.physical_result(&solved.state)?;
            let residual = crate::gas_multiscale_network::MultiscaleNetworkResidual {
                cells: result.kinetic_cells,
                inventory_residual_kg: result.inventory_residual,
                face_packets: result.face_packets.into_iter().enumerate().collect(),
                collision_heat_bath_energy_j: result.kinetic_collision_bath_energy_j,
                forcing_work_j: vec![],
                forcing_thermal_bath_energy_j: vec![],
            };
            (
                gas,
                residual,
                NativeGasAdjointSolve::Mixed {
                    system,
                    state: solved.state,
                },
            )
        } else {
            let scale = cfg.density_kg_m3 * cfg.spacing_m.powi(3);
            let system = MultiscaleNetworkSystem::new(
                assembly.inputs.clone(),
                c.transport_policy.clone(),
                vec![scale; assembly.inputs.gas.volume_m3.len()],
            )?;
            if system.dimension() == 0 {
                return Err(CaeError::contract(
                    "fixed-geometry gas transpose requires a positive-volume gas domain",
                ));
            }
            let scaling = self.gas_scaling(&assembly.inputs)?;
            let scaled = implexity_solve::diagonal_graph::DiagonalGraph::new(
                &system,
                scaling.clone(),
                scaling.clone(),
            )?;
            let solved = solve(
                &scaled,
                &scaled.from_inner_state(&system.pack(oldgas)?)?,
                &c.nonlinear_policy,
            )?;
            let gas = system.unpack(&scaled.to_inner_state(&solved.state)?)?;
            let residual = physical_residual_scalar(&assembly.inputs, &gas, &c.transport_policy)?;
            let adjoint = NativeGasAdjointSolve::Molecular {
                system,
                scaling,
                state: solved.state,
                volumes: assembly.inputs.gas.volume_m3.clone(),
                mass_scale: scale,
                populations: c.velocity_grid.velocity_m_s.len(),
            };
            (gas, residual, adjoint)
        };
        let mut packets = vec![
            TransientGasPacket {
                mass_kg: 0.,
                momentum_kg_m_s: [0.; 3],
                kinetic_energy_j: 0.,
                wall_work_j: 0.,
                wall_heat_j: 0.
            };
            ports
        ];
        for (face, packet) in &residual.face_packets {
            if let TransientGasBoundary::Port { port } = assembly.inputs.gas.faces[*face].boundary {
                packets[port].mass_kg += packet.mass_kg;
                packets[port].kinetic_energy_j += packet.kinetic_energy_j;
                for k in 0..3 {
                    packets[port].momentum_kg_m_s[k] += packet.momentum_kg_m_s[k];
                }
            }
        }
        let reference = self.reference_packets(&assembly.inputs, bulk, &packets, &links)?;
        let packets = reference.packets;
        let incoming = self.incoming(&rows, &packets, &reflected, &offsets)?;
        let native = c.bulk.coupled_tick_with_incoming_adjoint(
            input,
            &incoming.incoming_rows,
            &state_bar[..self.bulk_state_size],
            flux_bar,
            sample_bar,
        )?;
        let connector = match c.bulk.lattice() {
            LatticeKind::D2Q9 => c.connector.replace_incoming_vjp::<9, D2Q9>(
                &rows,
                &packets,
                &reflected,
                &offsets,
                &native.incoming,
                [0.; 4],
                0.,
            )?,
            LatticeKind::D3Q19 => c.connector.replace_incoming_vjp::<19, D3Q19>(
                &rows,
                &packets,
                &reflected,
                &offsets,
                &native.incoming,
                [0.; 4],
                0.,
            )?,
            LatticeKind::D3Q27 => c.connector.replace_incoming_vjp::<27, D3Q27>(
                &rows,
                &packets,
                &reflected,
                &offsets,
                &native.incoming,
                [0.; 4],
                0.,
            )?,
        };
        let (reference_bars, mut reference_pressure_bar) = match c.bulk.lattice() {
            LatticeKind::D2Q9 => c.connector.reference_projection_vjp::<9, D2Q9>(
                &links,
                &connector.packet_mass_momentum_energy,
            )?,
            LatticeKind::D3Q19 => c.connector.reference_projection_vjp::<19, D3Q19>(
                &links,
                &connector.packet_mass_momentum_energy,
            )?,
            LatticeKind::D3Q27 => c.connector.reference_projection_vjp::<27, D3Q27>(
                &links,
                &connector.packet_mass_momentum_energy,
            )?,
        };
        for face in &assembly.inputs.gas.faces {
            if let TransientGasBoundary::Port { port } = face.boundary {
                for k in 0..3 {
                    reference_pressure_bar +=
                        reference_bars[port][k] * face.area_m2 * face.normal[k] * dt;
                }
            }
        }
        let pressure_state_bars = match c.bulk.lattice() {
            LatticeKind::D2Q9 => c
                .connector
                .interface_reference_pressure_vjp::<9, D2Q9>(reference_pressure_bar)?,
            LatticeKind::D3Q19 => c
                .connector
                .interface_reference_pressure_vjp::<19, D3Q19>(reference_pressure_bar)?,
            LatticeKind::D3Q27 => c
                .connector
                .interface_reference_pressure_vjp::<27, D3Q27>(reference_pressure_bar)?,
        };
        let mut face_bars = vec![[0.; 7]; assembly.inputs.gas.faces.len()];
        for (face, _) in &residual.face_packets {
            if let TransientGasBoundary::Port { port } = assembly.inputs.gas.faces[*face].boundary {
                face_bars[*face][..5].copy_from_slice(&connector.packet_mass_momentum_energy[port]);
            }
        }
        let by_face: BTreeMap<_, _> = residual.face_packets.iter().map(|(i, p)| (*i, p)).collect();
        for stage in &assembly.wall_stages {
            let impulses = stage
                .face_indices
                .iter()
                .map(|i| {
                    let face = &assembly.inputs.gas.faces[*i];
                    crate::reference_traction::adjust_wall_reference(
                        by_face[i].momentum_kg_m_s,
                        face.normal.map(|n| n * face.area_m2 * dt),
                        face.wall_velocity_m_s,
                        c.reference_pressure_pa,
                    )
                    .map(|a| a.momentum_impulse_kg_m_s)
                })
                .collect::<CaeResult<Vec<_>>>()?;
            let bars = stage
                .transfer
                .scatter_adjoint(&impulses, trace, trace, dt, flux_bar, [0.; 3], 0.)?;
            for (face, bar) in stage.face_indices.iter().zip(bars.impulses_n_s) {
                for k in 0..3 {
                    face_bars[*face][k + 1] += bar[k];
                }
            }
        }
        let topology = crate::gas_multiscale_network::MultiscaleNetworkTopology::new_with_policy(
            &assembly.inputs,
            &c.transport_policy,
        )?;
        let direct = self.fixed_state_gas_output_vjp(
            &assembly.inputs,
            &topology,
            &gas,
            oldgas,
            &crate::gas_vjp::NetworkOutputBars {
                inventory_residual_kg: vec![0.; gas.len()],
                face_packets: face_bars.clone(),
            },
        )?;
        let gas_bar = state_bar[self.bulk_state_size..]
            .iter()
            .zip(&direct.current_inventory_kg)
            .map(|(a, b)| a + b)
            .collect::<Vec<_>>();
        let residual_bars = adjoint.residual_cotangent(&gas_bar, &c.nonlinear_policy)?;
        let parameters = self.fixed_state_gas_output_vjp(
            &assembly.inputs,
            &topology,
            &gas,
            oldgas,
            &crate::gas_vjp::NetworkOutputBars {
                inventory_residual_kg: residual_bars,
                face_packets: face_bars,
            },
        )?;
        let thermo = match c.bulk.lattice() {
            LatticeKind::D2Q9 => c.connector.port_distributions_vjp::<9, D2Q9>(
                bulk,
                &offsets,
                &c.velocity_grid,
                c.gas_rt_m2_s2,
                &c.transport_policy.kinetic,
                &parameters.port_density_kg_m3,
            )?,
            LatticeKind::D3Q19 => c.connector.port_distributions_vjp::<19, D3Q19>(
                bulk,
                &offsets,
                &c.velocity_grid,
                c.gas_rt_m2_s2,
                &c.transport_policy.kinetic,
                &parameters.port_density_kg_m3,
            )?,
            LatticeKind::D3Q27 => c.connector.port_distributions_vjp::<27, D3Q27>(
                bulk,
                &offsets,
                &c.velocity_grid,
                c.gas_rt_m2_s2,
                &c.transport_policy.kinetic,
                &parameters.port_density_kg_m3,
            )?,
        };
        let pulled = c
            .bulk
            .coupled_tick_incoming_adjoint(input, &connector.incoming_rows)?;
        let reflection =
            c.bulk
                .coupled_tick_reflection_adjoint(input, &links, &connector.reflections)?;
        let mut out = native.native.previous;
        for (a, (b, d)) in out.iter_mut().zip(pulled.iter().zip(&reflection.previous)) {
            *a += b + d;
        }
        let q = match c.bulk.lattice() {
            LatticeKind::D2Q9 => 9,
            LatticeKind::D3Q19 => 19,
            LatticeKind::D3Q27 => 27,
        };
        for (cell, bar) in thermo.populations.into_iter().chain(pressure_state_bars) {
            for k in 0..q {
                out[k * cfg.solid_mask.len() + cell] += bar[k];
            }
        }
        out.extend(parameters.previous_inventory_kg);
        Ok(out)
    }
}
