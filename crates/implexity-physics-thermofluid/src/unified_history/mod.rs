// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub mod audit;
mod diagnostics;
mod dual_volume;
pub mod kernel;
pub mod observers;
pub mod provider;
mod responses;
pub mod wall_film;

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use implexity_core::pyobj::list_repr;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_cfd::pyfmt::fmt_g;

pub mod split_history;
pub mod split_snapshot;

pub use dual_volume::FluidNodalDualVolume;
pub use kernel::{SolveContext, UnifiedKernel, build_kernel, kernel};
pub use provider::NativeUnifiedHistoryProvider;

pub const NAME: &str = "native_unified_history";
pub const COORDS: [&str; 3] = ["model:control", "model:parameters", "model:spatial_fields"];
pub const STATIONARY_CONTINUATION_KEY: &str = "stationary_continuation";
pub const MAX_DISPLACEMENT_OVER_CELL: f64 = 0.25;
pub const VALIDITY_KEYS: [&str; 5] = [
    "max_displacement_over_cell",
    "min_fluid_feature_cells",
    "max_relative_channel_width_change",
    "solid_reporting_threshold",
    "fluid_reporting_threshold",
];
pub const STAGED_THERMAL_TO_FLOW: &str =
    "native_unified_history.thermal_to_flow.temperature_dependent_fluid_laws";
pub const STAGED_FLOW_TO_THERMAL: &str = "native_unified_history.flow_to_thermal.shared_enthalpy_and_work";
pub const STAGED_COUPLING_IDS: [&str; 2] = [STAGED_THERMAL_TO_FLOW, STAGED_FLOW_TO_THERMAL];
pub const RESPONSES: [&str; 11] = [
    "unified_plastic_strain",
    "unified_creep_strain",
    "unified_inelastic_heat_J",
    "unified_temperature_peak_K",
    "unified_solid_mass_kg",
    "unified_elastic_energy_J",
    "unified_fluid_temperature_p12_K",
    "unified_hydraulic_power_W",
    "unified_mass_flow_kg_s",
    "unified_solid_volume_m3",
    "unified_first_state_compliance_J",
];
pub const UNITS: [&str; 11] = ["1", "1", "J", "K", "kg", "J", "K", "W", "kg/s", "m^3", "J"];
pub const TRANSPORT_BOUNDARY_LEDGER_RELATIVE_TOLERANCE: f64 = 1.0e-10;
pub const HEAT_CONTINUATION_MAX_BISECTIONS: usize = 4;
pub const BASE_HISTORY_SAMPLES: [&str; 5] = [
    "pressure_absolute_Pa",
    "fluid_temperature_K",
    "phase_cell_temperature_upper_K",
    "fluid_fraction",
    "shared_nodal_temperature_K",
];
pub const MATERIAL_HISTORY_SAMPLES: [&str; 4] = [
    "material_stored_energy_J_m3",
    "material_conductivity_W_mK",
    "material_yield_stress_Pa",
    "solid_cell_volume_m3",
];
pub const KINDS: [(&str, &str); 3] = [
    ("solid", "solid_history_field"),
    ("fluid", "fluid_history_field"),
    ("phase_load", "shared_phase_stress_load"),
];
pub const LIMITATIONS: [&str; 11] = [
    "Shared designable solid/fluid domain; continuous density projection, ersatz stiffness and finite Brinkman permeability.",
    "Perfect thermal contact through one shared temperature; no contact resistance or two-temperature porous-medium model.",
    "Density-jump total-stress loading; load interpolation/mesh/permeability convergence is not automatically established.",
    "Fixed spatial kinematics, small-strain T4 inelastic solid; no ALE, moving-wall work or large-deformation feedback.",
    "Resolved laminar incompressible flow, user-defined validity bounds; no turbulence, boiling, CHF/DNBR.",
    "Solid/fluid caloric storage, authored volumetric heat and integrated fluid work-to-heat use conservative row-sum-lumped parity-Kuhn T4 nodal dual volumes; fluid face/boundary advection and conduction use trilinear cell-centre temperatures and the transpose power transfer. Diffusion is energy-stable at frozen coefficients; no nodal discrete maximum principle is claimed. Mesh/time sensitivity remains required; parity-Kuhn solid reflection equivariance requires even cell counts.",
    "The optimized shared-temperature response is a smooth order-32 power mean; literal nodal maxima remain material/liquid guards and are reported separately.",
    "Optional fixed-group neutral transport can drive deposition and declared material kinetics; nuclear data and calibration required. No depletion/transmutation, erosion, recrystallisation, fracture or calibrated cyclic lifetime.",
    "Intermediate densities are a numerical relaxation, not validated manufactured porous material.",
    "Isolated pockets and disconnected solids require physical connectivity/regularisation studies; no pressure drainage is silently added.",
    "An optional authored Helmholtz density filter imposes a length scale on the phase control; it does not guarantee a minimum wall thickness or sharp-interface manufacturing feasibility; no production qualification.",
];

#[derive(Debug, Clone, Copy)]
pub struct ResponseMeta {
    pub name: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub family: &'static str,
    pub description: &'static str,
}

