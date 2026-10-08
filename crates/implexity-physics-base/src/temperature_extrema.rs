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
    AddInAdapter, AddInCategory, AddInContract, ContractInput, Fidelity, PortSpec,
};
use implexity_core::packages::InstallContext;

use crate::array::Tensor;

pub const IMPLEMENTATION: &str = "implexity.physics_library.temperature_extrema.TemperatureExtrema";

const LIMITATIONS: [&str; 3] = [
    "All shared nodes, including numerical replacement regions; no phase-threshold masking.",
    "Exact maximum is nonsmooth at ties; smooth bound overestimates by at most width_K*log(sample_count).",
    "Checks finite samples, not material calibration or engineering feasibility.",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ExtremaSettings {
    pub include_initial: bool,
    pub width_k: f64,
    pub provenance: String,
    raw: Value,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TemperatureExtrema;

impl TemperatureExtrema {
    pub const COMPONENT_KIND: &'static str = "history_response_observer";
    pub const REQUIRES: [&'static str; 1] = ["shared_nodal_temperature_K"];

    #[must_use]
    pub fn response_units() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("shared_temperature_max_K".to_string(), "K".to_string()),
            ("shared_temperature_upper_bound_K".to_string(), "K".to_string()),
        ])
    }


    pub fn validate(&self, settings: &Value) -> Result<ExtremaSettings, CaeError> {
        let err = || {
            CaeError::contract("temperature extrema require include_initial, positive width_K and provenance")
        };
        let s = settings.as_object().filter(|s| s.len() == 3).ok_or_else(err)?;
        let include_initial = s.get("include_initial").and_then(Value::as_bool).ok_or_else(err)?;
        let width_k = s
            .get("width_K")
            .filter(|v| v.is_number())
            .and_then(Value::as_f64)
            .filter(|w| w.is_finite() && *w > 0.0)
            .ok_or_else(err)?;
        let provenance = s
            .get("provenance")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(err)?
            .to_string();
        Ok(ExtremaSettings { include_initial, width_k, provenance, raw: settings.clone() })
    }


    pub fn bind(&self, settings: &Value) -> Result<BoundTemperatureExtrema, CaeError> {
        Ok(BoundTemperatureExtrema { settings: self.validate(settings)? })
    }
}

impl AddInAdapter for TemperatureExtrema {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        json!({"status": "field_component", "history": true, "limitations": LIMITATIONS}).as_object().cloned()
    }
    fn component_kind(&self) -> Option<String> {
        Some(Self::COMPONENT_KIND.into())
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        Some(Self::response_units())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundTemperatureExtrema {
    pub settings: ExtremaSettings,
}

impl BoundTemperatureExtrema {
    #[must_use]
    pub fn selected_states(&self, len: usize) -> std::ops::Range<usize> {
        (usize::from(!self.settings.include_initial))..len
    }


    pub fn values<S: Scalar>(&self, samples: &[Tensor<S>]) -> Result<[S; 2], CaeError> {
        let all: Vec<S> = samples.iter().flat_map(|t| t.data().iter().copied()).collect();
        if samples.is_empty() {
            return Err(CaeError::contract("temperature extrema require a selected stored state"));
        }
        let a = Tensor::vector(all);
        let maximum = a.max().map_err(|e| CaeError::contract(e.to_string()))?;
        let w = self.settings.width_k;
        let bound = maximum + a.map(|x| ((x - maximum) / w).exp()).sum().ln() * w;
        Ok([maximum, bound])
    }


    pub fn check(&self, samples: &[Tensor<f64>]) -> Result<Value, CaeError> {
        if samples.is_empty() {
            return Err(CaeError::contract("temperature extrema require samples"));
        }
        for s in samples {
            if s.size() == 0 || !s.data().iter().all(Scalar::is_finite) {
                return Err(CaeError::convergence("nonfinite or empty shared nodal temperature sample"));
            }
        }
        Ok(json!({"component": "shared_temperature_extrema", "all_samples_finite": true,
                  "scope": "all_shared_nodes_no_phase_mask", "engineering_limits_checked": false}))
    }

    #[must_use]
    pub fn fields(&self) -> Map<String, Value> {
        Map::new()
    }

    #[must_use]
    pub fn raw_settings(&self) -> &Value {
        &self.settings.raw
    }
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    let mut c = AddInContract::new("shared_temperature_extrema");
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new("temperature_history_metrics")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(TemperatureExtrema)))
}
