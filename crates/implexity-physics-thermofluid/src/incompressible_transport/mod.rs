// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub mod groups;
mod kernel;
pub mod material;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};

use implexity_core::orchestration::AddInCategory;
use implexity_core::packages::InstallContext;
use implexity_core::pyobj::list_repr;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_base::boundary_regions::aperture_bounds;
use implexity_physics_base::core_bridge::{ComponentOptions, PhysicsComponent, register_strict_component};

pub use crate::local_group::Role;
pub use kernel::{
    EnergyProfile, FluidBoundary, FluidFaces, FluidKernel, FluidMetrics, GroupSet, NodalFluidLaw,
    SHARED_TEMPERATURE_CALLBACKS, ShearRecord, ThermalBoundaryRecord, flat, ndindex,
    ndindex as kernel_ndindex, shared_temperature_energy_contract,
};
pub use material::{FluidCard, FluidLaw, NewtonianFluid, normalise_inactive_phase_numerical_material};

pub const MASS_BALANCE_RELATIVE_TOLERANCE: f64 = 1.0e-6;
pub const BRANCH_CONTRACT: &str = "implexity-laminar-solution-branch/1";
pub const STANDALONE_ENERGY_PROFILE: &str = "standalone_combined_v1";
pub const SHARED_TEMPERATURE_ENERGY_PROFILE: &str = "shared_temperature_split_v1";
pub const HYDROSTATIC_OPENING_WARNING: &str = "HydrostaticOpeningWarning";

pub const FLUID_MATERIAL_ID: &str = "newtonian_incompressible_fluid";
pub const FLUID_HISTORY_ID: &str = "mac_fluid_history";

pub const KERNEL_LIMITATIONS: [&str; 5] = [
    "Resolved MAC laminar incompressible momentum and cell enthalpy; fixed Cartesian region.",
    "Laminar solution-branch screening is numerical admission, not physical operating-regime qualification.",
    "Two opposite pressure openings, stationary no-slip walls; transition is unresolved and there is no moving mesh/turbulence/boiling.",
    "Optional Boussinesq body force requires at most 5% density variation over the authored material range; not variable-density or compressible transport.",
    "Brinkman obstruction does not create a mechanically solved solid in the fluid region.",
];

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}


pub fn selected_material(name: &str) -> CaeResult<NewtonianFluid> {
    let component = implexity_physics_base::core_bridge::registered_component(name)?;
    if component.component_kind().as_deref() != Some(NewtonianFluid::COMPONENT_KIND) {
        return contract(format!(
            "{name}: incompatible or inactive component {}",
            NewtonianFluid::COMPONENT_KIND
        ));
    }
    component.as_any().downcast_ref::<NewtonianFluid>().copied().ok_or_else(|| {
        CaeError::contract(format!(
            "{name}: incompatible or inactive component {}",
            NewtonianFluid::COMPONENT_KIND
        ))
    })
}

fn finite_real(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite()),
        _ => None,
    }
}

fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

#[must_use]
pub fn hydrostatic_opening_warning(p: &Value) -> Option<String> {
    let body = p.get("body_acceleration")?;
    let flow_axis = p["boundaries"]
        .as_array()?
        .iter()
        .find(|b| b["momentum"].as_str() == Some("pressure"))
        .and_then(|b| b["axis"].as_u64())
        .and_then(|a| usize::try_from(a).ok())?;
    let g_flow = body["acceleration_m_s2"][flow_axis].as_f64()?;
    if g_flow == 0.0 {
        return None;
    }
    let rho = p["material"]["density_kg_m3"].as_f64().unwrap_or(f64::NAN);
    let g = implexity_physics_cfd::pyfmt::fmt_g;
    Some(format!(
        "[MAC_HYDROSTATIC_OPENING] body_acceleration has component {} m/s^2 along the pressure-opening axis {flow_axis}. \
Opening pressures are absolute (not hydrostatically reduced) and the momentum residual carries the full rho*g weight, \
so the required equal opening pressures at t=0 are a gravity-driven state, not a hydrostatic rest state; a fluid at rest \
needs p_hi - p_lo = rho*g_f*L_f = {} Pa per metre of channel length along axis {flow_axis}. \
Author absolute histories that include this column; differential-only histories are not detected.",
        g(g_flow, 6),
        g(rho * g_flow, 6)
    ))
}

