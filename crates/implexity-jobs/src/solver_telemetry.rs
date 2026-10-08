// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::cell::RefCell;

use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

pub const NUMERICAL_SOLVER_RECORD_SCHEMA: &str = "implexity-numerical-solver-record/1";
pub const NUMERICAL_SOLVER_RECORD_SET_SCHEMA: &str = "implexity-numerical-solver-record-set/1";
pub const NUMERICAL_DEVIATION_POLICY_SCHEMA: &str = "implexity-numerical-deviation-policy/1";
pub const NUMERICAL_DEVIATION_SCHEMA: &str = "implexity-numerical-certification-deviation/1";
pub const NUMERICAL_SOLVER_FAILURE_SCHEMA: &str = "implexity-numerical-solver-failure/1";

#[derive(Debug, Clone, PartialEq)]
pub enum NumericalEvent {
    Deviation(Map<String, Value>),
    Failure(Map<String, Value>),
}

impl NumericalEvent {
    #[must_use]
    pub fn evidence(&self) -> &Map<String, Value> {
        match self {
            Self::Deviation(e) | Self::Failure(e) => e,
        }
    }

    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Deviation(_) => "NumericalCertificationDeviation",
            Self::Failure(_) => "NumericalSolverFailure",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        let text = |v: Option<&Value>| v.map_or_else(|| "unknown".to_string(), implexity_core::pyobj::py_str);
        match self {
            Self::Deviation(e) => format!(
                "bounded numerical certification deviation: solver_id={}; observed={}; nominal_limit={}",
                text(e.get("solver_id")),
                implexity_core::pyobj::repr(e.get("observed_relative_residual").unwrap_or(&Value::Null)),
                implexity_core::pyobj::repr(e.get("nominal_relative_residual_limit").unwrap_or(&Value::Null)),
            ),
            Self::Failure(e) => format!(
                "numerical solver certification failed closed: solver_id={}; classification={}",
                text(e.get("solver_record").and_then(|r| r.get("solver_id"))),
                text(e.get("classification")),
            ),
        }
    }
}

thread_local! {
    static RAISED: RefCell<Option<NumericalEvent>> = const { RefCell::new(None) };
    static ACTIVE_POLICY: RefCell<Vec<Option<Map<String, Value>>>> = const { RefCell::new(Vec::new()) };
}

#[must_use]
pub fn raise(event: NumericalEvent) -> CaeError {
    let error = CaeError::contract(event.message());
    RAISED.with(|slot| *slot.borrow_mut() = Some(event));
    error
}

#[must_use]
pub fn raised(error: &CaeError) -> Option<NumericalEvent> {
    RAISED.with(|slot| {
        let slot = slot.borrow();
        slot.as_ref()
            .filter(|e| error.message() == e.message() && error.python_class() == "CAEContractError")
            .cloned()
    })
}

pub fn clear_raised() {
    RAISED.with(|slot| *slot.borrow_mut() = None);
}

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn nonempty_text(value: Option<&Value>, label: &str) -> CaeResult<String> {
    match value.and_then(Value::as_str).map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => contract(format!("{label} must be non-empty text")),
    }
}

fn positive_finite(value: Option<&Value>, label: &str) -> Result<f64, String> {
    match value.and_then(Value::as_f64) {
        Some(v) if value.is_some_and(Value::is_number) && v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(format!("{label} must be positive finite data")),
    }
}

fn nonnegative_integer(value: Option<&Value>, label: &str, positive: bool) -> Result<i64, String> {
    let Some(v) = value.filter(|v| v.is_number()) else {
        return Err(format!("{label} must be integer data"));
    };
    let as_float = v.as_f64().unwrap_or(f64::NAN);
    if !as_float.is_finite() {
        return Err(format!("{label} must be integer data"));
    }
    #[allow(clippy::cast_possible_truncation)]
    let result = v.as_i64().unwrap_or(as_float.trunc() as i64);
    #[allow(clippy::float_cmp)]
    let integral = as_float == result as f64;
    if !integral || result < i64::from(positive) {
        let qualifier = if positive { "positive " } else { "nonnegative " };
        return Err(format!("{label} must be {qualifier}integer data"));
    }
    Ok(result)
}

fn finite_or_none(value: Option<f64>, label: &str) -> Result<Option<f64>, String> {
    match value {
        Some(v) if v.is_finite() => Ok(Some(v)),
        Some(_) => Ok(None),
        None => Err(format!("{label} must be numerical data")),
    }
}


