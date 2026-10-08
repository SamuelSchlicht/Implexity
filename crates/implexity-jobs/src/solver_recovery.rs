// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::cell::{Cell, RefCell};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Value, json};
use crate::error::JobError;

pub const SCHEMA: &str = "implexity-solver-recovery/1";
thread_local! { static ACTIVE: Cell<bool> = const { Cell::new(false) }; static NOTICE: RefCell<Option<Value>> = const { RefCell::new(None) }; }
pub fn clear_notice() { NOTICE.with(|v| *v.borrow_mut() = None); }
pub fn latest_notice() -> Option<Value> { NOTICE.with(|v| v.borrow().clone()) }
struct Guard(bool);
impl Drop for Guard { fn drop(&mut self) { ACTIVE.with(|v| v.set(self.0)); } }

pub trait RetryFailure {
    fn convergence_failure(&self) -> bool;
    fn recovery_report(&self) -> Option<&Value>;
    fn failure_class(&self) -> &str;
    fn failure_message(&self) -> String;
    fn recovered_failure(self, report: Value) -> Self;
}
impl RetryFailure for CaeError {
    fn convergence_failure(&self) -> bool { self.is_convergence() }
    fn recovery_report(&self) -> Option<&Value> { self.solver_recovery() }
    fn failure_class(&self) -> &str { self.python_class() }
    fn failure_message(&self) -> String { self.message().to_string() }
    fn recovered_failure(self, report: Value) -> Self {
        let message = format!("This solve could not finish after one automatic retry. {}", self.message());
        Self::Recovery { cause: Box::new(self), report, message }
    }
}
impl RetryFailure for JobError {
    fn convergence_failure(&self) -> bool {
        matches!(self.python_class(), "CAEConvergenceError" | "CAENewtonConvergenceError")
    }
    fn recovery_report(&self) -> Option<&Value> { self.solver_recovery() }
    fn failure_class(&self) -> &str { self.python_class() }
    fn failure_message(&self) -> String { self.message() }
    fn recovered_failure(self, report: Value) -> Self {
        let message = format!("This solve could not finish after one automatic retry. {}", self.message());
        Self::Recovery { cause: Box::new(self), report, message }
    }
}
fn bounded(text: &str, limit: usize) -> String {
    let plain = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for c in plain.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > limit { break; }
        out.push(c);
    }
    if out.is_empty() { "Unavailable".into() } else { out }
}
fn detail<E: RetryFailure>(error: &E) -> Value {
    json!({"error_type": bounded(error.failure_class(), 128), "reason": bounded(&error.failure_message(), 1536)})
}
fn report(operation: &str, status: &str, first: &Value, last: Option<Value>) -> Value {
    let message = match status {
        "retrying" => "Retrying this solve once with the same settings.",
        "recovered" => "The solve converged on the retry.",
        _ => "The solver needs attention after one automatic retry. Review its settings or boundary conditions before trying again.",
    };
    json!({"schema": SCHEMA, "operation": bounded(operation, 128), "status": status,
        "attempt_count": 2, "retry_count": 1, "max_retries": 1, "settings_changed": false,
        "message": message, "first_failure": first, "final_failure": last})
}
fn emit(report: &Value) {
    NOTICE.with(|v| *v.borrow_mut() = Some(report.clone()));
    let _ = implexity_solve::trace::point("solver_recovery", || {
        let mut fields = serde_json::Map::new();
        fields.insert("solver_recovery".into(), report.clone());
        fields
    });
}
pub fn once<T, E: RetryFailure>(operation: &str, mut solve: impl FnMut() -> Result<T, E>) -> Result<T, E> {
    if ACTIVE.with(Cell::get) { return solve(); }
    let _guard = Guard(ACTIVE.with(|v| v.replace(true)));
    NOTICE.with(|v| *v.borrow_mut() = None);
    let first = match solve() {
        Ok(value) => return Ok(value),
        Err(error) if error.convergence_failure() && error.recovery_report().is_none() => error,
        Err(error) => return Err(error),
    };
    let failure = detail(&first);
    emit(&report(operation, "retrying", &failure, None));
    match solve() {
        Ok(value) => { emit(&report(operation, "recovered", &failure, None)); Ok(value) }
        Err(error) => {
            let evidence = report(operation, "needs_attention", &failure, Some(detail(&error)));
            emit(&evidence);
            Err(error.recovered_failure(evidence))
        }
    }
}
pub fn validate(raw: &Value) -> CaeResult<Value> { implexity_core::error::validate_solver_recovery(raw) }

impl RetryFailure for implexity_optim::search::SearchError {
    fn convergence_failure(&self) -> bool {
        use implexity_optim::search::{SearchError, TrialFailure};
        match self { SearchError::Fatal(e) | SearchError::Trial(TrialFailure::Convergence(e)) => e.is_convergence(), _ => false }
    }
    fn recovery_report(&self) -> Option<&Value> {
        use implexity_optim::search::{SearchError, TrialFailure};
        match self { SearchError::Fatal(e) | SearchError::Trial(TrialFailure::Convergence(e)) => e.solver_recovery(), _ => None }
    }
    fn failure_class(&self) -> &str { CaeError::from(self.clone()).python_class() }
    fn failure_message(&self) -> String { CaeError::from(self.clone()).message().to_string() }
    fn recovered_failure(self, report: Value) -> Self { CaeError::from(self).recovered_failure(report).into() }
}
