// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;

use implexity_core::json::{DumpOptions, dumps};
use implexity_io::digest::sha256_hex;
use serde_json::{Map, Value, json};

use crate::error::{AgentError, AgentResult};
use crate::pyval::{is_int, py_str, truthy};

fn value_error(m: impl Into<String>) -> AgentError {
    AgentError::contract(m)
}



pub fn validate_budget(raw: Option<&Value>) -> AgentResult<Value> {
    let r = match raw {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(value_error("campaign budget must be an object")),
    };
    let pick = |a: &str, b: &str, d: i64| r.get(a).or_else(|| r.get(b)).cloned().unwrap_or_else(|| json!(d));
    let max_runs = pick("max_runs", "maxRuns", 4);
    let max_iterations = pick("max_iterations", "maxIterations", 200);
    if !is_int(&max_runs) || !is_int(&max_iterations) {
        return Err(value_error("campaign budget limits must be integers"));
    }
    let runs = max_runs.as_i64().unwrap_or(i64::MAX);
    let iterations = max_iterations.as_i64().unwrap_or(i64::MAX);
    if !(1..=128).contains(&runs) {
        return Err(value_error("campaign maximum runs must be between 1 and 128"));
    }
    if iterations < 1 {
        return Err(value_error("campaign maximum iterations must be positive"));
    }
    Ok(json!({"max_runs": runs, "max_iterations": iterations}))
}

fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim().to_ascii_lowercase();
            match t.trim_start_matches(['+', '-']) {
                "inf" | "infinity" => {
                    Some(if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY })
                }
                "nan" => Some(f64::NAN),
                _ => t.replace('_', "").parse().ok(),
            }
        }
        _ => None,
    }
}



pub fn rank_variants(rows: &Value, objective: &str, sense: &str) -> AgentResult<Value> {
    if sense != "minimize" && sense != "maximize" {
        return Err(value_error("campaign ranking sense must be minimize or maximize"));
    }
    let rows = rows.as_array().ok_or_else(|| AgentError::failed("variants must be a list"))?;
    let mut valid: Vec<(String, f64, Value)> = Vec::new();
    let mut rejected = Vec::new();
    for row in rows {
        let Some(m) = row.as_object() else {
            return Err(AgentError::failed(format!(
                "'{}' object has no attribute 'get'",
                crate::pyval::type_name(row)
            )));
        };
        let pick =
            |a: &str, b: &str| m.get(a).filter(|v| truthy(v)).or_else(|| m.get(b).filter(|v| truthy(v)));
        let name = pick("variant", "name").map(py_str).unwrap_or_default();
        let status = m.get("status").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        let physical = m.get("physically_valid").or_else(|| m.get("physicallyValid"));
        let physically_valid = if m.contains_key("physically_valid") {
            m.get("physically_valid") == Some(&Value::Bool(true))
        } else {
            physical == Some(&Value::Bool(true))
        };
        let value = m.get(objective).and_then(py_float).unwrap_or(f64::NAN);
        if status != "completed" || !physically_valid || !value.is_finite() {
            rejected.push(json!({"variant": name,
                "reason": "variant is incomplete, physically invalid, or lacks a finite ranking response"}));
            continue;
        }
        valid.push((name, value, row.clone()));
    }
    if sense == "maximize" {
        valid.sort_by(|a, b| b.1.total_cmp(&a.1));
    } else {
        valid.sort_by(|a, b| a.1.total_cmp(&b.1));
    }
    let winner = valid.first().map_or(Value::Null, |v| json!(v.0));
    let ranked: Vec<Value> = valid
        .into_iter()
        .enumerate()
        .map(|(i, (name, value, source))| json!({"rank": i + 1, "variant": name, "value": value, "source": source}))
        .collect();
    Ok(json!({"schema": "implexity-agent-campaign-ranking/1", "objective": objective, "sense": sense,
              "ranked": ranked, "rejected": rejected, "winner": winner}))
}

fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn py_repr_str(s: &str) -> String {
    implexity_core::py_repr::repr_str(s)
}

fn py_iter(v: &Value) -> AgentResult<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(m) => Ok(m.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(AgentError::failed(format!("'{}' object is not iterable", py_type(other)))),
    }
}

fn py_dict(x: &Value) -> AgentResult<Map<String, Value>> {
    if let Value::Object(m) = x {
        return Ok(m.clone());
    }
    let mut out = Map::new();
    for (i, item) in py_iter(x)?.iter().enumerate() {
        let pair = match item {
            Value::Array(_) | Value::Object(_) | Value::String(_) => py_iter(item)?,
            _ => {
                return Err(AgentError::failed(format!(
                    "cannot convert dictionary update sequence element #{i} to a sequence"
                )));
            }
        };
        if pair.len() != 2 {
            return Err(value_error(format!(
                "dictionary update sequence element #{i} has length {}; 2 is required",
                pair.len()
            )));
        }
        let key = match &pair[0] {
            Value::String(k) => k.clone(),
            other => py_str(other),
        };
        out.insert(key, pair[1].clone());
    }
    Ok(out)
}



pub fn allocate_budget(branches: &Value, raw_budget: Option<&Value>) -> AgentResult<Value> {
    let budget = validate_budget(raw_budget)?;
    let max_runs = usize::try_from(budget["max_runs"].as_i64().unwrap_or(1)).unwrap_or(1);
    let max_iterations = budget["max_iterations"].as_i64().unwrap_or(1);
    let rows: Vec<Map<String, Value>> = py_iter(branches)?.iter().map(py_dict).collect::<AgentResult<_>>()?;
    if rows.is_empty() {
        return Err(value_error("campaign requires at least one branch"));
    }
    let rows: Vec<Map<String, Value>> = rows.into_iter().take(max_runs).collect();
    let n_rows = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    if max_iterations < n_rows {
        return Err(value_error("campaign budget needs at least one iteration per selected branch"));
    }
    let mut weights = Vec::with_capacity(rows.len());
    for row in &rows {
        let w = match row.get("priority") {
            None => 1.0,
            Some(v) => py_float(v).ok_or_else(|| match v {
                Value::String(t) => {
                    value_error(format!("could not convert string to float: {}", py_repr_str(t)))
                }
                other => AgentError::failed(format!(
                    "float() argument must be a string or a real number, not '{}'",
                    py_type(other)
                )),
            })?,
        };
        if !w.is_finite() || w <= 0.0 {
            return Err(value_error("campaign branch priority must be finite and positive"));
        }
        weights.push(w);
    }
    let scale = weights.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let normalized: Vec<f64> = weights.iter().map(|w| w / scale).collect();
    let total: f64 = normalized.iter().sum();
    let mut remaining = max_iterations;
    let mut allocation = Vec::with_capacity(rows.len());
    for (i, (row, w)) in rows.iter().zip(&weights).enumerate() {
        let left = n_rows - i64::try_from(i).unwrap_or(0) - 1;
        let n = if left == 0 {
            remaining
        } else {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let share = (max_iterations as f64 * (normalized[i] / total)).round_ties_even() as i64;
            share.max(1).min(remaining - left)
        };
        remaining -= n;
        let name = row
            .get("branch")
            .filter(|v| truthy(v))
            .or_else(|| row.get("name").filter(|v| truthy(v)))
            .map_or_else(|| format!("Branch {}", i + 1), py_str);
        allocation.push(json!({"branch": name, "iterations": n, "priority": w}));
    }
    Ok(json!({"schema": "implexity-agent-campaign-budget/1", "budget": budget, "allocation": allocation,
              "note": "Allocation does not execute branches. Each branch must use ordinary validated Implexity actions and stop if physical/preflight validity fails."}))
}

type Pointer = Vec<String>;

