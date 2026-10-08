// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::contracts::ResponseSpec;
use implexity_core::numeric_contract::real_scalar;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::numeric::float_value;

pub const V18_SCHEMA: &str = "implexity-response-program/2";

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseProgram {
    pub objectives: Vec<ResponseSpec>,
    pub constraints: Vec<ResponseSpec>,
    pub normalisation: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProgramRow {
    Json(Value),
    Spec(ResponseSpec),
}

fn normalise_row(row: &ProgramRow, kind: &str) -> CaeResult<Option<ResponseSpec>> {
    let spec = match row {
        ProgramRow::Spec(s) => s.clone(),
        ProgramRow::Json(value) => {
            let Some(map) = value.as_object() else {
                return Err(CaeError::contract("response program rows must be objects"));
            };
            let mut data: Map<String, Value> = map.clone();
            let enabled = data.shift_remove("enabled").unwrap_or(Value::Bool(true));
            let Value::Bool(enabled) = enabled else {
                return Err(CaeError::contract("response enabled must be a Boolean"));
            };
            match data.shift_remove("id") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if !s.is_empty() => {}
                Some(_) => return Err(CaeError::contract("response UI identity must be nonempty text")),
            }
            if !enabled {
                return Ok(None);
            }
            match data.shift_remove("relation") {
                None | Some(Value::Null) => {}
                Some(relation) => {
                    let Value::String(relation) = relation else {
                        return Err(CaeError::contract("relation belongs only to a constraint row"));
                    };
                    if kind != "constraints" {
                        return Err(CaeError::contract("relation belongs only to a constraint row"));
                    }
                    let alias = |s: &str| -> String {
                        match s {
                            "<=" => "upper".into(),
                            ">=" => "lower".into(),
                            "=" => "equal".into(),
                            other => other.into(),
                        }
                    };
                    let sense = alias(&relation);
                    match data.get("sense") {
                        None | Some(Value::Null) => {}
                        Some(Value::String(declared)) => {
                            if alias(declared) != sense {
                                return Err(CaeError::contract("constraint relation and sense conflict"));
                            }
                        }
                        Some(_) => return Err(CaeError::contract("constraint relation and sense conflict")),
                    }
                    data.insert("sense".into(), Value::String(sense));
                }
            }
            ResponseSpec::from_dict(&Value::Object(data))?
        }
    };
    if kind == "constraints" && !spec.is_bounded() {
        return Err(CaeError::contract("constraint row must declare a bound"));
    }
    Ok(Some(spec))
}


pub fn normalise_response_program(value: &Value) -> CaeResult<ResponseProgram> {
    let Some(map) = value.as_object() else {
        return Err(CaeError::contract("response program must be a JSON object"));
    };
    let normalisation = match map.get("normalisation") {
        None => "response_scale".to_string(),
        Some(Value::String(s)) if s == "response_scale" => s.clone(),
        Some(_) => return Err(CaeError::contract("only explicit response_scale normalisation is supported")),
    };
    let mut groups: [Vec<ResponseSpec>; 2] = [Vec::new(), Vec::new()];
    for (slot, kind) in ["objectives", "constraints"].iter().enumerate() {
        let rows = match map.get(*kind) {
            None => Vec::new(),
            Some(Value::Array(rows)) => rows.iter().cloned().map(ProgramRow::Json).collect(),
            Some(_) => return Err(CaeError::contract(format!("response program {kind} must be a list"))),
        };
        for row in &rows {
            if let Some(spec) = normalise_row(row, kind)? {
                groups[slot].push(spec);
            }
        }
    }
    let [objectives, constraints] = groups;
    Ok(ResponseProgram { objectives, constraints, normalisation })
}


pub fn normalise_rows(objectives: &[ProgramRow], constraints: &[ProgramRow]) -> CaeResult<ResponseProgram> {
    let mut out = ResponseProgram {
        objectives: Vec::new(),
        constraints: Vec::new(),
        normalisation: "response_scale".into(),
    };
    for row in objectives {
        if let Some(s) = normalise_row(row, "objectives")? {
            out.objectives.push(s);
        }
    }
    for row in constraints {
        if let Some(s) = normalise_row(row, "constraints")? {
            out.constraints.push(s);
        }
    }
    Ok(out)
}

impl ResponseProgram {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(V18_SCHEMA.into()));
        m.insert(
            "objectives".into(),
            Value::Array(self.objectives.iter().map(ResponseSpec::to_value).collect()),
        );
        m.insert(
            "constraints".into(),
            Value::Array(self.constraints.iter().map(ResponseSpec::to_value).collect()),
        );
        m.insert("normalisation".into(), Value::String(self.normalisation.clone()));
        Value::Object(m)
    }
}


pub fn serialise_terms(program: &Value) -> CaeResult<Value> {
    Ok(normalise_response_program(program)?.to_value())
}


pub fn measurement_names(responses: &[ResponseSpec]) -> CaeResult<Vec<String>> {
    if responses.is_empty() {
        return Err(CaeError::contract("response program requires nonempty typed response specifications"));
    }
    let mut out: Vec<String> = Vec::new();
    for spec in responses {
        if !out.contains(&spec.name) {
            out.push(spec.name.clone());
        }
    }
    Ok(out)
}


pub fn combine_measurement_terms(terms: &[Value]) -> CaeResult<Vec<Value>> {
    if terms.is_empty() {
        return Err(CaeError::contract("response program requires nonempty measurement records"));
    }
    let allowed = ["response", "value", "objective_contribution", "operating_point"];
    let mut keys: Vec<(String, i64)> = Vec::new();
    let mut output: Vec<Map<String, Value>> = Vec::new();
    for term in terms {
        let Some(map) = term.as_object() else {
            return Err(CaeError::contract("malformed response measurement record"));
        };
        if !["response", "value", "objective_contribution"].iter().all(|k| map.contains_key(*k))
            || map.keys().any(|k| !allowed.contains(&k.as_str()))
        {
            return Err(CaeError::contract("malformed response measurement record"));
        }
        let name = match map.get("response") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => return Err(CaeError::contract("invalid response measurement identity")),
        };
        let point = match map.get("operating_point") {
            None => 0,
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => match n.as_i64() {
                Some(p) if p >= 0 => p,
                _ => return Err(CaeError::contract("invalid response measurement identity")),
            },
            Some(_) => return Err(CaeError::contract("invalid response measurement identity")),
        };
        let value = real_scalar(&map["value"], "response measurement value")?;
        let contribution = real_scalar(&map["objective_contribution"], "response role contribution")?;
        let key = (name, point);
        if let Some(pos) = keys.iter().position(|k| *k == key) {
            let old = &mut output[pos];
            let old_value = old.get("value").and_then(Value::as_f64).unwrap_or(f64::NAN);
            #[allow(clippy::float_cmp)]
            let differs = old_value != value;
            if differs || old.contains_key("operating_point") != map.contains_key("operating_point") {
                return Err(CaeError::contract(
                    "repeated response roles disagree on their physical measurement",
                ));
            }
            let previous = old.get("objective_contribution").and_then(Value::as_f64).unwrap_or(f64::NAN);
            let combined = implexity_core::numeric_contract::real_scalar_f64(
                previous + contribution,
                "combined response role contribution",
            )?;
            old.insert("objective_contribution".into(), float_value(combined));
        } else {
            let mut row = map.clone();
            row.insert("value".into(), float_value(value));
            row.insert("objective_contribution".into(), float_value(contribution));
            keys.push(key);
            output.push(row);
        }
    }
    Ok(output.into_iter().map(Value::Object).collect())
}

