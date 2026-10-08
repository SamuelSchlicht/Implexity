// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use implexity_core::json::{DumpOptions, dumps};
use implexity_core::py_repr::repr_str;
use serde_json::{Map, Value, json};

use crate::contracts::{
    ActionContract, EXTENSION, POLICY_SCHEMA, SCHEMA, contracts,
    normalise_computation_effort_request, normalise_intent,
};
use crate::error::{AgentError, AgentResult};
use crate::host::{AgentHost, HostOp};
use crate::managed::ManagedState;
use crate::pyval::{is_int, is_job_id, py_str, text_or, truthy, type_name};
use crate::views::{
    normalise_render_section_request, optimization_job_monitor_view,
    optimization_preflight_monitor_view,
};

pub const MANAGED_START_ACTIONS: [(&str, &str); 3] = [
    ("start_managed_preflight", "preflight"),
    ("start_managed_evaluation", "evaluate"),
    ("start_managed_sensitivity", "sensitivity"),
];

const OUTSIDE_BROAD_LOCK_ACTIONS: [&str; 10] = [
    "check_gradients",
    "evaluate_results",
    "sensitivity",
    "preflight_optimization",
    "author_application_case",
    "start_managed_preflight",
    "start_managed_evaluation",
    "start_managed_sensitivity",
    "inspect_managed_evaluation",
    "cancel_managed_evaluation",
];

const MANAGED_REQUEST_RESERVED_FIELDS: [&str; 24] = [
    "child_command",
    "command",
    "control",
    "control_token",
    "cwd",
    "env",
    "environment",
    "kill_grace_s",
    "managed_root",
    "operation_id",
    "pid",
    "poll_interval_s",
    "private_directory",
    "private_profile",
    "process_id",
    "runtime_profile",
    "signal",
    "signal_policy",
    "stderr",
    "stdout",
    "telemetry_interval_s",
    "term_grace_s",
    "timeout_s",
    "token",
];

const OPTIMIZATION_JOB_VIEWS: [&str; 2] = ["full", "monitor"];

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn key_error(key: &str) -> AgentError {
    AgentError::KeyError(repr_str(key))
}

pub(crate) fn field<'a>(p: &'a Map<String, Value>, key: &str) -> AgentResult<&'a Value> {
    p.get(key).ok_or_else(|| key_error(key))
}

fn view_of(p: &Map<String, Value>) -> String {
    p.get("view").map_or_else(|| "full".to_owned(), py_str)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

pub struct AgentManager {
    pub(crate) host: Arc<dyn AgentHost>,
    validation: Mutex<()>,
    action: Mutex<()>,
    audit: Mutex<()>,
    pub(crate) managed: ManagedState,
    pub(crate) state_dir: PathBuf,
    intent_path: PathBuf,
    audit_path: PathBuf,
    autonomy: String,
    restore_token: Option<String>,
}

impl std::fmt::Debug for AgentManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentManager")
            .field("autonomy", &self.autonomy)
            .finish_non_exhaustive()
    }
}

impl AgentManager {


    pub fn new(host: Arc<dyn AgentHost>) -> AgentResult<Self> {
        let autonomy =
            std::env::var("IMPLEXITY_AGENT_AUTONOMY").unwrap_or_else(|_| "operator".into());
        Self::with_policy(
            host,
            &autonomy,
            std::env::var("IMPLEXITY_AGENT_RESTORE_TOKEN").ok(),
        )
    }



    pub fn with_policy(
        host: Arc<dyn AgentHost>,
        autonomy: &str,
        restore_token: Option<String>,
    ) -> AgentResult<Self> {
        let c = contracts()?;
        let state_dir = host.state_dir();
        let mut autonomy = autonomy.trim().to_owned();
        if c.preset(&autonomy).is_none() {
            autonomy = "operator".into();
        }
        Ok(Self {
            host,
            validation: Mutex::new(()),
            action: Mutex::new(()),
            audit: Mutex::new(()),
            managed: ManagedState::default(),
            intent_path: state_dir.join("engineering_agent_intent.json"),
            audit_path: state_dir.join("engineering_agent_actions.jsonl"),
            state_dir,
            autonomy,
            restore_token,
        })
    }

    #[must_use]
    pub fn autonomy(&self) -> &str {
        &self.autonomy
    }

    #[must_use]
    pub fn has_restore_token(&self) -> bool {
        self.restore_token.as_ref().is_some_and(|t| !t.is_empty())
    }

    #[must_use]
    pub fn host(&self) -> &Arc<dyn AgentHost> {
        &self.host
    }



    pub fn policy(&self) -> AgentResult<Value> {
        let allowed = contracts()?
            .preset(&self.autonomy)
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        Ok(json!({"schema": POLICY_SCHEMA, "autonomy": self.autonomy,
                  "allowed_permission_classes": allowed,
                  "restore_requires_out_of_band_approval": true,
                  "shell_access": false, "filesystem_access": false,
                  "plugin_installation": false, "arbitrary_http": false}))
    }

    #[must_use]
    pub fn permission_allowed(&self, permission: &str) -> bool {
        contracts()
            .ok()
            .and_then(|c| c.preset(&self.autonomy))
            .is_some_and(|classes| classes.iter().any(|x| x == permission))
    }

    #[must_use]
    pub fn action_allowed(&self, contract: &ActionContract) -> bool {
        if contract.permission == "restore" {
            self.has_restore_token()
        } else {
            self.permission_allowed(&contract.permission)
        }
    }



    pub fn capabilities(&self) -> AgentResult<Value> {
        let actions: Vec<Value> = contracts()?
            .actions()
            .iter()
            .map(|c| {
                json!({"name": c.name, "label": c.label, "permission": c.permission,
                       "mutates": c.mutates, "requires_model": c.requires_model,
                       "description": c.description, "input_schema": c.input_schema,
                       "allowed": self.permission_allowed(&c.permission)})
            })
            .collect();
        let mut out = json!({"schema": SCHEMA, "actions": actions, "policy": self.policy()?,
                  "principle": "The agent operates the same authoritative model, physics providers and direct-gradient runtime as the native GUI."});
        if self.extension_active() {
            let extension: Vec<Value> = contracts()?
                .extension_actions()
                .iter()
                .map(|c| {
                    json!({"name": c.name, "label": c.label, "permission": c.permission,
                           "mutates": c.mutates, "requires_model": c.requires_model,
                           "description": c.description, "input_schema": c.input_schema,
                           "allowed": self.permission_allowed(&c.permission), "extension": EXTENSION})
                })
                .collect();
            out["rust_extension"] = json!({"schema": EXTENSION, "actions": extension});
        }
        Ok(out)
    }

    #[must_use]
    pub fn extension_active(&self) -> bool {
        cfg!(feature = "dynamic-results") && self.host.dynamic_results_available()
    }

