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

use crate::contact_constraint_tangent::ContactTangentPolicy;
use crate::moving_contact::{
    contact_field::ContactField,
    native_surface_binding::NativeNodalBinding,
    separate_body::{MovingFsiPairModel, PairCarrier, PairSolidField},
};
use crate::native_cut_cell_partition::NativeCutCellOwner;
use crate::native_gap_column_owner::native_gas_support_domain;
use crate::native_gas_geometry_adapter::{NativeGasExteriorBoundary, NativeGasGeometryAdapter};
use crate::native_gas_port_coupling::NativeGasPortConnector;
use crate::patch_unilateral_contact::{PatchContactSite, PatchUnilateralLaw};
use crate::reference_traction::ReferenceTractionOwnership;
use crate::surface_gap::{SurfaceGapCell, SurfaceGapKinematics};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    carrier::LagrangianCarrier, carrier_filter::FilteredCarrier, field::AnyMovingLbm,
    port_exchange::PortExchange, pushforward::Frame,
};
use implexity_solve::{multirate_coupling::FluxDrivenField, time_stepper::StepParameters};
use std::{collections::BTreeMap, sync::Arc};

pub struct NativeDryGapSettings {
    pub gap_cells: Vec<SurfaceGapCell>,
    pub paired_vertex_ids: Vec<[usize; 3]>,
    pub contact_sites: Vec<PatchContactSite>,
    pub layers: usize,
    pub residual_slots_per_cell: usize,
    pub populations: usize,
    pub halo_cells: usize,
    pub iso: f64,
    pub geometry_length_tolerance_m: f64,
    pub reference_pressure_pa: f64,
    pub gap_scale_m: f64,
    pub force_scale_n: f64,
    pub initial_residual_tolerance: f64,
    pub contact_tangent: ContactTangentPolicy,
}

pub struct NativeDryGapAssembly<'a> {
    pub contact: ContactField<PairSolidField<'a>, PatchUnilateralLaw>,
    pub geometry: Arc<NativeGasGeometryAdapter>,
    pub bulk: Arc<AnyMovingLbm>,
    pub connector: Arc<NativeGasPortConnector>,
    pub boundaries: BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
    pub initial_contact_state: Vec<f64>,
    pub initial_trace: Vec<f64>,
    pub initial_mechanical_residual: f64,
    pub gas_rt_m2_s2: f64,
    pub viscosity_pa_s: f64,
    pub reference_pressure_pa: f64,
}

pub fn assemble_native_dry_gap<'a>(
    model: &'a MovingFsiPairModel,
    design: &[f64],
    settings: NativeDryGapSettings,
) -> CaeResult<NativeDryGapAssembly<'a>> {
    if settings.gap_cells.len() != settings.paired_vertex_ids.len()
        || settings.gap_cells.is_empty()
        || settings.populations < 4
        || !settings.initial_residual_tolerance.is_finite()
        || settings.initial_residual_tolerance <= 0.
    {
        return Err(CaeError::contract(
            "invalid native dry gap assembly settings",
        ));
    }
    let solid = model.solid_field()?;
    let binding = NativeNodalBinding::new(model, &solid, design)?;
    let kinematics = SurfaceGapKinematics::from_native(&binding, settings.gap_cells)?;
    let trace_kinematics = kinematics.as_trace_kinematics()?;
    let law = PatchUnilateralLaw::new_with_reference_ownership(
        kinematics,
        solid.trace_operator().clone(),
        settings.contact_sites,
        settings.reference_pressure_pa,
        settings.gap_scale_m,
        settings.force_scale_n,
        ReferenceTractionOwnership::GasWettedWalls,
    )?;
    let contact =
        ContactField::new(solid, law)?.with_trace_derivative_extension(settings.contact_tangent)?;
    let initial_contact_state = contact.initial_state(design)?;
    contact
        .law()
        .validate_unique_reference(&initial_contact_state, design)?;
    let initial_trace = contact
        .trace_operator()
        .matvec(&initial_contact_state)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    let residual = contact.residual(
        1,
        &initial_contact_state,
        &initial_contact_state,
        &vec![0.; contact.trace_operator().nrows()],
        StepParameters {
            design,
            time_scale: 1.,
        },
    )?;
    let initial_mechanical_residual = residual.iter().fold(0f64, |s, v| s.hypot(*v));
    if !initial_mechanical_residual.is_finite()
        || initial_mechanical_residual > settings.initial_residual_tolerance
    {
        return Err(CaeError::contract(
            "native dry rest state requires mechanical equilibrium",
        ));
    }
    let mut config = model.native_body(0)?.lbm_config();
    let dt = config.macro_step_s / config.substeps as f64;
    let gas_rt_m2_s2 = settings.reference_pressure_pa / config.density_kg_m3;
    let viscosity_pa_s = config.density_kg_m3 * config.kinematic_viscosity_m2_s;
    let carrier: Arc<dyn LagrangianCarrier> = Arc::new(PairCarrier::new(
        model.native_body(0)?.carrier.clone(),
        model.native_body(1)?.carrier.clone(),
    ));
    let cloud = carrier.points(&initial_trace, design)?;
    let domain = native_gas_support_domain(
        &binding,
        &initial_trace,
        &config,
        &cloud,
        settings.halo_cells,
    )?;
    let owner = NativeCutCellOwner::from_native(
        &binding,
        domain.grid.clone(),
        settings.iso,
        settings.geometry_length_tolerance_m,
    )?;
    let partition = owner.partition(&initial_trace, design)?;
    let requested = domain
        .exterior_faces
        .iter()
        .filter(|f| !f.static_wall)
        .map(|f| (f.cell, f.axis, f.side))
        .collect::<Vec<_>>();
    owner.certify_full_fluid_faces(&partition, &requested)?;
    let mut outside = BTreeMap::new();
    let mut boundaries = BTreeMap::new();
    for face in &domain.exterior_faces {
        let boundary = if face.static_wall {
            NativeGasExteriorBoundary::StationaryWall
        } else {
            let cell = face.outside_cell.ok_or_else(|| {
                CaeError::contract("native dry gap domain reaches an unowned external boundary")
            })?;
            let next = outside.len();
            NativeGasExteriorBoundary::Port {
                port: *outside.entry(cell).or_insert(next),
            }
        };
        boundaries.insert((face.outside_cell, face.axis, face.side), boundary);
    }
    let mut mirrored = [[false; 2]; 3];
    for face in &config.symmetry_faces {
        mirrored[face.axis()][usize::from(face.inward() < 0)] = true;
    }
    let frame = Frame::new(
        config.shape,
        config.spacing_m,
        config.origin_m,
        config.periodic,
        config.kernel_width_cells,
    )?
    .with_mirrors(mirrored)?
    .with_kernel(config.interpolation_kernel);
    let filter = Arc::new(
        FilteredCarrier::new(carrier, (0..cloud.positions.len()).collect())?
            .with_runtime_ownership(
                &initial_trace,
                design,
                frame,
                domain.grid.cells.clone(),
                config.solid_mask.clone(),
            )?,
    );
    for &cell in &domain.grid.cells {
        config.solid_mask[cell] = true;
    }
    let bulk = Arc::new(AnyMovingLbm::new(
        model.native_body(0)?.problem.fluid.lattice,
        config.clone(),
        filter,
    )?);
    for link in bulk.coupled_interface_links(&domain.grid.cells)? {
        let next = outside.len();
        outside.entry(link.cell()).or_insert(next);
    }
    let mut ports = vec![vec![]; outside.len()];
    for (&cell, &port) in &outside {
        ports[port] = vec![(cell, 1.)];
    }
    let exchange = PortExchange::new(
        config.solid_mask.len(),
        domain.grid.cells.clone(),
        ports,
        config.spacing_m,
        config.density_kg_m3,
        dt,
    )?;
    let connector = Arc::new(NativeGasPortConnector::new(
        exchange,
        config.spacing_m,
        config.density_kg_m3,
        dt,
        settings.reference_pressure_pa,
    )?);
    let geometry = Arc::new(NativeGasGeometryAdapter::new(
        owner,
        trace_kinematics,
        settings.paired_vertex_ids,
        settings.layers,
        settings.residual_slots_per_cell,
        settings.populations,
        settings.geometry_length_tolerance_m,
    )?);
    Ok(NativeDryGapAssembly {
        contact,
        geometry,
        bulk,
        connector,
        boundaries,
        initial_contact_state,
        initial_trace,
        initial_mechanical_residual,
        gas_rt_m2_s2,
        viscosity_pa_s,
        reference_pressure_pa: settings.reference_pressure_pa,
    })
}

