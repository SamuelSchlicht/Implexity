// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::contracts::ResponseSpec;
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{PyNum, py_str, truthy};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::design_freedom::{DesignBlock, FREE_ROLES, active_coordinates};
use crate::numeric::{float_value, format_g6};
use crate::pyval::{first_truthy, py_float, py_int, raw_text_or, str_tuple, text_or};


pub fn check_limits(values: &[(String, f64)], limits: Option<&Value>) -> CaeResult<Value> {
    let mut issues: Vec<Value> = Vec::new();
    if let Some(Value::Object(limits)) = limits {
        for (name, bounds) in limits {
            let Some(v) = values.iter().find(|(k, _)| k == name).map(|(_, v)| *v) else {
                continue;
            };
            let Value::Object(b) = bounds else { continue };
            if let Some(lo) = b.get("min").filter(|x| !x.is_null())
                && v < py_float(lo)?
            {
                issues.push(Value::String(format!("{name}={} below {}", format_g6(v), py_str(lo))));
            }
            if let Some(hi) = b.get("max").filter(|x| !x.is_null())
                && v > py_float(hi)?
            {
                issues.push(Value::String(format!("{name}={} above {}", format_g6(v), py_str(hi))));
            }
        }
    }
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(issues.is_empty()));
    out.insert("issues".into(), Value::Array(issues));
    out.insert(
        "values".into(),
        Value::Object(values.iter().map(|(k, v)| (k.clone(), float_value(*v))).collect()),
    );
    Ok(Value::Object(out))
}

pub const ROBUST_MODES: [&str; 3] = ["nominal", "expected", "smooth_worst_case"];
pub const REGIME_ACTIONS: [&str; 3] = ["warn", "reject_step", "refuse"];

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleStage {
    pub id: String,
    pub provider: String,
    pub iterations: i64,
    pub released_blocks: Vec<String>,
    pub operating_points: Vec<i64>,
    pub robust_mode: String,
    pub robust_beta: f64,
    pub regime_action: String,
    pub fidelity: String,
    pub transition: String,
    pub validity_limits: Option<Map<String, Value>>,
    pub adaptive: Option<Map<String, Value>>,
    pub response_targets: Option<Vec<(String, f64)>>,
}

impl ScheduleStage {
    #[must_use]
    pub fn default_stage(provider: &str, released: Vec<String>) -> Self {
        Self {
            id: "default".into(),
            provider: provider.into(),
            iterations: 1,
            released_blocks: released,
            operating_points: vec![0],
            robust_mode: "nominal".into(),
            robust_beta: 20.0,
            regime_action: "warn".into(),
            fidelity: String::new(),
            transition: "carry".into(),
            validity_limits: None,
            adaptive: None,
            response_targets: None,
        }
    }


