// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::contracts::{CaeProvider, ProviderProblem, TOPOLOGY_COORDINATE};
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::design::{NamedArrays, design_identity, require_finite};
use crate::numeric::float_value;
use crate::provider_ops::{AdmissionReply, CandidateDesign, DesignOp, design_operations};


pub fn opaque_diagnostics(value: &Value, path: &str, depth: usize) -> CaeResult<Value> {
    if depth > 16 {
        return Err(CaeError::contract(format!("candidate admission {path} is nested too deeply")));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(value.clone()),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() || n.as_f64().is_some_and(f64::is_finite) {
                Ok(value.clone())
            } else {
                Err(CaeError::contract(format!(
                    "candidate admission {path} must contain only finite numbers"
                )))
            }
        }
        Value::Object(map) => {
            if map.len() > 10_000 {
                return Err(CaeError::contract(format!(
                    "candidate admission {path} contains too many entries"
                )));
            }
            let mut out = Map::new();
            for (key, item) in map {
                if key.is_empty() {
                    return Err(CaeError::contract(format!(
                        "candidate admission {path} keys must be nonempty text"
                    )));
                }
                out.insert(key.clone(), opaque_diagnostics(item, &format!("{path}.{key}"), depth + 1)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => {
            if items.len() > 10_000 {
                return Err(CaeError::contract(format!(
                    "candidate admission {path} contains too many entries"
                )));
            }
            items
                .iter()
                .enumerate()
                .map(|(i, item)| opaque_diagnostics(item, &format!("{path}[{i}]"), depth + 1))
                .collect::<CaeResult<Vec<_>>>()
                .map(Value::Array)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateAdmission {
    pub allow: bool,
    pub reason: String,
    pub max_step_scale: f64,
    pub event: Option<String>,
    pub relinearize_after_accept: bool,
    pub current_design_state_id: Option<String>,
    pub candidate_design_state_id: Option<String>,
    pub diagnostics: Option<Value>,
}

impl Default for CandidateAdmission {
    fn default() -> Self {
        Self {
            allow: true,
            reason: String::new(),
            max_step_scale: 1.0,
            event: None,
            relinearize_after_accept: false,
            current_design_state_id: None,
            candidate_design_state_id: None,
            diagnostics: None,
        }
    }
}

fn aliased<'a>(map: &'a Map<String, Value>, key: &str, alias: Option<&str>) -> CaeResult<Option<&'a Value>> {
    if let Some(a) = alias
        && map.contains_key(key)
        && map.contains_key(a)
    {
        return Err(CaeError::contract(format!(
            "candidate admission cannot contain both {} and {}",
            repr_str(key),
            repr_str(a)
        )));
    }
    Ok(map.get(key).or_else(|| alias.and_then(|a| map.get(a))))
}

fn strict_bool(map: &Map<String, Value>, key: &str, alias: Option<&str>, default: bool) -> CaeResult<bool> {
    match aliased(map, key, alias)? {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => {
            Err(CaeError::contract(format!("candidate admission {} must be a boolean", repr_str(key))))
        }
    }
}

fn optional_id(value: Option<&Value>, label: &str) -> CaeResult<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => {
            Err(CaeError::contract(format!("candidate admission {label} must be nonempty text or null")))
        }
    }
}

impl CandidateAdmission {

    #[allow(clippy::too_many_lines)]
    pub fn from_any(
        value: &AdmissionReply,
        current_design_state_id: Option<&str>,
        candidate_design_state_id: Option<&str>,
        require_identity_evidence: bool,
    ) -> CaeResult<Self> {
        let with_ids = || Self {
            current_design_state_id: current_design_state_id.map(str::to_string),
            candidate_design_state_id: candidate_design_state_id.map(str::to_string),
            ..Self::default()
        };
        let out = match value {
            AdmissionReply::Unspecified => {
                if require_identity_evidence {
                    return Err(CaeError::contract(
                        "candidate admission omitted required design-state identity evidence",
                    ));
                }
                return Ok(with_ids());
            }
            AdmissionReply::Typed(t) => t.clone(),
            AdmissionReply::Allow(b) => {
                if require_identity_evidence {
                    return Err(CaeError::contract(
                        "candidate admission boolean has no design-state identity evidence",
                    ));
                }
                Self { allow: *b, ..with_ids() }
            }
            AdmissionReply::Record(Value::Bool(b)) => {
                return Self::from_any(
                    &AdmissionReply::Allow(*b),
                    current_design_state_id,
                    candidate_design_state_id,
                    require_identity_evidence,
                );
            }
            AdmissionReply::Record(Value::Null) => {
                return Self::from_any(
                    &AdmissionReply::Unspecified,
                    current_design_state_id,
                    candidate_design_state_id,
                    require_identity_evidence,
                );
            }
            AdmissionReply::Record(Value::Object(map)) => {
                if require_identity_evidence && !map.contains_key("allow") {
                    return Err(CaeError::contract("candidate admission omitted explicit allow decision"));
                }
                let reason = match map.get("reason") {
                    None => String::new(),
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err(CaeError::contract("candidate admission reason must be text")),
                };
                let event = match map.get("event") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => {
                        return Err(CaeError::contract("candidate admission event must be text or null"));
                    }
                };
                let current_id = aliased(map, "current_design_state_id", Some("currentDesignStateId"))?;
                let candidate_id = aliased(map, "candidate_design_state_id", Some("candidateDesignStateId"))?;
                let diagnostics = match aliased(map, "diagnostics", Some("details"))? {
                    None | Some(Value::Null) => None,
                    Some(v @ Value::Object(_)) => Some(opaque_diagnostics(v, "diagnostics", 0)?),
                    Some(_) => {
                        return Err(CaeError::contract(
                            "candidate admission diagnostics must be a mapping or null",
                        ));
                    }
                };
                let current_id = optional_id(current_id, "current_design_state_id")?;
                let candidate_id = optional_id(candidate_id, "candidate_design_state_id")?;
                let scale = match aliased(map, "max_step_scale", Some("maxStepScale"))? {
                    None => 1.0,
                    Some(Value::Bool(_) | Value::Null | Value::Array(_) | Value::Object(_)) => {
                        return Err(CaeError::contract("candidate admission max_step_scale must be numeric"));
                    }
                    Some(v) => implexity_core::orchestration::py_float(v).map_err(|_| {
                        CaeError::contract("candidate admission max_step_scale must be numeric")
                    })?,
                };
                Self {
                    allow: strict_bool(map, "allow", None, true)?,
                    reason,
                    max_step_scale: scale,
                    event,
                    relinearize_after_accept: strict_bool(
                        map,
                        "relinearize_after_accept",
                        Some("relinearizeAfterAccept"),
                        false,
                    )?,
                    current_design_state_id: current_id,
                    candidate_design_state_id: candidate_id,
                    diagnostics,
                }
            }
            AdmissionReply::Record(_) => {
                return Err(CaeError::contract(
                    "candidate admission must be bool, mapping or CandidateAdmission",
                ));
            }
        };
        for (label, ident) in [
            ("current_design_state_id", &out.current_design_state_id),
            ("candidate_design_state_id", &out.candidate_design_state_id),
        ] {
            if ident.as_deref() == Some("") {
                return Err(CaeError::contract(format!(
                    "candidate admission {label} must be nonempty text or null"
                )));
            }
        }
        let diagnostics = match &out.diagnostics {
            None => None,
            Some(d) => Some(opaque_diagnostics(d, "diagnostics", 0)?),
        };
        let scale = out.max_step_scale;
        if scale.is_nan() || scale <= 0.0 || scale > 1.0 {
            return Err(CaeError::contract("max_step_scale must lie in (0,1]"));
        }
        if let Some(expected) = current_design_state_id {
            if out.current_design_state_id.is_none() && require_identity_evidence {
                return Err(CaeError::contract("candidate admission omitted current_design_state_id"));
            }
            if out.current_design_state_id.as_deref().is_some_and(|id| id != expected) {
                return Err(CaeError::contract(
                    "candidate admission current_design_state_id is stale or mismatched",
                ));
            }
        }
        if let Some(expected) = candidate_design_state_id {
            if out.candidate_design_state_id.is_none() && require_identity_evidence {
                return Err(CaeError::contract("candidate admission omitted candidate_design_state_id"));
            }
            if out.candidate_design_state_id.as_deref().is_some_and(|id| id != expected) {
                return Err(CaeError::contract(
                    "candidate admission candidate_design_state_id is stale or mismatched",
                ));
            }
        }
        Ok(Self {
            current_design_state_id: out
                .current_design_state_id
                .clone()
                .or_else(|| current_design_state_id.map(str::to_string)),
            candidate_design_state_id: out
                .candidate_design_state_id
                .clone()
                .or_else(|| candidate_design_state_id.map(str::to_string)),
            diagnostics,
            ..out
        })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("allow".into(), Value::Bool(self.allow));
        m.insert("reason".into(), Value::String(self.reason.clone()));
        m.insert("max_step_scale".into(), float_value(self.max_step_scale));
        m.insert("event".into(), self.event.clone().map_or(Value::Null, Value::String));
        m.insert("relinearize_after_accept".into(), Value::Bool(self.relinearize_after_accept));
        m.insert(
            "current_design_state_id".into(),
            self.current_design_state_id.clone().map_or(Value::Null, Value::String),
        );
        m.insert(
            "candidate_design_state_id".into(),
            self.candidate_design_state_id.clone().map_or(Value::Null, Value::String),
        );
        m.insert("diagnostics".into(), self.diagnostics.clone().unwrap_or(Value::Null));
        Value::Object(m)
    }
}


