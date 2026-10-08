// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, AuthoringRequirement, ContractInput, DesignCoordinateRef,
    ExecutionKind, Fidelity, PortSpec, ResponseCapability, RuntimeRoute,
};
use implexity_core::packages::InstallContext;
use implexity_core::py_repr::repr_str;

use crate::array::Tensor;
use crate::contracts::{AddinContract as LocalContract, PortSpec as LocalPort};
use crate::model_errors::{PhysicsError, PhysicsResult};
use crate::models::{self, ModelOutputs, RuntimeModel, construct};
use crate::registry::ensure_default_addins;
use crate::runtime::{enforce_runtime_validity, output_margins};

pub const ALGEBRAIC_ADAPTER_IMPLEMENTATION: &str =
    "implexity.physics_library.core_bridge.PhysicsLibraryAlgebraicAdapter";
pub const STRICT_COMPONENT_IMPLEMENTATION: &str =
    "implexity.physics_library.core_bridge.StrictComponentAdapter";
pub const ALGEBRAIC_INTERFACE: &str = "physics_library.algebraic";


pub trait PhysicsComponent: Send + Sync + 'static {
    fn implementation(&self) -> String;
    fn component_kind(&self) -> Option<String> {
        None
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        None
    }
    fn component_slots(&self) -> Option<Map<String, Value>> {
        None
    }
    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        None
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        None
    }
    fn state_contract(&self) -> Option<String> {
        None
    }
    fn as_any(&self) -> &dyn Any;
}

pub struct StrictComponentAdapter {
    pub delegate: Arc<dyn PhysicsComponent>,
    pub output_port_id: String,
    pub registration_identity: String,
}

impl StrictComponentAdapter {
    #[must_use]
    pub fn resolve_component(&self) -> BTreeMap<String, Arc<dyn PhysicsComponent>> {
        BTreeMap::from([(self.output_port_id.clone(), Arc::clone(&self.delegate))])
    }
}

impl AddInAdapter for StrictComponentAdapter {
    fn implementation(&self) -> String {
        STRICT_COMPONENT_IMPLEMENTATION.into()
    }
    fn registration_identity(&self) -> Option<String> {
        Some(self.registration_identity.clone())
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        self.delegate.runtime_support()
    }
    fn component_kind(&self) -> Option<String> {
        self.delegate.component_kind()
    }
    fn component_slots(&self) -> Option<Map<String, Value>> {
        self.delegate.component_slots()
    }
    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        self.delegate.authoring_contract()
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        self.delegate.response_units()
    }
    fn state_contract(&self) -> Option<String> {
        self.delegate.state_contract()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone)]
pub struct ComponentOptions {
    pub category: AddInCategory,
    pub domain: String,
    pub fidelity: Fidelity,
    pub notes: Vec<String>,
}

impl Default for ComponentOptions {
    fn default() -> Self {
        Self {
            category: AddInCategory::Constitutive,
            domain: "runtime:component".into(),
            fidelity: Fidelity::Intermediate,
            notes: Vec::new(),
        }
    }
}

#[must_use]
pub fn component_contract(
    addin_id: &str,
    quantity: &str,
    owner: &str,
    options: &ComponentOptions,
) -> AddInContract {
    let port_id = format!("{addin_id}.component");
    let mut port = PortSpec::new(quantity);
    port.unit = "-".into();
    port.domain.clone_from(&options.domain);
    port.temporal = "steady".into();
    port.port_id = port_id;
    port.cardinality = "singleton".into();
    let mut c = AddInContract::new(addin_id);
    c.category = options.category;
    c.provides = vec![port];
    c.fidelity = options.fidelity;
    c.priority = 10;
    c.exact_design_derivatives = Some(true);
    c.exact_state_transpose = Some(true);
    c.notes.clone_from(&options.notes);
    c.notes.push(
        "Component dependency-injection contract; numerical execution is owned by a selected field provider."
            .into(),
    );
    c.contract_version = 2;
    c.owner_id = owner.into();
    c.execution_kind = Some(ExecutionKind::Operation);
    c.supported_operations = vec!["resolve_component".into()];
    c
}


pub fn register_strict_component(
    ctx: &InstallContext<'_>,
    addin_id: &str,
    component: Arc<dyn PhysicsComponent>,
    quantity: &str,
    options: &ComponentOptions,
) -> Result<AddInContract, CaeError> {
    let owner = ctx.owner_id().to_string();
    let contract = component_contract(addin_id, quantity, &owner, options);
    let wrapper = StrictComponentAdapter {
        delegate: component,
        output_port_id: format!("{addin_id}.component"),
        registration_identity: owner,
    };
    ctx.register_addin(ContractInput::Typed(Box::new(contract)), Some(Arc::new(wrapper)))
}


pub fn registered_component(addin_id: &str) -> Result<Arc<dyn PhysicsComponent>, CaeError> {
    let row = implexity_core::registries::global().addins.get(addin_id)?;
    let adapter =
        row.adapter.as_ref().and_then(|a| a.as_any().downcast_ref::<StrictComponentAdapter>()).ok_or_else(
            || CaeError::contract(format!("{} is not a registered field component", repr_str(addin_id))),
        )?;
    Ok(Arc::clone(&adapter.delegate))
}

fn category_of(family: &str) -> AddInCategory {
    match family {
        "interface" => AddInCategory::Interface,
        "process" => AddInCategory::Process,
        "evolution" | "constitutive_evolution" => AddInCategory::Evolution,
        "network_system" => AddInCategory::Network,
        "field_mechanics"
        | "field_poromechanics"
        | "field_multiphase"
        | "field_radiation"
        | "field_electromagnetics"
        | "field_electrochemistry"
        | "field_thermal" => AddInCategory::Field,
        _ => AddInCategory::Constitutive,
    }
}

