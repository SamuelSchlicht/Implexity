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

use crate::gas_continuous_layout::{ContinuousGasLayout, ContinuousGasRepresentation};
use crate::gas_continuous_mixed::ContinuousMixedNetwork;
use crate::gas_continuous_mixed_solver::{
    ContinuousMixedState, ContinuousMixedStep, ContinuousMixedSystem,
};
use crate::gas_continuum_fv::ContinuumFvPolicy;
use crate::gas_quadratic_gaussian::QuadraticGaussianBasis;
use crate::gas_quadratic_network::{PreparedQuadraticGasNetwork, QuadraticGasNetworkPolicy};
use crate::gas_quadratic_network_solver::QuadraticGasNetworkSolvePolicy;
use crate::gas_transient_kinetic::{TransientGasBoundary, TransientGasPacket};
use crate::native_continuous_checkpoint::NativeContinuousCheckpointLayout;
use crate::native_continuous_ports::{NativeContinuousAcceptedPortStage, NativeContinuousGasPorts};
use crate::native_gas_geometry_adapter::{
    NativeGasExteriorBoundary, NativeGasGeometryAdapter, NativeGasGeometryPath,
    NativeReferencedGasWallLoad,
};
use crate::native_gas_port_coupling::{NativeGasMomentIncoming, NativeGasPortConnector};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    field::{
        gas_tick::{CoupledInterfaceLink, CoupledTickInput},
        AnyMovingLbm, LatticeKind,
    },
    lattice::{D2Q9, D3Q19, D3Q27},
};
use implexity_solve::{
    multirate_coupling::{SubcycleRecord, SubcycledField},
    time_stepper::{StepDiagnostics, StepParameters},
};
use std::{collections::BTreeMap, sync::Arc};

pub struct NativeContinuousCoupledFieldConfig {
    pub bulk: Arc<AnyMovingLbm>,
    pub geometry: Arc<NativeGasGeometryAdapter>,
    pub connector: Arc<NativeGasPortConnector>,
    pub boundaries: BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
    pub layout: Arc<ContinuousGasLayout>,
    pub transport_policy: QuadraticGasNetworkPolicy,
    pub continuum_policy: ContinuumFvPolicy,
    pub resolved_kinetic_sgs: Option<crate::gas_continuous_mixed::KineticResolvedSgsRegion>,
    pub solve_policy: QuadraticGasNetworkSolvePolicy,
    pub use_selected_kkt_newton: bool,
    pub gaussian_primal_cache_entries: Option<usize>,
    pub initial_trace: Vec<f64>,
    pub closed_contact_law: Option<Arc<crate::patch_unilateral_contact::PatchUnilateralLaw>>,
    pub initial_closed_witness:
        Option<Arc<crate::native_closed_release::NativeClosedGeometryWitness>>,
    pub initial_closed_release_certificate:
        Option<Arc<crate::native_closed_release::NativeClosedReleaseCertificate>>,
    pub initial_density_kg_m3: f64,
    pub initial_velocity_m_s: [f64; 3],
    pub gas_rt_m2_s2: f64,
    pub viscosity_pa_s: f64,
    pub reference_pressure_pa: f64,
}
pub(crate) enum NativeContinuousGeometryContext {
    Primal(Arc<crate::native_closed_release::NativeClosedGeometryWitness>),
    Derivative(Arc<crate::native_closed_release::NativeClosedReleaseCertificate>),
    Trial(Arc<crate::native_closed_release::NativeClosedTrialCertificate>),
}
pub struct NativeContinuousCoupledField {
    pub(crate) config: NativeContinuousCoupledFieldConfig,
    pub(crate) checkpoint: NativeContinuousCheckpointLayout,
    pub(crate) ports: NativeContinuousGasPorts,
    pub(crate) continuum_slots: Vec<usize>,
    pub(crate) kinetic_mask: Vec<bool>,
    pub(crate) port_cells: Vec<usize>,
    pub(crate) links: Vec<CoupledInterfaceLink>,
    transport_preconditioner: Option<crate::gas_continuous_preconditioner::ContinuousTransportPreconditionerPolicy>,
}
pub struct NativeContinuousTick {
    pub geometry: NativeGasGeometryPath<f64>,
    pub system: ContinuousMixedSystem,
    pub solved: ContinuousMixedStep,
    pub incoming: NativeGasMomentIncoming<f64>,
    pub accepted_rates: Vec<(usize, f64)>,
    pub referenced_wall_load: NativeReferencedGasWallLoad<f64>,
    pub projected_packets: Vec<[f64; 10]>,
    pub maximum_reference_moment_defect: f64,
}
macro_rules! lattice_call {
    ($owner:expr,$name:ident($($arg:expr),*)) => {
        match $owner.config.bulk.lattice() {
            LatticeKind::D2Q9 => $owner.ports.$name::<f64,9,D2Q9>($($arg),*),
            LatticeKind::D3Q19 => $owner.ports.$name::<f64,19,D3Q19>($($arg),*),
            LatticeKind::D3Q27 => $owner.ports.$name::<f64,27,D3Q27>($($arg),*),
        }
    }
}
impl NativeContinuousCoupledField {
    pub(crate) fn configure_network<S: crate::gas_transient_kinetic::TransientGasScalar>(
        &self,
        network: ContinuousMixedNetwork<S>,
    ) -> CaeResult<ContinuousMixedNetwork<S>> {
        match &self.config.resolved_kinetic_sgs {
            Some(region) => network.with_resolved_sgs(region.clone()),
            None => Ok(network),
        }
    }