pub const RESPONSE_METADATA: [ResponseMeta; 37] = [
    ResponseMeta {
        name: "shared_temperature_max_K",
        label: "Maximum shared nodal temperature",
        unit: "K",
        family: "thermal",
        description: "Literal maximum over selected stored nodal samples; nonsmooth at ties; includes numerical replacement regions.",
    },
    ResponseMeta {
        name: "shared_temperature_upper_bound_K",
        label: "Conservative shared-temperature bound",
        unit: "K",
        family: "thermal",
        description: "Unnormalized log-sum-exp upper bound on the maximum over selected nodal samples; explicit smoothing width in kelvin.",
    },
    ResponseMeta {
        name: "unified_plastic_strain",
        label: "Equivalent plastic strain",
        unit: "1",
        family: "mechanics",
        description: "Solid-volume-weighted mean equivalent plastic strain at the final stored history state.",
    },
    ResponseMeta {
        name: "unified_creep_strain",
        label: "Equivalent creep strain",
        unit: "1",
        family: "mechanics",
        description: "Solid-volume-weighted mean equivalent creep strain at the final stored history state.",
    },
    ResponseMeta {
        name: "unified_inelastic_heat_J",
        label: "Accumulated inelastic heat",
        unit: "J",
        family: "thermal",
        description: "Total density-weighted inelastic heat accumulated over all post-initial history increments.",
    },
    ResponseMeta {
        name: "unified_temperature_peak_K",
        label: "Smooth shared-temperature metric",
        unit: "K",
        family: "thermal",
        description: "Normalized order-32 power mean over shared nodal temperatures at all post-initial history states; the literal nodal maximum remains a separate hard material and liquid guard.",
    },
    ResponseMeta {
        name: "unified_solid_mass_kg",
        label: "Solid mass",
        unit: "kg",
        family: "mass",
        description: "Total interpolated solid mass at the final stored history state.",
    },
    ResponseMeta {
        name: "unified_elastic_energy_J",
        label: "Elastic strain energy",
        unit: "J",
        family: "mechanics",
        description: "Total density-weighted elastic strain energy at the final stored history state.",
    },
    ResponseMeta {
        name: "unified_fluid_temperature_p12_K",
        label: "Fluid-temperature p12 metric",
        unit: "K",
        family: "thermal",
        description: "Maximum over post-initial history of the fluid-fraction-weighted normalized order-12 cell-temperature mean sampled by the transport map.",
    },
    ResponseMeta {
        name: "unified_hydraulic_power_W",
        label: "Hydraulic power",
        unit: "W",
        family: "fluid",
        description: "Final-state pressure difference multiplied by the mean inlet and outlet resolved volumetric flow rate.",
    },
    ResponseMeta {
        name: "unified_mass_flow_kg_s",
        label: "Mass flow rate",
        unit: "kg/s",
        family: "fluid",
        description: "Final-state outlet volumetric flow rate multiplied by the declared fluid mass density.",
    },
    ResponseMeta {
        name: "unified_solid_volume_m3",
        label: "Solid volume",
        unit: "m^3",
        family: "geometry",
        description: "Total continuous solid occupancy multiplied by the authored physical cell volume.",
    },
    ResponseMeta {
        name: "unified_first_state_compliance_J",
        label: "First-state load compliance",
        unit: "J",
        family: "mechanics",
        description: "External-load work f.u (authored face tractions and nodal forces times nodal displacements) at the first stored post-initial state. Authored with a short first step before appreciable heating, it is the structural compliance under the design load, i.e. inverse stiffness; thermal expansion contributes to u as well.",
    },
    ResponseMeta {
        name: "material_stored_energy_J",
        label: "Stored material energy",
        unit: "J",
        family: "material",
        description: "Final stored constitutive-history energy integrated over the solid volume.",
    },
    ResponseMeta {
        name: "material_conductivity_mean_W_mK",
        label: "Mean material thermal conductivity",
        unit: "W/(m K)",
        family: "material",
        description: "Final solid-volume-weighted mean thermal conductivity from the selected material history.",
    },
    ResponseMeta {
        name: "material_yield_mean_Pa",
        label: "Mean material yield stress",
        unit: "Pa",
        family: "material",
        description: "Final solid-volume-weighted mean yield stress from the selected material history.",
    },
    ResponseMeta {
        name: "fluid_subcooling_bound_K",
        label: "Fluid subcooling bound",
        unit: "K",
        family: "phase admissibility",
        description: "Conservative differentiable all-history lower bound on saturation temperature minus solved fluid temperature; this is not a boiling or CHF model.",
    },
    ResponseMeta {
        name: "phase_cell_nodal_subcooling_bound_K",
        label: "Nodal subcooling bound",
        unit: "K",
        family: "phase admissibility",
        description: "Conservative differentiable all-history lower bound on saturation temperature minus the phase-cell nodal upper-temperature bound; this is not a wetted-wall or CHF result.",
    },
    ResponseMeta {
        name: "prescribed_joule_power_W",
        label: "Prescribed-field Joule heating power",
        unit: "W",
        family: "thermal source",
        description: "Final whole-domain heating power from prescribed real cell-attached electric fields and effective conductivity. This is not a solved electrical-field or charge-conservation result.",
    },
    ResponseMeta {
        name: "prescribed_volumetric_heating_final_W",
        label: "Prescribed volumetric heating power (final)",
        unit: "W",
        family: "thermal source",
        description: "Whole-domain power at the final host time of the prescribed material-resolved volumetric heating: authored densities per solid endmember and fluid, mixed by occupancy and phase fraction, with the optional depth attenuation and time factor. Not a radiation-transport result.",
    },
    ResponseMeta {
        name: "prescribed_volumetric_heating_energy_J",
        label: "Prescribed volumetric heating energy",
        unit: "J",
        family: "thermal source",
        description: "Backward-Euler endpoint time integral (sum of step power times step length, as in the host thermal history) of the prescribed volumetric heating power over all post-initial steps.",
    },
    ResponseMeta {
        name: "resolved_joule_power_W",
        label: "Resolved Joule heating power",
        unit: "W",
        family: "electrothermal",
        description: "Final native-domain Joule heating from the electrical potential and temperature-dependent effective conductivity.",
    },
    ResponseMeta {
        name: "electrical_terminal_power_W",
        label: "Electrical terminal input power",
        unit: "W",
        family: "electrothermal",
        description: "Final sum of prescribed electrode voltage times electrode current into the domain; charge/power validation is required.",
    },
    ResponseMeta {
        name: "neutral_local_deposition_W",
        label: "Neutral-particle deposited power",
        unit: "W",
        family: "neutral transport",
        description: "Final total local neutral-particle energy deposition integrated over the modeled domain.",
    },
    ResponseMeta {
        name: "neutral_outgoing_particles_per_s",
        label: "Outgoing neutral-particle rate",
        unit: "1/s",
        family: "neutral transport",
        description: "Final total neutral-particle rate leaving the modeled transport boundary.",
    },
    ResponseMeta {
        name: "neutral_solid_damage_rate_mean_dpa_s",
        label: "Mean solid damage rate",
        unit: "dpa/s",
        family: "neutral transport",
        description: "Final solid-occupancy-weighted mean endpoint damage rate from the selected neutral-transport source.",
    },
    ResponseMeta {
        name: "electromagnetic_force_peak_N",
        label: "Peak electromagnetic force",
        unit: "N",
        family: "electromagnetic",
        description: "Normalised order-p mean over post-initial history steps of the net Lorentz force magnitude; bounded by the largest stepwise net force and approaching it as p grows. Identically zero without terminals (insulated conductors carry no net force).",
    },
    ResponseMeta {
        name: "electromagnetic_impulse_x_N_s",
        label: "Electromagnetic impulse (x)",
        unit: "N s",
        family: "electromagnetic",
        description: "Backward-Euler time integral over the history of the x component of the net Lorentz force; identically zero without terminals.",
    },
    ResponseMeta {
        name: "electromagnetic_impulse_y_N_s",
        label: "Electromagnetic impulse (y)",
        unit: "N s",
        family: "electromagnetic",
        description: "Backward-Euler time integral over the history of the y component of the net Lorentz force; identically zero without terminals.",
    },
    ResponseMeta {
        name: "electromagnetic_impulse_z_N_s",
        label: "Electromagnetic impulse (z)",
        unit: "N s",
        family: "electromagnetic",
        description: "Backward-Euler time integral over the history of the z component of the net Lorentz force; identically zero without terminals.",
    },
    ResponseMeta {
        name: "electromagnetic_torque_peak_N_m",
        label: "Peak electromagnetic torque",
        unit: "N m",
        family: "electromagnetic",
        description: "Normalised order-p mean over post-initial history steps of the magnitude of the Lorentz torque about the authored moment reference point; independent of that point without terminals.",
    },
    ResponseMeta {
        name: "electromagnetic_angular_impulse_x_N_m_s",
        label: "Electromagnetic angular impulse (x)",
        unit: "N m s",
        family: "electromagnetic",
        description: "Backward-Euler time integral of the x component of the Lorentz torque about the authored moment reference point.",
    },
    ResponseMeta {
        name: "electromagnetic_angular_impulse_y_N_m_s",
        label: "Electromagnetic angular impulse (y)",
        unit: "N m s",
        family: "electromagnetic",
        description: "Backward-Euler time integral of the y component of the Lorentz torque about the authored moment reference point.",
    },
    ResponseMeta {
        name: "electromagnetic_angular_impulse_z_N_m_s",
        label: "Electromagnetic angular impulse (z)",
        unit: "N m s",
        family: "electromagnetic",
        description: "Backward-Euler time integral of the z component of the Lorentz torque about the authored moment reference point.",
    },
    ResponseMeta {
        name: "electromagnetic_force_density_pnorm_N_m3",
        label: "Lorentz force-density p-norm",
        unit: "N/m^3",
        family: "electromagnetic",
        description: "Normalised order-p mean of the local Lorentz force-density magnitude |J x B| over all post-initial steps and native tetrahedra; bounded by its maximum and approaching it as p grows.",
    },
    ResponseMeta {
        name: "electromagnetic_current_density_pnorm_A_m2",
        label: "Current-density p-norm",
        unit: "A/m^2",
        family: "electromagnetic",
        description: "Normalised order-p mean of the current-density magnitude over all post-initial steps and native tetrahedra; bounded by its maximum and approaching it as p grows.",
    },
    ResponseMeta {
        name: "electromagnetic_joule_energy_J",
        label: "Electromagnetic Joule energy",
        unit: "J",
        family: "electromagnetic",
        description: "Backward-Euler time integral of the total Joule dissipation of eddy and injected currents over the history.",
    },
];

