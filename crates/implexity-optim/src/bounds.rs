// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::collections::BTreeMap;

use implexity_core::contracts::{CoordinateOptimizationSettings, ResponseSpec};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{PyNum, py_eq};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::numeric::{float_list, float_value};
use crate::response_program::combine_measurement_terms;

pub const RESPONSE_NORMALIZATION_SCHEMA: &str = "implexity-response-normalization/1";
pub const RESPONSE_NORMALIZATION_RESULT_SCHEMA: &str = "implexity-response-normalization-result/1";
pub const BOUND_SENSES: [&str; 3] = ["upper", "lower", "equal"];
pub const BOUND_STATE_SCHEMA: &str = "implexity-response-bound-multipliers/1";

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizationPolicy {
    pub floors: Vec<(String, f64)>,
}

impl NormalizationPolicy {
    fn floors_value(&self) -> Value {
        Value::Object(self.floors.iter().map(|(k, v)| (k.clone(), float_value(*v))).collect())
    }

    fn floor(&self, name: &str) -> f64 {
        self.floors.iter().find(|(k, _)| k == name).map_or(f64::NAN, |(_, v)| *v)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(RESPONSE_NORMALIZATION_SCHEMA.into()));
        m.insert("mode".into(), Value::String("initial_exact".into()));
        m.insert("floors".into(), self.floors_value());
        Value::Object(m)
    }
}

fn unique_names(responses: &[ResponseSpec]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in responses {
        if !out.contains(&s.name) {
            out.push(s.name.clone());
        }
    }
    out
}

fn same_set(keys: &[&String], names: &[String]) -> bool {
    let mut a: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    a.sort_unstable();
    a.dedup();
    let mut b: Vec<&str> = names.iter().map(String::as_str).collect();
    b.sort_unstable();
    b.dedup();
    a == b
}


pub fn normalise_response_normalization(
    raw: Option<&Value>,
    responses: &[ResponseSpec],
) -> CaeResult<Option<NormalizationPolicy>> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(map) = raw.as_object() else {
        return Err(CaeError::contract("response_normalization must be an object"));
    };
    let mut extra: Vec<&String> =
        map.keys().filter(|k| !["schema", "mode", "floors"].contains(&k.as_str())).collect();
    extra.sort();
    if !extra.is_empty() {
        return Err(CaeError::contract(format!(
            "response_normalization contains unsupported fields {}",
            crate::numeric::str_list_repr(&extra)
        )));
    }
    if map.get("schema").and_then(Value::as_str) != Some(RESPONSE_NORMALIZATION_SCHEMA) {
        return Err(CaeError::contract(format!(
            "response_normalization schema must be {}",
            repr_str(RESPONSE_NORMALIZATION_SCHEMA)
        )));
    }
    if map.get("mode").and_then(Value::as_str) != Some("initial_exact") {
        return Err(CaeError::contract("response_normalization mode must be 'initial_exact'"));
    }
    let names: Vec<String> = responses.iter().map(|s| s.name.clone()).collect();
    let floors = map.get("floors").and_then(Value::as_object);
    let Some(floors) = floors.filter(|f| same_set(&f.keys().collect::<Vec<_>>(), &names)) else {
        return Err(CaeError::contract(
            "response_normalization floors must exactly match the requested responses",
        ));
    };
    let mut checked: Vec<(String, f64)> = Vec::new();
    for name in &names {
        let value = &floors[name];
        let Some(number) = PyNum::from_value(value).map(PyNum::as_f64) else {
            return Err(CaeError::contract(format!(
                "response normalization floor for {} must be numeric",
                repr_str(name)
            )));
        };
        if !number.is_finite() || number <= 0.0 {
            return Err(CaeError::contract(format!(
                "response normalization floor for {} must be finite and > 0",
                repr_str(name)
            )));
        }
        if let Some(slot) = checked.iter_mut().find(|(k, _)| k == name) {
            slot.1 = number;
        } else {
            checked.push((name.clone(), number));
        }
    }
    Ok(Some(NormalizationPolicy { floors: checked }))
}

fn with_scale(spec: &ResponseSpec, scale: f64) -> ResponseSpec {
    ResponseSpec { scale: PyNum::Float(scale), ..spec.clone() }
}

fn result_record(policy: &NormalizationPolicy, scales: &[(String, f64)]) -> Value {
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(RESPONSE_NORMALIZATION_RESULT_SCHEMA.into()));
    m.insert("mode".into(), Value::String("initial_exact".into()));
    m.insert("source".into(), Value::String("initial_exact_gradient_evaluation".into()));
    m.insert("floors".into(), policy.floors_value());
    m.insert(
        "scales".into(),
        Value::Object(scales.iter().map(|(k, v)| (k.clone(), float_value(*v))).collect()),
    );
    m.insert("frozen".into(), Value::Bool(true));
    Value::Object(m)
}


