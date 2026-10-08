// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

#[must_use]
pub fn py_str(value: &Value) -> String {
    implexity_core::pyobj::py_str(value)
}

#[must_use]
pub fn truthy(value: Option<&Value>) -> bool {
    value.is_some_and(implexity_core::pyobj::truthy)
}

#[must_use]
pub fn text_or(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(v) if implexity_core::pyobj::truthy(v) => py_str(v),
        _ => default.to_string(),
    }
}

#[must_use]
pub fn py_repr_value(value: &Value) -> String {
    implexity_core::pyobj::repr(value)
}

#[must_use]
pub fn mapping_or_empty(value: Option<&Value>) -> serde_json::Map<String, Value> {
    match value {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    }
}
