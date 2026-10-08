// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;

use super::HistoryEnergy;
use crate::util::{contract, convergence, f, has_exact_keys, nested, num, real_array, text};

pub const R_GAS: f64 = 8.314_462_618_153_24;
pub const KINDS: [&str; 3] = ["thermal", "oxidative", "hydrolytic"];
pub const UNITS: [(&str, &str); 8] = [
    ("rate_ref_s_inv", "1/s"),
    ("activation_J_mol", "J/mol"),
    ("T_ref_K", "K"),
    ("initial_extent", "1"),
    ("conductivity_remaining", "1"),
    ("yield_remaining", "1"),
    ("creep_log_multiplier", "1"),
    ("chemical_energy_J_kg", "J/kg"),
];
pub const LIMITATIONS: [&str; 5] = [
    "Effective local first-order reaction extents; each mechanism requires its own calibration and temperature interval.",
    "Oxygen and water activities are prescribed local histories, not solved diffusion or surface boundary values.",
    "No corrosion recession, oxide-scale geometry, swelling, cracking, precipitation microstructure or stress-assisted kinetics.",
    "Independent mechanisms combine multiplicatively in conductivity/strength; interacting chemistry is not resolved.",
    "Nonnegative chemical storage releases heat; optional Maxwell spring softening separately releases mechanical storage. Other elastic moduli do not evolve.",
];

fn units_value(maxwell: bool) -> Value {
    let mut m: Map<String, Value> = UNITS.iter().map(|(k, u)| ((*k).to_string(), json!(u))).collect();
    if maxwell {
        m.insert("maxwell_stiffness_remaining".into(), json!("1"));
    }
    Value::Object(m)
}

#[must_use]
pub fn authoring_contract() -> Value {
    json!({"schema": "implexity-component-authoring/1", "selection": "solid.material_history",
        "required_settings": ["name", "provenance", "mechanisms", "activities", "units"],
        "mechanism_kinds": KINDS, "endmember_parameter_units": units_value(false),
        "maxwell_endmember_parameter_units": {"maxwell_stiffness_remaining": "1"},
        "activities_shape": ["len(times_s)", "product(grid)", 2],
        "activities_channels": ["local_oxygen_activity", "local_water_activity"],
        "activity_bounds": [0, 1], "sampling": "backward Euler right endpoint, native C-order cells",
        "evolution": "da/dt = k_ref exp[-Q/R(1/T-1/T_ref)] activity (1-a)",
        "thermal_activity": 1, "energy": "sum rho_i weight_i H_i (1-a_i); heat=sum rho_i weight_i H_i da_i/dt",
        "calibrated_material_data_supplied": false})
}

#[must_use]
pub fn editor_schema(_settings: &Value, context: &Value) -> Value {
    let mut coefficient = Map::new();
    for (key, unit) in UNITS {
        coefficient
            .insert(key.into(), json!({"type": "number", "title": key.replace('_', " "), "units": unit}));
    }
    let visco = context.is_object() && truthy(&context["viscoelasticity"]);
    if visco {
        coefficient.insert(
            "maxwell_stiffness_remaining".into(),
            json!({"type": "number", "title": "Remaining Maxwell stiffness", "units": "1", "exclusiveMinimum": 0, "maximum": 1}),
        );
    }
    for key in ["rate_ref_s_inv", "activation_J_mol", "initial_extent", "chemical_energy_J_kg"] {
        coefficient[key]["minimum"] = json!(0);
    }
    coefficient["initial_extent"]["maximum"] = json!(1);
    for key in ["conductivity_remaining", "yield_remaining"] {
        coefficient[key]["exclusiveMinimum"] = json!(0);
        coefficient[key]["maximum"] = json!(1);
    }
    if visco {
        let y = &mut coefficient["yield_remaining"];
        y["minimum"] = json!(1);
        y["maximum"] = json!(1);
        y["description"] = json!("Keep at one; Maxwell uses spring stiffness rather than a yield law.");
        let c = &mut coefficient["creep_log_multiplier"];
        c["minimum"] = json!(0);
        c["maximum"] = json!(0);
        c["description"] = json!("Keep at zero; Norton creep is not selected with Maxwell relaxation.");
    }
    coefficient["T_ref_K"]["exclusiveMinimum"] = json!(0);
    json!({"properties": {
        "name": {"title": "Ageing model name"}, "provenance": {"title": "Calibration and validity evidence"},
        "mechanisms": {"title": "Independent ageing mechanisms", "type": "array", "minItems": 1, "maxItems": 3,
            "items": {"type": "object", "properties": {
                "kind": {"enum": KINDS}, "provenance": {"title": "Mechanism calibration evidence"},
                "T_min_K": {"type": "number", "exclusiveMinimum": 0, "title": "Minimum valid temperature", "units": "K"},
                "T_max_K": {"type": "number", "exclusiveMinimum": 0, "title": "Maximum valid temperature", "units": "K"},
                "endmembers": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"properties": coefficient}}}}},
        "activities": {"title": "Local oxygen and water activities", "type": "array",
            "items": {"type": "array", "items": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "minimum": 0, "maximum": 1}}},
            "description": "Prescribed [time, cell, 2] values in [0,1]; not ambient relative humidity unless locally justified."}}})
}

pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}


#[allow(clippy::too_many_lines)]
pub fn validate(s: &Value, context: &Value) -> Result<Value, CaeError> {
    let keys = ["activities", "mechanisms", "name", "provenance", "units"];
    if !has_exact_keys(s, &keys) || !(s["units"] == units_value(false) || s["units"] == units_value(true)) {
        return contract(
            "environmental ageing requires name, provenance, mechanisms, activities and explicit coefficient units",
        );
    }
    if !(text(&s["name"]) && text(&s["provenance"])) {
        return contract("ageing name and calibration provenance are required");
    }
    let Some(mechanisms) = s["mechanisms"].as_array().filter(|m| (1..=3).contains(&m.len())) else {
        return contract("declare one to three independent ageing mechanisms");
    };
    let plain: Vec<&str> = UNITS.iter().map(|(k, _)| *k).collect();
    let mut with_maxwell = plain.clone();
    with_maxwell.push("maxwell_stiffness_remaining");
    let mut seen: Vec<&str> = Vec::new();
    for row in mechanisms {
        if !has_exact_keys(row, &["T_max_K", "T_min_K", "endmembers", "kind", "provenance"]) {
            return contract(
                "each ageing mechanism requires kind, provenance, temperature limits and two endmembers",
            );
        }
        let kind = row["kind"].as_str().filter(|k| KINDS.contains(k) && !seen.contains(k));
        let Some(kind) = kind else { return contract("ageing kinds must be supported and unique") };
        seen.push(kind);
        if !text(&row["provenance"]) {
            return contract("mechanism calibration provenance is required");
        }
        let (lo, hi) = (num(&row["T_min_K"]), num(&row["T_max_K"]));
        let (Some(lo), Some(hi)) = (lo, hi) else { return contract("invalid ageing temperature interval") };
        if !(0.0 < lo && lo < hi) {
            return contract("invalid ageing temperature interval");
        }
        let Some(endmembers) = row["endmembers"].as_array().filter(|e| e.len() == 2) else {
            return contract("ageing requires two ordered material endmembers");
        };
        for (i, e) in endmembers.iter().enumerate() {
            let complete = has_exact_keys(e, &plain) || has_exact_keys(e, &with_maxwell);
            if !complete || !e.as_object().is_some_and(|m| m.values().all(|v| num(v).is_some())) {
                return contract("invalid finite ageing coefficients");
            }
            if let Some(r) = e.get("maxwell_stiffness_remaining") {
                let r = r.as_f64().unwrap_or(f64::NAN);
                if s["units"].get("maxwell_stiffness_remaining") != Some(&json!("1"))
                    || !(0.0 < r && r <= 1.0)
                {
                    return contract(
                        "Maxwell stiffness fraction requires explicit dimensionless units and bounds (0,1]",
                    );
                }
                if !truthy(context.get("viscoelasticity").unwrap_or(&Value::Null)) {
                    return contract(
                        "Maxwell stiffness modifier requires an explicitly selected viscoelastic component",
                    );
                }
            }
            let min3 = f(e, "rate_ref_s_inv").min(f(e, "activation_J_mol")).min(f(e, "chemical_energy_J_kg"));
            if min3 < 0.0 || !(0.0..=1.0).contains(&f(e, "initial_extent")) {
                return contract(
                    "ageing rates, activation and energy must be nonnegative; extent must be in [0,1]",
                );
            }
            let fractions_ok =
                ["conductivity_remaining", "yield_remaining"].iter().all(|k| 0.0 < f(e, k) && f(e, k) <= 1.0);
            if !fractions_ok || f(e, "creep_log_multiplier").abs() > 50.0 {
                return contract("invalid remaining property fractions or creep multiplier");
            }
            let mat = &context["materials"][i];
            let t_ref = f(e, "T_ref_K");
            if !(lo <= f(mat, "T_min") && f(mat, "T_min") <= f(mat, "T_max") && f(mat, "T_max") <= hi)
                || !(lo <= t_ref && t_ref <= hi)
            {
                return contract(
                    "material temperature range and reference must lie within ageing calibration interval",
                );
            }
            for t in [lo, hi] {
                let exponent = -f(e, "activation_J_mol") / R_GAS * (1.0 / t - 1.0 / t_ref);
                let rate = f(e, "rate_ref_s_inv") * exponent.exp();
                if exponent.abs() > 600.0
                    || !rate.is_finite()
                    || (f(e, "rate_ref_s_inv") > 0.0 && rate < f64::MIN_POSITIVE)
                {
                    return contract("ageing rate outside finite normal range");
                }
            }
        }
    }
    if let Some(materials) = context["materials"].as_array() {
        for (i, mat) in materials.iter().enumerate() {
            let factors: Vec<f64> =
                ["conductivity_remaining", "yield_remaining", "maxwell_stiffness_remaining"]
                    .iter()
                    .map(|key| {
                        mechanisms
                            .iter()
                            .map(|m| m["endmembers"][i].get(*key).and_then(Value::as_f64).unwrap_or(1.0))
                            .product()
                    })
                    .collect();
            let log_sum: f64 =
                mechanisms.iter().map(|m| f(&m["endmembers"][i], "creep_log_multiplier").max(0.0)).sum();
            let creep = mat.get("creep_rate_ref").and_then(Value::as_f64).unwrap_or(0.0) * log_sum.exp();
            let energy_sum: f64 =
                mechanisms.iter().map(|m| f(&m["endmembers"][i], "chemical_energy_J_kg")).sum();
            let energy = f(mat, "density") * energy_sum;
            let min_factor = factors.iter().copied().fold(f64::INFINITY, f64::min);
            if min_factor < f64::MIN_POSITIVE || !creep.is_finite() || !energy.is_finite() {
                return contract(
                    "combined ageing properties or chemical capacity exceed finite normal range",
                );
            }
        }
    }
    let Some((shape, activity)) = real_array(&s["activities"]) else {
        return contract("activities must be finite real numbers");
    };
    let expected = [crate::util::time_count(context), crate::util::grid_cells(context), 2];
    if shape != expected || activity.iter().any(|a| !(0.0..=1.0).contains(a)) {
        return contract(format!(
            "local activities must have shape ({}, {}, 2) and lie in [0,1]",
            expected[0], expected[1]
        ));
    }
    Ok(s.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Endpoint {
    rate: f64,
    activation: f64,
    t_ref: f64,
    initial: f64,
    conductivity: f64,
    yield_remaining: f64,
    creep_log: f64,
    chemical: f64,
    maxwell: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct Mechanism {
    kind: &'static str,
    endpoints: [Endpoint; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentalAgeing {
    mechanisms: Vec<Mechanism>,
}

impl EnvironmentalAgeing {
    #[must_use]
    pub fn bind(settings: &Value) -> Self {
        let mechanisms = settings["mechanisms"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                let kind = KINDS.iter().copied().find(|k| m["kind"] == json!(k)).unwrap_or("thermal");
                let e = |i: usize| {
                    let v = &m["endmembers"][i];
                    Endpoint {
                        rate: f(v, "rate_ref_s_inv"),
                        activation: f(v, "activation_J_mol"),
                        t_ref: f(v, "T_ref_K"),
                        initial: f(v, "initial_extent"),
                        conductivity: f(v, "conductivity_remaining"),
                        yield_remaining: f(v, "yield_remaining"),
                        creep_log: f(v, "creep_log_multiplier"),
                        chemical: f(v, "chemical_energy_J_kg"),
                        maxwell: v.get("maxwell_stiffness_remaining").and_then(Value::as_f64),
                    }
                };
                Mechanism { kind, endpoints: [e(0), e(1)] }
            })
            .collect();
        Self { mechanisms }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        2 * self.mechanisms.len()
    }

    #[must_use]
    pub fn state_metadata(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for m in &self.mechanisms {
            for (i, e) in m.endpoints.iter().enumerate() {
                out.push(json!({"name": format!("{}_extent_endpoint_{i}", m.kind), "units": "1", "scale": 1.0, "initial": e.initial}));
            }
        }
        out
    }

    #[must_use]
    pub fn declares_maxwell(&self) -> bool {
        self.mechanisms.iter().all(|m| m.endpoints.iter().all(|e| e.maxwell.is_some()))
    }


    pub fn maxwell_factor<S: Scalar>(&self, state: &[S], composition: S) -> Result<S, CaeError> {
        let mut factors = [S::one(), S::one()];
        for (i, factor) in factors.iter_mut().enumerate() {
            for (j, m) in self.mechanisms.iter().enumerate() {
                let Some(r) = m.endpoints[i].maxwell else {
                    return contract(
                        "environmental ageing endpoint lacks maxwell_stiffness_remaining required by the Maxwell coupling",
                    );
                };
                *factor *= -(state[2 * j + i] * (1.0 - r)) + 1.0;
            }
        }
        Ok((-composition + 1.0) * factors[0] + composition * factors[1])
    }

    fn rate_constant<S: Scalar>(e: &Endpoint, t: S) -> S {
        (-(S::one() / t - 1.0 / e.t_ref) * (e.activation / R_GAS)).exp() * e.rate
    }

    fn activity<S: Scalar>(kind: &str, forcing: &[S]) -> S {
        match kind {
            "thermal" => S::one(),
            "oxidative" => forcing[0],
            _ => forcing[1],
        }
    }

    pub fn rates<S: Scalar>(&self, state: &[S], t: S, forcing: &[S], out: &mut [S]) {
        for (j, m) in self.mechanisms.iter().enumerate() {
            let activity = Self::activity(m.kind, forcing);
            for (i, e) in m.endpoints.iter().enumerate() {
                let k = Self::rate_constant(e, t) * activity;
                out[2 * j + i] = k * (-state[2 * j + i] + 1.0);
            }
        }
    }

    pub fn residual<S: Scalar>(
        &self,
        state: &[S],
        previous: &[S],
        t: S,
        dt: S,
        forcing: &[S],
        out: &mut [S],
    ) {
        self.rates(state, t, forcing, out);
        for i in 0..self.size() {
            out[i] = state[i] - previous[i] - dt * out[i];
        }
    }

    pub fn properties<S: Scalar>(&self, endpoint: usize, state: &[S], base: [S; 3]) -> [S; 3] {
        let [mut k, mut y, mut creep] = base;
        for (j, m) in self.mechanisms.iter().enumerate() {
            let e = &m.endpoints[endpoint];
            let a = state[2 * j + endpoint];
            k *= -(a * (1.0 - e.conductivity)) + 1.0;
            y *= -(a * (1.0 - e.yield_remaining)) + 1.0;
            creep *= (a * e.creep_log).exp();
        }
        [k, y, creep]
    }

    pub fn energy<S: Scalar>(
        &self,
        state: &[S],
        t: S,
        forcing: &[S],
        c: S,
        densities: [f64; 2],
    ) -> HistoryEnergy<S> {
        let mut rates = vec![S::zero(); self.size()];
        self.rates(state, t, forcing, &mut rates);
        let mut out = HistoryEnergy::zero();
        for (j, m) in self.mechanisms.iter().enumerate() {
            for (i, e) in m.endpoints.iter().enumerate() {
                let weight = if i == 0 { -c + 1.0 } else { c };
                let capacity = weight * densities[i] * e.chemical;
                out.stored += capacity * (-state[2 * j + i] + 1.0);
                out.sensible_heat += capacity * rates[2 * j + i];
            }
        }
        out
    }


    pub fn check_state(&self, state: &[f64], width: usize) -> Result<(), CaeError> {
        if width != self.size() || width == 0 || !state.len().is_multiple_of(width) || state.iter().any(|v| !v.is_finite() || *v < -1e-9 || *v > 1.0 + 1e-9) {
            return convergence("ageing extent outside [0,1]; never clamped");
        }
        Ok(())
    }
}

fn real_list(value: &Value, label: &str) -> Result<(Vec<usize>, Vec<f64>), CaeError> {
    match real_array(value) {
        Some((shape, data)) if data.iter().all(Scalar::is_finite) => Ok((shape, data)),
        _ => contract(format!("{label} requires finite real numbers")),
    }
}


pub fn observe_temperature_history(
    settings: &Value,
    context: &Value,
    temperature: &Value,
    composition: &Value,
) -> Result<Value, CaeError> {
    let (tshape, times) = real_list(&context["times_s"], "times_s")?;
    if tshape.len() != 1 || times.len() < 2 || times[0] < 0.0 || times.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return contract("ageing times_s must be increasing nonnegative physical seconds");
    }
    let (gshape, grid) = real_list(&context["grid"], "grid")?;
    if gshape.len() != 1 || grid.len() != 3 || grid.iter().any(|g| *g < 1.0 || g.fract() != 0.0) {
        return contract("ageing grid requires three positive integers");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cells = grid.iter().map(|g| *g as usize).product::<usize>();
    let (ts, t) = real_list(temperature, "temperature_K")?;
    let (cs, c) = real_list(composition, "composition")?;
    if ts != [times.len(), cells] || cs != [cells] || c.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return contract("ageing requires [time,cell] temperatures and fixed [cell] composition in [0,1]");
    }
    let Some(materials) = context["materials"].as_array().filter(|m| m.len() == 2) else {
        return contract("ageing requires two ordered material endmembers");
    };
    for mat in materials {
        let (_, values) = real_list(&json!([mat["density"], mat["T_min"], mat["T_max"]]), "material")?;
        let (lo, hi) = (values[1], values[2]);
        if values[0] <= 0.0 || !(0.0 < lo && lo <= hi) || t.iter().any(|v| *v < lo || *v > hi) {
            return contract("actual temperatures must stay within both material calibration intervals");
        }
    }
    let s = validate(settings, context)?;
    let law = EnvironmentalAgeing::bind(&s);
    let (_, activities) = real_array(&s["activities"]).unwrap_or_default();
    let width = law.size();
    let nt = times.len();
    let mut state = vec![0.0; nt * cells * width];
    let initial: Vec<f64> = law.mechanisms.iter().flat_map(|m| m.endpoints.map(|e| e.initial)).collect();
    for cell in 0..cells {
        state[cell * width..(cell + 1) * width].copy_from_slice(&initial);
    }
    let mut capacity = vec![0.0; cells * width];
    for (j, m) in law.mechanisms.iter().enumerate() {
        for (i, e) in m.endpoints.iter().enumerate() {
            for cell in 0..cells {
                let w = if i == 1 { c[cell] } else { 1.0 - c[cell] };
                capacity[cell * width + 2 * j + i] = w * f(&materials[i], "density") * e.chemical;
            }
        }
    }
    let mut power = vec![0.0; (nt - 1) * cells];
    for n in 1..nt {
        let dt = times[n] - times[n - 1];
        for cell in 0..cells {
            let mut kd = Vec::with_capacity(width);
            for m in &law.mechanisms {
                let activity = match m.kind {
                    "thermal" => 1.0,
                    "oxidative" => activities[(n * cells + cell) * 2],
                    _ => activities[(n * cells + cell) * 2 + 1],
                };
                for e in &m.endpoints {
                    let k = e.rate
                        * (-e.activation / R_GAS * (1.0 / t[n * cells + cell] - 1.0 / e.t_ref)).exp()
                        * activity;
                    kd.push(k * dt);
                }
            }
            if kd.iter().any(|v| !v.is_finite()) {
                return convergence("ageing interval exceeds finite integration range");
            }
            let mut heat = 0.0;
            for (q, kdq) in kd.iter().enumerate() {
                let old = state[((n - 1) * cells + cell) * width + q];
                let increment = (1.0 - old) * (kdq / (1.0 + kdq));
                state[(n * cells + cell) * width + q] = old + increment;
                heat += capacity[cell * width + q] * (increment / dt);
            }
            power[(n - 1) * cells + cell] = heat;
        }
    }
    law.check_state(&state, width)?;
    let mut stored = vec![0.0; nt * cells];
    for n in 0..nt {
        for cell in 0..cells {
            stored[n * cells + cell] = (0..width)
                .map(|q| capacity[cell * width + q] * (1.0 - state[(n * cells + cell) * width + q]))
                .sum();
        }
    }
    if power.iter().chain(&stored).any(|v| !v.is_finite()) {
        return convergence("ageing energy accounting exceeds finite range");
    }
    let (tmin, tmax) = t.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    Ok(json!({"mode": "observer_only", "sampling": "backward_euler_right_endpoint",
        "times_s": times, "extent": nested(&[nt, cells, width], &state),
        "stored_energy_J_m3": nested(&[nt, cells], &stored), "interval_heat_W_m3": nested(&[nt - 1, cells], &power),
        "temperature_range_K": [tmin, tmax], "feedback_applied": false, "topology_objective_supported": false}))
}


pub fn prepare_cell_observer(
    specification: &Value,
    shape: &[usize],
    times: &[f64],
    cell_indices: Option<&[i64]>,
) -> Result<(Value, Value, Vec<f64>), CaeError> {
    if !has_exact_keys(specification, &["composition", "history_byte_budget", "materials", "settings"]) {
        return contract(
            "ageing observer requires settings, two materials, grid composition and history_byte_budget",
        );
    }
    let budget = match &specification["history_byte_budget"] {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(i64::MAX),
        _ => 0,
    };
    if budget < 1 {
        return contract("ageing observer history_byte_budget must be a positive integer");
    }
    let materials = &specification["materials"];
    let mut context = json!({"grid": shape, "times_s": times, "materials": materials});
    let Some(rows) = materials
        .as_array()
        .filter(|m| m.len() == 2 && m.iter().all(|r| has_exact_keys(r, &["T_max", "T_min", "density"])))
    else {
        return contract("observer materials require density (kg/m³), T_min and T_max (K)");
    };
    for m in rows {
        if !m.as_object().is_some_and(|o| o.values().all(|v| num(v).is_some())) {
            return contract("observer material coefficients must be finite real numbers");
        }
        if f(m, "density") <= 0.0 || !(0.0 < f(m, "T_min") && f(m, "T_min") <= f(m, "T_max")) {
            return contract("observer materials require positive density and valid temperature limits");
        }
    }
    let Some((cshape, composition)) = real_array(&specification["composition"]) else {

        let shape_ok = shape_of(&specification["composition"]) == shape;
        if !shape_ok {
            return contract("observer composition must match the native cell grid");
        }
        return contract("observer composition must contain finite real fractions in [0,1]");
    };
    if cshape != shape {
        return contract("observer composition must match the native cell grid");
    }
    if composition.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
        return contract("observer composition must contain finite real fractions in [0,1]");
    }
    let mut composition = composition;
    if let Some(indices) = cell_indices {
        let n = composition.len();
        let mut sorted = indices.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if indices.is_empty()
            || sorted.len() != indices.len()
            || indices.iter().any(|i| *i < 0 || usize::try_from(*i).unwrap_or(usize::MAX) >= n)
        {
            return contract("observer cell indices must be unique in-range integers");
        }
        composition = indices.iter().map(|i| composition[usize::try_from(*i).unwrap_or(0)]).collect();
        context["grid"] = json!([indices.len(), 1, 1]);
    }
    let settings = validate(&specification["settings"], &context)?;
    let mechanisms = settings["mechanisms"].as_array().map_or(0, Vec::len);
    let required = times.len() * composition.len() * (2 * mechanisms + 4) * 8;
    if i64::try_from(required).unwrap_or(i64::MAX) > budget {
        return contract(
            "ageing observer numeric history exceeds its authored byte budget; JSON overhead is additional",
        );
    }
    Ok((settings, context, composition))
}

fn shape_of(v: &Value) -> Vec<usize> {
    let mut shape = Vec::new();
    let mut level = vec![v];
    loop {
        let lens: Vec<Option<usize>> = level.iter().map(|x| x.as_array().map(Vec::len)).collect();
        match lens.first().copied().flatten() {
            Some(n) if lens.iter().all(|l| *l == Some(n)) => {
                shape.push(n);
                level = level.iter().flat_map(|x| x.as_array().into_iter().flatten()).collect();
                if n == 0 {
                    return shape;
                }
            }
            _ => return shape,
        }
    }
}
