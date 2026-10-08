// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::contracts::{CaeProvider, ProviderCapabilities};
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::design_operations;
use serde_json::{Map, Value, json};

pub const CATALOG_SCHEMA: &str = "implexity-coupling-catalog/1";
pub const PROVIDER_SCHEMA: &str = "implexity-provider-coupling-control/1";
const STATES: [&str; 3] = ["active", "disabled", "lagged"];
const COST_TIERS: [&str; 6] = ["high", "low", "moderate", "negligible", "unknown", "very_high"];

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}

fn text(value: Option<&Value>, path: &str) -> CaeResult<String> {
    match value.and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() && s == s.trim() && s.chars().count() <= 4096 => Ok(s.to_string()),
        _ => Err(contract(format!("{path} must be nonempty canonical text"))),
    }
}

fn py_list(items: &[&str]) -> String {
    implexity_core::pyobj::list_repr(items)
}

#[allow(clippy::too_many_lines)]
fn normalise_entry(raw: &Value, provider_id: &str, index: usize) -> CaeResult<Value> {
    let path = format!("provider {} coupling {index}", repr_str(provider_id));
    let Some(m) = raw.as_object() else {
        return Err(contract(format!("{path} must be an object")));
    };
    let required = [
        "allowed_states",
        "cost_note",
        "cost_tier",
        "default_state",
        "description",
        "id",
        "label",
        "truth_status_by_state",
    ];
    let missing: Vec<&str> = required.iter().copied().filter(|k| !m.contains_key(*k)).collect();
    if !missing.is_empty() {
        return Err(contract(format!("{path} omitted presentation fields {}", py_list(&missing))));
    }
    let id = text(m.get("id"), &format!("{path}.id"))?;
    let label = text(m.get("label"), &format!("{path}.label"))?;
    let description = text(m.get("description"), &format!("{path}.description"))?;
    let default_state = text(m.get("default_state"), &format!("{path}.default_state"))?;
    let states: Vec<String> = match &m["allowed_states"] {
        Value::Array(items)
            if !items.is_empty()
                && items.iter().all(|v| v.as_str().is_some_and(|s| STATES.contains(&s)))
                && items.iter().map(Value::to_string).collect::<std::collections::BTreeSet<_>>().len()
                    == items.len() =>
        {
            items.iter().map(crate::pyval::py_str).collect()
        }
        _ => {
            return Err(contract(format!(
                "{path}.allowed_states must be unique entries from {}",
                py_list(&STATES)
            )));
        }
    };
    if !states.contains(&default_state) {
        return Err(contract(format!("{path}.default_state must be allowed")));
    }
    let cost_tier = text(m.get("cost_tier"), &format!("{path}.cost_tier"))?;
    if !COST_TIERS.contains(&cost_tier.as_str()) {
        return Err(contract(format!("{path}.cost_tier must be one of {}", py_list(&COST_TIERS))));
    }
    let cost_note = text(m.get("cost_note"), &format!("{path}.cost_note"))?;
    let truth_ok = m["truth_status_by_state"].as_object().is_some_and(|t| {
        t.len() == states.len()
            && states.iter().all(|s| t.contains_key(s))
            && t.values().all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
    });
    if !truth_ok {
        return Err(contract(format!(
            "{path}.truth_status_by_state must describe every allowed state exactly once"
        )));
    }
    let restoration = match m.get("restoration_required_before_commit") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(contract(format!("{path}.restoration_required_before_commit must be boolean")));
        }
    };
    let current_state = match m.get("current_state") {
        None => default_state.clone(),
        Some(Value::String(s)) if states.contains(s) => s.clone(),
        Some(_) => return Err(contract(format!("{path}.current_state must be allowed"))),
    };
    let configuration = match m.get("configuration") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::Object(c)) => {
            let value = Value::Object(c.clone());
            if implexity_core::wire::ensure_finite(&value).is_err() {
                return Err(contract(format!("{path}.configuration must be finite JSON data")));
            }
            if crate::canonical::canonical_text(&value).len() > 65536 {
                return Err(contract(format!("{path}.configuration is too large")));
            }
            value
        }
        Some(_) => return Err(contract(format!("{path}.configuration must be an object or null"))),
    };
    Ok(json!({
        "id": id,
        "label": label,
        "description": description,
        "default_state": default_state,
        "allowed_states": states,
        "cost_tier": cost_tier,
        "cost_note": cost_note,
        "truth_status_by_state": m["truth_status_by_state"].clone(),
        "restoration_required_before_commit": restoration,
        "current_state": current_state,
        "configuration": configuration,
    }))
}