fn warn_once(message: &str) {
    static SEEN: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(BTreeSet::new()));
    let fresh = seen.lock().is_ok_and(|mut s| s.insert(message.to_string()));
    if fresh {

        eprintln!("{HYDROSTATIC_OPENING_WARNING}: {message}");
    }
}



pub fn normalise_fluid(p: &Value) -> CaeResult<Value> {
    let out = normalise_fluid_silent(p)?;
    if let Some(message) = hydrostatic_opening_warning(&out) {
        warn_once(&message);
    }
    Ok(out)
}

pub const TURBULENCE_KEY: &str = "turbulence";
pub const ADVECTION_SMOOTHING_KEY: &str = "momentum_advection_smoothing_m_s";
pub const MIXING_LENGTH_MODEL: &str = "prandtl_mixing_length";


pub fn validate_turbulence(t: &Value) -> CaeResult<()> {
    let keys = ["mixing_length_m", "model", "provenance", "rate_floor_s"];
    let ok = t.as_object().is_some_and(|o| {
        o.len() == keys.len()
            && keys.iter().all(|k| o.contains_key(*k))
            && o["model"] == MIXING_LENGTH_MODEL
            && ["mixing_length_m", "rate_floor_s"]
                .iter()
                .all(|k| o[*k].as_f64().is_some_and(|v| v.is_finite() && v > 0.0))
            && o["provenance"].as_str().is_some_and(|s| !s.trim().is_empty())
    });
    if ok {
        Ok(())
    } else {
        contract(format!(
            "turbulence requires exactly {} with model 'prandtl_mixing_length', positive mixing_length_m and rate_floor_s, and provenance",
            list_repr(&keys)
        ))
    }
}


