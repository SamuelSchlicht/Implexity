// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::Value;

use crate::error::{CfdError, CfdResult};
use crate::workspace_contract::{Brinkman, Fluid};

fn finite(x: f64, name: &str) -> CfdResult<f64> {
    if x.is_finite() { Ok(x) } else { Err(CfdError::Input(format!("{name} must be finite"))) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgeingState {
    pub time_s: f64,
    pub dose_gy: f64,
    pub fluence_m2: f64,
    pub cycles: f64,
    pub temperature_k: f64,
}

impl Default for AgeingState {
    fn default() -> Self {
        Self { time_s: 0.0, dose_gy: 0.0, fluence_m2: 0.0, cycles: 0.0, temperature_k: 293.15 }
    }
}

impl AgeingState {

    pub fn validate(&self) -> CfdResult<&Self> {
        for (name, v) in [
            ("time_s", self.time_s),
            ("dose_Gy", self.dose_gy),
            ("fluence_m2", self.fluence_m2),
            ("cycles", self.cycles),
            ("temperature_K", self.temperature_k),
        ] {
            if finite(v, &format!("ageing state {name}"))? < 0.0 {
                return Err(CfdError::Input(format!("ageing state {name} must be >= 0")));
            }
        }
        if self.temperature_k <= 0.0 {
            return Err(CfdError::Input("ageing temperature must be > 0 K".into()));
        }
        Ok(self)
    }
}

pub type AgeingEvaluator = Arc<dyn Fn(&AgeingState, f64) -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct PropertyLaw {
    pub name: String,
    pub evaluator: AgeingEvaluator,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

impl std::fmt::Debug for PropertyLaw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PropertyLaw")
            .field("name", &self.name)
            .field("minimum", &self.minimum)
            .field("maximum", &self.maximum)
            .finish_non_exhaustive()
    }
}

impl PropertyLaw {

    pub fn call(&self, state: &AgeingState, baseline: f64) -> CfdResult<f64> {
        let baseline = finite(baseline, "ageing baseline")?;
        for bound in [self.minimum, self.maximum].into_iter().flatten() {
            finite(bound, "ageing property bound")?;
        }
        if let (Some(lo), Some(hi)) = (self.minimum, self.maximum)
            && lo > hi
        {
            return Err(CfdError::Input("ageing property bounds are reversed".into()));
        }
        let y = finite(
            (self.evaluator)(state.validate()?, baseline),
            &format!("ageing law {} result", self.name),
        )?;
        if self.minimum.is_some_and(|lo| y < lo) {
            return Err(CfdError::Input(format!("ageing law {} violates its lower bound", self.name)));
        }
        if self.maximum.is_some_and(|hi| y > hi) {
            return Err(CfdError::Input(format!("ageing law {} violates its upper bound", self.name)));
        }
        Ok(y)
    }
}

pub const AGEABLE_PROPERTIES: [&str; 6] = [
    "density_kg_m3",
    "dynamic_viscosity_Pa_s",
    "heat_capacity_J_kg_K",
    "thermal_conductivity_W_m_K",
    "solid_permeability_m2",
    "fluid_permeability_m2",
];


pub fn apply(
    state: &AgeingState,
    fluid: &Fluid,
    brinkman: &Brinkman,
    laws: &[(String, PropertyLaw)],
) -> CfdResult<(Fluid, Brinkman)> {
    state.validate()?;
    let mut vals: Vec<(&str, Option<f64>)> = vec![
        ("density_kg_m3", Some(fluid.density_kg_m3)),
        ("dynamic_viscosity_Pa_s", Some(fluid.dynamic_viscosity_pa_s)),
        ("heat_capacity_J_kg_K", fluid.heat_capacity_j_kg_k),
        ("thermal_conductivity_W_m_K", fluid.thermal_conductivity_w_m_k),
        ("solid_permeability_m2", Some(brinkman.solid_permeability_m2)),
        ("fluid_permeability_m2", Some(brinkman.fluid_permeability_m2)),
    ];
    for (key, law) in laws {
        let slot = vals.iter_mut().find(|(k, _)| *k == key.as_str());
        let Some((_, Some(current))) = slot.as_ref().map(|s| (s.0, s.1)) else {
            return Err(CfdError::Input(format!(
                "ageing law targets unavailable property {}",
                implexity_core::py_repr::repr_str(key)
            )));
        };
        let aged = law.call(state, current)?;
        if let Some(s) = slot {
            s.1 = Some(aged);
        }
    }
    let get = |k: &str| vals.iter().find(|(n, _)| *n == k).and_then(|(_, v)| *v);
    let name = match &fluid.name {
        Value::String(s) => Value::String(format!("{s} (aged)")),
        other => {
            return Err(CfdError::Type(format!(
                "unsupported operand type(s) for +: '{}' and 'str'",
                match other {
                    Value::Null => "NoneType",
                    Value::Bool(_) => "bool",
                    Value::Number(n) if n.is_f64() => "float",
                    Value::Number(_) => "int",
                    Value::Array(_) => "list",
                    _ => "dict",
                }
            )));
        }
    };
    let f = Fluid::new(
        get("density_kg_m3").unwrap_or(f64::NAN),
        get("dynamic_viscosity_Pa_s").unwrap_or(f64::NAN),
        get("heat_capacity_J_kg_K"),
        get("thermal_conductivity_W_m_K"),
        state.temperature_k,
        name,
        false,
    )?;
    let b = Brinkman::new(
        get("fluid_permeability_m2").unwrap_or(f64::NAN),
        get("solid_permeability_m2").unwrap_or(f64::NAN),
        brinkman.ramp_q,
        &brinkman.continuation,
        brinkman.minimum_fluid_fraction,
    )?;
    Ok((f, b))
}

