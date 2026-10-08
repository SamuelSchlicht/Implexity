// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::py_repr::repr_str;
use serde_json::{Map, Value, json};

use crate::contracts::{ActionContract, contracts};
use crate::error::AgentResult;
use crate::pyval::{py_str, truthy};
use crate::runtime::AgentManager;

pub const SCHEMA: &str = "implexity-engineering-agent-guidance/1";
pub const CATEGORY_ORDER: [&str; 5] =
    ["Required next", "Useful now", "Blocked now", "Unavailable", "Redundant now"];

const RUNNING: [&str; 3] = ["queued", "running", "resuming"];
const TERMINAL: [&str; 5] = ["completed", "stopped", "accepted", "discarded", "error"];
const GEOMETRY_MUTATIONS: [&str; 14] = [
    "set_parameters",
    "edit_graph",
    "promote_cage",
    "rebind_region",
    "interaction_begin",
    "interaction_preview",
    "interaction_commit",
    "interaction_cancel",
    "manipulation_begin",
    "manipulation_preview",
    "manipulation_commit",
    "manipulation_cancel",
    "manipulation_undo",
    "manipulation_redo",
];
const ATTENTION_OBSERVATIONAL_ACTIONS: [&str; 17] = [
    "inspect_state",
    "inspect_optimization_job",
    "export_optimization_epoch",
    "render_optimization_history",
    "render_section",
    "render_3d",
    "export_stl",
    "inspect_renderables",
    "read_result_array",
    "inspect_physics_packages",
    "inspect_couplings",
    "inspect_boundary_controls",
    "inspect_workspace_capabilities",
    "inspect_physics_plan",
    "inspect_parameters",
    "inspect_study",
    "inspect_managed_evaluation",
];

fn status(job: &Value) -> String {
    job.get("status").filter(|v| truthy(v)).map(py_str).unwrap_or_default().trim().to_lowercase()
}

fn job_id(job: Option<&Value>) -> Value {
    let Some(j) = job.filter(|j| j.as_object().is_some_and(|m| !m.is_empty())) else { return Value::Null };
    j.get("job_id")
        .filter(|v| truthy(v))
        .or_else(|| j.get("id").filter(|v| truthy(v)))
        .map_or(Value::Null, |v| json!(py_str(v)))
}

struct Rows<'a> {
    manager: &'a AgentManager,
    contracts: &'a [ActionContract],
    by_action: BTreeMap<String, Value>,
}

impl Rows<'_> {
    fn contract(&self, name: &str) -> Option<&ActionContract> {
        self.contracts.iter().find(|c| c.name == name)
    }

    fn allowed(&self, name: &str) -> bool {
        self.contract(name).is_some_and(|c| self.manager.action_allowed(c))
    }

    fn requires_model(&self, name: &str) -> bool {
        self.contract(name).is_some_and(|c| c.requires_model)
    }

    fn set(&mut self, name: &str, category: &str, reason: &str, hint: Option<Value>) {
        let Some(c) = self.contract(name) else { return };
        let mut row = Map::new();
        row.insert("action".into(), json!(name));
        row.insert("label".into(), json!(c.label));
        row.insert("category".into(), json!(category));
        row.insert("reason".into(), json!(reason));
        row.insert("permission".into(), json!(c.permission));
        if let Some(h) = hint.filter(|h| h.as_object().is_some_and(|m| !m.is_empty())) {
            row.insert("payload_hint".into(), h);
        }
        self.by_action.insert(name.to_owned(), Value::Object(row));
    }
}



