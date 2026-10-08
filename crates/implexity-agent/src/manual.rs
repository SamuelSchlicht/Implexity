// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;

use implexity_core::pyobj::repr;
use serde_json::{Map, Value, json};

use crate::contracts::contracts;
use crate::error::{AgentError, AgentResult};
use crate::runtime::AgentManager;

pub const WORKFLOW_CATALOG_SCHEMA: &str = "implexity-agent-workflow-catalog/1";

fn workflow_id_ok(id: &str) -> bool {
    let mut parts = id.split('_');
    let Some(first) = parts.next() else { return false };
    let lower_alnum =
        |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && lower_alnum(first)
        && parts.all(lower_alnum)
}

fn invalid(m: impl Into<String>) -> AgentError {
    AgentError::contract(m)
}



pub fn validate_workflow_catalog(
    document: &Value,
    known: &BTreeSet<String>,
) -> AgentResult<Vec<Map<String, Value>>> {
    let d = document.as_object().filter(|m| {
        m.len() == 2
            && m.get("schema") == Some(&json!(WORKFLOW_CATALOG_SCHEMA))
            && m.get("workflows").is_some_and(Value::is_array)
    });
    let Some(d) = d else { return Err(invalid("invalid agent workflow catalogue")) };
    let c = contracts()?;
    let mut out = Vec::new();
    let required = ["package_id", "id", "label", "steps", "guidance"];
    for row in d.get("workflows").and_then(Value::as_array).into_iter().flatten() {
        let Some(r) = row.as_object().filter(|r| {
            required.iter().all(|k| r.contains_key(*k))
                && r.keys().all(|k| required.contains(&k.as_str()) || k == "example_authoring")
        }) else {
            return Err(invalid("agent workflow entry has unknown or missing fields"));
        };
        let id = r.get("id").cloned().unwrap_or(Value::Null);
        if r.get("example_authoring").is_some_and(|e| !e.is_object()) {
            return Err(invalid(format!(
                "agent workflow {}: example_authoring must be an object",
                repr(&id)
            )));
        }
        let package = r.get("package_id").cloned().unwrap_or(Value::Null);
        if !package.as_str().is_some_and(|p| known.contains(p)) {
            return Err(invalid(format!(
                "agent workflow {} names uncatalogued package {}",
                repr(&id),
                repr(&package)
            )));
        }
        if !id.as_str().is_some_and(workflow_id_ok) {
            return Err(invalid("agent workflow ids must be snake_case"));
        }
        let label_ok = r.get("label").and_then(Value::as_str).is_some_and(|l| !l.is_empty());
        if !label_ok || !r.get("guidance").is_some_and(Value::is_string) {
            return Err(invalid(format!("agent workflow {}: label and guidance must be text", repr(&id))));
        }
        let steps_ok = r.get("steps").and_then(Value::as_array).is_some_and(|s| {
            !s.is_empty() && s.iter().all(|step| step.as_str().is_some_and(|n| c.get(n).is_some()))
        });
        if !steps_ok {
            return Err(invalid(format!("agent workflow {}: steps must be known agent actions", repr(&id))));
        }
        out.push(r.clone());
    }
    Ok(out)
}



pub fn package_workflows() -> AgentResult<Vec<Value>> {
    let manager = implexity_core::packages::global();
    let loaded: BTreeSet<String> = manager.selected().into_iter().collect();
    let known: BTreeSet<String> = manager.descriptors()?.iter().map(|d| d.package_id.clone()).collect();
    let kernel = kernel_workflows()?;
    let mut seen: BTreeSet<String> =
        kernel.iter().filter_map(|w| w.get("id").and_then(Value::as_str)).map(str::to_owned).collect();
    let documents = implexity_core::distributions::global()
        .catalogue_documents("agent_workflows")
        .map_err(|e| AgentError::contract(e.to_string()))?;
    let mut rows = Vec::new();
    for (_distribution, document) in documents {
        for mut row in validate_workflow_catalog(&document, &known)? {
            let id = row.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            if !seen.insert(id.clone()) {
                return Err(invalid(format!("agent workflow {} is declared twice", repr(&json!(id)))));
            }
            let package = row.shift_remove("package_id").unwrap_or(Value::Null);
            let is_loaded = package.as_str().is_some_and(|p| loaded.contains(p));
            row.insert("package".into(), package);
            row.insert("package_loaded".into(), json!(is_loaded));
            rows.push(Value::Object(row));
        }
    }
    Ok(rows)
}

fn kernel_workflows() -> AgentResult<Vec<Value>> {
    Ok(contracts()?.manual_rows().get("workflows").and_then(Value::as_array).cloned().unwrap_or_default())
}



