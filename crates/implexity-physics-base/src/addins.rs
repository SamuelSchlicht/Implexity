// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use crate::contracts::{AddinContract, AuthoringField as A, PortSpec as P};
use crate::model_errors::PhysicsResult;
use crate::registry::PhysicsAddinRegistry;

pub const APPLICATION_DESIGN_COORDINATES: [&str; 1] = ["model:control"];

struct C(AddinContract);

fn c(addin_id: &str, family: &str) -> C {
    let mut contract = AddinContract::new(addin_id, family, "33.0", "intermediate");
    contract.design_coordinates = APPLICATION_DESIGN_COORDINATES.iter().map(|s| (*s).to_string()).collect();
    contract.exact_partial_derivatives = true;
    C(contract)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

impl C {
    fn aliases(mut self, aliases: &[&str]) -> Self {
        self.0.objective_aliases = strings(aliases);
        self
    }
    fn produces(mut self, ports: Vec<P>) -> Self {
        self.0.produces = ports;
        self
    }
    fn consumes(mut self, ports: Vec<P>) -> Self {
        self.0.consumes = ports;
        self
    }
    fn authoring(mut self, fields: Vec<A>) -> Self {
        self.0.authoring = fields;
        self
    }
    fn coupled(mut self, group: &str, strategy: &str) -> Self {
        self.0.coupled_group = Some(group.into());
        self.0.solve_strategy = Some(strategy.into());
        self
    }
    fn path_dependent(mut self) -> Self {
        self.0.path_dependent = true;
        self
    }
    fn balances(mut self, balances: &[&str]) -> Self {
        self.0.conservative_balances = strings(balances);
        self
    }
    fn notes(mut self, notes: &[&str]) -> Self {
        self.0.validity_notes = strings(notes);
        self
    }
    fn factory(mut self, factory: &str) -> Self {
        self.0.runtime_factory = Some(factory.into());
        self
    }
    fn metadata(mut self, metadata: Value) -> Self {
        if let Value::Object(m) = metadata {
            self.0.metadata = m;
        }
        self
    }
}

#[must_use]
#[allow(clippy::too_many_lines, clippy::large_stack_arrays)]
pub fn contracts() -> Vec<AddinContract> {
    let base: Vec<AddinContract> = [
        c("solid_material_temperature", "constitutive")
            .aliases(&["temperature_dependent_material"])
            .produces(vec![P::new("elasticity", "youngs_modulus", "Pa", "volume"), P::new("conductivity", "thermal_conductivity", "W/(m K)", "volume"), P::new("heat_capacity", "heat_capacity", "J/(kg K)", "volume"), P::new("mass_density", "mass_density", "kg/m^3", "volume"), P::new("volumetric_heat_capacity", "volumetric_heat_capacity", "J/(m^3 K)", "volume"), P::new("thermal_expansion", "thermal_expansion", "1/K", "volume")])
            .consumes(vec![P::new("temperature", "temperature", "K", "volume").optional(), P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("T_ref", "K").min(1.0e-9),
            A::new("T_min", "K").min(1.0e-9),
            A::new("T_max", "K").min(1.0e-9),
            A::new("E_ref", "Pa").min(1.0e-12),
            A::new("nu", "1").min(-0.999_999).max(0.499_999),
            A::new("k_ref", "W/(m K)").min(1.0e-18),
            A::new("cp_ref", "J/(kg K)").min(1.0e-18),
            A::new("density", "kg/m^3").min(1.0e-18),
            A::new("dlnE_dT", "1/K").optional().default_value(json!(0.0)),
            A::new("alpha_ref", "1/K").optional().default_value(json!(0.0)),
            A::new("dalpha_dT", "1/K").optional().default_value(json!(0.0)),
            A::new("dlnk_dT", "1/K").optional().default_value(json!(0.0)),
            A::new("dlncp_dT", "1/K").optional().default_value(json!(0.0)),
        ])
            .balances(&["energy"])
            .notes(&["Provider must block values outside authored temperature range."])
            .factory("implexity.physics_library.models:TemperatureDependentSolidMaterial"),
        c("external_convection_interface", "interface")
            .aliases(&["convective_heat_loss"])
            .produces(vec![P::new("heat_flux", "boundary_heat_flux", "W/m^2", "surface").aggregation("sum")])
            .consumes(vec![P::new("surface_temperature", "surface_temperature", "K", "surface"), P::new("ambient_temperature", "ambient_temperature", "K", "global")])
            .authoring(vec![
            A::new("h", "W/(m^2 K)").min(1.0e-18),
            A::new("ambient_temperature", "K").min(1.0e-9),
        ])
            .balances(&["energy"])
            .factory("implexity.physics_library.models:ExternalConvection"),
        c("temperature_dependent_fluid_viscosity", "constitutive")
            .aliases(&["dynamic_viscosity"])
            .produces(vec![P::new("viscosity", "dynamic_viscosity", "Pa s", "volume")])
            .consumes(vec![P::new("temperature", "fluid_temperature", "K", "volume").optional()])
            .authoring(vec![
            A::new("law", "1").choices(&["arrhenius_liquid", "sutherland_gas"]),
            A::new("mu_ref", "Pa s").min(1.0e-30),
            A::new("T_ref", "K").min(1.0e-9),
            A::new("T_min", "K").min(1.0e-9),
            A::new("T_max", "K").min(1.0e-9),
            A::new("activation_over_R", "K").optional().default_value(json!(0.0)),
            A::new("sutherland_constant", "K").optional().default_value(json!(0.0)),
        ])
            .notes(&["No extrapolation beyond the authored correlation range. Supply calibrated coefficients; zero defaults are not material calibration. Sutherland law is a dilute-gas correlation, not a high-pressure or reacting-mixture transport model."])
            .factory("implexity.physics_library.models:TemperatureDependentViscosity"),
        c("pressure_structure_transfer", "interface")
            .aliases(&["pressure_traction"])
            .produces(vec![P::new("traction", "surface_traction", "N", "surface").aggregation("sum").reverse("boundary_velocity")])
            .consumes(vec![P::new("pressure", "surface_pressure", "Pa", "surface"), P::new("normal", "surface_normal", "1", "surface")])
            .authoring(vec![
            A::new("mapping", "1").array(&[None, None]),
            A::new("quadrature_areas_m2", "m^2").array(&[None]),
            A::new("structural_points_m", "m").array(&[None, Some(3)]),
            A::new("quadrature_points_m", "m").array(&[None, Some(3)]),
            A::new("fluid_geometry_mode", "1").choices(&["frozen"]),
        ])
            .balances(&["mechanical_power", "force", "moment"])
            .notes(&["Output retains legacy surface_traction name but is an integrated force in N. Explicit positive quadrature areas and Cartesian node/face coordinates are required. Node-major xyz map must preserve rigid translations and rotations; reverse velocity uses its transpose. Frozen geometry only; area-free, unverified arbitrary maps, generalized-DOF maps, and two-way FSI requests are rejected."])
            .factory("implexity.physics_library.models:PressureStructureTransfer")
            .metadata(json!({"reverse_quantity": "boundary_velocity"})),
        c("regularised_contact_friction", "interface")
            .aliases(&["contact_pressure", "frictional_heat", "frictional_dissipation"])
            .produces(vec![P::new("normal_traction", "contact_pressure", "Pa", "interface"), P::new("tangential_traction", "tangential_traction", "Pa", "interface"), P::new("heat_a", "boundary_heat_flux", "W/m^2", "interface").aggregation("sum"), P::new("heat_b", "boundary_heat_flux", "W/m^2", "interface").aggregation("sum")])
            .consumes(vec![P::new("gap", "gap", "m", "interface"), P::new("slip", "slip_velocity", "m/s", "interface")])
            .authoring(vec![
            A::new("normal_penalty", "Pa/m").min(1.0e-18),
            A::new("gap_width", "m").min(1.0e-18),
            A::new("friction_coefficient", "1").min(0i64),
            A::new("slip_regularisation", "m/s").min(1.0e-18),
            A::new("heat_fraction_a", "1").min(0i64).max(1i64),
        ])
            .balances(&["mechanical_power", "energy"])
            .factory("implexity.physics_library.models:RegularisedContactFriction"),
        c("regularised_j2_plastic_damage", "constitutive")
            .aliases(&["plastic_strain", "damage", "plastic_work_estimate"])
            .produces(vec![P::new("stress", "stress", "Pa", "history"), P::new("plastic_strain", "plastic_strain", "1", "history"), P::new("damage", "damage", "1", "history"), P::new("plastic_work_estimate", "plastic_work_estimate", "J/m^3", "history"), P::new("hardening_storage_increment", "hardening_storage_increment", "J/m^3", "history")])
            .consumes(vec![P::new("strain", "strain", "1", "history"), P::new("temperature", "temperature", "K", "volume").optional(), P::new("material", "youngs_modulus", "Pa", "volume").optional()])
            .authoring(vec![
            A::new("youngs_modulus", "Pa").min(1.0e-18),
            A::new("poisson_ratio", "1").min(-0.999_999).max(0.499_999),
            A::new("yield_stress", "Pa").min(1.0e-18),
            A::new("hardening_modulus", "Pa").min(0i64),
            A::new("transition_width", "Pa").min(1.0e-18),
            A::new("damage_onset", "1").min(0i64),
            A::new("damage_rate", "1").min(0i64),
        ])
            .path_dependent()
            .notes(&["Blocked history adapter. Plastic work estimate includes recoverable hardening storage and is not dissipation or a heat source; thermodynamic damage/regularization accounting is unavailable."])
            .factory("implexity.physics_library.models:RegularisedJ2PlasticDamage"),
        c("plastic_dissipation_heat_conversion", "interface")
            .aliases(&["plastic_heat_generation"])
            .produces(vec![P::new("heat_rate", "volumetric_heat_source", "W/m^3", "volume").aggregation("sum")])
            .consumes(vec![P::new("dissipation", "dissipation_energy_density", "J/m^3", "history"), P::new("time_increment", "time_increment", "s", "global")])
            .authoring(vec![
            A::new("taylor_quinney", "1").min(0.0).max(1.0),
        ])
            .balances(&["mechanical_dissipation", "energy"])
            .factory("implexity.physics_library.models:DissipationHeatConversion"),
        c("species_reaction_network", "field_constitutive")
            .aliases(&["species_source", "reaction_rate", "reaction_heat"])
            .produces(vec![P::new("reaction_rate", "reaction_rate", "mol/(m^3 s)", "volume"), P::new("species_source", "species_source", "mol/(m^3 s)", "volume").aggregation("sum"), P::new("reaction_heat", "volumetric_heat_source", "W/m^3", "volume").aggregation("sum")])
            .consumes(vec![P::new("concentration", "species_concentration", "mol/m^3", "volume"), P::new("temperature", "temperature", "K", "volume")])
            .authoring(vec![
            A::new("stoichiometry", "1").array(&[None, None]),
            A::new("activation_energy", "J/mol").array(&[None]),
            A::new("prefactor", "(mol/m^3)^(1-order)/s").array(&[None]),
            A::new("reaction_enthalpy", "J/mol").array(&[None]),
            A::new("element_matrix", "1").optional().array(&[None, None]),
        ])
            .balances(&["species", "energy"])
            .notes(&["One local species vector; elementary irreversible mass action uses reactant stoichiometry as reaction order. Prefactor units depend on each reaction order; 1/s applies only to first order. Optional element_matrix enables elemental-balance validation. No species field solve."])
            .factory("implexity.physics_library.models:ReactionNetwork"),
        c("butler_volmer_interface", "interface")
            .aliases(&["current_density", "electrochemical_heat"])
            .produces(vec![P::new("current", "current_density", "A/m^2", "interface"), P::new("heat", "boundary_heat_flux", "W/m^2", "interface").aggregation("sum"), P::new("species_flux", "species_flux", "mol/(m^2 s)", "interface").aggregation("sum")])
            .consumes(vec![P::new("driving_voltage", "overpotential", "V", "interface"), P::new("temperature", "temperature", "K", "interface"), P::new("concentration", "species_concentration", "mol/m^3", "interface").optional(), P::new("activity_ratio", "activity_ratio", "1", "interface").optional()])
            .authoring(vec![
            A::new("exchange_current_density", "A/m^2").min(1.0e-30),
            A::new("alpha_anodic", "1").min(1.0e-12),
            A::new("alpha_cathodic", "1").min(1.0e-12),
            A::new("electrons", "1").min(1.0e-12),
            A::new("equilibrium_potential", "V"),
            A::new("entropy_change", "J/(mol K)").optional().default_value(json!(0.0)),
            A::new("input_mode", "1").optional().choices(&["overpotential", "electrode_potential"]).default_value(json!("overpotential")),
        ])
            .balances(&["charge"])
            .factory("implexity.physics_library.models:ButlerVolmerInterface"),
        c("phase_change_enthalpy", "constitutive")
            .aliases(&["phase_fraction", "latent_heat", "enthalpy"])
            .produces(vec![P::new("phase_fraction", "phase_fraction", "1", "volume"), P::new("enthalpy", "specific_enthalpy", "J/kg", "volume"), P::new("cp_effective", "heat_capacity", "J/(kg K)", "volume").aggregation("mixture")])
            .consumes(vec![P::new("temperature", "temperature", "K", "volume")])
            .authoring(vec![
            A::new("melting_temperature", "K").min(1.0e-9),
            A::new("transition_width", "K").min(1.0e-12),
            A::new("heat_capacity_solid", "J/(kg K)").min(1.0e-18),
            A::new("heat_capacity_liquid", "J/(kg K)").min(1.0e-18),
            A::new("latent_heat", "J/kg").min(0i64),
            A::new("T_ref", "K").min(1.0e-9),
        ])
            .balances(&["energy"])
            .factory("implexity.physics_library.models:PhaseChangeEnthalpy"),
        c("vibroacoustic_interface", "interface")
            .aliases(&["interface_power_residual"])
            .produces(vec![P::new("normal_velocity", "normal_velocity", "m/s", "interface"), P::new("structural_force", "surface_traction", "N", "interface").aggregation("sum"), P::new("interface_power_residual", "interface_power_residual", "W", "interface")])
            .consumes(vec![P::new("structural_velocity", "structural_velocity", "m/s", "interface"), P::new("acoustic_pressure", "acoustic_pressure", "Pa", "interface")])
            .authoring(vec![
            A::new("velocity_mapping", "1").array(&[None, None]),
            A::new("quadrature_areas_m2", "m^2").array(&[None]),
        ])
            .coupled("vibroacoustic", "monolithic_or_exact_partitioned")
            .balances(&["mechanical_acoustic_power"])
            .notes(&["Frozen interface velocity map; pressure is integrated with explicit positive acoustic quadrature areas, and structural force uses the adjoint map. Power conservation alone does not verify geometric interpolation or a coupled acoustic/structural field solve."])
            .factory("implexity.physics_library.models:VibroAcousticInterface"),
        c("topology_electrical_conductivity", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("sigma", "electrical_conductivity", "S/m", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "S/m").min(0i64),
            A::new("void_value", "S/m").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(3.0)),
        ])
            .notes(&["SIMP-style screening interpolation of electrical conductivity."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("topology_ionic_diffusivity", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("diffusivity", "ionic_diffusivity", "m^2/s", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "m^2/s").min(0i64),
            A::new("void_value", "m^2/s").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(3.0)),
        ])
            .notes(&["SIMP-style screening interpolation of an effective ionic diffusivity."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("topology_permeability", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("permeability", "permeability", "m^2", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "m^2").min(0i64),
            A::new("void_value", "m^2").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(3.0)),
        ])
            .notes(&["SIMP-style screening interpolation of permeability; not a pore-scale homogenisation law."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("finite_strain_neo_hookean", "field_mechanics")
            .aliases(&["deformation", "strain_energy", "stress"])
            .produces(vec![P::new("stress", "cauchy_stress", "Pa", "volume"), P::new("energy", "strain_energy_density", "J/m^3", "volume")])
            .consumes(vec![P::new("deformation_gradient", "deformation_gradient", "1", "volume"), P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("shear_modulus", "Pa").min(1.0e-18),
            A::new("bulk_modulus", "Pa").min(1.0e-18),
            A::new("density_floor", "1").optional().min(1.0e-9).max(1.0).default_value(json!(1e-06)),
        ])
            .notes(&["Compressible neo-Hookean finite-strain reference model; det(F) must remain positive."])
            .factory("implexity.physics_library.models:FiniteStrainNeoHookean"),
        c("generalized_maxwell_viscoelasticity", "constitutive_evolution")
            .aliases(&["viscoelastic_stress", "relaxation"])
            .produces(vec![P::new("stress", "stress", "Pa", "history"), P::new("dissipation", "dissipation_energy_density", "J/m^3", "history").aggregation("sum")])
            .consumes(vec![P::new("strain", "strain", "1", "history"), P::new("time_increment", "time_increment", "s", "global"), P::new("temperature", "temperature", "K", "volume").optional()])
            .authoring(vec![
            A::new("equilibrium_modulus", "Pa").min(1.0e-18),
            A::new("branch_moduli", "Pa").array(&[None]),
            A::new("relaxation_times", "s").array(&[None]),
        ])
            .path_dependent()
            .notes(&["Linear generalized-Maxwell strain measure; finite-strain use requires a finite-strain viscoelastic provider."])
            .factory("implexity.physics_library.models:GeneralizedMaxwellViscoelasticity"),
        c("biot_poromechanics", "field_poromechanics")
            .aliases(&["effective_stress", "fluid_content", "darcy_flux", "poroelastic_storage", "darcy_dissipation"])
            .produces(vec![P::new("effective_stress", "effective_stress", "Pa", "volume"), P::new("fluid_content", "fluid_content", "1", "volume"), P::new("darcy_flux", "darcy_flux", "m/s", "volume"), P::new("stored_energy_density", "stored_energy_density", "J/m^3", "volume"), P::new("darcy_dissipation_density", "volumetric_heat_source", "W/m^3", "volume")])
            .consumes(vec![P::new("volumetric_strain", "volumetric_strain", "1", "volume"), P::new("pore_pressure", "pore_pressure", "Pa", "volume"), P::new("pressure_gradient", "pressure_gradient", "Pa/m", "volume"), P::new("permeability", "permeability", "m^2", "volume"), P::new("dynamic_viscosity", "dynamic_viscosity", "Pa s", "volume").optional()])
            .authoring(vec![
            A::new("biot_coefficient", "1").min(0i64).max(1i64),
            A::new("biot_modulus", "Pa").min(1.0e-18),
            A::new("drained_bulk_modulus", "Pa").min(1.0e-18),
            A::new("viscosity", "Pa s").min(1.0e-30),
        ])
            .balances(&["fluid_mass", "mechanical_power"])
            .notes(&["Small-strain volumetric Biot constitutive closure with isotropic scalar permeability; no shear skeleton or native pressure/displacement field solve. effective_stress is the legacy name for total mean stress. Darcy dissipation excludes gravity and requires a pressure gradient with a final spatial axis; exported heat must not be counted twice."])
            .factory("implexity.physics_library.models:BiotPoromechanics"),
        c("cahn_hilliard_phase_field", "field_multiphase")
            .aliases(&["chemical_potential", "interface_energy"])
            .produces(vec![P::new("chemical_potential", "chemical_potential", "J/m^3", "volume"), P::new("phase_flux", "phase_flux", "1/s", "volume"), P::new("interface_energy", "interface_energy_density", "J/m^3", "volume")])
            .consumes(vec![P::new("phase", "phase_fraction", "1", "volume"), P::new("phase_gradient", "phase_gradient", "1/m", "volume"), P::new("laplacian_phase", "laplacian_phase", "1/m^2", "volume"), P::new("laplacian_mu", "laplacian_chemical_potential", "J/m^5", "volume")])
            .authoring(vec![
            A::new("mixing_energy", "J/m^3").min(1.0e-30),
            A::new("gradient_energy", "J/m").min(1.0e-30),
            A::new("mobility", "m^5/(J s)").min(1.0e-30),
        ])
            .balances(&["phase_mass"])
            .notes(&["Total free-energy density includes local mixing and gradient energy; phase_gradient is required, including for legacy inputs. Supplied differential fields must describe the same phase field; this closure does not solve phase evolution."])
            .factory("implexity.physics_library.models:CahnHilliardPhaseField"),
        c("gray_surface_radiation", "field_radiation")
            .aliases(&["radiative_heat_flux", "radiation_exchange"])
            .produces(vec![P::new("heat_flux", "boundary_heat_flux", "W/m^2", "surface").aggregation("sum")])
            .consumes(vec![P::new("surface_temperature", "surface_temperature", "K", "surface"), P::new("surroundings_temperature", "radiation_temperature", "K", "global")])
            .authoring(vec![
            A::new("emissivity", "1").min(0i64).max(1i64),
        ])
            .balances(&["energy"])
            .notes(&["Gray diffuse surface-to-large-surroundings model; participating-media radiation requires a different provider."])
            .factory("implexity.physics_library.models:GraySurfaceRadiation"),
        c("eddy_current_induction", "field_electromagnetics")
            .aliases(&["joule_heating", "current_density", "lorentz_force"])
            .produces(vec![P::new("current_density", "current_density", "A/m^2", "volume"), P::new("joule_heat", "volumetric_heat_source", "W/m^3", "volume").aggregation("sum"), P::new("lorentz_force", "body_force_density", "N/m^3", "volume").aggregation("sum")])
            .consumes(vec![P::new("electric_field", "electric_field", "V/m", "volume"), P::new("magnetic_flux_density", "magnetic_flux_density", "T", "volume"), P::new("conductivity", "electrical_conductivity", "S/m", "volume")])
            .balances(&["energy"])
            .notes(&["Quasi-static local constitutive closure; Maxwell field solution must be supplied by a compatible field provider."])
            .factory("implexity.physics_library.models:EddyCurrentInduction"),
        c("moving_conductor_electromagnetics", "field_electromagnetics")
            .aliases(&["moving_conductor_joule_heat", "moving_conductor_lorentz_force", "electromagnetic_mechanical_power"])
            .produces(vec![P::new("current_density", "current_density", "A/m^2", "volume"), P::new("joule_heat", "volumetric_heat_source", "W/m^3", "volume"), P::new("lorentz_force", "body_force_density", "N/m^3", "volume"), P::new("mechanical_power", "mechanical_power_density", "W/m^3", "volume"), P::new("electrical_power", "electrical_power_density", "W/m^3", "volume"), P::new("energy_balance", "electromagnetic_energy_balance", "W/m^3", "volume")])
            .consumes(vec![P::new("electric_field", "electric_field", "V/m", "volume"), P::new("magnetic_flux_density", "magnetic_flux_density", "T", "volume"), P::new("conductivity", "electrical_conductivity", "S/m", "volume"), P::new("material_velocity", "material_velocity", "m/s", "volume")])
            .balances(&["energy"])
            .notes(&["Instantaneous real fields in one laboratory frame, nonrelativistic isotropic Ohm law J=sigma(E+v cross B). J dot E = Joule heat + Lorentz mechanical power. Required velocity is collocated with fields; no Maxwell, charge-transport, thermal or mechanical field solve is supplied by this local closure."])
            .factory("implexity.physics_library.models:EddyCurrentInduction"),
        c("nernst_planck_ionic_transport", "field_electrochemistry")
            .aliases(&["ionic_flux", "ionic_current"])
            .produces(vec![P::new("ionic_flux", "species_flux", "mol/(m^2 s)", "volume").aggregation("sum"), P::new("ionic_current", "current_density", "A/m^2", "volume").aggregation("sum")])
            .consumes(vec![P::new("concentration", "species_concentration", "mol/m^3", "volume"), P::new("concentration_gradient", "species_concentration_gradient", "mol/m^4", "volume"), P::new("electric_field", "electric_field", "V/m", "volume"), P::new("temperature", "temperature", "K", "volume"), P::new("diffusivity", "ionic_diffusivity", "m^2/s", "volume")])
            .authoring(vec![
            A::new("charge_number", "1"),
        ])
            .balances(&["charge"])
            .factory("implexity.physics_library.models:NernstPlanckTransport"),
        c("archard_wear_evolution", "evolution")
            .aliases(&["wear_depth", "material_loss"])
            .produces(vec![P::new("wear_rate", "geometry_recession_rate", "m/s", "interface"), P::new("wear_increment", "geometry_recession", "m", "history")])
            .consumes(vec![P::new("contact_pressure", "contact_pressure", "Pa", "interface"), P::new("slip_speed", "slip_velocity", "m/s", "interface"), P::new("time_increment", "time_increment", "s", "global")])
            .authoring(vec![
            A::new("wear_coefficient", "1").min(0i64),
            A::new("hardness", "Pa").min(1.0e-18),
        ])
            .path_dependent()
            .notes(&["Archard sliding-wear law; abrasive/erosive mechanisms require separate providers. Slip may be scalar per contact point or a trailing 1/2/3-component tangential vector; vector wear uses its norm. The vector norm is nonsmooth at zero slip, where the implementation selects a zero derivative. Recession is a local increment; geometry feedback is not performed by this constitutive law."])
            .factory("implexity.physics_library.models:ArchardWearEvolution"),
        c("topology_hydraulic_geometry", "interface")
            .aliases(&[])
            .produces(vec![P::new("area", "hydraulic_area", "m^2", "network"), P::new("diameter", "hydraulic_diameter", "m", "network")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("maximum_area", "m^2").min(1.0e-30),
            A::new("maximum_hydraulic_diameter", "m").min(1.0e-18),
            A::new("minimum_open_fraction", "1").optional().min(1.0e-6).max(1.0).default_value(json!(0.02)),
        ])
            .notes(&["Screening geometry map from solid topology density to a hydraulically open network cross-section."])
            .factory("implexity.physics_library.models:TopologyHydraulicGeometry"),
        c("hydraulic_network", "network_system")
            .aliases(&["pressure_drop", "pump_power"])
            .produces(vec![P::new("pressure_drop", "pressure_drop", "Pa", "network"), P::new("pump_power", "pump_power", "W", "network")])
            .consumes(vec![P::new("mass_flow", "mass_flow_rate", "kg/s", "network"), P::new("density", "mass_density", "kg/m^3", "network"), P::new("viscosity", "dynamic_viscosity", "Pa s", "network"), P::new("area", "hydraulic_area", "m^2", "network"), P::new("diameter", "hydraulic_diameter", "m", "network")])
            .authoring(vec![
            A::new("length", "m").min(1.0e-18),
            A::new("roughness", "m").min(0i64),
        ])
            .balances(&["mass", "mechanical_energy"])
            .notes(&["pressure_drop is a nonnegative friction-loss magnitude for either mass-flow sign; pump_power is ideal hydraulic dissipation, pressure_drop*abs(mass_flow)/density, not electrical input power. Smoothed laminar/Haaland screening correlation, not a resolved flow solver."])
            .factory("implexity.physics_library.models:HydraulicNetworkSegment"),
        c("orthotropic_heat_conduction", "field_thermal")
            .aliases(&["heat_flux"])
            .produces(vec![P::new("heat_flux", "heat_flux", "W/m^2", "volume")])
            .consumes(vec![P::new("temperature_gradient", "temperature_gradient", "K/m", "volume"), P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("conductivity_tensor", "W/(m K)").array(&[None, None]),
            A::new("simp_penalty", "1").optional().min(1i64).default_value(json!(3.0)),
        ])
            .balances(&["energy"])
            .notes(&["Local Fourier constitutive closure, not a temperature-field solver. Requires a symmetric positive-definite conductivity tensor in the gradient coordinate frame. The former temperature_gradient objective alias was removed because it incorrectly returned heat flux; use heat_flux for W/m^2 results."])
            .factory("implexity.physics_library.models:OrthotropicHeatConduction"),
        c("topology_thermal_conductivity", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("conductivity", "thermal_conductivity", "W/(m K)", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "W/(m K)").min(0i64),
            A::new("void_value", "W/(m K)").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(3.0)),
        ])
            .notes(&["SIMP-style effective thermal-conductivity interpolation; not a microstructure homogenisation law."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("isotropic_heat_conduction", "field_thermal")
            .aliases(&["conductive_heat_flux"])
            .produces(vec![P::new("heat_flux", "heat_flux", "W/m^2", "volume")])
            .consumes(vec![P::new("temperature_gradient", "temperature_gradient", "K/m", "volume"), P::new("conductivity", "thermal_conductivity", "W/(m K)", "volume")])
            .balances(&["energy"])
            .notes(&["Local Fourier closure; temperature equilibrium requires a compatible thermal field provider when not directly authored."])
            .factory("implexity.physics_library.models:IsotropicHeatConduction"),
        c("topology_youngs_modulus", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("modulus", "youngs_modulus", "Pa", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "Pa").min(0i64),
            A::new("void_value", "Pa").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(3.0)),
        ])
            .notes(&["SIMP-style Young's-modulus interpolation for reference small-strain mechanics."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("isotropic_small_strain_elasticity", "field_mechanics")
            .aliases(&["linear_elastic_energy", "linear_elastic_stress"])
            .produces(vec![P::new("stress", "stress", "Pa", "volume"), P::new("energy", "strain_energy_density", "J/m^3", "volume")])
            .consumes(vec![P::new("strain", "strain", "1", "volume"), P::new("modulus", "youngs_modulus", "Pa", "volume")])
            .authoring(vec![
            A::new("poisson_ratio", "1").min(-0.999_999).max(0.499_999),
        ])
            .balances(&["mechanical_power"])
            .notes(&["Infinitesimal-strain constitutive closure; finite-deformation applications require a finite-strain provider."])
            .factory("implexity.physics_library.models:IsotropicSmallStrainElasticity"),
        c("topology_mass_density", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("density", "mass_density", "kg/m^3", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "kg/m^3").min(0i64),
            A::new("void_value", "kg/m^3").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(1.0)),
        ])
            .notes(&["Topology-dependent effective mass density."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("inertial_body_force", "field_mechanics")
            .aliases(&["inertial_load"])
            .produces(vec![P::new("force", "inertial_body_force_density", "N/m^3", "volume")])
            .consumes(vec![P::new("density", "mass_density", "kg/m^3", "volume"), P::new("acceleration", "acceleration", "m/s^2", "volume")])
            .balances(&["mechanical_power"])
            .factory("implexity.physics_library.models:InertialBodyForce"),
        c("density_heat_capacity", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("capacity", "volumetric_heat_capacity", "J/(m^3 K)", "volume")])
            .consumes(vec![P::new("density", "mass_density", "kg/m^3", "volume"), P::new("specific_heat", "heat_capacity", "J/(kg K)", "volume")])
            .notes(&["Local density times specific heat capacity. With apparent phase-change heat capacity, use once in rho*dh/dT*dT/dt; do not add latent heat again. Does not implement compressibility work, mass transport or a transient field solve."])
            .factory("implexity.physics_library.models:DensityHeatCapacity"),
        c("topology_volumetric_heat_capacity", "constitutive")
            .aliases(&[])
            .produces(vec![P::new("capacity", "volumetric_heat_capacity", "J/(m^3 K)", "volume")])
            .consumes(vec![P::new("topology", "topology_density", "1", "volume")])
            .authoring(vec![
            A::new("solid_value", "J/(m^3 K)").min(0i64),
            A::new("void_value", "J/(m^3 K)").min(0i64),
            A::new("penalty", "1").optional().min(1i64).default_value(json!(1.0)),
        ])
            .notes(&["Topology-dependent volumetric sensible heat capacity."])
            .factory("implexity.physics_library.models:TopologyScalarInterpolation"),
        c("enthalpy_storage_increment", "constitutive_evolution")
            .aliases(&["enthalpy_storage"])
            .produces(vec![P::new("increment", "stored_energy_increment", "J/m^3", "volume"), P::new("storage", "thermal_storage_rate", "W/m^3", "volume")])
            .consumes(vec![P::new("enthalpy", "specific_enthalpy", "J/kg", "volume"), P::new("previous_enthalpy", "previous_specific_enthalpy", "J/kg", "volume"), P::new("reference_density", "reference_mass_density", "kg/m^3", "volume"), P::new("time_increment", "time_increment", "s", "global")])
            .balances(&["energy"])
            .notes(&["Finite-step storage from explicit current and previous specific enthalpy, at fixed reference mass per reference volume. Previous enthalpy must be the same material point and reference convention. No mass transport, compressibility work, hidden history update or field solve. Negative storage is valid cooling, not negative dissipation."])
            .factory("implexity.physics_library.models:EnthalpyStorageIncrement"),
        c("thermal_storage_rate", "field_thermal")
            .aliases(&["thermal_storage"])
            .produces(vec![P::new("storage", "thermal_storage_rate", "W/m^3", "volume")])
            .consumes(vec![P::new("capacity", "volumetric_heat_capacity", "J/(m^3 K)", "volume"), P::new("temperature_rate", "temperature_rate", "K/s", "volume")])
            .balances(&["energy"])
            .factory("implexity.physics_library.models:ThermalStorageRate"),
        c("normalised_moving_heat_source", "process")
            .aliases(&["integrated_absorbed_power"])
            .produces(vec![P::new("heat_source", "volumetric_heat_source", "W/m^3", "volume").aggregation("sum"), P::new("integrated_power", "integrated_power", "W", "global")])
            .consumes(vec![P::new("geometry", "mesh_geometry", "m", "volume"), P::new("centre", "source_position", "m", "global"), P::new("cell_volumes", "cell_volumes", "m^3", "volume")])
            .authoring(vec![
            A::new("absorbed_power", "W").min(0i64),
            A::new("radius", "m").min(1.0e-18),
        ])
            .balances(&["energy"])
            .notes(&["Instantaneous normalized Gaussian at an explicitly supplied centre. A trajectory provider must supply time-dependent source_position; no time history is inferred. Explicit cell volumes normalize absorbed power over the supplied discrete domain."])
            .factory("implexity.physics_library.models:NormalisedMovingHeatSource"),

    ]
    .into_iter()
    .map(|b| b.0)
    .collect();

    let mut out = base.clone();
    if let Some(biot) = base.iter().find(|c| c.addin_id == "biot_poromechanics") {
        let mut coupled = biot.clone();
        coupled.addin_id = "viscosity_coupled_biot_poromechanics".into();
        for p in &mut coupled.consumes {
            p.required = true;
        }
        coupled.authoring.retain(|a| a.name != "viscosity");
        coupled.validity_notes.push(
            "Dynamic viscosity is a required collocated input; connect a viscosity law or author its field explicitly."
                .into(),
        );
        out.push(coupled);
    }
    if let Some(viscosity) = base.iter().find(|c| c.addin_id == "temperature_dependent_fluid_viscosity") {
        let mut field = viscosity.clone();
        field.addin_id = "field_temperature_fluid_viscosity".into();
        for p in &mut field.consumes {
            p.required = true;
        }
        field.validity_notes.push(
            "Requires a fluid-temperature field; no fallback to reference temperature. Supply the calibrated coefficient for the selected law."
                .into(),
        );
        out.push(field);
    }
    out
}


pub fn register_all(registry: &PhysicsAddinRegistry) -> PhysicsResult<()> {
    for contract in contracts() {
        registry.register(contract, false)?;
    }
    Ok(())
}
