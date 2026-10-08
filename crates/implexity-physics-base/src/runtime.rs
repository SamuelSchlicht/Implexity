// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use implexity_ad::Scalar;
use implexity_core::extensions::format_g6;
use implexity_core::py_repr::repr_float;

use crate::model_errors::{PhysicsError, PhysicsResult};
use crate::models::{ModelOutputs, ModelValue};

const MARKERS: [&str; 2] = ["margin", "validity"];

fn is_margin(key: &str) -> bool {
    let lk = key.to_lowercase();
    MARKERS.iter().any(|m| lk.contains(m))
}

fn min_or_nan(values: &[f64]) -> f64 {
    if values.is_empty() || !values.iter().all(Scalar::is_finite) {
        return f64::NAN;
    }
    values.iter().copied().fold(f64::INFINITY, f64::min)
}

fn json_margin(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(b) => f64::from(u8::from(*b)),
        other => crate::array::Tensor::from_json(other).map_or(f64::NAN, |t| min_or_nan(t.data())),
    }
}

#[must_use]
pub fn json_margins(payload: &serde_json::Map<String, Value>) -> Vec<(String, f64)> {
    payload.iter().filter(|(k, _)| is_margin(k)).map(|(k, v)| (k.clone(), json_margin(v))).collect()
}

#[must_use]
pub fn output_margins<S: Scalar>(payload: &ModelOutputs<S>) -> Vec<(String, f64)> {
    payload
        .iter()
        .filter(|(k, _)| is_margin(k))
        .map(|(k, v)| {
            let m = match v {
                ModelValue::Array(t) if t.ndim() == 0 => t.at(0).value(),
                ModelValue::Array(t) => min_or_nan(&t.values().into_data()),
                ModelValue::Text(_) => f64::NAN,
            };
            (k.to_string(), m)
        })
        .collect()
}


pub fn enforce_runtime_validity(
    margins: &[(String, f64)],
    addin_id: &str,
    tolerance: f64,
    nonnegative_margins: &[&str],
) -> PhysicsResult<()> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(PhysicsError::value("validity tolerance must be finite and nonnegative"));
    }
    for (key, value) in margins {
        if !value.is_finite() {
            return Err(PhysicsError::value(format!(
                "{addin_id}: non-finite validity margin {key}={}",
                repr_float(*value)
            )));
        }
        let crossed = if nonnegative_margins.contains(&key.as_str()) {
            *value < tolerance
        } else {
            *value <= tolerance
        };
        if crossed {
            return Err(PhysicsError::value(format!(
                "{addin_id}: validity boundary crossed for {key}: {}",
                format_g6(*value)
            )));
        }
    }
    Ok(())
}

