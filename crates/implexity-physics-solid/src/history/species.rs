// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;

use super::HistoryEnergy;
use crate::util::{
    contract, convergence, f, grid_cells, has_exact_keys, minimum_yield, nested, num, real_array, text,
};

pub const R_GAS: f64 = 8.314_462_618_153_24;
pub const UNITS: [(&str, &str); 10] = [
    ("capacity_atomic_fraction", "1"),
    ("initial_atomic_fraction", "1"),
    ("loss_rate_ref_s_inv", "1/s"),
    ("activation_J_mol", "J/mol"),
    ("reference_temperature_K", "K"),
    ("minimum_temperature_K", "K"),
    ("maximum_temperature_K", "K"),
    ("conductivity_resistance_factor", "1"),
    ("yield_increment_at_capacity_Pa", "Pa"),
    ("creep_log_multiplier", "1"),
];
pub const FORCING: &str = "right_endpoint_native_cell_atomic_fraction_per_second";
pub const ENERGY: &str = "athermal_inventory_heat_deposition_authored_separately";
pub const LIMITATIONS: [&str; 6] = [
    "Local saturating inventory driven by an explicitly prescribed atomic-fraction source.",
    "Not alpha transport, dpa, a helium-bubble/swelling model, or a calibrated material card.",
    "Loss is a local sink; transport of released species is not solved.",
    "Inventory energy and species mass are neglected; beam heat must be authored separately.",
    "Conductivity, yield and creep multipliers require joint calibration for the named species/material.",
    "No elastic-modulus change, fatigue life, rupture or geometry recession.",
];
const REQUIRED: [&str; 8] = [
    "name",
    "provenance",
    "species",
    "units",
    "endmembers",
    "source_atomic_fraction_s_inv",
    "forcing_convention",
    "energy_convention",
];

fn units_value() -> Value {
    Value::Object(UNITS.iter().map(|(k, u)| ((*k).to_string(), json!(u))).collect())
}

#[must_use]
pub fn authoring_contract() -> Value {
    json!({"schema": "implexity-component-authoring/1", "selection": "solid.material_history",
        "required_settings": REQUIRED,
        "forcing_units": "1/s", "forcing_shape": ["len(times_s)", "product(grid)", 2],
        "forcing_convention": FORCING, "energy_convention": ENERGY,
        "endmember_parameter_units": units_value(), "state_units": ["1", "1"],
        "evolution": "dc/dt = source*(1-c/capacity) - loss(T)*c",
        "calibrated_material_data_supplied": false})
}

#[must_use]
pub fn editor_schema(_settings: &Value, _context: &Value) -> Value {
    let props: Map<String, Value> = UNITS
        .iter()
        .map(|(k, u)| ((*k).to_string(), json!({"title": k.replace('_', " "), "type": "number", "units": u})))
        .collect();
    json!({"properties": {
        "name": {"title": "Retention model name"},
        "species": {"title": "Retained species identifier", "type": "string"},
        "provenance": {"title": "Source profile and material calibration provenance"},
        "endmembers": {"title": "Ordered material-specific parameters", "type": "array",
            "minItems": 2, "maxItems": 2, "items": {"properties": props}},
        "source_atomic_fraction_s_inv": {"title": "Local implantation source history", "type": "array", "units": "1/s",
            "description": "Explicit [time, C-order cell, material endpoint] array. Not dpa/s or surface fluence."},
        "forcing_convention": {"enum": [FORCING]},
        "energy_convention": {"enum": [ENERGY]}}})
}

