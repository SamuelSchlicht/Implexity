// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cmp::Ordering;

use implexity_core::contracts::ResponseSpec;
use implexity_core::pyobj::{py_eq, truthy};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::numeric::float_value;
use crate::optimizer::{SEARCH_METHOD, SEARCH_OUTCOME_SCHEMA, TERMINAL_REASONS};
use crate::pyval::is_int;

fn finite(value: Option<&Value>, what: &str) -> CaeResult<f64> {
    match value {
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.is_finite() => Ok(f),
            _ => Err(CaeError::contract(format!("response assessment requires finite {what}"))),
        },
        _ => Err(CaeError::contract(format!("response assessment requires finite {what}"))),
    }
}

fn int_list(value: &Value) -> Option<Vec<i64>> {
    value.as_array()?.iter().map(|v| if is_int(v) { v.as_i64() } else { None }).collect()
}


#[allow(clippy::too_many_lines)]
pub fn response_bound_report(
    responses: &[ResponseSpec],
    terms: Option<&Value>,
    expected_operating_points: Option<&Value>,
) -> CaeResult<Value> {
    if responses.is_empty() {
        return Err(CaeError::contract("response assessment needs typed response declarations"));
    }
    let mut by_name: Vec<(&str, Vec<(usize, &ResponseSpec)>)> = Vec::new();
    for (index, spec) in responses.iter().enumerate() {
        match by_name.iter_mut().find(|(n, _)| *n == spec.name) {
            Some((_, rows)) => rows.push((index, spec)),
            None => by_name.push((&spec.name, vec![(index, spec)])),
        }
    }
    let terms = match terms {
        Some(Value::Array(t)) if !t.is_empty() => t,
        _ => return Err(CaeError::contract("response assessment needs exact ordered response records")),
    };
    let allowed = ["response", "value", "objective_contribution", "operating_point"];
    let mut seen: Vec<(String, i64)> = Vec::new();
    let mut points: Vec<i64> = Vec::new();
    let mut rows = Vec::new();
    for term in terms {
        let Some(map) = term.as_object() else {
            return Err(CaeError::contract("response assessment received malformed response records"));
        };
        if map.keys().any(|k| !allowed.contains(&k.as_str()))
            || !["response", "value", "objective_contribution"].iter().all(|k| map.contains_key(*k))
        {
            return Err(CaeError::contract("response assessment received malformed response records"));
        }
        let name = map.get("response").and_then(Value::as_str);
        let point = match map.get("operating_point") {
            None => Some(0),
            Some(v) if is_int(v) => v.as_i64(),
            Some(_) => None,
        };
        let (Some(name), Some(point)) = (name, point) else {
            return Err(CaeError::contract("response assessment response or operating point is undeclared"));
        };
        let Some((_, roles)) = by_name.iter().find(|(n, _)| *n == name) else {
            return Err(CaeError::contract("response assessment response or operating point is undeclared"));
        };
        if point < 0 {
            return Err(CaeError::contract("response assessment response or operating point is undeclared"));
        }
        let key = (name.to_string(), point);
        if seen.contains(&key) {
            return Err(CaeError::contract("response assessment has a duplicate response point"));
        }
        seen.push(key);
        if !points.contains(&point) {
            points.push(point);
        }
        let value = finite(map.get("value"), "response value")?;
        finite(map.get("objective_contribution"), "objective contribution")?;
        for (index, spec) in roles {
            if !spec.is_bounded() {
                continue;
            }
            let target = spec.target.map_or(f64::NAN, implexity_core::pyobj::PyNum::as_f64);
            let margin = match spec.sense.as_str() {
                "upper" => target - value,
                "lower" => value - target,
                _ => -(value - target).abs(),
            };
            let margin = finite(Some(&float_value(margin)), "bound margin")?;
            let scaled =
                finite(Some(&float_value(f64::max(0.0, -margin) / spec.scale.as_f64())), "scaled violation")?;
            let mut row = Map::new();
            row.insert("response".into(), Value::String(name.to_string()));
            row.insert("operating_point".into(), Value::from(point));
            row.insert("sense".into(), Value::String(spec.sense.clone()));
            row.insert("value".into(), float_value(value));
            row.insert("target".into(), float_value(target));
            row.insert("margin".into(), float_value(margin));
            row.insert("satisfied".into(), Value::Bool(margin >= 0.0));
            row.insert("scaled_violation".into(), float_value(scaled));
            if roles.len() > 1 {
                row.insert("response_role_index".into(), Value::from(*index));
            }
            rows.push(Value::Object(row));
        }
    }
    let complete = by_name.iter().all(|(n, _)| points.iter().all(|p| seen.contains(&((*n).to_string(), *p))))
        && seen.len() == by_name.len() * points.len();
    if !complete {
        return Err(CaeError::contract("response assessment omitted a response/operating-point pair"));
    }
    if let Some(expected) = expected_operating_points {
        let declared = int_list(expected);
        let ok = declared.is_some_and(|d| {
            let mut uniq = d.clone();
            uniq.sort_unstable();
            uniq.dedup();
            let mut pts = points.clone();
            pts.sort_unstable();
            !d.is_empty() && d.iter().all(|p| *p >= 0) && uniq.len() == d.len() && uniq == pts
        });
        if !ok {
            return Err(CaeError::contract(
                "response assessment operating-point coverage differs from the declared stage",
            ));
        }
    }
    let max_violation = rows
        .iter()
        .filter_map(|r| r.get("scaled_violation").and_then(Value::as_f64))
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| if v > m { v } else { m })))
        .unwrap_or(0.0);
    let satisfied = rows.iter().all(|r| r.get("satisfied") == Some(&Value::Bool(true)));
    let mut out = Map::new();
    out.insert("schema".into(), Value::String("implexity-response-bound-report/1".into()));
    out.insert("method".into(), Value::String("soft_augmented_lagrangian_response_bounds".into()));
    out.insert("bound_count".into(), Value::from(rows.len()));
    out.insert("bounds_satisfied".into(), Value::Bool(satisfied));
    out.insert("max_scaled_bound_violation".into(), float_value(max_violation));
    out.insert("rows".into(), Value::Array(rows));
    out.insert("gates_acceptance".into(), Value::Bool(false));
    out.insert("optimization_convergence_inferred".into(), Value::Bool(false));
    Ok(Value::Object(out))
}


