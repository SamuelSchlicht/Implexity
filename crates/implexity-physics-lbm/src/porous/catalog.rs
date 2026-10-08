// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

pub const LIMITATIONS: [&str; 12] = [
    "Experimental common-quadrature native-solid/D3Q27 shared residual; not qualification.",
    "Responses distinguish endpoint, final-interval and explicitly selected full-history reductions. Stored-time maxima do not bound between-step extremes. No fatigue catalogue is implied.",
    "Fixed isotropic physical grid, full clear x-pressure collars, periodic transverse axes and fixed authored pressure trace, or an explicitly selected planar reference wall replacing one pressure port.",
    "Native constitutive storage/conduction occurs once; shared fluid caloric mass is conserved population mass.",
    "Relative-drag reaction/heat are coupled. Opt-in forced-BGK constitutive viscous stress/heat is a low-Mach closure, not an exact discrete kinetic-energy balance.",
    "Trace shear is authored by default. Explicit resolved mode replaces zero authored shear with the same collisional stress; trace geometry remains fixed and no extra momentum force is added.",
    "Constant or exponential temperature viscosity only; direct viscosity-design dependence is unsupported.",
    "Full state history is retained; no large-grid memory or temporal-convergence certification.",
    "Fixed-clear numerical material retains native regularization; it is not physical solid mass/storage.",
    "Explicit force-consistent initial SI pressure/velocities are opt-in; absent selection preserves legacy initialization. This is not steady-flow initialization.",
    "Optional reference-wall momentum exchange is reciprocal in material traction and discrete work, with explicit swept reference mass and pressure-datum loads. It is a small-displacement fixed-reference model, not finite-motion FSI or total-energy qualification.",
    "Current evaluator does not publish sealed acceptance/design-identity evidence; no provider acceptance operation is declared.",
];

const STATE: &[&str] = &["current_state"];
const INTERVAL: &[&str] = &["current_state", "previous_state", "design"];
const SMOOTH: &str = "smooth_within_selected_branch";

pub const LOCAL: [(&str, &str, &str, &str, &[&str], &str); 13] = [
    (
        "endpoint_mean_temperature_K",
        "K",
        "Arithmetic mean of all native temperature nodes.",
        "endpoint",
        STATE,
        SMOOTH,
    ),
    (
        "endpoint_mean_displacement_x_m",
        "m",
        "Arithmetic mean of native nodal x displacements.",
        "endpoint",
        STATE,
        SMOOTH,
    ),
    (
        "endpoint_mean_displacement_y_m",
        "m",
        "Arithmetic mean of native nodal y displacements.",
        "endpoint",
        STATE,
        SMOOTH,
    ),
    (
        "endpoint_mean_displacement_z_m",
        "m",
        "Arithmetic mean of native nodal z displacements.",
        "endpoint",
        STATE,
        SMOOTH,
    ),
    (
        "endpoint_max_temperature_K",
        "K",
        "Actual maximum of all native temperature nodes, including prescribed and numerical-material nodes; not a full-history or phase-only maximum.",
        "endpoint",
        STATE,
        "piecewise_smooth; JAX equal-share subgradient at tied maxima",
    ),
    (
        "design_solid_volume_m3",
        "m^3",
        "Physical solid occupancy integrated over the fixed cell grid; no numerical ghost-material floor is counted.",
        "time_invariant_design_derived",
        &["design"],
        SMOOTH,
    ),
    (
        "interval_inlet_mass_flow_kg_s",
        "kg/s",
        "Net mass entering the low-x port; signed, negative under reversal.",
        "interval",
        INTERVAL,
        SMOOTH,
    ),
    (
        "interval_outlet_mass_flow_kg_s",
        "kg/s",
        "Net mass leaving the high-x port; signed, negative under reversal.",
        "interval",
        INTERVAL,
        SMOOTH,
    ),
    (
        "interval_fluid_mass_rate_kg_s",
        "kg/s",
        "Actual conserved population-mass increment divided by the interval duration.",
        "interval",
        INTERVAL,
        SMOOTH,
    ),
    (
        "interval_advective_enthalpy_out_W",
        "W",
        "Net outward boundary caloric transport from the assembled donor-node operator, h=cp*T with zero-kelvin reference. Excludes pressure/kinetic fluxes.",
        "interval",
        INTERVAL,
        "piecewise_smooth_at_upwind_flow_reversal",
    ),
    (
        "interval_fluid_caloric_storage_W",
        "W",
        "Fluid caloric energy increment divided by duration, using the actual nodal quadrature. Not total coupled storage.",
        "interval",
        INTERVAL,
        SMOOTH,
    ),
    (
        "interval_port_gauge_pressure_work_W",
        "W",
        "Sum of prescribed port gauge pressure times signed inward volumetric flow, using the selected lattice EOS port density. Not pump shaft power or a total-energy certificate.",
        "interval",
        INTERVAL,
        SMOOTH,
    ),
    (
        "interval_drag_dissipation_W",
        "W",
        "Sum of the relative-drag heating assembled into the shared temperature state.",
        "interval",
        INTERVAL,
        SMOOTH,
    ),
];

