// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use ndarray::{ArrayD, IxDyn};
use serde_json::Value;
use crate::pyval::num;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Field {
    pub shape: Vec<usize>,
    pub values: Vec<f64>,
}

impl Field {
    #[must_use]
    pub fn new(shape: Vec<usize>, values: Vec<f64>) -> Self {
        Self { shape, values }
    }

    #[must_use]
    pub fn full(shape: &[usize], value: f64) -> Self {
        Self { shape: shape.to_vec(), values: vec![value; shape.iter().product()] }
    }

    #[must_use]
    pub fn from_array(a: &ArrayD<f64>) -> Self {
        Self { shape: a.shape().to_vec(), values: a.iter().copied().collect() }
    }

    #[must_use]
    pub fn to_array(&self) -> ArrayD<f64> {
        ArrayD::from_shape_vec(IxDyn(&self.shape), self.values.clone())
            .unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])))
    }

    #[must_use]
    pub fn to_nested(&self) -> Value {
        nested(&self.shape, &self.values)
    }

    #[must_use]
    pub fn from_nested(value: &Value) -> Option<Self> {
        let mut shape = Vec::new();
        let mut cur = value;
        while let Value::Array(a) = cur {
            shape.push(a.len());
            match a.first() {
                Some(first) => cur = first,
                None => break,
            }
        }
        let mut values = Vec::new();
        if !collect(value, &shape, 0, &mut values) {
            return None;
        }
        Some(Self { shape, values })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

fn collect(value: &Value, shape: &[usize], depth: usize, out: &mut Vec<f64>) -> bool {
    if depth == shape.len() {
        return match value {
            Value::Number(n) => n.as_f64().is_some_and(|v| {
                out.push(v);
                true
            }),
            Value::Bool(b) => {
                out.push(if *b { 1.0 } else { 0.0 });
                true
            }
            _ => false,
        };
    }
    match value {
        Value::Array(a) if a.len() == shape[depth] => a.iter().all(|v| collect(v, shape, depth + 1, out)),
        _ => false,
    }
}

#[must_use]
pub fn nested(shape: &[usize], values: &[f64]) -> Value {
    if shape.is_empty() {
        return values.first().map_or(Value::Null, |v| num(*v));
    }
    let inner: usize = shape[1..].iter().product();
    Value::Array((0..shape[0]).map(|i| nested(&shape[1..], &values[i * inner..(i + 1) * inner])).collect())
}

#[must_use]
pub fn nested_bool(shape: &[usize], values: &[bool]) -> Value {
    if shape.is_empty() {
        return values.first().map_or(Value::Null, |v| Value::Bool(*v));
    }
    let inner: usize = shape[1..].iter().product();
    Value::Array(
        (0..shape[0]).map(|i| nested_bool(&shape[1..], &values[i * inner..(i + 1) * inner])).collect(),
    )
}

#[must_use]
pub fn bool_mask(value: &Value) -> Option<(Vec<usize>, Vec<bool>)> {
    Field::from_nested(value).map(|f| (f.shape, f.values.iter().map(|v| *v != 0.0).collect()))
}

#[must_use]
pub fn is_bool_array(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::Array(a) => a.iter().all(is_bool_array),
        _ => false,
    }
}