fn tokens(pointer: &Value) -> AgentResult<Pointer> {
    let Some(p) = pointer.as_str().filter(|p| p.starts_with('/') && *p != "/") else {
        return Err(value_error("campaign paths must be non-root JSON pointers"));
    };
    let bytes = p.as_bytes();
    for (i, c) in bytes.iter().enumerate() {
        if *c == b'~' && !matches!(bytes.get(i + 1), Some(b'0' | b'1')) {
            return Err(value_error("invalid JSON pointer escape"));
        }
    }
    Ok(p[1..].split('/').map(|t| t.replace("~1", "/").replace("~0", "~")).collect())
}

fn is_index(part: &str) -> bool {
    part == "0"
        || (part.starts_with(|c: char| ('1'..='9').contains(&c)) && part.bytes().all(|c| c.is_ascii_digit()))
}

fn get<'a>(document: &'a Value, path: &Value) -> AgentResult<&'a Value> {
    let shown = path.as_str().unwrap_or_default();
    let mut value = document;
    for part in tokens(path)? {
        value = match value {
            Value::Object(m) => {
                m.get(&part).ok_or_else(|| value_error(format!("campaign path does not exist: {shown}")))?
            }
            Value::Array(a) => {
                let index = part.parse::<usize>().ok().filter(|i| is_index(&part) && *i < a.len());
                let Some(i) = index else {
                    return Err(value_error(format!("invalid campaign array path: {shown}")));
                };
                &a[i]
            }
            _ => return Err(value_error(format!("campaign path traverses a scalar: {shown}"))),
        };
    }
    Ok(value)
}

fn digest(value: &Value) -> String {
    sha256_hex(dumps(value, &DumpOptions::canonical().ascii(false)).as_bytes())
}

fn differences(a: &Value, b: &Value, path: &str) -> AgentResult<Vec<String>> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            let mut out = Vec::new();
            for key in keys {
                let child = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (x.get(key), y.get(key)) {
                    (Some(p), Some(q)) => out.extend(differences(p, q, &child)?),
                    _ => out.push(child),
                }
            }
            Ok(out)
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            let mut out = Vec::new();
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                out.extend(differences(p, q, &format!("{path}/{i}"))?);
            }
            Ok(out)
        }
        _ => Ok(if digest(a) == digest(b) {
            Vec::new()
        } else {
            vec![if path.is_empty() { "/".into() } else { path.to_owned() }]
        }),
    }
}

fn invariant_digest(document: &Value, allowed: &[Value]) -> AgentResult<Option<String>> {
    let mut normalized = document.clone();
    for path in allowed {
        let parts = tokens(path)?;
        let Some((last, parents)) = parts.split_last() else { return Ok(None) };
        let mut parent = &mut normalized;
        for part in parents {
            parent = match parent {
                Value::Array(a) => match part.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                    Some(v) => v,
                    None => return Ok(None),
                },
                Value::Object(m) => match m.get_mut(part) {
                    Some(v) => v,
                    None => return Ok(None),
                },
                _ => return Ok(None),
            };
        }
        let placeholder = json!({"campaign_factor_path": path});
        match parent {
            Value::Array(a) => match last.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                Some(slot) => *slot = placeholder,
                None => return Ok(None),
            },
            Value::Object(m) => {
                m.insert(last.clone(), placeholder);
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(digest(&normalized)))
}

fn paths(payload: &Map<String, Value>, key: &str, required: bool) -> AgentResult<Vec<Value>> {
    let values = payload.get(key).cloned().unwrap_or_else(|| json!([]));
    let Some(values) = values.as_array().filter(|v| !(required && v.is_empty())) else {
        return Err(value_error(format!(
            "{key} must be {} array",
            if required { "a non-empty" } else { "an" }
        )));
    };
    for v in values {
        tokens(v)?;
    }
    let unique: BTreeSet<String> = values.iter().map(ToString::to_string).collect();
    if unique.len() != values.len() {
        return Err(value_error(format!("duplicate {key}")));
    }
    Ok(values.clone())
}

fn empty_like(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}



