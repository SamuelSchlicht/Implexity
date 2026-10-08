// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cell::RefCell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use implexity_core::json::{DumpOptions, dumps};
use implexity_core::py_repr::is_printable;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

pub const SCHEMA: &str = "implexity-numerical-progress/1";
pub const PREFIX: &str = "NUMERICAL ";
pub const MAX_RECORD_BYTES: usize = 16 * 1024;
pub const MAX_RECORDS: i64 = 8192;
const CONTEXT_LIMIT: usize = 64;

pub const EVENTS: [&str; 25] = [
    "solver_recovery",
    "optimization_trial_decision",
    "provider_operation",
    "history_solve",
    "history_step_solve",
    "history_step_converged",
    "history_step_guess_fallback",
    "history_adjoint",
    "history_adjoint_step",
    "implicit_solve",
    "nonlinear_residual",
    "newton_iteration",
    "state_jacobian_callback",
    "linear_matrix_validation",
    "linear_csc_conversion",
    "linear_sparse_factorization",
    "linear_dense_factorization",
    "linear_factorization_ready",
    "linear_condition_estimate",
    "linear_solve",
    "linear_residual_certification",
    "line_search_residual",
    "exact_krylov_condition_estimate",
    "exact_krylov_dispatch",
    "exact_matrix_free_dispatch",
];
pub const PHASES: [&str; 4] = ["started", "finished", "failed", "point"];
const INT_FIELDS: [&str; 7] =
    ["history_step", "newton_iteration", "trial_count", "iterations", "steps", "state_size", "design_size"];
const FLOAT_FIELDS: [&str; 3] = ["residual_norm", "alpha", "duration_s"];
const MAP_FIELDS: [&str; 3] = ["normalized_residuals", "fields_passed", "field_residual_norms"];
const TEXT_FIELDS: [&str; 2] = ["error_type", "failure_reason"];
const BASE_FIELDS: [&str; 8] =
    ["schema", "sequence", "elapsed_s", "event", "phase", "diagnostic_only", "suppressed_events", "capped"];
const ROLES: [&str; 3] = ["authoritative_candidate", "auxiliary", "unspecified"];

fn invalid(m: &str) -> CaeError {
    CaeError::contract(m.to_string())
}

fn finite_nonnegative(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0),
        _ => false,
    }
}

fn exact_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if !n.is_f64() => n.as_i64(),
        _ => None,
    }
}

fn metric_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && key.len() <= 96
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

fn bounded_text(value: &Value, limit: usize) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= limit && s.chars().all(is_printable))
}

fn design_state_id(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.strip_prefix("design-").is_some_and(|h| {
            h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    })
}

fn validate_trial_decision(value: &Value) -> CaeResult<()> {
    let required = ["schema", "iteration", "attempt", "decision", "current_design_state_id", "step_fraction"];
    let optional = [
        "candidate_design_state_id",
        "objective",
        "armijo_bound",
        "directional",
        "error_type",
        "error",
        "reason",
        "unavailable_fields",
    ];
    let Some(m) = value.as_object().filter(|m| {
        required.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    }) else {
        return Err(invalid("trial decision has malformed fields"));
    };
    if m["schema"].as_str() != Some("implexity-optimization-trial-decision/1") {
        return Err(invalid("unknown trial decision schema"));
    }
    let decisions = [
        "started",
        "accepted",
        "numerical_failure",
        "armijo_rejected",
        "geometry_rejected",
        "admission_rejected",
        "admission_rescaled",
        "regime_rejected",
        "no_trial",
    ];
    if !m["decision"].as_str().is_some_and(|d| decisions.contains(&d)) {
        return Err(invalid("unknown trial decision"));
    }
    for key in ["iteration", "attempt"] {
        if exact_int(&m[key]).is_none_or(|v| v < 0) {
            return Err(invalid("invalid trial counter"));
        }
    }
    for key in ["current_design_state_id", "candidate_design_state_id"] {
        if let Some(v) = m.get(key)
            && !design_state_id(v)
        {
            return Err(invalid("invalid trial design identity"));
        }
    }
    if !finite_nonnegative(&m["step_fraction"]) || m["step_fraction"].as_f64().is_some_and(|v| v > 1.0) {
        return Err(invalid("invalid trial step fraction"));
    }
    for key in ["objective", "armijo_bound", "directional"] {
        if let Some(v) = m.get(key)
            && !v.as_f64().is_some_and(f64::is_finite)
        {
            return Err(invalid("invalid signed trial scalar"));
        }
    }
    for (key, limit) in [("error_type", 128), ("error", 1536), ("reason", 1536)] {
        if let Some(v) = m.get(key)
            && !bounded_text(v, limit)
        {
            return Err(invalid("invalid bounded trial error"));
        }
    }
    if let Some(v) = m.get("unavailable_fields") {
        let ok = v.as_array().is_some_and(|a| {
            a.len() <= 3
                && a.iter().all(|k| {
                    k.as_str().is_some_and(|s| ["objective", "armijo_bound", "directional"].contains(&s))
                })
        });
        if !ok {
            return Err(invalid("invalid unavailable trial scalar list"));
        }
    }
    Ok(())
}