fn legacy_laggable(provider_id: &str, raw: &Map<String, Value>) -> CaeResult<Value> {
    let ids = match raw.get("explicit_laggable_coupling_ids") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(v) if !crate::pyval::truthy(Some(v)) => Vec::new(),
        Some(_) => {
            return Err(contract(format!(
                "provider {} coupling approximation IDs are malformed",
                repr_str(provider_id)
            )));
        }
    };
    let mut couplings = Vec::new();
    for value in &ids {
        let id = text(Some(value), &format!("provider {} coupling id", repr_str(provider_id)))?;
        couplings.push(json!({
            "id": id,
            "label": id,
            "description": "Provider-approved exchange that may be lagged only while constructing a non-authoritative initial guess.",
            "default_state": "active",
            "allowed_states": ["active", "lagged"],
            "cost_tier": "unknown",
            "cost_note": "This legacy provider did not publish a qualitative cost tier.",
            "truth_status_by_state": {
                "active": "exact_within_provider_scope",
                "lagged": "approximate_initialization_only",
            },
            "restoration_required_before_commit": true,
            "current_state": "active",
            "configuration": {
                "kind": "computation_effort_request",
                "field": "coupling_approximation.lagged_coupling_ids",
                "active_when_absent": true,
            },
        }));
    }
    Ok(json!({
        "schema": PROVIDER_SCHEMA,
        "selection_scope": "initialization_only",
        "default_policy": "all_active",
        "couplings": couplings,
        "presets": crate::pyval::mapping_or_empty(raw.get("presets")),
    }))
}

fn fixed_profile(provider_id: &str, raw: &Map<String, Value>) -> CaeResult<Value> {
    let categories = [
        ("active_coupling_ids", "active", "Retained by this provider profile."),
        ("simplified_coupling_ids", "active", "Implemented in a provider-declared simplified form."),
        ("omitted_coupling_ids", "disabled", "Omitted by this provider profile."),
    ];
    let mut rows = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let profile_id = crate::pyval::text_or(raw.get("id"), provider_id);
    for (key, state, description) in categories {
        let values = match raw.get(key) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(v) if !crate::pyval::truthy(Some(v)) => Vec::new(),
            Some(_) => {
                return Err(contract(format!(
                    "provider {} coupling profile {key} is malformed",
                    repr_str(provider_id)
                )));
            }
        };
        for value in &values {
            let id = text(Some(value), &format!("provider {} coupling id", repr_str(provider_id)))?;
            if seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            let truth =
                if state == "active" { "approximate_provider_scope" } else { "omitted_provider_scope" };
            let mut truth_map = Map::new();
            truth_map.insert(state.into(), Value::String(truth.into()));
            rows.push(json!({
                "id": id,
                "label": id,
                "description": description,
                "default_state": state,
                "allowed_states": [state],
                "cost_tier": "unknown",
                "cost_note": "The fixed provider profile does not publish a selectable cost tier.",
                "truth_status_by_state": truth_map,
                "restoration_required_before_commit": false,
                "current_state": state,
                "configuration": {
                    "kind": "fixed_provider_profile",
                    "profile_id": profile_id,
                    "editable": false,
                },
            }));
        }
    }
    Ok(json!({
        "schema": PROVIDER_SCHEMA,
        "selection_scope": "fixed_provider_profile",
        "default_policy": "provider_fixed",
        "couplings": rows,
        "presets": {},
    }))
}


pub fn normalise_provider_coupling_control(
    provider_id: &str,
    capabilities: &ProviderCapabilities,
    provider: Option<&dyn CaeProvider>,
    problem: Option<&Value>,
) -> CaeResult<Value> {
    let provider_id = text(Some(&Value::String(provider_id.to_string())), "provider_id")?;
    let traits = match capabilities.get("traits") {
        Some(Value::Object(t)) => t,
        _ => Map::new(),
    };
    let mut raw: Option<Value> = None;
    if let Some(hook) = provider.and_then(design_operations).and_then(|o| o.coupling_control(problem)) {
        raw = Some(hook?).filter(|v| !v.is_null());
    }
    if raw.is_none() {
        raw = traits.get("coupling_control").cloned().filter(|v| !v.is_null());
    }
    if raw.is_none()
        && let Some(Value::Object(approx)) = traits.get("coupling_approximation")
    {
        raw = Some(legacy_laggable(&provider_id, approx)?);
    }
    if raw.is_none()
        && let Some(Value::Object(profile)) = traits.get("coupling_profile")
    {
        raw = Some(fixed_profile(&provider_id, profile)?);
    }
    let Some(raw) = raw else {
        return Ok(json!({
            "provider_id": provider_id,
            "available": false,
            "reason": "provider_did_not_publish_coupling_control",
            "selection_scope": "unavailable",
            "couplings": [],
            "presets": {},
        }));
    };
    let Some(m) =
        raw.as_object().filter(|m| m.get("schema").and_then(Value::as_str) == Some(PROVIDER_SCHEMA))
    else {
        return Err(contract(format!(
            "provider {} coupling-control declaration is malformed",
            repr_str(&provider_id)
        )));
    };
    let scope =
        text(m.get("selection_scope"), &format!("provider {}.selection_scope", repr_str(&provider_id)))?;
    let default_policy = match m.get("default_policy") {
        None => "all_active".to_string(),
        v => text(v, &format!("provider {}.default_policy", repr_str(&provider_id)))?,
    };
    let entries = match m.get("couplings") {
        Some(Value::Array(items)) if items.len() <= 512 => items,
        _ => {
            return Err(contract(format!(
                "provider {}.couplings must be an array of at most 512 entries",
                repr_str(&provider_id)
            )));
        }
    };
    let rows: Vec<Value> = entries
        .iter()
        .enumerate()
        .map(|(i, v)| normalise_entry(v, &provider_id, i))
        .collect::<CaeResult<_>>()?;
    let ids: std::collections::BTreeSet<String> = rows.iter().map(|r| r["id"].to_string()).collect();
    if ids.len() != rows.len() {
        return Err(contract(format!("provider {} coupling IDs must be unique", repr_str(&provider_id))));
    }
    let presets = match m.get("presets") {
        None => Value::Object(Map::new()),
        Some(Value::Object(p)) => Value::Object(p.clone()),
        Some(_) => {
            return Err(contract(format!("provider {}.presets must be an object", repr_str(&provider_id))));
        }
    };
    Ok(json!({
        "provider_id": provider_id,
        "available": true,
        "schema": PROVIDER_SCHEMA,
        "selection_scope": scope,
        "default_policy": default_policy,
        "couplings": rows,
        "presets": presets,
    }))
}

