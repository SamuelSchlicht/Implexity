// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_runtime::dynamic_frames::catalogue::{StoreId, is_job_id};
use implexity_runtime::dynamic_frames::is_identifier;
use serde_json::Value;

pub const IDENT_PATTERN: &str = "^[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}$";
pub const STORE_PATTERN: &str =
    "^(service/[A-Za-z0-9_-][A-Za-z0-9_.-]{0,95}|job/[0-9a-f]{12}/[A-Za-z0-9_-][A-Za-z0-9_.-]{0,95})$";
pub const JOB_PATTERN: &str = "^[0-9a-f]{12}$";

fn pattern_matches(pattern: &str, s: &str) -> Result<bool, String> {
    match pattern {
        IDENT_PATTERN => Ok(is_identifier(s)),
        STORE_PATTERN => Ok(StoreId::parse(s).is_ok()),
        JOB_PATTERN => Ok(is_job_id(s)),
        other => Err(format!("the schema pattern {other:?} has no checker")),
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "number" => v.as_f64().is_some_and(f64::is_finite),
        "integer" => v.is_i64() || v.is_u64(),
        "null" => v.is_null(),
        _ => false,
    }
}

fn describe(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}



pub fn validate(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(s) = schema.as_object() else { return Ok(()) };
    if let Some(options) = s.get("oneOf").and_then(Value::as_array) {
        let matched = options.iter().filter(|o| validate(o, value, path).is_ok()).count();
        if matched != 1 {
            return Err(format!("{path} matches none of its allowed forms"));
        }
    }
    if let Some(c) = s.get("const")
        && c != value
    {
        return Err(format!("{path} must be {c}"));
    }
    if let Some(options) = s.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        let names: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(format!("{path} must be one of {}", names.join(", ")));
    }
    if let Some(t) = s.get("type").and_then(Value::as_str)
        && !type_ok(t, value)
    {
        return Err(format!(
            "{path} must be {} {t}, not {}",
            if t == "integer" || t == "array" || t == "object" { "an" } else { "a" },
            describe(value)
        ));
    }
    if let Some(x) = value.as_f64() {
        if let Some(lo) = s.get("minimum").and_then(Value::as_f64)
            && x < lo
        {
            return Err(format!("{path} must be at least {lo}"));
        }
        if let Some(hi) = s.get("maximum").and_then(Value::as_f64)
            && x > hi
        {
            return Err(format!("{path} must be at most {hi}"));
        }
    }
    if let Some(text) = value.as_str() {
        let n = text.chars().count() as u64;
        if s.get("minLength").and_then(Value::as_u64).is_some_and(|m| n < m)
            || s.get("maxLength").and_then(Value::as_u64).is_some_and(|m| n > m)
        {
            return Err(format!("{path} has a length outside its bounds"));
        }
        if let Some(p) = s.get("pattern").and_then(Value::as_str)
            && !pattern_matches(p, text)?
        {
            return Err(format!("{path} is not a valid identifier ({text:?})"));
        }
    }
    if let Some(items) = value.as_array() {
        let n = items.len() as u64;
        if s.get("minItems").and_then(Value::as_u64).is_some_and(|m| n < m)
            || s.get("maxItems").and_then(Value::as_u64).is_some_and(|m| n > m)
        {
            return Err(format!("{path} has a number of items outside its bounds"));
        }
        if let Some(item) = s.get("items") {
            for (i, v) in items.iter().enumerate() {
                validate(item, v, &format!("{path}[{i}]"))?;
            }
        }
    }
    if let Some(map) = value.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for k in req.iter().filter_map(Value::as_str) {
                if !map.contains_key(k) {
                    return Err(format!("{path}.{k} is required"));
                }
            }
        }
        for (k, v) in map {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => validate(sub, v, &format!("{path}.{k}"))?,
                None if s.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{path} has the unknown key {k:?}"));
                }
                None => {}
            }
        }
    }
    Ok(())
}

#[must_use]
pub fn patterns(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match schema {
        Value::Object(m) => {
            if let Some(p) = m.get("pattern").and_then(Value::as_str) {
                out.push(p.to_owned());
            }
            for v in m.values() {
                out.extend(patterns(v));
            }
        }
        Value::Array(a) => a.iter().for_each(|v| out.extend(patterns(v))),
        _ => {}
    }
    out
}