pub fn resolve_initial_response_normalization(
    responses: &[ResponseSpec],
    values: &BTreeMap<String, f64>,
    policy: Option<&Value>,
) -> CaeResult<(Vec<ResponseSpec>, Option<Value>)> {
    let Some(checked) = normalise_response_normalization(policy, responses)? else {
        return Ok((responses.to_vec(), None));
    };
    let names: Vec<String> = responses.iter().map(|s| s.name.clone()).collect();
    if !same_set(&values.keys().collect::<Vec<_>>(), &names) {
        return Err(CaeError::contract(
            "initial exact responses do not exactly match response normalization",
        ));
    }
    let mut scales: Vec<(String, f64)> = Vec::new();
    for name in &names {
        let number = values[name];
        if !number.is_finite() {
            return Err(CaeError::contract(format!(
                "initial exact response {} must be finite",
                repr_str(name)
            )));
        }
        let scale = f64::max(number.abs(), checked.floor(name));
        if let Some(slot) = scales.iter_mut().find(|(k, _)| k == name) {
            slot.1 = scale;
        } else {
            scales.push((name.clone(), scale));
        }
    }
    let lookup = |n: &str| scales.iter().find(|(k, _)| k == n).map_or(f64::NAN, |(_, v)| *v);
    let resolved = responses.iter().map(|s| with_scale(s, lookup(&s.name))).collect();
    Ok((resolved, Some(result_record(&checked, &scales))))
}


pub fn restore_response_normalization(
    responses: &[ResponseSpec],
    policy: Option<&Value>,
    record: Option<&Value>,
) -> CaeResult<(Vec<ResponseSpec>, Option<Value>)> {
    let record = record.filter(|r| !r.is_null());
    let Some(checked) = normalise_response_normalization(policy, responses)? else {
        if record.is_some() {
            return Err(CaeError::contract(
                "checkpoint contains response normalization not declared by the job",
            ));
        }
        return Ok((responses.to_vec(), None));
    };
    let keys = ["schema", "mode", "source", "floors", "scales", "frozen"];
    let Some(map) = record
        .and_then(Value::as_object)
        .filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(CaeError::contract("checkpoint response normalization is malformed"));
    };
    if map.get("schema").and_then(Value::as_str) != Some(RESPONSE_NORMALIZATION_RESULT_SCHEMA)
        || map.get("mode").and_then(Value::as_str) != Some("initial_exact")
        || map.get("source").and_then(Value::as_str) != Some("initial_exact_gradient_evaluation")
        || map.get("frozen") != Some(&Value::Bool(true))
        || !map.get("floors").is_some_and(|f| py_eq(f, &checked.floors_value()))
    {
        return Err(CaeError::contract("checkpoint response normalization identity drifted"));
    }
    let names: Vec<String> = responses.iter().map(|s| s.name.clone()).collect();
    let Some(scales) = map
        .get("scales")
        .and_then(Value::as_object)
        .filter(|s| same_set(&s.keys().collect::<Vec<_>>(), &names))
    else {
        return Err(CaeError::contract("checkpoint response scales are malformed"));
    };
    let mut frozen: Vec<(String, f64)> = Vec::new();
    for name in &names {
        let Some(number) = PyNum::from_value(&scales[name]).map(PyNum::as_f64) else {
            return Err(CaeError::contract(format!(
                "checkpoint response scale for {} must be numeric",
                repr_str(name)
            )));
        };
        if !number.is_finite() || number < checked.floor(name) {
            return Err(CaeError::contract(format!(
                "checkpoint response scale for {} is invalid",
                repr_str(name)
            )));
        }
        if let Some(slot) = frozen.iter_mut().find(|(k, _)| k == name) {
            slot.1 = number;
        } else {
            frozen.push((name.clone(), number));
        }
    }
    let canonical = result_record(&checked, &frozen);
    if !py_eq(&Value::Object(map.clone()), &canonical) {
        return Err(CaeError::contract("checkpoint response normalization is not canonical"));
    }
    let lookup = |n: &str| frozen.iter().find(|(k, _)| k == n).map_or(f64::NAN, |(_, v)| *v);
    Ok((responses.iter().map(|s| with_scale(s, lookup(&s.name))).collect(), Some(canonical)))
}

#[must_use]
pub fn is_bound(spec: &ResponseSpec) -> bool {
    BOUND_SENSES.contains(&spec.sense.as_str())
}