#[allow(clippy::too_many_lines)]
pub fn normalise_fluid_silent(p: &Value) -> CaeResult<Value> {
    let required = [
        "grid",
        "times_s",
        "material",
        "material_component",
        "initial_temperature_K",
        "pressure_reference_Pa",
        "boundaries",
        "momentum_advection",
        "volumetric_heat_W_m3",
        "regularisation",
        "numerics",
        "assembly",
    ];
    let optional = ["inactive_phase_numerical_material", "body_acceleration", "applicability_policy"];
    let applicability = p.get("applicability_policy").and_then(Value::as_str).unwrap_or("enforce");
    if !matches!(applicability, "enforce" | "report_only")
        || p.get("applicability_policy").is_some_and(|v| !v.is_string())
    {
        return contract("applicability_policy must be enforce or report_only");
    }
    if applicability == "report_only"
        && p.get("inactive_phase_numerical_material").is_none_or(Value::is_null)
    {
        return contract("report_only requires an explicit supported numerical material continuation");
    }
    let mut sorted_required = required.to_vec();
    sorted_required.sort_unstable();
    let mut sorted_optional = optional.to_vec();
    sorted_optional.sort_unstable();
    let key_error = || {
        CaeError::contract(format!(
            "fluid history requires {}; optional keys: {}",
            list_repr(&sorted_required),
            list_repr(&sorted_optional)
        ))
    };
    let m = p.as_object().ok_or_else(key_error)?;

    if required.iter().any(|k| !m.contains_key(*k))
        || m.keys().any(|k| {
            !required.contains(&k.as_str())
                && !optional.contains(&k.as_str())
                && k != TURBULENCE_KEY
                && k != ADVECTION_SMOOTHING_KEY
        })
    {
        return Err(key_error());
    }
    if let Some(t) = m.get(TURBULENCE_KEY) {
        validate_turbulence(t)?;
    }
    if let Some(v) = m.get(ADVECTION_SMOOTHING_KEY)
        && !v.as_f64().is_some_and(|d| d.is_finite() && d > 0.0)
    {
        return contract("momentum_advection_smoothing_m_s must be a positive finite velocity");
    }
    let grid_ok = m["grid"]
        .as_array()
        .is_some_and(|g| g.len() == 3 && g.iter().all(|v| is_int(v) && v.as_i64().is_some_and(|n| n >= 1)));
    if !grid_ok {
        return contract("fluid grid must be three positive integers");
    }
    let grid: [usize; 3] =
        std::array::from_fn(|a| m["grid"][a].as_u64().and_then(|v| usize::try_from(v).ok()).unwrap_or(1));
    let times: Option<Vec<f64>> = m["times_s"].as_array().and_then(|t| t.iter().map(Value::as_f64).collect());
    let times = times.unwrap_or_default();
    let nt = times.len();
    if nt < 2
        || times.iter().any(|t| !t.is_finite())
        || times[0] != 0.0
        || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return contract("fluid times must start at zero and increase");
    }
    let mut p = p.clone();
    let material_component = p["material_component"].as_str().unwrap_or_default().to_string();
    let law = selected_material(&material_component)?;
    p["material"] = law.validate(&p["material"])?;
    let (evaluation_lower, evaluation_upper) = law.evaluation_bounds(&p["material"])?;
    if let Some(body) = p.get("body_acceleration").cloned() {
        let keys = ["acceleration_m_s2", "provenance", "reference_temperature_K", "thermal_expansion_K_inv"];
        let Some(b) = body.as_object().filter(|b| b.len() == 4 && keys.iter().all(|k| b.contains_key(*k)))
        else {
            return contract(format!("body_acceleration requires exactly {}", list_repr(&keys)));
        };
        let g: Option<Vec<f64>> = b["acceleration_m_s2"]
            .as_array()
            .filter(|g| g.len() == 3)
            .and_then(|g| g.iter().map(finite_real).collect());
        let Some(g) = g else {
            return contract("body acceleration requires three finite real components in m/s^2");
        };
        let beta = finite_real(&b["thermal_expansion_K_inv"]);
        let reference = finite_real(&b["reference_temperature_K"]);
        let (Some(beta), Some(reference)) = (beta, reference) else {
            return contract("nonnegative thermal expansion and in-range reference temperature required");
        };
        if beta < 0.0 || !(evaluation_lower <= reference && reference <= evaluation_upper) {
            return contract("nonnegative thermal expansion and in-range reference temperature required");
        }
        if beta * (evaluation_lower - reference).abs().max((evaluation_upper - reference).abs()) > 0.05 {
            return contract(
                "Boussinesq density variation must not exceed 5% throughout the material temperature interval",
            );
        }
        if b["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return contract("body acceleration provenance required");
        }
        let mut nb = b.clone();
        nb.insert("acceleration_m_s2".into(), json!(g));
        p["body_acceleration"] = Value::Object(nb);
    }
    if let Some(policy) = p.get("inactive_phase_numerical_material").cloned() {
        p["inactive_phase_numerical_material"] = normalise_inactive_phase_numerical_material(&policy)?;
    }
    let t0 = p["initial_temperature_K"].as_f64().unwrap_or(f64::NAN);
    if !t0.is_finite() || !(evaluation_lower <= t0 && t0 <= evaluation_upper) {
        return contract("initial fluid temperature outside evaluation interval");
    }
    let pref = p["pressure_reference_Pa"].as_f64().unwrap_or(f64::NAN);
    if !pref.is_finite() || pref <= 0.0 {
        return contract("positive absolute fluid pressure reference required");
    }
    if !p["momentum_advection"].is_boolean() {
        return contract("momentum_advection must be an explicit bool");
    }
    let Some(boundaries) = p["boundaries"].as_array().filter(|b| b.len() == 6).cloned() else {
        return contract("all six fluid boundary faces must be authored");
    };
    let mut seen = BTreeSet::new();
    let mut opens: Vec<(u64, String)> = Vec::new();
    for b in &boundaries {
        let momentum = b.get("momentum").and_then(Value::as_str);
        let thermal = b.get("thermal").and_then(Value::as_str);
        let mut keys: BTreeSet<&str> = ["axis", "side", "momentum", "thermal"].into_iter().collect();
        if momentum == Some("pressure") {
            keys.insert("pressure_absolute_Pa");
            keys.insert("incoming_temperature_K");
        }
        if b.get("opening").is_some() {
            keys.insert("opening");
            aperture_bounds(b, grid)?;
        }
        if !matches!(momentum, Some("pressure" | "no_slip")) {
            return contract("fluid momentum boundary must be pressure or no_slip");
        }
        if thermal == Some("temperature") {
            keys.insert("temperature_K");
        } else if !matches!(thermal, Some("insulated" | "interface")) {
            return contract("unknown fluid thermal boundary");
        }
        let actual: BTreeSet<&str> =
            b.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
        let axis = b.get("axis").filter(|v| is_int(v)).and_then(Value::as_u64).filter(|a| *a < 3);
        let side = b.get("side").and_then(Value::as_str).filter(|s| matches!(*s, "lo" | "hi"));
        let (Some(axis), Some(side)) = (axis, side) else {
            return contract("invalid fluid boundary keys/face");
        };
        if actual != keys {
            return contract("invalid fluid boundary keys/face");
        }
        if !seen.insert((axis, side.to_string())) {
            return contract("duplicated fluid boundary");
        }
        for key in ["pressure_absolute_Pa", "incoming_temperature_K", "temperature_K"] {
            if let Some(history) = b.get(key) {
                let a: Option<Vec<f64>> =
                    history.as_array().and_then(|h| h.iter().map(Value::as_f64).collect());
                let valid = a.as_ref().is_some_and(|a| {
                    a.len() == nt
                        && a.iter().all(|v| v.is_finite())
                        && a.iter().copied().fold(f64::INFINITY, f64::min) > 0.0
                });
                if !valid {
                    return contract(format!("invalid {key} history"));
                }
                let a = a.unwrap_or_default();
                if key.ends_with("temperature_K") {
                    let lo = a.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = a.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    if lo < evaluation_lower || hi > evaluation_upper {
                        return contract("boundary temperature outside fluid evaluation interval");
                    }
                }
            }
        }
        if momentum == Some("pressure") {
            opens.push((axis, side.to_string()));
        }
        if thermal == Some("interface") && momentum != Some("no_slip") {
            return contract("current fluid interface requires a stationary no-slip wall");
        }
    }
    if opens.len() != 2 || opens[0].0 != opens[1].0 {
        return contract("current MAC boundary adapter requires two opposite pressure openings");
    }
    let initial: Vec<f64> = boundaries
        .iter()
        .filter(|b| b["momentum"].as_str() == Some("pressure"))
        .map(|b| b["pressure_absolute_Pa"][0].as_f64().unwrap_or(f64::NAN))
        .collect();
    #[allow(clippy::float_cmp)]               
    if initial[0] != initial[1] {
        return contract(
            "stationary initial fluid requires equal absolute opening pressures at t=0; a precomputed nonzero-flow initial history is not yet supported",
        );
    }
    let q: Option<Vec<f64>> =
        p["volumetric_heat_W_m3"].as_array().and_then(|q| q.iter().map(Value::as_f64).collect());
    let q_ok = q.as_ref().is_some_and(|q| q.len() == nt && q.iter().all(|v| v.is_finite()) && q[0] == 0.0);
    if !q_ok {
        return contract("explicit finite fluid heat-source history starting at zero required");
    }
    let r = &p["regularisation"];
    let r_keys = ["brinkman_max_Pa_s_m2", "fluid_fraction_floor", "obstruction_exponent"];
    let r_ok = r.as_object().is_some_and(|o| {
        o.len() == 3
            && r_keys.iter().all(|k| o.contains_key(*k))
            && o.values().all(|v| v.as_f64().is_some_and(f64::is_finite))
    });
    let rv = |k: &str| r[k].as_f64().unwrap_or(f64::NAN);
    if !r_ok
        || rv("brinkman_max_Pa_s_m2") <= 0.0
        || rv("obstruction_exponent") < 1.0
        || !(0.0 < rv("fluid_fraction_floor") && rv("fluid_fraction_floor") < 1.0)
    {
        return contract("invalid fluid design regularisation");
    }
    let n = p["numerics"].as_object().cloned().unwrap_or_default();
    let keys: BTreeSet<&str> = [
        "velocity_scale_m_s",
        "pressure_scale_Pa",
        "temperature_scale_K",
        "length_scale_m",
        "power_scale_W",
        "max_reynolds",
        "max_cell_peclet",
    ]
    .into_iter()
    .collect();
    let optional_n: BTreeSet<&str> =
        ["max_cell_reynolds", "cell_reynolds_limit_provenance"].into_iter().collect();
    let policy_keys: BTreeSet<&str> =
        ["regime_screen_policy", "regime_screen_policy_provenance"].into_iter().collect();
    let text_keys: BTreeSet<&str> =
        ["cell_reynolds_limit_provenance", "regime_screen_policy", "regime_screen_policy_provenance"]
            .into_iter()
            .collect();
    let names: BTreeSet<&str> = n.keys().map(String::as_str).collect();
    let authored: BTreeSet<&str> = names.difference(&policy_keys).copied().collect();
    let with_optional: BTreeSet<&str> = keys.union(&optional_n).copied().collect();
    let policy_present: BTreeSet<&str> = names.intersection(&policy_keys).copied().collect();
    if !p["numerics"].is_object()
        || (authored != keys && authored != with_optional)
        || (!policy_present.is_empty() && policy_present != policy_keys)
    {
        return contract(
            "explicit positive fluid numerical scales and regime limits required; regime_screen_policy requires its provenance",
        );
    }
    for (key, v) in &n {
        if text_keys.contains(key.as_str()) {
            continue;
        }
        let ok = matches!(v, Value::Number(_)) && v.as_f64().is_some_and(|x| x.is_finite() && x > 0.0);
        if !ok {
            return contract("explicit positive fluid numerical scales and regime limits required");
        }
    }
    if optional_n.is_subset(&names)
        && n["cell_reynolds_limit_provenance"].as_str().is_none_or(|s| s.trim().is_empty())
    {
        return contract("cell Reynolds limit provenance must be a nonempty string");
    }
    if policy_keys.is_subset(&names) {
        if !matches!(n["regime_screen_policy"].as_str(), Some("reject" | "report")) {
            return contract("regime_screen_policy must be reject or report");
        }
        if n["regime_screen_policy_provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return contract("regime_screen_policy_provenance must be a nonempty string");
        }
    }
    let a = &p["assembly"];
    let a_ok = a.as_object().is_some_and(|o| {
        o.len() == 2
            && o.contains_key("batch_size")
            && o.contains_key("max_estimated_bytes")
            && o.values().all(|v| is_int(v) && v.as_i64().is_some_and(|x| x >= 1))
    }) && a["batch_size"].as_i64().is_some_and(|b| b <= 4096);
    if !a_ok {
        return contract("invalid fluid assembly budget");
    }
    let cells: u128 = grid.iter().map(|g| *g as u128).product();
    let estimate = cells * (12000 + nt as u128 * 64) * 8;
    if estimate > u128::from(a["max_estimated_bytes"].as_u64().unwrap_or(0)) {
        return contract("fluid sparse assembly/history estimate exceeds resource budget");
    }
    Ok(p)
}