#[must_use]
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn num(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(f64::NAN)
}

#[must_use]
pub fn fixed_geometry_validity(
    displacement_nodes: &[[f64; 3]],
    spacing_m: &[f64; 3],
    occupancy: &[f64],
    tetrahedra: &[[usize; 4]],
    owners: &[usize],
    validity: &Value,
) -> Value {
    let h_min = spacing_m.iter().copied().fold(f64::INFINITY, f64::min);
    let threshold = num(validity, "solid_reporting_threshold");
    let mut solid_nodes = vec![false; displacement_nodes.len()];
    for (tet, owner) in tetrahedra.iter().zip(owners) {
        if occupancy[*owner] >= threshold {
            for node in tet {
                solid_nodes[*node] = true;
            }
        }
    }
    let magnitude = |u: &[f64; 3]| (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    let umax = displacement_nodes
        .iter()
        .zip(&solid_nodes)
        .filter(|(_, s)| **s)
        .map(|(u, _)| magnitude(u))
        .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| if v > a || v.is_nan() { v } else { a })))
        .unwrap_or(0.0);
    let over_cell = umax / h_min;
    let feature = num(validity, "min_fluid_feature_cells") * h_min;
    let width_change = 2.0 * umax / feature;
    let cell_limit = num(validity, "max_displacement_over_cell");
    let width_limit = num(validity, "max_relative_channel_width_change");
    json!({
        "maximum_displacement_over_cell": over_cell,
        "maximum_displacement_over_cell_limit": cell_limit,
        "displacement_over_cell_screen_passed": over_cell <= cell_limit,
        "maximum_relative_channel_width_change": width_change,
        "maximum_relative_channel_width_change_limit": width_limit,
        "minimum_fluid_feature_m": feature,
        "channel_width_screen_passed": width_change <= width_limit,
        "maximum_solid_displacement_m": umax,
        "screened_solid_nodes": solid_nodes.iter().filter(|s| **s).count(),
        "displacement_screen_scope": "nodes_of_cells_at_or_above_solid_reporting_threshold",
    })
}

