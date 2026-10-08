// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;

use implexity_core::{CaeError, CaeResult};
use implexity_optim::numeric::{float_value, format_g};
use serde_json::{Map, Value};

pub const REPLAY_RELATIVE_TOLERANCE: f64 = 1.0e-12;
pub const REPLAY_OBJECTIVE_ABSOLUTE_TOLERANCE: f64 = 1.0e-12;

fn finite(value: &Value, label: &str) -> CaeResult<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite()),
        _ => None,
    }
    .ok_or_else(|| CaeError::contract(format!("resume replay {label} must be a finite real number")))
}


pub fn validate_objective_replay(
    committed_total: &Value,
    committed_terms: &Value,
    replay_total: &Value,
    replay_terms: &Value,
) -> CaeResult<Value> {
    validate_objective_replay_within(
        committed_total,
        committed_terms,
        replay_total,
        replay_terms,
        REPLAY_RELATIVE_TOLERANCE,
    )
}


pub fn validate_objective_replay_within(
    committed_total: &Value,
    committed_terms: &Value,
    replay_total: &Value,
    replay_terms: &Value,
    relative_tolerance: f64,
) -> CaeResult<Value> {
    if !relative_tolerance.is_finite() || relative_tolerance <= 0.0 {
        return Err(CaeError::contract("resume replay tolerance must be finite and positive"));
    }
    let prior = finite(committed_total, "committed objective")?;
    let fresh = finite(replay_total, "reevaluated objective")?;
    let (Value::Array(old_terms), Value::Array(new_terms)) = (committed_terms, replay_terms) else {
        return Err(CaeError::contract("resume replay terms must be ordered lists"));
    };
    if old_terms.is_empty() || old_terms.len() != new_terms.len() {
        return Err(CaeError::contract("resume replay response count changed"));
    }
    let mut comparisons: Vec<Value> = Vec::new();
    let mut compare = |a: &Value, b: &Value, label: &str, atol: f64| -> CaeResult<()> {
        let (a, b) = (finite(a, label)?, finite(b, label)?);
        let difference = (a - b).abs();
        let scale = a.abs().max(b.abs());
        let limit = atol + relative_tolerance * a.abs();
        if difference > limit {
            return Err(CaeError::contract(format!(
                "resume numerical replay disagrees with committed {label}: stored={}, fresh={}, delta={}, allowed={}",
                format_g(a, 17),
                format_g(b, 17),
                format_g(difference, 6),
                format_g(limit, 6)
            )));
        }
        let mut row = Map::new();
        row.insert("field".into(), Value::String(label.into()));
        row.insert("committed".into(), float_value(a));
        row.insert("replayed".into(), float_value(b));
        row.insert("absolute_delta".into(), float_value(difference));
        row.insert("relative_delta".into(), float_value(if scale == 0.0 { 0.0 } else { difference / scale }));
        row.insert("absolute_tolerance".into(), float_value(atol));
        #[allow(clippy::float_cmp)]
        row.insert("identical_value".into(), Value::Bool(a == b));
        comparisons.push(Value::Object(row));
        Ok(())
    };
    compare(&float_value(prior), &float_value(fresh), "objective", REPLAY_OBJECTIVE_ABSOLUTE_TOLERANCE)?;
    let mut seen: BTreeSet<(String, Option<i64>)> = BTreeSet::new();
    for (index, (old, new)) in old_terms.iter().zip(new_terms).enumerate() {
        let required = ["response", "value", "objective_contribution"];
        let (Some(o), Some(n)) = (old.as_object(), new.as_object()) else {
            return Err(CaeError::contract("resume replay response schema changed"));
        };
        let same_keys = o.len() == n.len() && o.keys().all(|k| n.contains_key(k));
        if !required.iter().all(|k| o.contains_key(*k))
            || !same_keys
            || o.keys().any(|k| !required.contains(&k.as_str()) && k != "operating_point")
        {
            return Err(CaeError::contract("resume replay response schema changed"));
        }
        let response = o["response"].as_str().filter(|s| !s.is_empty());
        if response.is_none() || n["response"].as_str() != response {
            return Err(CaeError::contract("resume replay response identity/order changed"));
        }
        let response = response.unwrap_or_default().to_string();
        let exact_int = |v: &Value| if v.is_f64() { None } else { v.as_i64() };
        let point = o.get("operating_point").and_then(exact_int);
        if o.contains_key("operating_point")
            && (point.is_none_or(|p| p < 0) || n.get("operating_point").and_then(exact_int) != point)
        {
            return Err(CaeError::contract("resume replay operating-point identity changed"));
        }
        if !seen.insert((response.clone(), point)) {
            return Err(CaeError::contract("resume replay duplicate response/operating-point term"));
        }
        let prefix = format!("terms[{index}]/{response}");
        compare(&o["value"], &n["value"], &format!("{prefix}/value"), 0.0)?;
        compare(
            &o["objective_contribution"],
            &n["objective_contribution"],
            &format!("{prefix}/objective_contribution"),
            REPLAY_OBJECTIVE_ABSOLUTE_TOLERANCE,
        )?;
    }
    let identical = comparisons.iter().all(|r| r["identical_value"] == Value::Bool(true));
    let mut out = Map::new();
    out.insert("schema".into(), Value::String("implexity-objective-numerical-replay/1".into()));
    out.insert("relative_tolerance".into(), float_value(relative_tolerance));
    out.insert("raw_response_absolute_tolerance".into(), float_value(0.0));
    out.insert("objective_absolute_tolerance".into(), float_value(REPLAY_OBJECTIVE_ABSOLUTE_TOLERANCE));
    out.insert(
        "checkpoint_content_verification".into(),
        Value::String("separate_exact_identity_contract".into()),
    );
    out.insert("all_values_identical".into(), Value::Bool(identical));
    out.insert("comparisons".into(), Value::Array(comparisons));
    Ok(Value::Object(out))
}
