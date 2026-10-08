// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[must_use]
pub fn float_hex(x: f64) -> String {
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exponent = i64::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    let mantissa = bits & ((1_u64 << 52) - 1);
    if exponent == 0 && mantissa == 0 {
        return format!("{sign}0x0.0p+0");
    }
    let (lead, exp) = if exponent == 0 { (0, -1022) } else { (1, exponent - 1023) };
    let exp_sign = if exp < 0 { '-' } else { '+' };
    format!("{sign}0x{lead}.{mantissa:013x}p{exp_sign}{}", exp.abs())
}

#[must_use]
pub fn binary64_value(value: &Value) -> Value {
    match value {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(0.0);
            let f = if f == 0.0 { 0.0 } else { f };
            let mut m = Map::new();
            m.insert("$binary64".into(), Value::String(float_hex(f)));
            Value::Object(m)
        }
        Value::Array(items) => Value::Array(items.iter().map(binary64_value).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), binary64_value(v))).collect())
        }
        other => other.clone(),
    }
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[must_use]
pub fn canonical_text(value: &Value) -> String {
    implexity_core::json::canonical(value)
}

#[must_use]
pub fn canonical_sha256(value: &Value) -> String {
    sha256_hex(canonical_text(value).as_bytes())
}

#[must_use]
pub fn evidence_sha256(value: &Value) -> String {
    canonical_sha256(&binary64_value(value))
}

#[must_use]
pub fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[must_use]
pub fn is_digest_value(value: &Value) -> bool {
    value.as_str().is_some_and(is_digest)
}

#[must_use]
pub fn is_canonical_text(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && !value.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
        && unicode_normalization::is_nfc(value)
}

#[must_use]
pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

#[must_use]
pub fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

#[must_use]
pub fn opt_s(value: Option<&str>) -> Value {
    value.map_or(Value::Null, s)
}

#[must_use]
pub fn f(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn opt_f(value: Option<f64>) -> Value {
    value.map_or(Value::Null, f)
}

#[must_use]
pub fn str_array<S: AsRef<str>>(items: &[S]) -> Value {
    Value::Array(items.iter().map(|i| Value::String(i.as_ref().to_string())).collect())
}

#[must_use]
pub fn has_exact_keys(map: &Map<String, Value>, expected: &[&str]) -> bool {
    map.len() == expected.len() && expected.iter().all(|k| map.contains_key(*k))
}

