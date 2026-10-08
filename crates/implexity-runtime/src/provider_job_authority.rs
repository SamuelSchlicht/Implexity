// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use implexity_core::contracts::{CaeProvider, ProviderCapabilities};
use implexity_core::orchestration::{AddInRegistry, OrchestrationPlan};
use implexity_core::wire::fingerprint;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::{ProviderScope, design_operations};
use serde_json::{Map, Value};

use crate::canonical::{canonical_text, is_digest, sha256_hex};
use crate::computation_effort::{
    ComputationEffortPolicy, ComputationMode, EffectiveComputationEffort, ExactCorrectionState,
    ObservedComputationEffort, PolicyArgs, ResultWithTruth, TruthEnvelope, TruthStatus,
};

pub const EXACT_DIRECT_POLICY: &str = "exact_direct_default";
pub const EFFORT_BINDING_SCHEMA: &str = "implexity-worker-computation-effort/1";
pub const EFFORT_PROFILE_SCHEMA: &str = "implexity-provider-computation-profile/1";
pub const EFFORT_CONTEXT_SCHEMA: &str = "implexity-computation-operation-context/1";
pub const EFFORT_EVIDENCE_SCHEMA: &str = "implexity-computation-evidence/1";

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}

fn fp(value: &Value) -> CaeResult<String> {
    fingerprint(value)
}

fn keys_are(map: &Map<String, Value>, expected: &[&str]) -> bool {
    map.len() == expected.len() && expected.iter().all(|k| map.contains_key(*k))
}

thread_local! {
    static ACTIVE_ORCHESTRATION_EFFORT: RefCell<Option<Map<String, Value>>> = const { RefCell::new(None) };
    static ACTIVE_SELECTED_PROVIDER_EFFORT: RefCell<Option<(String, String, usize)>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct OrchestrationEffortGuard {
    _private: (),
}

impl Drop for OrchestrationEffortGuard {
    fn drop(&mut self) {
        ACTIVE_ORCHESTRATION_EFFORT.with(|slot| *slot.borrow_mut() = None);
    }
}


pub fn orchestration_computation_effort_scope(binding: &Value) -> CaeResult<OrchestrationEffortGuard> {
    let (_selection, checked) = validate_effort_binding(binding, None)?;
    ACTIVE_ORCHESTRATION_EFFORT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(contract("nested orchestration computation-effort scopes are forbidden"));
        }
        *slot = Some(checked);
        Ok(())
    })?;
    Ok(OrchestrationEffortGuard { _private: () })
}

pub struct SelectedEffortScope {
    provider_scope: Option<ProviderScope>,
    marker_set: bool,
}

impl std::fmt::Debug for SelectedEffortScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectedEffortScope")
            .field("provider_scope", &self.provider_scope)
            .field("marker_set", &self.marker_set)
            .finish()
    }
}

impl SelectedEffortScope {
    #[must_use]
    pub fn profile(&self) -> Option<&Value> {
        self.provider_scope.as_ref().and_then(|s| s.value.as_ref())
    }
}

impl Drop for SelectedEffortScope {
    fn drop(&mut self) {

        self.provider_scope = None;
        if self.marker_set {
            ACTIVE_SELECTED_PROVIDER_EFFORT.with(|slot| *slot.borrow_mut() = None);
        }
    }
}


#[allow(clippy::too_many_lines)]
pub fn selected_provider_computation_effort_scope(
    plan: &OrchestrationPlan,
    registry: &AddInRegistry,
) -> CaeResult<Option<SelectedEffortScope>> {
    let Some(outer) = ACTIVE_ORCHESTRATION_EFFORT.with(|slot| slot.borrow().clone()) else {
        return Ok(None);
    };
    let snapshot = registry.snapshot();
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
    let mut candidates: Vec<(String, Arc<dyn CaeProvider>)> = Vec::new();
    for owner in &owners {
        let Some(entry) = snapshot.entries.iter().find(|e| &e.contract.addin_id == owner) else {
            return Err(contract("selected computation-effort owner is unavailable"));
        };
        if let Some(provider) = entry.adapter.as_ref().and_then(|a| a.provider()) {
            let has_scope = design_operations(provider.as_ref())
                .is_some_and(implexity_optim::provider_ops::DesignOperations::has_computation_effort_scope);
            if has_scope {
                candidates.push((owner.clone(), provider));
            }
        }
    }
    if candidates.len() != 1 {
        return Ok(None);
    }
    let (provider_name, provider) = candidates.remove(0);
    let provider_ptr = Arc::as_ptr(&provider).cast::<()>() as usize;
    let outer_operation_sha =
        crate::pyval::py_str(outer.get("operation_context_digest").unwrap_or(&Value::Null));
    let active = ACTIVE_SELECTED_PROVIDER_EFFORT.with(|slot| slot.borrow().clone());
    if let Some(active) = active {
        if active != (outer_operation_sha, provider_name, provider_ptr) {
            return Err(contract("nested selected-provider exact-effort scope drifted"));
        }
        return Ok(None);
    }
    let (outer_selection, outer) = validate_effort_binding(&Value::Object(outer), None)?;
    let requested = &outer_selection.requested;
    let mut budgets = Map::new();
    budgets.insert("wall_time_s".into(), crate::canonical::opt_f(requested.wall_time_budget_s));
    budgets.insert("memory_bytes".into(), requested.memory_budget_bytes.map_or(Value::Null, Value::from));
    if requested.wall_time_mode == "unlimited" {
        budgets.insert("wall_time_mode".into(), Value::String("unlimited".into()));
    }
    let public = crate::canonical::obj([
        ("schema", Value::String("implexity-computation-effort-request/1".into())),
        ("mode", Value::String(requested.mode.as_str().into())),
        ("hard_budgets", Value::Object(budgets)),
        ("target_update_rate_hz", crate::canonical::opt_f(requested.target_update_rate_hz)),
        (
            "error_limits",
            crate::canonical::obj([
                ("response", crate::canonical::f(requested.max_response_error)),
                ("state", crate::canonical::f(requested.max_state_error)),
                ("gradient", crate::canonical::f(requested.max_gradient_error)),
            ]),
        ),
        ("trust_radius", crate::canonical::f(requested.trust_radius)),
        (
            "exact_correction",
            crate::canonical::obj([
                ("cadence_updates", Value::from(requested.exact_correction_cadence)),
                ("deadline_s", crate::canonical::f(requested.exact_correction_deadline_s)),
            ]),
        ),
        ("ood_policy", Value::String(requested.ood_policy.as_str().into())),
        ("coupling_approximation", outer["coupling_approximation"].clone()),
    ]);
    let capabilities = provider.capabilities()?;
    let physics = implexity_core::packages::global().status()?;
    let mut operation_context = outer["operation_context"].as_object().cloned().unwrap_or_default();
    operation_context.shift_remove("schema");
    let nested = make_server_effort_binding(
        Some(&public),
        &provider_name,
        &capabilities,
        &physics,
        Some(&operation_context),
        None,
    )?;
    let (_selection, checked) = validate_effort_binding(
        &Value::Object(nested),
        Some(&ProviderFacts {
            provider_name: &provider_name,
            capabilities: &capabilities,
            physics: &physics,
            candidates: None,
        }),
    )?;
    require_ordinary_exact_profile_authority(&checked["provider_profile"])?;
    let marker = (outer_operation_sha, provider_name, provider_ptr);
    ACTIVE_SELECTED_PROVIDER_EFFORT.with(|slot| *slot.borrow_mut() = Some(marker));
    let mut guard = SelectedEffortScope { provider_scope: None, marker_set: true };
    let scope = design_operations(provider.as_ref())
        .and_then(|o| o.computation_effort_scope(&Value::Object(checked)))
        .ok_or_else(|| contract("selected provider computation-effort scope disappeared"))??;
    guard.provider_scope = Some(scope);
    Ok(Some(guard))
}


