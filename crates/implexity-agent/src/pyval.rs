// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

#[must_use]
pub fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

#[must_use]
pub fn is_number(v: &Value) -> bool {
    matches!(v, Value::Number(_))
}

#[must_use]
pub fn number(v: &Value) -> Option<f64> {
    v.as_f64()
}

#[must_use]
pub fn type_is_dict(v: &Value) -> bool {
    v.is_object()
}

#[must_use]
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

#[must_use]
pub fn py_str(v: &Value) -> String {
    implexity_core::pyobj::py_str(v)
}

#[must_use]
pub fn text_or(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(x) if truthy(x) => py_str(x),
        _ => default.to_owned(),
    }
}

#[must_use]
pub fn is_job_id(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() == 12 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}

#[must_use]
pub fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[must_use]
pub fn extra_keys<'a>(m: &'a serde_json::Map<String, Value>, allowed: &[&str]) -> Vec<&'a String> {
    m.keys().filter(|k| !allowed.contains(&k.as_str())).collect()
}

#[must_use]
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}