fn target(spec: &ResponseSpec) -> f64 {
    spec.target.map_or(f64::NAN, PyNum::as_f64)
}

#[must_use]
pub fn scaled_constraint(spec: &ResponseSpec, value: f64) -> (f64, f64) {
    let scale = spec.scale.as_f64();
    if spec.sense == "lower" {
        ((target(spec) - value) / scale, -1.0 / scale)
    } else {
        ((value - target(spec)) / scale, 1.0 / scale)
    }
}

#[must_use]
pub fn bound_program_identity(responses: &[ResponseSpec]) -> String {
    let rows: Vec<Value> = responses
        .iter()
        .enumerate()
        .filter(|(_, s)| is_bound(s))
        .map(|(i, s)| {
            Value::Array(vec![
                Value::from(i),
                Value::String(s.name.clone()),
                Value::String(s.sense.clone()),
                float_value(target(s)),
                float_value(s.scale.as_f64()),
                float_value(s.weight.as_f64()),
            ])
        })
        .collect();
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(BOUND_STATE_SCHEMA.into()));
    m.insert("bounds".into(), Value::Array(rows));
    implexity_core::json::canonical_sha256(&Value::Object(m))
}

fn finite_float(value: &Value, label: &str) -> CaeResult<f64> {
    match PyNum::from_value(value).map(PyNum::as_f64) {
        Some(f) if f.is_finite() => Ok(f),
        _ => Err(CaeError::contract(format!("{label} must be a finite number"))),
    }
}

pub type BoundParameters<'a> = Vec<(&'a ResponseSpec, Option<(f64, f64, f64)>)>;

#[derive(Debug, Clone, PartialEq)]
pub struct BoundMultiplierState {
    pub program: String,
    pub multipliers: Vec<f64>,
    pub penalties: Vec<f64>,
    pub initial_penalties: Vec<f64>,
    pub updates: i64,
    pub reference_measure: Option<f64>,
}

impl BoundMultiplierState {
    #[must_use]
    pub fn initial(responses: &[ResponseSpec]) -> Self {
        let penalties: Vec<f64> =
            responses.iter().filter(|s| is_bound(s)).map(|s| 2.0 * s.weight.as_f64()).collect();
        Self {
            program: bound_program_identity(responses),
            multipliers: vec![0.0; penalties.len()],
            penalties: penalties.clone(),
            initial_penalties: penalties,
            updates: 0,
            reference_measure: None,
        }
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(BOUND_STATE_SCHEMA.into()));
        m.insert("program".into(), Value::String(self.program.clone()));
        m.insert("multipliers".into(), float_list(&self.multipliers));
        m.insert("penalties".into(), float_list(&self.penalties));
        m.insert("initial_penalties".into(), float_list(&self.initial_penalties));
        m.insert("updates".into(), Value::from(self.updates));
        m.insert("reference_measure".into(), self.reference_measure.map_or(Value::Null, float_value));
        Value::Object(m)
    }


    pub fn from_wire(raw: &Value, responses: &[ResponseSpec]) -> CaeResult<Self> {
        let keys = [
            "schema",
            "program",
            "multipliers",
            "penalties",
            "initial_penalties",
            "updates",
            "reference_measure",
        ];
        let Some(map) = raw.as_object().filter(|m| {
            m.len() == keys.len()
                && keys.iter().all(|k| m.contains_key(*k))
                && m.get("schema").and_then(Value::as_str) == Some(BOUND_STATE_SCHEMA)
        }) else {
            return Err(CaeError::contract("response-bound multiplier state is malformed"));
        };
        let program = bound_program_identity(responses);
        if map.get("program").and_then(Value::as_str) != Some(program.as_str()) {
            return Err(CaeError::contract(
                "response-bound multiplier state belongs to another response program",
            ));
        }
        let bounds: Vec<&ResponseSpec> = responses.iter().filter(|s| is_bound(s)).collect();
        let mut arrays: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (slot, key) in ["multipliers", "penalties", "initial_penalties"].iter().enumerate() {
            let rows = map[*key].as_array().filter(|r| r.len() == bounds.len()).ok_or_else(|| {
                CaeError::contract(format!("response-bound {key} do not match the bounded responses"))
            })?;
            arrays[slot] = rows
                .iter()
                .map(|v| finite_float(v, &format!("response-bound {key}")))
                .collect::<CaeResult<_>>()?;
        }
        let [multipliers, penalties, initial_penalties] = arrays;
        for (((spec, lam), mu), mu0) in
            bounds.iter().zip(&multipliers).zip(&penalties).zip(&initial_penalties)
        {
            #[allow(clippy::float_cmp)]
            let wrong_mu0 = *mu0 != 2.0 * spec.weight.as_f64();
            if wrong_mu0 || mu0 > mu || (spec.sense != "equal" && *lam < 0.0) {
                return Err(CaeError::contract(
                    "response-bound multiplier state is inconsistent with its program",
                ));
            }
        }
        let updates = match &map["updates"] {
            v if crate::pyval::is_int(v) => v.as_i64().filter(|u| *u >= 0),
            _ => None,
        }
        .ok_or_else(|| CaeError::contract("response-bound update count is malformed"))?;
        let reference = match &map["reference_measure"] {
            Value::Null => None,
            v => {
                let r = finite_float(v, "response-bound reference measure")?;
                if r < 0.0 {
                    return Err(CaeError::contract("response-bound reference measure must be nonnegative"));
                }
                Some(r)
            }
        };
        Ok(Self { program, multipliers, penalties, initial_penalties, updates, reference_measure: reference })
    }

    #[must_use]
    pub fn parameters<'a>(&self, responses: &'a [ResponseSpec]) -> BoundParameters<'a> {
        let mut cursor = 0;
        responses
            .iter()
            .map(|spec| {
                if is_bound(spec) {
                    let p =
                        (self.multipliers[cursor], self.penalties[cursor], self.initial_penalties[cursor]);
                    cursor += 1;
                    (spec, Some(p))
                } else {
                    (spec, None)
                }
            })
            .collect()
    }
}