#[must_use]
#[allow(clippy::match_same_arms)]
pub fn response_reduction(addin_id: &str, response: &str) -> Option<&'static str> {
    let id = match addin_id {
        "viscosity_coupled_biot_poromechanics" => "biot_poromechanics",
        "field_temperature_fluid_viscosity" => "temperature_dependent_fluid_viscosity",
        other => other,
    };
    let q = match (id, response) {
        ("enthalpy_storage_increment", "enthalpy_storage") => "thermal_storage_rate",
        ("plastic_dissipation_heat_conversion", "plastic_heat_generation") => "volumetric_heat_source",
        ("solid_material_temperature", "temperature_dependent_material") => "youngs_modulus",
        ("external_convection_interface", "convective_heat_loss") => "boundary_heat_flux",
        ("temperature_dependent_fluid_viscosity", "dynamic_viscosity") => "dynamic_viscosity",
        ("pressure_structure_transfer", "pressure_traction") => "surface_traction",
        ("regularised_contact_friction", "contact_pressure") => "contact_pressure",
        ("regularised_contact_friction", "frictional_heat" | "frictional_dissipation") => {
            "boundary_heat_flux"
        }
        ("species_reaction_network", "reaction_rate") => "reaction_rate",
        ("species_reaction_network", "reaction_heat") => "volumetric_heat_source",
        ("species_reaction_network", "species_source") => "species_source",
        ("butler_volmer_interface", "current_density") => "current_density",
        ("butler_volmer_interface", "electrochemical_heat") => "boundary_heat_flux",
        ("phase_change_enthalpy", "phase_fraction") => "phase_fraction",
        ("phase_change_enthalpy", "enthalpy") => "specific_enthalpy",
        ("vibroacoustic_interface", "interface_power_residual") => "interface_power_residual",
        ("normalised_moving_heat_source", "integrated_absorbed_power") => "integrated_power",
        ("finite_strain_neo_hookean", "strain_energy") => "strain_energy_density",
        ("finite_strain_neo_hookean", "stress") => "cauchy_stress",
        ("biot_poromechanics", "effective_stress") => "effective_stress",
        ("biot_poromechanics", "fluid_content") => "fluid_content",
        ("biot_poromechanics", "darcy_flux") => "darcy_flux",
        ("biot_poromechanics", "poroelastic_storage") => "stored_energy_density",
        ("biot_poromechanics", "darcy_dissipation") => "volumetric_heat_source",
        ("cahn_hilliard_phase_field", "chemical_potential") => "chemical_potential",
        ("cahn_hilliard_phase_field", "interface_energy") => "interface_energy_density",
        ("gray_surface_radiation", "radiative_heat_flux" | "radiation_exchange") => "boundary_heat_flux",
        ("eddy_current_induction", "joule_heating") => "volumetric_heat_source",
        ("eddy_current_induction", "current_density") => "current_density",
        ("eddy_current_induction", "lorentz_force") => "body_force_density",
        ("moving_conductor_electromagnetics", "moving_conductor_joule_heat") => "volumetric_heat_source",
        ("moving_conductor_electromagnetics", "moving_conductor_lorentz_force") => "body_force_density",
        ("moving_conductor_electromagnetics", "electromagnetic_mechanical_power") => {
            "mechanical_power_density"
        }
        ("nernst_planck_ionic_transport", "ionic_flux") => "species_flux",
        ("nernst_planck_ionic_transport", "ionic_current") => "current_density",
        ("archard_wear_evolution", "wear_depth" | "material_loss") => "geometry_recession",
        ("hydraulic_network", "pressure_drop") => "pressure_drop",
        ("hydraulic_network", "pump_power") => "pump_power",
        ("orthotropic_heat_conduction", "heat_flux") => "heat_flux",
        ("isotropic_heat_conduction", "conductive_heat_flux") => "heat_flux",
        ("isotropic_small_strain_elasticity", "linear_elastic_energy") => "strain_energy_density",
        ("isotropic_small_strain_elasticity", "linear_elastic_stress") => "stress",
        ("inertial_body_force", "inertial_load") => "inertial_body_force_density",
        ("thermal_storage_rate", "thermal_storage") => "thermal_storage_rate",
        _ => return None,
    };
    Some(q)
}

#[must_use]
#[allow(clippy::match_same_arms)]
pub fn response_dependencies(addin_id: &str, response: &str) -> &'static [&'static str] {
    let id = match addin_id {
        "viscosity_coupled_biot_poromechanics" => "biot_poromechanics",
        "field_temperature_fluid_viscosity" => "temperature_dependent_fluid_viscosity",
        other => other,
    };
    match (id, response) {
        ("enthalpy_storage_increment", "enthalpy_storage") => {
            &["specific_enthalpy", "previous_specific_enthalpy", "reference_mass_density", "time_increment"]
        }
        ("solid_material_temperature", "temperature_dependent_material") => {
            &["temperature", "topology_density"]
        }
        ("external_convection_interface", "convective_heat_loss") => {
            &["surface_temperature", "ambient_temperature"]
        }
        ("temperature_dependent_fluid_viscosity", "dynamic_viscosity") => &["fluid_temperature"],
        ("pressure_structure_transfer", "pressure_traction") => &["surface_pressure", "surface_normal"],
        ("regularised_contact_friction", "contact_pressure") => &["gap"],
        ("regularised_contact_friction", "frictional_heat" | "frictional_dissipation") => {
            &["gap", "slip_velocity"]
        }
        ("plastic_dissipation_heat_conversion", "plastic_heat_generation") => {
            &["dissipation_energy_density", "time_increment"]
        }
        ("species_reaction_network", "species_source" | "reaction_rate" | "reaction_heat") => {
            &["species_concentration", "temperature"]
        }
        ("butler_volmer_interface", "current_density" | "electrochemical_heat") => {
            &["overpotential", "temperature", "species_concentration", "activity_ratio"]
        }
        ("phase_change_enthalpy", "phase_fraction" | "enthalpy") => &["temperature"],
        ("vibroacoustic_interface", "interface_power_residual") => {
            &["structural_velocity", "acoustic_pressure"]
        }
        ("normalised_moving_heat_source", "integrated_absorbed_power") => {
            &["mesh_geometry", "source_position", "cell_volumes"]
        }
        ("finite_strain_neo_hookean", "strain_energy" | "stress") => {
            &["deformation_gradient", "topology_density"]
        }
        ("biot_poromechanics", "effective_stress" | "fluid_content" | "poroelastic_storage") => {
            &["volumetric_strain", "pore_pressure"]
        }
        ("biot_poromechanics", "darcy_flux" | "darcy_dissipation") => {
            &["pressure_gradient", "permeability", "dynamic_viscosity"]
        }
        ("cahn_hilliard_phase_field", "chemical_potential") => &["phase_fraction", "laplacian_phase"],
        ("cahn_hilliard_phase_field", "interface_energy") => &["phase_fraction", "phase_gradient"],
        ("gray_surface_radiation", "radiative_heat_flux" | "radiation_exchange") => {
            &["surface_temperature", "radiation_temperature"]
        }
        ("eddy_current_induction", "joule_heating" | "current_density") => {
            &["electric_field", "electrical_conductivity"]
        }
        ("eddy_current_induction", "lorentz_force") => {
            &["electric_field", "magnetic_flux_density", "electrical_conductivity"]
        }
        (
            "moving_conductor_electromagnetics",
            "moving_conductor_joule_heat"
            | "moving_conductor_lorentz_force"
            | "electromagnetic_mechanical_power",
        ) => &["electric_field", "magnetic_flux_density", "electrical_conductivity", "material_velocity"],
        ("nernst_planck_ionic_transport", "ionic_flux" | "ionic_current") => &[
            "species_concentration",
            "species_concentration_gradient",
            "electric_field",
            "temperature",
            "ionic_diffusivity",
        ],
        ("archard_wear_evolution", "wear_depth" | "material_loss") => {
            &["contact_pressure", "slip_velocity", "time_increment"]
        }
        ("hydraulic_network", "pressure_drop" | "pump_power") => {
            &["mass_flow_rate", "mass_density", "dynamic_viscosity", "hydraulic_area", "hydraulic_diameter"]
        }
        ("orthotropic_heat_conduction", "heat_flux") => &["temperature_gradient", "topology_density"],
        ("isotropic_heat_conduction", "conductive_heat_flux") => {
            &["temperature_gradient", "thermal_conductivity"]
        }
        ("isotropic_small_strain_elasticity", "linear_elastic_energy" | "linear_elastic_stress") => {
            &["strain", "youngs_modulus"]
        }
        ("inertial_body_force", "inertial_load") => &["mass_density", "acceleration"],
        ("thermal_storage_rate", "thermal_storage") => &["volumetric_heat_capacity", "temperature_rate"],
        _ => &[],
    }
}