pub fn validate_numerical_deviation_policy(policy: Option<&Value>) -> CaeResult<Option<Map<String, Value>>> {
    let Some(policy) = policy.filter(|p| !p.is_null()) else {
        return Ok(None);
    };
    let Some(mut row) = policy.as_object().cloned() else {
        return contract("numerical deviation policy must be an object");
    };
    let allowed = [
        "schema",
        "action",
        "event_token",
        "solver_prefix",
        "bounded_ceiling",
        "retry_max_iterations_per_attempt",
        "retry_attempt_limit",
        "provenance",
    ];
    if row.keys().any(|k| !allowed.contains(&k.as_str())) {
        return contract("numerical deviation policy contains unsupported fields");
    }
    if row.get("schema").and_then(Value::as_str) != Some(NUMERICAL_DEVIATION_POLICY_SCHEMA) {
        return contract("numerical deviation policy schema is unsupported");
    }
    let action = nonempty_text(row.get("action"), "numerical deviation action")?;
    if action != "retry_exact" && action != "continue_exploratory" {
        return contract("numerical deviation action must be retry_exact or continue_exploratory");
    }
    row.insert("action".into(), Value::String(action.clone()));
    let token = nonempty_text(row.get("event_token"), "numerical deviation event_token")?;
    row.insert("event_token".into(), Value::String(token));
    let prefix = nonempty_text(row.get("solver_prefix"), "numerical deviation solver_prefix")?;
    row.insert("solver_prefix".into(), Value::String(prefix));
    let ceiling = positive_finite(row.get("bounded_ceiling"), "numerical deviation bounded_ceiling")
        .map_err(CaeError::contract)?;
    row.insert("bounded_ceiling".into(), Value::from(ceiling));
    let Some(provenance) = row.get("provenance").and_then(Value::as_object).cloned() else {
        return contract("numerical deviation policy provenance must be an object");
    };
    let required = [
        "source_job_id",
        "solve_id",
        "design_state_id",
        "run_fingerprint",
        "provider",
        "runtime_source_sha256",
        "problem_sha256",
        "requested_policy_digest",
        "effective_effort_digest",
        "operation_context_digest",
    ];
    if provenance.len() != required.len() || !required.iter().all(|k| provenance.contains_key(*k)) {
        return contract("numerical deviation policy provenance fields are incomplete");
    }
    let mut checked = Map::new();
    for (key, value) in &provenance {
        checked.insert(
            key.clone(),
            Value::String(nonempty_text(Some(value), &format!("numerical deviation provenance {key}"))?),
        );
    }
    row.insert("provenance".into(), Value::Object(checked));
    if action == "retry_exact" {
        let iterations = nonnegative_integer(
            row.get("retry_max_iterations_per_attempt"),
            "retry_max_iterations_per_attempt",
            true,
        )
        .map_err(CaeError::contract)?;
        let attempts = nonnegative_integer(row.get("retry_attempt_limit"), "retry_attempt_limit", true)
            .map_err(CaeError::contract)?;
        row.insert("retry_max_iterations_per_attempt".into(), Value::from(iterations));
        row.insert("retry_attempt_limit".into(), Value::from(attempts));
    } else if row.contains_key("retry_max_iterations_per_attempt") || row.contains_key("retry_attempt_limit")
    {
        return contract("exploratory numerical policy cannot carry retry budgets");
    }
    Ok(Some(row))
}


pub fn numerical_deviation_scope<T>(
    policy: Option<&Value>,
    body: impl FnOnce(Option<&Map<String, Value>>) -> CaeResult<T>,
) -> CaeResult<T> {
    let checked = validate_numerical_deviation_policy(policy)?;
    ACTIVE_POLICY.with(|stack| stack.borrow_mut().push(Some(checked.clone().unwrap_or_default())));
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            ACTIVE_POLICY.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }
    let _pop = Pop;
    body(checked.as_ref())
}

fn active_policy() -> Option<Map<String, Value>> {
    ACTIVE_POLICY.with(|stack| stack.borrow().last().cloned().flatten())
}

#[must_use]
pub fn current_numerical_deviation_policy() -> Option<Map<String, Value>> {
    active_policy()
}