#[allow(clippy::too_many_lines)]
pub fn compare_variants(payload: &Value) -> AgentResult<Value> {
    let Some(p) = payload.as_object() else {
        return Err(value_error("campaign comparison requires an object"));
    };
    let Some(reference) = p.get("reference").filter(|r| r.as_object().is_some_and(|m| !m.is_empty())) else {
        return Err(value_error("campaign reference must be a non-empty object"));
    };
    let Some(variants) =
        p.get("variants").and_then(Value::as_array).filter(|v| !v.is_empty() && v.len() <= 128)
    else {
        return Err(value_error("campaign comparison requires between 1 and 128 variants"));
    };
    let allowed = paths(p, "allowed_paths", true)?;
    let protected = paths(p, "invariant_paths", true)?;
    let required = paths(p, "required_resolved_paths", false)?;
    for path in &allowed {
        if matches!(get(reference, path)?, Value::Object(_) | Value::Array(_)) {
            return Err(value_error("allowed campaign paths must identify scalar leaves"));
        }
        for locked in &protected {
            let (a, b) = (tokens(path)?, tokens(locked)?);
            if a.starts_with(&b) || b.starts_with(&a) {
                return Err(value_error("allowed campaign path overlaps an invariant path"));
            }
        }
    }
    for path in protected.iter().chain(&required) {
        get(reference, path)?;
    }
    let reference_sha = digest(reference);
    let invariant_sha = invariant_digest(reference, &allowed)?;
    let allowed_text: Vec<&str> = allowed.iter().filter_map(Value::as_str).collect();
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in variants {
        let Some(v) = raw.as_object() else {
            return Err(value_error("campaign variant must be an object"));
        };
        let name =
            v.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty() && !seen.contains(*n));
        let Some(name) = name else {
            return Err(value_error("campaign variant names must be non-empty and unique"));
        };
        let Some(data) = v.get("inputs").filter(|d| d.is_object()) else {
            return Err(value_error(format!("{name}: campaign inputs must be an object")));
        };
        seen.insert(name.to_owned());
        let full_sha = digest(data);
        let changed = differences(reference, data, "")?;
        let forbidden: Vec<&String> =
            changed.iter().filter(|c| !allowed_text.contains(&c.as_str())).collect();
        let mut unresolved = Vec::new();
        for path in &required {
            match get(data, path) {
                Ok(value) if !empty_like(value) => {}
                _ => unresolved.push(path.clone()),
            }
        }
        let mut row_sha = None;
        let scalars = allowed.iter().all(|path| {
            get(data, path).is_ok_and(|value| !matches!(value, Value::Object(_) | Value::Array(_)))
        });
        if scalars {
            row_sha = invariant_digest(data, &allowed)?;
        }
        let consistent = forbidden.is_empty() && row_sha == invariant_sha;
        let status = if !consistent {
            "inconsistent"
        } else if !unresolved.is_empty() {
            "blocked_unresolved_declarations"
        } else {
            "requires_native_preflight"
        };
        rows.push(json!({"name": name, "status": status, "input_consistent": consistent,
                         "input_sha256": full_sha, "invariant_sha256": row_sha,
                         "changed_paths": changed, "forbidden_changes": forbidden,
                         "unresolved_paths": unresolved, "optimization_authorized": false}));
    }
    let all_consistent = rows.iter().all(|r| r["input_consistent"] == true);
    let all_present = rows.iter().all(|r| r["unresolved_paths"].as_array().is_some_and(Vec::is_empty));
    Ok(json!({"schema": "implexity-agent-campaign-comparison/1",
              "reference_sha256": reference_sha,
              "reference_invariant_sha256": invariant_sha,
              "all_input_consistent": all_consistent,
              "all_required_declarations_present": all_present,
              "variants": rows, "optimization_authorized": false,
              "optimizer_dispatches": 0,
              "next_native_action": "preflight_optimization",
              "evidence_scope": "Equality of submitted declarations only; no model realization, physics validity, solver execution or result provenance is certified."}))
}