#[must_use]
pub fn fixed_geometry_failure_message(report: &Value, history_step: usize) -> String {
    let mut failed = Vec::new();
    let f = |k: &str| report[k].as_f64().unwrap_or(f64::NAN);
    if report["displacement_over_cell_screen_passed"] != json!(true) {
        failed.push(format!(
            "max solid |u| {} m = {} of the smallest cell, bound max_displacement_over_cell={}",
            fmt_g(f("maximum_solid_displacement_m"), 4),
            fmt_g(f("maximum_displacement_over_cell"), 4),
            fmt_g(f("maximum_displacement_over_cell_limit"), 6)
        ));
    }
    if report["channel_width_screen_passed"] != json!(true) {
        failed.push(format!(
            "two facing walls moving {} m change the narrowest declared fluid feature ({} m) by {} of its width, bound max_relative_channel_width_change={}",
            fmt_g(f("maximum_solid_displacement_m"), 4),
            fmt_g(f("minimum_fluid_feature_m"), 4),
            fmt_g(f("maximum_relative_channel_width_change"), 4),
            fmt_g(f("maximum_relative_channel_width_change_limit"), 6)
        ));
    }
    format!(
        "unified fixed-geometry approximation outside displacement validity: history step {history_step}, screened {} solid nodes; {}",
        report["screened_solid_nodes"],
        failed.join("; ")
    )
}


pub fn selected(
    name: &str,
    kind: &str,
) -> CaeResult<std::sync::Arc<dyn implexity_core::orchestration::AddInAdapter>> {
    let adapter = implexity_core::registries::global()
        .addins
        .get(name)
        .ok()
        .and_then(|row| row.adapter.clone())
        .ok_or_else(|| CaeError::contract(format!("inactive shared-domain component {name}")))?;
    if adapter.component_kind().as_deref() != Some(kind) {
        return contract(format!("{name}: incompatible {kind}"));
    }
    Ok(adapter)
}

#[must_use]
pub fn selected_limitations(problem: &Value) -> Vec<String> {
    let transport = problem.get("temperature_transport").and_then(Value::as_str);
    let nodal = transport.is_some_and(|t| crate::nodal_transport::PROFILES.contains(&t));
    LIMITATIONS
        .iter()
        .map(|row| {
            if nodal && row.starts_with("Solid/fluid caloric storage,") {
                let smooth = if transport == Some(crate::nodal_transport::SMOOTH_PROFILE) {
                    " (C1-smoothed upwind split with the authored smoothing velocity). "
                } else {
                    ". "
                };
                format!(
                    "Caloric and work-to-heat terms retain T4 nodal dual volumes. Fluid transport uses two-point nodal diffusion and first-order upwind enthalpy from the same MAC velocity{smooth}Fixed-coefficient sign properties do not establish a nonlinear coupled maximum principle; mesh/time and numerical-diffusion studies remain required."
                )
            } else {
                (*row).to_string()
            }
        })
        .collect()
}


pub fn normalise_density_filter(raw: &Value) -> CaeResult<Value> {
    let keys = ["method", "provenance", "radius_mm"];
    let Some(m) = raw.as_object().filter(|m| m.len() == 3 && keys.iter().all(|k| m.contains_key(*k))) else {
        return contract("phase_map.density_filter requires exactly method, radius_mm and provenance");
    };
    if m["method"].as_str() != Some("helmholtz") {
        return contract("phase_map.density_filter.method must be 'helmholtz'");
    }
    let r = match &m["radius_mm"] {
        Value::Number(n) => n.as_f64().filter(|r| r.is_finite() && *r > 0.0),
        _ => None,
    };
    let Some(r) = r else {
        return contract("phase_map.density_filter.radius_mm must be a finite positive length in mm");
    };
    if m["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return contract("phase_map.density_filter.provenance must state why this length scale was chosen");
    }
    Ok(json!({"method": "helmholtz", "radius_mm": r, "provenance": m["provenance"]}))
}

