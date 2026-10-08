// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