    pub(crate) fn prepare_network_for_path<S: crate::gas_geometry_time::GeometryTimeBase>(
        &self,
        path: &crate::native_gas_geometry_adapter::NativeGasGeometryPath<S>,
        averaging_window_s: S,
    ) -> CaeResult<ContinuousMixedNetwork<S>> {
        let c = &self.config;
        let geometry = crate::native_continuous_gas::network_geometry_path(
            path,
            &vec![averaging_window_s; path.faces.len()],
        )?;
        let prepared = PreparedQuadraticGasNetwork::new_mixed_with_exact_primal_cache(
            c.layout.basis().clone(),
            geometry,
            c.transport_policy.clone(),
            self.kinetic_mask.clone(),
            c.gaussian_primal_cache_entries,
        )?;
        let prepared = if path.wall_interior_anchors.iter().any(Option::is_some) {
            prepared.with_wall_interior_anchors(path.wall_interior_anchors.clone())?
        } else {
            prepared
        };
        self.configure_network(ContinuousMixedNetwork::new(
            prepared,
            (*c.layout).clone(),
            c.continuum_policy.clone(),
        )?)
    }

    pub fn new(config: NativeContinuousCoupledFieldConfig) -> CaeResult<Self> {
        if config.closed_contact_law.is_some() != config.initial_closed_witness.is_some() {
            return Err(CaeError::contract(
                "native closed geometry requires its shared contact law and initial witness",
            ));
        }
        let cfg = config.bulk.config();
        let grid = &config.geometry.owner.capacity_owner().grid;
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
            || cfg.body_acceleration_m_s2.iter().any(|v| *v != 0.)
            || cfg.substeps == 0
            || config.layout.slots() != config.geometry.layout.slots()
            || config.connector.exchange().port_count() == 0
            || grid.origin_m != cfg.origin_m
            || grid.spacing_m != cfg.spacing_m
            || grid.shape != cfg.shape
            || grid.cells != config.connector.exchange().excluded_cells()
        {
            return Err(CaeError::contract(
                "native continuous field configuration or ownership mismatch",
            ));
        }
        let pressure = cfg.density_kg_m3 * config.gas_rt_m2_s2;
        let native_step = cfg.macro_step_s / cfg.substeps as f64;
        let native_pressure = cfg.density_kg_m3 * (cfg.spacing_m / native_step).powi(2) / 3.;
        if !native_pressure.is_finite()
            || (native_pressure - pressure).abs()
                > 128. * f64::EPSILON * native_pressure.abs().max(pressure.abs())
            || !pressure.is_finite()
            || (pressure - config.reference_pressure_pa).abs()
                > 128. * f64::EPSILON * pressure.abs().max(config.reference_pressure_pa)
        {
            return Err(CaeError::contract(
                "native continuous gas reference EOS mismatch",
            ));
        }
        config
            .connector
            .exchange()
            .validate_native_mask(&cfg.solid_mask)?;
        config.connector.exchange().validate_native_scales(
            cfg.spacing_m,
            cfg.density_kg_m3,
            cfg.macro_step_s / cfg.substeps as f64,
        )?;
        if config.connector.reference_pressure_pa()!=config.reference_pressure_pa
            || config.connector.exchange().ports().iter().any(|p|p.len()!=1||p[0].1!=1.)
            || config.boundaries.values().any(|b|matches!(b,NativeGasExteriorBoundary::Port{port} if *port>=config.connector.exchange().port_count()))
        { return Err(CaeError::contract("native continuous gas exchange contract mismatch")); }
        let continuum_slots = (0..config.layout.slots())
            .filter(|&s| {
                config.layout.representation(s).unwrap() == ContinuousGasRepresentation::Continuum
            })
            .collect();
        let kinetic_mask = (0..config.layout.slots())
            .map(|s| {
                config.layout.representation(s).unwrap() == ContinuousGasRepresentation::Kinetic
            })
            .collect();
        let mut port_cells = config
            .connector
            .exchange()
            .ports()
            .iter()
            .map(|p| p[0].0)
            .collect::<Vec<_>>();
        port_cells.sort_unstable();
        port_cells.dedup();
        let links = config
            .bulk
            .coupled_interface_links(config.connector.exchange().excluded_cells())?;
        let gaussian = QuadraticGaussianBasis::new(
            config.layout.basis().clone(),
            config.layout.basis().reference_speed_m_s(),
        )?;
        let gaussian = Arc::new(match config.gaussian_primal_cache_entries {
            Some(entries) => gaussian.with_exact_primal_cache(entries)?,
            None => gaussian,
        });
        let ports = NativeContinuousGasPorts::new(
            config.connector.clone(),
            gaussian,
            config.transport_policy.entropy.clone(),
        )?;
        let checkpoint = NativeContinuousCheckpointLayout::new(
            config.bulk.state_size(),
            config.layout.clone(),
            config.connector.exchange().port_count(),
        )?;
        Ok(Self {
            config,
            checkpoint,
            ports,
            continuum_slots,
            kinetic_mask,
            port_cells,
            links,
            transport_preconditioner: None,
        })
    }
    pub fn with_transport_preconditioner(
        mut self,
        policy: crate::gas_continuous_preconditioner::ContinuousTransportPreconditionerPolicy,
    ) -> CaeResult<Self> {
        policy.validate()?;
        self.transport_preconditioner = Some(policy);
        Ok(self)
    }
    pub fn state_size(&self) -> usize {
        self.checkpoint.dimension()
    }
    pub fn checkpoint_layout(&self) -> &NativeContinuousCheckpointLayout {
        &self.checkpoint
    }
    pub fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        let c = &self.config;
        let bulk = c.bulk.initial_state(design)?;
        let path = match c.initial_closed_witness.as_ref() {
            Some(witness) => {
                witness.validate_macro(
                    0,
                    StepParameters {
                        design,
                        time_scale: 1.,
                    },
                )?;
                c.geometry.assemble_geometry_path_with_closed_witness(
                    &c.initial_trace,
                    &c.initial_trace,
                    design,
                    c.bulk.config().macro_step_s,
                    &c.boundaries,
                    witness,
                )?
            }
            None => c.geometry.assemble_geometry_path(
                &c.initial_trace,
                &c.initial_trace,
                design,
                c.bulk.config().macro_step_s,
                &c.boundaries,
            )?,
        };
        path.certify_stationary_continuum_slots(&self.continuum_slots)?;
        let volumes = &path.old_geometry.aggregated.volumes_m3;
        let kinetic_slots = (0..c.layout.slots())
            .filter(|&s| self.kinetic_mask[s])
            .collect::<Vec<_>>();
        let kinetic_volumes = kinetic_slots
            .iter()
            .map(|&s| volumes[s])
            .collect::<Vec<_>>();
        let inventory = self.ports.initial_gas_inventory(
            &kinetic_volumes,
            c.initial_density_kg_m3,
            c.initial_velocity_m_s,
            c.gas_rt_m2_s2,
        )?;
        let mut gas = vec![0.; c.layout.state_size()];
        for (&slot, row) in kinetic_slots.iter().zip(inventory) {
            if let Some(row) = row {
                gas[c.layout.range(slot)?].copy_from_slice(&row);
            }
        }
        for &slot in &self.continuum_slots {
            let range = c.layout.range(slot)?;
            let mass = c.initial_density_kg_m3 * volumes[slot];
            gas[range.start] = mass;
            for k in 0..3 {
                gas[range.start + 1 + k] = mass * c.initial_velocity_m_s[k];
            }
        }
        c.layout.validate(&gas, volumes)?;
        let stress = lattice_call!(self, initial_stress(&bulk))?;
        self.checkpoint.pack(&bulk, &gas, &stress)
    }
    pub fn prepare_tick(
        &self,
        input: CoupledTickInput<'_>,
        gas: &[f64],
        stress: &[f64],
    ) -> CaeResult<NativeContinuousTick> {
        self.prepare_tick_with_closed_context(input, gas, stress, None)
    }
    pub(crate) fn prepare_tick_with_closed_context(
        &self,
        input: CoupledTickInput<'_>,
        gas: &[f64],
        stress: &[f64],
        context: Option<&NativeContinuousGeometryContext>,
    ) -> CaeResult<NativeContinuousTick> {
        let c = &self.config;
        let cfg = c.bulk.config();
        if input.time_scale != 1. || input.tick == 0 || input.tick > cfg.substeps {
            return Err(CaeError::contract("native continuous tick timing mismatch"));
        }
        c.bulk
            .coupled_tick_check_collision_exchange_ports(input, &self.port_cells)?;
        let a = (input.tick - 1) as f64 / cfg.substeps as f64;
        let b = input.tick as f64 / cfg.substeps as f64;
        let at = input
            .start
            .iter()
            .zip(input.end)
            .map(|(s, e)| s + (e - s) * a)
            .collect::<Vec<_>>();
        let bt = input
            .start
            .iter()
            .zip(input.end)
            .map(|(s, e)| s + (e - s) * b)
            .collect::<Vec<_>>();
        let dt = cfg.macro_step_s / cfg.substeps as f64;
        let path = match context {
            None => c
                .geometry
                .assemble_geometry_path(&at, &bt, input.design, dt, &c.boundaries)?,
            Some(NativeContinuousGeometryContext::Primal(witness)) => {
                witness.validate_macro(
                    input.macro_index,
                    StepParameters {
                        design: input.design,
                        time_scale: input.time_scale,
                    },
                )?;
                witness.validate_path(
                    &c.geometry.kinematics,
                    input.start,
                    input.end,
                    input.design,
                )?;
                let law = c.closed_contact_law.as_ref().ok_or_else(|| {
                    CaeError::contract("native primal geometry lacks its contact owner")
                })?;
                let sample=self.primal_birth_sample_for_tick(input.macro_index,input.tick,input.start,input.end,input.design,input.time_scale,witness.clone())?;
                let stages=self.primal_birth_stages_from_sample(sample.clone())?;
                let mut path=crate::native_gas_geometry_adapter::NativeGasGeometryAdapter::assemble_retained_birth_primal_path(&c.geometry,sample.child_start(),sample.child_end(),input.design,dt,&c.boundaries,sample.primal_child_witness_arc()?)?;
                self.consume_birth_stages(&mut path,&stages)?;path
            }
            Some(NativeContinuousGeometryContext::Derivative(certificate)) => {
                certificate.validate_macro(
                    input.macro_index,
                    StepParameters {
                        design: input.design,
                        time_scale: input.time_scale,
                    },
                )?;
                certificate.validate_path(
                    &c.geometry.kinematics,
                    input.start,
                    input.end,
                    input.design,
                )?;
                let law = c.closed_contact_law.as_ref().ok_or_else(|| {
                    CaeError::contract("native derivative geometry lacks its contact owner")
                })?;
                let sample=Arc::new(certificate.sample_child(law,a,b)?);
                let receipt=c.geometry.project_and_issue_closed_child(input.start,input.end,input.design,input.time_scale,sample.clone())?;
                let stages=self.derivative_birth_stages(input.macro_index,input.tick,receipt)?;
                let mut path=crate::native_gas_geometry_adapter::NativeGasGeometryAdapter::assemble_retained_birth_release_path(&c.geometry,input.start,input.end,input.design,input.time_scale,dt,&c.boundaries,sample.clone())?;
                self.consume_birth_stages(&mut path,&stages)?;path
            }
            Some(NativeContinuousGeometryContext::Trial(certificate)) => {
                certificate.validate_macro(
                    input.macro_index,
                    StepParameters {
                        design: input.design,
                        time_scale: input.time_scale,
                    },
                )?;
                certificate.validate_path(
                    &c.geometry.kinematics,
                    input.start,
                    input.end,
                    input.design,
                )?;
                let law = c.closed_contact_law.as_ref().ok_or_else(|| {
                    CaeError::contract("native trial geometry lacks its contact owner")
                })?;
                let sample=Arc::new(certificate.sample_child(law,a,b)?);
                let receipt=c.geometry.project_and_issue_closed_trial_child(input.start,input.end,input.design,input.time_scale,sample.clone())?;
                let stages=self.trial_birth_stages(input.macro_index,input.tick,receipt)?;
                let mut path=crate::native_gas_geometry_adapter::NativeGasGeometryAdapter::assemble_retained_birth_trial_path(&c.geometry,input.start,input.end,input.design,input.time_scale,dt,&c.boundaries,sample.clone())?;
                self.consume_birth_stages(&mut path,&stages)?;path
            }
        };
        path.certify_stationary_continuum_slots(&self.continuum_slots)?;
        c.layout
            .validate(gas, &path.old_geometry.aggregated.volumes_m3)?;
        self.admit_birth_inventory(&path,gas)?;
        let sparse = (0..c.layout.slots())
            .map(|slot| {
                if self.kinetic_mask[slot] && path.old_geometry.aggregated.active_slots[slot] {
                    Ok(Some(c.layout.kinetic_inventory(gas, slot)?.to_vec()))
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
        let mut previous = gas.to_vec();
        for slot in 0..c.layout.slots() {
            if self.kinetic_mask[slot] {
                let range = c.layout.range(slot)?;
                previous[range.clone()].fill(0.);
                if let Some(row) = &remap.coefficient_inventory[slot] {
                    previous[range].copy_from_slice(row);
                }
            }
        }
        let offsets = vec![[0.; 3]; c.connector.exchange().port_count()];
        let donors = lattice_call!(self, donor_coefficients(input.previous, stress, &offsets))?;
        let network = self.prepare_network_for_path(&path, dt)?;
        let system = ContinuousMixedSystem::new(
            network,
            ContinuousMixedState {
                previous_inventory: previous.clone(),
                port_coefficient_density: donors,
                gas_rt_m2_s2: c.gas_rt_m2_s2,
                molecular_viscosity_pa_s: c.viscosity_pa_s,
            },
        )?;
        let system = match &self.transport_preconditioner {
            Some(policy) => {
                let scales = system.build_constrained_graph(&c.solve_policy)?.scales().to_vec();
                system.with_transport_preconditioner(policy.clone(), scales)?
            }
            None => system,
        };
        let solved = if c.use_selected_kkt_newton {
            system.solve_with_selected_kkt_newton(&previous, &c.solve_policy)?
        } else {
            system.solve(&previous, &c.solve_policy)?
        };
        let mut packets = vec![[0.; 10]; c.connector.exchange().port_count()];
        let mut wall_packets = Vec::with_capacity(solved.evaluated.boundary_packets.len());
        for packet in &solved.evaluated.boundary_packets {
            let face = &path.faces[packet.face];
            if let TransientGasBoundary::Port { port } = face.boundary {
                for j in 0..10 {
                    packets[port][j] += packet.moments[j];
                }
            }
            wall_packets.push((
                packet.face,
                TransientGasPacket {
                    mass_kg: packet.moments[0],
                    momentum_kg_m_s: [packet.moments[1], packet.moments[2], packet.moments[3]],
                    kinetic_energy_j: 0.5
                        * (packet.moments[4] + packet.moments[5] + packet.moments[6]),
                    wall_work_j: packet.wall_work_j,
                    wall_heat_j: packet.wall_heat_j,
                },
            ));
        }
        let referenced_wall_load = path.scatter_wall_packets_with_reference(
            &wall_packets,
            &at,
            &bt,
            c.reference_pressure_pa,
        )?;
        let interface_reference = match c.bulk.lattice() {
            LatticeKind::D2Q9 => c
                .connector
                .interface_reference_pressure::<f64, 9, D2Q9>(input.previous)?,
            LatticeKind::D3Q19 => c
                .connector
                .interface_reference_pressure::<f64, 19, D3Q19>(input.previous)?,
            LatticeKind::D3Q27 => c
                .connector
                .interface_reference_pressure::<f64, 27, D3Q27>(input.previous)?,
        };
        let mut reference_faces = vec![[0.; 3]; packets.len()];
        for face in &path.faces {
            if let TransientGasBoundary::Port { port } = face.boundary {
                for k in 0..3 {
                    reference_faces[port][k] +=
                        interface_reference * face.area_m2 * face.normal[k] * dt;
                }
            }
        }
        let projected = match c.bulk.lattice() {
            LatticeKind::D2Q9 => c.connector.reference_moment_projection::<f64, 9, D2Q9>(
                &packets,
                &reference_faces,
                &self.links,
                interface_reference,
            )?,
            LatticeKind::D3Q19 => c.connector.reference_moment_projection::<f64, 19, D3Q19>(
                &packets,
                &reference_faces,
                &self.links,
                interface_reference,
            )?,
            LatticeKind::D3Q27 => c.connector.reference_moment_projection::<f64, 27, D3Q27>(
                &packets,
                &reference_faces,
                &self.links,
                interface_reference,
            )?,
        };
        let reflection = c
            .bulk
            .coupled_tick_reflection(input, &self.links)?
            .into_iter()
            .map(|(cell, j)| (cell, [0., j[0], j[1], j[2], 0., 0., 0., 0., 0., 0.]))
            .collect::<Vec<_>>();
        let rows = c.bulk.coupled_tick_incoming(input, &self.port_cells)?;
        let incoming = lattice_call!(
            self,
            incoming(&rows, &projected.packets, &reflection, &offsets)
        )?;
        let accepted_rates = c.bulk.coupled_tick_pre_collision_rates(
            input,
            &incoming.incoming_rows,
            &self.port_cells,
        )?;
        let maximum_reference_moment_defect = projected
            .reference_moment_defect
            .iter()
            .map(|v| v.abs())
            .fold(0., f64::max);
        Ok(NativeContinuousTick {
            geometry: path,
            system,
            solved,
            incoming,
            accepted_rates,
            referenced_wall_load,
            projected_packets: projected.packets,
            maximum_reference_moment_defect,
        })
    }
    pub fn accepted_port_stage(
        &self,
        tick: &NativeContinuousTick,
        post_bulk: &[f64],
    ) -> CaeResult<NativeContinuousAcceptedPortStage<f64>> {
        let offsets = vec![[0.; 3]; self.config.connector.exchange().port_count()];
        lattice_call!(
            self,
            accepted_stage(
                &tick.incoming.incoming_rows,
                &tick.accepted_rates,
                post_bulk,
                &offsets
            )
        )
    }
    pub fn physical_totals(&self, gas: &[f64]) -> CaeResult<[f64; 5]> {
        let c = &self.config;
        if gas.len() != c.layout.state_size() || gas.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "native continuous totals checkpoint domain",
            ));
        }
        let mut total = [0.; 5];
        for slot in 0..c.layout.slots() {
            let macro_inventory = c.layout.macro_inventory(gas, slot)?;
            for j in 0..4 {
                total[j] += macro_inventory[j];
            }
            let range = c.layout.range(slot)?;
            total[4] += match c.layout.representation(slot)? {
                ContinuousGasRepresentation::Kinetic => {
                    let moments = c.layout.basis().coefficient_moments(&gas[range])?;
                    0.5 * (moments[4] + moments[5] + moments[6])
                }
                ContinuousGasRepresentation::Continuum => {
                    let m = macro_inventory[0];
                    if m <= 0. {
                        return Err(CaeError::contract("continuum totals require positive mass"));
                    }
                    0.5 * macro_inventory[1..].iter().map(|j| j * j / m).sum::<f64>()
                        + 1.5 * m * c.gas_rt_m2_s2
                }
            };
        }
        if total.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "native continuous physical totals overflow",
            ));
        }
        Ok(total)
    }
    pub fn forward(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        self.forward_with_closed_context(n, previous, start, end, p, None)
    }
    pub(crate) fn forward_with_closed_context(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        context: Option<&NativeContinuousGeometryContext>,
    ) -> CaeResult<SubcycleRecord> {
        let c = &self.config;
        if p.time_scale != 1.
            || start.len() != c.bulk.trace_size()
            || end.len() != start.len()
            || p.design.len() != c.bulk.design_size()
            || start
                .iter()
                .chain(end)
                .chain(p.design)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "native continuous macrostep input domain",
            ));
        }
        let old = self.checkpoint.split(previous)?;
        let mut bulk = old.bulk.to_vec();
        let mut gas = old.gas.to_vec();
        let mut stress = old.stress_trace_pa.to_vec();
        let mut session = c.bulk.coupled_macro_session(n, start, end, p.design, 1.)?;
        let ticks = c.bulk.config().substeps;
        let mut force = vec![0.; start.len()];
        let mut work = 0.;
        let mut reference_work = 0.;
        let mut projection_heat = 0.;
        let mut collision_heat = 0.;
        let mut diagnostics = StepDiagnostics::default();
        let mut max_exchange = 0f64;
        let mut max_reference = 0f64;
        let mut max_physical = 0f64;
        let initial_totals = self.physical_totals(&gas)?;
        let mut boundary_totals = [0.; 5];
        let mut gas_bath_heat = 0.;
        let mut continuum_energy_remainder = 0.;
        for tick in 1..=ticks {
            let input = CoupledTickInput {
                macro_index: n,
                tick,
                previous: &bulk,
                start,
                end,
                design: p.design,
                time_scale: 1.,
            };
            let evaluated = self.prepare_tick_with_closed_context(input, &gas, &stress, context)?;
            let post = session.tick_with_incoming(&bulk, &evaluated.incoming.incoming_rows)?;
            let accepted = self.accepted_port_stage(&evaluated, &post)?;
            stress = accepted.stress_trace_pa;
            collision_heat += accepted
                .collision
                .energy_to_thermostat_j
                .iter()
                .sum::<f64>();
            for packet in &evaluated.solved.evaluated.boundary_packets {
                boundary_totals[0] += packet.moments[0];
                for j in 1..4 {
                    boundary_totals[j] += packet.moments[j];
                }
                boundary_totals[4] +=
                    0.5 * (packet.moments[4] + packet.moments[5] + packet.moments[6]);
            }
            gas_bath_heat += evaluated.solved.evaluated.kinetic_collision_heat_j;
            continuum_energy_remainder += evaluated
                .solved
                .evaluated
                .continuum
                .equilibrium_energy_balance_residual_j;
            gas = evaluated.solved.inventory;
            bulk = post;
            for (f, g) in force
                .iter_mut()
                .zip(&evaluated.referenced_wall_load.load.force_flux_n)
            {
                *f += g / ticks as f64;
            }
            work += evaluated.referenced_wall_load.load.wall_work_j;
            reference_work += evaluated.referenced_wall_load.reference_work_j;
            projection_heat += evaluated.incoming.projection_heat_j;
            diagnostics.newton_iterations += evaluated.solved.newton_iterations;
            diagnostics.krylov_iterations += evaluated.solved.linear_iterations;
            diagnostics.residual_norm = diagnostics
                .residual_norm
                .max(evaluated.solved.scaled_kkt_residual_norm);
            max_physical =
                max_physical.max(evaluated.solved.maximum_relative_physical_moment_residual);
            max_exchange = max_exchange.max(
                evaluated
                    .incoming
                    .exchange_moment_defect
                    .iter()
                    .map(|v| v.abs())
                    .fold(0., f64::max),
            );
            max_reference = max_reference.max(evaluated.maximum_reference_moment_defect);
        }
        let final_totals = self.physical_totals(&gas)?;
        let mut native = session.finish(old.bulk, bulk)?;
        native.ledger.insert("gas_mass_kg".into(), final_totals[0]);
        native.ledger.insert(
            "gas_mass_conservation_defect_kg".into(),
            final_totals[0] - initial_totals[0] + boundary_totals[0],
        );
        native
            .ledger
            .insert("gas_collision_bath_energy_J".into(), gas_bath_heat);
        native.ledger.insert(
            "gas_continuum_equilibrium_energy_remainder_J".into(),
            continuum_energy_remainder,
        );
        let equilibrium_energy_remainder =
            final_totals[4] - initial_totals[4] + boundary_totals[4] + gas_bath_heat;
        native.ledger.insert(
            "gas_equilibrium_energy_remainder_J".into(),
            equilibrium_energy_remainder,
        );
        native.ledger.insert(
            "gas_kinetic_energy_conservation_defect_J".into(),
            equilibrium_energy_remainder - continuum_energy_remainder,
        );
        for (j, axis) in ["x", "y", "z"].iter().enumerate() {
            native.ledger.insert(
                format!("gas_momentum_conservation_defect_{axis}_kg_m_s"),
                final_totals[j + 1] - initial_totals[j + 1] + boundary_totals[j + 1],
            );
        }
        for (f, g) in native.flux.iter_mut().zip(force) {
            *f += g;
        }
        native.pairing += work;
        native.state = self.checkpoint.pack(&native.state, &gas, &stress)?;
        native
            .ledger
            .insert("interface_work_J".into(), native.pairing);
        for (name, value) in [
            ("gas_interface_work_J", work),
            ("gas_reference_work_J", reference_work),
            ("gas_bulk_projection_heat_J", projection_heat),
            ("gas_bulk_collision_bath_energy_J", collision_heat),
            ("gas_relative_moment_residual", max_physical),
            ("gas_bulk_maximum_moment_exchange_defect", max_exchange),
            ("gas_reference_maximum_moment_defect", max_reference),
        ] {
            native.ledger.insert(name.into(), value);
        }
        if native
            .ledger
            .values()
            .chain(&native.state)
            .chain(&native.flux)
            .chain([&native.pairing])
            .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "native continuous macrostep result overflow",
            ));
        }
        Ok(SubcycleRecord {
            state: native.state,
            flux: native.flux,
            samples: native.samples,
            pairing: native.pairing,
            ledger: native.ledger,
            diagnostics,
        })
    }
}