fn finite_scalar(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn numbers_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| numbers_equal(p, q))
        }
        (Value::Number(_), Value::Number(_)) => a.as_f64() == b.as_f64(),
        _ => a == b,
    }
}


pub fn source_catalog() -> CaeResult<implexity_core::field_source_catalog::FieldSourceCatalog> {
    implexity_core::field_source_catalog::load_field_source_catalog(implexity_core::distributions::global())
}


#[allow(clippy::too_many_lines)]
pub fn normalise(problem: &Value) -> CaeResult<Value> {
    if problem.as_object().is_some_and(|m| m.contains_key("volume_constraint")) {
        return contract(
            "unified history no longer accepts volume_constraint: hard constraints were removed from Implexity and the physical-volume projection no longer exists. Remove the key and declare the response 'unified_solid_volume_m3' with sense 'upper', 'lower' or 'equal' as a penalty term",
        );
    }
    let required = ["name", "components", "solid", "fluid", "phase_map", "validity", "numerics"];
    let optional = [
        "history_observers",
        "operator_split_history",
        "numerical_initial_guess",
        "connectivity",
        "field_sources",
        "temperature_transport",
        "temperature_transport_smoothing_velocity_m_s",
        "initialization",
        "numerical_fallback",
        "applicability_policy",
        wall_film::KEY,
        STATIONARY_CONTINUATION_KEY,
    ];
    let Some(m) = problem.as_object() else {
        return contract(
            "unified history requires name, components, solid, fluid, phase_map, validity and numerics",
        );
    };
    let remaining: BTreeSet<&str> = m.keys().map(String::as_str).filter(|k| !optional.contains(k)).collect();
    if remaining != required.iter().copied().collect() {
        return contract(
            "unified history requires name, components, solid, fluid, phase_map, validity and numerics",
        );
    }
    let mut p = problem.clone();
    if let Some(card) = p.get("numerical_initial_guess").filter(|c| !c.is_null()) {
        implexity_core::contracts::SourceBoundNumericalGuess::from_value(card)?;

    }
    if let Some(card)=p.get("operator_split_history").filter(|c|!c.is_null()) {
        let fields:BTreeSet<&str>=card.as_object().map(|m|m.keys().map(String::as_str).collect()).unwrap_or_default();
        let required:BTreeSet<&str>=["schema","local_substeps","coupling_sweeps","provenance"].into_iter().collect();
        let aged=(card["schema"]=="implexity-frozen-reference-aged-condition/1" || card["schema"]=="implexity-prescribed-stress-relaxing-aged-condition/1");
        let aged_keys:BTreeSet<&str>=["schema","provenance"].into_iter().collect();
        if (aged && fields!=aged_keys) || (!aged && (fields!=required || card["schema"]!="implexity-operator-split-history/1" || card["local_substeps"].as_u64().is_none_or(|n|n==0) || !matches!(card["coupling_sweeps"].as_u64(),Some(1|2)))) || card["provenance"].as_str().is_none_or(|s|s.trim().is_empty()) {return contract("invalid explicitly authored approximate history or aged condition card");}
        if aged && (p["solid"]["times_s"].as_array().is_none_or(|times|times.len()!=2 || times[0]!=0.) || p["fluid"]["times_s"]!=p["solid"]["times_s"]) {return contract("aged condition requires exactly initial and positive-age observation times");}
        if p.get(STATIONARY_CONTINUATION_KEY).is_some_and(|v|!v.is_null()) || p.get("numerical_fallback").is_some_and(|v|v!="none") {return contract("operator_split_history cannot combine final approximate history with initialization-only continuation");}
    }

    let applicability = p.get("applicability_policy").and_then(Value::as_str).unwrap_or("enforce").to_string();
    if !matches!(applicability.as_str(), "enforce" | "report_only")
        || p.get("applicability_policy").is_some_and(|v| !v.is_string())
    {
        return contract("applicability_policy must be enforce or report_only");
    }
    if p.get("applicability_policy").is_some() {
        p["solid"]["applicability_policy"] = json!(applicability);
        p["fluid"]["applicability_policy"] = json!(applicability);
    }
    let fallback = p.get("numerical_fallback").and_then(Value::as_str).unwrap_or("none");
    if !matches!(fallback, "none" | "heat_continuation")
        || p.get("numerical_fallback").is_some_and(|v| !v.is_string())
    {
        return contract("numerical_fallback must be none or explicitly authored heat_continuation");
    }
    let transport = match p.get("temperature_transport") {
        None => "trilinear_cell_average_v1".to_string(),
        Some(v) => v.as_str().unwrap_or("<invalid>").to_string(),
    };
    if transport != "trilinear_cell_average_v1"
        && !crate::nodal_transport::PROFILES.contains(&transport.as_str())
    {
        return contract("unsupported shared-temperature transport discretization; re-author and preflight");
    }
    p["temperature_transport"] = json!(transport);
    let smoothing = p.get("temperature_transport_smoothing_velocity_m_s").filter(|v| !v.is_null()).cloned();
    if (transport == crate::nodal_transport::SMOOTH_PROFILE) != smoothing.is_some() {
        return contract(format!(
            "{} requires temperature_transport_smoothing_velocity_m_s, and no other transport accepts it",
            crate::nodal_transport::SMOOTH_PROFILE
        ));
    }
    if let Some(s) = &smoothing
        && finite_scalar(s).is_none_or(|v| v <= 0.0)
    {
        return contract("temperature_transport_smoothing_velocity_m_s must be a finite positive speed");
    }
    if let Some(init) = p.get("initialization").cloned() {
        p["initialization"] = crate::elastic_preload::normalise_initialization(&init)?;
    }
    if let Some(card) = p.get(wall_film::KEY).filter(|v| !v.is_null()) {
        wall_film::validate(card)?;
        if !crate::nodal_transport::PROFILES.contains(&transport.as_str()) {
            return contract("wall_film requires a nodal temperature transport profile (nodal_dual_*)");
        }
    }
    let components = p["components"].as_object().cloned().unwrap_or_default();
    let names: BTreeSet<&str> = components.keys().map(String::as_str).collect();
    if !p["components"].is_object() || names != KINDS.iter().map(|(k, _)| *k).collect() {
        return contract("explicit shared-domain field and phase-load selections required");
    }
    for (key, kind) in KINDS {
        selected(components[key].as_str().unwrap_or_default(), kind)?;
    }
    let solid_name = components["solid"].as_str().unwrap_or_default();
    let fluid_name = components["fluid"].as_str().unwrap_or_default();
    kernel::solid_factory(solid_name)?;
    p["solid"] = implexity_physics_solid::solid_history::SolidHistoryFactory::validate(&p["solid"])?;
    p["fluid"] =
        crate::incompressible_transport::selected_fluid_factory(fluid_name)?.validate(&p["fluid"])?;
    let (s, f) = (&p["solid"], &p["fluid"]);
    if !numbers_equal(&s["grid"], &f["grid"]) || !numbers_equal(&s["times_s"], &f["times_s"]) {
        return contract("both fields require the SAME full domain grid and loading times");
    }
    let cells: u64 = s["grid"].as_array().map_or(0, |g| g.iter().filter_map(Value::as_u64).product());
    if cells < 2 {
        return contract("shared-domain stress reconstruction requires at least two cells");
    }
    if !numbers_equal(&s["temperature_initial_K"], &f["initial_temperature_K"]) {
        return contract("shared temperature requires one stress-free reference temperature");
    }
    if f["boundaries"].as_array().into_iter().flatten().any(|b| b["thermal"] == "interface") {
        return contract("fixed fluid/solid interface faces are forbidden in a shared-domain problem");
    }
    let pm = p["phase_map"].clone();
    let pm_keys: BTreeSet<&str> = pm
        .as_object()
        .map(|o| o.keys().map(String::as_str).filter(|k| *k != "density_filter").collect())
        .unwrap_or_default();
    let provenance_ok =
        pm.get("provenance").is_some_and(|v| !implexity_core::pyobj::py_str(v).trim().is_empty());
    if !pm.is_object() || pm_keys != ["projection_beta", "provenance"].into_iter().collect() || !provenance_ok
    {
        return contract("explicit phase projection and provenance required");
    }
    implexity_geometry::phase_partition::ComplementaryPhaseMap::new(
        pm["projection_beta"].as_f64().unwrap_or(f64::NAN),
    )
    .map_err(|e| CaeError::contract(e.to_string()))?;
    if let Some(filter) = pm.get("density_filter") {
        p["phase_map"]["density_filter"] = normalise_density_filter(filter)?;
    }
    let v = p["validity"].clone();
    let v_keys: BTreeSet<&str> =
        v.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    if !v.is_object() || v_keys != VALIDITY_KEYS.iter().copied().collect() {
        return contract(format!(
            "validity requires exactly {}: the fixed-kinematics bounds (per cell and relative to the narrowest declared fluid feature) and the phase reporting thresholds",
            VALIDITY_KEYS.join(", ")
        ));
    }
    if v.as_object().into_iter().flatten().any(|(_, a)| finite_scalar(a).is_none()) {
        return contract("validity settings must be finite numbers");
    }
    let vv = |k: &str| v[k].as_f64().unwrap_or(f64::NAN);
    if !(0.0 < vv("max_displacement_over_cell")
        && vv("max_displacement_over_cell") <= MAX_DISPLACEMENT_OVER_CELL)
    {
        return contract(format!(
            "validity.max_displacement_over_cell must lie in (0, {MAX_DISPLACEMENT_OVER_CELL}]"
        ));
    }
    if ["solid_reporting_threshold", "fluid_reporting_threshold", "max_relative_channel_width_change"]
        .iter()
        .any(|k| !(0.0 < vv(k) && vv(k) < 1.0))
    {
        return contract(
            "validity reporting thresholds and max_relative_channel_width_change must lie in (0, 1)",
        );
    }
    if vv("min_fluid_feature_cells") < 1.0 {
        return contract(
            "validity.min_fluid_feature_cells must be at least one cell: narrower fluid features are not resolved",
        );
    }
    let n = &p["numerics"];
    let n_ok = n.as_object().is_some_and(|o| {
        o.len() == 2
            && o.contains_key("tolerance")
            && o.contains_key("max_iterations")
            && o.values().all(|a| finite_scalar(a).is_some_and(|x| x > 0.0))
    }) && n["max_iterations"].as_f64().is_some_and(|x| x.fract() == 0.0);
    if !n_ok {
        return contract("positive tolerance and integer iteration count required");
    }
    if let Some(rows) = p.get("history_observers").cloned() {
        p["history_observers"] = json!(observers::declarations(Some(&rows))?);
    }
    if let Some(c) = p.get("connectivity").cloned() {
        let required = [
            "admission_threshold",
            "max_closed_fluid_fraction",
            "max_disconnected_solid_fraction",
            "policy",
            "provenance",
            "require_through_path",
            "thresholds",
        ];
        let ok = c
            .as_object()
            .is_some_and(|o| o.len() == required.len() && required.iter().all(|k| o.contains_key(*k)));
        if !ok {
            return contract("explicit phase-connectivity policy required");
        }
        let thresholds = c["thresholds"].as_array().cloned().unwrap_or_default();
        let values: Option<Vec<f64>> = thresholds.iter().map(finite_scalar).collect();
        let valid = c["thresholds"].is_array()
            && !thresholds.is_empty()
            && values.as_ref().is_some_and(|v| {
                let unique: BTreeSet<u64> = v.iter().map(|x| x.to_bits()).collect();
                unique.len() == v.len() && v.iter().all(|x| *x > 0.0 && *x <= 1.0)
            });
        if !valid {
            return contract("unique phase thresholds in (0,1] required");
        }
        let admission = finite_scalar(&c["admission_threshold"]);
        let in_thresholds = admission.is_some_and(|a| values.as_ref().is_some_and(|v| v.contains(&a)));
        if !matches!(c["policy"].as_str(), Some("report" | "reject"))
            || !in_thresholds
            || !c["require_through_path"].is_boolean()
        {
            return contract("invalid connectivity admission policy");
        }
        if ["max_closed_fluid_fraction", "max_disconnected_solid_fraction"]
            .iter()
            .any(|k| finite_scalar(&c[*k]).is_none_or(|x| !(0.0..=1.0).contains(&x)))
        {
            return contract("connectivity fractions must be finite in [0,1]");
        }
        if c["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return contract("phase audit provenance required");
        }
    }
    if let Some(card) = p.get(STATIONARY_CONTINUATION_KEY).cloned() {
        p[STATIONARY_CONTINUATION_KEY] = normalise_stationary_continuation(problem, &p, &card)?;
    }
    if let Some(rows) = p.get("field_sources").cloned() {
        let registry = &implexity_core::registries::global().addins;
        let catalog = source_catalog()?;
        let context = p.clone();
        let declared = implexity_core::history_field_sources::declarations(
            registry,
            &catalog,
            Some(&rows),
            &context as &dyn std::any::Any,
        )?;
        p["field_sources"] = Value::Array(
            declared.iter().map(implexity_core::history_field_sources::SourceDeclaration::to_value).collect(),
        );
    }
    Ok(p)
}

