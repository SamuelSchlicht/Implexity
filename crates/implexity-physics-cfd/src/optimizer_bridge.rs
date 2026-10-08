// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use crate::error::{CfdError, CfdResult};
use crate::preflight::run_preflight;
use crate::workspace_contract::{CfdProblem, Objective, TOPOLOGY_PARAMETER};

#[derive(Debug, Clone, PartialEq)]
pub struct CfdRunDeclaration {
    pub problem: Value,
    pub free: Vec<String>,
    pub objectives: Vec<Value>,
    pub constraints: Vec<Value>,
}

impl CfdRunDeclaration {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({
            "problem": self.problem,
            "free": self.free,
            "objectives": self.objectives,
            "constraints": self.constraints,
            "topologyAlwaysFree": true,
            "backend": "resolved-cfd-v23",
        })
    }
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => implexity_core::py_repr::PyValue::from_json(other).repr(),
    }
}


pub fn declare(problem: &CfdProblem, free: &[Value], constraints: &[Value]) -> CfdResult<CfdRunDeclaration> {
    let pre = run_preflight(problem, None);
    if !pre.ok {
        return Err(CfdError::Optimization(format!("CFD preflight failed: {}", pre.error_messages("; "))));
    }
    let mut ordered = vec![TOPOLOGY_PARAMETER.to_string()];
    ordered.extend(free.iter().map(py_str).filter(|x| x != TOPOLOGY_PARAMETER));
    let objectives = problem.objectives.iter().map(Objective::as_dict).collect();
    let mut rows = Vec::with_capacity(constraints.len());
    for c in constraints {
        match c {
            Value::Object(m) => rows.push(Value::Object(m.clone())),
            other => {
                return Err(CfdError::Type(format!(
                    "cannot convert dictionary update sequence element #0 to a sequence: {}",
                    py_str(other)
                )));
            }
        }
    }
    Ok(CfdRunDeclaration { problem: problem.as_dict(), free: ordered, objectives, constraints: rows })
}

fn diag_float(diag: &Map<String, Value>, key: &str, default: f64) -> CfdResult<f64> {
    match diag.get(key) {
        None => Ok(default),
        Some(Value::Number(n)) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(other) => Err(CfdError::Type(format!(
            "float() argument must be a string or a real number, not '{}'",
            match other {
                Value::Null => "NoneType",
                Value::Array(_) => "list",
                Value::Object(_) => "dict",
                _ => "str",
            }
        ))),
    }
}

fn nested_shape(v: &Value, shape: &mut Vec<usize>, out: &mut Vec<f64>, depth: usize) -> bool {
    match v {
        Value::Array(items) => {
            if shape.len() == depth {
                shape.push(items.len());
            } else if shape[depth] != items.len() {
                return false;
            }
            items.iter().all(|x| nested_shape(x, shape, out, depth + 1))
        }
        Value::Number(n) => {
            if shape.len() != depth {
                return false;
            }
            out.push(n.as_f64().unwrap_or(f64::NAN));
            true
        }
        _ => false,
    }
}


pub fn validate_gradient_contract(result: &Value, shape: [usize; 3]) -> CfdResult<()> {
    let Some(g) = result.get("gradient") else {
        return Err(CfdError::Optimization("CFD backend did not return a topology gradient".into()));
    };
    let mut gshape = Vec::new();
    let mut values = Vec::new();
    if !nested_shape(g, &mut gshape, &mut values, 0) || gshape.as_slice() != shape.as_slice() {
        let parts: Vec<String> = gshape.iter().map(ToString::to_string).collect();
        let shown =
            if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) };
        return Err(CfdError::Optimization(format!(
            "CFD gradient shape {shown} differs from topology shape ({}, {}, {})",
            shape[0], shape[1], shape[2]
        )));
    }
    if !values.iter().all(|v| v.is_finite()) {
        return Err(CfdError::Optimization("CFD topology gradient contains non-finite values".into()));
    }
    let empty = Map::new();
    let diag = result.get("diagnostics").and_then(Value::as_object).unwrap_or(&empty);
    let converged = diag.get("converged").is_some_and(|v| match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Null => false,
    });
    if !converged {
        return Err(CfdError::Optimization(
            "CFD state did not converge; its gradient is not admissible".into(),
        ));
    }
    if diag_float(diag, "mass_balance_relative", 1.0)? > 1e-6 {
        return Err(CfdError::Optimization(
            "CFD mass balance is insufficient for topology optimization".into(),
        ));
    }
    if diag_float(diag, "adjoint_relative_residual", 1.0)? > 1e-6 {
        return Err(CfdError::Optimization(
            "CFD adjoint residual is insufficient for topology optimization".into(),
        ));
    }
    Ok(())
}

pub trait UniversalOptimizerManager: Send + Sync {
    fn declare(&self, _request: &Value) -> Option<CfdResult<Value>> {
        None
    }
    fn create_job(&self, _request: &Value) -> Option<CfdResult<Value>> {
        None
    }
}


pub fn run_with_universal_optimizer(
    manager: &dyn UniversalOptimizerManager,
    problem: &CfdProblem,
    free: &[Value],
    constraints: &[Value],
    settings: Option<&Map<String, Value>>,
) -> CfdResult<Value> {
    let mut req = declare(problem, free, constraints)?.as_dict();
    if let Value::Object(m) = &mut req {
        m.insert("settings".into(), Value::Object(settings.cloned().unwrap_or_default()));
    }
    if let Some(r) = manager.declare(&req) {
        return r;
    }
    if let Some(r) = manager.create_job(&req) {
        return r;
    }
    Err(CfdError::Optimization("the active optimizer manager does not expose declare/create_job".into()))
}