pub const FIELD_NAMES: [&str; 3] = [
    "interval_boundary_mass_inflow_kg_s",
    "interval_boundary_enthalpy_inflow_W",
    "interval_fluid_mass_balance_residual_kg_s",
];

pub type HistoryDefinition = (&'static str, &'static str, &'static str, &'static str, &'static str);

const BASE: [HistoryDefinition; 12] = [
    (
        "history_max_temperature_K",
        "native_nodal_temperature",
        "maximum",
        "K",
        "Maximum of ALL stored native temperature nodes INCLUDING the prescribed initial state and numerical-material nodes. Not a bound between time levels.",
    ),
    (
        "history_min_inlet_mass_flow_kg_s",
        "interval_inlet_mass_flow_kg_s",
        "minimum",
        "kg/s",
        "Minimum signed net inlet flow across all solved intervals; negative under net inlet reversal. Not a pointwise boundary minimum.",
    ),
    (
        "history_min_outlet_mass_flow_kg_s",
        "interval_outlet_mass_flow_kg_s",
        "minimum",
        "kg/s",
        "Minimum signed net outlet flow across all solved intervals; negative under net outlet reversal. Not a pointwise boundary minimum.",
    ),
    (
        "history_inlet_transported_mass_kg",
        "interval_inlet_mass_flow_kg_s",
        "integral",
        "kg",
        "Signed net inlet mass integrated with the actual physical interval durations; reversal subtracts.",
    ),
    (
        "history_outlet_transported_mass_kg",
        "interval_outlet_mass_flow_kg_s",
        "integral",
        "kg",
        "Signed net outlet mass integrated with the actual physical interval durations; reversal subtracts.",
    ),
    (
        "history_advective_enthalpy_out_J",
        "interval_advective_enthalpy_out_W",
        "integral",
        "J",
        "Net outward boundary caloric energy over all intervals using h=cp*T. Mass drainage alone can produce a positive value. Excludes pressure and kinetic fluxes.",
    ),
    (
        "history_fluid_caloric_storage_J",
        "interval_fluid_caloric_storage_W",
        "integral",
        "J",
        "Sum of source-caloric interval energy increments. Not native-solid or complete coupled stored energy.",
    ),
    (
        "history_port_gauge_pressure_work_J",
        "interval_port_gauge_pressure_work_W",
        "integral",
        "J",
        "Integral of the original signed port gauge-pressure work. Not shaft work or a total kinetic-energy identity.",
    ),
    (
        "history_drag_dissipation_J",
        "interval_drag_dissipation_W",
        "integral",
        "J",
        "Integral of the relative-drag heat already assembled into the shared thermal residual. Not viscous heating.",
    ),
    (
        "history_fluid_caloric_demand_J",
        "interval_fluid_caloric_demand_W",
        "integral",
        "J",
        "Integral of the actual fluid caloric residual sum: storage plus outward transport. Positive requires energy supplied to the fluid caloric subsystem; includes shared-node/reservoir effects, not an isolated interface measurement.",
    ),
    (
        "history_mean_fluid_caloric_demand_W",
        "interval_fluid_caloric_demand_W",
        "time_mean",
        "W",
        "Duration-weighted mean of fluid caloric demand; not the unweighted mean of time-step values and not automatically heat removed from the solid.",
    ),
    (
        "history_max_abs_fluid_mass_rate_kg_s",
        "interval_fluid_mass_rate_kg_s",
        "maximum_absolute",
        "kg/s",
        "Largest absolute fluid mass-storage rate across all intervals. Describes compressible transients; not a mass-conservation residual.",
    ),
];

pub const WORK: [HistoryDefinition; 6] = [
    (
        "history_fluid_drag_work_J",
        "interval_fluid_drag_work_W",
        "integral",
        "J",
        "Signed work of the actual fluid relative-drag force at its force-corrected interval velocity. Not an additional caloric source.",
    ),
    (
        "history_solid_drag_work_J",
        "interval_solid_drag_work_W",
        "integral",
        "J",
        "Signed work of the actual nodal drag load at the displacement-increment velocity, including prescribed nodes. Paired fluid work plus this work plus drag heat sums to zero within arithmetic error.",
    ),
    (
        "history_pressure_trace_solid_work_J",
        "interval_pressure_trace_solid_work_W",
        "integral",
        "J",
        "Signed work of the actual fixed pressure-trace load, including any authored shear, at the nodal displacement-increment velocity. Includes prescribed nodes. No reciprocal moving-wall fluid work is assembled.",
    ),
    (
        "history_viscous_trace_solid_work_J",
        "interval_viscous_trace_solid_work_W",
        "integral",
        "J",
        "Signed work of the selected resolved viscous nodal traction at the displacement-increment velocity, including prescribed nodes. Requires trace_traction=true. Not reciprocal moving-wall FSI.",
    ),
    (
        "history_viscous_dissipation_J",
        "interval_viscous_dissipation_W",
        "integral",
        "J",
        "Integral of constitutive viscous heat actually selected in the shared residual. Requires heating=true. Not the exact finite-step loss of fluid kinetic energy.",
    ),
    (
        "history_pressure_porosity_work_J",
        "interval_pressure_porosity_work_W",
        "integral",
        "J",
        "Work of the actual algebraic pressure-porosity correction in the source collision. Not port work, physical interface work, or an additional heat source.",
    ),
];

pub const WALL: [HistoryDefinition; 6] = [
    (
        "history_wall_fluid_traction_work_J",
        "interval_wall_fluid_traction_work_W",
        "integral",
        "J",
        "Work on fluid of the actual population-wall material traction, with the moving-mass momentum contribution removed. Uses the same wall velocity as solid transfer. Small-displacement reference domain only, not additional heat.",
    ),
    (
        "history_wall_solid_traction_work_J",
        "interval_wall_solid_traction_work_W",
        "integral",
        "J",
        "Work on the solid of kinetic wall momentum exchange, including prescribed degrees of freedom. Opposite to fluid traction work. Excludes the separately authored pressure-datum/exterior load.",
    ),
    (
        "history_wall_pressure_datum_work_J",
        "interval_wall_pressure_datum_work_W",
        "integral",
        "J",
        "Work of the separate load replacing the lattice reference pressure by the authored physical reference and optional exterior pressure. External reference-load accounting, not another fluid force or heat source.",
    ),
    (
        "history_wall_total_solid_work_J",
        "interval_wall_total_solid_work_W",
        "integral",
        "J",
        "Work of the actual total wall load assembled into the native solid, kinetic traction plus pressure-datum/exterior load, including prescribed degrees of freedom.",
    ),
    (
        "history_wall_reference_mass_kg",
        "interval_wall_reference_mass_rate_kg_s",
        "integral",
        "kg",
        "Signed reference-domain mass term due to normal wall kinematics. Not real reservoir flow through a material wall and not an exact finite-motion geometric conservation law.",
    ),
    (
        "history_wall_reference_caloric_exchange_J",
        "interval_wall_reference_caloric_power_W",
        "integral",
        "J",
        "Local cp*T transport paired with the reference-domain wall mass term. Uses previous native nodal donor temperatures, not reservoir temperature or mechanical-work heating.",
    ),
];

pub const STAGES: [(&str, &str); 8] = [
    ("raw_population_kinetic_change", "whole interval, actual raw-population endpoint kinetic increment"),
    ("collision_raw_kinetic_change", "actual collision-stage raw macro kinetic increment"),
    (
        "streaming_raw_kinetic_change",
        "raw macro kinetic increment under open streaming before boundary closure",
    ),
    (
        "wall_population_kinetic_change",
        "raw macro kinetic increment under reference-wall population return, zero when absent",
    ),
    (
        "port_population_kinetic_change",
        "raw macro kinetic increment under the remaining pressure-port closures",
    ),
    (
        "population_solution_defect_kinetic_change",
        "kinetic effect of actual populations minus the expected interval update, not a heat source",
    ),
    (
        "streaming_beam_outflow",
        "outgoing lattice beam second moment at open boundaries, NOT a physical enthalpy or macroscopic kinetic flux",
    ),
    (
        "streaming_kinetic_redistribution",
        "streaming raw macro kinetic increment plus escaped beam moment, not nonnegative viscous dissipation",
    ),
];

#[derive(Clone, Debug)]
pub struct History {
    pub name: String,
    pub source: String,
    pub reduction: &'static str,
    pub unit: &'static str,
    pub description: String,
}

#[must_use]
pub fn history() -> Vec<History> {
    let own = |d: &HistoryDefinition| History {
        name: d.0.to_string(),
        source: d.1.to_string(),
        reduction: d.2,
        unit: d.3,
        description: d.4.to_string(),
    };
    let mut out: Vec<History> = BASE.iter().chain(&WORK).chain(&WALL).map(own).collect();
    for (name, description) in STAGES {
        out.push(History {
            name: format!("history_{name}_J"),
            source: format!("interval_{name}_W"),
            reduction: "integral",
            unit: "J",
            description: format!(
                "{description}. Integrated on the actual physical history. Raw velocity excludes endpoint half-force correction. Not total-energy qualification."
            ),
        });
    }
    out
}

#[must_use]
pub fn history_definition(name: &str) -> Option<History> {
    history().into_iter().find(|h| h.name == name)
}

#[must_use]
pub fn local_metadata(row: &(&str, &str, &str, &str, &[&str], &str)) -> Value {
    json!({"unit": row.1, "description": row.2, "temporal_association": row.3,
        "dependencies": row.4, "derivative_scope": "complete_discrete_state_previous_state_and_design_chain",
        "differentiable": true, "design_reachable": true, "smoothness": row.5})
}

#[must_use]
pub fn history_metadata(h: &History) -> Value {
    let smoothness = if ["maximum", "minimum", "maximum_absolute"].contains(&h.reduction) {
        "piecewise; equal-share generalized derivative at tied stored samples"
    } else {
        "smooth_within_selected_state_and_upwind_branch"
    };
    json!({"unit": h.unit, "description": h.description, "temporal_association": "whole_stored_history",
        "source_quantity": h.source, "reduction": h.reduction, "selection": "problem.history_responses",
        "dependencies": ["all_relevant_states", "design", "design_dependent_initial_state"],
        "derivative_scope": "complete_discrete_history_and_initial_state_chain",
        "differentiable": true, "design_reachable": true, "smoothness": smoothness})
}

#[must_use]
pub fn response_metadata() -> Map<String, Value> {
    let mut m = Map::new();
    for row in &LOCAL {
        m.insert(row.0.to_string(), local_metadata(row));
    }
    for h in history() {
        m.insert(h.name.clone(), history_metadata(&h));
    }
    m
}

#[must_use]
pub fn responses() -> Vec<String> {
    LOCAL.iter().map(|r| r.0.to_string()).chain(history().into_iter().map(|h| h.name)).collect()
}

#[must_use]
pub fn is_local(name: &str) -> bool {
    LOCAL.iter().any(|r| r.0 == name)
}

#[must_use]
pub fn is_transport(name: &str) -> bool {
    LOCAL.iter().any(|r| r.0 == name && r.3 == "interval")
}

#[must_use]
pub fn missing_local(wall: Option<&Value>) -> Vec<&'static str> {
    match wall {
        None => Vec::new(),
        Some(w) if w["face"] == json!("x_min") => vec!["interval_inlet_mass_flow_kg_s"],
        Some(_) => vec!["interval_outlet_mass_flow_kg_s"],
    }
}

#[must_use]
pub fn family_sources(family: &[HistoryDefinition]) -> Vec<&'static str> {
    family.iter().map(|d| d.1).collect()
}

#[must_use]
pub fn energy_sources() -> Vec<String> {
    STAGES.iter().map(|(n, _)| format!("interval_{n}_W")).collect()
}