pub const STATEFUL_UNSUPPORTED: [&str; 2] =
    ["regularised_j2_plastic_damage", "generalized_maxwell_viscoelasticity"];

#[must_use]
pub fn integrated_response(response: &str) -> Option<(&'static str, &'static str)> {
    Some(match response {
        "enthalpy_storage"
        | "plastic_heat_generation"
        | "reaction_heat"
        | "joule_heating"
        | "thermal_storage" => ("m^3", "W"),
        "convective_heat_loss"
        | "frictional_heat"
        | "frictional_dissipation"
        | "electrochemical_heat"
        | "radiation_exchange" => ("m^2", "W"),
        "strain_energy" | "linear_elastic_energy" | "interface_energy" => ("m^3", "J"),
        "material_loss" => ("m^2", "m^3"),
        _ => return None,
    })
}

fn input_port_id(addin_id: &str, port: &LocalPort) -> String {
    format!("{addin_id}.input.{}", port.name)
}

fn output_port_id(addin_id: &str, port: &LocalPort) -> String {
    format!("{addin_id}.output.{}", port.name)
}

fn core_port(p: &LocalPort, port_id: String) -> PortSpec {
    let mut port = PortSpec::new(p.quantity.clone());
    port.unit.clone_from(&p.units);
    port.domain.clone_from(&p.support);
    port.temporal = if p.support == "history" { "history" } else { "instantaneous" }.into();
    port.conserved = ["mass", "energy", "charge", "species"].iter().any(|t| p.quantity.contains(t));
    port.port_id = port_id;
    port.aggregation = match p.aggregation.as_str() {
        "sum" => "sum",
        "minimum" => "minimum",
        "maximum" => "maximum",
        "stack" => "concatenate",
        _ => "single",
    }
    .into();
    port
}


