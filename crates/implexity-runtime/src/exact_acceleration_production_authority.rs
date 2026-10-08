// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use implexity_core::contracts::{CaeProvider, ProviderCapabilities};
use implexity_core::orchestration::AddInRegistry;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::design_operations;
use serde_json::{Map, Value};
use std::sync::Arc;

use crate::canonical::is_digest;
use crate::computation_effort::ComputationMode;
use crate::provider_job_authority::{provider_effort_profile, validate_effort_binding};

pub const PRODUCTION_CHILD_MESSAGE_SCHEMA: &str = "implexity-private-exact-acceleration-production-child/1";
pub const PRODUCTION_OPERATION_AUTHORITY_SCHEMA: &str =
    "implexity-private-exact-acceleration-production-operation/1";
pub const PRODUCTION_PAIR_AUTHORITY_SCHEMA: &str = "implexity-exact-acceleration-operation-pair/1";
pub const PREEXECUTION_EVIDENCE_PATH: &str =
    "canonical_inputs/exact_acceleration_preexecution_mechanism_evidence.json";
pub const PROMOTION_REGISTRY_PATH: &str = "service/implexity/cae/exact_acceleration_promotion_registry.py";
pub const ACCELERATED_POLICY: &str = "exact_krylov_bounded_sparse_ilu_v1";
pub const SOLVER_WORKSPACE_LIMIT_BYTES: i64 = 512 * 1024 * 1024;
const PUBLIC_OPERATION_CLASSES: [&str; 3] = ["evaluate", "sensitivity", "optimize"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProductionAuthorityError {
    #[error("{0}")]
    Authority(String),
    #[error("{0}")]
    Drift(String),
    #[error("{0}")]
    Replay(String),
}

impl From<ProductionAuthorityError> for CaeError {
    fn from(e: ProductionAuthorityError) -> Self {
        CaeError::contract(e.to_string())
    }
}

fn authority<T>(m: impl Into<String>) -> CaeResult<T> {
    Err(ProductionAuthorityError::Authority(m.into()).into())
}

fn digest(value: &Value, label: &str) -> CaeResult<String> {
    match value.as_str() {
        Some(s) if is_digest(s) => Ok(s.to_string()),
        _ => authority(format!("{label} is not a lowercase SHA-256 digest")),
    }
}


pub fn parse_strict_production_json(raw: &[u8], label: &str) -> CaeResult<Map<String, Value>> {
    if raw.is_empty() {
        return authority(format!("{label} bytes are absent"));
    }
    match implexity_core::json::parse_strict_bytes(raw) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => authority(format!("{label} must be a JSON object")),
        Err(e) if e.message.starts_with("non-finite number ") => {
            let constant =
                e.message.trim_start_matches("non-finite number ").trim_end_matches(" is not admissible");
            authority(format!("{label} contains nonfinite JSON number {constant}"))
        }
        Err(e) if e.message.to_ascii_lowercase().contains("duplicate") => {
            authority(format!("{label} contains a duplicate or non-string key"))
        }
        Err(e) if e.message.contains("overflow") => {
            authority(format!("{label} contains a nonfinite JSON number"))
        }
        Err(_) => authority(format!("{label} is not strict UTF-8 JSON")),
    }
}

#[must_use]
pub fn production_admission_inputs_present() -> bool {
    false
}


pub fn derive_stable_production_source() -> CaeResult<Value> {
    match implexity_io::bundle_resources::implementation_source_root() {
        Ok(root) => {
            authority(format!("source manifest at {} does not attest the running executable", root.display()))
        }
        Err(message) => Err(CaeError::contract(message)),
    }
}


pub fn resolve_selected_numerical_provider(
    provider_id: &str,
    problem: &Value,
    registry: &implexity_core::registries::Registries,
) -> CaeResult<Option<(String, Arc<dyn CaeProvider>)>> {
    let outer = registry.providers.get(provider_id)?;
    if !matches!(outer.capabilities()?, ProviderCapabilities::Descriptor(_) | ProviderCapabilities::Legacy(_))
    {
        return authority("selected provider capabilities are not typed");
    }
    if !outer.orchestration_meta() {
        return Ok(Some((provider_id.to_string(), outer)));
    }
    let Some(meta) = outer.as_any().downcast_ref::<crate::intent_orchestrated::IntentOrchestratedProvider>()
    else {
        return authority("orchestration meta-provider lacks its source-owned planner");
    };
    let (_normalised, plan, _context) = meta.parts(problem)?;
    let mut owners: Vec<String> = Vec::new();
    for v in plan.response_providers.values() {
        let s = crate::pyval::py_str(v);
        if !owners.contains(&s) {
            owners.push(s);
        }
    }
    if owners.len() != 1 {
        return Ok(None);
    }
    selected_owner(&registry.addins, &owners[0]).map(Some)
}