pub fn terminal_search_outcome(
    summary: &Map<String, Value>,
    last_row: &Map<String, Value>,
) -> CaeResult<bool> {
    let outcome = summary.get("search_outcome").and_then(Value::as_object);
    let Some(outcome) = outcome.filter(|o| {
        o.get("schema").and_then(Value::as_str) == Some(SEARCH_OUTCOME_SCHEMA)
            && o.get("search_method").and_then(Value::as_str) == Some(SEARCH_METHOD)
    }) else {
        return Err(CaeError::contract(
            "terminal search outcome must be the projected augmented-Lagrangian search record",
        ));
    };
    let converged = outcome.get("optimization_converged").and_then(Value::as_bool);
    let stationary = outcome.get("search_stationary").and_then(Value::as_bool);
    let satisfied = outcome
        .get("response_bounds")
        .and_then(Value::as_object)
        .and_then(|b| b.get("satisfied_within_tolerance"))
        .and_then(Value::as_bool);
    let (Some(converged), Some(stationary), Some(satisfied)) = (converged, stationary, satisfied) else {
        return Err(CaeError::contract("terminal search outcome convergence record is inconsistent"));
    };
    if converged != (stationary && satisfied) {
        return Err(CaeError::contract("terminal search outcome convergence record is inconsistent"));
    }
    let row_reason = match last_row.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if TERMINAL_REASONS.contains(&s.as_str()) => Some(s.as_str()),
        Some(_) => return Err(CaeError::contract("terminal row carries a non-terminal search reason")),
    };
    let expected = row_reason.unwrap_or(if converged { "converged" } else { "iteration_limit" });
    let reason = outcome.get("termination_reason");
    if reason.and_then(Value::as_str) != Some(expected) || (expected == "converged" && !converged) {
        return Err(CaeError::contract("terminal search outcome disagrees with its checked final row"));
    }
    if summary.get("termination_reason").and_then(Value::as_str) != Some(expected)
        || summary.get("optimization_converged") != Some(&Value::Bool(converged))
    {
        return Err(CaeError::contract("terminal summary disagrees with its search outcome"));
    }
    Ok(converged)
}