    pub fn from_dict(raw: &Value, default_provider: &str) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("every regime/fidelity stage must be a mapping"));
        };
        let ident = text_or(map, &["id"], "");
        if ident.is_empty() {
            return Err(CaeError::contract("every regime/fidelity stage requires a non-empty id"));
        }
        let rid = repr_str(&ident);
        let provider = text_or(map, &["provider"], default_provider);
        if provider.is_empty() {
            return Err(CaeError::contract(format!("stage {rid}: provider is required")));
        }
        let iterations = match map.get("iterations") {
            None => 1,
            Some(v) => py_int(v)?,
        };
        if iterations < 1 {
            return Err(CaeError::contract(format!("stage {rid}: iterations must be >= 1")));
        }
        let points = match first_truthy(map, &["operating_points", "operatingPoints"]) {
            None => vec![0],
            Some(Value::Array(items)) => items.iter().map(py_int).collect::<CaeResult<Vec<_>>>()?,
            Some(Value::String(s)) => {
                s.chars().map(|c| py_int(&Value::String(c.to_string()))).collect::<CaeResult<Vec<_>>>()?
            }
            Some(_) => {
                return Err(CaeError::contract(format!("stage {rid}: operating points must be a list")));
            }
        };
        if points.is_empty() || points.iter().any(|p| *p < 0) {
            return Err(CaeError::contract(format!(
                "stage {rid}: operating points must be non-negative indices"
            )));
        }
        let mode = raw_text_or(map, &["robust_mode", "robustMode"], "nominal");
        if !ROBUST_MODES.contains(&mode.as_str()) {
            return Err(CaeError::contract(format!(
                "stage {rid}: unsupported robust mode {}",
                repr_str(&mode)
            )));
        }
        let beta = match map.get("robust_beta").or_else(|| map.get("robustBeta")) {
            None => 20.0,
            Some(v) => py_float(v)?,
        };
        if beta <= 0.0 || beta.is_nan() {
            return Err(CaeError::contract(format!("stage {rid}: robust_beta must be > 0")));
        }
        let action = raw_text_or(map, &["regime_action", "regimeAction"], "warn");
        if !REGIME_ACTIONS.contains(&action.as_str()) {
            return Err(CaeError::contract(format!(
                "stage {rid}: unsupported regime action {}",
                repr_str(&action)
            )));
        }
        let transition = raw_text_or(map, &["transition"], "carry");
        if transition != "carry" && transition != "reinitialize_physics" {
            return Err(CaeError::contract(format!(
                "stage {rid}: unsupported transition {}",
                repr_str(&transition)
            )));
        }
        let limits = match first_truthy(map, &["validity_limits", "validityLimits"]) {
            None => None,
            Some(Value::Object(m)) => Some(m.clone()),
            Some(_) => {
                return Err(CaeError::contract(format!("stage {rid}: validity limits must be a mapping")));
            }
        };
        let adaptive = match map.get("adaptive") {
            None | Some(Value::Null) => None,
            Some(Value::Object(m)) => Some(m.clone()),
            Some(_) => {
                return Err(CaeError::contract(format!("stage {rid}: adaptive policy must be a mapping")));
            }
        };
        let released = match first_truthy(map, &["released_blocks", "releasedBlocks"]) {
            None => Vec::new(),
            Some(v) => str_tuple(v, &format!("stage {rid} released blocks"))?,
        };
        let fidelity = match first_truthy(map, &["fidelity"]) {
            None => String::new(),
            Some(v) => py_str(v),
        };
        let response_targets = parse_response_targets(map, &rid)?;
        Ok(Self {
            id: ident,
            provider,
            iterations,
            released_blocks: released,
            operating_points: points,
            robust_mode: mode,
            robust_beta: beta,
            regime_action: action,
            fidelity,
            transition,
            validity_limits: limits,
            adaptive,
            response_targets,
        })
    }


    pub fn stage_responses(&self, responses: &[ResponseSpec]) -> CaeResult<Vec<ResponseSpec>> {
        let mut out = responses.to_vec();
        let Some(targets) = &self.response_targets else { return Ok(out) };
        for (name, target) in targets {
            let mut matches = out.iter_mut().filter(|r| r.name == *name && r.is_bounded());
            let (Some(spec), None) = (matches.next(), matches.next()) else {
                return Err(CaeError::contract(format!(
                    "stage {}: response target {} names no single bounded response of the program",
                    repr_str(&self.id),
                    repr_str(name)
                )));
            };
            spec.target = Some(PyNum::Float(*target));
            spec.validate()?;
        }
        Ok(out)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), Value::String(self.id.clone()));
        m.insert("provider".into(), Value::String(self.provider.clone()));
        m.insert("iterations".into(), Value::from(self.iterations));
        m.insert(
            "released_blocks".into(),
            Value::Array(self.released_blocks.iter().cloned().map(Value::String).collect()),
        );
        m.insert(
            "operating_points".into(),
            Value::Array(self.operating_points.iter().map(|p| Value::from(*p)).collect()),
        );
        m.insert("robust_mode".into(), Value::String(self.robust_mode.clone()));
        m.insert("robust_beta".into(), float_value(self.robust_beta));
        m.insert("regime_action".into(), Value::String(self.regime_action.clone()));
        m.insert("fidelity".into(), Value::String(self.fidelity.clone()));
        m.insert("transition".into(), Value::String(self.transition.clone()));
        m.insert("validity_limits".into(), self.validity_limits.clone().map_or(Value::Null, Value::Object));
        m.insert("adaptive".into(), self.adaptive.clone().map_or(Value::Null, Value::Object));
        if let Some(targets) = &self.response_targets {
            m.insert(
                "response_targets".into(),
                Value::Object(targets.iter().map(|(k, v)| (k.clone(), float_value(*v))).collect()),
            );
        }
        Value::Object(m)
    }
}

fn parse_response_targets(map: &Map<String, Value>, rid: &str) -> CaeResult<Option<Vec<(String, f64)>>> {
    match map.get("response_targets").or_else(|| map.get("responseTargets")) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) if !m.is_empty() => {
            let mut rows = Vec::with_capacity(m.len());
            for (name, value) in m {
                let target = match value {
                    Value::Number(_) => value.as_f64().filter(|v| v.is_finite()),
                    _ => None,
                };
                let Some(target) = target.filter(|_| !name.trim().is_empty()) else {
                    return Err(CaeError::contract(format!(
                        "stage {rid}: response_targets must map response names to finite numbers"
                    )));
                };
                rows.push((name.clone(), target));
            }
            Ok(Some(rows))
        }
        Some(_) => Err(CaeError::contract(format!(
            "stage {rid}: response_targets must be a nonempty mapping of response names to finite numbers"
        ))),
    }
}


pub fn validate_schedule(
    raw: Option<&[Value]>,
    default_provider: &str,
    blocks: &[DesignBlock],
    coordinates: &[String],
    operating_point_count: Option<usize>,
) -> CaeResult<Vec<ScheduleStage>> {
    let raw = raw.unwrap_or_default();
    if raw.is_empty() {
        let released = if blocks.is_empty() {
            Vec::new()
        } else {
            blocks.iter().filter(|b| FREE_ROLES.contains(&b.role.as_str())).map(|b| b.id.clone()).collect()
        };
        return Ok(vec![ScheduleStage::default_stage(default_provider, released)]);
    }
    let stages =
        raw.iter().map(|x| ScheduleStage::from_dict(x, default_provider)).collect::<CaeResult<Vec<_>>>()?;
    let mut ids: Vec<&str> = stages.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != stages.len() {
        return Err(CaeError::contract("regime/fidelity stage ids must be unique"));
    }
    for stage in &stages {
        active_coordinates(blocks, Some(&stage.released_blocks), coordinates)?;
        let top = stage.operating_points.iter().copied().max().unwrap_or(0);
        if let Some(count) = operating_point_count
            && usize::try_from(top).is_ok_and(|t| t >= count)
        {
            return Err(CaeError::contract(format!(
                "stage {} references operating point {top}, but the problem declares only {count}",
                repr_str(&stage.id)
            )));
        }
    }
    Ok(stages)
}

#[must_use]
pub fn is_truthy(value: &Value) -> bool {
    truthy(value)
}

