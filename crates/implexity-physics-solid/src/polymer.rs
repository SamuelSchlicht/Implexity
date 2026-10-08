// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;

use crate::history::{HistoryLaw, MaterialHistoryBinding};
use crate::mandel::{IDENTITY, Mandel, dev};
use crate::util::{contract, convergence, f, has_exact_keys, num, obj, text};

pub const R_GAS: f64 = 8.314_462_618_153_24;
pub const SCHEMA: &str = "implexity-native-maxwell-polymer/1";
pub const COMPONENT_ID: &str = "native_maxwell_polymer";
pub const IMPLEMENTATION: &str = "implexity.physics_library.polymer_ageing.NativeMaxwellPolymer";
pub const COMPONENT_KIND: &str = "viscoelastic_solid";
pub const LIMITATIONS: [&str; 4] = [
    "Single homogeneous small-strain isotropic spectrum; no implicit endpoint rheology mixing.",
    "No simultaneous plasticity or creep; environmental material history requires the explicit energy-consistent Maxwell coupling.",
    "Moduli depend on degradation only; Arrhenius time shifts are user calibrated.",
    "Physical viscous/degradation heat excludes separately reported backward-Euler numerical dissipation.",
];
const SETTINGS_KEYS: [&str; 12] = [
    "T_max_K",
    "T_min_K",
    "T_ref_K",
    "bulk_branches",
    "degradation",
    "equilibrium_bulk_Pa",
    "equilibrium_shear_Pa",
    "maximum_strain",
    "name",
    "provenance",
    "schema",
    "shear_branches",
];
const BRANCH_KEYS: [&str; 3] = ["activation_J_mol", "modulus_Pa", "relaxation_time_s"];
const DEGRADATION_KEYS: [&str; 6] = [
    "activation_J_mol",
    "chemical_energy_J_m3",
    "initial_extent",
    "provenance",
    "rate_ref_s_inv",
    "residual_stiffness",
];

#[must_use]
pub fn runtime_support() -> Map<String, Value> {
    obj(
        json!({"status": "field_component", "history": true, "data": "user_required", "limitations": LIMITATIONS}),
    )
}

#[must_use]
pub fn authoring_contract() -> Map<String, Value> {
    obj(json!({"schema": SCHEMA,
        "state": "6 deviatoric Mandel strains per shear branch, one volumetric strain per bulk branch, degradation extent",
        "required_settings": ["schema", "name", "provenance", "T_ref_K", "T_min_K", "T_max_K", "maximum_strain", "equilibrium_shear_Pa", "equilibrium_bulk_Pa", "shear_branches", "bulk_branches", "degradation"],
        "branch_required": BRANCH_KEYS, "degradation_required": DEGRADATION_KEYS,
        "units": {"modulus_Pa": "Pa", "equilibrium_shear_Pa": "Pa", "equilibrium_bulk_Pa": "Pa", "relaxation_time_s": "s", "activation_J_mol": "J/mol", "chemical_energy_J_m3": "J/m^3", "rate_ref_s_inv": "1/s", "maximum_strain": "Mandel norm, dimensionless", "T_ref_K": "K", "T_min_K": "K", "T_max_K": "K", "residual_stiffness": "1", "initial_extent": "1"},
        "rate_shift": "exp[-Q/R*(1/T-1/T_ref)]",
        "degradation_law": "d a/dt = rate_ref_s_inv*shift*(1-a); stiffness factor=1-(1-residual_stiffness)*a",
        "initial_state": "zero viscous branch strain; explicit initial_extent",
        "temperature_moduli": "constant before degradation; no implicit thermoelastic modulus law",
        "chemical_energy": "stored chemical energy H*(1-extent); irreversible release H*d_extent"}))
}

pub const EDITOR_LABEL: &str = "Polymer relaxation and degradation";