#[must_use]
pub fn contribution(spec: &ResponseSpec, value: f64, lam_mu: Option<(f64, f64)>) -> (f64, f64) {
    let weight = spec.weight.as_f64();
    let scale = spec.scale.as_f64();
    match spec.sense.as_str() {
        "minimise" => (weight * value / scale, weight / scale),
        "maximise" => (-weight * value / scale, -weight / scale),
        _ => {
            let (lam, mu) = lam_mu.unwrap_or((0.0, 1.0));
            let (c, dc) = scaled_constraint(spec, value);
            if spec.sense == "equal" {
                (lam * c + 0.5 * mu * c * c, (lam + mu * c) * dc)
            } else {
                let shifted = f64::max(0.0, lam + mu * c);
                ((shifted * shifted - lam * lam) / (2.0 * mu), shifted * dc)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Augmented {
    pub total: f64,
    pub coefficients: Vec<f64>,
    pub terms: Vec<Value>,
    pub base: f64,
}


pub fn augmented_objective(
    responses: &[ResponseSpec],
    values: &dyn Fn(&str) -> Option<f64>,
    state: &BoundMultiplierState,
    operating_point: Option<i64>,
) -> CaeResult<Augmented> {
    if state.program != bound_program_identity(responses) {
        return Err(CaeError::contract(
            "response-bound multiplier state belongs to another response program",
        ));
    }
    let mut total = 0.0;
    let mut base = 0.0;
    let mut coefficients = Vec::with_capacity(responses.len());
    let mut terms = Vec::with_capacity(responses.len());
    for (spec, params) in state.parameters(responses) {
        let value = values(&spec.name).ok_or_else(|| {
            CaeError::contract(format!("response value {} is missing", repr_str(&spec.name)))
        })?;
        let (term, coefficient) = contribution(spec, value, params.map(|(l, m, _)| (l, m)));
        total += term;
        if !is_bound(spec) {
            base += term;
        }
        coefficients.push(coefficient);
        let mut row = Map::new();
        row.insert("response".into(), Value::String(spec.name.clone()));
        row.insert("value".into(), float_value(value));
        row.insert("objective_contribution".into(), float_value(term));
        if let Some(p) = operating_point {
            row.insert("operating_point".into(), Value::from(p));
        }
        terms.push(Value::Object(row));
    }
    if !total.is_finite() {
        return Err(CaeError::contract("nonfinite aggregate response objective"));
    }
    Ok(Augmented { total, coefficients, terms: combine_measurement_terms(&terms)?, base })
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundMeasures {
    pub bounds: Vec<Value>,
    pub max_scaled_violation: f64,
    pub max_measure: f64,
}

impl BoundMeasures {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("bounds".into(), Value::Array(self.bounds.clone()));
        m.insert("max_scaled_violation".into(), float_value(self.max_scaled_violation));
        m.insert("max_measure".into(), float_value(self.max_measure));
        Value::Object(m)
    }
}

fn py_max(values: impl Iterator<Item = f64>) -> f64 {
    let mut out: Option<f64> = None;
    for v in values {
        out = Some(match out {
            None => v,
            Some(m) if v > m => v,
            Some(m) => m,
        });
    }
    out.unwrap_or(0.0)
}


pub fn bound_measures(
    responses: &[ResponseSpec],
    values: &dyn Fn(&str) -> Option<f64>,
    state: &BoundMultiplierState,
) -> CaeResult<BoundMeasures> {
    let mut rows = Vec::new();
    let mut violations = Vec::new();
    let mut measures = Vec::new();
    for (spec, params) in state.parameters(responses) {
        let Some((lam, mu, _)) = params else { continue };
        let value = values(&spec.name).ok_or_else(|| {
            CaeError::contract(format!("response value {} is missing", repr_str(&spec.name)))
        })?;
        let (c, _dc) = scaled_constraint(spec, value);
        let (violation, measure) = if spec.sense == "equal" {
            (c.abs(), c.abs())
        } else {
            (f64::max(0.0, c), f64::max(c, -lam / mu).abs())
        };
        let mut row = Map::new();
        row.insert("response".into(), Value::String(spec.name.clone()));
        row.insert("sense".into(), Value::String(spec.sense.clone()));
        row.insert("target".into(), float_value(target(spec)));
        row.insert("scale".into(), float_value(spec.scale.as_f64()));
        row.insert("value".into(), float_value(value));
        row.insert("scaled_constraint".into(), float_value(c));
        row.insert("scaled_violation".into(), float_value(violation));
        row.insert("measure".into(), float_value(measure));
        row.insert("multiplier".into(), float_value(lam));
        row.insert("penalty".into(), float_value(mu));
        rows.push(Value::Object(row));
        violations.push(violation);
        measures.push(measure);
    }
    Ok(BoundMeasures {
        bounds: rows,
        max_scaled_violation: py_max(violations.into_iter()),
        max_measure: py_max(measures.into_iter()),
    })
}


pub fn update_bound_multipliers(
    responses: &[ResponseSpec],
    values: &dyn Fn(&str) -> Option<f64>,
    state: &BoundMultiplierState,
    settings: &CoordinateOptimizationSettings,
    trigger: &str,
) -> CaeResult<(BoundMultiplierState, Value)> {
    let measures = bound_measures(responses, values, state)?;
    let measure = measures.max_measure;
    let grow = state.reference_measure.is_some_and(|r| measure > settings.violation_reduction.as_f64() * r);
    let mut multipliers = Vec::new();
    let mut penalties = Vec::new();
    let mut at_limit = true;
    let mut grown: Vec<Value> = Vec::new();
    let tolerance = settings.bound_tolerance.as_f64();
    let bounded: Vec<(&ResponseSpec, (f64, f64, f64))> =
        state.parameters(responses).into_iter().filter_map(|(s, p)| p.map(|p| (s, p))).collect();
    for ((spec, (lam, mu, mu0)), row) in bounded.into_iter().zip(&measures.bounds) {
        let c = row.get("scaled_constraint").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let row_measure = row.get("measure").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let shifted = lam + mu * c;
        multipliers.push(if spec.sense == "equal" { shifted } else { f64::max(0.0, shifted) });
        let limit = settings.penalty_limit.as_f64() * mu0;
        let mut new_mu = mu;
        if grow && row_measure > tolerance && mu < limit {
            new_mu = f64::min(limit, mu * settings.penalty_growth.as_f64());
            grown.push(Value::String(spec.name.clone()));
        }
        if row_measure > tolerance && new_mu < limit {
            at_limit = false;
        }
        penalties.push(new_mu);
    }
    let updated = BoundMultiplierState {
        multipliers,
        penalties,
        updates: state.updates + 1,
        reference_measure: Some(measure),
        ..state.clone()
    };
    let mut record = Map::new();
    record.insert("trigger".into(), Value::String(trigger.into()));
    record.insert("update_index".into(), Value::from(updated.updates));
    record.insert("measure_before".into(), float_value(measure));
    record.insert("max_scaled_violation".into(), float_value(measures.max_scaled_violation));
    record.insert("penalty_grown".into(), Value::Array(grown));
    record.insert("violating_penalties_at_limit".into(), Value::Bool(at_limit));
    record.insert("multipliers".into(), float_list(&updated.multipliers));
    record.insert("penalties".into(), float_list(&updated.penalties));
    Ok((updated, Value::Object(record)))
}

pub fn lookup(values: &BTreeMap<String, f64>) -> impl Fn(&str) -> Option<f64> + '_ {
    move |name| values.get(name).copied()
}

#[must_use]
pub fn response_names(responses: &[ResponseSpec]) -> Vec<String> {
    unique_names(responses)
}