#[allow(clippy::too_many_lines)]
pub fn validate_numerical_progress(raw: &Value) -> CaeResult<Value> {
    if let Some(m) = raw.as_object()
        && let Some(decision) = m.get("trial_decision")
    {
        if m.get("event").and_then(Value::as_str) != Some("optimization_trial_decision") {
            return Err(invalid("trial decision requires its declared progress event"));
        }
        validate_trial_decision(decision)?;
    }
    if let Some(report) = raw.get("solver_recovery") {
        if raw.get("event").and_then(Value::as_str) != Some("solver_recovery") { return Err(invalid("solver recovery requires its declared progress event")); }
        implexity_core::error::validate_solver_recovery(report)?;
    }
    if raw.get("event").and_then(Value::as_str) == Some("solver_recovery") && raw.get("solver_recovery").is_none() { return Err(invalid("solver recovery report is missing")); }
    let known = |k: &str| {
        BASE_FIELDS.contains(&k)
            || INT_FIELDS.contains(&k)
            || FLOAT_FIELDS.contains(&k)
            || MAP_FIELDS.contains(&k)
            || TEXT_FIELDS.contains(&k)
            || matches!(k, "evaluation_role" | "last_rejection" | "trial_decision" | "solver_recovery")
    };
    let Some(m) = raw
        .as_object()
        .filter(|m| m.keys().all(|k| known(k)) && BASE_FIELDS.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(invalid("numerical progress has unknown or missing fields"));
    };
    let event = m["event"].as_str();
    let phase = m["phase"].as_str();
    if m["schema"].as_str() != Some(SCHEMA)
        || m["diagnostic_only"] != Value::Bool(true)
        || !m["capped"].is_boolean()
        || !event.is_some_and(|e| EVENTS.contains(&e))
        || !phase.is_some_and(|p| PHASES.contains(&p))
    {
        return Err(invalid("numerical progress schema/event/authority is invalid"));
    }
    for (name, minimum) in [("sequence", 1), ("suppressed_events", 0)] {
        if exact_int(&m[name]).is_none_or(|v| v < minimum) {
            return Err(invalid("numerical progress counter is invalid"));
        }
    }
    let sequence = exact_int(&m["sequence"]).unwrap_or(0);
    if sequence > MAX_RECORDS {
        return Err(invalid("numerical progress record budget exceeded"));
    }
    if !finite_nonnegative(&m["elapsed_s"]) {
        return Err(invalid("numerical progress elapsed time is invalid"));
    }
    for name in INT_FIELDS {
        if let Some(v) = m.get(name)
            && exact_int(v).is_none_or(|x| x < 0)
        {
            return Err(invalid("numerical progress integer is invalid"));
        }
    }
    for name in FLOAT_FIELDS {
        if let Some(v) = m.get(name) {
            if !finite_nonnegative(v) {
                return Err(invalid("numerical progress scalar is invalid"));
            }
            if name == "alpha" && v.as_f64().is_some_and(|a| a > 1.0) {
                return Err(invalid("numerical progress step fraction is invalid"));
            }
        }
    }
    for name in MAP_FIELDS {
        if let Some(v) = m.get(name) {
            let Some(entries) = v.as_object().filter(|e| e.len() <= 32 && e.keys().all(|k| metric_name(k)))
            else {
                return Err(invalid("numerical progress field map is invalid"));
            };
            for value in entries.values() {
                let valid =
                    if name == "fields_passed" { value.is_boolean() } else { finite_nonnegative(value) };
                if !valid {
                    return Err(invalid("numerical progress field metric is invalid"));
                }
            }
        }
    }
    for name in TEXT_FIELDS {
        if let Some(v) = m.get(name) {
            let limit = if name == "failure_reason" { 1536 } else { 128 };
            if phase != Some("failed") || !bounded_text(v, limit) {
                return Err(invalid("numerical progress failure text is invalid"));
            }
        }
    }
    if m.contains_key("failure_reason") && !m.contains_key("error_type") {
        return Err(invalid("numerical failure reason requires an error type"));
    }
    if let Some(rejection) = m.get("last_rejection") {
        let allowed = [
            "schema",
            "sequence",
            "elapsed_s",
            "event",
            "evaluation_role",
            "history_step",
            "newton_iteration",
            "alpha",
            "error_type",
            "failure_reason",
        ];
        let required =
            ["schema", "sequence", "elapsed_s", "event", "evaluation_role", "error_type", "failure_reason"];
        let Some(r) = rejection.as_object().filter(|r| {
            r.keys().all(|k| allowed.contains(&k.as_str()))
                && required.iter().all(|k| r.contains_key(*k))
                && r["schema"].as_str() == Some("implexity-rejected-trial/1")
        }) else {
            return Err(invalid("last rejected trial has an invalid closed schema"));
        };
        let rs = exact_int(&r["sequence"]);
        let future = !rs.is_some_and(|s| s >= 1 && s <= sequence)
            || !finite_nonnegative(&r["elapsed_s"])
            || r["elapsed_s"].as_f64().unwrap_or(f64::NAN) > m["elapsed_s"].as_f64().unwrap_or(f64::NAN);
        if future {
            return Err(invalid("last rejected trial cannot be a future observation"));
        }
        let mut checked: Map<String, Value> =
            r.iter().filter(|(k, _)| k.as_str() != "schema").map(|(k, v)| (k.clone(), v.clone())).collect();
        checked.insert("schema".into(), Value::String(SCHEMA.into()));
        checked.insert("phase".into(), Value::String("failed".into()));
        checked.insert("diagnostic_only".into(), Value::Bool(true));
        checked.insert("suppressed_events".into(), Value::from(0));
        checked.insert("capped".into(), Value::Bool(false));
        validate_numerical_progress(&Value::Object(checked))?;
    }
    if let Some(role) = m.get("evaluation_role")
        && !role.as_str().is_some_and(|r| ROLES.contains(&r))
    {
        return Err(invalid("numerical progress evaluation role is invalid"));
    }
    if dumps(raw, &DumpOptions::compact()).len() > MAX_RECORD_BYTES {
        return Err(invalid("numerical progress record exceeds its byte bound"));
    }
    Ok(raw.clone())
}