#[must_use]
pub fn solid_study_templates(solid: &Value) -> Vec<Value> {
    if !solid.is_object() || solid.get("material_history").is_some_and(|v| !v.is_null()) {
        return Vec::new();
    }
    let (materials, grid, times) = (&solid["materials"], &solid["grid"], &solid["times_s"]);
    let materials_ok = materials.as_array().is_some_and(|ms| {
        ms.len() == 2
            && ms
                .iter()
                .all(|m| ["T_min", "T_max", "T_ref"].iter().all(|k| m.get(*k).is_some_and(Value::is_number)))
    });
    let grid_ok = grid.as_array().is_some_and(|g| {
        g.len() == 3 && g.iter().all(|v| v.as_i64().is_some_and(|n| n >= 1) && (v.is_i64() || v.is_u64()))
    });
    let times_ok = times.as_array().is_some_and(|t| t.len() >= 2);
    if !(materials_ok && grid_ok && times_ok) {
        return Vec::new();
    }
    let endmembers: Vec<Value> = materials
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            json!({"capacity_atomic_fraction": 1e-3, "initial_atomic_fraction": 0.0,
                "loss_rate_ref_s_inv": 0.0, "activation_J_mol": 0.0,
                "reference_temperature_K": f(m, "T_ref"),
                "minimum_temperature_K": f(m, "T_min"), "maximum_temperature_K": f(m, "T_max"),
                "conductivity_resistance_factor": 0.0, "yield_increment_at_capacity_Pa": 0.0,
                "creep_log_multiplier": 0.0})
        })
        .collect();
    let nt = times.as_array().map_or(0, Vec::len);
    let nc = grid_cells(solid);
    let settings = json!({"name": "Local species retention (zero-source starter)",
        "provenance": "Zero implantation source and zero property effects; author the species source profile and a joint calibration before use",
        "species": "unspecified", "units": units_value(), "endmembers": endmembers,
        "source_atomic_fraction_s_inv": nested(&[nt, nc, 2], &vec![0.0; nt * nc * 2]),
        "forcing_convention": FORCING, "energy_convention": ENERGY});
    if validate(&settings, solid).is_err() {
        return Vec::new();
    }
    let name = "saturating_species_retention";
    vec![json!({"id": "saturating_species_retention_setup",
        "label": "Add local species retention (zero-source starter)",
        "description": "Adds a saturating retained-species inventory dc/dt = source·(1-c/capacity) - loss(T)·c per cell and material endpoint, with conductivity, yield and creep feedback. The starter has zero source and zero effects; supply the [time, C-order cell, endpoint] source in atomic fraction per second and calibrated endpoint parameters. Not dpa, fluence, transport or swelling.",
        "truth_status": "unvalidated_zero_load_authoring_starter",
        "problem_requirements": [{"path": ["material_history"], "value": null, "missing_equals_null": true},
            {"path": ["grid"], "value": grid}, {"path": ["times_s"], "value": times}],
        "problem_patch": {"material_history": {"component": name, "settings": settings}},
        "editor_schema_patch": {"properties": {"material_history": {"title": "Local species retention and thermal release",
            "properties": {"component": {"title": "Material-state component", "enum": [name]},
                "settings": editor_schema(&settings, solid)}}}}})]
}