#[allow(clippy::too_many_lines)]
pub fn build_manual(manager: &AgentManager) -> AgentResult<Value> {
    manager.capabilities()?;
    let live = manager.state()?;
    let c = contracts()?;
    let actions: Vec<Value> = c
        .actions()
        .iter()
        .chain(manager.served_extension_actions()?)
        .map(|a| {
            json!({"action": a.name, "tool_name": format!("implexity_{}", a.name), "label": a.label,
                   "description": if a.description.is_empty() { &a.label } else { &a.description },
                   "permission": a.permission, "allowed": manager.action_allowed(a),
                   "mutates_model_or_job": a.mutates, "requires_model": a.requires_model,
                   "input_schema": a.input_schema})
        })
        .collect();
    let mut workflows = package_workflows()?;
    workflows.extend(kernel_workflows()?);
    if manager.extension_active() {
        workflows.push(json!({
            "id": "dynamic_result_views",
            "label": "Read and display dynamic results",
            "steps": ["inspect_dynamic_results", "read_time_series", "render_dynamic_frames", "render_cycle_animation", "export_animation"],
            "guidance": "Select a store, field, and time or phase. Keep display interpolation separate from solved states."
        }));
    }
    let rows = c.manual_rows();
    let get = |k: &str| live.get(k).cloned();
    Ok(json!({
        "schema": "implexity-engineering-agent-manual/1",
        "title": "Implexity Engineering Agent operating manual",
        "purpose": "Use the framework tools, solvers, and direct-gradient optimizer.",
        "operating_model": {
            "before_action": "Read the current model, job, and provider identities.",
            "execute": "Call a tool only when allowed=true. Use its declared inputs.",
            "after_mutation": "Read the state after each change.",
            "optimization": "Use the framework optimizer. Do not replace it with client calculations.",
            "manual_intervention": "Enter intervention mode before a manual change. Continue with a linked branch after a new preflight.",
        },
        "capability_interpretation": {
            "installed_package": "Load required packages. Check active components.",
            "constitutive_component": "Select a field provider for spatial solves.",
            "executable_provider": "Check provider operations, backend requirements, and limits.",
            "differentiable_response": "Check coordinate and coupling derivatives. Treat imported fixed loads as constant.",
            "coupled_solution": "Check coupling execution, transfer conservation, and convergence.",
            "physical_qualification": "Keep model limits, calibration, and verification scope with results.",
        },
        "request_checklist": [
            "Get the current tool list and permissions.",
            "Read the provider template and input schema.",
            "Set design bounds and regions. Protect fixed geometry, flow ports, and supports.",
            "Check grid registration and units.",
            "Set response units, direction, limits, and scales. Record material sources.",
            "Save with the intended model identity. Run preflight before submission.",
            "Submit each job once. Keep its request, reply, identity, and epoch exports.",
            "Match model, problem, and operating-point identities before comparison.",
        ],
        "workflows": workflows,
        "safety_rules": rows.get("safety_rules").cloned().unwrap_or_else(|| json!([])),
        "recovery": rows.get("recovery").cloned().unwrap_or_else(|| json!([])),
        "actions": actions,
        "policy": manager.policy()?,
        "available_physics": get("available_physics").unwrap_or_else(|| json!({})),
        "available_physics_addins": get("available_physics_addins").unwrap_or_else(|| json!({})),
        "physics_component_status": get("physics_component_status").unwrap_or_else(|| json!({})),
        "automatic_physics_plan": get("physics_plan").unwrap_or(Value::Null),
        "engineering_intent": manager.get_intent()?,
        "notes": [
            "Get permission before changing the requested physics.",
            "Check problem_requirements before applying problem_patch. Refresh mismatched templates. Use editor_schema_patch only for display settings.",
        ],
    }))
}



pub fn concise_context(manager: &AgentManager) -> AgentResult<Value> {
    let manual = build_manual(manager)?;
    let guidance = crate::guidance::build_guidance(manager)?;
    let allowed: Vec<Value> = manual["actions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|x| x["allowed"] == true)
                .map(|x| json!({"action": x["action"], "label": x["label"]}))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "schema": "implexity-engineering-agent-context/1",
        "purpose": manual["purpose"], "operating_model": manual["operating_model"],
        "allowed_actions": allowed,
        "workflows": manual["workflows"], "safety_rules": manual["safety_rules"], "recovery": manual["recovery"],
        "capability_interpretation": manual["capability_interpretation"],
        "request_checklist": manual["request_checklist"],
        "available_physics": manual["available_physics"],
        "available_physics_addins": manual["available_physics_addins"],
        "physics_component_status": manual["physics_component_status"],
        "automatic_physics_plan": manual["automatic_physics_plan"],
        "engineering_intent": manual["engineering_intent"],
        "guidance": guidance,
    }))
}