pub fn numerical_retry_budget(
    solver_prefix: &str,
    default_max_iterations_per_attempt: i64,
    default_attempt_limit: i64,
    provider_max_iterations_per_attempt: i64,
    provider_attempt_limit: i64,
) -> CaeResult<(i64, i64)> {
    let prefix = nonempty_text(Some(&Value::String(solver_prefix.into())), "solver_prefix")?;
    let check = |v: i64, label: &str| -> CaeResult<i64> {
        nonnegative_integer(Some(&Value::from(v)), label, true).map_err(CaeError::contract)
    };
    let default_iterations = check(default_max_iterations_per_attempt, "default_max_iterations_per_attempt")?;
    let default_attempts = check(default_attempt_limit, "default_attempt_limit")?;
    let max_iterations = check(provider_max_iterations_per_attempt, "provider_max_iterations_per_attempt")?;
    let max_attempts = check(provider_attempt_limit, "provider_attempt_limit")?;
    if max_iterations < default_iterations || max_attempts < default_attempts {
        return contract("provider retry caps must not be smaller than default budgets");
    }
    let Some(policy) =
        active_policy().filter(|p| p.get("action").and_then(Value::as_str) == Some("retry_exact"))
    else {
        return Ok((default_iterations, default_attempts));
    };
    let scope = policy.get("solver_prefix").and_then(Value::as_str).unwrap_or("");
    if !prefix.starts_with(scope) {
        return Ok((default_iterations, default_attempts));
    }
    let selected_iterations =
        policy.get("retry_max_iterations_per_attempt").and_then(Value::as_i64).unwrap_or(0);
    let selected_attempts = policy.get("retry_attempt_limit").and_then(Value::as_i64).unwrap_or(0);
    if selected_iterations < default_iterations
        || selected_attempts < default_attempts
        || selected_iterations > max_iterations
        || selected_attempts > max_attempts
    {
        return contract("server-issued exact retry budget exceeds provider bounds");
    }
    Ok((selected_iterations, selected_attempts))
}

fn failure(
    record: &Map<String, Value>,
    classification: &str,
    observed: Option<f64>,
    nominal: f64,
    ceiling: f64,
) -> CaeError {
    let mut evidence = Map::new();
    evidence.insert("schema".into(), Value::String(NUMERICAL_SOLVER_FAILURE_SCHEMA.into()));
    evidence.insert("classification".into(), Value::String(classification.into()));
    evidence.insert("solver_record".into(), Value::Object(record.clone()));
    evidence.insert("observed_relative_residual".into(), observed.map_or(Value::Null, Value::from));
    evidence.insert("nominal_relative_residual_limit".into(), Value::from(nominal));
    evidence.insert("bounded_ceiling".into(), Value::from(ceiling));
    raise(NumericalEvent::Failure(evidence))
}


