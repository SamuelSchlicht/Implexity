// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