pub fn require_ordinary_exact_profile_authority(profile: &Value) -> CaeResult<()> {
    let Some(p) = profile.as_object() else {
        return Err(contract("provider computation profile is malformed"));
    };
    let policy = p.get("solver_policy").and_then(Value::as_str);
    let exact = p.get("exact_solver_profile").and_then(Value::as_object);
    match (policy, exact) {
        (Some(policy), Some(exact))
            if !policy.is_empty() && exact.get("solver_policy").and_then(Value::as_str) == Some(policy) =>
        {
            Ok(())
        }
        _ => Err(contract("provider exact solver profile is malformed")),
    }
}


pub fn canonical_payload_bytes(payload: &Map<String, Value>) -> CaeResult<Vec<u8>> {
    let value = Value::Object(payload.clone());
    implexity_core::wire::ensure_finite(&value)
        .map_err(|_| contract("result payload cannot be bound to canonical computation evidence"))?;
    Ok(canonical_text(&value).into_bytes())
}


pub fn require_single_provider_schedule(
    declared_provider: &Value,
    schedule: Option<&Value>,
) -> CaeResult<Vec<String>> {
    let owner = if declared_provider.is_null() || !implexity_core::pyobj::truthy(declared_provider) {
        String::new()
    } else {
        crate::pyval::py_str(declared_provider).trim().to_string()
    };
    if owner.is_empty() {
        return Err(contract("hierarchical schedule requires a declared provider"));
    }
    let Some(schedule) = schedule.filter(|s| !s.is_null()) else {
        return Ok(vec![owner]);
    };
    let Value::Array(rows) = schedule else {
        return Err(contract("hierarchical schedule must be a sequence of stages"));
    };
    if rows.is_empty() {
        return Ok(vec![owner]);
    }
    let mut resolved = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Value::Object(m) = row else {
            return Err(contract(format!(
                "hierarchical schedule stage {index} must be a mapping or validated stage"
            )));
        };
        let candidate = crate::pyval::text_or(m.get("provider"), &owner).trim().to_string();
        let candidate = if candidate.is_empty() { owner.clone() } else { candidate };
        if candidate != owner {
            return Err(contract(
                "cross-provider hierarchical schedules require independent server-issued per-stage computation bindings; refusing base-provider authority reuse",
            ));
        }
        resolved.push(candidate);
    }
    Ok(resolved)
}

