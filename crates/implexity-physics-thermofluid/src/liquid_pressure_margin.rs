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
use implexity_physics_cfd::pyfmt::fmt_g;

use crate::liquid_admissibility::{OBSERVER_KIND, ObserverSample, log_weight, soft_minimum};
use crate::water_saturation::{SaturationLaw, law_pressure, saturation_component};

pub const RESPONSES: [&str; 2] = ["fluid_pressure_margin_bound_Pa", "phase_cell_pressure_margin_bound_Pa"];
pub const FIELDS: [&str; 3] =
    ["fluid_saturation_pressure_Pa", "fluid_pressure_margin_Pa", "phase_cell_pressure_margin_Pa"];
pub const IMPLEMENTATION: &str = "implexity.physics_library.liquid_pressure_margin.LiquidPressureMargin";
pub const LIMITATIONS: [&str; 5] = [
    "Pressure-margin response/admission check, not a two-phase cavitation residual.",
    "No nuclei population, mass transfer, bubble collapse, erosion, CHF or NPSH calculation.",
    "Cell pressure with a nodal upper-temperature bound is not resolved wall pressure/temperature.",
    "No pressure clipping, pressure-reference substitution or saturation-law extrapolation.",
    "Valid only in the explicitly selected saturation-law temperature domain.",
];
const REQUIRED: [&str; 7] = [
    "minimum_pressure_margin_Pa",
    "phase_fraction_threshold",
    "provenance",
    "reference_temperature_K",
    "saturation_component",
    "saturation_settings",
    "softmin_scale_Pa",
];

#[derive(Debug, Clone, PartialEq)]
pub struct PressureMarginSettings {
    pub saturation_component: String,
    pub reference_temperature_k: f64,
    pub phase_fraction_threshold: f64,
    pub softmin_scale_pa: f64,
    pub minimum_pressure_margin_pa: f64,
    raw: Value,
}

impl PressureMarginSettings {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LiquidPressureMargin;

impl LiquidPressureMargin {
    #[must_use]
    pub fn response_units() -> BTreeMap<String, String> {
        RESPONSES.iter().map(|r| ((*r).to_string(), "Pa".to_string())).collect()
    }

    #[must_use]
    pub fn runtime_support() -> Map<String, Value> {
        json!({"status": "field_component", "history": true,
               "data": "explicit saturation law and single-phase pressure margin", "limitations": LIMITATIONS})
        .as_object()
        .cloned()
        .unwrap_or_default()
    }


    pub fn validate(&self, addins: &AddInRegistry, s: &Value) -> Result<PressureMarginSettings, CaeError> {
        let keys_ok = s
            .as_object()
            .is_some_and(|m| m.len() == REQUIRED.len() && REQUIRED.iter().all(|k| m.contains_key(*k)));
        if !keys_ok {
            let quoted: Vec<String> = REQUIRED.iter().map(|k| implexity_core::py_repr::repr_str(k)).collect();
            return Err(CaeError::contract(format!(
                "liquid pressure margin requires [{}]",
                quoted.join(", ")
            )));
        }
        if s["provenance"].as_str().is_none_or(|p| p.trim().is_empty()) {
            return Err(CaeError::contract("pressure-margin provenance required"));
        }
        let mut num = BTreeMap::new();
        for key in [
            "reference_temperature_K",
            "phase_fraction_threshold",
            "softmin_scale_Pa",
            "minimum_pressure_margin_Pa",
        ] {
            match s[key].as_f64().filter(|v| f64::is_finite(*v)) {
                Some(v) if !s[key].is_boolean() => {
                    num.insert(key, v);
                }
                _ => return Err(CaeError::contract(format!("{key}: finite real parameter required"))),
            }
        }
        let threshold = num["phase_fraction_threshold"];
        if !(0.0 < threshold && threshold <= 1.0)
            || num["softmin_scale_Pa"] <= 0.0
            || num["minimum_pressure_margin_Pa"] < 0.0
        {
            return Err(CaeError::contract("invalid pressure margin, threshold or smoothing scale"));
        }
        let name = s["saturation_component"].as_str().unwrap_or_default();
        let law = saturation_component(addins, name)?;
        law.validate(&s["saturation_settings"])?;
        law.temperature_check(&[num["reference_temperature_K"]])?;
        Ok(PressureMarginSettings {
            saturation_component: name.to_string(),
            reference_temperature_k: num["reference_temperature_K"],
            phase_fraction_threshold: threshold,
            softmin_scale_pa: num["softmin_scale_Pa"],
            minimum_pressure_margin_pa: num["minimum_pressure_margin_Pa"],
            raw: s.clone(),
        })
    }