#[allow(clippy::too_many_lines)]
pub fn convert_contract(local: &LocalContract) -> Result<AddInContract, CaeError> {
    let coordinates = &local.design_coordinates;
    let unique: BTreeSet<&String> = coordinates.iter().collect();
    if coordinates.is_empty()
        || unique.len() != coordinates.len()
        || coordinates.iter().any(|c| c.trim().is_empty())
    {
        return Err(CaeError::contract(format!(
            "{}: application package must declare unique non-empty design-coordinate IDs",
            local.addin_id
        )));
    }
    let id = &local.addin_id;
    let fidelity = match local.fidelity.as_str() {
        "screening" => Fidelity::Screening,
        "high" | "verification" => Fidelity::High,
        "production" => Fidelity::Qualification,
        _ => Fidelity::Intermediate,
    };
    let provides: Vec<PortSpec> =
        local.produces.iter().map(|p| core_port(p, output_port_id(id, p))).collect();
    let required_local: Vec<&LocalPort> = local.consumes.iter().filter(|p| p.required).collect();
    let consumes: Vec<PortSpec> = required_local.iter().map(|p| core_port(p, input_port_id(id, p))).collect();
    let direct: Vec<&PortSpec> = required_local
        .iter()
        .zip(&consumes)
        .filter(|(l, _)| l.quantity == "topology_density")
        .map(|(_, c)| c)
        .collect();
    if !direct.is_empty() && coordinates.len() != 1 {
        return Err(CaeError::contract(format!(
            "{id}: one physical design port cannot be bound ambiguously to {} coordinates",
            coordinates.len()
        )));
    }
    let design_inputs: Vec<DesignCoordinateRef> = direct
        .iter()
        .map(|c| DesignCoordinateRef {
            coordinate: coordinates[0].clone(),
            addin_id: id.clone(),
            port_id: c.port_id.clone(),
        })
        .collect();
    let mut dependency_ids: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for (l, c) in required_local.iter().zip(&consumes) {
        let entry = dependency_ids.entry(l.quantity.as_str()).or_insert_with(|| Some(c.port_id.clone()));
        if entry.as_deref() != Some(c.port_id.as_str()) {
            *entry = None;
        }
    }
    let executable = !STATEFUL_UNSUPPORTED.contains(&id.as_str());
    let produced: BTreeSet<&str> = local.produces.iter().map(|p| p.quantity.as_str()).collect();
    let required_names: BTreeSet<&str> = required_local.iter().map(|p| p.quantity.as_str()).collect();
    let mut responses = Vec::new();
    for alias in &local.objective_aliases {
        if !executable {
            continue;
        }
        let Some(target) = response_reduction(id, alias) else { continue };
        if !produced.contains(target) {
            continue;
        }
        let deps = response_dependencies(id, alias);
        if deps.is_empty() {
            continue;
        }
        let deps: Vec<&str> = deps.iter().copied().filter(|d| required_names.contains(d)).collect();
        if deps.is_empty() {
            continue;
        }
        let resolved: Vec<String> = deps
            .iter()
            .map(|d| dependency_ids.get(d).and_then(Clone::clone).unwrap_or_else(|| (*d).to_string()))
            .collect();
        let units: BTreeSet<&str> =
            local.produces.iter().filter(|p| p.quantity == target).map(|p| p.units.as_str()).collect();
        let integrated = integrated_response(alias);
        let unit = match integrated {
            Some((_, u)) => u.to_string(),
            None if units.len() == 1 => units.iter().next().map(|u| (*u).to_string()).unwrap_or_default(),
            None => "-".into(),
        };
        let description = match integrated {
            Some((m, _)) => format!(
                "Fixed-geometry quadrature integral; requires response_measures.{alias} with positive weights in {m}."
            ),
            None => "Local sampled statistic or norm, not a spatially integrated objective.".into(),
        };
        let mut cap = ResponseCapability::new(alias.clone());
        cap.unit = unit;
        cap.description = description;
        cap.differentiable = Some(true);
        cap.depends_on = resolved;
        cap.design_reachable = Some(true);
        responses.push(cap);
    }
    let mut authoring: Vec<AuthoringRequirement> = local
        .authoring
        .iter()
        .map(|a| AuthoringRequirement {
            key: format!("addins.{id}.{}", a.name),
            description: a.description.clone(),
            optional: !a.required || !a.default.is_null(),
        })
        .collect();
    let mut notes = local.validity_notes.clone();
    notes.push(format!("physics-library family={}", local.family));
    let integrals: Vec<&String> =
        local.objective_aliases.iter().filter(|a| integrated_response(a).is_some()).collect();
    if !integrals.is_empty() {
        notes.push("Integrated responses require authored response_measures per response: positive shape-matched weights, SI unit m^2 or m^3, fixed_geometry=true. No unweighted density sum or moving-quadrature derivative is implied.".into());
        if id == "finite_strain_neo_hookean" {
            notes
                .push("Strain-energy quadrature weights are reference volumes, not deformed volumes.".into());
        }
        if id == "archard_wear_evolution" {
            notes.push("Material-loss volume integrates recession over fixed surface area; evolving curved-surface measures are not included.".into());
        }
        authoring.push(AuthoringRequirement {
            key: format!("addins.{id}.response_measures"),
            description: format!(
                "Required for integrated response aliases: {}",
                integrals.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ),
            optional: true,
        });
    }
    notes.push("Other local response reductions are sampled scalar statistics/norms, not spatial integrals or evidence of a solved field coupling.".into());
    let mut c = AddInContract::new(id.clone());
    c.category = category_of(&local.family);
    c.provides = provides;
    c.consumes = consumes;
    c.responses = responses;
    c.authoring = authoring;
    c.fidelity = fidelity;
    c.priority = 10;
    c.runtime_route = RuntimeRoute::Composite;
    c.exact_design_derivatives = Some(local.exact_partial_derivatives && local.exact_coupled_derivatives);
    c.exact_state_transpose = Some(local.exact_coupled_derivatives);
    c.notes = notes;
    c.contract_version = 2;
    c.owner_id = format!("physics-library:{id}");
    c.execution_kind = Some(ExecutionKind::Algebraic);
    c.supported_operations = vec!["evaluate".into(), "sensitivity".into()];
    c.design_inputs = design_inputs;
    Ok(c)
}

#[must_use]
pub fn authoring_namespace(context: &Value, addin_id: &str) -> Map<String, Value> {
    for key in ["authoring", "provider_authoring"] {
        let Some(root) = context.get(key).and_then(Value::as_object) else { continue };
        if let Some(Value::Object(m)) =
            root.get("addins").and_then(|a| a.as_object()).and_then(|a| a.get(addin_id))
        {
            return m.clone();
        }
        if let Some(Value::Object(m)) = root.get(addin_id) {
            return m.clone();
        }
    }
    Map::new()
}

fn reduce<S: Scalar>(x: &Tensor<S>, mode: &str) -> PhysicsResult<S> {
    if x.ndim() == 0 {
        return Ok(x.at(0));
    }
    Ok(match mode {
        "max" => x.max()?,
        "max_abs" => x.map(Scalar::abs).max()?,
        "l2" => x.map(|v| v * v).sum().sqrt(),
        "sum" => x.sum(),
        _ => x.mean(),
    })
}

#[derive(Debug, Clone)]
pub struct PhysicsLibraryAlgebraicAdapter {
    pub contract: LocalContract,
}

impl AddInAdapter for PhysicsLibraryAlgebraicAdapter {
    fn implementation(&self) -> String {
        ALGEBRAIC_ADAPTER_IMPLEMENTATION.into()
    }
    fn has_algebraic_evaluate(&self) -> bool {
        true
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(self.runtime_support_map())
    }
    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        (name == ALGEBRAIC_INTERFACE).then_some(self as &(dyn Any + Send + Sync))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

type Inputs<S> = BTreeMap<String, Tensor<S>>;

pub type PortJacobians = BTreeMap<String, Vec<f64>>;

fn input<'a, S: Scalar>(i: &'a Inputs<S>, q: &str) -> PhysicsResult<&'a Tensor<S>> {
    i.get(q).ok_or_else(|| PhysicsError::Key(repr_str(q)))
}

