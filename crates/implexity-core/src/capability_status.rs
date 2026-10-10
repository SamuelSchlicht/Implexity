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