pub fn committed_merit(row: &Map<String, Value>) -> CaeResult<(i32, f64)> {
    let Some(feasible) = row.get("bound_feasible") else {
        let objective = row.get("L").or_else(|| row.get("objective"));
        return Ok((0, finite(objective, "committed objective")?));
    };
    match feasible {
        Value::Bool(true) => Ok((0, finite(row.get("base_objective"), "committed unbounded objective")?)),
        Value::Bool(false) => {
            Ok((1, finite(row.get("max_scaled_bound_violation"), "committed bound violation")?))
        }
        _ => Err(CaeError::contract("committed bound feasibility must be boolean")),
    }
}

fn stage_get<'a>(stage: &'a Value, a: &str, b: &str) -> Option<&'a Value> {
    stage.get(a).or_else(|| stage.get(b))
}

fn bool_of(map: &Map<String, Value>, key: &str) -> Option<bool> {
    map.get(key).and_then(Value::as_bool)
}


#[allow(clippy::too_many_lines)]
pub fn final_monitor_report(
    row: &Map<String, Value>,
    schedule: Option<&[Value]>,
    point_only: bool,
) -> CaeResult<Value> {
    let diagnostics = match row.get("diagnostics") {
        None => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(CaeError::contract("malformed final candidate diagnostics")),
    };
    let assessment = diagnostics.get("engineering_regime_assessment").filter(|v| !v.is_null());
    let schedule = schedule.filter(|s| !s.is_empty());
    let mut limits: Option<&Value> = None;
    let mut stage: Option<&Value> = None;
    if let Some(sched) = schedule {
        let index = row.get("stage_index").cloned().unwrap_or(Value::from(0));
        let idx = if is_int(&index) { index.as_i64() } else { None };
        let Some(idx) = idx.and_then(|i| usize::try_from(i).ok()).filter(|i| *i < sched.len()) else {
            return Err(CaeError::contract("final candidate stage has no authored scope"));
        };
        stage = Some(&sched[idx]);
        limits = stage_get(&sched[idx], "validity_limits", "validityLimits");
    }
    let default_points = Value::Array(vec![Value::from(0)]);
    let expected = row.get("operating_points").unwrap_or(&default_points);
    let nominal = Value::String("nominal".into());
    let mode = row.get("robust_mode").unwrap_or(&nominal);
    if let (Some(stage), false) = (stage, point_only) {
        let authored = stage_get(stage, "operating_points", "operatingPoints").unwrap_or(&default_points);
        let authored_mode = stage_get(stage, "robust_mode", "robustMode").unwrap_or(&nominal);
        if !py_eq(expected, authored) || !py_eq(mode, authored_mode) {
            return Err(CaeError::contract(
                "final monitor operating-point scope differs from authored stage",
            ));
        }
    }
    let expected_list = int_list(expected).filter(|l| {
        let mut u = l.clone();
        u.sort_unstable();
        u.dedup();
        !l.is_empty() && l.iter().all(|p| *p >= 0) && u.len() == l.len()
    });
    let Some(expected_list) = expected_list else {
        return Err(CaeError::contract("invalid final monitor operating-point scope"));
    };
    let checked: Vec<i64> =
        if mode.as_str() == Some("nominal") { expected_list[..1].to_vec() } else { expected_list.clone() };
    let checked_value = Value::Array(checked.iter().map(|p| Value::from(*p)).collect());
    if !point_only
        && let Some(Value::Object(a)) = assessment
        && a.get("assessment_schema").and_then(Value::as_str) == Some("implexity-regime-monitor-assessment/2")
    {
        let entries = a.get("points").and_then(Value::as_array);
        let ok = entries.is_some_and(|e| {
            a.get("operating_points").is_some_and(|p| py_eq(p, &checked_value))
                && e.len() == checked.len()
                && e.iter().all(Value::is_object)
                && e.iter()
                    .zip(&checked)
                    .all(|(v, p)| v.get("operating_point").is_some_and(|x| py_eq(x, &Value::from(*p))))
        });
        let Some(entries) = entries.filter(|_| ok) else {
            return Err(CaeError::contract("final monitor lacks exact required operating-point coverage"));
        };
        let mut reports = Vec::new();
        for item in entries {
            let mut scoped = row.clone();
            let mut diag = Map::new();
            diag.insert(
                "engineering_regime_assessment".into(),
                item.get("assessment").cloned().unwrap_or(Value::Null),
            );
            scoped.insert("diagnostics".into(), Value::Object(diag));
            reports.push(final_monitor_report(&scoped, schedule, true)?);
        }
        let satisfied = reports.iter().all(|r| r.get("satisfied") == Some(&Value::Bool(true)));
        if bool_of(a, "computable") != Some(true)
            || bool_of(a, "engineering_satisfied") != Some(satisfied)
            || bool_of(a, "ok") != Some(satisfied)
        {
            return Err(CaeError::contract("inconsistent final robust monitor summary"));
        }
        let mut out = Map::new();
        out.insert("status".into(), Value::String(if satisfied { "satisfied" } else { "violated" }.into()));
        out.insert("satisfied".into(), Value::Bool(satisfied));
        out.insert("operating_points".into(), checked_value);
        out.insert("points".into(), Value::Array(reports));
        out.insert("final_role".into(), Value::String("acceptance_required".into()));
        out.insert("qualification_claim".into(), Value::Bool(false));
        return Ok(Value::Object(out));
    }
    let limits_truthy = limits.is_some_and(truthy);
    if !point_only && checked.len() > 1 && (limits_truthy || assessment.is_some()) {
        return Err(CaeError::contract(
            "legacy aggregate monitor evidence lacks per-operating-point provenance",
        ));
    }
    let Some(assessment) = assessment else {
        if limits_truthy {
            return Err(CaeError::contract("authored final monitor limits lack sealed assessment"));
        }
        let mut out = Map::new();
        out.insert("status".into(), Value::String("not_declared".into()));
        out.insert("satisfied".into(), Value::Bool(true));
        out.insert("qualification_claim".into(), Value::Bool(false));
        return Ok(Value::Object(out));
    };
    let Some(a) = assessment.as_object().filter(|a| {
        a.get("assessment_schema").and_then(Value::as_str) == Some("implexity-regime-monitor-assessment/1")
    }) else {
        return Err(CaeError::contract("unknown or malformed final monitor assessment"));
    };
    for key in ["ok", "computable", "engineering_satisfied"] {
        if bool_of(a, key).is_none() {
            return Err(CaeError::contract("malformed final monitor Boolean"));
        }
    }
    let mut lists: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for (slot, key) in ["contract_issues", "engineering_violations"].iter().enumerate() {
        match a.get(*key).and_then(Value::as_array) {
            Some(items) if items.iter().all(Value::is_string) => {
                lists[slot] = items.iter().filter_map(Value::as_str).map(str::to_string).collect();
            }
            _ => return Err(CaeError::contract("malformed final monitor issue list")),
        }
    }
    let ok = bool_of(a, "ok").unwrap_or(false);
    let computable = bool_of(a, "computable").unwrap_or(false);
    let eng = bool_of(a, "engineering_satisfied").unwrap_or(false);
    if computable != lists[0].is_empty() || eng != lists[1].is_empty() || ok != (computable && eng) {
        return Err(CaeError::contract("inconsistent final monitor assessment"));
    }
    if !computable {
        return Err(CaeError::contract("final monitor is not computable"));
    }
    if !limits_truthy {
        let mut out = Map::new();
        out.insert(
            "status".into(),
            Value::String(if ok { "satisfied" } else { "unresolved_legacy_final_role" }.into()),
        );
        out.insert("satisfied".into(), Value::Bool(ok));
        out.insert("assessment".into(), Value::Object(a.clone()));
        out.insert("qualification_claim".into(), Value::Bool(false));
        return Ok(Value::Object(out));
    }
    let Some(Value::Object(limits)) = limits else {
        return Err(CaeError::contract("malformed authored final monitor limits"));
    };
    let (Some(Value::Object(values)), Some(Value::Object(units))) = (a.get("values"), a.get("units")) else {
        return Err(CaeError::contract("final monitor omits metric evidence"));
    };
    let mut satisfied = true;
    for (name, bounds) in limits {
        let Some(bounds) =
            bounds.as_object().filter(|_| values.contains_key(name) && units.contains_key(name))
        else {
            return Err(CaeError::contract("final monitor omits authored metric"));
        };
        let value = finite(values.get(name), "final monitored metric")?;
        if let Some(unit) = bounds.get("unit").filter(|u| !u.is_null())
            && Some(unit) != units.get(name)
        {
            return Err(CaeError::contract("final monitor unit differs from authored limit"));
        }
        let low = match bounds.get("min") {
            None | Some(Value::Null) => None,
            v => Some(finite(v, "final lower limit")?),
        };
        let high = match bounds.get("max") {
            None | Some(Value::Null) => None,
            v => Some(finite(v, "final upper limit")?),
        };
        if let (Some(l), Some(h)) = (low, high)
            && l > h
        {
            return Err(CaeError::contract("inverted authored final limits"));
        }
        satisfied = satisfied && low.is_none_or(|l| value >= l) && high.is_none_or(|h| value <= h);
    }
    if satisfied != eng {
        return Err(CaeError::contract("final monitor assessment differs from authored limits"));
    }
    let mut out = Map::new();
    out.insert("status".into(), Value::String(if satisfied { "satisfied" } else { "violated" }.into()));
    out.insert("satisfied".into(), Value::Bool(satisfied));
    out.insert("final_role".into(), Value::String("acceptance_required".into()));
    out.insert("assessment".into(), Value::Object(a.clone()));
    out.insert("qualification_claim".into(), Value::Bool(false));
    Ok(Value::Object(out))
}