#[allow(clippy::too_many_lines)]
pub fn build_guidance(manager: &AgentManager) -> AgentResult<Value> {
    let state = manager.state()?;
    let model = state.get("model").filter(|v| !v.is_null());
    let problem = state.get("engineering_problem").filter(|v| !v.is_null());
    let intent = state.get("intent").filter(|v| !v.is_null());
    let raw_jobs = match state.get("optimization_jobs") {
        Some(Value::Object(m)) => m.get("jobs").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!([])),
        Some(v) if truthy(v) => v.clone(),
        _ => json!([]),
    };
    let jobs: Vec<Value> = raw_jobs
        .as_array()
        .map(|a| a.iter().filter(|j| j.is_object()).cloned().collect())
        .unwrap_or_default();
    let latest = |pred: &dyn Fn(&Value) -> bool| jobs.iter().rev().find(|j| pred(j)).cloned();
    let running = latest(&|j| RUNNING.contains(&status(j).as_str()));
    let paused = latest(&|j| status(j) == "paused");
    let intervening = latest(&|j| status(j) == "intervening");
    let attention_state = |j: &Value, s: &str| {
        status(j) == "attention"
            && j.get("numerical_attention")
                .and_then(Value::as_object)
                .is_some_and(|a| a.get("state") == Some(&json!(s)))
    };
    let attention = latest(&|j| attention_state(j, "open"));
    let attention_pending = latest(&|j| attention_state(j, "branch_pending"));
    let terminal = latest(&|j| TERMINAL.contains(&status(j).as_str()));
    let active = running
        .clone()
        .or_else(|| paused.clone())
        .or_else(|| intervening.clone())
        .or_else(|| attention.clone())
        .or_else(|| attention_pending.clone());

    let has_model = model.is_some();
    let has_problem = problem.is_some();
    let has_intent = intent.is_some();
    let c = contracts()?;
    let mut r = Rows { manager, contracts: c.actions(), by_action: BTreeMap::new() };
    for contract in c.actions() {
        let name = contract.name.as_str();
        if !manager.action_allowed(contract) {
            r.set(
                name,
                "Unavailable",
                &format!(
                    "The current {} autonomy policy does not permit this action.",
                    repr_str(manager.autonomy())
                ),
                None,
            );
        } else if contract.requires_model && !has_model {
            r.set(name, "Unavailable", "No authoritative implicit model is loaded.", None);
        } else {
            r.set(
                name,
                "Useful now",
                "The action is permitted and its native Implexity validator will decide whether the supplied payload is admissible.",
                None,
            );
        }
    }
    r.set(
        "inspect_state",
        if has_model { "Useful now" } else { "Required next" },
        "Inspect the authoritative model, physics and job state before choosing a consequential action.",
        None,
    );
    r.set(
        "inspect_couplings",
        "Useful now",
        "Read provider-declared coupling states, qualitative cost and truth impact before choosing exact or approximate initialization.",
        None,
    );
    if !has_intent && r.allowed("set_intent") {
        r.set("set_intent", "Required next", "No Engineering Intent is stored; goals and constraints must be explicit before an autonomous campaign is orchestrated.", None);
    } else if has_intent && r.allowed("set_intent") {
        r.set("set_intent", "Redundant now", "Engineering Intent is already present. Use this action only to revise the declared goals or constraints.", None);
    }
    let physics_plan = state.get("physics_plan").filter(|p| p.is_object());
    if r.allowed("inspect_physics_plan") {
        if !has_intent {
            r.set("inspect_physics_plan", "Blocked now", "Engineering Intent is required before Implexity can infer the necessary add-ins and coupling graph.", None);
        } else if let Some(plan) = physics_plan {
            match plan.get("status").filter(|v| truthy(v)).map(py_str).unwrap_or_default().as_str() {
                "ready" => r.set("inspect_physics_plan", "Useful now", "The automatically inferred physics graph is complete; inspect it only when provenance or an expert override is needed.", None),
                "needs_authoring" => r.set("inspect_physics_plan", "Required next", "The physics graph is resolvable but required boundary/model authoring is incomplete; inspect the missing authoring fields rather than assembling solvers manually.", None),
                _ => r.set("inspect_physics_plan", "Required next", "The requested responses cannot yet be closed with the installed add-ins; inspect the missing physics or incompatible coupling evidence.", None),
            }
        } else {
            r.set(
                "inspect_physics_plan",
                "Required next",
                "Derive the required installed physics add-ins automatically from Engineering Intent.",
                None,
            );
        }
    }
    if !has_model {
        if r.allowed("validate_model") {
            r.set("validate_model", "Useful now", "A candidate implicit document can be validated, but loading or creating the model remains a native host operation.", None);
        }
        for name in [
            "set_engineering_problem",
            "preflight_optimization",
            "start_optimization",
            "sensitivity",
            "evaluate_results",
        ] {
            if r.allowed(name) {
                r.set(name, "Unavailable", "No authoritative implicit model is loaded.", None);
            }
        }
    } else if !has_problem {
        if r.allowed("set_engineering_problem") {
            r.set("set_engineering_problem", "Required next", "The loaded model has no current engineering problem. Define physics, boundaries, objectives and constraints before preflight.", None);
        }
        for name in ["preflight_optimization", "start_optimization", "sensitivity", "evaluate_results"] {
            if r.allowed(name) {
                r.set(
                    name,
                    "Blocked now",
                    "A current engineering problem must be defined for the loaded model first.",
                    None,
                );
            }
        }
    } else if r.allowed("set_engineering_problem") {
        r.set("set_engineering_problem", "Useful now", "A problem is already stored; use this only when physics, boundaries, objectives or constraints must change.", None);
    }

    let gated = |r: &Rows<'_>, name: &str| r.allowed(name) && (!r.requires_model(name) || has_model);
    let names: Vec<String> = c.actions().iter().map(|a| a.name.clone()).collect();
    if let Some(att) = &attention {
        let jid = job_id(Some(att));
        let event =
            att.get("numerical_attention").filter(|e| e.is_object()).cloned().unwrap_or_else(|| json!({}));
        let token = event.get("event_token").cloned().unwrap_or(Value::Null);
        let available: Vec<String> = event
            .get("available_actions")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(py_str).collect())
            .unwrap_or_default();
        for name in &names {
            if ATTENTION_OBSERVATIONAL_ACTIONS.contains(&name.as_str())
                || name == "resolve_numerical_attention"
                || name == "branch_after_numerical_attention"
            {
                continue;
            }
            if r.by_action.contains_key(name) && gated(&r, name) {
                r.set(name, "Blocked now", "A bounded numerical deviation is awaiting an explicit technical decision; unrelated mutation or optimization cannot bypass that checkpoint.", None);
            }
        }
        if r.allowed("inspect_optimization_job") {
            r.set("inspect_optimization_job", "Required next", "Inspect the residual, required limit, retry budget, and authority state before choosing an action.", Some(json!({"job_id": jid, "view": "monitor"})));
        }
        if r.allowed("resolve_numerical_attention") {
            let options: Vec<&str> =
                ["retry_exact", "discard"].into_iter().filter(|n| available.iter().any(|a| a == n)).collect();
            r.set("resolve_numerical_attention", "Required next", "Retry with more bounded solver effort where available, or discard the run and restore the pre-run model.", Some(json!({"job_id": jid, "event_token": token, "action_options": options})));
        }
        if r.allowed("branch_after_numerical_attention") {
            let continuable = available.iter().any(|a| a == "continue_exploratory");
            let (category, reason) = if continuable {
                (
                    "Required next",
                    "Continue the unchanged problem in a distinct exploratory, non-authoritative branch; it cannot be accepted as canonical.",
                )
            } else {
                ("Blocked now", "This numerical deviation cannot be continued as exploratory.")
            };
            r.set(
                "branch_after_numerical_attention",
                category,
                reason,
                Some(json!({"job_id": jid, "event_token": token})),
            );
        }
        if r.allowed("optimization_operation") {
            r.set("optimization_operation", "Redundant now", "Use the numerical-attention actions bound to this event, rather than a generic job operation.", None);
        }
    } else if let Some(run) = &running {
        let jid = job_id(Some(run));
        for name in GEOMETRY_MUTATIONS {
            if gated(&r, name) {
                r.set(name, "Blocked now", "Geometry cannot be changed while direct-gradient optimization is running. Pause the job and enter Manual intervention first.", None);
            }
        }
        if r.allowed("optimization_operation") {
            r.set("optimization_operation", "Required next", "A direct-gradient job is running. Continue monitoring it, or pause it before any manual geometry intervention.", Some(json!({"job_id": jid, "op_options": ["pause", "stop"]})));
        }
        for name in ["start_optimization", "preflight_optimization", "branch_after_intervention"] {
            if r.allowed(name) {
                r.set(
                    name,
                    "Blocked now",
                    "A direct-gradient job is already running on the authoritative design state.",
                    None,
                );
            }
        }
        for name in ["resolve_numerical_attention", "branch_after_numerical_attention"] {
            if r.allowed(name) {
                r.set(name, "Blocked now", "A numerical-deviation branch is already running; monitor that child rather than reusing its parent event.", None);
            }
        }
    } else if let Some(pending) = &attention_pending {
        let jid = job_id(Some(pending));
        for name in &names {
            if ATTENTION_OBSERVATIONAL_ACTIONS.contains(&name.as_str()) {
                continue;
            }
            if r.by_action.contains_key(name) && gated(&r, name) {
                r.set(name, "Blocked now", "The exploratory branch transition is pending; its identity-bound parent event cannot be resolved again.", None);
            }
        }
        if r.allowed("inspect_optimization_job") {
            r.set("inspect_optimization_job", "Required next", "Inspect the pending branch transition until its child is published or the parent event safely reopens.", Some(json!({"job_id": jid, "view": "monitor"})));
        }
    } else if let Some(p) = &paused {
        let jid = job_id(Some(p));
        for name in GEOMETRY_MUTATIONS {
            if gated(&r, name) {
                r.set(name, "Blocked now", "The optimization is paused but Manual intervention has not been opened. Enter intervention before changing geometry.", None);
            }
        }
        if r.allowed("optimization_operation") {
            r.set("optimization_operation", "Required next", "Choose whether to resume, stop, accept/discard where admissible, or enter Manual intervention.", Some(json!({"job_id": jid, "op_options": ["intervene", "resume", "stop", "accept", "discard"]})));
        }
        if r.allowed("branch_after_intervention") {
            r.set(
                "branch_after_intervention",
                "Blocked now",
                "Open Manual intervention before creating a provenance-linked continuation branch.",
                None,
            );
        }
        if r.allowed("start_optimization") {
            r.set(
                "start_optimization",
                "Redundant now",
                "A paused optimization job already owns the current optimization lifecycle.",
                None,
            );
        }
    } else if let Some(i) = &intervening {
        let jid = job_id(Some(i));
        for name in GEOMETRY_MUTATIONS {
            if gated(&r, name) {
                r.set(name, "Useful now", "Manual intervention is open; edits act on the same authoritative implicit model and will invalidate the old state and adjoint.", None);
            }
        }
        if r.allowed("branch_after_intervention") {
            r.set("branch_after_intervention", "Required next", "After the intended edits are committed, re-preflight and continue through a provenance-linked optimization branch.", Some(json!({"job_id": jid})));
        }
        if r.allowed("optimization_operation") {
            r.set("optimization_operation", "Redundant now", "The parent job is already in Manual intervention; use branch_after_intervention after committing the edit.", None);
        }
        for name in ["preflight_optimization", "start_optimization"] {
            if r.allowed(name) {
                r.set(name, "Blocked now", "Complete the intervention through the linked branch operation rather than starting an unrelated lifecycle.", None);
            }
        }
    } else if has_model && has_problem {
        if r.allowed("preflight_optimization") {
            r.set("preflight_optimization", "Required next", "Preflight the current model/problem pair before starting or restarting direct-gradient optimization.", None);
        }
        if r.allowed("start_optimization") {
            r.set("start_optimization", "Blocked now", "Run current-model preflight first; the native start action will also fail closed if its declaration is stale or incomplete.", None);
        }
        if r.allowed("branch_after_intervention") {
            r.set(
                "branch_after_intervention",
                "Redundant now",
                "No optimization job currently has an open Manual-intervention checkpoint.",
                None,
            );
        }
        if r.allowed("optimization_operation") && terminal.is_none() {
            r.set(
                "optimization_operation",
                "Redundant now",
                "There is no active optimization job to operate.",
                None,
            );
        }
    }
    if let Some(t) = terminal.as_ref().filter(|_| active.is_none()) {
        let jid = job_id(Some(t));
        if r.allowed("evaluate_results") {
            r.set(
                "evaluate_results",
                "Useful now",
                "A terminal optimization result is available for engineering evaluation.",
                None,
            );
        }
        if r.allowed("optimization_operation") {
            r.set("optimization_operation", "Useful now", "A terminal job may have an admissible accept or discard transition; the native job manager will decide.", Some(json!({"job_id": jid, "op_options": ["accept", "discard"]})));
        }
    }

    let mut categories: Vec<(&str, Vec<Value>)> = CATEGORY_ORDER.iter().map(|c| (*c, Vec::new())).collect();
    for name in &names {
        let row = r.by_action.get(name).cloned().unwrap_or(Value::Null);
        let category = row.get("category").and_then(Value::as_str).unwrap_or_default().to_owned();
        if let Some((_, list)) = categories.iter_mut().find(|(c, _)| *c == category) {
            list.push(row);
        }
    }
    let required: Vec<Value> = categories[0].1.iter().filter_map(|row| row.get("action").cloned()).collect();
    let recommended = if required.is_empty() { vec![json!("inspect_state")] } else { required };
    let count = |k: &str| state.get(k).and_then(Value::as_object).map_or(0, Map::len);
    let plan_status = physics_plan.and_then(|p| p.get("status").cloned()).unwrap_or(Value::Null);
    let categories: Map<String, Value> =
        categories.into_iter().map(|(k, v)| (k.to_owned(), Value::Array(v))).collect();
    Ok(json!({
        "schema": SCHEMA,
        "categories": categories,
        "recommended_sequence": recommended,
        "current_context": {
            "model_loaded": has_model,
            "engineering_problem_defined": has_problem,
            "engineering_intent_defined": has_intent,
            "active_job_id": job_id(active.as_ref()),
            "active_job_status": active.as_ref().map_or(Value::Null, |a| json!(status(a))),
            "available_physics_count": count("available_physics"),
            "available_physics_addin_count": count("available_physics_addins"),
            "physics_plan_status": plan_status,
        },
        "principle": "Guidance is advisory. Native Implexity validators, physics preflight and the direct-gradient job runtime remain authoritative.",
    }))
}