#[must_use]
pub fn stationary_continuation_problem(problem: &Value, card: &Value) -> Value {
    let mut aux = problem.clone();
    if let Some(m) = aux.as_object_mut() {
        m.remove(STATIONARY_CONTINUATION_KEY);
    }
    aux["solid"] = card["solid"].clone();
    aux["fluid"] = card["fluid"].clone();
    if let Some(sources) = card.get("field_sources") {
        aux["field_sources"] = sources.clone();
    }
    aux
}



pub fn normalise_stationary_continuation(raw: &Value, normalised: &Value, card: &Value) -> CaeResult<Value> {
    let keys = ["fluid", "provenance", "solid"];
    let Some(m) = card.as_object().filter(|m| {
        keys.iter().all(|k| m.contains_key(*k)) && m.len() == 3 + usize::from(m.contains_key("field_sources"))
    }) else {
        return contract(format!(
            "{STATIONARY_CONTINUATION_KEY} requires solid, fluid and provenance and accepts only field_sources besides"
        ));
    };
    if m.contains_key("field_sources") != normalised.get("field_sources").is_some() {
        return contract(format!(
            "{STATIONARY_CONTINUATION_KEY}.field_sources must be given exactly when the problem declares field_sources"
        ));
    }
    if m["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return contract(format!(
            "{STATIONARY_CONTINUATION_KEY}.provenance must state why this pseudo-transient history reaches the operating state"
        ));
    }
    let aux = normalise(&stationary_continuation_problem(raw, card))?;
    let times = |p: &Value| -> Vec<f64> {
        p["solid"]["times_s"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect()
    };
    let (stationary, pseudo) = (times(normalised), times(&aux));
    if stationary.len() < 3
        || pseudo.len() < 3
        || pseudo[..2] != stationary[..2]
        || pseudo.last() > stationary.last()
    {
        return contract(format!(
            "{STATIONARY_CONTINUATION_KEY}: the pseudo-transient history needs at least three states, the stationary history's first two times and no time after its last"
        ));
    }
    let mut out = json!({"solid": aux["solid"], "fluid": aux["fluid"], "provenance": m["provenance"]});
    if let Some(sources) = aux.get("field_sources") {
        out["field_sources"] = sources.clone();
    }
    Ok(out)
}

