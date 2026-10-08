// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use ndarray::{ArrayD, IxDyn};
use serde_json::Value;

use crate::error::{CaeError, CaeResult};
use crate::orchestration::py_float;


pub fn require_finite(values: &[f64], label: &str) -> CaeResult<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(CaeError::contract(format!("{label} must contain only finite values")))
    }
}


pub fn require_finite_array(values: &ArrayD<f64>, label: &str) -> CaeResult<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(CaeError::contract(format!("{label} must contain only finite values")))
    }
}

fn shape_of(value: &Value, out: &mut Vec<usize>) {
    if let Value::Array(items) = value {
        out.push(items.len());
        if let Some(first) = items.first() {
            shape_of(first, out);
        }
    }
}

fn flatten(value: &Value, shape: &[usize], label: &str, out: &mut Vec<f64>) -> CaeResult<()> {
    match (value, shape.split_first()) {
        (Value::Array(items), Some((&n, rest))) if items.len() == n => {
            for item in items {
                flatten(item, rest, label, out)?;
            }
            Ok(())
        }
        (Value::Array(_), _) | (_, Some(_)) => {
            Err(CaeError::contract(format!("{label} must be a numeric array")))
        }
        (Value::Object(_) | Value::Null, None) => {
            Err(CaeError::contract(format!("{label} must be a real numeric array")))
        }
        (v, None) => {
            out.push(
                py_float(v)
                    .map_err(|_| CaeError::contract(format!("{label} must be a real numeric array")))?,
            );
            Ok(())
        }
    }
}


pub fn real_array(value: &Value, label: &str) -> CaeResult<ArrayD<f64>> {
    let mut shape = Vec::new();
    shape_of(value, &mut shape);
    let mut data = Vec::new();
    flatten(value, &shape, label, &mut data)?;
    require_finite(&data, label)?;
    ArrayD::from_shape_vec(IxDyn(&shape), data)
        .map_err(|_| CaeError::contract(format!("{label} must be a numeric array")))
}


pub fn real_scalar(value: &Value, label: &str) -> CaeResult<f64> {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => Ok(f),
            _ => Err(CaeError::contract(format!("{label} must be a finite real scalar"))),
        },
        _ => Err(CaeError::contract(format!(
            "{label} must be a finite real scalar, not Boolean, text, complex or array data"
        ))),
    }
}


pub fn real_scalar_f64(value: f64, label: &str) -> CaeResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CaeError::contract(format!("{label} must be a finite real scalar")))
    }
}

