// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::time::{SystemTime, UNIX_EPOCH};

use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::design_operations;
use serde_json::{Map, Value, json};

use crate::canonical::{canonical_text, sha256_hex};

pub const SCHEMA: &str = "implexity-provider-engineering-problem/1";
const FIELDS: [&str; 5] = ["schema", "provider", "problem", "provenance", "expected_model"];
const MAX_PROBLEM_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROVENANCE_BYTES: usize = 64 * 1024;

fn fail(m: impl Into<String>) -> CaeError {
    CaeError::contract(m.into())
}

fn encoded(value: &Value, path: &str, limit: usize) -> CaeResult<(Value, String)> {
    implexity_core::wire::ensure_finite(value)
        .map_err(|_| fail(format!("{path} contains a non-finite number")))?;
    let text = canonical_text(value);
    if text.len() > limit {
        return Err(fail(format!("{path} exceeds the {limit}-byte public document limit")));
    }
    Ok((value.clone(), text))
}

fn text(value: Option<&Value>, path: &str, maximum: usize) -> CaeResult<String> {
    match value.and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() && s == s.trim() && s.chars().count() <= maximum => Ok(s.to_string()),
        _ => Err(fail(format!("{path} must be nonempty canonical text"))),
    }
}

#[must_use]
pub fn is_provider_problem_envelope(value: &Value) -> bool {
    let Some(m) = value.as_object() else { return false };
    if m.get("schema").and_then(Value::as_str) == Some(SCHEMA) {
        return true;
    }
    m.contains_key("provider") && m.contains_key("problem") && m.keys().all(|k| FIELDS.contains(&k.as_str()))
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}


#[allow(clippy::too_many_lines)]
pub fn normalise_provider_problem_envelope(
    value: &Value,
    model_identity: &Value,
    interface: &str,
    actor: &str,
) -> CaeResult<Value> {
    if !is_provider_problem_envelope(value) {
        return Err(fail(format!(
            "provider-owned declarations require schema {}, provider and problem",
            repr_str(SCHEMA)
        )));
    }
    let m = value.as_object().cloned().unwrap_or_default();
    let mut extra: Vec<String> = m.keys().filter(|k| !FIELDS.contains(&k.as_str())).cloned().collect();
    extra.sort();
    if !extra.is_empty() {
        return Err(fail(format!(
            "provider-owned declaration contains unsupported envelope fields {}",
            implexity_core::pyobj::list_repr(&extra)
        )));
    }
    if m.get("schema").is_some_and(|s| s.as_str() != Some(SCHEMA)) {
        return Err(fail(format!("provider-owned declaration schema must be {}", repr_str(SCHEMA))));
    }
    let provider_id = text(m.get("provider"), "provider", 160)?;
    let Some(problem @ Value::Object(_)) = m.get("problem") else {
        return Err(fail("problem must be an object"));
    };
    if model_identity.as_object().is_none_or(Map::is_empty) {
        return Err(fail("provider-owned declaration requires the live model identity"));
    }
    if let Some(expected) = m.get("expected_model") {
        let Some(e) = expected
            .as_object()
            .filter(|e| e.len() == 2 && e.contains_key("structure_id") && e.contains_key("content_id"))
        else {
            return Err(fail("expected_model requires structure_id and content_id"));
        };
        for key in ["structure_id", "content_id"] {
            text(e.get(key), &format!("expected_model.{key}"), 160)?;
        }
        if ["structure_id", "content_id"].iter().any(|k| e.get(*k) != model_identity.get(*k)) {
            return Err(fail(
                "The model changed before the problem could be saved; review the current geometry and validate again",
            ));
        }
    }
    let (source_problem, source_text) = encoded(problem, "problem", MAX_PROBLEM_BYTES)?;
    let (model, model_text) = encoded(model_identity, "model_identity", MAX_PROVENANCE_BYTES)?;
    let provenance =
        m.get("provenance").filter(|v| crate::pyval::truthy(Some(v))).cloned().unwrap_or_else(|| json!({}));
    let (client_provenance, _) = encoded(&provenance, "provenance", MAX_PROVENANCE_BYTES)?;
    let interface = text(Some(&Value::String(interface.into())), "interface", 160)?;
    let actor = text(Some(&Value::String(actor.into())), "actor", 160)?;
    let registries = implexity_core::registries::global();
    let before = registries.providers.binding_token();
    let provider = registries.providers.get(&provider_id).map_err(|e| fail(e.message().to_string()))?;
    let normalised_raw = provider.normalise_problem(&source_problem).map_err(|e| {
        fail(format!("provider {} rejected its problem document: {}", repr_str(&provider_id), e.message()))
    })?;
    let document =
        match design_operations(provider.as_ref()).and_then(|o| o.problem_document(&normalised_raw)) {
            Some(d) => Some(d?),
            None => normalised_raw.downcast_ref::<Value>().cloned(),
        };
    let Some(normalised_value @ Value::Object(_)) = document else {
        return Err(fail(format!(
            "provider {} normalise_problem must return an object",
            repr_str(&provider_id)
        )));
    };
    let (normalised, normalised_text) = encoded(&normalised_value, "normalised_problem", MAX_PROBLEM_BYTES)?;
    let caps = Value::Object(provider.capabilities()?.to_map());
    let (capabilities, capabilities_text) = encoded(&caps, "provider.capabilities", MAX_PROVENANCE_BYTES)?;
    let after = registries.providers.binding_token();
    if before != after {
        return Err(fail("provider registry changed while the problem document was validated"));
    }
    let source_schema = source_problem.get("schema").cloned().unwrap_or(Value::Null);
    let normalised_schema = normalised.get("schema").cloned().unwrap_or(Value::Null);
    if !source_schema.is_null() {
        text(Some(&source_schema), "problem.schema", 256)?;
    }
    if !normalised_schema.is_null() {
        text(Some(&normalised_schema), "normalised_problem.schema", 256)?;
    }
    let identity_core = json!({
        "provider": {
            "id": provider_id,
            "implementation": provider.implementation(),
            "capabilities_sha256": sha256_hex(capabilities_text.as_bytes()),
        },
        "problem_sha256": sha256_hex(normalised_text.as_bytes()),
        "model_sha256": sha256_hex(model_text.as_bytes()),
    });
    let problem_id = sha256_hex(canonical_text(&identity_core).as_bytes())[..16].to_string();
    let mut identity: Map<String, Value> = identity_core.as_object().cloned().unwrap_or_default();
    identity.insert("problem_id".into(), Value::String(problem_id.clone()));
    identity.insert("submitted_problem_schema".into(), source_schema);
    identity.insert("normalised_problem_schema".into(), normalised_schema);
    identity.insert("submitted_problem_sha256".into(), Value::String(sha256_hex(source_text.as_bytes())));
    Ok(json!({
        "schema": SCHEMA,
        "provider": provider_id,
        "problem": normalised,
        "model": model,
        "identity": identity,
        "problem_id": problem_id,
        "provenance": {
            "action": "set_engineering_problem",
            "interface": interface,
            "actor": actor,
            "validation": "registered_provider.normalise_problem",
            "provider_registry": {"generation": before.generation, "fingerprint": before.fingerprint},
            "provider_capabilities": capabilities,
            "client": client_provenance,
        },
        "updated": implexity_optim::numeric::float_value(now()),
    }))
}