pub type Clock = Box<dyn Fn() -> f64>;
pub type Sink = Box<dyn FnMut(&Value) -> CaeResult<()>>;

pub struct NumericalProgressEmitter {
    send: Sink,
    clock: Clock,
    interval_s: f64,
    maximum_records: i64,
    started: f64,
    last_sent: f64,
    sequence: i64,
    suppressed: i64,
    latest: Option<(String, String)>,
    contexts: Vec<(String, String)>,
    history_contexts: Vec<(String, i64)>,
    last_rejection: Option<Map<String, Value>>,
}

impl std::fmt::Debug for NumericalProgressEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NumericalProgressEmitter")
            .field("sequence", &self.sequence)
            .field("suppressed", &self.suppressed)
            .finish_non_exhaustive()
    }
}

impl NumericalProgressEmitter {

    pub fn new(send: Sink, clock: Clock, interval_s: f64, maximum_records: i64) -> CaeResult<Self> {
        if !interval_s.is_finite() || interval_s < 0.0 || !(2..=MAX_RECORDS).contains(&maximum_records) {
            return Err(invalid("numerical progress bounds are invalid"));
        }
        let started = clock();
        Ok(Self {
            send,
            clock,
            interval_s,
            maximum_records,
            started,
            last_sent: f64::NEG_INFINITY,
            sequence: 0,
            suppressed: 0,
            latest: None,
            contexts: Vec::new(),
            history_contexts: Vec::new(),
            last_rejection: None,
        })
    }

