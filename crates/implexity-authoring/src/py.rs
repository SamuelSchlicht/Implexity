// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Number, Value};

use crate::error::{AResult, AuthoringError};

pub use implexity_core::pyobj::{py_eq, py_str, repr, truthy, type_name};

pub(crate) fn value_error(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("ValueError", message)
}

pub(crate) fn type_error(message: impl Into<String>) -> AuthoringError {
    AuthoringError::Type(message.into())
}

#[must_use]
pub fn jf(x: f64) -> Value {
    Number::from_f64(x).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn jfs(xs: &[f64]) -> Value {
    Value::Array(xs.iter().map(|x| jf(*x)).collect())
}

#[must_use]
pub fn ju(x: usize) -> Value {
    Value::from(x as u64)
}

fn parse_py_float_text(text: &str) -> Option<f64> {
    let s = text.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let (sign, body) = match lower.as_bytes()[0] {
        b'+' => (1.0, &lower[1..]),
        b'-' => (-1.0, &lower[1..]),
        _ => (1.0, lower.as_str()),
    };
    match body {
        "inf" | "infinity" => return Some(sign * f64::INFINITY),
        "nan" => return Some(f64::NAN),
        _ => {}
    }

    let bytes = body.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            let ok = i > 0
                && i + 1 < bytes.len()
                && bytes[i - 1].is_ascii_digit()
                && bytes[i + 1].is_ascii_digit();
            if !ok {
                return None;
            }
        } else if !(b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-')) {
            return None;
        }
    }
    let cleaned: String = body.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() || cleaned == "." || !cleaned.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    cleaned.parse::<f64>().ok().map(|v| sign * v)
}


pub fn py_float(value: &Value) -> AResult<f64> {
    match value {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => parse_py_float_text(s)
            .ok_or_else(|| value_error(format!("could not convert string to float: {}", repr(value)))),
        other => Err(type_error(format!(
            "float() argument must be a string or a real number, not '{}'",
            type_name(other)
        ))),
    }
}


pub fn py_float_opt(value: Option<&Value>) -> AResult<f64> {
    py_float(value.unwrap_or(&Value::Null))
}


pub fn py_int(value: &Value) -> AResult<i64> {
    match value {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            if let Some(u) = n.as_u64() {
                return i64::try_from(u)
                    .map_err(|_| value_error("Python int too large to convert to C long"));
            }
            let f = n.as_f64().unwrap_or(f64::NAN);
            if f.is_nan() {
                return Err(value_error("cannot convert float NaN to integer"));
            }
            if f.is_infinite() {
                return Err(AuthoringError::value(
                    "OverflowError",
                    "cannot convert float infinity to integer",
                ));
            }
            #[allow(clippy::cast_possible_truncation)]
            Ok(f.trunc() as i64)
        }
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::String(s) => {
            let t = s.trim();
            let cleaned: String = t.chars().filter(|c| *c != '_').collect();
            cleaned
                .parse::<i64>()
                .map_err(|_| value_error(format!("invalid literal for int() with base 10: {}", repr(value))))
        }
        other => Err(type_error(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}


pub fn py_int_opt(value: Option<&Value>) -> AResult<i64> {
    py_int(value.unwrap_or(&Value::Null))
}

#[must_use]
pub fn py_bool(value: Option<&Value>) -> bool {
    value.is_some_and(truthy)
}

#[must_use]
pub fn py_str_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_string(), py_str)
}

#[must_use]
pub fn lower_str(value: Option<&Value>, default: &str) -> String {
    value.map_or_else(|| default.to_string(), py_str).trim().to_lowercase()
}

#[must_use]
pub fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|m| m.get(key))
}

#[must_use]
pub fn get_nn<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    get(value, key).filter(|v| !v.is_null())
}

#[must_use]
pub fn path<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut cur = value;
    for k in keys {
        let next = get(cur, k)?;
        if !truthy(next) {
            return None;
        }
        cur = next;
    }
    Some(cur)
}

#[must_use]
pub fn path_obj<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    path(value, keys).and_then(Value::as_object)
}

#[must_use]
pub fn first<'a>(source: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|n| get(source, n).filter(|v| !v.is_null()))
}

#[must_use]
pub fn get_either<'a>(value: &'a Value, a: &str, b: &str) -> Option<&'a Value> {
    get(value, a).or_else(|| get(value, b))
}