pub fn declaration_for_rebind(record: &Value) -> CaeResult<Value> {
    if record.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(fail("stored provider problem record is malformed"));
    }
    let client = record
        .get("provenance")
        .and_then(Value::as_object)
        .map_or_else(|| json!({}), |p| p.get("client").cloned().unwrap_or_else(|| json!({})));
    Ok(json!({
        "schema": SCHEMA,
        "provider": record.get("provider").cloned().unwrap_or(Value::Null),
        "problem": record.get("problem").cloned().unwrap_or(Value::Null),
        "provenance": client,
    }))
}


pub fn prepare_provider_revisions(previous: &Value, revised: &Value) -> CaeResult<(Value, usize)> {
    fn walk(before: &Value, after: &mut Value, rebuilt: &mut usize) -> CaeResult<()> {
        match (before, after) {
            (Value::Object(b), Value::Object(a)) => {
                let key = ["provider", "physics_provider"].into_iter().find(|k| {
                    a.get(*k).is_some_and(Value::is_string) && a.get("problem").is_some_and(Value::is_object)
                });
                if let Some(key) = key {
                    if b.get(key) != a.get(key) {
                        return Err(fail("geometry revision cannot change the selected physics provider"));
                    }
                    let Some(old @ Value::Object(_)) = b.get("problem") else {
                        return Err(fail("previous provider problem is missing"));
                    };
                    if !a.get("problem").is_some_and(|new| implexity_core::pyobj::py_eq(old, new)) {
                        let name = a[key].as_str().unwrap_or_default().to_string();
                        let provider = implexity_core::registries::global().providers.get(&name)?;
                        let replanned = design_operations(provider.as_ref())
                            .and_then(|o| o.replan_problem_revision(old, &a["problem"]));
                        if let Some(replanned) = replanned {
                            a.insert("problem".into(), replanned?);
                            *rebuilt += 1;
                        }
                    }
                    return Ok(());
                }
                let keys: Vec<String> = a.keys().cloned().collect();
                for k in keys {
                    if let (Some(bv), Some(av)) = (b.get(&k), a.get_mut(&k)) {
                        walk(bv, av, rebuilt)?;
                    }
                }
                Ok(())
            }
            (Value::Array(b), Value::Array(a)) => {
                for (old, new) in b.iter().zip(a.iter_mut()) {
                    walk(old, new, rebuilt)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    let mut output = revised.clone();
    let mut rebuilt = 0;
    walk(previous, &mut output, &mut rebuilt)?;
    Ok((output, rebuilt))
}