#[allow(clippy::too_many_lines)]
pub fn require_numerical_certification(
    record: &Value,
    bounded_ceiling: f64,
    retry_max_iterations_per_attempt: i64,
    retry_attempt_limit: i64,
    allow_exploratory: bool,
    solver_prefix: Option<&str>,
) -> CaeResult<Map<String, Value>> {
    let Some(mut checked) = record
        .as_object()
        .filter(|r| r.get("schema").and_then(Value::as_str) == Some(NUMERICAL_SOLVER_RECORD_SCHEMA))
        .cloned()
    else {
        return contract("numerical certification record is malformed");
    };
    let Some(certification) = checked.get("certification").and_then(Value::as_object).cloned() else {
        return contract("numerical certification evidence is malformed");
    };
    if certification.get("passed") == Some(&Value::Bool(true)) {
        checked.insert("authority".into(), Value::String("authoritative".into()));
        return Ok(checked);
    }
    let solver_id = nonempty_text(checked.get("solver_id"), "solver_id")?;
    let prefix_value =
        solver_prefix.map_or_else(|| Value::String(solver_id.clone()), |p| Value::String(p.into()));
    let prefix = nonempty_text(Some(&prefix_value), "solver_prefix")?;
    if !solver_id.starts_with(&prefix) {
        return contract("solver_id is outside its provider solver scope");
    }
    let nominal =
        positive_finite(certification.get("relative_residual_limit"), "nominal certification limit")
            .map_err(CaeError::contract)?;
    let ceiling = positive_finite(Some(&Value::from(bounded_ceiling)), "bounded ceiling")
        .map_err(CaeError::contract)?;
    let current_iterations =
        nonnegative_integer(checked.get("max_iterations_per_attempt"), "max_iterations_per_attempt", true)
            .map_err(CaeError::contract)?;
    let current_attempts = nonnegative_integer(checked.get("attempt_limit"), "attempt_limit", true)
        .map_err(CaeError::contract)?;
    let retry_iterations = nonnegative_integer(
        Some(&Value::from(retry_max_iterations_per_attempt)),
        "retry_max_iterations_per_attempt",
        true,
    )
    .map_err(CaeError::contract)?;
    let retry_attempts =
        nonnegative_integer(Some(&Value::from(retry_attempt_limit)), "retry_attempt_limit", true)
            .map_err(CaeError::contract)?;
    if ceiling <= nominal {
        return contract("bounded numerical ceiling must exceed the nominal limit");
    }
    if retry_iterations < current_iterations || retry_attempts < current_attempts {
        return contract("provider retry budget must not shrink the completed solve budget");
    }
    let exact_retry_available = retry_iterations > current_iterations || retry_attempts > current_attempts;
    let observed = match checked.get("final_relative_residual") {
        None | Some(Value::Null) => {
            return Err(failure(&checked, "nonfinite_residual", None, nominal, ceiling));
        }
        Some(v) => match v.as_f64() {
            Some(x) => x,
            None if matches!(v, Value::Bool(b) if *b) => 1.0,
            None if matches!(v, Value::Bool(_)) => 0.0,
            None => {
                return contract(format!("{solver_id}: numerical certification residual is malformed"));
            }
        },
    };
    if !observed.is_finite() {
        return Err(failure(&checked, "nonfinite_residual", None, nominal, ceiling));
    }
    if observed <= nominal {
        return contract(format!("{solver_id}: failed certification contradicts its residual"));
    }
    if observed > ceiling {
        return Err(failure(&checked, "outside_bounded_envelope", Some(observed), nominal, ceiling));
    }
    let active = ACTIVE_POLICY.with(|stack| stack.borrow().last().cloned());
    let Some(active) = active else {
        return Err(failure(&checked, "attention_scope_unavailable", Some(observed), nominal, ceiling));
    };
    if let Some(active) = active.filter(|a| !a.is_empty())
        && active.get("action").and_then(Value::as_str) == Some("continue_exploratory")
        && allow_exploratory
        && solver_id.starts_with(active.get("solver_prefix").and_then(Value::as_str).unwrap_or(""))
    {
        let policy_ceiling = active.get("bounded_ceiling").and_then(Value::as_f64).unwrap_or(f64::NAN);
        if policy_ceiling > ceiling {
            return contract("exploratory numerical ceiling exceeds provider policy");
        }
        if observed <= policy_ceiling {
            let mut waived = certification.clone();
            waived.insert("status".into(), Value::String("exploratory_waived".into()));
            waived.insert("passed".into(), Value::Bool(false));
            waived.insert("bounded_ceiling".into(), Value::from(policy_ceiling));
            checked.insert("certification".into(), Value::Object(waived));
            checked.insert("authority".into(), Value::String("exploratory_non_authoritative".into()));
            return Ok(checked);
        }
    }
    let mut evidence = Map::new();
    evidence.insert("schema".into(), Value::String(NUMERICAL_DEVIATION_SCHEMA.into()));
    evidence.insert("solver_id".into(), Value::String(solver_id));
    evidence.insert("solver_prefix".into(), Value::String(prefix));
    evidence.insert("solver_record".into(), Value::Object(checked.clone()));
    evidence.insert("observed_relative_residual".into(), Value::from(observed));
    evidence.insert("nominal_relative_residual_limit".into(), Value::from(nominal));
    evidence.insert("bounded_ceiling".into(), Value::from(ceiling));
    evidence.insert("attempt_count".into(), checked.get("attempt_count").cloned().unwrap_or(Value::Null));
    evidence.insert("attempt_limit".into(), checked.get("attempt_limit").cloned().unwrap_or(Value::Null));
    evidence.insert(
        "max_iterations_per_attempt".into(),
        checked.get("max_iterations_per_attempt").cloned().unwrap_or(Value::Null),
    );
    evidence.insert("retry_max_iterations_per_attempt".into(), Value::from(retry_iterations));
    evidence.insert("retry_attempt_limit".into(), Value::from(retry_attempts));
    evidence.insert("exact_retry_available".into(), Value::Bool(exact_retry_available));
    evidence.insert("exploratory_continuation_available".into(), Value::Bool(allow_exploratory));
    Err(raise(NumericalEvent::Deviation(evidence)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct SolverRecordInput<'a> {
    pub solver_id: &'a str,
    pub requested_relative_tolerance: f64,
    pub initial_relative_residual: f64,
    pub final_relative_residual: f64,
    pub attempt_count: i64,
    pub retry_count: i64,
    pub attempt_limit: i64,
    pub max_iterations_per_attempt: i64,
    pub total_iteration_budget: i64,
    pub certification_limit: f64,
    pub algorithm: &'a str,
}


pub fn numerical_solver_record(input: &SolverRecordInput<'_>) -> Result<Map<String, Value>, String> {
    let identifier = input.solver_id.trim();
    let method = input.algorithm.trim();
    if identifier.is_empty() {
        return Err("solver_id must be nonempty text".into());
    }
    if method.is_empty() {
        return Err("algorithm must be nonempty text".into());
    }
    let tolerance = positive_finite(
        Some(&Value::from(input.requested_relative_tolerance)),
        "requested_relative_tolerance",
    )
    .map_err(|_| "requested_relative_tolerance must be positive finite data".to_string())?;
    let limit = positive_finite(Some(&Value::from(input.certification_limit)), "certification_limit")
        .map_err(|_| "certification_limit must be positive finite data".to_string())?;
    let initial = finite_or_none(Some(input.initial_relative_residual), "initial_relative_residual")?;
    let final_residual = finite_or_none(Some(input.final_relative_residual), "final_relative_residual")?;
    let int =
        |v: i64, label: &str, positive: bool| nonnegative_integer(Some(&Value::from(v)), label, positive);
    let attempts = int(input.attempt_count, "attempt_count", true)?;
    let retries = int(input.retry_count, "retry_count", false)?;
    let allowed = int(input.attempt_limit, "attempt_limit", true)?;
    let per_attempt = int(input.max_iterations_per_attempt, "max_iterations_per_attempt", true)?;
    let total_budget = int(input.total_iteration_budget, "total_iteration_budget", true)?;
    if attempts > allowed {
        return Err("attempt_count must not exceed attempt_limit".into());
    }
    if retries >= attempts {
        return Err("retry_count must be smaller than attempt_count".into());
    }
    if total_budget < allowed * per_attempt {
        return Err("total_iteration_budget must cover every allowed attempt".into());
    }
    let passed = final_residual.is_some_and(|f| f <= limit);
    let mut certification = Map::new();
    certification.insert("status".into(), Value::String(if passed { "passed" } else { "failed" }.into()));
    certification.insert("passed".into(), Value::Bool(passed));
    certification.insert("relative_residual_limit".into(), Value::from(limit));
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(NUMERICAL_SOLVER_RECORD_SCHEMA.into()));
    m.insert("solver_id".into(), Value::String(identifier.into()));
    m.insert("algorithm".into(), Value::String(method.into()));
    m.insert("solve_scope".into(), Value::String("primal_forward".into()));
    m.insert("requested_relative_tolerance".into(), Value::from(tolerance));
    m.insert("initial_relative_residual".into(), initial.map_or(Value::Null, Value::from));
    m.insert("initial_residual_scope".into(), Value::String("first_attempt_completion_before_retry".into()));
    m.insert("final_relative_residual".into(), final_residual.map_or(Value::Null, Value::from));
    m.insert("attempt_count".into(), Value::from(attempts));
    m.insert("retry_count".into(), Value::from(retries));
    m.insert("attempt_limit".into(), Value::from(allowed));
    m.insert("max_iterations_per_attempt".into(), Value::from(per_attempt));
    m.insert("attempted_iteration_budget".into(), Value::from(attempts * per_attempt));
    m.insert("total_iteration_budget".into(), Value::from(total_budget));
    m.insert("actual_iteration_count".into(), Value::Null);
    m.insert("certification".into(), Value::Object(certification));
    Ok(m)
}


pub fn numerical_solver_record_set(records: &[Value]) -> Result<Map<String, Value>, String> {
    let mut materialized = Vec::with_capacity(records.len());
    for record in records {
        match record.as_object() {
            Some(r) if r.get("schema").and_then(Value::as_str) == Some(NUMERICAL_SOLVER_RECORD_SCHEMA) => {
                materialized.push(Value::Object(r.clone()));
            }
            _ => return Err("records must contain versioned numerical-solver records".into()),
        }
    }
    let passed: Vec<bool> = materialized
        .iter()
        .map(|r| {
            r.get("certification").and_then(|c| c.get("passed")).is_some_and(implexity_core::pyobj::truthy)
        })
        .collect();
    let all_certified = if materialized.is_empty() { None } else { Some(passed.iter().all(|p| *p)) };
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(NUMERICAL_SOLVER_RECORD_SET_SCHEMA.into()));
    m.insert(
        "status".into(),
        Value::String(
            match all_certified {
                None => "not_requested",
                Some(true) => "passed",
                Some(false) => "failed",
            }
            .into(),
        ),
    );
    m.insert("all_certified".into(), all_certified.map_or(Value::Null, Value::Bool));
    m.insert("record_count".into(), Value::from(materialized.len()));
    m.insert("records".into(), Value::Array(materialized));
    Ok(m)
}