pub fn setdefault_obj<'a>(map: &'a mut Map<String, Value>, key: &str) -> AResult<&'a mut Map<String, Value>> {
    let entry = map.entry(key.to_string()).or_insert_with(|| Value::Object(Map::new()));
    let tname = type_name(entry);
    entry.as_object_mut().ok_or_else(|| {
        AuthoringError::value("AttributeError", format!("'{tname}' object has no attribute 'setdefault'"))
    })
}


pub fn setdefault_list<'a>(map: &'a mut Map<String, Value>, key: &str) -> AResult<&'a mut Vec<Value>> {
    let entry = map.entry(key.to_string()).or_insert_with(|| Value::Array(Vec::new()));
    let tname = type_name(entry);
    entry.as_array_mut().ok_or_else(|| {
        AuthoringError::value("AttributeError", format!("'{tname}' object has no attribute 'append'"))
    })
}


pub fn obj_mut(value: &mut Value) -> AResult<&mut Map<String, Value>> {
    let t = type_name(value);
    value.as_object_mut().ok_or_else(|| type_error(format!("'{t}' object is not a mapping")))
}

#[must_use]
pub fn canonical_unicode(value: &Value) -> String {
    implexity_core::json::dumps(value, &implexity_core::json::DumpOptions::canonical().ascii(false))
}

#[must_use]
pub fn canonical_ascii(value: &Value) -> String {
    implexity_core::json::canonical(value)
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    implexity_core::json::sha256_hex(bytes)
}

#[must_use]
pub fn sha_unicode(value: &Value) -> String {
    sha256_hex(canonical_unicode(value).as_bytes())
}