#[must_use]
pub fn problem_for_provider<'a>(
    provider_id: &str,
    context: Option<&'a Value>,
    allow_direct: bool,
) -> Option<&'a Value> {
    let context = context?;
    let map = context.as_object()?;
    if let Some(Value::Object(direct)) = map.get("provider_problems")
        && let Some(p @ Value::Object(_)) = direct.get(provider_id)
    {
        return Some(p);
    }
    if map.get("provider").and_then(Value::as_str) == Some(provider_id)
        && let Some(p @ Value::Object(_)) = map.get("problem")
    {
        return Some(p);
    }
    for key in ["physics", "problem", "authoring", "intent"] {
        if let Some(nested @ Value::Object(_)) = map.get(key)
            && let Some(found) = problem_for_provider(provider_id, Some(nested), false)
        {
            return Some(found);
        }
    }
    if allow_direct { Some(context) } else { None }
}


pub fn coupling_catalogue(
    provider_id: Option<&str>,
    problem_context: Option<&Value>,
    context_source: &str,
) -> CaeResult<Value> {
    let registries = implexity_core::registries::global();
    let snapshot = registries.providers.snapshot();
    let mut rows = Vec::new();
    let providers: Vec<(String, Arc<dyn CaeProvider>)> = snapshot.entries.clone();
    for (name, provider) in providers {
        if provider_id.is_some_and(|p| p != name) {
            continue;
        }
        let capabilities = provider.capabilities()?;
        let provider_problem =
            problem_for_provider(&name, problem_context, provider_id == Some(name.as_str()));
        let mut row = normalise_provider_coupling_control(
            &name,
            &capabilities,
            Some(provider.as_ref()),
            provider_problem,
        )?;

        let raw_problem = provider_problem.map(|p| crate::results::json_problem(p.clone()));
        if let Value::Object(m) = &mut row {
            m.insert("problem_context_applied".into(), Value::Bool(provider_problem.is_some()));
            m.insert(
                "implemented_feedback".into(),
                implexity_core::coupling_inventory::inspect_inventory(
                    provider.as_ref(),
                    &name,
                    raw_problem.as_ref(),
                )?,
            );
            if let Some(Value::String(truth)) =
                capabilities.get("traits").and_then(|t| t.get("truth_status").cloned())
            {
                m.insert("provider_truth_status".into(), Value::String(truth));
            }
        }
        rows.push(row);
    }
    if let Some(p) = provider_id
        && rows.is_empty()
    {
        return Err(contract(format!("unknown CAE provider {}", repr_str(p))));
    }
    let token = registries.providers.binding_token();
    Ok(json!({
        "schema": CATALOG_SCHEMA,
        "provider_registry": {"generation": token.generation, "fingerprint": token.fingerprint},
        "providers": rows,
        "problem_context": {"source": context_source, "supplied": problem_context.is_some()},
        "request_contract": {
            "initialization_approximation": {
                "field": "computation_effort.coupling_approximation",
                "supported_selectable_states": ["active", "lagged"],
                "lagged_wire_field": "lagged_coupling_ids",
                "exact_mode_forces_all_active": true,
            },
            "provider_owned_selection": {
                "supported_selectable_states": ["active", "lagged", "disabled"],
                "configuration_is_declared_per_coupling": true,
                "configuration_action_is_declared_per_coupling": true,
                "selection_must_match_provider_allowed_states": true,
            },
            "exact_mode_forces_all_active": true,
            "note": "A state is selectable only when the provider advertises it. Generic computation-effort requests may lag approved couplings during initialization. Providers may additionally expose whole-run active, lagged, or disabled states through each row's explicit configuration field and action; clients must never infer a wire path.",
        },
    }))
}
