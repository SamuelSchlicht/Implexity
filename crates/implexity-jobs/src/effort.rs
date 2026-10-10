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



use implexity_core::contracts::CaeProvider;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::{ProviderScope, design_operations};
use implexity_runtime::provider_job_authority::{
    ProviderFacts, private_provider_effort_validation, require_ordinary_exact_profile_authority,
    validate_effort_binding,
};
use serde_json::Value;


pub fn physics_snapshot() -> CaeResult<Value> {
    let mut snapshot=implexity_core::packages::global().status()?;
    let imports=implexity_runtime::provider_import::snapshot();
    if imports.as_array().is_some_and(|a| !a.is_empty()) { snapshot["imported_providers"]=imports; }
    Ok(snapshot)
}


pub fn provider_computation_effort_scope(
    provider: &dyn CaeProvider,
    effort_binding: Option<&Value>,
) -> CaeResult<Option<ProviderScope>> {
    let Some(binding) = effort_binding.filter(|b| !b.is_null()) else {
        return Ok(None);
    };
    let (_selection, initial) = validate_effort_binding(binding, None)?;
    let capabilities = provider.capabilities()?;
    let physics = physics_snapshot()?;
    let provider_id = initial
        .get("provider_profile")
        .and_then(|p| p.get("provider_id"))
        .map(implexity_core::pyobj::py_str)
        .unwrap_or_default();
    let initial_value = Value::Object(initial);
    let (initial_value, candidates) = match private_provider_effort_validation(
        &provider_id,
        provider,
        &capabilities,
        &physics,
        &initial_value,
    )? {
        Some((rebound, candidates)) => (Value::Object(rebound), Some(candidates)),
        None => (initial_value, None),
    };
    let facts = ProviderFacts {
        provider_name: &provider_id,
        capabilities: &capabilities,
        physics: &physics,
        candidates: candidates.as_ref(),
    };
    let (_selection, checked) = validate_effort_binding(&initial_value, Some(&facts))?;
    require_ordinary_exact_profile_authority(checked.get("provider_profile").unwrap_or(&Value::Null))?;
    let Some(ops) = design_operations(provider).filter(|o| o.has_computation_effort_scope()) else {
        return Ok(None);
    };
    match ops.computation_effort_scope(&Value::Object(checked)) {
        Some(scope) => scope.map(Some),
        None => Err(CaeError::contract("provider computation_effort_scope is unavailable")),
    }
}