    #[must_use]
    pub fn sequence(&self) -> i64 {
        self.sequence
    }

    fn evaluation_role(&mut self, record: &Map<String, Value>) -> CaeResult<String> {
        let parent = self.contexts.last().map_or_else(|| "unspecified".to_string(), |c| c.1.clone());
        let flag = match record.get("operation_context_authority_eligible") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return Err(invalid("numerical trace context eligibility must be boolean")),
        };
        let mut role = if flag == Some(false) || parent == "auxiliary" {
            "auxiliary".to_string()
        } else if flag == Some(true) {
            "authoritative_candidate".to_string()
        } else {
            parent
        };
        let span = record.get("span_id").filter(|v| !v.is_null());
        if flag.is_some()
            && let Some(span) = span
        {
            let Some(span) = span.as_str().filter(|s| (1..=256).contains(&s.chars().count())) else {
                return Err(invalid("numerical trace context span is invalid"));
            };
            match record.get("phase").and_then(Value::as_str) {
                Some("started") => {
                    if self.contexts.len() >= CONTEXT_LIMIT || self.contexts.iter().any(|(s, _)| s == span) {
                        return Err(invalid("numerical trace context nesting is invalid"));
                    }
                    self.contexts.push((span.to_string(), role.clone()));
                }
                Some("finished" | "failed") => {
                    if self.contexts.last().is_none_or(|(s, _)| s != span) {
                        return Err(invalid("numerical trace context lifecycle is unbalanced"));
                    }
                    role = self.contexts.pop().map_or(role, |c| c.1);
                }
                _ => {}
            }
        }
        Ok(role)
    }

    fn history_step(&mut self, record: &Map<String, Value>) -> CaeResult<Option<i64>> {
        let parent = self.history_contexts.last().map(|c| c.1);
        let step = match record.get("history_step") {
            None => parent,
            Some(Value::Null) => None,
            Some(v) => match exact_int(v) {
                Some(s) if s >= 0 => Some(s),
                _ => return Err(invalid("numerical trace history step must be a nonnegative integer")),
            },
        };
        let event = record.get("event").and_then(Value::as_str);
        if matches!(event, Some("history_step_solve" | "history_adjoint_step")) {
            if let Some(span) = record.get("span_id").filter(|v| !v.is_null()) {
                let span = span.as_str().filter(|s| (1..=256).contains(&s.chars().count()));
                let (Some(span), Some(step)) = (span, step) else {
                    return Err(invalid("numerical trace history span is invalid"));
                };
                match record.get("phase").and_then(Value::as_str) {
                    Some("started") => {
                        if self.history_contexts.len() >= CONTEXT_LIMIT
                            || self.history_contexts.iter().any(|(s, _)| s == span)
                        {
                            return Err(invalid("numerical trace history nesting is invalid"));
                        }
                        self.history_contexts.push((span.to_string(), step));
                    }
                    Some("finished" | "failed") => {
                        if self.history_contexts.last() != Some(&(span.to_string(), step)) {
                            return Err(invalid("numerical trace history lifecycle is unbalanced"));
                        }
                        self.history_contexts.pop();
                    }
                    _ => {}
                }
            }
        } else if parent.is_some() && step != parent {
            return Err(invalid("numerical trace history step conflicts with active scope"));
        }
        Ok(step)
    }


    pub fn observe(&mut self, record: &Value) -> CaeResult<()> {
        if self.sequence >= self.maximum_records {
            return Ok(());
        }
        let empty = Map::new();
        let record = record.as_object().unwrap_or(&empty);
        let role = self.evaluation_role(record)?;
        let history_step = self.history_step(record)?;
        let (Some(event), Some(phase)) =
            (record.get("event").and_then(Value::as_str), record.get("phase").and_then(Value::as_str))
        else {
            return Ok(());
        };
        if !EVENTS.contains(&event) || !PHASES.contains(&phase) {
            return Ok(());
        }
        let now = (self.clock)();
        let long_phase_started = phase == "started"
            && [
                "state_jacobian_callback",
                "linear_csc_conversion",
                "linear_sparse_factorization",
                "linear_dense_factorization",
                "linear_condition_estimate",
                "exact_krylov_condition_estimate",
            ]
            .contains(&event)
            && self.latest.as_ref().is_none_or(|(e, p)| (e.as_str(), p.as_str()) != (event, phase));
        let important = long_phase_started
            || phase == "failed"
            || [
                "provider_operation",
                "history_step_solve",
                "history_step_converged",
                "history_step_guess_fallback",
                "newton_iteration",
                "solver_recovery",
    "optimization_trial_decision",
            ]
            .contains(&event);
        if !important && now - self.last_sent < self.interval_s {
            self.suppressed += 1;
            return Ok(());
        }
        let mut data = Map::new();
        data.insert("schema".into(), Value::String(SCHEMA.into()));
        data.insert("sequence".into(), Value::from(self.sequence + 1));
        data.insert("elapsed_s".into(), crate::canonical::f((now - self.started).max(0.0)));
        data.insert("event".into(), Value::String(event.into()));
        data.insert("phase".into(), Value::String(phase.into()));
        data.insert("diagnostic_only".into(), Value::Bool(true));
        data.insert("evaluation_role".into(), Value::String(role));
        data.insert("suppressed_events".into(), Value::from(self.suppressed));
        data.insert("capped".into(), Value::Bool(self.sequence + 1 == self.maximum_records));
        if let Some(step) = history_step {
            data.insert("history_step".into(), Value::from(step));
        }
        if let Some(report) = record.get("solver_recovery") {
            data.insert("solver_recovery".into(), report.clone());
        }
        if let Some(decision) = record.get("trial_decision") {
            data.insert("trial_decision".into(), decision.clone());
        }
        for name in INT_FIELDS.iter().chain(&FLOAT_FIELDS).chain(&MAP_FIELDS).chain(&TEXT_FIELDS) {
            if let Some(v) = record.get(*name) {
                data.insert((*name).to_string(), v.clone());
            }
        }
        if phase == "failed" && data.contains_key("failure_reason") {
            let names = [
                "sequence",
                "elapsed_s",
                "event",
                "evaluation_role",
                "history_step",
                "newton_iteration",
                "alpha",
                "error_type",
                "failure_reason",
            ];
            let mut rejection: Map<String, Value> = data
                .iter()
                .filter(|(k, _)| names.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            rejection.insert("schema".into(), Value::String("implexity-rejected-trial/1".into()));
            self.last_rejection = Some(rejection);
        }
        if let Some(r) = &self.last_rejection {
            data.insert("last_rejection".into(), Value::Object(r.clone()));
        }
        let data = validate_numerical_progress(&Value::Object(data))?;
        (self.send)(&data)?;
        self.sequence += 1;
        self.last_sent = now;
        self.latest = Some((event.to_string(), phase.to_string()));
        Ok(())
    }
}


pub fn managed_numerical_progress<T>(body: impl FnOnce() -> CaeResult<T>) -> CaeResult<T> {
    let origin = Instant::now();
    let send: Sink = Box::new(|record: &Value| {
        let line = format!("{PREFIX}{}\n", dumps(record, &DumpOptions::compact()));
        let mut out = std::io::stdout().lock();
        out.write_all(line.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| CaeError::contract(e.to_string()))
    });
    let clock: Clock = Box::new(move || origin.elapsed().as_secs_f64());
    let emitter = Rc::new(RefCell::new(NumericalProgressEmitter::new(send, clock, 0.25, MAX_RECORDS)?));
    let observer = Rc::clone(&emitter);
    let _guard = implexity_solve::trace::observe(move |record| observer.borrow_mut().observe(record));
    let start = serde_json::json!({"event": "provider_operation", "phase": "started"});
    emitter.borrow_mut().observe(&start)?;
    match body() {
        Ok(v) => Ok(v),
        Err(e) => {
            let failed = serde_json::json!({"event": "provider_operation", "phase": "failed"});
            emitter.borrow_mut().observe(&failed)?;
            Err(e)
        }
    }
}

