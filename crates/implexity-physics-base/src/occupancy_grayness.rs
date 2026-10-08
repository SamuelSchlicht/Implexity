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

pub const IMPLEMENTATION: &str = "implexity_rust_extension.occupancy_grayness";
pub const COMPONENT_ID: &str = "occupancy_grayness";
pub const FLUID_FRACTION_SAMPLE: &str = "fluid_fraction";
pub const REQUIRES: [&str; 1] = [FLUID_FRACTION_SAMPLE];
pub const RESPONSE: &str = "occupancy_grayness";
pub const FIELD: &str = "occupancy_grayness_density";

const LIMITATIONS: [&str; 3] = [
    "Measures intermediate occupancy of the sampled host cells only; a binary sampled occupancy does not establish a manufacturable sharp geometry between the samples.",
    "Unweighted cell mean (the volume mean on a grid of equal cells); fixed binary regions dilute it.",
    "A design-quality response; it neither removes the porous-flow relaxation of the host nor validates a gray design.",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraynessSettings {
    pub provenance: String,
    raw: Value,
}

impl GraynessSettings {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}


pub fn validate(settings: &Value) -> Result<GraynessSettings, CaeError> {
    let provenance = settings
        .as_object()
        .filter(|s| s.len() == 1)
        .and_then(|s| s.get("provenance"))
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| CaeError::contract("occupancy grayness requires exactly a nonempty provenance"))?;
    Ok(GraynessSettings { provenance: provenance.to_string(), raw: settings.clone() })
}

pub fn grayness<S: Scalar>(fluid_fraction: &[S]) -> S {
    let mut sum = S::zero();
    for phi in fluid_fraction {
        sum += *phi * (-*phi + 1.0);
    }
    #[allow(clippy::cast_precision_loss)]
    let scale = 4.0 / fluid_fraction.len() as f64;
    sum * scale
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundGrayness {
    pub settings: GraynessSettings,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OccupancyGrayness;

impl OccupancyGrayness {
    pub const COMPONENT_KIND: &'static str = "history_response_observer";

    #[must_use]
    pub fn response_units() -> Vec<(String, String)> {
        vec![(RESPONSE.to_string(), "1".to_string())]
    }


    pub fn bind(&self, settings: &Value) -> Result<BoundGrayness, CaeError> {
        Ok(BoundGrayness { settings: validate(settings)? })
    }
}

impl BoundGrayness {
    #[must_use]
    pub fn selected_states(&self, len: usize) -> Vec<usize> {
        len.checked_sub(1).into_iter().collect()
    }


    pub fn values<S: Scalar>(&self, fluid_fractions: &[&[S]]) -> Result<Vec<S>, CaeError> {
        match fluid_fractions.last() {
            Some(phi) if !phi.is_empty() => Ok(vec![grayness(phi)]),
            _ => Err(CaeError::contract("occupancy grayness requires a nonempty cell fluid-fraction sample")),
        }
    }


    pub fn check(&self, fluid_fractions: &[&[f64]]) -> Result<Value, CaeError> {
        let Some(phi) = fluid_fractions.last().filter(|p| !p.is_empty()) else {
            return Err(CaeError::contract("occupancy grayness requires samples"));
        };
        if !phi.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return Err(CaeError::convergence("nonfinite or out-of-range occupancy grayness sample"));
        }
        #[allow(clippy::cast_precision_loss)]
        let count = phi.len() as f64;
        let gray = phi.iter().filter(|v| (0.1..=0.9).contains(*v)).count();
        #[allow(clippy::cast_precision_loss)]
        let gray_fraction = gray as f64 / count;
        Ok(json!({"component": COMPONENT_ID, "all_samples_finite": true,
                  "occupancy_grayness": grayness(phi),
                  "cell_fraction_with_occupancy_0.1_to_0.9": gray_fraction,
                  "scope": "cell_mean_of_4_rho_(1-rho)_of_the_final_state",
                  "engineering_limits_checked": false}))
    }

    #[must_use]
    pub fn fields(&self, fluid_fraction: &[f64]) -> BTreeMap<String, Vec<f64>> {
        BTreeMap::from([(FIELD.to_string(), fluid_fraction.iter().map(|p| 4.0 * p * (1.0 - p)).collect())])
    }

    #[must_use]
    pub fn field_metadata(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            FIELD.into(),
            json!({"units": "1", "association": "cell", "rank": 0, "source": COMPONENT_ID,
                   "description": "Cell grayness 4 rho (1 - rho) of the physical occupancy (host cell order)"}),
        );
        m
    }
}

impl AddInAdapter for OccupancyGrayness {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        json!({"status": "field_component", "history": true, "rust_extension": true, "limitations": LIMITATIONS})
            .as_object()
            .cloned()
    }
    fn component_kind(&self) -> Option<String> {
        Some(OccupancyGrayness::COMPONENT_KIND.into())
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        Some(OccupancyGrayness::response_units().into_iter().collect())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    let mut c = AddInContract::new(COMPONENT_ID);
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new("design_quality_metrics")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(OccupancyGrayness)))
}