type Candidate<'a> = ((i32, f64), usize, f64, &'a Map<String, Value>, Value, Value);

const SCOPE_KEYS: [&str; 8] = [
    "stage_index",
    "stage",
    "provider",
    "fidelity",
    "robust_mode",
    "operating_points",
    "released_blocks",
    "active_coordinates",
];

fn scope(row: &Map<String, Value>) -> Map<String, Value> {
    SCOPE_KEYS.iter().map(|k| ((*k).to_string(), row.get(*k).cloned().unwrap_or(Value::Null))).collect()
}


#[allow(clippy::too_many_lines)]
pub fn select_screened_history_candidate(
    responses: &[ResponseSpec],
    history: Option<&[Value]>,
    schedule: Option<&[Value]>,
) -> CaeResult<Option<Value>> {
    let has_monitor = history.unwrap_or_default().iter().any(|r| {
        r.get("diagnostics")
            .and_then(Value::as_object)
            .is_some_and(|d| d.contains_key("engineering_regime_assessment"))
    });
    let authored_monitor = schedule
        .unwrap_or_default()
        .iter()
        .any(|s| stage_get(s, "validity_limits", "validityLimits").is_some_and(truthy));
    if !has_monitor && !authored_monitor {
        return Ok(None);
    }
    let history = match history {
        Some(h) if !h.is_empty() => h,
        _ => return Err(CaeError::contract("engineering screening requires committed candidate history")),
    };
    let Some(last) = history.last().and_then(Value::as_object) else {
        return Err(CaeError::contract("engineering screening requires a terminal stage record"));
    };
    let terminal_scope = scope(last);
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for (index, row) in history.iter().enumerate() {
        let Some(row) = row.as_object() else {
            return Err(CaeError::contract("engineering screening received malformed candidate history"));
        };
        let iteration = row.get("i").or_else(|| row.get("iteration"));
        let iteration_ok = iteration.is_some_and(|v| is_int(v) && v.as_u64() == u64::try_from(index).ok());
        let accepted = row.get("accepted").and_then(Value::as_bool);
        let Some(accepted) = accepted.filter(|_| iteration_ok) else {
            return Err(CaeError::contract("engineering screening received malformed candidate history"));
        };
        let expected = match row.get("operating_points") {
            None | Some(Value::Null) => None,
            Some(Value::Array(points)) if !points.is_empty() => {
                if row.get("robust_mode").and_then(Value::as_str) == Some("nominal") {
                    Some(Value::Array(points[..1].to_vec()))
                } else {
                    Some(Value::Array(points.clone()))
                }
            }
            Some(_) => {
                return Err(CaeError::contract(
                    "engineering screening stage operating points must be explicit",
                ));
            }
        };
        let report = response_bound_report(responses, row.get("terms"), expected.as_ref())?;
        let objective = finite(row.get("L").or_else(|| row.get("objective")), "committed objective")?;
        if scope(row) != terminal_scope {
            continue;
        }
        let stationary = !accepted && row.get("reason").and_then(Value::as_str) == Some("converged");
        if stationary {
            let updates = row.get("coordinate_updates").and_then(Value::as_object);
            let unchanged = row.get("continuable") == Some(&Value::Bool(false))
                && finite(row.get("step_fraction"), "stationary step fraction")? == 0.0
                && updates.is_some_and(|u| !u.is_empty());
            let mut all_zero = true;
            if unchanged && let Some(updates) = updates {
                for v in updates.values() {
                    let Some(v) = v.as_object() else {
                        all_zero = false;
                        break;
                    };
                    let max_abs = finite(v.get("max_abs"), "stationary coordinate change")?;
                    let l2 = finite(v.get("l2"), "stationary coordinate change")?;
                    if max_abs != 0.0 || l2 != 0.0 {
                        all_zero = false;
                        break;
                    }
                }
            }
            if !unchanged || !all_zero {
                return Err(CaeError::contract(
                    "stationary admission requires an unchanged terminal design snapshot",
                ));
            }
        }
        if !accepted && !stationary {
            continue;
        }
        let monitor = final_monitor_report(row, schedule, false)?;
        if monitor.get("satisfied") == Some(&Value::Bool(true)) {
            let push_ok = row
                .get("push")
                .and_then(Value::as_object)
                .and_then(|p| p.get("file"))
                .and_then(Value::as_str)
                == Some(format!("live_{index:06}.npz").as_str());
            let id_ok = row.get("design_state_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
            if !push_ok || !id_ok {
                return Err(CaeError::contract("screened candidate omitted its immutable design identity"));
            }
            candidates.push((committed_merit(row)?, index, objective, row, report, monitor));
        }
    }
    let best = candidates.into_iter().min_by(|a, b| {
        a.0.0.cmp(&b.0.0).then(a.0.1.partial_cmp(&b.0.1).unwrap_or(Ordering::Equal)).then(a.1.cmp(&b.1))
    });
    let Some((_merit, index, objective, row, report, monitor)) = best else {
        return Err(CaeError::contract(
            "no committed design update satisfies all resolved final engineering screens; iteration completion is not engineering feasibility",
        ));
    };
    let mut out = Map::new();
    out.insert("epoch".into(), Value::from(index));
    out.insert("file".into(), row["push"]["file"].clone());
    out.insert("design_state_id".into(), row["design_state_id"].clone());
    out.insert("objective".into(), float_value(objective));
    out.insert("response_bounds".into(), report);
    out.insert("terminal_stage_scope".into(), Value::Object(terminal_scope));
    out.insert("engineering_regime_admission".into(), monitor);
    let accepted = row.get("accepted") == Some(&Value::Bool(true));
    out.insert(
        "candidate_kind".into(),
        Value::String(if accepted { "accepted_design_update" } else { "stationary_evaluated_design" }.into()),
    );
    Ok(Some(Value::Object(out)))
}

