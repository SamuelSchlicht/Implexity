// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Float,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NdArray {
    pub shape: Vec<usize>,
    pub kind: Kind,
    pub data: Vec<f64>,
}

fn promote(a: Kind, b: Kind) -> Kind {
    match (a, b) {
        (Kind::Other, _) | (_, Kind::Other) => Kind::Other,
        (Kind::Float, _) | (_, Kind::Float) => Kind::Float,
        (Kind::Int, _) | (_, Kind::Int) => Kind::Int,
        (Kind::Bool, Kind::Bool) => Kind::Bool,
    }
}

fn shape_of(value: &Value) -> Option<Vec<usize>> {
    match value {
        Value::Array(items) => {
            let mut inner: Option<Vec<usize>> = None;
            for item in items {
                let s = shape_of(item)?;
                match &inner {
                    None => inner = Some(s),
                    Some(prev) if *prev == s => {}
                    Some(_) => return None,
                }
            }
            let mut shape = vec![items.len()];
            shape.extend(inner.unwrap_or_default());
            Some(shape)
        }
        _ => Some(Vec::new()),
    }
}

fn collect(value: &Value, kind: &mut Option<Kind>, out: &mut Vec<f64>) {
    let (k, v) = match value {
        Value::Array(items) => {
            for item in items {
                collect(item, kind, out);
            }
            return;
        }
        Value::Bool(b) => (Kind::Bool, if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                #[allow(clippy::cast_precision_loss)]
                let v = n.as_i64().map_or_else(|| n.as_u64().map_or(f64::NAN, |u| u as f64), |i| i as f64);
                (Kind::Int, v)
            } else {
                (Kind::Float, n.as_f64().unwrap_or(f64::NAN))
            }
        }
        _ => (Kind::Other, f64::NAN),
    };
    *kind = Some(kind.map_or(k, |prev| promote(prev, k)));
    out.push(v);
}

impl NdArray {
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let Some(shape) = shape_of(value) else {
            return Self { shape: Vec::new(), kind: Kind::Other, data: Vec::new() };
        };
        let mut kind = None;
        let mut data = Vec::new();
        collect(value, &mut kind, &mut data);

        Self { shape, kind: kind.unwrap_or(Kind::Float), data }
    }

    #[must_use]
    pub fn is_real(&self) -> bool {
        matches!(self.kind, Kind::Int | Kind::Float)
    }

    #[must_use]
    pub fn is_bool(&self) -> bool {
        self.kind == Kind::Bool
    }

    #[must_use]
    pub fn all_finite(&self) -> bool {
        self.data.iter().all(|v| v.is_finite())
    }

    #[must_use]
    pub fn is_scalar(&self) -> bool {
        self.shape.is_empty()
    }

    #[must_use]
    pub fn has_shape(&self, shape: &[usize]) -> bool {
        self.shape == shape
    }

    #[must_use]
    pub fn real_scalar(&self) -> Option<f64> {
        (self.is_scalar() && self.is_real() && self.all_finite()).then(|| self.data[0])
    }

    #[must_use]
    pub fn bools(&self) -> Vec<bool> {
        self.data.iter().map(|v| *v != 0.0).collect()
    }
}

#[must_use]
pub fn asarray(value: &Value) -> NdArray {
    NdArray::from_value(value)
}

#[must_use]
pub fn py_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if n.is_i64() => n.as_i64(),
        Value::Number(n) if n.is_u64() => n.as_u64().and_then(|u| i64::try_from(u).ok()),
        _ => None,
    }
}

#[must_use]
pub fn py_real(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

#[must_use]
pub fn to_nested(values: &[f64], shape: &[usize]) -> Value {
    fn build(values: &[f64], shape: &[usize]) -> Value {
        match shape.split_first() {
            None => serde_json::json!(values[0]),
            Some((&n, rest)) => {
                let stride: usize = rest.iter().product();
                Value::Array((0..n).map(|i| build(&values[i * stride..(i + 1) * stride], rest)).collect())
            }
        }
    }
    if shape.is_empty() {
        return serde_json::json!(values.first().copied().unwrap_or(0.0));
    }
    build(values, shape)
}

#[must_use]
pub fn to_nested_bool(values: &[bool], shape: &[usize]) -> Value {
    fn build(values: &[bool], shape: &[usize]) -> Value {
        match shape.split_first() {
            None => Value::Bool(values[0]),
            Some((&n, rest)) => {
                let stride: usize = rest.iter().product();
                Value::Array((0..n).map(|i| build(&values[i * stride..(i + 1) * stride], rest)).collect())
            }
        }
    }
    build(values, shape)
}

