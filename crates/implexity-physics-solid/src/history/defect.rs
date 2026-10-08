// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;

use super::HistoryEnergy;
use crate::util::{
    contract, convergence, f, grid_cells, has_exact_keys, minimum_yield, nested, num, real_array, text,
    time_count,
};

pub const R_GAS: f64 = 8.314_462_618_153_24;

pub const UNITS: [(&str, &str); 9] = [
    ("production_per_dpa", "1/dpa"),
    ("recovery_rate_ref_s_inv", "1/s"),
    ("activation_J_mol", "J/mol"),
    ("recovery_T_ref_K", "K"),
    ("conductivity_resistance_factor", "1"),
    ("yield_increment_Pa", "Pa"),
    ("creep_log_multiplier", "1"),
    ("stored_energy_J_kg", "J/kg"),
    ("initial_population", "1"),
];

pub const LIMITATIONS: [&str; 4] = [
    "Reduced two-endmember prescribed-dose production/recovery populations, NOT calibrated data for any specific material.",
    "No neutron transport, dose attenuation, transmutation, swelling, He/H evolution, erosion, recrystallisation or rupture.",
    "Conductivity resistance, additive yield shift and exponential creep-rate multiplier need experimental calibration.",
    "Nonthermal defect-source energy is separate from prescribed sensible volumetric heat; recovery heats the solid.",
];

const FORCING: &str = "backward_euler_right_endpoint_native_cell_prescribed_dose";
const ENERGY: &str = "sensible_heat_excludes_nonthermal_defect_source";

fn units_value() -> Value {
    Value::Object(UNITS.iter().map(|(k, u)| ((*k).to_string(), json!(u))).collect())
}

#[must_use]
pub fn authoring_contract() -> Value {
    json!({
        "schema": "implexity-component-authoring/1", "selection": "solid.material_history",
        "required_settings": ["name", "provenance", "endmembers", "units", "dose_rate_dpa_s", "forcing_convention", "energy_convention"],
        "endmember_parameter_units": units_value(), "ordered_endmembers": "same order as solid.materials",
        "forcing_shape": ["len(times_s)", "product(grid)", 2],
        "forcing_convention": FORCING, "energy_convention": ENERGY,
        "evolving_properties": ["k", "yield_stress", "creep_rate_ref"],
        "state_dependencies": ["temperature"], "prescribed_driving_input": "dose_rate_dpa_s",
        "external_forcing_binding": "compatible field_sources may explicitly replace the zero prescribed profile with a solved current-field rate",
        "state_names": ["defect_population_endpoint_0", "defect_population_endpoint_1"],
        "state_units": ["1", "1"], "state_bounds": [0, 1],
        "calibrated_material_data_supplied": false,
        "limitations": ["prescribed dose is input, not computed neutron transport or spectrum-dependent damage",
            "stored defect energy is separate from sensible heat and is released by recovery"]})
}

#[must_use]
pub fn external_forcing_contract() -> Value {
    json!({"quantity": "ordered_endpoint_damage_rate", "units": "dpa/s", "channels": 2,
        "energy": "external_energy_W_m3_is_nonthermal_storage_source"})
}

