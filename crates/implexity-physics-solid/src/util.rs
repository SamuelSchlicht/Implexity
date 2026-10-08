// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value};

use implexity_core::CaeError;


pub fn contract<T>(message: impl Into<String>) -> Result<T, CaeError> {
    Err(CaeError::contract(message.into()))
}


pub fn convergence<T>(message: impl Into<String>) -> Result<T, CaeError> {
    Err(CaeError::convergence(message.into()))
}

#[must_use]
pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

#[must_use]
pub fn f(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

#[must_use]
pub fn text(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}

#[must_use]
pub fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

#[must_use]
pub fn real_array(v: &Value) -> Option<(Vec<usize>, Vec<f64>)> {
    fn walk(v: &Value, depth: usize, shape: &mut Vec<usize>, data: &mut Vec<f64>) -> bool {
        match v {
            Value::Array(items) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape.len() < depth || shape[depth] != items.len() {
                    return false;
                }
                items.iter().all(|item| walk(item, depth + 1, shape, data))
            }
            Value::Number(n) => {
                if shape.len() != depth {
                    return false;
                }
                data.push(n.as_f64().unwrap_or(f64::NAN));
                true
            }
            _ => false,
        }
    }
    let mut shape = Vec::new();
    let mut data = Vec::new();
    walk(v, 0, &mut shape, &mut data).then_some((shape, data))
}

#[must_use]
pub fn nested(shape: &[usize], data: &[f64]) -> Value {
    if shape.is_empty() {
        return float(data.first().copied().unwrap_or(f64::NAN));
    }
    let stride: usize = shape[1..].iter().product();
    Value::Array((0..shape[0]).map(|i| nested(&shape[1..], &data[i * stride..(i + 1) * stride])).collect())
}

#[must_use]
pub fn float(x: f64) -> Value {
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn floats(xs: &[f64]) -> Value {
    Value::Array(xs.iter().map(|x| float(*x)).collect())
}

#[must_use]
pub fn sorted_repr<'a>(keys: impl IntoIterator<Item = &'a str>) -> String {
    let mut v: Vec<&str> = keys.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    implexity_core::pyobj::list_repr(&v)
}

#[must_use]
pub fn key_set(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    keys.sort();
    keys
}

#[must_use]
pub fn has_exact_keys(v: &Value, keys: &[&str]) -> bool {
    v.as_object().is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
}

#[must_use]
pub fn grid_cells(context: &Value) -> usize {
    context["grid"].as_array().map_or(0, |g| g.iter().map(count).product())
}

#[must_use]
pub fn time_count(context: &Value) -> usize {
    context["times_s"].as_array().map_or(0, Vec::len)
}

#[must_use]
pub fn times(context: &Value) -> Vec<f64> {
    context["times_s"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect())
        .unwrap_or_default()
}

#[must_use]
pub fn minimum_yield(m: &Value) -> f64 {
    if let Some(t) = m.get("temperature_table") {
        return t["yield_stress"]
            .as_array()
            .map_or(f64::NAN, |a| a.iter().filter_map(Value::as_f64).fold(f64::INFINITY, f64::min));
    }
    let slope = f(&m["temperature_slopes"], "yield_stress");
    [f(m, "T_min"), f(m, "T_max")]
        .iter()
        .map(|t| f(m, "yield_stress") + slope * (t - f(m, "T_ref")))
        .fold(f64::INFINITY, f64::min)
}

#[must_use]
pub fn bool_array(v: &Value) -> Option<(Vec<usize>, Vec<bool>)> {
    fn walk(v: &Value, depth: usize, shape: &mut Vec<usize>, data: &mut Vec<bool>) -> bool {
        match v {
            Value::Array(items) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape.len() < depth || shape[depth] != items.len() {
                    return false;
                }
                items.iter().all(|item| walk(item, depth + 1, shape, data))
            }
            Value::Bool(b) => {
                if shape.len() != depth {
                    return false;
                }
                data.push(*b);
                true
            }
            _ => false,
        }
    }
    let mut shape = Vec::new();
    let mut data = Vec::new();
    walk(v, 0, &mut shape, &mut data).then_some((shape, data))
}

