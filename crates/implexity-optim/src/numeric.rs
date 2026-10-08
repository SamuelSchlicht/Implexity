// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Number, Value};

#[must_use]
pub fn np_sum(a: &[f64]) -> f64 {
    const BLOCK: usize = 128;
    let n = a.len();
    if n == 0 {
        return 0.0;
    }
    if n < 8 {
        let mut res = -0.0_f64;
        for &v in a {
            res += v;
        }
        return res;
    }
    if n <= BLOCK {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for (k, slot) in r.iter_mut().enumerate() {
                *slot += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        return res;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    np_sum(&a[..n2]) + np_sum(&a[n2..])
}

#[must_use]
pub fn array_sum(a: &ArrayD<f64>) -> f64 {
    if let Some(s) = a.as_slice() {
        np_sum(s)
    } else {
        let owned: Vec<f64> = a.iter().copied().collect();
        np_sum(&owned)
    }
}

#[must_use]
pub fn array_mean(a: &ArrayD<f64>) -> f64 {
    if a.is_empty() {
        return f64::NAN;
    }
    array_sum(a) / a.len() as f64
}

#[must_use]
pub fn max_abs(a: &ArrayD<f64>) -> f64 {
    a.iter().fold(0.0_f64, |m, v| {
        let x = v.abs();
        if x > m || x.is_nan() { x } else { m }
    })
}

#[must_use]
pub fn float_value(x: f64) -> Value {
    Number::from_f64(x).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn array_to_value(a: &ArrayD<f64>) -> Value {
    fn nest(shape: &[usize], data: &mut dyn Iterator<Item = f64>) -> Value {
        match shape.split_first() {
            None => data.next().map_or(Value::Null, float_value),
            Some((&n, rest)) => Value::Array((0..n).map(|_| nest(rest, data)).collect()),
        }
    }
    let shape = a.shape().to_vec();
    let mut it = a.iter().copied();
    nest(&shape, &mut it)
}

#[must_use]
pub fn bool_array_to_value(a: &ArrayD<bool>) -> Value {
    fn nest(shape: &[usize], data: &mut dyn Iterator<Item = bool>) -> Value {
        match shape.split_first() {
            None => data.next().map_or(Value::Null, Value::Bool),
            Some((&n, rest)) => Value::Array((0..n).map(|_| nest(rest, data)).collect()),
        }
    }
    let shape = a.shape().to_vec();
    let mut it = a.iter().copied();
    nest(&shape, &mut it)
}

#[must_use]
pub fn float_list(values: &[f64]) -> Value {
    Value::Array(values.iter().copied().map(float_value).collect())
}

#[must_use]
pub fn vector(values: Vec<f64>) -> ArrayD<f64> {
    let n = values.len();
    ArrayD::from_shape_vec(IxDyn(&[n]), values).unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])))
}

#[must_use]
pub fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

#[must_use]
pub fn str_list_repr<S: AsRef<str>>(items: &[S]) -> String {
    let inner: Vec<String> = items.iter().map(|s| implexity_core::py_repr::repr_str(s.as_ref())).collect();
    format!("[{}]", inner.join(", "))
}

#[must_use]
pub fn shape_repr(shape: &[usize]) -> String {
    match shape {
        [] => "()".to_string(),
        [n] => format!("({n},)"),
        _ => format!("({})", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

#[must_use]
pub fn format_g6(x: f64) -> String {
    format_g(x, 6)
}

#[must_use]
pub fn format_g(x: f64, precision: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let p = precision.max(1);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{:.*e}", p - 1, x);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let p_i = i32::try_from(p).unwrap_or(i32::MAX);
    if exp < -4 || exp >= p_i {
        let m = trim_fraction(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{m}e{sign}{:02}", exp.abs())
    } else {
        let decimals = usize::try_from(p_i - 1 - exp).unwrap_or(0);
        trim_fraction(&format!("{x:.decimals$}")).to_string()
    }
}

fn trim_fraction(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

#[must_use]
pub fn format_e(x: f64, precision: usize) -> String {
    if !x.is_finite() {
        return format_g(x, 6);
    }
    let s = format!("{x:.precision$e}");
    let (m, e) = s.split_once('e').unwrap_or((s.as_str(), "0"));
    let e: i32 = e.parse().unwrap_or(0);
    let sign = if e < 0 { '-' } else { '+' };
    format!("{m}e{sign}{:02}", e.abs())
}

