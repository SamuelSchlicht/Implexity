// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