pub fn candidate_identity(value: &CandidateDesign) -> CaeResult<String> {
    match value {
        CandidateDesign::Named(named) => {
            if named.is_empty() || named.names().iter().any(String::is_empty) {
                return Err(CaeError::contract(
                    "candidate named designs require nonempty text coordinate ids",
                ));
            }
            for (k, v) in named.iter() {
                require_finite(v, &format!("candidate coordinate {}", repr_str(k)))?;
            }
            design_identity(named)
        }
        CandidateDesign::Array(a) => {
            require_finite(a, "candidate design")?;
            if a.is_empty() {
                return Err(CaeError::contract("candidate designs must be finite and nonempty"));
            }
            design_identity(&NamedArrays::single(TOPOLOGY_COORDINATE, a.clone()))
        }
    }
}


pub fn provider_candidate_admission(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    current: &CandidateDesign,
    trial: &CandidateDesign,
    operation: DesignOp,
    require_operation: bool,
    require_identity_evidence: bool,
) -> CaeResult<CandidateAdmission> {
    let current_id = candidate_identity(current)?;
    let candidate_id = candidate_identity(trial)?;
    let ops = design_operations(provider).filter(|ops| ops.provides(operation));
    let Some(ops) = ops else {
        if require_operation {
            return Err(CaeError::contract(format!(
                "provider declares required {} operation but it is unavailable",
                operation.name()
            )));
        }
        return CandidateAdmission::from_any(
            &AdmissionReply::Unspecified,
            Some(&current_id),
            Some(&candidate_id),
            false,
        );
    };
    let raw = ops.candidate_admission(operation, problem, &current.clone(), &trial.clone())?;
    CandidateAdmission::from_any(&raw, Some(&current_id), Some(&candidate_id), require_identity_evidence)
}

