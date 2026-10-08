// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("derivative request rejected:\n  {}", problems.join("\n  "))]
pub struct DerivativeError {
    pub problems: Vec<String>,
}

fn reject(p: &str) -> DerivativeError {
    DerivativeError { problems: vec![p.to_string()] }
}

#[must_use]
pub fn catalogue() -> Value {
    json!({
        "kind": "implicit_derivative_operators", "schema": "implexity-derivatives/1",
        "operators": [
            {"id": "jacobian", "input": "selected design parameters", "output": "selected engineering responses",
             "action": "full response Jacobian for small/medium selected design sets"},
            {"id": "jvp", "input": "design-space tangent", "output": "response-space tangent",
             "action": "matrix-free forward directional derivative J v"},
            {"id": "vjp", "input": "response-space cotangent", "output": "design-space cotangent",
             "action": "matrix-free reverse pullback J^T w"},
        ],
        "note": "All operators act on the same coupled response function used by sensitivity and optimisation.",
    })
}


pub fn normalise(req: &Value) -> Result<Value, DerivativeError> {
    let Some(m) = req.as_object() else { return Err(reject("request must be an object")) };
    let op = match m.get("operator") {
        None | Some(Value::Null) => "jacobian".to_string(),
        Some(Value::String(s)) if s.is_empty() => "jacobian".to_string(),
        Some(Value::Bool(false)) => "jacobian".to_string(),
        Some(Value::String(s)) => s.to_lowercase(),
        Some(other) => crate::document::py_str(other).to_lowercase(),
    };
    if !["jacobian", "jvp", "vjp"].contains(&op.as_str()) {
        return Err(reject("operator must be jacobian, jvp or vjp"));
    }
    let mut out = m.clone();
    out.insert("operator".into(), json!(op));
    if op == "jvp" && !m.get("tangent").is_some_and(Value::is_object) {
        return Err(reject("jvp needs tangent={parameter_ref: scalar-or-array, ...}"));
    }
    if op == "vjp" && !m.get("cotangent").is_some_and(Value::is_object) {
        return Err(reject("vjp needs cotangent={response_name: weight, ...}"));
    }
    Ok(Value::Object(out))
}