#[must_use]
pub fn editor_schema(settings: &Value, context: &Value) -> Value {
    if !settings.is_object() {
        return json!({});
    }
    let environmental = context
        .get("material_history")
        .and_then(|m| m.get("component"))
        .is_some_and(|c| c == "environmental_ageing");
    let number = |title: &str, unit: &str, extra: Value| {
        let mut row = json!({"title": title, "units": unit, "type": "number"});
        let extra = extra.as_object().cloned().unwrap_or_default();
        if !extra.contains_key("minimum") {
            row["exclusiveMinimum"] = json!(0);
        }
        for (k, v) in extra {
            row[k] = v;
        }
        row
    };
    let branch = json!({"type": "object", "properties": {
        "modulus_Pa": number("Branch modulus", "Pa", json!({"description": "Positive shear or bulk modulus for this branch."})),
        "relaxation_time_s": number("Reference relaxation time", "s", json!({"description": "Positive relaxation time at the reference temperature."})),
        "activation_J_mol": number("Relaxation activation energy", "J/mol", json!({"minimum": 0}))}});
    let mut rate_extra = json!({"minimum": 0});
    let mut energy_extra = json!({"minimum": 0});
    if environmental {
        rate_extra["maximum"] = json!(0);
        energy_extra["maximum"] = json!(0);
    }
    let description = if environmental {
        "Environmental ageing owns degradation and chemical heat: keep the built-in rate, initial extent and chemical energy at zero."
    } else {
        "Set the reference rate to zero to disable evolution. Physical heat excludes backward-Euler numerical dissipation."
    };
    json!({"properties": {
        "schema": {"title": "Constitutive declaration version", "enum": [SCHEMA]},
        "name": {"title": "Polymer model name"}, "provenance": {"title": "Material data and calibration provenance"},
        "T_ref_K": number("Reference temperature", "K", json!({})),
        "T_min_K": number("Minimum valid temperature", "K", json!({})),
        "T_max_K": number("Maximum valid temperature", "K", json!({})),
        "maximum_strain": number("Maximum strain norm", "1", json!({"description": "Positive small-strain validity limit; Mandel strain norm."})),
        "equilibrium_shear_Pa": number("Equilibrium shear modulus", "Pa", json!({})),
        "equilibrium_bulk_Pa": number("Equilibrium bulk modulus", "Pa", json!({})),
        "shear_branches": {"title": "Shear relaxation branches", "type": "array", "maxItems": 8, "items": branch.clone(), "description": "Up to eight branches. At least one shear or bulk branch is required."},
        "bulk_branches": {"title": "Bulk relaxation branches", "type": "array", "maxItems": 8, "items": branch, "description": "Up to eight volumetric branches; leave empty when not used."},
        "degradation": {"title": "Irreversible degradation", "description": description, "properties": {
            "rate_ref_s_inv": number("Reference degradation rate", "1/s", rate_extra),
            "activation_J_mol": number("Degradation activation energy", "J/mol", json!({"minimum": 0})),
            "residual_stiffness": number("Remaining stiffness fraction at full degradation", "1", json!({"maximum": 1, "description": "Strictly positive and at most one."})),
            "initial_extent": number("Initial degradation extent", "1", json!({"minimum": 0, "maximum": i32::from(!environmental)})),
            "chemical_energy_J_m3": number("Stored chemical energy at zero degradation", "J/m³", energy_extra),
            "provenance": {"title": "Degradation calibration provenance"}}}}})
}