#[must_use]
pub fn unified_history_starter() -> Value {
    let grid = [4usize, 2, 2];
    let times = [0.0, 1.0];
    let temperature = 300.0;
    let mut solid = implexity_physics_solid::solid_history::solid_history_starter();
    for (k, v) in [
        ("name", json!("Synthetic shared-domain solid, replace material data before use")),
        ("grid", json!(grid)),
        ("times_s", json!(times)),
        ("temperature_initial_K", json!(temperature)),
        ("temperature_bcs", json!([])),
        ("tractions", json!([])),
        ("heat_fluxes", json!([])),
    ] {
        solid[k] = v;
    }
    let fluid = crate::incompressible_transport::fluid_history_starter(grid, &times, temperature, None);
    json!({"name": "Synthetic shared-domain channel, author loads, match grid and replace material data before use",
        "components": {"solid": "inelastic_solid_history_block", "fluid": "mac_fluid_history",
                       "phase_load": "density_jump_total_stress"},
        "solid": solid, "fluid": fluid,
        "phase_map": {"projection_beta": 0.0,
                      "provenance": "Complementary fluid fraction 1 - rho of the authored geometry occupancy"},
        "validity": {"max_displacement_over_cell": 0.05, "min_fluid_feature_cells": 1.0,
                     "max_relative_channel_width_change": 0.1,
                     "solid_reporting_threshold": 0.5, "fluid_reporting_threshold": 0.5},
        "numerics": {"tolerance": 1e-8, "max_iterations": 30},
        "temperature_transport": "trilinear_cell_average_v1"})
}