#[must_use]
pub fn uuid_hex() -> String {
    let bytes = implexity_io::atomic::os_random_bytes(16).unwrap_or_else(|_| fallback_random(16));
    let mut b = [0u8; 16];
    b.copy_from_slice(&bytes[..16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    hex::encode(b)
}

#[must_use]
pub fn token_hex(n: usize) -> String {
    hex::encode(implexity_io::atomic::os_random_bytes(n).unwrap_or_else(|_| fallback_random(n)))
}

fn fallback_random(n: usize) -> Vec<u8> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::with_capacity(n + 8);
    while out.len() < n {
        let mut h = RandomState::new().build_hasher();
        h.write_usize(out.len());
        h.write_u128(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
        );
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    out.truncate(n);
    out
}

#[must_use]
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

#[must_use]
pub fn py_round(x: f64, ndigits: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let text = format!("{:.*}", usize::try_from(ndigits.max(0)).unwrap_or(0), x);
    text.parse::<f64>().unwrap_or(x)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arr {
    pub shape: Vec<usize>,
    pub data: Vec<f64>,
}

impl Arr {

    pub fn from_json(value: &Value) -> AResult<Self> {
        fn shape_of(v: &Value, out: &mut Vec<usize>) {
            if let Value::Array(a) = v {
                out.push(a.len());
                if let Some(first) = a.first() {
                    shape_of(first, out);
                }
            }
        }
        fn fill(v: &Value, shape: &[usize], depth: usize, out: &mut Vec<f64>) -> AResult<()> {
            if depth == shape.len() {
                return match v {
                    Value::Array(_) => Err(inhomogeneous(shape, depth)),
                    Value::Null => {
                        out.push(f64::NAN);
                        Ok(())
                    }
                    Value::Object(_) => {
                        Err(type_error("float() argument must be a string or a real number, not 'dict'"))
                    }
                    other => {
                        out.push(py_float(other)?);
                        Ok(())
                    }
                };
            }
            match v {
                Value::Array(a) if a.len() == shape[depth] => {
                    for x in a {
                        fill(x, shape, depth + 1, out)?;
                    }
                    Ok(())
                }
                _ => Err(inhomogeneous(shape, depth)),
            }
        }
        fn inhomogeneous(shape: &[usize], depth: usize) -> AuthoringError {
            let dims: Vec<String> =
                shape[..depth.max(1).min(shape.len())].iter().map(ToString::to_string).collect();
            value_error(format!(
                "setting an array element with a sequence. The requested array has an inhomogeneous shape after {} dimensions. The detected shape was ({}{}) + inhomogeneous part.",
                depth.max(1),
                dims.join(", "),
                if dims.len() == 1 { "," } else { "" }
            ))
        }
        let mut shape = Vec::new();
        shape_of(value, &mut shape);
        let mut data = Vec::with_capacity(shape.iter().product());
        fill(value, &shape, 0, &mut data)?;
        Ok(Self { shape, data })
    }


    pub fn from_opt(value: Option<&Value>) -> AResult<Self> {
        Self::from_json(value.unwrap_or(&Value::Null))
    }

    #[must_use]
    pub fn new(shape: Vec<usize>, data: Vec<f64>) -> Self {
        Self { shape, data }
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    #[must_use]
    pub fn ndim(&self) -> usize {
        self.shape.len()
    }

    #[must_use]
    pub fn all_finite(&self) -> bool {
        self.data.iter().all(|v| v.is_finite())
    }

    #[must_use]
    pub fn vec3(&self) -> Option<[f64; 3]> {
        (self.shape == [3]).then(|| [self.data[0], self.data[1], self.data[2]])
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        nested(&self.shape, &self.data)
    }
}

#[must_use]
pub fn nested(shape: &[usize], data: &[f64]) -> Value {
    fn build(shape: &[usize], data: &[f64], offset: &mut usize) -> Value {
        if shape.is_empty() {
            let v = data.get(*offset).copied().unwrap_or(f64::NAN);
            *offset += 1;
            return jf(v);
        }
        Value::Array((0..shape[0]).map(|_| build(&shape[1..], data, offset)).collect())
    }
    let mut offset = 0;
    build(shape, data, &mut offset)
}

#[must_use]
pub fn nested_bool(shape: &[usize], data: &[bool]) -> Value {
    fn build(shape: &[usize], data: &[bool], offset: &mut usize) -> Value {
        if shape.is_empty() {
            let v = data.get(*offset).copied().unwrap_or(false);
            *offset += 1;
            return Value::Bool(v);
        }
        Value::Array((0..shape[0]).map(|_| build(&shape[1..], data, offset)).collect())
    }
    let mut offset = 0;
    build(shape, data, &mut offset)
}


pub fn bool_array(value: &Value) -> AResult<(Vec<usize>, Vec<bool>)> {
    fn conv(v: &Value) -> Value {
        match v {
            Value::Array(a) => Value::Array(a.iter().map(conv).collect()),
            Value::Bool(b) => Value::from(u8::from(*b)),
            Value::Null => Value::from(0),
            Value::String(s) => Value::from(u8::from(!s.is_empty())),
            Value::Object(m) => Value::from(u8::from(!m.is_empty())),
            n @ Value::Number(_) => n.clone(),
        }
    }
    let a = Arr::from_json(&conv(value))?;
    Ok((a.shape, a.data.iter().map(|v| *v != 0.0).collect()))
}

#[must_use]
pub fn norm3(v: [f64; 3]) -> f64 {
    dot3(v, v).sqrt()
}

#[must_use]
pub fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[2].mul_add(b[2], a[1].mul_add(b[1], a[0] * b[0]))
}

#[must_use]
pub fn row_norm3(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

#[must_use]
pub fn row_dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[must_use]
pub fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[must_use]
pub fn np_sum(a: &[f64]) -> f64 {
    implexity_mesh::numeric::pairwise_sum(a)
}

#[must_use]
pub fn clip(v: f64, lo: f64, hi: f64) -> f64 {
    implexity_mesh::numeric::clip(v, lo, hi)
}

#[must_use]
pub fn keys(value: &Value) -> Vec<String> {
    value.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

#[macro_export]
macro_rules! obj {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = serde_json::Map::new();
        $( m.insert(($k).to_string(), serde_json::Value::from($v)); )*
        serde_json::Value::Object(m)
    }};
}

#[must_use]
pub fn canonical_ascii_spaced(value: &Value) -> String {
    let opts = implexity_core::json::DumpOptions {
        sort_keys: true,
        item_separator: ", ".into(),
        key_separator: ": ".into(),
        ensure_ascii: true,
        indent: None,
    };
    implexity_core::json::dumps(value, &opts)
}

#[must_use]
pub fn np_dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).fold(0.0, |acc, (x, y)| acc + x * y)
}
