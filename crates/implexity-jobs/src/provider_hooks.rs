// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, RwLock};

use implexity_core::CaeResult;
use implexity_core::contracts::{CaeProvider, ProviderProblem};
use implexity_geometry::Node;
use implexity_runtime::intent_orchestrated::IntentOrchestratedProvider;
use ndarray::ArrayD;
use serde_json::{Map, Value};

pub type DerivedModelArrays = BTreeMap<String, ArrayD<f64>>;

#[allow(unused_variables)]
pub trait ProviderJobHooks: Send + Sync {
    fn required_topology_registration(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        None
    }
    fn required_topology_semantics(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        None
    }
    fn design_coordinate_shape(
        &self,
        problem: &ProviderProblem,
        coordinate: &str,
    ) -> Option<CaeResult<Option<Vec<usize>>>> {
        None
    }
    fn validate_design_model(
        &self,
        problem: &ProviderProblem,
        node: &Node,
        parameter: &str,
    ) -> Option<CaeResult<()>> {
        None
    }
    fn has_derived_model_output_refs(&self) -> bool {
        false
    }
    fn has_derive_model_updates(&self) -> bool {
        false
    }
    fn derived_model_hooks_enabled(&self, _problem: &ProviderProblem) -> bool {
        true
    }
    fn derived_model_output_refs(&self, problem: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        None
    }
    fn derive_model_updates(
        &self,
        problem: &ProviderProblem,
        design: &BTreeMap<String, ArrayD<f64>>,
    ) -> Option<CaeResult<BTreeMap<String, ArrayD<f64>>>> {
        None
    }
    fn primal_sensitivity_consistency(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        None
    }
    fn preflight_declaration(&self, problem: &ProviderProblem) -> Option<CaeResult<Map<String, Value>>> {
        None
    }
}

static REGISTRY: LazyLock<RwLock<BTreeMap<String, Arc<dyn ProviderJobHooks>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

pub fn register(provider: &str, hooks: Arc<dyn ProviderJobHooks>) {
    REGISTRY.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(provider.to_string(), hooks);
}

pub fn unregister(provider: &str) {
    REGISTRY.write().unwrap_or_else(std::sync::PoisonError::into_inner).remove(provider);
}

struct IntentHooks<'a>(&'a IntentOrchestratedProvider);

fn document(problem: &ProviderProblem) -> Option<&Value> {
    problem.downcast_ref::<Value>()
}

impl ProviderJobHooks for IntentHooks<'_> {
    fn design_coordinate_shape(
        &self,
        problem: &ProviderProblem,
        coordinate: &str,
    ) -> Option<CaeResult<Option<Vec<usize>>>> {
        let doc = document(problem)?;
        Some(self.0.design_coordinate_shape(doc, coordinate))
    }

    fn validate_design_model(
        &self,
        problem: &ProviderProblem,
        node: &Node,
        parameter: &str,
    ) -> Option<CaeResult<()>> {
        let doc = document(problem)?;
        Some(self.0.validate_design_model(doc, node, parameter))
    }

    fn preflight_declaration(&self, problem: &ProviderProblem) -> Option<CaeResult<Map<String, Value>>> {
        let doc = document(problem)?;
        Some(self.0.preflight_value(doc, false))
    }
}

pub fn with_hooks<T>(
    provider: &dyn CaeProvider,
    f: impl FnOnce(&dyn ProviderJobHooks) -> Option<T>,
) -> Option<T> {
    if let Some(intent) = provider.as_any().downcast_ref::<IntentOrchestratedProvider>() {
        return f(&IntentHooks(intent));
    }
    let hooks =
        REGISTRY.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(provider.name()).cloned();
    hooks.and_then(|h| f(h.as_ref()))
}