    pub(crate) fn served_extension_actions(&self) -> AgentResult<&'static [ActionContract]> {
        Ok(if self.extension_active() {
            contracts()?.extension_actions()
        } else {
            &[]
        })
    }



    pub fn tool_manifest(&self) -> AgentResult<Value> {
        let c = contracts()?;
        let extension = self.served_extension_actions()?;
        let tools: Vec<Value> = c
            .actions()
            .iter()
            .chain(extension)
            .map(|c| {
                let description = if c.description.is_empty() { &c.label } else { &c.description };
                json!({"name": format!("implexity_{}", c.name), "label": c.label, "permission": c.permission,
                       "mutates": c.mutates, "requires_model": c.requires_model,
                       "description": description, "input_schema": c.input_schema, "action": c.name,
                       "allowed": (c.permission == "restore" && self.has_restore_token())
                           || self.permission_allowed(&c.permission),
                       "invoke": {"method": "POST", "path": "/v1/agent/action",
                                  "envelope": {"action": c.name, "payload": "<tool arguments>"}}})
            })
            .zip(c.actions().iter().map(|_| false).chain(extension.iter().map(|_| true)))
            .map(|(mut t, ext)| {
                if ext {
                    t["extension"] = json!(EXTENSION);
                }
                t
            })
            .collect();
        Ok(
            json!({"schema": "implexity-agent-tool-manifest/1", "tools": tools,
                  "manual": {"method": "GET", "path": "/v1/agent/manual"},
                  "context": {"method": "GET", "path": "/v1/agent/context"},
                  "guidance": {"method": "GET", "path": "/v1/agent/guidance"},
                  "execution_model": "one consequential start or mutation at a time; managed inspection/cancellation remains responsive; re-inspect state after consequential mutations",
                  "arbitrary_http_not_required": true}),
        )
    }

    fn authorize(
        &self,
        contract: &ActionContract,
        payload: &Map<String, Value>,
    ) -> AgentResult<()> {
        if contract.permission == "restore" {
            let token = text_or(payload.get("approval_token"), "");
            if self
                .restore_token
                .as_deref()
                .is_none_or(|t| t.is_empty() || token != t)
            {
                return Err(AgentError::permission(
                    "restoring an engineering branch requires out-of-band user approval",
                ));
            }
            return Ok(());
        }
        if !self.permission_allowed(&contract.permission) {
            return Err(AgentError::permission(format!(
                "agent autonomy {} does not permit {}",
                repr_str(&self.autonomy),
                contract.label.to_lowercase()
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn safe_summary(payload: &Map<String, Value>) -> Value {
        let mut out = Map::new();
        for (k, v) in payload {
            let entry = if k == "document" || k == "model" {
                json!({"present": true, "type": type_name(v)})
            } else {
                match v {
                    Value::Array(a) => json!({"items": a.len()}),
                    Value::Object(m) => {
                        let mut keys: Vec<&String> = m.keys().collect();
                        keys.sort();
                        keys.truncate(24);
                        json!({"keys": keys})
                    }
                    scalar => scalar.clone(),
                }
            };
            out.insert(k.clone(), entry);
        }
        Value::Object(out)
    }

    #[must_use]
    pub fn model_identity(&self) -> Value {
        let status = self.host.model().and_then(|m| m.status_value().ok());
        let get = |k: &str| {
            status
                .as_ref()
                .and_then(|s| s.get(k))
                .cloned()
                .unwrap_or(Value::Null)
        };
        json!({"structure_id": get("structure_id"), "content_id": get("content_id")})
    }

    fn audit_row(
        &self,
        action: &str,
        payload: &Map<String, Value>,
        result: Option<&Value>,
        error: Option<&str>,
    ) {
        let anonymous = matches!(
            action,
            "resolve_numerical_attention" | "branch_after_numerical_attention"
        );
        let mut row = Map::new();
        row.insert("schema".into(), json!("implexity-agent-action-audit/1"));
        row.insert("time".into(), json!(now()));
        row.insert("action".into(), json!(action));
        row.insert("ok".into(), json!(error.is_none()));
        row.insert("model".into(), self.model_identity());
        row.insert("payload_summary".into(), Self::safe_summary(payload));
        if !anonymous {
            row.insert("actor".into(), json!("Engineering Agent"));
        }
        if let Some(Value::Object(r)) = result {
            for key in ["job_id", "solve_id", "status", "kind"] {
                if let Some(v) = r.get(key) {
                    row.insert(key.into(), v.clone());
                }
            }
        }
        if let Some(e) = error.filter(|e| !e.is_empty()) {
            row.insert("error".into(), json!(e));
        }
        let line = dumps(&Value::Object(row), &DumpOptions::canonical()) + "\n";
        {
            let _g = lock(&self.audit);
            if let Some(parent) = self.audit_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut fh) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.audit_path)
            {
                let _ = fh.write_all(line.as_bytes());
            }
        }
        if anonymous {
            return;
        }
        let label = contracts()
            .ok()
            .and_then(|c| c.get(action))
            .map(|c| c.label.clone())
            .unwrap_or_default();
        let details = json!({"action": action, "ok": error.is_none(), "error": error});
        let _ = self.host.call(HostOp::HistoryAppend(label, details));
    }



    pub fn get_intent(&self) -> AgentResult<Option<Value>> {
        if !self.intent_path.is_file() {
            return Ok(None);
        }
        let raw =
            std::fs::read(&self.intent_path).map_err(|e| AgentError::failed(e.to_string()))?;
        serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|e| AgentError::contract(e.to_string()))
    }



    pub fn set_intent(&self, raw: &Value) -> AgentResult<Value> {
        let intent = normalise_intent(raw)?;
        let tmp = self.intent_path.with_extension("tmp");
        if let Some(parent) = self.intent_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AgentError::failed(e.to_string()))?;
        }
        std::fs::write(&tmp, dumps(&intent, &DumpOptions::indented(2).sorted(true)))
            .map_err(|e| AgentError::failed(e.to_string()))?;
        std::fs::rename(&tmp, &self.intent_path).map_err(|e| AgentError::failed(e.to_string()))?;
        Ok(intent)
    }



    pub fn physics_plan(&self, raw: Option<&Value>) -> AgentResult<Value> {
        let base = match raw.filter(|v| truthy(v)) {
            Some(v) => v.clone(),
            None => self.get_intent()?.unwrap_or_else(|| json!({})),
        };
        let Some(intent_map) = base.as_object().filter(|m| !m.is_empty()) else {
            return Ok(Value::Null);
        };
        let _ = self.host.packages_status();
        let registries = implexity_core::registries::global();
        registries.providers.catalogue(
            &registries.addins,
            Some(&implexity_runtime::cae_runtime::catalogue_traits),
        )?;
        let mut intent = intent_map.clone();
        if !intent.contains_key("authoring") {
            let mut authoring = Map::new();
            if let Ok(st) = self.state_without_plan() {
                if let Some(p) = st.get("engineering_problem").filter(|v| !v.is_null()) {
                    authoring.insert("engineering_problem".into(), p.clone());
                }
                if let Some(m) = st.get("model").filter(|v| !v.is_null()) {
                    authoring.insert("model".into(), m.clone());
                }
            }
            if !authoring.is_empty() {
                intent.insert("authoring".into(), Value::Object(authoring));
            }
        }
        let planner_intent: Map<String, Value> = intent
            .into_iter()
            .filter(|(k, _)| !matches!(k.as_str(), "schema" | "compatibility" | "autonomy"))
            .collect();
        let planner_value = Value::Object(planner_intent);
        let parsed =
            implexity_core::orchestration::EngineeringIntent::from_mapping(&planner_value)?;
        let planner = implexity_core::orchestration::OrchestrationPlanner;
        let snap = registries.addins.snapshot();
        let plan = planner
            .plan_snapshot(&parsed, &snap, &registries.sufficiency)?
            .as_dict();
        let Value::Object(mut plan) = plan else {
            return Err(AgentError::failed("planner returned a non-object"));
        };
        plan.insert(
            "fidelity_ladder".into(),
            Value::Object(planner.plan_fidelity_ladder(&parsed, &snap, &registries.sufficiency)?),
        );
        let result = implexity_runtime::execution_readiness::inspect_execution(
            &planner_value,
            &plan,
            None,
            &registries.addins,
        )?;
        let mut result = Value::Object(result);
        identify_actor(&mut result);
        Ok(result)
    }



    pub fn state_without_plan(&self) -> AgentResult<Value> {
        let model = self.host.model().and_then(|m| m.status_value().ok());
        let problem = if model.is_some() {
            self.host
                .call(HostOp::CurrentProblem)
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let jobs = self.host.call(HostOp::JobsList)?;
        let history = self.host.call(HostOp::HistoryList(40))?;
        let registries = implexity_core::registries::global();
        let (physics, addins, status) = match registries.providers.catalogue(
            &registries.addins,
            Some(&implexity_runtime::cae_runtime::catalogue_traits),
        ) {
            Ok(p) => (
                Value::Object(p.into_iter().collect()),
                Value::Object(registries.addins.catalogue()),
                implexity_core::capability_status::report(&registries.addins),
            ),
            Err(_) => (json!({}), json!({}), json!({})),
        };
        Ok(
            json!({"schema": SCHEMA, "model": model, "engineering_problem": problem,
                  "optimization_jobs": jobs, "history": history,
                  "available_physics": physics, "available_physics_addins": addins,
                  "physics_component_status": status,
                  "intent": self.get_intent()?, "policy": self.policy()?}),
        )
    }



    pub fn state(&self) -> AgentResult<Value> {
        let mut out = self.state_without_plan()?;
        let intent = out.get("intent").cloned();
        let plan = match self.physics_plan(intent.as_ref().filter(|v| !v.is_null())) {
            Ok(p) => p,
            Err(e) => json!({"status": "blocked", "blocked_reasons": [e.message()]}),
        };
        if let Some(m) = out.as_object_mut() {
            m.insert("physics_plan".into(), plan);
            m.insert(
                "agent_help".into(),
                json!({"manual": "/v1/agent/manual", "context": "/v1/agent/context",
                       "guidance": "/v1/agent/guidance", "tools": "/v1/agent/tools"}),
            );
        }
        Ok(out)
    }



    #[allow(clippy::too_many_lines)]
    pub fn validate_action(&self, raw: &Value) -> AgentResult<Value> {
        let Some(envelope) = raw.as_object() else {
            return Err(AgentError::contract("agent action must be an object"));
        };
        let action = text_or(envelope.get("action"), "").trim().to_owned();
        let c = contracts()?;
        let Some(contract) = c
            .get(&action)
            .filter(|_| !c.is_extension(&action) || self.extension_active())
        else {
            return Err(AgentError::contract(format!(
                "unknown engineering action {}",
                repr_str(&action)
            )));
        };
        let empty = Value::Object(Map::new());
        let Value::Object(payload) = envelope.get("payload").unwrap_or(&empty) else {
            return Err(AgentError::contract("action payload must be an object"));
        };
        self.authorize(contract, payload)?;
        let p = payload;
        let a = action.as_str();
        let keys = |allowed: &[&str]| p.keys().any(|k| !allowed.contains(&k.as_str()));
        if matches!(
            a,
            "inspect_optimization_setup" | "inspect_guided_setup" | "inspect_runtime_environment"
        ) && !p.is_empty()
        {
            return Err(AgentError::contract(
                "This inspection accepts no arguments.",
            ));
        }
        if a == "revise_engineering_problem" {
            self.host
                .call(HostOp::ReviseProblemValidate(Value::Object(p.clone())))?;
        }
        if matches!(a, "save_optimization_setup" | "review_guided_setup") {
            self.host
                .call(HostOp::OptimizationSetupValidate(Value::Object(p.clone())))?;
        }
        if a == "apply_guided_setup" {
            self.host
                .call(HostOp::GuidedSetupValidateApply(Value::Object(p.clone())))?;
        }
        if a == "steer_optimization" {
            if p.len() != 2
                || !p.contains_key("job_id")
                || !p.get("request").is_some_and(Value::is_object)
            {
                return Err(AgentError::contract(
                    "optimization steering requires exactly job_id and a request object",
                ));
            }
            if !p.get("job_id").is_some_and(is_job_id) {
                return Err(AgentError::contract(
                    "optimization steering requires a 12-character lowercase hexadecimal job_id",
                ));
            }
        }
        if a == "save_branch_point" && p.get("details").is_some_and(|d| !d.is_object()) {
            return Err(AgentError::contract(
                "branch-point details must be an object",
            ));
        }
        if a == "initialize_lattice_seed"
            && (keys(&["node", "specification", "expected_content_id"])
                || !p.get("node").is_some_and(Value::is_string)
                || !p.get("specification").is_some_and(Value::is_object))
        {
            return Err(AgentError::contract(
                "lattice seed calibration requires node, specification and optional expected_content_id",
            ));
        }
        if a == "inspect_application_cases" && !p.is_empty() {
            return Err(AgentError::contract(
                "This inspection accepts no arguments.",
            ));
        }
        if a == "author_application_case" {
            if keys(&["case", "execute", "stop_before", "execution_view"])
                || !p.contains_key("case")
            {
                return Err(AgentError::contract(
                    "application case authoring requires case and optional execute, stop_before, execution_view",
                ));
            }
            let schema_ok = p
                .get("case")
                .and_then(Value::as_object)
                .and_then(|m| m.get("schema"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            if !schema_ok {
                return Err(AgentError::contract(
                    "the application case must be an object with a versioned schema",
                ));
            }
            if p.get("execute").is_some_and(|e| !e.is_boolean()) {
                return Err(AgentError::contract("execute must be a boolean"));
            }
            if let Some(s) = p.get("stop_before")
                && s.as_str().is_none_or(|n| c.get(n).is_none())
            {
                return Err(AgentError::contract(
                    "stop_before must name a public action",
                ));
            }
            let view = p
                .get("execution_view")
                .cloned()
                .unwrap_or_else(|| json!("monitor"));
            if !view
                .as_str()
                .is_some_and(|v| OPTIMIZATION_JOB_VIEWS.contains(&v))
            {
                return Err(AgentError::contract(
                    "execution_view must be full or monitor",
                ));
            }
        }
        if a == "evaluate_authored_case" {
            if keys(&["problem", "design", "view"])
                || !p.get("problem").is_some_and(Value::is_object)
                || !p.get("design").is_some_and(Value::is_object)
            {
                return Err(AgentError::contract(
                    "authored-case evaluation requires explicit problem/design objects and optional view",
                ));
            }
            let view = p.get("view").cloned().unwrap_or_else(|| json!("full"));
            if !view
                .as_str()
                .is_some_and(|v| v == "full" || v == "artifact")
            {
                return Err(AgentError::contract(
                    "authored-case evaluation view must be full or artifact",
                ));
            }
        }
        if a == "set_parameters" && !p.get("values").is_some_and(Value::is_object) {
            return Err(AgentError::contract(
                "set parameters requires a values object",
            ));
        }
        if a == "use_optimization_result" {
            self.host
                .call(HostOp::ValidateWorkingResult(Value::Object(p.clone())))?;
        }
        if a == "optimization_operation" {
            if text_or(p.get("job_id"), "").is_empty() {
                return Err(AgentError::contract(
                    "optimization operation requires job_id",
                ));
            }
            let op = text_or(p.get("op"), "");
            if !["pause", "intervene", "resume", "stop", "accept", "discard"].contains(&op.as_str())
            {
                return Err(AgentError::contract("unsupported optimization operation"));
            }
        }
        if matches!(
            a,
            "preflight_optimization"
                | "start_optimization"
                | "optimization_operation"
                | "branch_after_intervention"
                | "resolve_numerical_attention"
                | "branch_after_numerical_attention"
        ) {
            let view = p.get("view").cloned().unwrap_or_else(|| json!("full"));
            if !view
                .as_str()
                .is_some_and(|v| OPTIMIZATION_JOB_VIEWS.contains(&v))
            {
                return Err(AgentError::contract(format!(
                    "{} view must be full or monitor",
                    a.replace('_', "-")
                )));
            }
        }
        if a == "inspect_optimization_job" {
            if keys(&["job_id", "view"]) || !p.contains_key("job_id") {
                return Err(AgentError::contract(
                    "optimization-job inspection accepts only job_id and optional view",
                ));
            }
            if !p.get("job_id").is_some_and(is_job_id) {
                return Err(AgentError::contract(
                    "optimization-job inspection requires a 12-character lowercase hexadecimal job_id",
                ));
            }
            let view = p.get("view").cloned().unwrap_or_else(|| json!("full"));
            if !view
                .as_str()
                .is_some_and(|v| OPTIMIZATION_JOB_VIEWS.contains(&v))
            {
                return Err(AgentError::contract(
                    "optimization-job inspection view must be full or monitor",
                ));
            }
        }
        if a == "read_optimization_epoch_field" {
            if p.get("mode").is_some_and(|v| !v.as_str().is_some_and(|s| ["payload", "summary"].contains(&s))) { return Err(AgentError::contract("epoch field mode must be payload or summary")); }

            if keys(&[
                "job_id",
                "epoch",
                "field",
                "operating_point",
                "maximum_bytes",
                "mode",
            ]) {
                return Err(AgentError::contract("unknown epoch field read arguments"));
            }
            if !p.get("job_id").is_some_and(is_job_id) {
                return Err(AgentError::contract(
                    "epoch field read requires a canonical job id",
                ));
            }
            if !p
                .get("epoch")
                .is_some_and(|e| is_int(e) && e.as_i64().is_some_and(|x| x >= 0))
            {
                return Err(AgentError::contract(
                    "epoch field read requires a nonnegative epoch",
                ));
            }
            if !p
                .get("field")
                .and_then(Value::as_str)
                .is_some_and(|f| (1..=256).contains(&f.chars().count()))
            {
                return Err(AgentError::contract(
                    "epoch field read requires a bounded field name",
                ));
            }
            for (key, lo, hi, default) in [
                ("operating_point", 0, 1_000_000, 0),
                ("maximum_bytes", 1, 16_777_216, 1_048_576),
            ] {
                let v = p.get(key).cloned().unwrap_or_else(|| json!(default));
                if !(is_int(&v) && v.as_i64().is_some_and(|x| (lo..=hi).contains(&x))) {
                    return Err(AgentError::contract(format!(
                        "invalid epoch field read {key}"
                    )));
                }
            }
        }
        if a == "export_optimization_epoch" {
            if p.len() != 2 || !p.contains_key("job_id") || !p.contains_key("epoch") {
                return Err(AgentError::contract(
                    "optimization epoch export requires exactly job_id and epoch",
                ));
            }
            if !p.get("job_id").is_some_and(is_job_id) {
                return Err(AgentError::contract(
                    "optimization epoch export requires a 12-character lowercase hexadecimal job_id",
                ));
            }
            if !p
                .get("epoch")
                .is_some_and(|e| is_int(e) && e.as_i64().is_some_and(|x| x >= 0))
            {
                return Err(AgentError::contract(
                    "optimization epoch export requires a nonnegative integer epoch",
                ));
            }
        }
        if a == "render_section" {
            normalise_render_section_request(&Value::Object(p.clone()))?;
        }
        if a == "render_mesh_scene" { crate::public_scene::validate(p)?; }
        if crate::public_geometry::handles(a) {
            crate::public_geometry::validate(a, p)?;
        }
        if a == "render_3d" {
            implexity_render::render3d::normalise_request(&Value::Object(p.clone()))
                .map_err(|e| AgentError::contract(e.to_string()))?;
        }
        if a == "export_stl" {
            implexity_mesh::stl_export::normalise_request(&Value::Object(p.clone()))
                .map_err(|e| AgentError::contract(e.to_string()))?;
        }
        if a == "inspect_renderables" {
            if keys(&["job_id"]) {
                return Err(AgentError::contract("render discovery accepts only job_id"));
            }
            if p.get("job_id").is_some_and(|j| !is_job_id(j)) {
                return Err(AgentError::contract(
                    "render discovery job_id must be 12 lowercase hex characters",
                ));
            }
        }
        if a == "render_optimization_history" {
            validate_history_render(p)?;
        }
        if matches!(
            a,
            "inspect_couplings" | "inspect_boundary_controls" | "inspect_workspace_capabilities"
        ) {
            let subject = match a {
                "inspect_couplings" => "coupling",
                "inspect_boundary_controls" => "boundary-control",
                _ => "workspace-capability",
            };
            if keys(&["provider", "problem", "use_current_problem"]) {
                return Err(AgentError::contract(format!(
                    "{subject} inspection accepts only provider, problem, and use_current_problem"
                )));
            }
            if let Some(provider) = p.get("provider").filter(|v| !v.is_null())
                && !provider.as_str().is_some_and(|s| {
                    !s.trim().is_empty() && s == s.trim() && s.chars().count() <= 160
                })
            {
                return Err(AgentError::contract(format!(
                    "{subject} inspection provider must be canonical nonempty text"
                )));
            }
            if p.get("problem").is_some_and(|v| !v.is_object()) {
                return Err(AgentError::contract(format!(
                    "{subject} inspection problem must be an object"
                )));
            }
            if p.get("use_current_problem")
                .is_some_and(|v| !v.is_boolean())
            {
                return Err(AgentError::contract(format!(
                    "{subject} inspection use_current_problem must be boolean"
                )));
            }
            if p.contains_key("problem") && p.get("use_current_problem") == Some(&Value::Bool(true))
            {
                return Err(AgentError::contract(
                    "provided problem and use_current_problem=true are mutually exclusive",
                ));
            }
        }
        if a == "set_engineering_problem" {
            let schema = implexity_runtime::provider_problem_document::SCHEMA;
            let provider_form = p.get("schema").and_then(Value::as_str) == Some(schema)
                || p.contains_key("provider");
            if provider_form {
                if keys(&[
                    "schema",
                    "provider",
                    "problem",
                    "provenance",
                    "expected_model",
                ]) {
                    return Err(AgentError::contract(
                        "provider-owned engineering problem has unknown envelope fields",
                    ));
                }
                if p.get("schema").is_some_and(|s| s.as_str() != Some(schema)) {
                    return Err(AgentError::contract(format!(
                        "provider-owned engineering problem schema must be {}",
                        repr_str(schema)
                    )));
                }
                if !p.get("provider").and_then(Value::as_str).is_some_and(|s| {
                    !s.trim().is_empty() && s == s.trim() && s.chars().count() <= 160
                }) {
                    return Err(AgentError::contract(
                        "provider-owned engineering problem requires a canonical provider id",
                    ));
                }
                if !p.get("problem").is_some_and(Value::is_object) {
                    return Err(AgentError::contract(
                        "provider-owned engineering problem requires an opaque problem object",
                    ));
                }
                if p.get("provenance").is_some_and(|v| !v.is_object()) {
                    return Err(AgentError::contract(
                        "provider-owned engineering problem provenance must be an object",
                    ));
                }
            }
        }
        if MANAGED_START_ACTIONS.iter().any(|(n, _)| *n == a) {
            let Some(request) = p
                .get("request")
                .and_then(Value::as_object)
                .filter(|_| p.len() == 1)
            else {
                return Err(AgentError::contract(
                    "managed evaluation start requires exactly one request object",
                ));
            };
            let mut reserved: Vec<&str> = MANAGED_REQUEST_RESERVED_FIELDS
                .iter()
                .copied()
                .filter(|k| request.contains_key(*k))
                .collect();
            reserved.sort_unstable();
            if !reserved.is_empty() {
                return Err(AgentError::contract(format!(
                    "managed evaluation request contains server-owned fields {}",
                    implexity_core::pyobj::list_repr(&reserved)
                )));
            }
            if let Some(effort) = request.get("computation_effort") {
                normalise_computation_effort_request(Some(effort))?;
            }
            if a == "start_managed_sensitivity" && request.contains_key("sensitivity_responses") {
                check_sensitivity_batch(request)?;
            }
        }
        if matches!(
            a,
            "inspect_managed_evaluation" | "cancel_managed_evaluation"
        ) {
            if p.len() != 1 || !p.contains_key("operation_id") {
                return Err(AgentError::contract(
                    "managed evaluation operation requires exactly operation_id",
                ));
            }
            crate::managed::operation_id(p.get("operation_id"))?;
        }
        if a == "sensitivity" && p.contains_key("sensitivity_responses") {
            check_sensitivity_batch(p)?;
        }
        if matches!(
            a,
            "evaluate_results" | "sensitivity" | "preflight_optimization" | "start_optimization"
        ) && let Some(effort) = p.get("computation_effort")
        {
            normalise_computation_effort_request(Some(effort))?;
        }
        if a == "branch_after_intervention" && text_or(p.get("job_id"), "").is_empty() {
            return Err(AgentError::contract("branching requires job_id"));
        }
        if a == "resolve_numerical_attention" {
            if keys(&["job_id", "event_token", "action", "view"])
                || !["job_id", "event_token", "action"]
                    .iter()
                    .all(|k| p.contains_key(*k))
            {
                return Err(AgentError::contract(
                    "numerical attention resolution accepts only job_id, event_token, action, and optional view",
                ));
            }
            if !p
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|x| x == "retry_exact" || x == "discard")
            {
                return Err(AgentError::contract(
                    "numerical attention action must be retry_exact or discard",
                ));
            }
            if !p.get("job_id").is_some_and(is_job_id)
                || p.get("event_token")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(AgentError::contract(
                    "numerical attention resolution requires a 12-character lowercase hexadecimal job_id and nonempty event_token",
                ));
            }
        }
        if a == "branch_after_numerical_attention" {
            if keys(&["job_id", "event_token", "view"])
                || !p.contains_key("job_id")
                || !p.contains_key("event_token")
            {
                return Err(AgentError::contract(
                    "numerical attention branch accepts job_id, event_token, and only optional view",
                ));
            }
            if !p.get("job_id").is_some_and(is_job_id)
                || p.get("event_token")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(AgentError::contract(
                    "numerical attention branch requires a 12-character lowercase hexadecimal job_id and nonempty event_token",
                ));
            }
        }
        if a == "restore_branch_point" && text_or(p.get("entry_id"), "").is_empty() {
            return Err(AgentError::contract("restoring a branch requires entry_id"));
        }
        #[cfg(feature = "dynamic-results")]
        if c.is_extension(a) {
            crate::dynamic::validate(a, p)?;
        }
        Ok(json!({"ok": true, "action": action, "contract": contract.as_dict()}))
    }



    pub fn validate_plan(&self, raw: &Value) -> AgentResult<Value> {
        let Some(actions) = raw
            .get("actions")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
        else {
            return Err(AgentError::contract(
                "agent plan requires a non-empty actions array",
            ));
        };
        let mut checked = Vec::new();
        let mut problems = Vec::new();
        for (i, a) in actions.iter().enumerate() {
            match self.validate_action(a) {
                Ok(v) => checked.push(v),
                Err(e) => problems.push(json!({"index": i, "problem": e.message()})),
            }
        }
        Ok(
            json!({"schema": "implexity-agent-plan-validation/1", "valid": problems.is_empty(),
                  "actions": checked, "problems": problems,
                  "note": "Plan validation does not execute actions; autonomous agents execute one validated action at a time and re-inspect state between consequential steps."}),
        )
    }



    pub fn execute(&self, raw: &Value) -> AgentResult<Value> {
        let (action, payload) = {
            let _g = lock(&self.validation);
            let checked = self.validate_action(raw).map_err(|error| {
                if raw
                    .get("action")
                    .and_then(Value::as_str)
                    .is_some_and(|a| MANAGED_START_ACTIONS.iter().any(|(name, _)| *name == a))
                {
                    error.managed_not_started()
                } else {
                    error
                }
            })?;
            let action = checked["action"].as_str().unwrap_or_default().to_owned();
            let payload = raw
                .get("payload")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            (action, payload)
        };
        let outcome = if OUTSIDE_BROAD_LOCK_ACTIONS.contains(&action.as_str()) {
            self.dispatch(&action, &payload)
        } else {
            let _g = lock(&self.action);
            self.dispatch(&action, &payload)
        };
        match outcome {
            Ok(result) => {
                self.audit_row(&action, &payload, Some(&result), None);
                Ok(json!({"schema": SCHEMA, "ok": true, "action": action, "result": result}))
            }
            Err(e) => {
                self.audit_row(&action, &payload, None, Some(e.message()));
                Err(e)
            }
        }
    }

    fn problem_context(
        &self,
        p: &Map<String, Value>,
    ) -> AgentResult<(Option<Value>, &'static str)> {
        if let Some(problem) = p.get("problem") {
            return Ok((Some(problem.clone()), "provided_problem"));
        }
        if p.get("use_current_problem").is_none_or(truthy) {
            let problem = self
                .state_without_plan()?
                .get("engineering_problem")
                .cloned()
                .filter(|v| !v.is_null());
            let source = if problem.is_some() {
                "current_problem"
            } else {
                "none_available"
            };
            return Ok((problem, source));
        }
        Ok((None, "not_requested"))
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&self, action: &str, p: &Map<String, Value>) -> AgentResult<Value> {
        let obj = || Value::Object(p.clone());
        let host = &self.host;
        let text = |k: &str| field(p, k).map(py_str);
        let monitor = |result: Value, view: &str| -> AgentResult<Value> {
            if view == "monitor" {
                optimization_job_monitor_view(&result)
            } else {
                Ok(result)
            }
        };
        match action {
            "render_mesh_scene" => self.render_mesh_scene(p),
            "inspect_geometry_kinds" | "inspect_model_document" | "evaluate_geometry" | "export_geometry" | "import_mesh_geometry" | "prepare_run_comparison" | "check_gradients" => self.public_geometry_action(action, p),
            "inspect_runtime_environment" => {
                Ok(implexity_core::runtime_environment::inspect_runtime_environment())
            }
            "inspect_guided_setup" => host.call(HostOp::GuidedSetupInspect),
            "review_guided_setup" => host.call(HostOp::GuidedSetupReview(obj())),
            "apply_guided_setup" => host.call(HostOp::GuidedSetupApply(obj())),
            "revise_engineering_problem" => host.call(HostOp::ReviseProblem(obj())),
            "inspect_optimization_setup" => host.call(HostOp::OptimizationSetupInspect),
            "save_optimization_setup" => host.call(HostOp::OptimizationSetupSave(obj())),
            "start_managed_preflight" | "start_managed_evaluation" | "start_managed_sensitivity" => {
                let kind =
                    MANAGED_START_ACTIONS.iter().find(|(n, _)| *n == action).map_or("evaluate", |(_, k)| *k);
                let request = field(p, "request")?.clone();
                self.start_managed_evaluation(kind, request)
            }
            "inspect_managed_evaluation" => self.inspect_managed_evaluation(&text("operation_id")?),
            "cancel_managed_evaluation" => self.cancel_managed_evaluation(&text("operation_id")?),
            "initialize_lattice_seed" => host.call(HostOp::InitializeLatticeSeed(obj())),
            "inspect_application_cases" => Ok(json!({"schema": "implexity-application-case-catalogue/1",
                "builders": implexity_core::registries::global().extensions.case_authoring_catalogue()})),
            "author_application_case" => self.author_application_case(p),
            "inspect_imported_providers" => host.imported_providers(None,false),
            "import_physics_provider" | "remove_physics_provider" => {
                let mut request=p.clone();request.insert("operation".into(),json!(if action=="import_physics_provider" {"import"} else {"remove"}));
                host.imported_providers(Some(&Value::Object(request)),false)
            }
            "check_imported_provider" => host.imported_providers(Some(&obj()),true),
            "inspect_physics_packages" => host.packages_status(),
            "load_physics_package" | "unload_physics_package" => {
                let op = if action == "load_physics_package" { "load" } else { "unload" };
                host.packages_change(&text("package")?, op, p.get("expected_generation"))
            }
            "inspect_couplings" | "inspect_boundary_controls" | "inspect_workspace_capabilities" => {
                let (problem, source) = self.problem_context(p)?;
                let provider = p.get("provider").filter(|v| !v.is_null()).map(py_str);
                let out = match action {
                    "inspect_couplings" => implexity_runtime::coupling_control::coupling_catalogue(
                        provider.as_deref(),
                        problem.as_ref(),
                        source,
                    )?,
                    "inspect_boundary_controls" => {
                        implexity_solve_boundary(provider.as_deref(), problem.as_ref(), source)?
                    }
                    _ => implexity_runtime::workspace_capabilities::workspace_capabilities(
                        provider.as_deref(),
                        problem.as_ref(),
                        source,
                    )?,
                };
                Ok(out)
            }
            "inspect_geometry_seeds" => host.call(HostOp::SeedCatalogue),
            "preview_geometry_seed" => host.call(HostOp::SeedPreview(obj())),
            "commit_geometry_seed" => host.call(HostOp::SeedCommit(obj())),
            "preview_current_bake" => host.call(HostOp::SeedPreviewCurrent(obj())),
            "bake_current_geometry" => host.call(HostOp::SeedBakeCurrent(obj())),
            "import_model" => host.call(HostOp::ImportModel(field(p, "document")?.clone())),
            "inspect_state" => self.state(),
            "inspect_optimization_job" => {
                let info = host.call(HostOp::JobInfo(text("job_id")?))?;
                monitor(info, &view_of(p))
            }
            "read_optimization_epoch_field" => host.call(HostOp::ReadEpochField(json!({
                "job_id": text("job_id")?, "epoch": field(p, "epoch")?,
                "field": text("field")?,
                "operating_point": p.get("operating_point").cloned().unwrap_or_else(|| json!(0)),
                "maximum_bytes": p.get("maximum_bytes").cloned().unwrap_or_else(|| json!(1_048_576)),
                "mode": p.get("mode").cloned().unwrap_or_else(|| json!("payload"))}))),
            "export_optimization_epoch" => {
                let epoch = field(p, "epoch")?
                    .as_i64()
                    .ok_or_else(|| AgentError::contract("epoch must be an integer"))?;
                host.call(HostOp::ExportEpoch(text("job_id")?, epoch))
            }
            "render_section" => self.render_section(p),
            "prepare_viewer_scene" => self.prepare_viewer_scene(p),
            "render_viewer_snapshot" => self.render_viewer_snapshot(p),
            "render_3d" => self.render_3d(p),
            "export_stl" => self.export_stl(p),
            "inspect_renderables" => self.inspect_renderables(p),
            "render_optimization_history" => self.render_optimization_history(p),
            "campaign_rank" => crate::campaign::rank_variants(
                field(p, "variants")?,
                &text_or(p.get("objective"), "L_best"),
                &text_or(p.get("sense"), "minimize"),
            ),
            "campaign_compare" => crate::campaign::compare_variants(&obj()),
            "campaign_budget" => crate::campaign::allocate_budget(field(p, "branches")?, Some(&obj())),
            "set_intent" => {
                let intent = self.set_intent(&obj())?;
                let plan = self.physics_plan(Some(&intent))?;
                Ok(json!({"intent": intent, "physics_plan": plan}))
            }
            "inspect_physics_plan" => Ok(json!({"physics_plan": self.physics_plan(p.get("intent"))?})),
            "inspect_study" => host.call(HostOp::InspectStudy(text("study_id")?)),
            "create_study" => host.call(HostOp::CreateStudy(obj())),
            "run_study_variant" => host.call(HostOp::RunStudyVariant(obj())),
            "promote_cage" => host.call(HostOp::PromoteCage(obj())),
            "rebind_region" => {
                host.call(HostOp::RebindRegion(field(p, "region")?.clone(), field(p, "candidates")?.clone()))
            }
            "interaction_capabilities" => host.call(HostOp::InteractionCapabilities),
            "interaction_history_state"
            | "interaction_field"
            | "interaction_begin"
            | "interaction_preview"
            | "interaction_commit"
            | "interaction_cancel"
            | "interaction_refine"
            | "interaction_undo"
            | "interaction_redo" => {
                let method = action.split_once('_').map_or("", |(_, m)| m).to_owned();
                let mut payload = p.clone();
                if method == "begin" {
                    payload.insert("_client_origin".into(), json!("public_agent"));
                }
                host.call(HostOp::Interaction(method, Value::Object(payload)))
            }
            "inspect_parameters" => host.call(HostOp::InspectParameters),
            "set_parameters" => host.call(HostOp::SetParameters(field(p, "values")?.clone())),
            "edit_graph" => host.call(HostOp::EditGraph(obj())),
            "set_engineering_problem" => host.call(HostOp::SetEngineeringProblem {
                payload: obj(),
                provider_envelope: implexity_runtime::provider_problem_document::is_provider_problem_envelope(
                    &obj(),
                ),
            }),
            "validate_model" => {
                host.call(HostOp::ValidateModel(p.get("document").cloned().unwrap_or_else(obj)))
            }
            "read_result_array" => self.read_result_array(p),
            "evaluate_authored_case" => self.evaluate_authored_case(p),
            "evaluate_results" => host.call(HostOp::Results(obj())),
            "sensitivity" => host.call(HostOp::Sensitivity(obj())),
            "preflight_optimization" => {
                let mut request = p.clone();
                let view = request.shift_remove("view").map_or_else(|| "full".to_owned(), |v| py_str(&v));
                let result = host.call(HostOp::Preflight(Value::Object(request)))?;
                if view == "monitor" { optimization_preflight_monitor_view(&result) } else { Ok(result) }
            }
            "start_optimization" => {
                let mut request = p.clone();
                let view = request.shift_remove("view").map_or_else(|| "full".to_owned(), |v| py_str(&v));
                let result = host.call(HostOp::Start(Value::Object(request)))?;
                monitor(result, &view)
            }
            "steer_optimization" => host.call(HostOp::Steer(text("job_id")?, field(p, "request")?.clone())),
            "use_optimization_result" => host.call(HostOp::UseWorkingDesign(obj())),
            "optimization_operation" => {
                let result = host.call(HostOp::JobOperation(text("job_id")?, text("op")?))?;
                monitor(result, &view_of(p))
            }
            "branch_after_intervention" => {
                let request = p.get("request").cloned().unwrap_or(Value::Null);
                let result = host.call(HostOp::BranchAfterIntervention(text("job_id")?, request))?;
                monitor(result, &view_of(p))
            }
            "resolve_numerical_attention" => {
                let result = host.call(HostOp::ResolveNumericalAttention(
                    text("job_id")?,
                    text("event_token")?,
                    text("action")?,
                ))?;
                monitor(result, &view_of(p))
            }
            "branch_after_numerical_attention" => {
                let result =
                    host.call(HostOp::BranchAfterNumericalAttention(text("job_id")?, text("event_token")?))?;
                monitor(result, &view_of(p))
            }
            "save_branch_point" => {
                let label = text_or(p.get("label"), "Agent branch point");
                let details =
                    p.get("details").cloned().unwrap_or_else(|| json!({"actor": "Engineering Agent"}));
                host.call(HostOp::HistorySnapshot(label, details))
            }
            "restore_branch_point" => host.call(HostOp::HistoryRestore(text("entry_id")?)),
            "manipulation_begin"
            | "manipulation_preview"
            | "manipulation_guidance"
            | "manipulation_commit"
            | "manipulation_cancel"
            | "manipulation_undo"
            | "manipulation_redo" => {
                let method = action.trim_start_matches("manipulation_").to_owned();
                host.call(HostOp::Manipulation(method, obj()))
            }
            #[cfg(feature = "dynamic-results")]
            a if crate::dynamic::is_action(a) => crate::dynamic::dispatch_with_origin(
                &host.dynamic_catalogue(),
                host.viewer_capture_origin().as_deref(),
                a,
                p,
            ),
            other => {
                Err(AgentError::refused(format!("unimplemented engineering action {}", repr_str(other))))
            }
        }
    }

    fn read_result_array(&self, p: &Map<String, Value>) -> AgentResult<Value> {
        if p.keys()
            .any(|k| !["artifact_id", "field", "offset", "count"].contains(&k.as_str()))
            || !p.contains_key("artifact_id")
            || !p.contains_key("field")
        {
            return Err(AgentError::contract(
                "result read requires artifact_id/field and only optional offset/count",
            ));
        }
        if !p.get("artifact_id").is_some_and(Value::is_string)
            || p.get("field")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(AgentError::contract(
                "result read requires string artifact_id and nonempty field",
            ));
        }
        let offset = p.get("offset").cloned().unwrap_or_else(|| json!(0));
        if !(is_int(&offset) && offset.as_i64().is_some_and(|o| o >= 0)) {
            return Err(AgentError::contract(
                "result-array offset must be a nonnegative integer",
            ));
        }
        if let Some(count) = p.get("count").filter(|v| !v.is_null())
            && !(is_int(count) && count.as_i64().is_some_and(|c| (1..=4096).contains(&c)))
        {
            return Err(AgentError::contract(
                "agent result-array chunk count must lie in 1..4096",
            ));
        }
        self.host
            .call(HostOp::ReadResultArray(Value::Object(p.clone())))
    }

    fn evaluate_authored_case(&self, p: &Map<String, Value>) -> AgentResult<Value> {
        let declaration = field(p, "problem")?.clone();
        let design = field(p, "design")?.clone();
        let runtime = implexity_runtime::cae_runtime::CaeRuntime::default();
        let mut result = runtime.evaluate(&declaration, Some(&design))?;
        let prepared = runtime.prepare(&declaration)?;
        for (k, v) in prepared.coordinate_selection() {
            result.insert(k, v);
        }
        let result = Value::Object(result);
        if p.get("view").map_or_else(|| "full".to_owned(), py_str) == "artifact" {
            return self.host.call(HostOp::AuthoredEvaluationArtifact {
                result,
                problem: declaration,
                design,
            });
        }
        Ok(result)
    }

    fn author_application_case(&self, p: &Map<String, Value>) -> AgentResult<Value> {
        let case = field(p, "case")?.clone();
        let schema = case.get("schema").map(py_str).unwrap_or_default();
        let entry = implexity_core::registries::global()
            .extensions
            .case_authoring(&schema)
            .map_err(|e| AgentError::contract(e.message()))?;
        let built = (entry.builder)(&case)
            .map_err(|e| AgentError::contract(format!("{}: {}", entry.label, e.message)))?;
        let Some(output) = built.as_object().cloned() else {
            return Err(AgentError::contract(format!(
                "{}: builder did not return an object",
                entry.label
            )));
        };
        if let Err(e) = implexity_core::wire::ensure_finite(&Value::Object(output.clone())) {
            return Err(AgentError::contract(format!(
                "{}: builder output is not finite JSON: {e}",
                entry.label
            )));
        }
        let c = contracts()?;
        let actions = output
            .get("authoring_actions")
            .and_then(Value::as_array)
            .cloned();
        let well_formed = actions.as_ref().is_some_and(|rows| {
            rows.iter().all(|row| {
                row.as_object().is_some_and(|r| {
                    r.len() == 2
                        && r.get("action")
                            .and_then(Value::as_str)
                            .is_some_and(|a| c.get(a).is_some())
                        && r.get("payload").is_some_and(Value::is_object)
                })
            })
        });
        let Some(actions) = actions.filter(|_| well_formed) else {
            return Err(AgentError::contract(format!(
                "{}: builder returned malformed authoring_actions",
                entry.label
            )));
        };
        if actions.iter().any(|r| {
            matches!(
                r["action"].as_str(),
                Some("start_optimization" | "author_application_case")
            )
        }) {
            return Err(AgentError::contract(format!(
                "{}: authoring actions may not start optimization or nest case authoring",
                entry.label
            )));
        }
        let reports: Map<String, Value> = entry
            .report_keys
            .iter()
            .filter_map(|k| output.get(k).map(|v| (k.clone(), v.clone())))
            .collect();
        let execute = p.get("execute").is_some_and(truthy);
        let mut result = json!({"schema": "implexity-application-case-authoring/1",
            "case_schema": entry.schema,
            "builder": {"owner_id": entry.owner_id, "label": entry.label},
            "reports": reports, "output": output,
            "execution": {"requested": execute, "status": "not_requested", "steps": []}});
        if !execute {
            return Ok(result);
        }
        let stop_before = p.get("stop_before").cloned();
        let view = p
            .get("execution_view")
            .cloned()
            .unwrap_or_else(|| json!("monitor"));
        let mut steps = Vec::new();
        let mut status = "completed".to_owned();
        for (index, row) in actions.iter().enumerate() {
            let name = row["action"].as_str().unwrap_or_default();
            if stop_before.as_ref().is_some_and(|s| s == name) {
                status = format!("stopped_before:{name}");
                break;
            }
            let mut payload = row["payload"].as_object().cloned().unwrap_or_default();
            if name == "preflight_optimization" && !payload.contains_key("view") {
                payload.insert("view".into(), view.clone());
            }
            match self.execute(&json!({"action": name, "payload": payload})) {
                Ok(reply) => steps.push(json!({"index": index, "action": name, "ok": true,
                                               "result": reply.get("result").cloned().unwrap_or(Value::Null)})),
                Err(e) => {
                    steps.push(json!({"index": index, "action": name, "ok": false,
                                      "error": e.message(), "error_type": e.python_class()}));
                    status = "failed".into();
                    break;
                }
            }
        }
        result["execution"]["status"] = json!(status);
        result["execution"]["steps"] = Value::Array(steps);
        Ok(result)
    }
}