pub fn validate(settings: &Value, context: &Value) -> Result<Value, CaeError> {
    if !has_exact_keys(settings, &REQUIRED) {
        return contract(format!(
            "species retention requires exactly {}",
            crate::util::sorted_repr(REQUIRED)
        ));
    }
    for key in ["name", "provenance", "species"] {
        if !text(&settings[key]) {
            return contract(format!("species retention requires explicit {key}"));
        }
    }
    if settings["units"] != units_value() {
        return contract("species retention requires declared parameter units; no dpa conversion");
    }
    if settings["forcing_convention"] != json!(FORCING) || settings["energy_convention"] != json!(ENERGY) {
        return contract("explicit source/energy conventions required");
    }
    let Some(endpoints) = settings["endmembers"].as_array().filter(|e| e.len() == 2) else {
        return contract("two ordered material endpoints required");
    };
    let keys: Vec<&str> = UNITS.iter().map(|(k, _)| *k).collect();
    let mut result = settings.clone();
    let mut parsed = Vec::new();
    for (i, raw) in endpoints.iter().enumerate() {
        if !has_exact_keys(raw, &keys) {
            return contract("incomplete species-retention endpoint");
        }
        let mut e = Map::new();
        for (key, value) in raw.as_object().into_iter().flatten() {
            let Some(x) = num(value) else { return contract(format!("{key}: finite real scalar required")) };
            e.insert(key.clone(), crate::util::float(x));
        }
        let e = Value::Object(e);
        let cap = f(&e, "capacity_atomic_fraction");
        let init = f(&e, "initial_atomic_fraction");
        if !(0.0 < cap && cap <= 1.0) || !(0.0 <= init && init <= cap) {
            return contract("capacity in (0,1] and bounded initial inventory required");
        }
        let (lo, hi) = (f(&e, "minimum_temperature_K"), f(&e, "maximum_temperature_K"));
        let t_ref = f(&e, "reference_temperature_K");
        if !(0.0 < lo && lo < hi) || !(lo <= t_ref && t_ref <= hi) {
            return contract("invalid retention calibration temperature interval");
        }
        if f(&e, "loss_rate_ref_s_inv")
            .min(f(&e, "activation_J_mol"))
            .min(f(&e, "conductivity_resistance_factor"))
            < 0.0
        {
            return contract("negative release, activation or conductivity resistance");
        }
        let m = &context["materials"][i];
        if lo > f(m, "T_min") || hi < f(m, "T_max") {
            return contract("retention calibration must cover the authored material interval");
        }
        if minimum_yield(m) + f(&e, "yield_increment_at_capacity_Pa").min(0.0) <= 0.0 {
            return contract("retained-species yield law can become nonpositive");
        }
        for t in [lo, hi] {
            let exponent = -f(&e, "activation_J_mol") / R_GAS * (1.0 / t - 1.0 / t_ref);
            if exponent.abs() > 600.0 || f(&e, "creep_log_multiplier").abs() > 600.0 {
                return contract("retention exponent outside finite supported range");
            }
            if !(f(&e, "loss_rate_ref_s_inv") * exponent.exp()).is_finite() {
                return contract("retention loss rate overflows in calibration interval");
            }
        }
        result["endmembers"][i] = e.clone();
        parsed.push(e);
    }
    let times: Vec<f64> = match real_array(&context["times_s"]) {
        Some((s, t)) if s.len() == 1 => t,
        _ => return contract("ordered finite material-history times required"),
    };
    if times.len() < 2 || times.iter().any(|t| !t.is_finite()) || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return contract("ordered finite material-history times required");
    }
    let dt: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let (nt, nc) = (times.len(), grid_cells(context));
    let Some((shape, source)) = real_array(&settings["source_atomic_fraction_s_inv"]) else {
        return contract("finite numeric species source required");
    };
    if shape != [nt, nc, 2] || source.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return contract(format!("species source requires nonnegative finite shape ({nt}, {nc}, 2)"));
    }
    for (i, e) in parsed.iter().enumerate() {
        let cap = f(e, "capacity_atomic_fraction");
        let worst_loss = f(e, "loss_rate_ref_s_inv")
            * (-f(e, "activation_J_mol") / R_GAS
                * (1.0 / f(e, "maximum_temperature_K") - 1.0 / f(e, "reference_temperature_K")))
            .exp();
        let mut ok = true;
        for n in 0..nt {
            for cell in 0..nc {
                let s = source[(n * nc + cell) * 2 + i];
                let scaled = s / cap;
                ok &= scaled.is_finite();
                if n >= 1 {
                    ok &= (1.0 + dt[n - 1] * (scaled + worst_loss)).is_finite();
                    ok &= (cap + dt[n - 1] * s).is_finite();
                }
            }
        }
        if !ok {
            return contract("source/loss and time increment exceed finite backward-Euler range");
        }
    }
    result["source_atomic_fraction_s_inv"] = nested(&shape, &source);
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Endpoint {
    capacity: f64,
    initial: f64,
    loss_rate: f64,
    activation: f64,
    t_ref: f64,
    resistance: f64,
    yield_increment: f64,
    creep_log: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpeciesRetention {
    endpoints: [Endpoint; 2],
}

impl SpeciesRetention {
    #[must_use]
    pub fn bind(settings: &Value) -> Self {
        let e = |i: usize| {
            let v = &settings["endmembers"][i];
            Endpoint {
                capacity: f(v, "capacity_atomic_fraction"),
                initial: f(v, "initial_atomic_fraction"),
                loss_rate: f(v, "loss_rate_ref_s_inv"),
                activation: f(v, "activation_J_mol"),
                t_ref: f(v, "reference_temperature_K"),
                resistance: f(v, "conductivity_resistance_factor"),
                yield_increment: f(v, "yield_increment_at_capacity_Pa"),
                creep_log: f(v, "creep_log_multiplier"),
            }
        };
        Self { endpoints: [e(0), e(1)] }
    }

    #[must_use]
    pub fn state_metadata(&self) -> Vec<Value> {
        self.endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| {
                json!({"name": format!("retained_atomic_fraction_endpoint_{i}"), "units": "1",
                    "scale": e.capacity, "initial": e.initial})
            })
            .collect()
    }


    pub fn validate_numerical_extension(&self, settings: &Value, context: &Value) -> Result<(), CaeError> {
        implexity_physics_base::history_numerical_extension::validate_kinetic_range(
            settings,
            context,
            "loss_rate_ref_s_inv",
            "reference_temperature_K",
            "source_atomic_fraction_s_inv",
            &[1.0 / self.endpoints[0].capacity, 1.0 / self.endpoints[1].capacity],
        )
    }

    pub fn residual<S: Scalar>(
        &self,
        state: &[S],
        previous: &[S],
        temps: [S; 2],
        dt: S,
        forcing: &[S],
        out: &mut [S],
    ) {
        for (i, e) in self.endpoints.iter().enumerate() {
            let loss = (-(S::one() / temps[i] - 1.0 / e.t_ref) * (e.activation / R_GAS)).exp() * e.loss_rate;
            let source = forcing[i];
            let target = (previous[i] + dt * source) / (dt * (loss + source / e.capacity) + 1.0);
            out[i] = state[i] - target;
        }
    }

    pub fn properties<S: Scalar>(&self, endpoint: usize, state: &[S], base: [S; 3]) -> [S; 3] {
        let e = &self.endpoints[endpoint];
        let occupancy = state[endpoint] / e.capacity;
        [
            base[0] / (occupancy * e.resistance + 1.0),
            base[1] + occupancy * e.yield_increment,
            base[2] * (occupancy * e.creep_log).exp(),
        ]
    }

    #[must_use]
    pub fn energy<S: Scalar>(&self) -> HistoryEnergy<S> {
        HistoryEnergy::zero()
    }


    pub fn check_state(&self, state: &[f64], width: usize) -> Result<(), CaeError> {
        if width != 2 || !state.len().is_multiple_of(width) || state.iter().any(|v| !v.is_finite()) {
            return convergence("invalid retained-species state shape/values");
        }
        for row in state.chunks(2) {
            for (i, v) in row.iter().enumerate() {
                let cap = self.endpoints[i].capacity;
                if *v < -1e-9 * cap || *v > (1.0 + 1e-9) * cap {
                    return convergence("retained species outside [0,capacity]; never clamped");
                }
            }
        }
        Ok(())
    }
}
