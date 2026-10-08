// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::error::{CaeError, CaeResult};
use crate::py_repr::{PyValue, repr_float, repr_str};

#[must_use]
pub fn repr(value: &Value) -> String {
    PyValue::from_json(value).repr()
}

#[must_use]
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

#[must_use]
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

#[must_use]
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64(), x.as_u64(), y.as_u64()) {
            (Some(p), Some(q), _, _) => p == q,
            (_, _, Some(p), Some(q)) => p == q,
            #[allow(clippy::float_cmp)]
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => {
            #[allow(clippy::float_cmp)]
            let eq = numeric(a) == numeric(b);
            eq
        }
        _ => a == b,
    }
}

#[must_use]
pub fn list_repr<S: AsRef<str>>(items: &[S]) -> String {
    let inner: Vec<String> = items.iter().map(|s| repr_str(s.as_ref())).collect();
    format!("[{}]", inner.join(", "))
}

#[must_use]
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[must_use]
pub fn is_real(value: &Value) -> bool {
    matches!(value, Value::Number(_))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PyNum {
    Int(i64),
    Float(f64),
}

impl PyNum {
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }

    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Some(Self::Int(i))
                } else if n.is_f64() {
                    n.as_f64().map(Self::Float)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn to_value(self) -> Value {
        match self {
            Self::Int(i) => Value::Number(Number::from(i)),
            Self::Float(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        }
    }

    #[must_use]
    pub fn repr(self) -> String {
        match self {
            Self::Int(i) => i.to_string(),
            Self::Float(f) => repr_float(f),
        }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        match self {
            Self::Int(_) => true,
            Self::Float(f) => f.is_finite(),
        }
    }
}

impl From<f64> for PyNum {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<i64> for PyNum {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

impl Serialize for PyNum {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Int(i) => serializer.serialize_i64(*i),
            Self::Float(f) => serializer.serialize_f64(*f),
        }
    }
}

impl<'de> Deserialize<'de> for PyNum {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        Self::from_value(&v).ok_or_else(|| serde::de::Error::custom("expected a number"))
    }
}

pub trait MapExt {
    fn unknown_keys(&self, allowed: &[&str]) -> Vec<String>;
    fn missing_keys(&self, required: &[&str]) -> Vec<String>;
}

impl MapExt for Map<String, Value> {
    fn unknown_keys(&self, allowed: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = self.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
        out.sort();
        out
    }

    fn missing_keys(&self, required: &[&str]) -> Vec<String> {
        let mut out: Vec<String> =
            required.iter().filter(|k| !self.contains_key(**k)).map(|k| (*k).to_string()).collect();
        out.sort();
        out
    }
}


pub fn require_object<'a>(value: &'a Value, message: &str) -> CaeResult<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| CaeError::contract(message))
}

#[must_use]
pub fn str_list(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|a| a.iter().map(py_str).collect())
}

#[must_use]
pub fn text_list(value: &Value) -> Option<Vec<String>> {
    value.as_array().and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
}