    pub fn bind(
        &self,
        addins: &AddInRegistry,
        settings: &Value,
        times_s: &[f64],
    ) -> Result<BoundLiquidPressureMargin, CaeError> {
        let settings = self.validate(addins, settings)?;
        let law = saturation_component(addins, &settings.saturation_component)?;
        if times_s.len() < 2
            || !times_s.iter().copied().all(f64::is_finite)
            || times_s.windows(2).any(|w| w[1] - w[0] <= 0.0)
        {
            return Err(CaeError::contract(
                "pressure-margin fields require the complete ordered host time coordinates",
            ));
        }
        Ok(BoundLiquidPressureMargin {
            settings,
            law,
            final_time: (times_s.len() - 1, times_s[times_s.len() - 1]),
        })
    }
}

impl AddInAdapter for LiquidPressureMargin {
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

pub struct BoundLiquidPressureMargin {
    pub settings: PressureMarginSettings,
    law: Arc<dyn SaturationLaw>,
    final_time: (usize, f64),
}

impl std::fmt::Debug for BoundLiquidPressureMargin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundLiquidPressureMargin").field("settings", &self.settings).finish_non_exhaustive()
    }
}

impl BoundLiquidPressureMargin {
    #[must_use]
    pub fn field_metadata(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for name in FIELDS {
            m.insert(
                name.into(),
                json!({"units": "Pa", "association": "cell", "rank": "scalar",
                       "phase_mask": "pressure_margin_reporting_mask",
                       "source": "solved_absolute_pressure_minus_selected_saturation_pressure; single_phase_screening_only",
                       "temporal_association": "final_stored_state", "time_index": self.final_time.0,
                       "time_s": self.final_time.1}),
            );
        }
        m.insert(
            "pressure_margin_reporting_mask".into(),
            json!({"units": "1", "association": "cell", "rank": "scalar",
                   "source": "authored_pressure_margin_phase_reporting_threshold",
                   "temporal_association": "time_invariant_design_derived"}),
        );
        m
    }

    fn sample_arrays(&self, s: &ObserverSample<f64>) -> Result<Vec<bool>, CaeError> {
        if s.fluid_fraction.is_empty() || !s.well_formed() {
            return Err(CaeError::contract("pressure-margin sample shapes/values disagree"));
        }
        if s.fluid_fraction.iter().any(|e| !(0.0..=1.0).contains(e)) {
            return Err(CaeError::contract("fluid fraction outside [0,1]"));
        }
        let active: Vec<bool> = s.fluid_fraction.iter().map(|e| *e > 0.0).collect();
        let on =
            |v: &[f64]| -> Vec<f64> { v.iter().zip(&active).filter(|(_, a)| **a).map(|(x, _)| *x).collect() };
        if on(&s.pressure_absolute_pa).iter().any(|p| *p <= 0.0) {
            return Err(CaeError::convergence("absolute pressure must be positive on actual fluid support"));
        }
        let t = on(&s.fluid_temperature_k);
        let u = on(&s.phase_cell_temperature_upper_k);
        if u.iter().zip(&t).any(|(u, t)| u < t) {
            return Err(CaeError::contract("nodal upper-temperature field is not an upper bound"));
        }
        for v in [&t, &u] {
            if let Err(e) = self.law.temperature_check(v) {
                return Err(CaeError::convergence(format!("liquid pressure-margin saturation domain: {e}")));
            }
        }
        Ok(active)
    }

    fn margins<S: Scalar>(&self, s: &ObserverSample<S>) -> (Vec<S>, Vec<S>, Vec<S>) {
        let reference = self.settings.reference_temperature_k;
        let law = self.law.as_ref();
        let (mut pv, mut margin, mut upper) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..s.fluid_fraction.len() {
            let active = s.fluid_fraction[k].value() > 0.0;
            let t = if active { s.fluid_temperature_k[k] } else { S::from_f64(reference) };
            let tu = if active { s.phase_cell_temperature_upper_k[k] } else { S::from_f64(reference) };
            let v = law_pressure(law, t);
            let vu = law_pressure(law, tu);
            let p = s.pressure_absolute_pa[k];
            pv.push(v);
            margin.push(p - v);
            upper.push(p - vu);
        }
        (pv, margin, upper)
    }


