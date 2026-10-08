// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;

use crate::mandel::{Mandel, dev, dot, equivalent};
use crate::material::{Props, SolidMaterial, idx};

pub const YIELD_SWITCH_CERTIFICATE: f64 = 1e-8;
pub const R_GAS: f64 = 8.314_462_618_153_24;

fn contract<T>(message: impl Into<String>) -> Result<T, CaeError> {
    Err(CaeError::contract(message.into()))
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlasticLaw {
    J2LinearHardening,
    J2Chaboche,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreepLaw {
    Norton,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkEnergy<S> {
    pub stored: S,
    pub previous_stored: S,
    pub stored_increment: S,
    pub backward_euler_defect: S,
    pub coefficient_exchange: S,
    pub dissipation: S,
    pub work: S,
    pub work_identity_defect: S,
}

impl<S: Scalar> WorkEnergy<S> {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({"stored_energy_J_m3": self.stored.value(),
            "previous_stored_energy_J_m3": self.previous_stored.value(),
            "stored_energy_increment_J_m3": self.stored_increment.value(),
            "backward_euler_defect_J_m3": self.backward_euler_defect.value(),
            "coefficient_exchange_J_m3": self.coefficient_exchange.value(),
            "dissipation_increment_J_m3": self.dissipation.value(),
            "endpoint_work_J_m3": self.work.value(),
            "work_identity_defect_J_m3": self.work_identity_defect.value()})
    }
}

fn mandel<S: Scalar>(a: &[S]) -> Mandel<S> {
    std::array::from_fn(|i| a[i])
}

impl PlasticLaw {
    #[must_use]
    pub fn component_id(self) -> &'static str {
        match self {
            Self::J2LinearHardening => "j2_linear_hardening",
            Self::J2Chaboche => "j2_chaboche",
        }
    }

    #[must_use]
    pub fn implementation(self) -> &'static str {
        match self {
            Self::J2LinearHardening => "implexity.physics_library.inelastic_components.J2LinearHardening",
            Self::J2Chaboche => "implexity.physics_library.chaboche.J2Chaboche",
        }
    }

    #[must_use]
    pub fn limitations(self) -> &'static [&'static str] {
        match self {
            Self::J2LinearHardening => &[
                "Small strain; linear isotropic and Prager kinematic hardening.",
                "Piecewise differentiable active set; no damage or rupture law.",
            ],
            Self::J2Chaboche => &[
                "Small-strain associated J2; nonlinear kinematic hardening only; 1..8 branches.",
                "Requires Chaboche-compatible properties. No fatigue-life or rupture law.",
                "Incremental plastic work/storage split includes BE numerical dissipation and coefficient-energy exchange; not a complete thermoelastic free-energy model.",
            ],
        }
    }

    #[must_use]
    pub fn runtime_support(self) -> Map<String, Value> {
        obj(json!({"status": "field_component", "history": true, "data": "user_required",
            "limitations": self.limitations()}))
    }

    #[must_use]
    pub fn authoring_contract(self) -> Option<Map<String, Value>> {
        (self == Self::J2Chaboche).then(|| {
            obj(json!({"schema": "implexity-component-authoring/1", "selection": "solid.components.plasticity",
                "requires_material_component": "temperature_tabulated_chaboche_solid",
                "internal_layout": "plastic strain Mandel6, accumulated equivalent plastic strain, then 6 strain-like backstrain components per branch",
                "initial_state": "zero plastic strain and backstrain only in the current stress-free host initialiser",
                "law": "d a_j = d eps_p - gamma_j a_j d p; alpha_j = (2/3) C_j(T) a_j",
                "temperature_convention": "thermal rescaling of backstress through C(T); no static recovery",
                "calibrated_fatigue_life": false}))
        })
    }

    #[must_use]
    pub fn heat_coupling(self) -> Value {
        json!({"dissipation_multiplier": "taylor_quinney",
            "coefficient_storage_exchange": self == Self::J2Chaboche})
    }


    pub fn state_size(self, materials: &[SolidMaterial]) -> Result<usize, CaeError> {
        match self {
            Self::J2LinearHardening => Ok(7),
            Self::J2Chaboche => {
                self.validate_models(materials)?;
                Ok(7 + 6 * branch_count(&materials[0]))
            }
        }
    }


    pub fn validate_models(self, materials: &[SolidMaterial]) -> Result<(), CaeError> {
        if self == Self::J2LinearHardening {
            return Ok(());
        }
        for m in materials {
            crate::material::MaterialLaw::ChabocheTable.validate(&Value::Object(m.raw.clone()))?;
        }
        let first = materials.first().map_or(0, branch_count);
        if materials.iter().any(|m| branch_count(m) != first) {
            return contract("all mixed endpoints must declare the same ordered hardening branches");
        }
        Ok(())
    }


    pub fn state_metadata(self, materials: &[SolidMaterial]) -> Result<Vec<Value>, CaeError> {
        if self == Self::J2LinearHardening {
            return Ok(Vec::new());
        }
        let n = (self.state_size(materials)? - 7) / 6;
        Ok((0..n)
            .map(|i| json!({"name": format!("backstrain_{i}_mandel"), "offset": 7 + 6 * i, "components": 6, "units": "1"}))
            .collect())
    }

    #[must_use]
    pub fn has_yield_function(self) -> bool {
        self == Self::J2Chaboche
    }

    pub fn backstress<S: Scalar>(self, state: &[S], prop: &Props<S>) -> Mandel<S> {
        match self {
            Self::J2LinearHardening => {
                let h = prop.get(idx::H_KIN) * (2.0 / 3.0);
                std::array::from_fn(|i| h * state[i])
            }
            Self::J2Chaboche => chaboche_backstress(state, prop),
        }
    }

    pub fn yield_function<S: Scalar>(self, stress: &Mandel<S>, state: &[S], prop: &Props<S>) -> S {
        let back = self.backstress(state, prop);
        let xi: Mandel<S> = std::array::from_fn(|i| dev(stress)[i] - back[i]);
        match self {
            Self::J2LinearHardening => {
                equivalent(&xi) - prop.get(idx::YIELD) - prop.get(idx::H_ISO) * state[6]
            }
            Self::J2Chaboche => equivalent(&xi) - prop.get(idx::YIELD),
        }
    }

    pub fn residual<S: Scalar>(
        self,
        stress: &Mandel<S>,
        current: &[S],
        previous: &[S],
        prop: &Props<S>,
        e_scale: f64,
        out: &mut [S],
    ) {
        match self {
            Self::J2LinearHardening => {
                let p = current[6];
                let back = self.backstress(current, prop);
                let d = dev(stress);
                let xi: Mandel<S> = std::array::from_fn(|i| d[i] - back[i]);
                let q = equivalent(&xi);
                let dg = p - previous[6];
                let f = q - prop.get(idx::YIELD) - prop.get(idx::H_ISO) * p;
                let consistency = dg - S::zero().maximum(dg + f / e_scale);
                for i in 0..6 {
                    let normal = xi[i] * 1.5 / q;
                    out[i] = current[i] - previous[i] - dg * normal;
                }
                out[6] = consistency;
            }
            Self::J2Chaboche => {
                let dp = current[6] - previous[6];
                let back = chaboche_backstress(current, prop);
                let d = dev(stress);
                let xi: Mandel<S> = std::array::from_fn(|i| d[i] - back[i]);
                let q = equivalent(&xi);
                let f = q - prop.get(idx::YIELD);
                for i in 0..6 {
                    out[i] = current[i] - previous[i] - dp * (xi[i] * 1.5 / q);
                }
                out[6] = current[6] - previous[6] - S::zero().maximum(dp + f / e_scale);
                for j in 0..branches(current.len()) {
                    let o = 7 + 6 * j;
                    for i in 0..6 {
                        let dep = current[i] - previous[i];
                        out[o + i] =
                            current[o + i] - previous[o + i] - dep + prop.kin_gamma[j] * current[o + i] * dp;
                    }
                }
            }
        }
    }

    pub fn switch_distance<S: Scalar>(
        self,
        stress: &Mandel<S>,
        current: &[S],
        previous: &[S],
        prop: &Props<S>,
        e_scale: f64,
    ) -> Option<S> {
        Some({
            let p = current[6];
            let back = self.backstress(current, prop);
            let d = dev(stress);
            let xi: Mandel<S> = std::array::from_fn(|i| d[i] - back[i]);
            let q = equivalent(&xi);
            let dg = p - previous[6];
            let f = match self {
                Self::J2LinearHardening => q - prop.get(idx::YIELD) - prop.get(idx::H_ISO) * p,
                Self::J2Chaboche => q - prop.get(idx::YIELD),
            };
            (dg + f / e_scale).abs()
        })
    }

    pub fn dissipated_increment<S: Scalar>(self, current: &[S], previous: &[S], prop: &Props<S>) -> S {
        let dp = current[6] - previous[6];
        match self {
            Self::J2LinearHardening => prop.get(idx::YIELD) * dp,
            Self::J2Chaboche => {
                let mut out = prop.get(idx::YIELD) * dp;
                for j in 0..branches(current.len()) {
                    let o = 7 + 6 * j;
                    let a = mandel(&current[o..o + 6]);
                    let old = mandel(&previous[o..o + 6]);
                    let (c, g) = (prop.kin_c[j], prop.kin_gamma[j]);
                    let diff: Mandel<S> = std::array::from_fn(|i| (a[i] - old[i]).powi(2));
                    out = out
                        + c * (2.0 / 3.0) * g * sum6(&std::array::from_fn(|i| a[i] * a[i])) * dp
                        + c / 3.0 * sum6(&diff);
                }
                out
            }
        }
    }

    pub fn stored_energy<S: Scalar>(self, state: &[S], prop: &Props<S>) -> S {
        let mut out = S::zero();
        if self == Self::J2Chaboche {
            for j in 0..branches(state.len()) {
                let o = 7 + 6 * j;
                let a = mandel(&state[o..o + 6]);
                out += prop.kin_c[j] / 3.0 * sum6(&std::array::from_fn(|i| a[i] * a[i]));
            }
        }
        out
    }

    pub fn heat<S: Scalar>(
        self,
        current: &[S],
        previous: &[S],
        prop: &Props<S>,
        previous_prop: &Props<S>,
    ) -> S {
        let tq = prop.get(idx::TAYLOR_QUINNEY) * self.dissipated_increment(current, previous, prop);
        match self {
            Self::J2LinearHardening => tq,
            Self::J2Chaboche => {
                let exchange =
                    self.stored_energy(previous, prop) - self.stored_energy(previous, previous_prop);
                tq - exchange
            }
        }
    }

    pub fn work_energy_increment<S: Scalar>(
        self,
        stress: &Mandel<S>,
        current: &[S],
        previous: &[S],
        prop: &Props<S>,
        previous_prop: &Props<S>,
    ) -> Option<WorkEnergy<S>> {
        if self != Self::J2LinearHardening {
            return None;
        }
        let ep = mandel(&current[..6]);
        let eo = mandel(&previous[..6]);
        let (p, po) = (current[6], previous[6]);
        let dep: Mandel<S> = std::array::from_fn(|i| ep[i] - eo[i]);
        let dp = p - po;
        let (hi, hk) = (prop.get(idx::H_ISO), prop.get(idx::H_KIN));
        let (phi, phk) = (previous_prop.get(idx::H_ISO), previous_prop.get(idx::H_KIN));
        let stored = hi * 0.5 * p * p + hk / 3.0 * dot(&ep, &ep);
        let old_stored = phi * 0.5 * po * po + phk / 3.0 * dot(&eo, &eo);
        let exchange = (hi - phi) * 0.5 * po * po + (hk - phk) / 3.0 * dot(&eo, &eo);
        let sum: Mandel<S> = std::array::from_fn(|i| ep[i] + eo[i]);
        let delta = hi * 0.5 * dp * (p + po) + hk / 3.0 * dot(&dep, &sum) + exchange;
        let numerical = hi * 0.5 * dp * dp + hk / 3.0 * dot(&dep, &dep);
        let dissipation = self.dissipated_increment(current, previous, prop);
        let work = dot(stress, &dep);
        Some(WorkEnergy {
            stored,
            previous_stored: old_stored,
            stored_increment: delta,
            backward_euler_defect: numerical,
            coefficient_exchange: exchange,
            dissipation,
            work,
            work_identity_defect: work - delta - numerical + exchange - dissipation,
        })
    }
}