pub fn validate_numerical_solver_record(record: &Value) -> Result<Map<String, Value>, String> {
    let Some(row) = record.as_object() else {
        return Err("numerical solver record must be an object".into());
    };
    let expected = [
        "schema",
        "solver_id",
        "algorithm",
        "solve_scope",
        "requested_relative_tolerance",
        "initial_relative_residual",
        "initial_residual_scope",
        "final_relative_residual",
        "attempt_count",
        "retry_count",
        "attempt_limit",
        "max_iterations_per_attempt",
        "attempted_iteration_budget",
        "total_iteration_budget",
        "actual_iteration_count",
        "certification",
    ];
    if row.len() != expected.len()
        || !expected.iter().all(|k| row.contains_key(*k))
        || row.get("schema").and_then(Value::as_str) != Some(NUMERICAL_SOLVER_RECORD_SCHEMA)
    {
        return Err("numerical solver record fields are invalid".into());
    }
    if row.get("solve_scope").and_then(Value::as_str) != Some("primal_forward")
        || row.get("initial_residual_scope").and_then(Value::as_str)
            != Some("first_attempt_completion_before_retry")
        || !row.get("actual_iteration_count").is_some_and(Value::is_null)
    {
        return Err("numerical solver record scope is invalid".into());
    }
    let Some(certification) = row.get("certification").and_then(Value::as_object).filter(|c| {
        c.len() == 3
            && c.contains_key("status")
            && c.contains_key("passed")
            && c.contains_key("relative_residual_limit")
    }) else {
        return Err("numerical solver certification fields are invalid".into());
    };
    let text = |k: &str| row.get(k).map(implexity_core::pyobj::py_str).unwrap_or_default();
    let real = |k: &str, label: &str| -> Result<f64, String> {
        match row.get(k) {
            Some(Value::Null) => Ok(f64::NAN),
            Some(v) if v.is_number() => Ok(v.as_f64().unwrap_or(f64::NAN)),
            _ => Err(format!("{label} must be numerical data")),
        }
    };
    let number = |v: Option<&Value>, label: &str| -> Result<f64, String> {
        v.filter(|v| v.is_number())
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("{label} must be positive finite data"))
    };
    let int = |k: &str, label: &str, positive: bool| nonnegative_integer(row.get(k), label, positive);
    let solver_id = text("solver_id");
    let algorithm = text("algorithm");
    let rebuilt = numerical_solver_record(&SolverRecordInput {
        solver_id: &solver_id,
        requested_relative_tolerance: number(
            row.get("requested_relative_tolerance"),
            "requested_relative_tolerance",
        )?,
        initial_relative_residual: real("initial_relative_residual", "initial_relative_residual")?,
        final_relative_residual: real("final_relative_residual", "final_relative_residual")?,
        attempt_count: int("attempt_count", "attempt_count", true)?,
        retry_count: int("retry_count", "retry_count", false)?,
        attempt_limit: int("attempt_limit", "attempt_limit", true)?,
        max_iterations_per_attempt: int("max_iterations_per_attempt", "max_iterations_per_attempt", true)?,
        total_iteration_budget: int("total_iteration_budget", "total_iteration_budget", true)?,
        certification_limit: number(certification.get("relative_residual_limit"), "certification_limit")?,
        algorithm: &algorithm,
    })?;
    if Value::Object(rebuilt.clone()) != Value::Object(row.clone()) {
        return Err("numerical solver record values are inconsistent".into());
    }
    Ok(rebuilt)
}