#[must_use]
pub fn stable_package_identity(snapshot: &Value) -> Value {
    let mut out = Map::new();
    for key in ["schema", "loaded", "loaded_manifests", "load_order_fingerprint", "registry_fingerprint"] {
        if let Some(v) = snapshot.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Value::Object(out)
}


pub fn runtime_source_binding(identity: &Value) -> CaeResult<(Value, String)> {
    let wire = implexity_core::wire::to_wire(identity)?;
    let digest = fp(&wire)?;
    Ok((wire, digest))
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderFacts<'a> {
    pub provider_name: &'a str,
    pub capabilities: &'a ProviderCapabilities,
    pub physics: &'a Value,
    pub candidates: Option<&'a Value>,
}

fn descriptor_traits(capabilities: &ProviderCapabilities) -> CaeResult<&Map<String, Value>> {
    match capabilities {

        ProviderCapabilities::Descriptor(d) => Ok(&d.traits),
        ProviderCapabilities::Legacy(l) => Ok(&l.base.traits),
        ProviderCapabilities::Mapping(_) => {
            Err(contract("computation profile requires typed provider capabilities"))
        }
    }
}


#[allow(clippy::too_many_lines)]
pub fn provider_effort_profile(
    provider_name: &str,
    capabilities: &ProviderCapabilities,
    physics: &Value,
    source_owned_candidates: Option<&Value>,
    source_owned_solver_policy: Option<&str>,
    prefer_conservative_direct: bool,
) -> CaeResult<Map<String, Value>> {
    let traits = descriptor_traits(capabilities)?;
    let Some(snapshot) = physics.as_object() else {
        return Err(contract("computation profile requires a server physics snapshot"));
    };
    if ["registry_fingerprint", "load_order_fingerprint", "loaded"].iter().any(|k| !snapshot.contains_key(*k))
    {
        return Err(contract("server physics snapshot is incomplete for effort binding"));
    }
    let registry_fingerprint = crate::pyval::py_str(&snapshot["registry_fingerprint"]);
    let load_order_fingerprint = crate::pyval::py_str(&snapshot["load_order_fingerprint"]);
    if !is_digest(&registry_fingerprint) || !is_digest(&load_order_fingerprint) {
        return Err(contract("server physics fingerprints are malformed"));
    }
    let digest = sha256_hex(format!("{registry_fingerprint}:{load_order_fingerprint}").as_bytes());
    let generation = i64::from_str_radix(&digest[..13], 16)
        .map_err(|_| contract("server physics fingerprints are malformed"))?;
    let loaded = match &snapshot["loaded"] {
        Value::Array(items) if items.iter().all(Value::is_string) => items.clone(),
        _ => return Err(contract("server physics load order is malformed")),
    };
    let public_advertised = traits.get("computation_effort");
    if source_owned_solver_policy.is_some() && source_owned_candidates.is_none() {
        return Err(contract("source-owned exact policy requires its private candidate catalog"));
    }
    let advertised =
        if source_owned_candidates.is_some() { source_owned_candidates } else { public_advertised };
    if let Some(candidates) = source_owned_candidates {
        let Some(public) = public_advertised else {
            return Err(contract("source-owned exact candidates require a public direct capability"));
        };
        let (Some(c), Some(p)) = (candidates.as_object(), public.as_object()) else {
            return Err(contract("source-owned exact candidate catalog disagrees with public capability"));
        };
        if c.get("schema") != p.get("schema")
            || c.get("default_solver_policy") != p.get("default_solver_policy")
        {
            return Err(contract("source-owned exact candidate catalog disagrees with public capability"));
        }
        match (p.get("profiles").and_then(Value::as_array), c.get("profiles").and_then(Value::as_array)) {
            (Some(public_rows), Some(candidate_rows))
                if public_rows.iter().all(|r| candidate_rows.contains(r)) => {}
            _ => return Err(contract("source-owned exact candidate catalog omits a public profile")),
        }
    }
    let exact_solver_profile: Map<String, Value> = match advertised {
        None => {
            let mut m = Map::new();
            m.insert("schema".into(), Value::String("implexity-provider-exact-direct-profile/1".into()));
            m.insert("solver_policy".into(), Value::String(EXACT_DIRECT_POLICY.into()));
            m
        }
        Some(advertised) => {
            let ok = advertised.as_object().is_some_and(|a| {
                keys_are(a, &["schema", "default_solver_policy", "profiles"])
                    && a.get("schema").and_then(Value::as_str)
                        == Some("implexity-provider-exact-effort-capability/1")
                    && a.get("default_solver_policy").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                    && a.get("profiles").and_then(Value::as_array).is_some_and(|p| !p.is_empty())
            });
            if !ok {
                return Err(contract("provider computation-effort capability is malformed"));
            }
            let mut profiles: Vec<(String, Map<String, Value>)> = Vec::new();
            for candidate in advertised["profiles"].as_array().into_iter().flatten() {
                let valid = candidate.as_object().is_some_and(|c| {
                    c.get("schema").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                        && c.get("solver_policy").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                });
                if !valid {
                    return Err(contract("provider exact solver profile is malformed"));
                }
                let wire = implexity_core::wire::to_wire(candidate)?;
                let policy = crate::pyval::py_str(&wire["solver_policy"]);
                if profiles.iter().any(|(p, _)| *p == policy) {
                    return Err(contract("provider exact solver policies must be unique"));
                }
                profiles.push((policy, wire.as_object().cloned().unwrap_or_default()));
            }
            let default_policy = crate::pyval::py_str(&advertised["default_solver_policy"]);
            let find = |name: &str| profiles.iter().find(|(p, _)| p == name).map(|(_, m)| m.clone());
            if find(&default_policy).is_none() {
                return Err(contract("provider default exact solver policy is not advertised"));
            }
            match source_owned_solver_policy {
                None => {
                    let selected = if prefer_conservative_direct && find(EXACT_DIRECT_POLICY).is_some() {
                        EXACT_DIRECT_POLICY.to_string()
                    } else {
                        default_policy
                    };
                    find(&selected).unwrap_or_default()
                }
                Some(policy) => find(policy)
                    .ok_or_else(|| contract("source-owned exact policy is absent from candidate catalog"))?,
            }
        }
    };
    let caps_wire = Value::Object(capabilities.to_map());
    let mut profile = Map::new();
    profile.insert("schema".into(), Value::String(EFFORT_PROFILE_SCHEMA.into()));
    profile.insert("provider_id".into(), Value::String(provider_name.into()));
    profile.insert("provider_descriptor_sha256".into(), Value::String(fp(&caps_wire)?));
    profile.insert("registry_generation".into(), Value::from(generation));
    profile.insert("registry_fingerprint".into(), Value::String(registry_fingerprint));
    profile.insert("load_order_fingerprint".into(), Value::String(load_order_fingerprint));
    profile.insert("loaded_packages".into(), Value::Array(loaded));
    profile.insert("execution_truth".into(), Value::String("authoritative_exact".into()));
    profile.insert("solver_policy".into(), exact_solver_profile["solver_policy"].clone());
    profile.insert("exact_solver_profile".into(), Value::Object(exact_solver_profile));
    implexity_core::wire::ensure_finite(&Value::Object(profile.clone()))?;
    Ok(profile)
}


pub fn normalised_public_effort_or_default(raw: Option<&Value>) -> CaeResult<Map<String, Value>> {
    let Some(raw) = raw.filter(|r| !r.is_null()) else {
        let v = serde_json::json!({
            "schema": "implexity-computation-effort-request/1",
            "mode": "exact",
            "hard_budgets": {"wall_time_s": null, "memory_bytes": null},
            "target_update_rate_hz": null,
            "error_limits": {"response": 0.0, "state": 0.0, "gradient": 0.0},
            "trust_radius": 0.0,
            "exact_correction": {"cadence_updates": 1, "deadline_s": 0.0},
            "ood_policy": "refuse",
            "coupling_approximation": {"preset": "exact", "lagged_coupling_ids": []},
        });
        return Ok(v.as_object().cloned().unwrap_or_default());
    };
    let top = [
        "schema",
        "mode",
        "hard_budgets",
        "target_update_rate_hz",
        "error_limits",
        "trust_radius",
        "exact_correction",
        "ood_policy",
        "coupling_approximation",
    ];
    let Some(m) = raw.as_object().filter(|m| keys_are(m, &top)) else {
        return Err(contract("public computation effort is not canonical"));
    };
    if m["schema"].as_str() != Some("implexity-computation-effort-request/1") {
        return Err(contract("public computation effort schema is unsupported"));
    }
    for (name, expected) in [
        ("hard_budgets", &["wall_time_s", "memory_bytes"][..]),
        ("error_limits", &["response", "state", "gradient"][..]),
        ("exact_correction", &["cadence_updates", "deadline_s"][..]),
    ] {
        let mut expected: Vec<&str> = expected.to_vec();
        if name == "hard_budgets"
            && let Some(budget) = m[name].as_object()
            && budget.contains_key("wall_time_mode")
        {
            expected.push("wall_time_mode");
            let memory_ok = budget.get("memory_bytes").and_then(Value::as_i64).is_some_and(|b| b > 0)
                && budget.get("memory_bytes").is_some_and(|v| v.is_i64() || v.is_u64());
            if budget["wall_time_mode"].as_str() != Some("unlimited")
                || !budget.get("wall_time_s").is_none_or(Value::is_null)
                || !memory_ok
            {
                return Err(contract(
                    "canonical unlimited wall time requires null duration and positive explicit memory",
                ));
            }
        }
        if !m[name].as_object().is_some_and(|b| keys_are(b, &expected)) {
            return Err(contract(format!("public computation effort {name} is not canonical")));
        }
    }
    let coupling_ok = m["coupling_approximation"].as_object().is_some_and(|c| {
        keys_are(c, &["preset", "lagged_coupling_ids"])
            && c["preset"]
                .as_str()
                .is_some_and(|p| ["exact", "staged", "interactive", "explicit"].contains(&p))
            && c["lagged_coupling_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())))
    });
    if !coupling_ok {
        return Err(contract("public coupling approximation policy is not canonical"));
    }
    Ok(m.clone())
}

fn effort_error(e: &crate::computation_effort::EffortError) -> CaeError {
    CaeError::contract(e.0.clone())
}


#[allow(clippy::too_many_lines)]
pub fn make_server_effort_binding(
    public_effort: Option<&Value>,
    provider_name: &str,
    capabilities: &ProviderCapabilities,
    physics: &Value,
    operation_context: Option<&Map<String, Value>>,
    source_owned_candidates: Option<&Value>,
) -> CaeResult<Map<String, Value>> {
    let public = normalised_public_effort_or_default(public_effort)?;
    let exact_coupling = serde_json::json!({"preset": "exact", "lagged_coupling_ids": []});
    let conservative =
        public["mode"].as_str() == Some("exact") && public["coupling_approximation"] == exact_coupling;
    let profile = provider_effort_profile(
        provider_name,
        capabilities,
        physics,
        source_owned_candidates,
        None,
        conservative,
    )?;
    let profile_value = Value::Object(profile.clone());
    let profile_digest = fp(&profile_value)?;
    let context = operation_context.cloned().unwrap_or_default();
    let context_ok = keys_are(&context, &["operation", "scope_digest"])
        && context["operation"].as_str().is_some_and(|s| !s.is_empty())
        && context["scope_digest"].as_str().is_some_and(is_digest);
    if !context_ok {
        return Err(contract("server computation operation context is malformed"));
    }
    let mut context_wire = Map::new();
    context_wire.insert("schema".into(), Value::String(EFFORT_CONTEXT_SCHEMA.into()));
    for (k, v) in &context {
        context_wire.insert(k.clone(), v.clone());
    }
    let budgets = public["hard_budgets"].as_object().cloned().unwrap_or_default();
    let wall_mode = budgets.get("wall_time_mode").and_then(Value::as_str).unwrap_or("default").to_string();
    let generation = profile["registry_generation"].as_i64().unwrap_or(0);
    let fingerprint_s = crate::pyval::py_str(&profile["registry_fingerprint"]);
    let profile_id = format!("{provider_name}:{}", crate::pyval::py_str(&profile["solver_policy"]));
    let limits = &public["error_limits"];
    let correction = &public["exact_correction"];
    let requested = ComputationEffortPolicy::from_args(&PolicyArgs {
        mode: &public["mode"],
        wall_time_budget_s: &budgets["wall_time_s"],
        wall_time_mode: &wall_mode,
        memory_budget_bytes: &budgets["memory_bytes"],
        target_update_rate_hz: &public["target_update_rate_hz"],
        max_response_error: &limits["response"],
        max_state_error: &limits["state"],
        max_gradient_error: &limits["gradient"],
        trust_radius: &public["trust_radius"],
        exact_correction_cadence: &correction["cadence_updates"],
        exact_correction_deadline_s: &correction["deadline_s"],
        ood_policy: &public["ood_policy"],
        provider_registry_generation: generation,
        provider_registry_fingerprint: &fingerprint_s,
        provider_profile_id: &profile_id,
        normalized_profile_digest: &profile_digest,
    })
    .map_err(|e| effort_error(&e))?;
    let zero = Value::from(0.0);
    let one = Value::from(1);
    let effective = ComputationEffortPolicy::from_args(&PolicyArgs {
        mode: &Value::String("exact".into()),
        wall_time_budget_s: &budgets["wall_time_s"],
        wall_time_mode: &wall_mode,
        memory_budget_bytes: &budgets["memory_bytes"],
        target_update_rate_hz: &public["target_update_rate_hz"],
        max_response_error: &zero,
        max_state_error: &zero,
        max_gradient_error: &zero,
        trust_radius: &zero,
        exact_correction_cadence: &one,
        exact_correction_deadline_s: &zero,
        ood_policy: &public["ood_policy"],
        provider_registry_generation: generation,
        provider_registry_fingerprint: &fingerprint_s,
        provider_profile_id: &profile_id,
        normalized_profile_digest: &profile_digest,
    })
    .map_err(|e| effort_error(&e))?;
    let reason = if requested.mode == ComputationMode::Exact {
        "requested exact authoritative execution"
    } else {
        "Taylor/JVP proposals selected; authoritative execution upgraded to mandatory exact correction before commit"
    };
    let selection =
        EffectiveComputationEffort::new(requested, effective, reason).map_err(|e| effort_error(&e))?;
    let coupling = public["coupling_approximation"].clone();
    let context_value = Value::Object(context_wire);
    let mut out = Map::new();
    out.insert("schema".into(), Value::String(EFFORT_BINDING_SCHEMA.into()));
    out.insert("selection".into(), selection.to_wire());
    out.insert("requested_policy_digest".into(), Value::String(selection.requested.sha256()));
    out.insert("effective_effort_digest".into(), Value::String(selection.sha256()));
    out.insert("coupling_approximation_digest".into(), Value::String(fp(&coupling)?));
    out.insert("coupling_approximation".into(), coupling);
    out.insert("provider_profile".into(), profile_value);
    out.insert("operation_context_digest".into(), Value::String(fp(&context_value)?));
    out.insert("operation_context".into(), context_value);

    let order = [
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
    Ok(order.iter().map(|k| ((*k).to_string(), out[*k].clone())).collect())
}


#[allow(clippy::too_many_lines)]
pub fn validate_effort_binding(
    raw: &Value,
    facts: Option<&ProviderFacts<'_>>,
) -> CaeResult<(EffectiveComputationEffort, Map<String, Value>)> {
    let top = [
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
    let Some(m) = raw.as_object().filter(|m| keys_are(m, &top)) else {
        return Err(contract("worker computation-effort envelope is not canonical"));
    };
    if m["schema"].as_str() != Some(EFFORT_BINDING_SCHEMA) {
        return Err(contract("worker computation-effort schema is unsupported"));
    }
    let selection = EffectiveComputationEffort::from_wire(&m["selection"])
        .map_err(|_| contract("worker effective computation policy is malformed"))?;
    if m["requested_policy_digest"].as_str() != Some(selection.requested.sha256().as_str()) {
        return Err(contract("worker requested computation policy digest drifted"));
    }
    if m["effective_effort_digest"].as_str() != Some(selection.sha256().as_str()) {
        return Err(contract("worker effective computation effort digest drifted"));
    }
    let coupling = &m["coupling_approximation"];
    let coupling_ok = coupling.as_object().is_some_and(|c| {
        let ids = c.get("lagged_coupling_ids").and_then(Value::as_array);
        keys_are(c, &["preset", "lagged_coupling_ids"])
            && c["preset"]
                .as_str()
                .is_some_and(|p| ["exact", "staged", "interactive", "explicit"].contains(&p))
            && ids.is_some_and(|ids| {
                let unique: std::collections::BTreeSet<String> =
                    ids.iter().map(ToString::to_string).collect();
                ids.len() <= 32
                    && unique.len() == ids.len()
                    && ids
                        .iter()
                        .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.chars().count() <= 160))
            })
    });
    if !coupling_ok {
        return Err(contract("worker coupling approximation policy is malformed"));
    }
    let lagged_empty = coupling["lagged_coupling_ids"].as_array().is_some_and(Vec::is_empty);
    if selection.requested.mode == ComputationMode::Exact
        && (coupling["preset"].as_str() != Some("exact") || !lagged_empty)
    {
        return Err(contract("exact worker policy cannot lag or omit couplings"));
    }
    if m["coupling_approximation_digest"].as_str() != Some(fp(coupling)?.as_str()) {
        return Err(contract("worker coupling approximation policy digest drifted"));
    }
    if selection.effective.mode != ComputationMode::Exact {
        return Err(contract("unregistered approximate execution is forbidden"));
    }
    let profile_keys = [
        "schema",
        "provider_id",
        "provider_descriptor_sha256",
        "registry_generation",
        "registry_fingerprint",
        "load_order_fingerprint",
        "loaded_packages",
        "execution_truth",
        "solver_policy",
        "exact_solver_profile",
    ];
    let Some(profile) = m["provider_profile"].as_object().filter(|p| keys_are(p, &profile_keys)) else {
        return Err(contract("worker provider computation profile is not canonical"));
    };
    let policy = profile["solver_policy"].as_str().filter(|s| !s.is_empty());
    let supported = profile["schema"].as_str() == Some(EFFORT_PROFILE_SCHEMA)
        && profile["execution_truth"].as_str() == Some("authoritative_exact")
        && policy.is_some()
        && profile["exact_solver_profile"]
            .as_object()
            .is_some_and(|e| e.get("solver_policy").and_then(Value::as_str) == policy);
    if !supported {
        return Err(contract("worker provider computation profile is unsupported"));
    }
    if fp(&m["provider_profile"])? != selection.effective.normalized_profile_digest {
        return Err(contract("worker provider computation profile digest drifted"));
    }
    let context_ok = m["operation_context"].as_object().is_some_and(|c| {
        keys_are(c, &["schema", "operation", "scope_digest"])
            && c["schema"].as_str() == Some(EFFORT_CONTEXT_SCHEMA)
    });
    if !context_ok {
        return Err(contract("worker computation operation context is malformed"));
    }
    let context = &m["operation_context"];
    if context["operation"].as_str().is_none_or(str::is_empty)
        || !context["scope_digest"].as_str().is_some_and(is_digest)
    {
        return Err(contract("worker computation operation context is malformed"));
    }
    if m["operation_context_digest"].as_str() != Some(fp(context)?.as_str()) {
        return Err(contract("worker computation operation context digest drifted"));
    }
    if let Some(facts) = facts {
        let conservative = selection.requested.mode == ComputationMode::Exact
            && coupling == &serde_json::json!({"preset": "exact", "lagged_coupling_ids": []});
        let expected = provider_effort_profile(
            facts.provider_name,
            facts.capabilities,
            facts.physics,
            facts.candidates,
            None,
            conservative,
        )?;
        if Value::Object(expected) != m["provider_profile"] {
            return Err(contract("worker provider computation facts drifted"));
        }
    }
    Ok((selection, m.clone()))
}


pub fn rebind_private_exact_effort_profile(raw: &Value, profile: &Value) -> CaeResult<Map<String, Value>> {
    let (selection, checked) = validate_effort_binding(raw, None)?;
    let Some(target) = profile.as_object() else {
        return Err(contract("private exact provider profile is malformed"));
    };
    let original = checked["provider_profile"].as_object().cloned().unwrap_or_default();
    let invariant = [
        "schema",
        "provider_id",
        "provider_descriptor_sha256",
        "registry_generation",
        "registry_fingerprint",
        "load_order_fingerprint",
        "loaded_packages",
        "execution_truth",
    ];
    if invariant.iter().any(|k| target.get(*k) != original.get(*k)) {
        return Err(contract("private exact provider profile changed a bound provider identity"));
    }
    let target = implexity_core::wire::to_wire(&Value::Object(target.clone()))?;
    let digest = fp(&target)?;
    let profile_id = format!(
        "{}:{}",
        crate::pyval::py_str(&target["provider_id"]),
        crate::pyval::py_str(&target["solver_policy"])
    );
    let generation = target["registry_generation"].as_i64().unwrap_or(-1);
    let reg_fp = crate::pyval::py_str(&target["registry_fingerprint"]);
    let requested = selection
        .requested
        .with_identity(generation, &reg_fp, &profile_id, &digest)
        .map_err(|e| effort_error(&e))?;
    let effective = selection
        .effective
        .with_identity(generation, &reg_fp, &profile_id, &digest)
        .map_err(|e| effort_error(&e))?;
    let rebound_selection = EffectiveComputationEffort::new(
        requested,
        effective,
        "source-owned exact mechanism admitted by private child authority",
    )
    .map_err(|e| effort_error(&e))?;
    let mut rebound = checked;
    rebound.insert("selection".into(), rebound_selection.to_wire());
    rebound.insert("requested_policy_digest".into(), Value::String(rebound_selection.requested.sha256()));
    rebound.insert("effective_effort_digest".into(), Value::String(rebound_selection.sha256()));
    rebound.insert("provider_profile".into(), target);
    let (_verified, rebound) = validate_effort_binding(&Value::Object(rebound), None)?;
    Ok(rebound)
}


pub fn private_provider_effort_validation(
    provider_name: &str,
    provider: &dyn CaeProvider,
    capabilities: &ProviderCapabilities,
    physics: &Value,
    effort_binding: &Value,
) -> CaeResult<Option<(Map<String, Value>, Value)>> {
    let candidate_hook = design_operations(provider).and_then(
        implexity_optim::provider_ops::DesignOperations::exact_computation_effort_candidate_profiles,
    );
    let candidates = candidate_hook.transpose()?;
    if let Some(private) =
        crate::exact_acceleration_production_authority::private_production_provider_effort_validation(
            provider_name,
            capabilities,
            physics,
            effort_binding,
            candidates.as_ref(),
        )?
    {
        return Ok(Some(private));
    }
    crate::qualification_benchmark_authority::private_qualification_provider_effort_validation(
        provider_name,
        capabilities,
        physics,
        effort_binding,
        candidates.as_ref(),
    )
}

#[must_use]
pub fn public_coupling_restoration_schedule(coupling: &Value) -> Value {
    let preset = crate::pyval::py_str(&coupling["preset"]);
    let requested = coupling["lagged_coupling_ids"].clone();
    if preset == "exact" {
        return serde_json::json!([{
            "stage": "authoritative_execution",
            "coupling_state": "all_active",
            "lagged_coupling_ids": [],
            "inactive_coupling_ids": [],
            "authoritative": true,
            "truth_status": "exact",
        }]);
    }
    let empty = requested.as_array().is_none_or(Vec::is_empty);
    serde_json::json!([
        {
            "stage": "preview_initial_guess",
            "coupling_state": "temporarily_lagged",
            "coupling_preset": preset,
            "lagged_coupling_ids": requested.clone(),
            "inactive_coupling_ids": requested,
            "provider_resolution_required": empty,
            "authoritative": false,
        },
        {
            "stage": "exact_correction",
            "coupling_state": "all_active",
            "lagged_coupling_ids": [],
            "inactive_coupling_ids": [],
            "method": "selected_provider_authoritative_exact_solve",
            "authoritative": true,
            "truth_status": "exact",
            "required_before_commit": true,
        },
    ])
}


pub fn public_effort_view(raw: &Value) -> CaeResult<Map<String, Value>> {
    let (selection, binding) = validate_effort_binding(raw, None)?;
    let coupling = binding["coupling_approximation"].clone();
    let mut out = Map::new();
    out.insert("schema".into(), Value::String("implexity-computation-effort-selection/1".into()));
    out.insert("selection".into(), selection.to_wire());
    out.insert("requested_policy_digest".into(), binding["requested_policy_digest"].clone());
    out.insert("effective_effort_digest".into(), binding["effective_effort_digest"].clone());
    out.insert("coupling_approximation".into(), coupling.clone());
    out.insert("coupling_approximation_digest".into(), binding["coupling_approximation_digest"].clone());
    out.insert("coupling_restoration_schedule".into(), public_coupling_restoration_schedule(&coupling));
    out.insert("operation_context".into(), binding["operation_context"].clone());
    out.insert("operation_context_digest".into(), binding["operation_context_digest"].clone());
    Ok(out)
}

#[must_use]
#[cfg(target_os = "macos")]
pub fn peak_memory_bytes() -> i64 {
    nix::sys::resource::getrusage(nix::sys::resource::UsageWho::RUSAGE_SELF)
        .map_or(0, |usage| i64::from(usage.max_rss()).max(0))
}

#[must_use]
#[cfg(not(target_os = "macos"))]
pub fn peak_memory_bytes() -> i64 {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else { return 0 };
    text.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<i64>().ok())
        .map_or(0, |kb| kb.saturating_mul(1024))
}


pub fn attach_exact_effort_evidence(
    payload: &Map<String, Value>,
    effort_binding: &Value,
    started_at: Instant,
    update_count: u64,
) -> CaeResult<Map<String, Value>> {
    let (selection, binding) = validate_effort_binding(effort_binding, None)?;
    let elapsed = started_at.elapsed().as_secs_f64().max(0.0);
    #[allow(clippy::cast_precision_loss)]
    let rate = if update_count > 0 && elapsed > 0.0 { update_count as f64 / elapsed } else { 0.0 };
    let observed = ObservedComputationEffort {
        requested_policy_digest: crate::pyval::py_str(&binding["requested_policy_digest"]),
        effective_effort_digest: crate::pyval::py_str(&binding["effective_effort_digest"]),
        normalized_profile_digest: selection.effective.normalized_profile_digest.clone(),
        wall_time_s: Some(elapsed),
        peak_memory_bytes: Some(peak_memory_bytes()),
        published_update_rate_hz: Some(rate),
        updates_since_exact: 0,
        exact_correction_age_s: Some(0.0),
    }
    .checked()
    .map_err(|e| effort_error(&e))?;
    let violations = observed.hard_budget_violations(&selection).map_err(|e| effort_error(&e))?;
    if !violations.is_empty() {
        return Err(contract(format!("hard computation budget refused result: {}", violations.join(", "))));
    }
    let mut bare = payload.clone();
    bare.shift_remove("computation_evidence");
    for key in [
        "requested_policy_digest",
        "effective_effort_digest",
        "coupling_approximation_digest",
        "operation_context_digest",
    ] {
        let expected = binding[key].clone();
        if let Some(v) = bare.get(key)
            && v != &expected
        {
            return Err(contract(format!("result payload {key} disagrees with computation binding")));
        }
        bare.insert(key.into(), expected);
    }
    let coupling = binding["coupling_approximation"].clone();
    let schedule = public_coupling_restoration_schedule(&coupling);
    for (key, expected) in [("coupling_approximation", coupling), ("coupling_restoration_schedule", schedule)]
    {
        if let Some(v) = bare.get(key)
            && v != &expected
        {
            return Err(contract(format!("result payload {key} disagrees with computation binding")));
        }
        bare.insert(key.into(), expected);
    }
    let payload_bytes = canonical_payload_bytes(&bare)?;
    let result_digest = sha256_hex(&payload_bytes);
    let truth = TruthEnvelope {
        mode: selection.effective.mode,
        truth_status: TruthStatus::Exact,
        requested_policy_digest: crate::pyval::py_str(&binding["requested_policy_digest"]),
        effective_effort_digest: crate::pyval::py_str(&binding["effective_effort_digest"]),
        observed_effort_digest: observed.sha256(),
        provider_registry_generation: selection.effective.provider_registry_generation,
        provider_registry_fingerprint: selection.effective.provider_registry_fingerprint.clone(),
        provider_profile_id: selection.effective.provider_profile_id.clone(),
        normalized_profile_digest: selection.effective.normalized_profile_digest.clone(),
        result_digest: Some(result_digest.clone()),
        exact_anchor_digest: None,
        exact_correction_digest: None,
        correction_state: ExactCorrectionState::NotApplicable,
        calibration_digest: None,
        confidence: 1.0,
        response_error_bound: 0.0,
        state_error_bound: 0.0,
        gradient_error_bound: 0.0,
        trust_distance: 0.0,
        out_of_distribution: false,
        refusal_reason: None,
    }
    .checked()
    .map_err(|e| effort_error(&e))?;
    truth.validate_against(&selection, &observed).map_err(|e| effort_error(&e))?;
    ResultWithTruth::new(payload_bytes.clone(), truth.clone()).map_err(|e| effort_error(&e))?;
    let mut bound: Vec<String> = bare.keys().cloned().collect();
    bound.sort();
    let payload_row = crate::canonical::obj([
        ("encoding", Value::String("canonical-json/1".into())),
        ("sha256", Value::String(result_digest)),
        ("bytes", Value::from(payload_bytes.len())),
        ("bound_keys", Value::Array(bound.into_iter().map(Value::String).collect())),
    ]);
    bare.insert(
        "computation_evidence".into(),
        crate::canonical::obj([
            ("schema", Value::String(EFFORT_EVIDENCE_SCHEMA.into())),
            ("selection", selection.to_wire()),
            ("observed", observed.to_wire()),
            ("truth", truth.to_wire()),
            ("payload", payload_row),
        ]),
    );
    Ok(bare)
}


pub fn validate_exact_effort_evidence(
    payload: &Value,
    effort_binding: &Value,
) -> CaeResult<Map<String, Value>> {
    let (selection, binding) = validate_effort_binding(effort_binding, None)?;
    let Some(payload) = payload.as_object() else {
        return Err(contract("worker result is not an object"));
    };
    let evidence = payload.get("computation_evidence").and_then(Value::as_object).filter(|e| {
        keys_are(e, &["schema", "selection", "observed", "truth", "payload"])
            && e["schema"].as_str() == Some(EFFORT_EVIDENCE_SCHEMA)
    });
    let Some(evidence) = evidence else {
        return Err(contract("worker result lacks canonical computation evidence"));
    };
    if evidence["selection"] != selection.to_wire() {
        return Err(contract("worker result computation selection drifted"));
    }
    let malformed = || contract("worker result computation evidence is malformed");
    let observed = ObservedComputationEffort::from_wire(&evidence["observed"]).map_err(|_| malformed())?;
    let truth = TruthEnvelope::from_wire(&evidence["truth"]).map_err(|_| malformed())?;
    observed.validate_against(&selection).map_err(|_| malformed())?;
    truth.validate_against(&selection, &observed).map_err(|_| malformed())?;
    let Some(row) = evidence["payload"].as_object().filter(|r| {
        keys_are(r, &["encoding", "sha256", "bytes", "bound_keys"])
            && r["encoding"].as_str() == Some("canonical-json/1")
    }) else {
        return Err(contract("worker result payload binding is malformed"));
    };
    let bytes_ok = row["bytes"].as_u64().is_some() && (row["bytes"].is_u64() || row["bytes"].is_i64());
    if !row["sha256"].as_str().is_some_and(is_digest) || !bytes_ok {
        return Err(contract("worker result payload binding is malformed"));
    }
    let bound_ok = row["bound_keys"].as_array().is_some_and(|keys| {
        let names: Option<Vec<&str>> = keys.iter().map(Value::as_str).collect();
        names.is_some_and(|names| {
            let mut sorted: Vec<&str> = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted == names
                && !names.contains(&"computation_evidence")
                && names.iter().all(|k| payload.contains_key(*k))
        })
    });
    if !bound_ok {
        return Err(contract("worker result payload key binding is malformed"));
    }
    let bare: Map<String, Value> = row["bound_keys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|k| (k.to_string(), payload[k].clone()))
        .collect();
    for key in [
        "requested_policy_digest",
        "effective_effort_digest",
        "coupling_approximation_digest",
        "operation_context_digest",
    ] {
        if bare.get(key) != Some(&binding[key]) {
            return Err(contract(format!("worker result {key} disagrees with computation binding")));
        }
    }
    if bare.get("coupling_approximation") != Some(&binding["coupling_approximation"])
        || bare.get("coupling_restoration_schedule")
            != Some(&public_coupling_restoration_schedule(&binding["coupling_approximation"]))
    {
        return Err(contract("worker result coupling restoration policy drifted"));
    }
    let payload_bytes = canonical_payload_bytes(&bare)?;
    let result = ResultWithTruth::new(payload_bytes.clone(), truth.clone())
        .map_err(|_| contract("worker result payload digest drifted"))?;
    if row["sha256"].as_str() != Some(result.payload_digest().as_str())
        || row["bytes"].as_u64() != Some(payload_bytes.len() as u64)
    {
        return Err(contract("worker result payload binding drifted"));
    }
    if binding["requested_policy_digest"].as_str() != Some(truth.requested_policy_digest.as_str())
        || binding["effective_effort_digest"].as_str() != Some(truth.effective_effort_digest.as_str())
    {
        return Err(contract("worker result effort identity drifted"));
    }
    Ok(payload.clone())
}


pub fn validate_initial_control_input(output_dir: &Path) -> CaeResult<()> {
    let path = output_dir.join("control.json");
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return Ok(()) };
    let fail = |m: &str| contract(format!("invalid initial control input: {m}"));
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 4096 {
        return Err(fail("control must be a small regular file"));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| fail(&e.to_string()))?;
    let raw = implexity_core::json::parse_strict(&text).map_err(|e| fail(&e.to_string()))?;
    let ok = raw.as_object().is_some_and(|m| {
        m.len() == 1 && m.get("op").and_then(Value::as_str).is_some_and(|op| op == "pause" || op == "stop")
    });
    if ok { Ok(()) } else { Err(fail("expected exactly a pause or stop operation")) }
}
