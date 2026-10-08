// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use crate::orchestration::{AddInRegistry, ExecutionKind, RegisteredAddIn, RuntimeRoute};

#[must_use]
pub fn inspect_entry(entry: &RegisteredAddIn) -> Value {
    let c = &entry.contract;
    let limitations = json!(c.notes);
    let support: Value = match entry.adapter.as_ref() {
        None => json!({"status": "declaration_only", "limitations": limitations}),
        Some(adapter) => {
            if let Some(supplied) = adapter.runtime_support() {
                Value::Object(supplied)
            } else if adapter.has_residual_contributions() {
                json!({"status": "residual_adapter", "limitations": limitations})
            } else if adapter.has_algebraic_evaluate() {
                json!({"status": "algebraic_adapter", "limitations": limitations})
            } else if let Some(provider) = adapter.provider() {
                if let Some(explicit) = provider.runtime_support() {
                    Value::Object(explicit)
                } else {
                    let kind = if c.runtime_route == RuntimeRoute::MatureJob {
                        "model_aware_runtime"
                    } else if c.execution_kind == Some(ExecutionKind::Provider)
                        || (entry.compatibility_mode && c.execution_kind == Some(ExecutionKind::Legacy))
                    {
                        "numerical_provider"
                    } else {
                        "unverified_provider_adapter"
                    };
                    json!({"status": kind, "limitations": limitations})
                }
            } else {
                json!({"status": "unverified_adapter", "limitations": limitations})
            }
        }
    };
    let provider = entry.adapter.as_ref().and_then(|a| a.provider());
    let (slots, authoring) = match (&provider, entry.adapter.as_ref()) {
        (Some(p), _) => (p.component_slots(), p.authoring_contract()),
        (None, Some(a)) => (a.component_slots(), a.authoring_contract()),
        (None, None) => (None, None),
    };
    let kind = entry.adapter.as_ref().and_then(|a| a.component_kind());
    let mut row = Map::new();
    row.insert("authoring_contract".into(), authoring.map_or(Value::Null, Value::Object));
    row.insert("component_kind".into(), json!(kind));
    row.insert("field_component_slots".into(), Value::Object(slots.unwrap_or_default()));
    row.insert("addin_id".into(), json!(c.addin_id));
    row.insert("category".into(), json!(c.category.as_str()));
    row.insert("responses".into(), json!(c.responses.iter().map(|r| &r.response).collect::<Vec<_>>()));
    row.insert("runtime_route".into(), json!(c.runtime_route.as_str()));
    row.insert("execution_kind".into(), json!(c.execution_kind.map(ExecutionKind::as_str)));
    row.insert("runtime_support".into(), support);
    row.insert("model_preflight_required".into(), json!(true));
    row.insert("physical_validation_inferred".into(), json!(false));
    Value::Object(row)
}

#[must_use]
pub fn report(registry: &AddInRegistry) -> Value {
    let mut rows: Vec<Value> = registry.snapshot().entries.iter().map(|e| inspect_entry(e)).collect();
    let fields: Vec<(String, Map<String, Value>)> = rows
        .iter()
        .map(|r| {
            (
                r["addin_id"].as_str().unwrap_or_default().to_string(),
                r["field_component_slots"].as_object().cloned().unwrap_or_default(),
            )
        })
        .collect();
    for row in &mut rows {
        let kind = row["component_kind"].as_str().map(str::to_string);
        let mut integrations = Vec::new();
        if let Some(kind) = kind {
            for (field_solver, slots) in &fields {
                for (name, slot) in slots {
                    if slot.get("component_kind").and_then(Value::as_str) == Some(kind.as_str()) {
                        integrations.push(json!({
                            "field_solver": field_solver,
                            "slot": name,
                            "integration": slot.get("integration").cloned().unwrap_or(Value::Null),
                            "compatibility": "declared_contract_match",
                            "model_preflight_required": true,
                        }));
                    }
                }
            }
        }
        if let Some(obj) = row.as_object_mut() {
            obj.insert("compatible_field_integrations".into(), Value::Array(integrations));
        }
    }
    json!({
        "schema": "implexity-active-component-status/1",
        "components": rows,
        "meaning": "Loaded components are not a claim that every model is executable, coupled, calibrated or validated.",
    })
}
