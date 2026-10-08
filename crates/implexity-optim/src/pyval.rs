// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{py_str, truthy};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

#[must_use]
pub fn first_truthy<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().filter_map(|k| map.get(*k)).find(|v| truthy(v))
}

#[must_use]
pub fn text_or(map: &Map<String, Value>, keys: &[&str], default: &str) -> String {
    first_truthy(map, keys).map_or_else(|| default.to_string(), py_str).trim().to_string()
}

#[must_use]
pub fn raw_text_or(map: &Map<String, Value>, keys: &[&str], default: &str) -> String {
    first_truthy(map, keys).map_or_else(|| default.to_string(), py_str)
}


pub fn str_tuple(value: &Value, label: &str) -> CaeResult<Vec<String>> {
    match value {
        Value::Array(items) => Ok(items.iter().map(py_str).collect()),
        Value::String(s) => Ok(s.chars().map(|c| c.to_string()).collect()),
        Value::Object(m) => Ok(m.keys().cloned().collect()),
        Value::Null => Ok(Vec::new()),
        _ => Err(CaeError::contract(format!("{label} must be iterable"))),
    }
}


pub fn py_int(value: &Value) -> CaeResult<i64> {
    match value {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            let f = n.as_f64().unwrap_or(f64::NAN);
            if f.is_nan() {
                return Err(CaeError::contract("cannot convert float NaN to integer"));
            }
            if f.is_infinite() {
                return Err(CaeError::contract("cannot convert float infinity to integer"));
            }
            #[allow(clippy::cast_possible_truncation)]
            let t = f.trunc() as i64;
            Ok(t)
        }
        Value::String(s) => {
            let t = s.trim();
            let cleaned: String = t.chars().filter(|c| *c != '_').collect();
            cleaned.parse::<i64>().map_err(|_| {
                CaeError::contract(format!("invalid literal for int() with base 10: {}", repr_str(s)))
            })
        }
        Value::Null => Err(CaeError::contract(
            "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'",
        )),
        Value::Array(_) => Err(CaeError::contract(
            "int() argument must be a string, a bytes-like object or a real number, not 'list'",
        )),
        Value::Object(_) => Err(CaeError::contract(
            "int() argument must be a string, a bytes-like object or a real number, not 'dict'",
        )),
    }
}


pub fn py_float(value: &Value) -> CaeResult<f64> {
    match value {
        Value::Null => {
            Err(CaeError::contract("float() argument must be a string or a real number, not 'NoneType'"))
        }
        Value::Array(_) => {
            Err(CaeError::contract("float() argument must be a string or a real number, not 'list'"))
        }
        Value::Object(_) => {
            Err(CaeError::contract("float() argument must be a string or a real number, not 'dict'"))
        }
        other => implexity_core::orchestration::py_float(other),
    }
}

#[must_use]
pub fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

#[must_use]
pub fn finite_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    }
}