impl PhysicsLibraryAlgebraicAdapter {
    #[must_use]
    pub fn new(contract: LocalContract) -> Self {
        Self { contract }
    }

    fn runtime_support_map(&self) -> Map<String, Value> {
        let v = if STATEFUL_UNSUPPORTED.contains(&self.contract.addin_id.as_str()) {
            json!({
                "status": "blocked_history_adapter", "history": true,
                "limitations": ["Local constitutive implementation exists, but its native history adapter is not implemented."],
                "physical_validation_inferred": false,
            })
        } else {
            json!({
                "status": "algebraic_adapter", "history": self.contract.path_dependent,
                "limitations": self.contract.validity_notes, "physical_validation_inferred": false,
            })
        };
        v.as_object().cloned().unwrap_or_default()
    }

    #[must_use]
    pub fn nonnegative_validity_margins(&self) -> &'static [&'static str] {
        match self.contract.addin_id.as_str() {
            "plastic_dissipation_heat_conversion" => &["dissipation_margin"],
            "species_reaction_network" | "nernst_planck_ionic_transport" => &["concentration_margin"],
            "biot_poromechanics" | "viscosity_coupled_biot_poromechanics" => &["permeability_margin"],
            "eddy_current_induction" | "moving_conductor_electromagnetics" => &["conductivity_margin"],
            _ => &[],
        }
    }

    fn authoring(&self, context: &Value) -> PhysicsResult<Map<String, Value>> {
        let c = &self.contract;
        if c.runtime_factory.as_deref().is_none_or(str::is_empty) {
            return Err(PhysicsError::Value(format!("{}: no runtime factory", c.addin_id)));
        }
        let mut auth = authoring_namespace(context, &c.addin_id);
        for field in &c.authoring {
            if !auth.contains_key(&field.name) && !field.default.is_null() {
                auth.insert(field.name.clone(), field.default.clone());
            }
        }
        let mut validated = auth.clone();
        if c.objective_aliases.iter().any(|a| integrated_response(a).is_some()) {
            validated.shift_remove("response_measures");
        }
        let issues: Vec<String> = c
            .validate_authoring(Some(&Value::Object(validated)))
            .into_iter()
            .filter(|i| i.blocking)
            .map(|i| i.message)
            .collect();
        if !issues.is_empty() {
            return Err(PhysicsError::contract(issues.join("; ")));
        }
        Ok(auth)
    }

    fn model<M: RuntimeModel>(&self, context: &Value) -> PhysicsResult<M> {
        construct::<M>(&self.contract.addin_id, &self.authoring(context)?)
    }


    #[allow(clippy::too_many_lines)]
    pub fn algebraic_evaluate<S: Scalar>(
        &self,
        i: &Inputs<S>,
        context: &Value,
    ) -> PhysicsResult<ModelOutputs<S>> {
        use models::{
            ArchardWearEvolution, BiotPoromechanics, ButlerVolmerInterface, CahnHilliardPhaseField,
            DensityHeatCapacity, DissipationHeatConversion, EddyCurrentInduction, EnthalpyStorageIncrement,
            ExternalConvection, FiniteStrainNeoHookean, GraySurfaceRadiation, HydraulicNetworkSegment,
            InertialBodyForce, IsotropicHeatConduction, IsotropicSmallStrainElasticity, ModelOutputs,
            NernstPlanckTransport, NormalisedMovingHeatSource, OrthotropicHeatConduction,
            PhaseChangeEnthalpy, PressureStructureTransfer, ReactionNetwork, RegularisedContactFriction,
            TemperatureDependentSolidMaterial, TemperatureDependentViscosity, ThermalStorageRate,
            TopologyHydraulicGeometry, TopologyScalarInterpolation, VibroAcousticInterface,
        };
        let aid = self.contract.addin_id.as_str();
        if STATEFUL_UNSUPPORTED.contains(&aid) {
            return Err(PhysicsError::Value(format!(
                "{aid}: path-dependent state requires a residual/history adapter; algebraic execution is intentionally disabled"
            )));
        }
        let q = |name: &str| input(i, name);
        match aid {
            "solid_material_temperature" => {
                let m: TemperatureDependentSolidMaterial = self.model(context)?;
                let t = i.get("temperature").cloned().unwrap_or_else(|| Tensor::scalar(S::from_f64(m.t_ref)));
                m.properties(&t, q("topology_density")?)
            }
            "external_convection_interface" => {
                let m: ExternalConvection = self.model(context)?;
                let amb = q("ambient_temperature")?.item()?.value();
                #[allow(clippy::float_cmp)]
                if m.ambient_temperature != amb {
                    return Err(PhysicsError::value(
                        "external_convection_interface: authored and coupled ambient temperature disagree",
                    ));
                }
                Ok(ModelOutputs::new()
                    .with("boundary_heat_flux", m.heat_flux_out(q("surface_temperature")?)?))
            }
            "temperature_dependent_fluid_viscosity" | "field_temperature_fluid_viscosity" => {
                let m: TemperatureDependentViscosity = self.model(context)?;
                let t = i
                    .get("fluid_temperature")
                    .cloned()
                    .unwrap_or_else(|| Tensor::scalar(S::from_f64(m.t_ref)));
                m.viscosity(&t)
            }
            "pressure_structure_transfer" => {
                let m: PressureStructureTransfer = self.model(context)?;
                let (p, n) = (q("surface_pressure")?, q("surface_normal")?);
                let mut out = ModelOutputs::new().with("surface_traction", m.transfer(p, n)?);
                out.extend(m.validity(p, n)?);
                Ok(out)
            }
            "regularised_contact_friction" => {
                let m: RegularisedContactFriction = self.model(context)?;
                let r = m.response(q("gap")?, q("slip_velocity")?)?;
                let ha = r.require("frictional_heat_a")?.clone();
                let hb = r.require("frictional_heat_b")?.clone();
                Ok(ModelOutputs::new()
                    .with("contact_pressure", r.require("contact_pressure")?.clone())
                    .with("tangential_traction", r.require("tangential_traction")?.clone())
                    .with("boundary_heat_flux", ha.add(&hb)?)
                    .with("heat_a", ha)
                    .with("heat_b", hb)
                    .with("frictional_dissipation", r.require("frictional_dissipation")?.clone()))
            }
            "plastic_dissipation_heat_conversion" => {
                let m: DissipationHeatConversion = self.model(context)?;
                m.heat_rate(q("dissipation_energy_density")?, q("time_increment")?)
            }
            "species_reaction_network" => {
                let m: ReactionNetwork = self.model(context)?;
                m.rates(q("species_concentration")?, q("temperature")?)
            }
            "butler_volmer_interface" => {
                let m: ButlerVolmerInterface = self.model(context)?;
                let ratio = i.get("activity_ratio").cloned().unwrap_or_else(|| Tensor::scalar(S::one()));
                let mut r = m.response(q("overpotential")?, q("temperature")?, &ratio)?;
                let heat = r.require("reaction_heat")?.clone();
                let flux = r.require("current_density")?.map(|x| x / (m.electrons * 96_485.332_12));
                r.set("boundary_heat_flux", heat);
                r.set("species_flux", flux);
                Ok(r)
            }
            "phase_change_enthalpy" => {
                let m: PhaseChangeEnthalpy = self.model(context)?;
                let mut r = m.response(q("temperature")?)?;
                let cp = r.require("effective_heat_capacity")?.clone();
                r.set("heat_capacity", cp);
                Ok(r)
            }
            "vibroacoustic_interface" => {
                let m: VibroAcousticInterface = self.model(context)?;
                m.response(q("structural_velocity")?, q("acoustic_pressure")?)
            }
            "topology_electrical_conductivity"
            | "topology_ionic_diffusivity"
            | "topology_permeability"
            | "topology_thermal_conductivity"
            | "topology_youngs_modulus"
            | "topology_mass_density"
            | "topology_volumetric_heat_capacity" => {
                let name = match aid {
                    "topology_electrical_conductivity" => "electrical_conductivity",
                    "topology_ionic_diffusivity" => "ionic_diffusivity",
                    "topology_permeability" => "permeability",
                    "topology_thermal_conductivity" => "thermal_conductivity",
                    "topology_youngs_modulus" => "youngs_modulus",
                    "topology_mass_density" => "mass_density",
                    _ => "volumetric_heat_capacity",
                };
                let m: TopologyScalarInterpolation = self.model(context)?;
                m.response(q("topology_density")?, name)
            }
            "isotropic_heat_conduction" => {
                let m: IsotropicHeatConduction = self.model(context)?;
                m.response(q("temperature_gradient")?, q("thermal_conductivity")?)
            }
            "isotropic_small_strain_elasticity" => {
                let m: IsotropicSmallStrainElasticity = self.model(context)?;
                m.response(q("strain")?, q("youngs_modulus")?)
            }
            "inertial_body_force" => {
                let m: InertialBodyForce = self.model(context)?;
                m.response(q("mass_density")?, q("acceleration")?)
            }
            "thermal_storage_rate" => {
                let m: ThermalStorageRate = self.model(context)?;
                m.response(q("volumetric_heat_capacity")?, q("temperature_rate")?)
            }
            "density_heat_capacity" => {
                let m: DensityHeatCapacity = self.model(context)?;
                m.response(q("mass_density")?, q("heat_capacity")?)
            }
            "enthalpy_storage_increment" => {
                let m: EnthalpyStorageIncrement = self.model(context)?;
                m.response(
                    q("specific_enthalpy")?,
                    q("previous_specific_enthalpy")?,
                    q("reference_mass_density")?,
                    q("time_increment")?,
                )
            }
            "normalised_moving_heat_source" => {
                let m: NormalisedMovingHeatSource = self.model(context)?;
                m.volumetric_source(q("mesh_geometry")?, q("source_position")?, q("cell_volumes")?)
            }
            "finite_strain_neo_hookean" => {
                let m: FiniteStrainNeoHookean = self.model(context)?;
                m.response(q("deformation_gradient")?, q("topology_density")?)
            }
            "biot_poromechanics" | "viscosity_coupled_biot_poromechanics" => {
                let m: BiotPoromechanics = self.model(context)?;
                m.response(
                    q("volumetric_strain")?,
                    q("pore_pressure")?,
                    q("pressure_gradient")?,
                    q("permeability")?,
                    i.get("dynamic_viscosity"),
                )
            }
            "cahn_hilliard_phase_field" => {
                let m: CahnHilliardPhaseField = self.model(context)?;
                m.response(
                    q("phase_fraction")?,
                    q("laplacian_phase")?,
                    q("laplacian_chemical_potential")?,
                    i.get("phase_gradient"),
                )
            }
            "gray_surface_radiation" => {
                let m: GraySurfaceRadiation = self.model(context)?;
                m.heat_flux_out(q("surface_temperature")?, q("radiation_temperature")?)
            }
            "eddy_current_induction" => {
                let m: EddyCurrentInduction = self.model(context)?;
                m.response(
                    q("electric_field")?,
                    q("magnetic_flux_density")?,
                    q("electrical_conductivity")?,
                    None,
                )
            }
            "moving_conductor_electromagnetics" => {
                let m: EddyCurrentInduction = self.model(context)?;
                m.response(
                    q("electric_field")?,
                    q("magnetic_flux_density")?,
                    q("electrical_conductivity")?,
                    Some(q("material_velocity")?),
                )
            }
            "nernst_planck_ionic_transport" => {
                let m: NernstPlanckTransport = self.model(context)?;
                m.response(
                    q("species_concentration")?,
                    q("species_concentration_gradient")?,
                    q("electric_field")?,
                    q("temperature")?,
                    q("ionic_diffusivity")?,
                )
            }
            "archard_wear_evolution" => {
                let m: ArchardWearEvolution = self.model(context)?;
                m.update(q("contact_pressure")?, q("slip_velocity")?, q("time_increment")?)
            }
            "topology_hydraulic_geometry" => {
                let m: TopologyHydraulicGeometry = self.model(context)?;
                m.response(q("topology_density")?)
            }
            "hydraulic_network" => {
                let m: HydraulicNetworkSegment = self.model(context)?;
                m.response(
                    q("mass_flow_rate")?,
                    q("mass_density")?,
                    q("dynamic_viscosity")?,
                    q("hydraulic_area")?,
                    q("hydraulic_diameter")?,
                )
            }
            "orthotropic_heat_conduction" => {
                let m: OrthotropicHeatConduction = self.model(context)?;
                m.response(q("temperature_gradient")?, q("topology_density")?)
            }
            other => Err(PhysicsError::Value(format!("{other}: no algebraic adapter implementation"))),
        }
    }

    fn check_design(&self, design: &[String], message: impl Fn(&[String]) -> String) -> PhysicsResult<()> {
        let missing: Vec<String> =
            self.contract.design_coordinates.iter().filter(|c| !design.contains(c)).cloned().collect();
        if missing.is_empty() { Ok(()) } else { Err(PhysicsError::contract(message(&missing))) }
    }


    pub fn algebraic_evaluate_design_with_validity<S: Scalar>(
        &self,
        inputs: &Inputs<S>,
        design: &[String],
        context: &Value,
    ) -> PhysicsResult<(Inputs<S>, ModelOutputs<S>)> {
        let c = &self.contract;
        let id = &c.addin_id;
        self.check_design(design, |missing| {
            format!(
                "{id}: named design is missing [{}]",
                missing.iter().map(|m| repr_str(m)).collect::<Vec<_>>().join(", ")
            )
        })?;
        let required: Vec<&LocalPort> = c.consumes.iter().filter(|p| p.required).collect();
        let expected: BTreeSet<String> = required.iter().map(|p| input_port_id(id, p)).collect();
        let given: BTreeSet<String> = inputs.keys().cloned().collect();
        if given != expected {
            let fmt = |s: Vec<&String>| s.iter().map(|x| repr_str(x)).collect::<Vec<_>>().join(", ");
            return Err(PhysicsError::contract(format!(
                "{id}: strict algebraic input mismatch; missing=[{}], extra=[{}]",
                fmt(expected.difference(&given).collect()),
                fmt(given.difference(&expected).collect())
            )));
        }
        let mut quantities: Inputs<S> = BTreeMap::new();
        for port in &required {
            if quantities.contains_key(&port.quantity) {
                return Err(PhysicsError::contract(format!(
                    "{id}: local quantity {} is ambiguous",
                    repr_str(&port.quantity)
                )));
            }
            if let Some(v) = inputs.get(&input_port_id(id, port)) {
                quantities.insert(port.quantity.clone(), v.clone());
            }
        }
        let raw = self.algebraic_evaluate(&quantities, context)?;
        let mut outputs = BTreeMap::new();
        for port in &c.produces {
            let value = raw.get(&port.name).or_else(|| raw.get(&port.quantity)).ok_or_else(|| {
                PhysicsError::contract(format!(
                    "{id}: model omitted declared output {}/{}",
                    repr_str(&port.name),
                    repr_str(&port.quantity)
                ))
            })?;
            outputs.insert(output_port_id(id, port), value.clone());
        }
        let mut margins = ModelOutputs::new();
        for (k, v) in raw.iter() {
            let lk = k.to_lowercase();
            if (lk.contains("margin") || lk.contains("validity"))
                && let Some(t) = raw.get(k).filter(|_| matches!(v, models::ModelValue::Array(_)))
            {
                margins.set(k, t.clone());
            }
        }
        Ok((outputs, margins))
    }


    pub fn algebraic_evaluate_design<S: Scalar>(
        &self,
        inputs: &Inputs<S>,
        design: &[String],
        context: &Value,
    ) -> PhysicsResult<Inputs<S>> {
        let (outputs, margins) = self.algebraic_evaluate_design_with_validity(inputs, design, context)?;
        enforce_runtime_validity(
            &output_margins(&margins),
            &self.contract.addin_id,
            0.0,
            self.nonnegative_validity_margins(),
        )?;
        Ok(outputs)
    }


    pub fn response_value_design<S: Scalar>(
        &self,
        response: &str,
        outputs: &Inputs<S>,
        design: &[String],
        context: &Value,
    ) -> PhysicsResult<S> {
        let id = &self.contract.addin_id;
        self.check_design(design, |_| format!("{id}: response reduction lacks named design"))?;
        let mut physical: Inputs<S> = BTreeMap::new();
        let mut aggregations: BTreeMap<String, String> = BTreeMap::new();
        for port in &self.contract.produces {
            let token = output_port_id(id, port);
            let value = outputs.get(&token).ok_or_else(|| {
                PhysicsError::contract(format!("{id}: response reduction lacks {}", repr_str(&token)))
            })?;
            if let Some(existing) = physical.get(&port.quantity) {
                if aggregations.get(&port.quantity).map(String::as_str) != Some("sum")
                    || port.aggregation != "sum"
                {
                    return Err(PhysicsError::contract(format!(
                        "{id}: duplicate output quantity {} lacks additive reduction semantics",
                        repr_str(&port.quantity)
                    )));
                }
                let summed = existing.add(value)?;
                physical.insert(port.quantity.clone(), summed);
            } else {
                physical.insert(port.quantity.clone(), value.clone());
                aggregations.insert(port.quantity.clone(), port.aggregation.clone());
            }
        }
        self.response_value(response, &physical, context)
    }


    pub fn response_value<S: Scalar>(
        &self,
        response: &str,
        outputs: &Inputs<S>,
        context: &Value,
    ) -> PhysicsResult<S> {
        let aid = &self.contract.addin_id;
        let value = response_reduction(aid, response).and_then(|q| outputs.get(q)).ok_or_else(|| {
            PhysicsError::Value(format!("{aid}: no scalar response reduction for {}", repr_str(response)))
        })?;
        if let Some((unit, _)) = integrated_response(response) {
            return integrate_response(value, context, aid, response, unit);
        }
        let mode = match response {
            "contact_pressure" | "wear_depth" | "stress" | "effective_stress" | "linear_elastic_stress" => {
                "max_abs"
            }
            "strain_energy"
            | "linear_elastic_energy"
            | "thermal_storage"
            | "convective_heat_loss"
            | "frictional_heat"
            | "frictional_dissipation"
            | "reaction_heat"
            | "electrochemical_heat"
            | "radiation_exchange"
            | "joule_heating"
            | "material_loss" => "sum",
            "pressure_traction"
            | "lorentz_force"
            | "moving_conductor_lorentz_force"
            | "ionic_flux"
            | "darcy_flux"
            | "heat_flux"
            | "conductive_heat_flux"
            | "inertial_load" => "l2",
            _ => "mean",
        };
        reduce(value, mode)
    }


    pub fn linearize_design(
        &self,
        inputs: &Inputs<f64>,
        design: &[String],
        context: &Value,
    ) -> PhysicsResult<(Inputs<f64>, PortJacobians)> {
        const W: usize = 8;
        let values = self.algebraic_evaluate_design(inputs, design, context)?;
        let (flat, layout) = flatten(inputs);
        let mut jac: PortJacobians =
            values.iter().map(|(k, v)| (k.clone(), vec![0.0; v.size() * flat.len()])).collect();
        for start in (0..flat.len()).step_by(W) {
            let seeded = seed::<W>(&flat, start);
            let dual_inputs = unflatten(&seeded, &layout)?;
            let outputs = self.algebraic_evaluate_design(&dual_inputs, design, context)?;
            for (k, out) in &outputs {
                if let Some(block) = jac.get_mut(k) {
                    for (r, d) in out.data().iter().enumerate() {
                        for (w, e) in d.eps.iter().enumerate() {
                            if start + w < flat.len() {
                                block[r * flat.len() + start + w] = *e;
                            }
                        }
                    }
                }
            }
        }
        Ok((values, jac))
    }


    pub fn response_gradient_design(
        &self,
        response: &str,
        inputs: &Inputs<f64>,
        design: &[String],
        context: &Value,
    ) -> PhysicsResult<(f64, Vec<f64>)> {
        const W: usize = 8;
        let outputs = self.algebraic_evaluate_design(inputs, design, context)?;
        let value = self.response_value_design(response, &outputs, design, context)?;
        let (flat, layout) = flatten(inputs);
        let mut grad = vec![0.0; flat.len()];
        for start in (0..flat.len()).step_by(W) {
            let seeded = seed::<W>(&flat, start);
            let dual_inputs = unflatten(&seeded, &layout)?;
            let out = self.algebraic_evaluate_design(&dual_inputs, design, context)?;
            let r = self.response_value_design(response, &out, design, context)?;
            for (w, e) in r.eps.iter().enumerate() {
                if start + w < flat.len() {
                    grad[start + w] = *e;
                }
            }
        }
        Ok((value, grad))
    }
}