#[must_use]
pub fn fluid_history_starter(
    grid: [usize; 3],
    times: &[f64],
    temperature: f64,
    interface_face: Option<(usize, &str)>,
) -> Value {
    let nt = times.len();
    let material = json!({"provenance": "Illustrative constant-density liquid, not a calibrated fluid",
        "density_kg_m3": 1000.0, "mu_Pa_s": 1e-3, "k_W_mK": 0.6, "cp_J_kgK": 4000.0,
        "T_ref_K": temperature, "T_min_K": temperature - 50.0, "T_max_K": temperature + 100.0,
        "mu_slope": 0.0, "k_slope": 0.0, "cp_slope": 0.0});
    let mut inlet = vec![1e5];
    inlet.extend(std::iter::repeat_n(1e5 + 1.0, nt - 1));
    let mut boundaries = vec![
        json!({"axis": 0, "side": "lo", "momentum": "pressure", "thermal": "insulated",
            "pressure_absolute_Pa": inlet, "incoming_temperature_K": vec![temperature; nt]}),
        json!({"axis": 0, "side": "hi", "momentum": "pressure", "thermal": "insulated",
            "pressure_absolute_Pa": vec![1e5; nt], "incoming_temperature_K": vec![temperature; nt]}),
    ];
    for a in [1usize, 2] {
        for side in ["lo", "hi"] {
            let thermal = if interface_face == Some((a, side)) { "interface" } else { "insulated" };
            boundaries.push(json!({"axis": a, "side": side, "momentum": "no_slip", "thermal": thermal}));
        }
    }
    json!({"grid": grid, "times_s": times, "material": material,
        "material_component": FLUID_MATERIAL_ID, "initial_temperature_K": temperature,
        "pressure_reference_Pa": 1e5,
        "boundaries": boundaries,
        "momentum_advection": false, "volumetric_heat_W_m3": vec![0.0; nt],
        "regularisation": {"brinkman_max_Pa_s_m2": 1e7, "obstruction_exponent": 3.0, "fluid_fraction_floor": 1e-3},
        "numerics": {"velocity_scale_m_s": 1e-3, "pressure_scale_Pa": 1.0, "temperature_scale_K": 10.0,
            "length_scale_m": 0.01, "power_scale_W": 1.0, "max_reynolds": 2000.0, "max_cell_peclet": 1e3},
        "assembly": {"batch_size": 64, "max_estimated_bytes": 268_435_456}})
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn fluid_editor_schema() -> Value {
    let aperture = implexity_physics_base::boundary_regions::SCHEMA;
    let domain = implexity_physics_base::material_domains::SCHEMA;
    json!({"properties": {
        "applicability_policy": {"type": "string", "enum": ["enforce", "report_only"], "default": "enforce"},
        "body_acceleration": {
            "title": "Gravity and thermal buoyancy", "type": "object",
            "description": "Optional full body acceleration with Boussinesq density only in momentum. Opening pressures remain absolute. Maximum density variation is 5% over the complete material temperature interval; this is an admission limit, not an accuracy guarantee.",
            "properties": {
                "acceleration_m_s2": {"title": "Acceleration vector", "type": "array", "minItems": 3, "maxItems": 3, "unit": "m/s²", "items": {"type": "number"}},
                "thermal_expansion_K_inv": {"title": "Volumetric thermal expansion", "type": "number", "minimum": 0, "unit": "1/K"},
                "reference_temperature_K": {"title": "Density reference temperature", "type": "number", "exclusiveMinimum": 0, "unit": "K"},
                "provenance": {"title": "Coefficient source and validity", "type": "string", "minLength": 1}},
            "required": ["acceleration_m_s2", "thermal_expansion_K_inv", "reference_temperature_K", "provenance"],
            "additionalProperties": false},
        "boundaries": {"type": "array", "title": "Flow boundaries",
            "description": "Exactly six faces: two opposite pressure openings and four walls.",
            "items": {"type": "object", "default": {"axis": 1, "side": "lo", "momentum": "no_slip", "thermal": "insulated"},
              "properties": {
                "axis": {"title": "Face axis (0=X, 1=Y, 2=Z)", "type": "integer", "enum": [0, 1, 2]},
                "side": {"title": "Face side (lo=minimum, hi=maximum)", "enum": ["lo", "hi"]},
                "momentum": {"title": "Momentum condition", "enum": ["pressure", "no_slip"],
                    "description": "pressure faces also need pressure_absolute_Pa and incoming_temperature_K histories."},
                "thermal": {"title": "Thermal condition", "enum": ["insulated", "temperature", "interface"],
                    "description": "temperature faces need temperature_K; interface is reserved for hosts that own the adjacent solid region."},
                "pressure_absolute_Pa": {"title": "Absolute pressure history", "unit": "Pa", "items": {"type": "number", "exclusiveMinimum": 0}},
                "incoming_temperature_K": {"title": "Incoming fluid temperature history", "unit": "K", "items": {"type": "number", "exclusiveMinimum": 0}},
                "temperature_K": {"title": "Wall temperature history", "unit": "K", "items": {"type": "number", "exclusiveMinimum": 0}},
                "opening": {
                "type": "object", "title": "Fixed rectangular pressure aperture",
                "default": {"schema": aperture, "id": "opening", "lower_fraction": [0.25, 0.25], "upper_fraction": [0.75, 0.75],
                           "closed_remainder": "stationary_no_slip_adiabatic",
                           "provenance": "Replace with the aperture source; fractions must align with the analysis grid"},
                "description": "Tangential axes in increasing order. Fractions follow affine domain scaling. Edges must align with the analysis grid. The remainder is a stationary, no-slip, adiabatic wall. No moving aperture or arbitrary cut cells.",
                "properties": {"schema": {"const": aperture}, "id": {"type": "string", "minLength": 1},
                    "lower_fraction": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "minimum": 0, "maximum": 1}},
                    "upper_fraction": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "minimum": 0, "maximum": 1}},
                    "closed_remainder": {"const": "stationary_no_slip_adiabatic"}, "provenance": {"type": "string", "minLength": 1}},
                "required": ["schema", "id", "lower_fraction", "upper_fraction", "closed_remainder", "provenance"],
                "additionalProperties": false}}}},
        "numerics": {"type": "object", "title": "Fluid numerical scales and regime screens",
            "properties": {
                "velocity_scale_m_s": {"title": "Velocity scale", "type": "number", "exclusiveMinimum": 0, "unit": "m/s"},
                "pressure_scale_Pa": {"title": "Pressure scale", "type": "number", "exclusiveMinimum": 0, "unit": "Pa"},
                "temperature_scale_K": {"title": "Temperature scale", "type": "number", "exclusiveMinimum": 0, "unit": "K"},
                "length_scale_m": {"title": "Length scale", "type": "number", "exclusiveMinimum": 0, "unit": "m"},
                "power_scale_W": {"title": "Power scale", "type": "number", "exclusiveMinimum": 0, "unit": "W"},
                "max_reynolds": {"title": "Maximum hydraulic Reynolds number", "type": "number", "exclusiveMinimum": 0, "unit": "1"},
                "max_cell_peclet": {"title": "Maximum cell Péclet number", "type": "number", "exclusiveMinimum": 0, "unit": "1"},
                "max_cell_reynolds": {"title": "Maximum local cell Reynolds number", "type": "number", "exclusiveMinimum": 0, "unit": "1", "default": 1000.0,
                    "description": "Optional screen; requires cell_reynolds_limit_provenance."},
                "cell_reynolds_limit_provenance": {"title": "Cell Reynolds limit provenance", "type": "string", "minLength": 1,
                    "default": "State why this local cell Reynolds limit is appropriate",
                    "description": "Required together with max_cell_reynolds."},
                "regime_screen_policy": {"title": "Regime screen policy", "enum": ["reject", "report"], "default": "reject",
                    "description": "reject (default when omitted): cell Péclet/Reynolds screens are numerical admission. report: they are reported diagnostics only, for deliberately simplified flow models used outside laminar validity. Finiteness, temperature domain, positive pressure and mass conservation always remain admission. Requires regime_screen_policy_provenance."},
                "regime_screen_policy_provenance": {"title": "Regime screen policy provenance", "type": "string", "minLength": 1,
                    "default": "State why the regime screens are reported rather than enforced",
                    "description": "Required together with regime_screen_policy."}}},
        "material": {"type": "object", "properties": {
            "evaluation_domain": {"type": "object", "title": "Explicit constitutive evaluation envelope",
                "description": "Optional provider-supported continuation of the authored law. Original T_min_K/T_max_K and provenance remain unchanged. This does not extend calibration, establish single-phase validity, or certify an engineering design. Unsupported material providers refuse it.",
                "properties": {"schema": {"const": domain},
                    "variable": {"const": "temperature_K"},
                    "lower": {"type": "number", "exclusiveMinimum": 0, "unit": "K"},
                    "upper": {"type": "number", "exclusiveMinimum": 0, "unit": "K"},
                    "continuation": {"const": "authored_law"},
                    "provenance": {"type": "string", "minLength": 1}},
                "required": ["schema", "variable", "lower", "upper", "continuation", "provenance"],
                "additionalProperties": false}}}}})
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FluidFactory;

