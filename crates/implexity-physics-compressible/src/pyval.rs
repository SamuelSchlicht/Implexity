// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Number, Value};
pub use implexity_core::pyobj::{py_str, repr, truthy};

#[must_use]
pub fn strip(s: &str) -> &str {
    s.trim_matches(char::is_whitespace)
}

fn underscores_ok(s: &str) -> bool {

    let b = s.as_bytes();
    for (i, c) in b.iter().enumerate() {
        if *c == b'_' {
            let prev = i.checked_sub(1).map(|j| b[j]);
            let next = b.get(i + 1).copied();
            if !prev.is_some_and(|p| p.is_ascii_digit()) || !next.is_some_and(|n| n.is_ascii_digit()) {
                return false;
            }
        }
    }
    true
}

#[must_use]
pub fn parse_float(text: &str) -> Option<f64> {
    let t = strip(text);
    if t.is_empty() || !underscores_ok(t) {
        return None;
    }
    let cleaned: String = t.chars().filter(|c| *c != '_').collect();
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if body.len() + 1 < lower.len() {
        return None;
    }
    let negative = lower.starts_with('-');
    match body {
        "inf" | "infinity" => return Some(if negative { f64::NEG_INFINITY } else { f64::INFINITY }),
        "nan" => return Some(f64::NAN),
        _ => {}
    }
    if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-')) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

#[must_use]
pub fn parse_int(text: &str) -> Option<i64> {
    let t = strip(text);
    if t.is_empty() || !underscores_ok(t) {
        return None;
    }
    let cleaned: String = t.chars().filter(|c| *c != '_').collect();
    let digits = cleaned.trim_start_matches(['+', '-']);
    if digits.is_empty() || cleaned.len() - digits.len() > 1 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    cleaned.parse::<i64>().ok()
}

#[must_use]
pub fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_float(s),
        _ => None,
    }
}

#[must_use]
pub fn py_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                let f = n.as_f64()?;
                #[allow(clippy::cast_possible_truncation)]
                (f.is_finite() && f.abs() < 9.2e18).then(|| f.trunc() as i64)
            }
        }
        Value::String(s) => parse_int(s),
        _ => None,
    }
}

#[must_use]
pub fn is_real_number(value: &Value) -> bool {
    matches!(value, Value::Number(_))
}

#[must_use]
pub fn num(x: f64) -> Value {
    Number::from_f64(x).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn nums(xs: &[f64]) -> Value {
    Value::Array(xs.iter().map(|x| num(*x)).collect())
}

#[must_use]
pub fn strs<S: AsRef<str>>(xs: &[S]) -> Value {
    Value::Array(xs.iter().map(|x| Value::String(x.as_ref().to_string())).collect())
}

#[must_use]
pub fn obj_or_empty(value: Option<&Value>) -> Map<String, Value> {
    value.and_then(Value::as_object).cloned().unwrap_or_default()
}

#[must_use]
pub fn get<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    m.get(key).filter(|v| !v.is_null())
}

#[must_use]
pub fn fmt_e(x: f64, prec: usize) -> String {
    if !x.is_finite() {
        return fmt_nonfinite(x);
    }
    let s = format!("{x:.prec$e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

#[must_use]
pub fn fmt_upper_e(x: f64, prec: usize) -> String {
    if !x.is_finite() {
        return fmt_nonfinite(x).to_uppercase();
    }
    fmt_e(x, prec).to_uppercase()
}

#[must_use]
pub fn fmt_g(x: f64, prec: usize) -> String {
    implexity_core::extensions::format_g(x, prec)
}

#[must_use]
pub fn fmt_g6(x: f64) -> String {
    fmt_g(x, 6)
}

#[must_use]
pub fn fmt_f(x: f64, prec: usize) -> String {
    if !x.is_finite() {
        return fmt_nonfinite(x);
    }
    format!("{x:.prec$}")
}

fn fmt_nonfinite(x: f64) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x > 0.0 {
        "inf".into()
    } else {
        "-inf".into()
    }
}

#[must_use]
pub fn repr_float(x: f64) -> String {
    implexity_core::py_repr::repr_float(x)
}

#[must_use]
pub fn shape_repr(shape: &[usize]) -> String {
    match shape {
        [] => "()".into(),
        [n] => format!("({n},)"),
        _ => format!("({})", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

#[must_use]
pub fn list_repr<S: AsRef<str>>(items: &[S]) -> String {
    implexity_core::pyobj::list_repr(items)
}
