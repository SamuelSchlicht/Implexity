// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, AddInRegistry, ContractInput, Fidelity, PortSpec,
};
use implexity_core::packages::InstallContext;

use crate::water_saturation::{SaturationLaw, law_temperature, saturation_component};

pub const OBSERVER_KIND: &str = "history_response_observer";
pub const IMPLEMENTATION: &str = "implexity.physics_library.liquid_admissibility.LiquidOnlyHistory";
pub const REQUIRES: [&str; 4] =
    ["pressure_absolute_Pa", "fluid_temperature_K", "phase_cell_temperature_upper_K", "fluid_fraction"];
pub const LIMITATIONS: [&str; 3] = [
    "Observer/admissibility constraint; NOT a two-phase/CHF/nucleation model.",
    "Nodal upper-temperature bound is not a resolved wetted-wall temperature.",
    "Temperatures must remain inside the saturation-law correlation range; no freezing/metastable-phase model is provided.",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ObserverSample<S> {
    pub pressure_absolute_pa: Vec<S>,
    pub fluid_temperature_k: Vec<S>,
    pub phase_cell_temperature_upper_k: Vec<S>,
    pub fluid_fraction: Vec<S>,
}

impl ObserverSample<f64> {
    pub(crate) fn well_formed(&self) -> bool {
        let n = self.fluid_fraction.len();
        let arrays = [
            &self.pressure_absolute_pa,
            &self.fluid_temperature_k,
            &self.phase_cell_temperature_upper_k,
            &self.fluid_fraction,
        ];
        arrays.iter().all(|a| a.len() == n && a.iter().copied().all(f64::is_finite))
    }
}

pub(crate) fn soft_minimum<S: Scalar>(entries: &[S], scale: f64) -> S {
    if entries.is_empty() {
        return S::from_f64(f64::NAN);
    }
    let m = entries.iter().fold(f64::NEG_INFINITY, |a, e| a.max(e.value()));
    let mut total = S::zero();
    for (i, e) in entries.iter().enumerate() {
        let t = (*e - m).exp();
        total = if i == 0 { t } else { total + t };
    }
    -(total.ln() + m) * scale
}

pub(crate) fn log_weight<S: Scalar>(eps: S, cutoff: f64) -> S {
    (eps / cutoff).ln()
}

fn number(settings: &Map<String, Value>, key: &str) -> Result<f64, CaeError> {
    settings
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| CaeError::contract("finite liquid validity parameters required"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiquidGuardSettings {
    pub saturation_component: String,
    pub saturation_settings: Value,
    pub phase_fraction_threshold: f64,
    pub softmin_scale_k: f64,
    pub minimum_subcooling_k: f64,
    pub provenance: String,
    raw: Value,
}

impl LiquidGuardSettings {
    fn from_value(p: &Value) -> Result<Self, CaeError> {
        let Some(m) = p.as_object() else {
            return Err(CaeError::contract(format!("liquid guard requires {}", key_list())));
        };
        Ok(Self {
            saturation_component: m
                .get("saturation_component")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            saturation_settings: m.get("saturation_settings").cloned().unwrap_or(Value::Null),
            phase_fraction_threshold: number(m, "phase_fraction_threshold")?,
            softmin_scale_k: number(m, "softmin_scale_K")?,
            minimum_subcooling_k: number(m, "minimum_subcooling_K")?,
            provenance: m.get("provenance").and_then(Value::as_str).unwrap_or_default().to_string(),
            raw: p.clone(),
        })
    }

    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

const GUARD_KEYS: [&str; 6] = [
    "minimum_subcooling_K",
    "phase_fraction_threshold",
    "provenance",
    "saturation_component",
    "saturation_settings",
    "softmin_scale_K",
];

fn key_list() -> String {
    let quoted: Vec<String> = GUARD_KEYS.iter().map(|k| implexity_core::py_repr::repr_str(k)).collect();
    format!("[{}]", quoted.join(", "))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LiquidOnlyHistory;

impl LiquidOnlyHistory {
    #[must_use]
    pub fn response_units() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("fluid_subcooling_bound_K".to_string(), "K".to_string()),
            ("phase_cell_nodal_subcooling_bound_K".to_string(), "K".to_string()),
        ])
    }

    #[must_use]
    pub fn runtime_support() -> Map<String, Value> {
        json!({"status": "field_component", "history": true,
               "data": "explicit saturation law, phase threshold and validity margin", "limitations": LIMITATIONS})
        .as_object()
        .cloned()
        .unwrap_or_default()
    }


    pub fn validate(&self, addins: &AddInRegistry, p: &Value) -> Result<Value, CaeError> {
        let keys_ok = p
            .as_object()
            .is_some_and(|m| m.len() == GUARD_KEYS.len() && GUARD_KEYS.iter().all(|k| m.contains_key(*k)));
        if !keys_ok {
            return Err(CaeError::contract(format!("liquid guard requires {}", key_list())));
        }
        if p["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err(CaeError::contract("liquid-only admissibility provenance required"));
        }
        for name in ["phase_fraction_threshold", "softmin_scale_K", "minimum_subcooling_K"] {
            if !p[name].as_f64().is_some_and(f64::is_finite) {
                return Err(CaeError::contract("finite liquid validity parameters required"));
            }
        }
        let s = LiquidGuardSettings::from_value(p)?;
        if !(0.0 < s.phase_fraction_threshold && s.phase_fraction_threshold <= 1.0)
            || s.softmin_scale_k <= 0.0
            || s.minimum_subcooling_k < 0.0
        {
            return Err(CaeError::contract("invalid liquid-only validity margin/threshold/smoothing"));
        }
        let law = saturation_component(addins, p["saturation_component"].as_str().unwrap_or_default())?;
        law.validate(&p["saturation_settings"])?;
        Ok(p.clone())
    }


    pub fn bind(&self, addins: &AddInRegistry, settings: &Value) -> Result<BoundLiquid, CaeError> {
        let s = LiquidGuardSettings::from_value(settings)?;
        let law = saturation_component(addins, &s.saturation_component)?;
        Ok(BoundLiquid { settings: s, law })
    }
}

impl AddInAdapter for LiquidOnlyHistory {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(Self::runtime_support())
    }
    fn component_kind(&self) -> Option<String> {
        Some(OBSERVER_KIND.into())
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        Some(Self::response_units())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct BoundLiquid {
    pub settings: LiquidGuardSettings,
    law: Arc<dyn SaturationLaw>,
}

impl std::fmt::Debug for BoundLiquid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundLiquid").field("settings", &self.settings).finish_non_exhaustive()
    }
}

impl BoundLiquid {
    #[must_use]
    pub fn field_metadata(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for name in ["fluid_saturation_temperature_K", "fluid_subcooling_K", "phase_cell_nodal_subcooling_K"]
        {
            m.insert(
                name.into(),
                json!({"units": "K", "association": "cell", "rank": "scalar",
                       "phase_mask": "liquid_guard_reporting_mask",
                       "source": "active_saturation_component_on_solved_absolute_pressure_NOT_CHF"}),
            );
        }
        m.insert(
            "liquid_guard_reporting_mask".into(),
            json!({"units": "1", "association": "cell", "rank": "scalar",
                   "source": "explicit_observer_phase_reporting_threshold"}),
        );
        m
    }


    pub fn check(&self, samples: &[ObserverSample<f64>]) -> Result<Value, CaeError> {
        let mut rows = Vec::new();
        let law = self.law.as_ref();
        let threshold = self.settings.phase_fraction_threshold;
        for (n, s) in samples.iter().enumerate() {
            if !s.well_formed() || s.fluid_fraction.iter().any(|e| !(0.0..=1.0).contains(e)) {
                return Err(CaeError::contract("invalid liquid observer field sample"));
            }
            let active: Vec<bool> = s.fluid_fraction.iter().map(|e| *e > 0.0).collect();
            let qualified: Vec<bool> = s.fluid_fraction.iter().map(|e| *e >= threshold).collect();
            if !qualified.iter().any(|q| *q) {
                return Err(CaeError::convergence(
                    "no fluid cells meet the declared physical phase threshold; liquid safety cannot be asserted",
                ));
            }
            let pick = |v: &[f64], mask: &[bool]| -> Vec<f64> {
                v.iter().zip(mask).filter(|(_, m)| **m).map(|(x, _)| *x).collect()
            };
            let domain = law
                .pressure_check(&pick(&s.pressure_absolute_pa, &active))
                .and_then(|()| law.temperature_check(&pick(&s.fluid_temperature_k, &active)))
                .and_then(|()| law.temperature_check(&pick(&s.phase_cell_temperature_upper_k, &active)));
            if let Err(e) = domain {
                return Err(CaeError::convergence(format!(
                    "liquid state outside saturation-law pressure/temperature correlation domain: {e}"
                )));
            }
            let sat: Vec<f64> = s
                .pressure_absolute_pa
                .iter()
                .zip(&active)
                .map(|(p, a)| law.temperature_jet(if *a { *p } else { 1e5 }).0)
                .collect();
            let m: Vec<f64> = sat.iter().zip(&s.fluid_temperature_k).map(|(a, b)| a - b).collect();
            let mu: Vec<f64> =
                sat.iter().zip(&s.phase_cell_temperature_upper_k).map(|(a, b)| a - b).collect();
            let m_act = pick(&m, &active);
            let mu_act = pick(&mu, &active);
            if !m_act.iter().chain(&mu_act).copied().all(f64::is_finite) {
                return Err(CaeError::convergence("liquid phase margin is nonfinite"));
            }
            if m_act.iter().chain(&mu_act).any(|v| *v < 0.0) {
                return Err(CaeError::convergence(format!(
                    "negative liquid-only phase margin on actual fluid support at sample {n}; reporting threshold cannot hide a phase-model violation"
                )));
            }
            let min = |v: Vec<f64>| v.into_iter().fold(f64::INFINITY, f64::min);
            let m_q = min(pick(&m, &qualified));
            let mu_q = min(pick(&mu, &qualified));
            if m_q.min(mu_q) < self.settings.minimum_subcooling_k {
                return Err(CaeError::convergence(format!(
                    "authored positive subcooling reserve not met at sample {n}; strict reserve requirement remains enforced"
                )));
            }
            let pq = pick(&s.pressure_absolute_pa, &qualified);
            rows.push(json!({
                "sample": n,
                "minimum_fluid_subcooling_K": m_q,
                "minimum_phase_cell_nodal_subcooling_K": mu_q,
                "qualified_cells": qualified.iter().filter(|q| **q).count(),
                "pressure_min_Pa": pq.iter().copied().fold(f64::INFINITY, f64::min),
                "pressure_max_Pa": pq.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            }));
        }
        if rows.is_empty() {
            return Err(CaeError::contract("liquid admissibility requires at least one field sample"));
        }
        Ok(json!({
            "samples": rows,
            "liquid_only_regime_valid": true,
            "chf_or_dnbr_computed": false,
            "phase_fraction_threshold": self.settings.phase_fraction_threshold,
            "bound_definition": "unnormalized phase-weighted soft minimum over every stored time, including initial; w=fluid_fraction/threshold; conservative for cells above threshold",
            "minimum_required_subcooling_K": self.settings.minimum_subcooling_k,
            "saturation_component": self.settings.saturation_component,
        }))
    }

    #[must_use]
    pub fn values<S: Scalar>(&self, samples: &[ObserverSample<S>]) -> [S; 2] {
        let scale = self.settings.softmin_scale_k;
        let cutoff = self.settings.phase_fraction_threshold;
        let (mut fluid, mut upper) = (Vec::new(), Vec::new());
        for s in samples {
            for k in 0..s.fluid_fraction.len() {
                let eps = s.fluid_fraction[k];
                if eps.value() <= 0.0 {
                    continue;
                }
                let sat = law_temperature(self.law.as_ref(), s.pressure_absolute_pa[k]);
                let logw = log_weight(eps, cutoff);
                fluid.push(logw - (sat - s.fluid_temperature_k[k]) / scale);
                upper.push(logw - (sat - s.phase_cell_temperature_upper_k[k]) / scale);
            }
        }
        [soft_minimum(&fluid, scale), soft_minimum(&upper, scale)]
    }


    pub fn fields(&self, s: &ObserverSample<f64>) -> Result<BTreeMap<String, Vec<f64>>, CaeError> {
        let active: Vec<bool> = s.fluid_fraction.iter().map(|e| *e > 0.0).collect();
        let act_p: Vec<f64> =
            s.pressure_absolute_pa.iter().zip(&active).filter(|(_, a)| **a).map(|(p, _)| *p).collect();
        self.law.pressure_check(&act_p)?;
        let sat: Vec<f64> = s
            .pressure_absolute_pa
            .iter()
            .zip(&active)
            .map(|(p, a)| self.law.temperature_jet(if *a { *p } else { 1e5 }).0)
            .collect();
        let masked = |f: &dyn Fn(usize) -> f64| -> Vec<f64> {
            (0..active.len()).map(|k| if active[k] { f(k) } else { 0.0 }).collect()
        };
        Ok(BTreeMap::from([
            ("fluid_saturation_temperature_K".to_string(), masked(&|k| sat[k])),
            ("fluid_subcooling_K".to_string(), masked(&|k| sat[k] - s.fluid_temperature_k[k])),
            (
                "phase_cell_nodal_subcooling_K".to_string(),
                masked(&|k| sat[k] - s.phase_cell_temperature_upper_k[k]),
            ),
            (
                "liquid_guard_reporting_mask".to_string(),
                s.fluid_fraction
                    .iter()
                    .map(|e| if *e >= self.settings.phase_fraction_threshold { 1.0 } else { 0.0 })
                    .collect(),
            ),
        ]))
    }
}

#[must_use]
pub fn contract() -> AddInContract {
    let mut c = AddInContract::new("liquid_only_history_guard");
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new("liquid_history_admissibility")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
    c
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    ctx.register_addin(ContractInput::Typed(Box::new(contract())), Some(Arc::new(LiquidOnlyHistory)))
}