fn implexity_solve_boundary(
    provider: Option<&str>,
    problem: Option<&Value>,
    source: &str,
) -> AgentResult<Value> {
    Ok(implexity_solve::boundary_control::boundary_catalogue(
        provider, problem, source,
    )?)
}

fn identify_actor(node: &mut Value) {
    match node {
        Value::Object(m) => {
            if let Some(Value::Object(p)) = m.get_mut("provenance") {
                p.insert("requested_by".into(), json!("engineering_agent"));
                p.insert("request_interface".into(), json!("/v1/agent"));
            }
            for child in m.values_mut() {
                identify_actor(child);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(identify_actor),
        _ => {}
    }
}

fn check_sensitivity_batch(request: &Map<String, Value>) -> AgentResult<()> {
    let ok = request
        .get("sensitivity_responses")
        .and_then(Value::as_array)
        .is_some_and(|r| {
            let mut seen = std::collections::BTreeSet::new();
            !r.is_empty()
                && r.iter().all(|n| {
                    n.as_str()
                        .is_some_and(|s| !s.trim().is_empty() && seen.insert(s))
                })
        });
    if !ok {
        return Err(AgentError::contract(
            "sensitivity_responses must be a nonempty array of unique nonempty strings",
        ));
    }
    if request.get("response").is_some_and(|v| !v.is_null())
        || request.get("stream_response").is_some_and(|v| !v.is_null())
    {
        return Err(AgentError::contract(
            "a sensitivity batch cannot also select response or stream_response",
        ));
    }
    Ok(())
}

fn validate_history_render(p: &Map<String, Value>) -> AgentResult<()> {
    let allowed = [
        "job_id",
        "series",
        "width_px",
        "height_px",
        "events",
        "through_iteration",
        "background",
    ];
    if p.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(AgentError::contract(
            "optimization history render has unknown fields",
        ));
    }
    if !p.get("job_id").is_some_and(is_job_id) {
        return Err(AgentError::contract(
            "optimization history render requires a 12-character lowercase hexadecimal job_id",
        ));
    }
    if let Some(series) = p.get("series").filter(|v| !v.is_null()) {
        let ok = series.as_array().is_some_and(|s| {
            let mut seen = std::collections::BTreeSet::new();
            !s.is_empty()
                && s.len() <= 8
                && s.iter().all(|x| {
                    x.as_str().is_some_and(|t| {
                        !t.is_empty() && t.chars().count() <= 160 && seen.insert(t)
                    })
                })
        });
        if !ok {
            return Err(AgentError::contract(
                "optimization history series must contain 1 to 8 unique names",
            ));
        }
    }
    let width = p.get("width_px").cloned().unwrap_or_else(|| json!(640));
    let height = p.get("height_px").cloned().unwrap_or_else(|| json!(360));
    let in_range = |v: &Value, lo: i64, hi: i64| {
        is_int(v) && v.as_i64().is_some_and(|x| (lo..=hi).contains(&x))
    };
    if !in_range(&width, 256, 768) || !in_range(&height, 192, 512) {
        return Err(AgentError::contract(
            "optimization history dimensions are outside the public bounds",
        ));
    }
    let background = p
        .get("background")
        .cloned()
        .unwrap_or_else(|| json!("dark"));
    if !background
        .as_str()
        .is_some_and(|b| b == "dark" || b == "white")
    {
        return Err(AgentError::contract(
            "optimization history background must be dark or white",
        ));
    }
    if let Some(t) = p.get("through_iteration").filter(|v| !v.is_null())
        && !(is_int(t) && t.as_i64().is_some_and(|x| x >= 0))
    {
        return Err(AgentError::contract(
            "optimization history through_iteration must be a nonnegative integer",
        ));
    }
    let events = p.get("events").cloned().unwrap_or_else(|| json!([]));
    let ok = events.as_array().is_some_and(|e| {
        e.len() <= 32
            && e.iter().all(|row| {
                row.as_object().is_some_and(|r| {
                    r.len() == 2
                        && r.get("iteration")
                            .is_some_and(|i| is_int(i) && i.as_i64().is_some_and(|x| x >= 0))
                        && r.get("label")
                            .and_then(Value::as_str)
                            .is_some_and(|l| !l.is_empty() && l.chars().count() <= 80)
                })
            })
    });
    if !ok {
        return Err(AgentError::contract(
            "optimization history events are invalid",
        ));
    }
    Ok(())
}