    pub fn check(&self, samples: &[ObserverSample<f64>]) -> Result<Value, CaeError> {
        let cutoff = self.settings.phase_fraction_threshold;
        let mut rows = Vec::new();
        for (n, s) in samples.iter().enumerate() {
            let active = self.sample_arrays(s)?;
            let qualified: Vec<bool> = s.fluid_fraction.iter().map(|e| *e >= cutoff).collect();
            if !qualified.iter().any(|q| *q) {
                return Err(CaeError::convergence(
                    "no cells meet the authored pressure-margin phase threshold",
                ));
            }
            let (_, margin, bound) = self.margins(s);
            let on = |v: &[f64], mask: &[bool]| -> Vec<f64> {
                v.iter().zip(mask).filter(|(_, m)| **m).map(|(x, _)| *x).collect()
            };
            let (ma, ba) = (on(&margin, &active), on(&bound, &active));
            if !ma.iter().chain(&ba).copied().all(f64::is_finite) {
                return Err(CaeError::convergence("saturation pressure/margin is nonfinite"));
            }
            if ma.iter().chain(&ba).any(|v| *v < 0.0) {
                return Err(CaeError::convergence(format!(
                    "negative liquid-only saturation margin on actual fluid support at sample {n}; the reporting threshold cannot hide an invalid fluid state"
                )));
            }
            let min = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
            let minimum = min(&on(&margin, &qualified));
            let upper_min = min(&on(&bound, &qualified));
            let floor = self.settings.minimum_pressure_margin_pa;
            if minimum.min(upper_min) < floor {
                return Err(CaeError::convergence(format!(
                    "authored positive pressure reserve not met at sample {n}: fluid_min_Pa={}; nodal_bound_min_Pa={}; required_Pa={}; no vapour or pressure clipping was applied",
                    fmt_g(minimum, 17),
                    fmt_g(upper_min, 17),
                    fmt_g(floor, 17)
                )));
            }
            rows.push(json!({
                "sample": n,
                "minimum_fluid_pressure_margin_Pa": minimum,
                "minimum_phase_cell_pressure_margin_Pa": upper_min,
                "qualified_cells": qualified.iter().filter(|q| **q).count(),
                "active_fluid_cells": active.iter().filter(|a| **a).count(),
                "minimum_actual_fluid_margin_Pa": min(&ma),
                "minimum_actual_fluid_nodal_bound_Pa": min(&ba),
            }));
        }
        if rows.is_empty() {
            return Err(CaeError::contract("pressure margin requires nonempty solved history"));
        }
        Ok(json!({
            "component": "liquid_pressure_margin",
            "samples": rows,
            "single_phase_margin_passed": true,
            "two_phase_cavitation_model": false,
            "cavitation_erosion_predicted": false,
            "minimum_required_pressure_margin_Pa": self.settings.minimum_pressure_margin_pa,
            "bound_definition": "unnormalized phase-weighted soft minimum over stored history, including initial",
        }))
    }

    #[must_use]
    pub fn values<S: Scalar>(&self, samples: &[ObserverSample<S>]) -> [S; 2] {
        let scale = self.settings.softmin_scale_pa;
        let cutoff = self.settings.phase_fraction_threshold;
        let (mut fluid, mut upper) = (Vec::new(), Vec::new());
        for s in samples {
            let (_, margin, bound) = self.margins(s);
            for k in 0..s.fluid_fraction.len() {
                let eps = s.fluid_fraction[k];
                if eps.value() <= 0.0 {
                    continue;
                }
                let logw = log_weight(eps, cutoff);
                fluid.push(logw - margin[k] / scale);
                upper.push(logw - bound[k] / scale);
            }
        }
        [soft_minimum(&fluid, scale), soft_minimum(&upper, scale)]
    }


    pub fn fields(&self, s: &ObserverSample<f64>) -> Result<BTreeMap<String, Vec<f64>>, CaeError> {
        let active = self.sample_arrays(s)?;
        let (pv, margin, upper) = self.margins(s);
        let masked = |v: &[f64]| -> Vec<f64> {
            v.iter().zip(&active).map(|(x, a)| if *a { *x } else { 0.0 }).collect()
        };
        Ok(BTreeMap::from([
            (FIELDS[0].to_string(), masked(&pv)),
            (FIELDS[1].to_string(), masked(&margin)),
            (FIELDS[2].to_string(), masked(&upper)),
            (
                "pressure_margin_reporting_mask".to_string(),
                s.fluid_fraction
                    .iter()
                    .map(|e| if *e >= self.settings.phase_fraction_threshold { 1.0 } else { 0.0 })
                    .collect(),
            ),
        ]))
    }

    #[must_use]
    pub fn raw_settings(&self) -> &Value {
        &self.settings.raw
    }
}

#[must_use]
pub fn contract() -> AddInContract {
    let mut c = AddInContract::new("liquid_pressure_margin");
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new("liquid_history_admissibility")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
    c
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    ctx.register_addin(ContractInput::Typed(Box::new(contract())), Some(Arc::new(LiquidPressureMargin)))
}