#[must_use]
pub fn int_array(v: &Value) -> Option<(Vec<usize>, Vec<i64>)> {
    let (shape, data) = real_array(v)?;
    let ints = flatten_json(v);
    if ints.iter().any(|x| !(x.is_i64() || x.is_u64())) {
        return None;
    }
    let _ = data;
    Some((shape, ints.iter().map(|x| x.as_i64().unwrap_or(i64::MAX)).collect()))
}

fn flatten_json(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(items) => items.iter().flat_map(flatten_json).collect(),
        other => vec![other],
    }
}

#[must_use]
pub fn f64_shaped(v: &Value, shape: &[usize]) -> Option<Vec<f64>> {
    real_array(v).filter(|(s, _)| s == shape).map(|(_, d)| d)
}

pub fn jnp_interp<S: implexity_ad::Scalar>(x: S, xp: &[S], fp: &[S], left: Option<S>, right: Option<S>) -> S {
    let n = xp.len();
    let xv = x.value();
    let i = xp.partition_point(|v| v.value() <= xv).clamp(1, n - 1);
    let df = fp[i] - fp[i - 1];
    let dx = xp[i] - xp[i - 1];
    let delta = x - xp[i - 1];

    let spacing = f64::EPSILON * 2f64.powi(-52);
    let f = if dx.value().abs() <= spacing { fp[i - 1] } else { fp[i - 1] + delta / dx * df };
    if xv < xp[0].value() {
        return left.unwrap_or(fp[0]);
    }
    if xv > xp[n - 1].value() {
        return right.unwrap_or(fp[n - 1]);
    }
    f
}

pub fn tie_min<S: implexity_ad::Scalar>(values: &[S]) -> S {
    tie_extremum(values, |a, b| a < b)
}

pub fn tie_max<S: implexity_ad::Scalar>(values: &[S]) -> S {
    tie_extremum(values, |a, b| a > b)
}

fn tie_extremum<S: implexity_ad::Scalar>(values: &[S], better: impl Fn(f64, f64) -> bool) -> S {
    let mut best = values[0].value();
    for v in values {
        if better(v.value(), best) || v.value().is_nan() {
            best = v.value();
        }
    }
    #[allow(clippy::float_cmp)]
    let ties: Vec<&S> = values.iter().filter(|v| v.value() == best).collect();
    if ties.is_empty() {
        return S::from_f64(best);
    }

    let count = ties.len() as f64;
    let mut spread = S::zero();
    for v in ties {
        spread += *v - best;
    }
    spread / count + best
}

#[must_use]
pub fn argmin(values: &[f64]) -> usize {
    let mut best = 0;
    for (i, v) in values.iter().enumerate() {
        if *v < values[best] || (v.is_nan() && !values[best].is_nan()) {
            best = i;
        }
    }
    best
}

#[must_use]
pub fn format_e(x: f64, precision: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".into()
        } else if x > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let s = format!("{x:.precision$e}");
    match s.split_once('e') {
        Some((m, e)) => {
            let (sign, digits) = if let Some(d) = e.strip_prefix('-') { ("-", d) } else { ("+", e) };
            format!("{m}e{sign}{digits:0>2}")
        }
        None => s,
    }
}

pub fn pow10<S: implexity_ad::Scalar>(y: S) -> S {
    let value = 10f64.powf(y.value());
    let ln10 = std::f64::consts::LN_10;
    y.chain(value, ln10 * value, ln10 * ln10 * value)
}

#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn count(v: &Value) -> usize {
    v.as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or_else(|| {
        v.as_f64().filter(|x| x.is_finite()).map_or(0, |x| x.clamp(0.0, 9_007_199_254_740_992.0) as usize)
    })
}