fn selected_owner(addins: &AddInRegistry, owner: &str) -> CaeResult<(String, Arc<dyn CaeProvider>)> {
    let snapshot = addins.snapshot();
    let Some(entry) = snapshot.entries.iter().find(|e| e.contract.addin_id == owner) else {
        return authority("selected numerical owner is unavailable");
    };
    let Some(provider) = entry.adapter.as_ref().and_then(|a| a.provider()) else {
        return Err(
            ProductionAuthorityError::Drift("selected numerical owner identity drifted".into()).into()
        );
    };
    let declared = provider.provider_id().map_or_else(|| provider.name().to_string(), str::to_string);
    if declared != owner {
        return Err(
            ProductionAuthorityError::Drift("selected numerical owner identity drifted".into()).into()
        );
    }
    Ok((owner.to_string(), provider))
}


pub fn accelerated_provider_profile(
    provider_id: &str,
    provider: &dyn CaeProvider,
) -> CaeResult<Option<Map<String, Value>>> {
    let capabilities = provider.capabilities()?;
    if !matches!(capabilities, ProviderCapabilities::Descriptor(_) | ProviderCapabilities::Legacy(_)) {
        return authority("numerical provider capabilities are not typed");
    }
    let Some(candidates) = design_operations(provider).and_then(
        implexity_optim::provider_ops::DesignOperations::exact_computation_effort_candidate_profiles,
    ) else {
        return Ok(None);
    };
    let candidates = candidates?;
    let physics = implexity_core::packages::global().status()?;
    provider_effort_profile(
        provider_id,
        &capabilities,
        &physics,
        Some(&candidates),
        Some(ACCELERATED_POLICY),
        false,
    )
    .map(Some)
}


pub fn prepare_production_child_launch(
    operation_class: &str,
    request_sha256: &Value,
    effort_binding: &Value,
    provider_id: &str,
    problem: &Value,
    registry: &implexity_core::registries::Registries,
) -> CaeResult<Option<Value>> {
    if !PUBLIC_OPERATION_CLASSES.contains(&operation_class) {
        return authority("production operation class is not closed");
    }
    digest(request_sha256, "managed request")?;
    let Some(binding) = effort_binding.as_object() else {
        return authority("production effort binding is absent");
    };
    digest(binding.get("operation_context_digest").unwrap_or(&Value::Null), "outer operation")?;
    let canonical_keys = [
        "schema",
        "selection",
        "requested_policy_digest",
        "effective_effort_digest",
        "coupling_approximation",
        "coupling_approximation_digest",
        "provider_profile",
        "operation_context",
        "operation_context_digest",
    ];
    let mut checked = false;
    if binding.len() == canonical_keys.len() && canonical_keys.iter().all(|k| binding.contains_key(*k)) {
        let (requested, checked_binding) = validate_effort_binding(effort_binding, None)?;
        if requested.requested.mode == ComputationMode::Exact
            && checked_binding["coupling_approximation"]
                == serde_json::json!({"preset": "exact", "lagged_coupling_ids": []})
        {
            return Ok(None);
        }
        checked = true;
    }
    let Some((selected_id, provider)) = resolve_selected_numerical_provider(provider_id, problem, registry)?
    else {
        return Ok(None);
    };
    if accelerated_provider_profile(&selected_id, provider.as_ref())?.is_none() {
        return Ok(None);
    }
    if !checked {
        validate_effort_binding(effort_binding, None)?;
    }
    derive_stable_production_source()?;
    authority("production child launch requires a verified source inventory")
}


pub fn build_production_child_execution(effort_binding: &Value) -> CaeResult<()> {
    let (requested, checked) = validate_effort_binding(effort_binding, None)?;
    if requested.requested.mode == ComputationMode::Exact
        && checked["coupling_approximation"]
            == serde_json::json!({"preset": "exact", "lagged_coupling_ids": []})
    {
        return Err(ProductionAuthorityError::Drift(
            "conservative exact request entered an accelerated child".into(),
        )
        .into());
    }
    derive_stable_production_source()?;
    authority("production child execution requires a verified source inventory")
}


pub fn private_production_provider_effort_validation(
    _provider_name: &str,
    _capabilities: &ProviderCapabilities,
    _physics: &Value,
    _effort_binding: &Value,
    _candidates: Option<&Value>,
) -> CaeResult<Option<(Map<String, Value>, Value)>> {
    Ok(None)
}


pub fn require_private_production_request(_request_sha256: &str) -> CaeResult<()> {
    authority("verified production request lacks child authority")
}

#[must_use]
pub fn production_profile_visible(_candidate_policy: &str) -> bool {
    false
}

#[must_use]
pub fn select_private_production_profile(
    _profiles: &Map<String, Value>,
    _default_policy: &str,
) -> Option<Value> {
    None
}


pub fn require_private_production_profile(_profile: &Value) -> CaeResult<()> {
    authority("nondefault production profile lacks child authority")
}

#[must_use]
pub fn imported_numerical_modules() -> Vec<String> {
    Vec::new()
}