#[must_use]
pub fn editor_schema(settings: &Value, context: &Value) -> Value {
    if !settings.is_object() {
        return json!({});
    }
    let titles = [
        ("production_per_dpa", "Defect production per dose"),
        ("recovery_rate_ref_s_inv", "Recovery rate at reference temperature"),
        ("activation_J_mol", "Recovery activation energy"),
        ("recovery_T_ref_K", "Recovery reference temperature"),
        ("conductivity_resistance_factor", "Conductivity resistance factor"),
        ("yield_increment_Pa", "Yield-stress change at full population"),
        ("creep_log_multiplier", "Logarithmic creep-rate change at full population"),
        ("stored_energy_J_kg", "Stored energy at full population"),
        ("initial_population", "Initial defect population"),
    ];
    let nonnegative = [
        "production_per_dpa",
        "recovery_rate_ref_s_inv",
        "activation_J_mol",
        "conductivity_resistance_factor",
        "stored_energy_J_kg",
        "initial_population",
    ];
    let mut coefficients = serde_json::Map::new();
    for ((key, unit), (_, title)) in UNITS.iter().zip(titles) {
        let mut row = json!({"title": title, "units": unit, "type": "number"});
        if nonnegative.contains(key) {
            row["minimum"] = json!(0);
        }
        coefficients.insert((*key).into(), row);
    }
    coefficients.insert("conductivity_scattering".into(), json!({"type":"object", "additionalProperties":false,
        "title":"Temperature-dependent conductivity scattering", "required":["constant_s_inv","linear_s_inv_K_inv","quadratic_s_inv_K2_inv","increment_s_inv"],
        "properties":{"constant_s_inv":{"type":"number","minimum":0,"units":"1/s"},"linear_s_inv_K_inv":{"type":"number","minimum":0,"units":"1/(s K)"},"quadratic_s_inv_K2_inv":{"type":"number","minimum":0,"units":"1/(s K^2)"},"increment_s_inv":{"type":"number","minimum":0,"units":"1/s"}},
        "description":"k = k0/(1 + population*increment/(constant + linear*T + quadratic*T*T)); replaces the constant resistance factor, which must be zero."}));
    coefficients["initial_population"]["maximum"] = json!(1);
    let labels = endpoint_labels(context);
    json!({"properties": {
        "endmembers": {"title": "Ordered solid-material parameters", "type": "array", "minItems": 2, "maxItems": 2,
            "prefixItems": labels.iter().map(|l| json!({"title": l, "properties": coefficients.clone()})).collect::<Vec<_>>()},
        "dose_rate_dpa_s": {"title": "Prescribed damage-rate history", "units": "dpa/s", "type": "array",
            "description": "Array [time, native C-order cell, material endpoint]. Use zero prescribed rates when a compatible transport source drives the material state.",
            "items": {"type": "array", "items": {"type": "array", "prefixItems": labels.iter().map(|l| json!({"title": l, "units": "dpa/s", "type": "number", "minimum": 0})).collect::<Vec<_>>()}}},
        "provenance": {"title": "Parameter source and calibration provenance"},
        "forcing_convention": {"title": "Time and cell sampling convention", "enum": [FORCING]},
        "energy_convention": {"title": "Stored-energy accounting", "enum": [ENERGY]}}})
}

pub(crate) fn endpoint_labels(context: &Value) -> Vec<String> {
    let materials = context.get("materials").and_then(Value::as_array);
    (0..2)
        .map(|i| match materials.and_then(|m| m.get(i)) {
            Some(Value::Object(m)) => {
                m.get("name").map_or_else(|| format!("Material {i}"), implexity_core::pyobj::py_str)
            }
            _ => format!("Material {i}"),
        })
        .collect()
}