pub fn installed_response_units() -> CaeResult<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> =
        RESPONSES.iter().zip(UNITS).map(|(r, u)| ((*r).to_string(), u.to_string())).collect();
    let set = implexity_core::distributions::global();

    let samples: BTreeSet<&str> = BASE_HISTORY_SAMPLES
        .iter()
        .chain(MATERIAL_HISTORY_SAMPLES.iter())
        .copied()
        .chain([
            implexity_physics_base::region_temperature_extrema::REGION_FRACTION_SAMPLE,
            implexity_physics_base::cyclic_plastic_strain::PLASTIC_STRAIN_SAMPLE,
            implexity_physics_base::cyclic_plastic_strain::CELL_REGION_SAMPLE,
            implexity_physics_base::wall_film_temperature::WALL_FLUX_SAMPLE,
            implexity_physics_base::wall_film_temperature::SPEED_SAMPLE,
        ])
        .collect();
    let history = set
        .catalogue_documents("history_responses")
        .map_err(|e| CaeError::contract(format!("cannot read installed history response catalogues: {e}")))?;

    implexity_core::component_manifests::load_history_catalog(set)?;
    source_catalog()?;
    let mut history_rows: Vec<(String, String)> = Vec::new();
    for (_, document) in &history {
        for component in document["components"].as_array().into_iter().flatten() {
            let requires: Vec<&str> =
                component["requires"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if !requires.iter().all(|r| samples.contains(r)) {
                continue;
            }
            for (name, unit) in component["response_units"].as_object().into_iter().flatten() {
                if history_rows.iter().any(|(n, _)| n == name) {
                    return contract(format!(
                        "ambiguous installed history response {}",
                        implexity_core::py_repr::repr_str(name)
                    ));
                }
                history_rows.push((name.clone(), unit.as_str().unwrap_or_default().to_string()));
            }
        }
    }
    let sources = set
        .catalogue_documents("field_sources")
        .map_err(|e| CaeError::contract(format!("cannot read installed field-source catalogues: {e}")))?;
    let mut source_rows: Vec<(String, String)> = Vec::new();
    for (_, document) in &sources {
        for component in document["components"].as_array().into_iter().flatten() {
            for (name, unit) in component["response_units"].as_object().into_iter().flatten() {
                if source_rows.iter().any(|(n, _)| n == name) {
                    return contract("duplicate field-source response manifest");
                }
                source_rows.push((name.clone(), unit.as_str().unwrap_or_default().to_string()));
            }
        }
    }
    for rows in [history_rows, source_rows] {
        let mut collisions: Vec<String> =
            rows.iter().filter(|(n, _)| out.iter().any(|(o, _)| o == n)).map(|(n, _)| n.clone()).collect();
        if !collisions.is_empty() {
            collisions.sort();
            return contract(format!(
                "duplicate advertised shared-domain responses: {}",
                list_repr(&collisions)
            ));
        }
        out.extend(rows);
    }
    Ok(out)
}


pub fn source_component_order() -> CaeResult<Vec<String>> {
    let documents = implexity_core::distributions::global()
        .catalogue_documents("field_sources")
        .map_err(|e| CaeError::contract(format!("cannot read installed field-source catalogues: {e}")))?;
    let mut out = Vec::new();
    for (_, document) in &documents {
        for component in document["components"].as_array().into_iter().flatten() {
            if let Some(id) = component["component_id"].as_str()
                && !out.iter().any(|o| o == id)
            {
                out.push(id.to_string());
            }
        }
    }
    Ok(out)
}


pub fn published_response_metadata(response_units: &[(String, String)]) -> CaeResult<Map<String, Value>> {
    let mut rows = Map::new();
    for (name, unit) in response_units {
        match RESPONSE_METADATA.iter().find(|m| m.name == name) {
            None => {
                rows.insert(
                    name.clone(),
                    json!({"unit": unit, "differentiable": true, "design_reachable": true}),
                );
            }
            Some(meta) => {
                if meta.unit != unit {
                    return contract(format!(
                        "{name}: response presentation unit disagrees with the numerical response contract"
                    ));
                }
                rows.insert(
                    name.clone(),
                    json!({"label": meta.label, "unit": meta.unit, "family": meta.family,
                        "description": meta.description, "differentiable": true, "design_reachable": true}),
                );
            }
        }
    }
    let mut stale: Vec<&str> = RESPONSE_METADATA
        .iter()
        .map(|m| m.name)
        .filter(|n| !response_units.iter().any(|(r, _)| r == n))
        .collect();
    if !stale.is_empty() {
        stale.sort_unstable();
        return contract(format!(
            "unadvertised shared-domain response presentation metadata: {}",
            list_repr(&stale)
        ));
    }
    Ok(rows)
}