pub type NativeDryGapStepper<'a> = implexity_solve::multirate_coupling::MultirateStepper<
    crate::native_gas_coupled_field::NativeGasCoupledField,
    ContactField<PairSolidField<'a>, PatchUnilateralLaw>,
>;

pub struct NativeDryGapTransportSettings {
    pub velocity_grid: crate::gas_transient_kinetic::TransientVelocityGrid,
    pub transport_policy: crate::gas_multiscale_network::MultiscaleNetworkPolicy,
    pub nonlinear_policy: implexity_solve::nonlinear_graph::NonlinearGraphPolicy,
    pub mixed: Option<crate::native_gas_coupled_field::NativeGasMixedConfig>,
    pub initial_velocity_m_s: [f64; 3],
}

impl<'a> NativeDryGapAssembly<'a> {
    pub fn into_stepper(
        self,
        transport: NativeDryGapTransportSettings,
        coupling: &crate::problem::CouplingSpec,
        identity: String,
    ) -> CaeResult<NativeDryGapStepper<'a>> {
        let density = self.bulk.config().density_kg_m3;
        let field = crate::native_gas_coupled_field::NativeGasCoupledField::new(
            crate::native_gas_coupled_field::NativeGasCoupledFieldConfig {
                bulk: self.bulk,
                geometry: self.geometry,
                connector: self.connector,
                boundaries: self.boundaries,
                velocity_grid: transport.velocity_grid,
                transport_policy: transport.transport_policy,
                nonlinear_policy: transport.nonlinear_policy,
                mixed: transport.mixed,
                initial_trace: self.initial_trace,
                initial_density_kg_m3: density,
                initial_velocity_m_s: transport.initial_velocity_m_s,
                gas_rt_m2_s2: self.gas_rt_m2_s2,
                viscosity_pa_s: self.viscosity_pa_s,
                reference_pressure_pa: self.reference_pressure_pa,
            },
        )?;
        implexity_solve::multirate_coupling::MultirateStepper::new(
            field,
            self.contact,
            implexity_solve::multirate_coupling::MultirateOptions {
                mode: coupling.mode.clone(),
                schur_ratio_limit: coupling.schur_ratio_limit,
                work_defect_limit: coupling.work_defect_limit,
            },
        )?
        .with_identity(identity)
        .with_initial_prediction(coupling.initial_prediction)?
        .with_field_newton(coupling.field_newton)?
        .with_linear_solves(coupling.linear_solves)
        .map(|stepper| stepper.with_step_cache_bytes(coupling.step_cache_bytes))
    }
}