type Layout = Vec<(String, Vec<usize>, usize)>;

fn flatten(inputs: &Inputs<f64>) -> (Vec<f64>, Layout) {
    let mut flat = Vec::new();
    let mut layout = Vec::new();
    for (k, v) in inputs {
        layout.push((k.clone(), v.shape().to_vec(), v.size()));
        flat.extend_from_slice(v.data());
    }
    (flat, layout)
}

fn seed<const W: usize>(flat: &[f64], start: usize) -> Vec<Dual<W>> {
    flat.iter()
        .enumerate()
        .map(
            |(j, &v)| {
                if (start..start + W).contains(&j) { Dual::variable(v, j - start) } else { Dual::constant(v) }
            },
        )
        .collect()
}

fn unflatten<S: Scalar>(flat: &[S], layout: &Layout) -> PhysicsResult<Inputs<S>> {
    let mut out = BTreeMap::new();
    let mut offset = 0;
    for (k, shape, n) in layout {
        out.insert(k.clone(), Tensor::from_vec(shape.clone(), flat[offset..offset + n].to_vec())?);
        offset += n;
    }
    Ok(out)
}

fn integrate_response<S: Scalar>(
    value: &Tensor<S>,
    context: &Value,
    addin_id: &str,
    response: &str,
    unit: &str,
) -> PhysicsResult<S> {
    let measures = authoring_namespace(context, addin_id).get("response_measures").cloned();
    let spec = measures.as_ref().and_then(|m| m.get(response)).and_then(Value::as_object);
    let keys_ok = spec.is_some_and(|s| {
        s.len() == 3
            && s.contains_key("weights")
            && s.contains_key("unit")
            && s.contains_key("fixed_geometry")
    });
    let Some(spec) = spec.filter(|_| keys_ok) else {
        return Err(PhysicsError::Value(format!(
            "{addin_id}.{response}: integrated response requires explicit response_measures with weights, unit and fixed_geometry"
        )));
    };
    if spec.get("unit").and_then(Value::as_str) != Some(unit)
        || spec.get("fixed_geometry") != Some(&Value::Bool(true))
    {
        return Err(PhysicsError::Value(format!(
            "{addin_id}.{response}: requires fixed-geometry quadrature in {unit}; moving-measure sensitivities are not implemented"
        )));
    }
    let weights =
        spec.get("weights").map(Tensor::from_json).transpose()?.unwrap_or_else(|| Tensor::vector(Vec::new()));
    let total: f64 = weights.data().iter().sum();
    if weights.shape() != value.shape()
        || weights.size() == 0
        || !weights.data().iter().all(|w| w.is_finite() && *w > 0.0)
        || !total.is_finite()
    {
        return Err(PhysicsError::Value(format!(
            "{addin_id}.{response}: quadrature weights must be positive finite and match the scalar density/flux field shape exactly"
        )));
    }
    Ok(value.zip(&weights.lift(), |x, w| x * w)?.sum())
}


pub fn register_into_core(ctx: &InstallContext<'_>) -> Result<Vec<String>, CaeError> {
    let existing: BTreeSet<String> =
        ctx.registries().addins.snapshot().entries.iter().map(|e| e.contract.addin_id.clone()).collect();
    let mut rows = Vec::new();
    for local in ensure_default_addins(None).map_err(CaeError::from)?.list() {
        if existing.contains(&local.addin_id) {
            continue;
        }
        let core = convert_contract(&local)?;
        let adapter: Arc<dyn AddInAdapter> = Arc::new(PhysicsLibraryAlgebraicAdapter::new(local));
        let registered = ctx.register_addin(ContractInput::Typed(Box::new(core)), Some(adapter))?;
        rows.push(registered.addin_id);
    }
    Ok(rows)
}