pub fn validate(settings: &Value, context: &Value) -> Result<Value, CaeError> {
    let required = [
        "dose_rate_dpa_s",
        "endmembers",
        "energy_convention",
        "forcing_convention",
        "name",
        "provenance",
        "units",
    ];
    if !has_exact_keys(settings, &required) {
        return contract(format!("defect kinetics requires {}", crate::util::sorted_repr(required)));
    }
    if !(text(&settings["name"]) && text(&settings["provenance"])) {
        return contract("kinetic name and provenance required");
    }
    if settings["units"] != units_value() {
        return contract("defect kinetics requires declared SI/dpa units");
    }
    if settings["forcing_convention"] != json!(FORCING) {
        return contract("explicit prescribed cell dose convention required; no attenuation is computed");
    }
    if settings["energy_convention"] != json!(ENERGY) {
        return contract(
            "declare sensible heat separately from nonthermal defect energy to prevent double counting",
        );
    }
    let Some(endpoints) = settings["endmembers"].as_array().filter(|e| e.len() == 2) else {
        return contract("two explicit ordered material endpoint kinetics required");
    };
    let keys: Vec<&str> = UNITS.iter().map(|(k, _)| *k).collect();
    for (i, e) in endpoints.iter().enumerate() {
        let mut endpoint_keys = keys.clone();
        if e.get("conductivity_scattering").is_some() { endpoint_keys.push("conductivity_scattering"); }
        if !has_exact_keys(e, &endpoint_keys) {
            return contract("incomplete defect endpoint parameters");
        }
        if keys.iter().any(|k| num(&e[*k]).is_none()) {
            return contract("finite real scalar defect coefficients required");
        }
        let positive = [
            "production_per_dpa",
            "recovery_rate_ref_s_inv",
            "activation_J_mol",
            "conductivity_resistance_factor",
            "stored_energy_J_kg",
        ];
        let p0 = f(e, "initial_population");
        if positive.iter().any(|k| f(e, k) < 0.0)
            || f(e, "recovery_T_ref_K") <= 0.0
            || !(0.0..=1.0).contains(&p0)
        {
            return contract("invalid defect source/recovery/energy or population bounds");
        }
        if let Some(scattering) = e.get("conductivity_scattering") {
            let names = ["constant_s_inv", "linear_s_inv_K_inv", "quadratic_s_inv_K2_inv", "increment_s_inv"];
            if !has_exact_keys(scattering, &names) || names.iter().any(|key| num(&scattering[*key]).is_none_or(|x| x < 0.0)) || f(e, "conductivity_resistance_factor") != 0.0 {
                return contract("conductivity scattering requires finite nonnegative SI coefficients and zero constant resistance factor");
            }
            for temperature in [f(&context["materials"][i], "T_min"), f(&context["materials"][i], "T_max")] {
                let background = f(scattering, "constant_s_inv") + temperature * (f(scattering, "linear_s_inv_K_inv") + temperature * f(scattering, "quadratic_s_inv_K2_inv"));
                if !background.is_finite() || background <= 0.0 || !(f(scattering, "increment_s_inv") / background).is_finite() {
                    return contract("conductivity scattering requires a positive finite background throughout the material interval");
                }
            }
        }
        let m = &context["materials"][i];
        if minimum_yield(m) + f(e, "yield_increment_Pa").min(0.0) <= 0.0 {
            return contract("evolving yield would become nonpositive");
        }
        for t in [f(m, "T_min"), f(m, "T_max")] {
            let exponent = -f(e, "activation_J_mol") / R_GAS * (1.0 / t - 1.0 / f(e, "recovery_T_ref_K"));
            if exponent.abs() > 600.0 || f(e, "creep_log_multiplier").abs() > 600.0 {
                return contract("declared kinetics exceed safe exponential range");
            }
            let recovery = f(e, "recovery_rate_ref_s_inv") * exponent.exp();
            if !recovery.is_finite() {
                return contract("recovery rate exceeds finite range in the declared temperature interval");
            }
        }
    }
    let (nt, nc) = (time_count(context), grid_cells(context));
    let Some((shape, rate)) = real_array(&settings["dose_rate_dpa_s"]) else {
        return contract("dose rates require finite real numeric components");
    };
    if shape != [nt, nc, 2] || rate.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return contract(format!("dose rates require nonnegative finite shape ({nt}, {nc}, 2)"));
    }
    let mut result = settings.clone();
    result["dose_rate_dpa_s"] = nested(&shape, &rate);
    result["endmembers"] = Value::Array(
        endpoints
            .iter()
            .map(|e| {
                Value::Object(
                    e.as_object()
                        .into_iter()
                        .flatten()
                        .map(|(k, v)| (k.clone(), if k == "conductivity_scattering" { v.clone() } else { crate::util::float(v.as_f64().unwrap_or(f64::NAN)) }))
                        .collect(),
                )
            })
            .collect(),
    );
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Endpoint {
    production: f64,
    recovery_rate: f64,
    activation: f64,
    recovery_t_ref: f64,
    resistance: f64,
    scattering: Option<[f64; 4]>,
    yield_increment: f64,
    creep_log: f64,
    stored_energy: f64,
    initial: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DefectKinetics {
    endpoints: [Endpoint; 2],
}

impl DefectKinetics {
    #[must_use]
    pub fn bind(settings: &Value) -> Self {
        let e = |i: usize| {
            let v = &settings["endmembers"][i];
            Endpoint {
                production: f(v, "production_per_dpa"),
                recovery_rate: f(v, "recovery_rate_ref_s_inv"),
                activation: f(v, "activation_J_mol"),
                recovery_t_ref: f(v, "recovery_T_ref_K"),
                resistance: f(v, "conductivity_resistance_factor"),
                scattering: v.get("conductivity_scattering").map(|s| [f(s, "constant_s_inv"), f(s, "linear_s_inv_K_inv"), f(s, "quadratic_s_inv_K2_inv"), f(s, "increment_s_inv")]),
                yield_increment: f(v, "yield_increment_Pa"),
                creep_log: f(v, "creep_log_multiplier"),
                stored_energy: f(v, "stored_energy_J_kg"),
                initial: f(v, "initial_population"),
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
                json!({"name": format!("defect_population_endpoint_{i}"), "units": "1", "scale": 1.0, "initial": e.initial})
            })
            .collect()
    }


    pub fn validate_numerical_extension(&self, settings: &Value, context: &Value) -> Result<(), CaeError> {
        implexity_physics_base::history_numerical_extension::validate_kinetic_range(
            settings,
            context,
            "recovery_rate_ref_s_inv",
            "recovery_T_ref_K",
            "dose_rate_dpa_s",
            &[self.endpoints[0].production, self.endpoints[1].production],
        )
    }

    fn recovery<S: Scalar>(e: &Endpoint, t: S) -> S {
        (-(S::one() / t - 1.0 / e.recovery_t_ref) * (e.activation / R_GAS)).exp() * e.recovery_rate
    }

    pub fn properties<S: Scalar>(&self, endpoint: usize, state: &[S], base: [S; 3]) -> [S; 3] {
        let e = &self.endpoints[endpoint];
        let a = state[endpoint];
        [
            base[0] / (a * e.resistance + 1.0),
            base[1] + a * e.yield_increment,
            base[2] * (a * e.creep_log).exp(),
        ]
    }

    pub fn properties_at_temperature<S: Scalar>(&self, endpoint: usize, state: &[S], base: [S; 3], temperature: S) -> [S; 3] {
        let mut properties = self.properties(endpoint, state, base);
        if let Some([constant, linear, quadratic, increment]) = self.endpoints[endpoint].scattering {
            let background = S::from_f64(constant) + temperature * (S::from_f64(linear) + temperature * quadratic);
            properties[0] = base[0] / (S::one() + state[endpoint] * increment / background);
        }
        properties
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
            let production = forcing[i] * e.production;
            let recovery = Self::recovery(e, temps[i]);
            let target = (previous[i] + dt * production) / ((recovery + production) * dt + 1.0);
            out[i] = state[i] - target;
        }
    }

    pub fn energy<S: Scalar>(
        &self,
        state: &[S],
        temps: [S; 2],
        forcing: &[S],
        c: S,
        densities: [f64; 2],
    ) -> HistoryEnergy<S> {
        let mut out = HistoryEnergy::zero();
        for (i, e) in self.endpoints.iter().enumerate() {
            let a = state[i];
            let source = a.mul_add_f(-1.0, 1.0) * (forcing[i] * e.production);
            let recovery = Self::recovery(e, temps[i]) * a;
            let weight = if i == 0 { -c + 1.0 } else { c };
            let capacity = weight * densities[i] * e.stored_energy;
            out.stored += capacity * a;
            out.sensible_heat += capacity * recovery;
            out.external += capacity * source;
        }
        out
    }


    pub fn check_state(&self, state: &[f64], width: usize) -> Result<(), CaeError> {
        if width != 2
            || !state.len().is_multiple_of(width)
            || state.iter().any(|v| !v.is_finite())
            || state.iter().any(|v| *v < -1e-9 || *v > 1.0 + 1e-9)
        {
            return convergence("defect population outside [0,1]; never clamped");
        }
        Ok(())
    }
}

trait MulAddF {
    fn mul_add_f(self, a: f64, b: f64) -> Self;
}

impl<S: Scalar> MulAddF for S {
    fn mul_add_f(self, a: f64, b: f64) -> Self {
        self * a + b
    }
}