fn sum6<S: Scalar>(a: &Mandel<S>) -> S {
    a[0] + a[1] + a[2] + a[3] + a[4] + a[5]
}

#[must_use]
pub fn branches(width: usize) -> usize {
    width.saturating_sub(7) / 6
}

fn branch_count(m: &SolidMaterial) -> usize {
    m.kinematic.as_ref().map_or(0, |k| k.len())
}

fn chaboche_backstress<S: Scalar>(state: &[S], prop: &Props<S>) -> Mandel<S> {
    let mut out = [S::zero(); 6];
    for j in 0..branches(state.len()) {
        let o = 7 + 6 * j;
        let c = prop.kin_c[j] * (2.0 / 3.0);
        for i in 0..6 {
            out[i] += c * state[o + i];
        }
    }
    out
}

impl CreepLaw {
    #[must_use]
    pub fn component_id(self) -> &'static str {
        "norton_creep"
    }

    #[must_use]
    pub fn implementation(self) -> &'static str {
        "implexity.physics_library.inelastic_components.NortonCreep"
    }

    #[must_use]
    pub fn limitations(self) -> &'static [&'static str] {
        &["Isotropic secondary Norton creep; no primary/tertiary creep or rupture law."]
    }

    #[must_use]
    pub fn runtime_support(self) -> Map<String, Value> {
        obj(json!({"status": "field_component", "history": true, "data": "user_required",
            "limitations": self.limitations()}))
    }

    #[must_use]
    pub fn heat_coupling(self) -> Value {
        json!({"dissipation_multiplier": "unity", "coefficient_storage_exchange": false})
    }

    pub fn increment<S: Scalar>(self, stress: &Mandel<S>, prop: &Props<S>, dt: S) -> S {
        let q = equivalent(stress);
        let arrhenius = (-prop.get(idx::CREEP_ACTIVATION) / R_GAS
            * (S::one() / prop.temperature - S::one() / prop.get(idx::CREEP_T_REF)))
        .exp();
        let rate = prop.get(idx::CREEP_RATE)
            * arrhenius
            * (q / prop.get(idx::CREEP_STRESS)).pow(prop.get(idx::CREEP_EXPONENT));
        dt * rate
    }

    pub fn residual<S: Scalar>(
        self,
        stress: &Mandel<S>,
        current: &[S],
        previous: &[S],
        prop: &Props<S>,
        dt: S,
        out: &mut [S],
    ) {
        let increment = self.increment(stress, prop, dt);
        let q = equivalent(stress);
        let d = dev(stress);
        for i in 0..6 {
            let flow = d[i] * 1.5 / q;
            out[i] = current[i] - previous[i] - increment * flow;
        }
        out[6] = current[6] - previous[6] - increment;
    }

    pub fn dissipated_increment<S: Scalar>(self, stress: &Mandel<S>, current: &[S], previous: &[S]) -> S {
        let mut out = S::zero();
        for i in 0..6 {
            out += stress[i] * (current[i] - previous[i]);
        }
        out
    }

    pub fn work_energy_increment<S: Scalar>(
        self,
        stress: &Mandel<S>,
        current: &[S],
        previous: &[S],
    ) -> WorkEnergy<S> {
        let work = self.dissipated_increment(stress, current, previous);
        let zero = S::zero();
        WorkEnergy {
            stored: zero,
            previous_stored: zero,
            stored_increment: zero,
            backward_euler_defect: zero,
            coefficient_exchange: zero,
            dissipation: work,
            work,
            work_identity_defect: zero,
        }
    }

    #[must_use]
    pub fn solid_study_templates(self, solid: &Value) -> Vec<Value> {
        let Some(components) = solid.get("components").and_then(Value::as_object) else { return Vec::new() };
        if components.get("creep").is_some_and(|v| !v.is_null()) {
            return Vec::new();
        }
        let keys = [
            ("creep_rate_ref", "Reference creep strain rate", "1/s"),
            ("creep_stress_ref", "Reference creep stress", "Pa"),
            ("creep_exponent", "Creep stress exponent", "1"),
            ("creep_activation_J_mol", "Creep activation energy", "J/mol"),
            ("creep_T_ref", "Reference creep temperature", "K"),
        ];
        let zero = solid.get("materials").and_then(Value::as_array).is_some_and(|ms| {
            ms.iter().any(|m| m.get("creep_rate_ref").and_then(Value::as_f64).is_some_and(|v| v == 0.0))
        });
        let mut properties = Map::new();
        for (key, title, unit) in keys {
            properties.insert(
                key.into(),
                json!({"title": title, "unit": unit, "type": "number", "description": "Norton creep coefficient (selected creep law)."}),
            );
        }
        let description = format!(
            "Selects norton_creep: rate = creep_rate_ref·exp(-Q/R·(1/T-1/T_ref))·(σ_eq/σ_ref)^n in every material endpoint. The coefficients are the creep_* entries of each material card{} Secondary creep only; no primary/tertiary creep or rupture.",
            if zero {
                ". At least one endpoint has creep_rate_ref = 0, so creep stays inactive until you author calibrated coefficients."
            } else {
                "."
            }
        );
        vec![json!({"id": "norton_creep_setup", "label": "Add Norton secondary creep",
            "description": description,
            "truth_status": "unvalidated_user_coefficient_authoring_starter",
            "problem_requirements": [{"path": ["components", "creep"], "value": null, "missing_equals_null": true}],
            "problem_patch": {"components": {"creep": "norton_creep"}},
            "editor_schema_patch": {"properties": {"materials": {"items": {"properties": properties}}}}})]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InelasticLayout {
    pub plastic_size: usize,
    pub creep_size: usize,
    pub viscoelastic_size: usize,
}

impl Default for InelasticLayout {
    fn default() -> Self {
        Self { plastic_size: 7, creep_size: 7, viscoelastic_size: 0 }
    }
}

impl InelasticLayout {
    #[must_use]
    pub fn plastic(&self) -> std::ops::Range<usize> {
        0..self.plastic_size
    }
    #[must_use]
    pub fn plastic_strain(&self) -> std::ops::Range<usize> {
        0..if self.plastic_size > 0 { 6 } else { 0 }
    }
    #[must_use]
    pub fn plastic_accumulation(&self) -> Option<usize> {
        (self.plastic_size > 0).then_some(6)
    }
    #[must_use]
    pub fn creep(&self) -> std::ops::Range<usize> {
        self.plastic_size..self.plastic_size + self.creep_size
    }
    #[must_use]
    pub fn creep_strain(&self) -> std::ops::Range<usize> {
        self.plastic_size..self.plastic_size + if self.creep_size > 0 { 6 } else { 0 }
    }
    #[must_use]
    pub fn creep_accumulation(&self) -> Option<usize> {
        (self.creep_size > 0).then_some(self.plastic_size + 6)
    }
    #[must_use]
    pub fn viscoelastic(&self) -> std::ops::Range<usize> {
        self.plastic_size + self.creep_size..self.material_start()
    }
    #[must_use]
    pub fn material_start(&self) -> usize {
        self.plastic_size + self.creep_size + self.viscoelastic_size
    }
}


pub fn layout_for(
    plastic: Option<PlasticLaw>,
    creep: Option<CreepLaw>,
    viscoelastic_size: Option<usize>,
    materials: &[SolidMaterial],
) -> Result<InelasticLayout, CaeError> {
    let size = match plastic {
        None => 0,
        Some(p) => {
            let size = p.state_size(materials)?;
            if !(7..=103).contains(&size) {
                return contract("plastic component must declare 7..103 strain-like native coordinates");
            }
            if size > 7 && !p.has_yield_function() {
                return contract(
                    "extended plastic state requires an explicit yield_function for diagnostic consistency",
                );
            }
            p.validate_models(materials)?;
            size
        }
    };
    Ok(InelasticLayout {
        plastic_size: size,
        creep_size: if creep.is_some() { 7 } else { 0 },
        viscoelastic_size: viscoelastic_size.unwrap_or(0),
    })
}

#[must_use]
pub fn state_scales(
    layout: &InelasticLayout,
    strain_scale: f64,
    viscoelastic: Option<&[f64]>,
    material_history: Option<&[f64]>,
) -> Vec<f64> {
    let mut out = vec![strain_scale; layout.plastic_size + layout.creep_size];
    out.extend_from_slice(viscoelastic.unwrap_or(&[]));
    out.extend_from_slice(material_history.unwrap_or(&[]));
    out
}