pub fn validate(s: &Value) -> Result<Value, CaeError> {
    if !has_exact_keys(s, &SETTINGS_KEYS) || s["schema"] != json!(SCHEMA) {
        return contract(format!(
            "native Maxwell settings require exactly {} with schema {SCHEMA}",
            crate::util::sorted_repr(SETTINGS_KEYS)
        ));
    }
    if !(text(&s["name"]) && text(&s["provenance"])) {
        return contract("Maxwell material name and calibration provenance required");
    }
    let scalars =
        ["T_ref_K", "T_min_K", "T_max_K", "maximum_strain", "equilibrium_shear_Pa", "equilibrium_bulk_Pa"];
    if scalars.iter().any(|k| num(&s[*k]).is_none_or(|v| v <= 0.0)) {
        return contract("Maxwell moduli, temperatures and maximum strain must be positive finite scalars");
    }
    let (lo, t_ref, hi) = (f(s, "T_min_K"), f(s, "T_ref_K"), f(s, "T_max_K"));
    if !(lo <= t_ref && t_ref <= hi) || lo >= hi {
        return contract("invalid Maxwell absolute-temperature interval");
    }
    for kind in ["shear_branches", "bulk_branches"] {
        let Some(rows) = s[kind].as_array().filter(|r| r.len() <= 8) else {
            return contract("each Maxwell spectrum requires a list of at most eight branches");
        };
        for row in rows {
            if !has_exact_keys(row, &BRANCH_KEYS) || BRANCH_KEYS.iter().any(|k| num(&row[*k]).is_none()) {
                return contract(
                    "Maxwell branch requires finite modulus_Pa, relaxation_time_s and activation_J_mol",
                );
            }
            if f(row, "modulus_Pa") <= 0.0
                || f(row, "relaxation_time_s") <= 0.0
                || f(row, "activation_J_mol") < 0.0
            {
                return contract("branch modulus/time must be positive and activation nonnegative");
            }
        }
    }
    if s["shear_branches"].as_array().is_some_and(Vec::is_empty)
        && s["bulk_branches"].as_array().is_some_and(Vec::is_empty)
    {
        return contract("at least one Maxwell branch must be authored");
    }
    let d = &s["degradation"];
    if !has_exact_keys(d, &DEGRADATION_KEYS) || !text(&d["provenance"]) {
        return contract(
            "explicit degradation coefficients and provenance required, including when disabled",
        );
    }
    if DEGRADATION_KEYS.iter().filter(|k| **k != "provenance").any(|k| num(&d[*k]).is_none()) {
        return contract("degradation coefficients must be finite");
    }
    let rs = f(d, "residual_stiffness");
    if f(d, "rate_ref_s_inv").min(f(d, "activation_J_mol")).min(f(d, "chemical_energy_J_m3")) < 0.0
        || !(0.0 < rs && rs <= 1.0)
        || !(0.0..=1.0).contains(&f(d, "initial_extent"))
    {
        return contract("invalid irreversible degradation bounds");
    }
    let branches: Vec<&Value> = ["shear_branches", "bulk_branches"]
        .iter()
        .flat_map(|k| s[*k].as_array().into_iter().flatten())
        .collect();
    let energies: Vec<f64> =
        branches.iter().map(|b| f(b, "activation_J_mol")).chain([f(d, "activation_J_mol")]).collect();
    for q in energies {
        for t in [lo, hi] {
            let shift = -q / R_GAS * (1.0 / t - 1.0 / t_ref);
            if !shift.is_finite() || shift.abs() > 600.0 {
                return contract("authored Arrhenius shift exceeds finite numerical range");
            }
        }
    }
    for t in [lo, hi] {
        for b in &branches {
            let rate = (-f(b, "activation_J_mol") / R_GAS * (1.0 / t - 1.0 / t_ref)).exp()
                / f(b, "relaxation_time_s");
            if !rate.is_finite() || rate < f64::MIN_POSITIVE {
                return contract(
                    "temperature-adjusted Maxwell relaxation rate is outside the finite normal range",
                );
            }
        }
        let rate =
            f(d, "rate_ref_s_inv") * (-f(d, "activation_J_mol") / R_GAS * (1.0 / t - 1.0 / t_ref)).exp();
        if !rate.is_finite() || (f(d, "rate_ref_s_inv") > 0.0 && rate < f64::MIN_POSITIVE) {
            return contract(
                "temperature-adjusted polymer degradation rate is outside the finite normal range",
            );
        }
    }
    Ok(s.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Branch {
    modulus: f64,
    time: f64,
    activation: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaxwellResponse<S> {
    pub stress: Mandel<S>,
    pub residual: Vec<S>,
    pub stored_energy: S,
    pub mechanical_stored_energy: S,
    pub dissipation_increment: S,
    pub heat_increment: S,
    pub coefficient_exchange: S,
    pub chemical_release: S,
    pub numerical_dissipation_increment: S,
    pub energy_balance_residual: S,
    pub strain_margin: S,
}

impl<S: Scalar> MaxwellResponse<S> {
    #[must_use]
    pub fn diagnostics(&self) -> [(&'static str, S); 9] {
        [
            ("stored_energy_J_m3", self.stored_energy),
            ("mechanical_stored_energy_J_m3", self.mechanical_stored_energy),
            ("dissipation_increment_J_m3", self.dissipation_increment),
            ("heat_increment_J_m3", self.heat_increment),
            ("coefficient_exchange_J_m3", self.coefficient_exchange),
            ("chemical_release_J_m3", self.chemical_release),
            ("numerical_dissipation_increment_J_m3", self.numerical_dissipation_increment),
            ("energy_balance_residual_J_m3", self.energy_balance_residual),
            ("strain_margin", self.strain_margin),
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundMaxwell {
    pub settings: Value,
    shear: Vec<Branch>,
    bulk: Vec<Branch>,
    t_ref: f64,
    t_min: f64,
    t_max: f64,
    maximum_strain: f64,
    g_eq: f64,
    k_eq: f64,
    residual_stiffness: f64,
    rate_ref: f64,
    degradation_activation: f64,
    initial_extent: f64,
    chemical_energy: f64,
    pub environmental: Option<MaterialHistoryBinding>,
}

impl BoundMaxwell {
    fn new(settings: Value, environmental: Option<MaterialHistoryBinding>) -> Self {
        let branches = |k: &str| -> Vec<Branch> {
            settings[k]
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| Branch {
                    modulus: f(b, "modulus_Pa"),
                    time: f(b, "relaxation_time_s"),
                    activation: f(b, "activation_J_mol"),
                })
                .collect()
        };
        let d = &settings["degradation"];
        Self {
            shear: branches("shear_branches"),
            bulk: branches("bulk_branches"),
            t_ref: f(&settings, "T_ref_K"),
            t_min: f(&settings, "T_min_K"),
            t_max: f(&settings, "T_max_K"),
            maximum_strain: f(&settings, "maximum_strain"),
            g_eq: f(&settings, "equilibrium_shear_Pa"),
            k_eq: f(&settings, "equilibrium_bulk_Pa"),
            residual_stiffness: f(d, "residual_stiffness"),
            rate_ref: f(d, "rate_ref_s_inv"),
            degradation_activation: f(d, "activation_J_mol"),
            initial_extent: f(d, "initial_extent"),
            chemical_energy: f(d, "chemical_energy_J_m3"),
            settings,
            environmental,
        }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        6 * self.shear.len() + self.bulk.len() + 1
    }

    #[must_use]
    pub fn metadata(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        for i in 0..self.shear.len() {
            for j in 0..6 {
                rows.push(json!({"name": format!("maxwell_shear_{i}_mandel_{j}"), "units": "1", "scale": 1.0, "initial": 0.0}));
            }
        }
        for i in 0..self.bulk.len() {
            rows.push(
                json!({"name": format!("maxwell_bulk_{i}"), "units": "1", "scale": 1.0, "initial": 0.0}),
            );
        }
        rows.push(json!({"name": "polymer_degradation_extent", "units": "1", "scale": 1.0, "initial": self.initial_extent}));
        rows
    }

    #[must_use]
    pub fn scales(&self) -> Vec<f64> {
        vec![1.0; self.size()]
    }

    #[must_use]
    pub fn initial(&self) -> Vec<f64> {
        let mut v = vec![0.0; self.size()];
        if let Some(last) = v.last_mut() {
            *last = self.initial_extent;
        }
        v
    }


    pub fn check_state(&self, state: &[f64], temperature: &[f64]) -> Result<(), CaeError> {
        let n = self.size();
        if !state.len().is_multiple_of(n) || state.iter().any(|v| !v.is_finite()) {
            return convergence("invalid Maxwell branch state or degradation extent");
        }
        if state.chunks(n).any(|r| r[n - 1] < -1e-10 || r[n - 1] > 1.0 + 1e-10) {
            return convergence("invalid Maxwell branch state or degradation extent");
        }
        if temperature.len() != 1 && temperature.len() != state.len() / n {
            return convergence("Maxwell temperature must be scalar or match the state batch");
        }
        for row in state.chunks(n) {
            for i in 0..self.shear.len() {
                if (row[6 * i] + row[6 * i + 1] + row[6 * i + 2]).abs() > 1e-8 {
                    return convergence("Maxwell shear branch is not deviatoric");
                }
            }
        }
        if temperature.iter().any(|t| !t.is_finite() || *t < self.t_min || *t > self.t_max) {
            return convergence("Maxwell temperature outside authored validity");
        }
        Ok(())
    }

    fn shift<S: Scalar>(&self, q: f64, t: S) -> S {
        (-(S::one() / t - 1.0 / self.t_ref) * (q / R_GAS)).exp()
    }


    #[allow(clippy::too_many_arguments)]
    pub fn response<S: Scalar>(
        &self,
        strain: &Mandel<S>,
        current: &[S],
        previous: &[S],
        temperature: S,
        dt: S,
        previous_strain: &Mandel<S>,
        material: Option<(&[S], &[S], S)>,
    ) -> Result<MaxwellResponse<S>, CaeError> {
        let (external, old_external) = match &self.environmental {
            None => (S::one(), S::one()),
            Some(mh) => {
                let Some((mc, mp, c)) = material else {
                    return contract(
                        "environmental Maxwell response requires current/previous material extents and composition",
                    );
                };
                let HistoryLaw::Ageing(law) = &mh.law else {
                    return contract(
                        "Maxwell supports only the explicit environmental-ageing material history coupling",
                    );
                };
                (law.maxwell_factor(mc, c)?, law.maxwell_factor(mp, c)?)
            }
        };
        Ok(self.response_with_factors(
            strain,
            current,
            previous,
            temperature,
            dt,
            previous_strain,
            external,
            old_external,
        ))
    }

    #[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
    pub fn response_with_factors<S: Scalar>(
        &self,
        e: &Mandel<S>,
        z: &[S],
        old: &[S],
        t: S,
        dt: S,
        old_e: &Mandel<S>,
        external: S,
        old_external: S,
    ) -> MaxwellResponse<S> {
        let sum = |v: &Mandel<S>| v[0] + v[1] + v[2] + v[3] + v[4] + v[5];
        let sq = |v: &Mandel<S>| sum(&std::array::from_fn(|i| v[i] * v[i]));
        let ed = dev(e);
        let od = dev(old_e);
        let tr = e[0] + e[1] + e[2];
        let otr = old_e[0] + old_e[1] + old_e[2];
        let n = self.size();
        let (a, oa) = (z[n - 1], old[n - 1]);
        let factor = (-(a * (1.0 - self.residual_stiffness)) + 1.0) * external;
        let oldfactor = (-(oa * (1.0 - self.residual_stiffness)) + 1.0) * old_external;
        let (g, k) = (self.g_eq, self.k_eq);
        let mut sigma: Mandel<S> = std::array::from_fn(|i| ed[i] * (2.0 * g) + tr * k * IDENTITY[i]);
        let mut storage = sq(&ed) * g + tr * tr * (0.5 * k);
        let mut oldstorage = sq(&od) * g + otr * otr * (0.5 * k);
        let dd: Mandel<S> = std::array::from_fn(|i| (ed[i] - od[i]).powi(2));
        let mut numerical = sum(&dd) * g + (tr - otr).powi(2) * (0.5 * k);
        let mut diss = S::zero();
        let mut rows = Vec::with_capacity(n);
        for (i, b) in self.shear.iter().enumerate() {
            let q: Mandel<S> = std::array::from_fn(|j| z[6 * i + j]);
            let oq: Mandel<S> = std::array::from_fn(|j| old[6 * i + j]);
            let v: Mandel<S> = std::array::from_fn(|j| ed[j] - q[j]);
            let ov: Mandel<S> = std::array::from_fn(|j| od[j] - oq[j]);
            let rate = self.shift(b.activation, t) / b.time;
            let gm = b.modulus;
            for j in 0..6 {
                rows.push(q[j] - oq[j] - dt * rate * v[j]);
                sigma[j] += v[j] * (2.0 * gm);
            }
            storage += sq(&v) * gm;
            oldstorage += sq(&ov) * gm;
            diss += dt * (2.0 * gm) * rate * sq(&v);
            let dv: Mandel<S> = std::array::from_fn(|j| (v[j] - ov[j]).powi(2));
            numerical += sum(&dv) * gm;
        }
        let offset = 6 * self.shear.len();
        for (i, b) in self.bulk.iter().enumerate() {
            let q = z[offset + i];
            let oq = old[offset + i];
            let v = tr - q;
            let ov = otr - oq;
            let rate = self.shift(b.activation, t) / b.time;
            let km = b.modulus;
            rows.push(q - oq - dt * rate * v);
            for j in 0..6 {
                sigma[j] += v * km * IDENTITY[j];
            }
            storage += v * v * (0.5 * km);
            oldstorage += ov * ov * (0.5 * km);
            diss += dt * km * rate * v * v;
            numerical += (v - ov).powi(2) * (0.5 * km);
        }
        let ageing_rate = self.shift(self.degradation_activation, t) * self.rate_ref;
        rows.push(a - oa - dt * ageing_rate * (-a + 1.0));
        let exchange = (factor - oldfactor) * oldstorage;
        let chemical_release = (a - oa) * self.chemical_energy;
        let heat = factor * diss - exchange + chemical_release;
        let stress: Mandel<S> = std::array::from_fn(|j| factor * sigma[j]);
        let total = factor * storage + (-a + 1.0) * self.chemical_energy;
        let oldtotal = oldfactor * oldstorage + (-oa + 1.0) * self.chemical_energy;
        let num = factor * numerical;
        let work = sum(&std::array::from_fn(|j| stress[j] * (e[j] - old_e[j])));
        let balance = work - (total - oldtotal) - heat - num;
        let norm = sq(e).sqrt();
        MaxwellResponse {
            stress,
            residual: rows,
            stored_energy: total,
            mechanical_stored_energy: factor * storage,
            dissipation_increment: factor * diss,
            heat_increment: heat,
            coefficient_exchange: exchange,
            chemical_release,
            numerical_dissipation_increment: num,
            energy_balance_residual: balance,
            strain_margin: -norm + self.maximum_strain,
        }
    }
}


pub fn bind_viscoelastic(row: &Value, context: &Value) -> Result<Option<BoundMaxwell>, CaeError> {
    if row.is_null() {
        return Ok(None);
    }
    if !has_exact_keys(row, &["component", "settings"]) || row["component"] != json!(COMPONENT_ID) {
        return contract("viscoelasticity requires component=native_maxwell_polymer and settings");
    }
    let components = context.get("components");
    let selected = |k: &str| components.and_then(|c| c.get(k)).is_some_and(|v| !v.is_null());
    if selected("plasticity") || selected("creep") {
        return contract("native Maxwell cannot be combined with plasticity or creep");
    }
    let present = |k: &str| context.get(k).is_some_and(|v| !v.is_null());
    if present("inactive_phase_numerical_material") || present("inactive_solid_numerical_material") {
        return contract("native Maxwell has no qualified inactive-phase material continuation");
    }
    let settings = validate(&row["settings"])?;
    let mut environmental = None;
    if present("material_history") {
        let mh = &context["material_history"];
        if mh.get("component") != Some(&json!("environmental_ageing")) {
            return contract(
                "Maxwell supports only the explicit environmental-ageing material history coupling",
            );
        }
        let binding = crate::history::bind(mh, context)?;
        if let Some(b) = &binding {
            for mechanism in b.settings["mechanisms"].as_array().into_iter().flatten() {
                for e in mechanism["endmembers"].as_array().into_iter().flatten() {
                    #[allow(clippy::float_cmp)]
                    let neutral = e.get("maxwell_stiffness_remaining").is_some()
                        && f(e, "yield_remaining") == 1.0
                        && f(e, "creep_log_multiplier") == 0.0;
                    if !neutral {
                        return contract(
                            "environmental Maxwell requires explicit stiffness fractions and neutral unused yield/creep modifiers",
                        );
                    }
                }
            }
        }
        let d = &settings["degradation"];
        if ["rate_ref_s_inv", "initial_extent", "chemical_energy_J_m3"].iter().any(|k| f(d, k) != 0.0) {
            return contract(
                "disable built-in Maxwell degradation when environmental ageing owns degradation and chemical heat",
            );
        }
        environmental = binding;
    }
    Ok(Some(BoundMaxwell::new(settings, environmental)))
}

#[must_use]
pub fn with_environmental_maxwell_couplings(
    mut base: implexity_core::coupling_graph::CouplingDeclaration,
    solid: &Value,
) -> implexity_core::coupling_graph::CouplingDeclaration {
    let visco = solid.get("viscoelasticity").is_some_and(crate::history::ageing::truthy);
    let ageing = solid
        .get("material_history")
        .and_then(|m| m.get("component"))
        .is_some_and(|c| c == "environmental_ageing");
    if !visco || !ageing {
        return base;
    }
    base.edges.retain(|e| e.quantity != "evolved_strength_and_creep");
    base.edges.push(crate::coupling::edge(
        "material_state",
        "structure",
        "evolved_maxwell_stiffness",
        "monolithic",
        "current environmental extents modify spring stress/storage; coefficient release enters physical heat",
    ));
    base
}