impl FluidFactory {
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.incompressible_transport._FluidFactory";
    pub const COMPONENT_KIND: &'static str = "fluid_history_field";


    pub fn validate(&self, p: &Value) -> CaeResult<Value> {
        normalise_fluid(p)
    }


    pub fn create(&self, p: &Value) -> CaeResult<Arc<FluidKernel>> {
        Ok(Arc::new(FluidKernel::new(p, EnergyProfile::Standalone)?))
    }


    pub fn create_shared_temperature(&self, p: &Value) -> CaeResult<Arc<FluidKernel>> {
        Ok(Arc::new(FluidKernel::new(p, EnergyProfile::SharedTemperature)?))
    }

    #[must_use]
    pub fn shared_temperature_energy_contract(&self) -> Value {
        shared_temperature_energy_contract()
    }

    #[must_use]
    pub fn shared_temperature_callbacks(&self) -> [&'static str; 8] {
        SHARED_TEMPERATURE_CALLBACKS
    }

    #[must_use]
    pub fn runtime_support() -> Map<String, Value> {
        json!({"status": "field_component", "history": true, "data": "user_required",
            "limitations": KERNEL_LIMITATIONS})
        .as_object()
        .cloned()
        .unwrap_or_default()
    }
}

impl PhysicsComponent for FluidFactory {
    fn implementation(&self) -> String {
        Self::IMPLEMENTATION.into()
    }
    fn component_kind(&self) -> Option<String> {
        Some(Self::COMPONENT_KIND.into())
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(Self::runtime_support())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}


pub fn selected_fluid_factory(name: &str) -> CaeResult<FluidFactory> {
    let component = implexity_physics_base::core_bridge::registered_component(name)?;
    component
        .as_any()
        .downcast_ref::<FluidFactory>()
        .copied()
        .ok_or_else(|| CaeError::contract(format!("{name}: incompatible {}", FluidFactory::COMPONENT_KIND)))
}


pub fn register_components(ctx: &InstallContext<'_>) -> CaeResult<()> {
    let notes = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    register_strict_component(
        ctx,
        FLUID_MATERIAL_ID,
        Arc::new(NewtonianFluid),
        "incompressible_fluid_properties",
        &ComponentOptions {
            category: AddInCategory::Constitutive,
            domain: "fluid".into(),
            notes: notes(&material::LIMITATIONS),
            ..ComponentOptions::default()
        },
    )?;
    register_strict_component(
        ctx,
        FLUID_HISTORY_ID,
        Arc::new(FluidFactory),
        "fluid_history_field",
        &ComponentOptions {
            category: AddInCategory::Field,
            domain: "fluid".into(),
            notes: notes(&KERNEL_LIMITATIONS),
            ..ComponentOptions::default()
        },
    )?;
    Ok(())
}
