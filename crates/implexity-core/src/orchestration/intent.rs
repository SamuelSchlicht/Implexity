// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::error::{CaeError, CaeResult};
use crate::py_repr::repr_str;
use crate::pyobj::{list_repr, py_eq, py_str, truthy};

use super::types::{ExternalPortValue, Fidelity, STRICT_CONTRACT_VERSION};

pub const RELATIONS: [&str; 5] = ["minimize", "maximize", "less_equal", "greater_equal", "equal"];


pub fn py_float(value: &Value) -> CaeResult<f64> {
    match value {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64().ok_or_else(|| CaeError::contract("int too large to convert to float")),
        Value::String(s) => {
            let t = s.trim();
            let cleaned: String = if t.contains('_') {
                let bytes = t.as_bytes();
                let ok = bytes.iter().enumerate().all(|(i, &b)| {
                    b != b'_'
                        || (i > 0
                            && i + 1 < bytes.len()
                            && bytes[i - 1].is_ascii_digit()
                            && bytes[i + 1].is_ascii_digit())
                });
                if ok { t.replace('_', "") } else { String::from("\u{0}") }
            } else {
                t.to_string()
            };
            let lower = cleaned.to_ascii_lowercase();
            let unsigned = lower.trim_start_matches(['+', '-']);
            let special = matches!(unsigned, "inf" | "infinity" | "nan");
            let plain = !cleaned.is_empty()
                && cleaned.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'));
            if (special || plain)
                && let Ok(v) = cleaned.parse::<f64>()
            {
                return Ok(v);
            }
            Err(CaeError::contract(format!("could not convert string to float: {}", repr_str(s))))
        }
        Value::Null => {
            Err(CaeError::contract("float() argument must be a string or a real number, not 'NoneType'"))
        }
        Value::Array(_) => {
            Err(CaeError::contract("float() argument must be a string or a real number, not 'list'"))
        }
        Value::Object(_) => {
            Err(CaeError::contract("float() argument must be a string or a real number, not 'dict'"))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntentGoal {
    pub response: String,
    pub relation: String,
    pub value: Option<f64>,
    pub weight: f64,
}

impl IntentGoal {

    pub fn new(response: &str, relation: &str, value: Option<f64>, weight: f64) -> CaeResult<Self> {
        if response.trim().is_empty() {
            return Err(CaeError::contract("intent goal response is required"));
        }
        if !RELATIONS.contains(&relation) {
            return Err(CaeError::contract(format!("unsupported intent relation {}", repr_str(relation))));
        }
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(CaeError::contract(format!(
                "intent goal {}: value must be finite",
                repr_str(response)
            )));
        }
        if !weight.is_finite() {
            return Err(CaeError::contract(format!(
                "intent goal {}: weight must be finite",
                repr_str(response)
            )));
        }
        Ok(Self { response: response.to_string(), relation: relation.to_string(), value, weight })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineeringIntent {
    pub goals: Vec<IntentGoal>,
    pub constraints: Vec<IntentGoal>,
    pub authoring: Map<String, Value>,
    pub application: Vec<String>,
    pub fidelity: Fidelity,
    pub provider_overrides: BTreeMap<String, String>,
    pub excluded_addins: Vec<String>,
    pub expert_mode: bool,
    pub external_ports: Vec<ExternalPortValue>,
    pub contract_version: i64,
    pub active_design_coordinates: Vec<String>,
    pub strict_fidelity: bool,
}

impl EngineeringIntent {

    pub fn with_goals(goals: Vec<IntentGoal>) -> CaeResult<Self> {
        let intent = Self {
            goals,
            constraints: Vec::new(),
            authoring: Map::new(),
            application: Vec::new(),
            fidelity: Fidelity::Intermediate,
            provider_overrides: BTreeMap::new(),
            excluded_addins: Vec::new(),
            expert_mode: false,
            external_ports: Vec::new(),
            contract_version: STRICT_CONTRACT_VERSION,
            active_design_coordinates: Vec::new(),
            strict_fidelity: true,
        };
        intent.validate()?;
        Ok(intent)
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.goals.is_empty() && self.constraints.is_empty() {
            return Err(CaeError::contract("engineering intent requires at least one goal or constraint"));
        }
        for (values, label) in
            [(&self.application, "application"), (&self.excluded_addins, "excluded_addins")]
        {
            if values.iter().any(String::is_empty) {
                return Err(CaeError::contract(format!("engineering intent {label} must be a tuple of ids")));
            }
        }
        if self.contract_version != 1 && self.contract_version != STRICT_CONTRACT_VERSION {
            return Err(CaeError::contract(format!(
                "unsupported engineering intent contract version {}",
                self.contract_version
            )));
        }
        let mut keys = std::collections::BTreeSet::new();
        if !self.external_ports.iter().all(|r| keys.insert(r.port.key())) {
            return Err(CaeError::contract("engineering intent has duplicate external port values"));
        }
        if self.active_design_coordinates.iter().any(|c| c.trim().is_empty()) {
            return Err(CaeError::contract(
                "engineering intent active_design_coordinates must be a tuple of non-empty ids",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if !self.active_design_coordinates.iter().all(|c| seen.insert(c)) {
            return Err(CaeError::contract("engineering intent active_design_coordinates must be unique"));
        }
        Ok(())
    }

    #[must_use]
    pub fn requested_responses(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for g in self.goals.iter().chain(&self.constraints) {
            if !out.contains(&g.response) {
                out.push(g.response.clone());
            }
        }
        out
    }


    #[allow(clippy::too_many_lines)]
    pub fn from_mapping(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("engineering intent must be an object"));
        };
        let goals = |key: &str| -> CaeResult<Vec<IntentGoal>> {
            let rows: Vec<Value> = match map.get(key) {
                None => Vec::new(),
                Some(v) if !truthy(v) => Vec::new(),
                Some(Value::Array(items)) => items.clone(),
                Some(Value::Object(m)) => m.keys().map(|k| Value::String(k.clone())).collect(),
                Some(Value::String(s)) => s.chars().map(|c| Value::String(c.to_string())).collect(),
                Some(other) => {
                    return Err(CaeError::contract(format!(
                        "'{}' object is not iterable",
                        crate::pyobj::type_name(other)
                    )));
                }
            };
            let mut out = Vec::new();
            for row in &rows {
                let Some(row) = row.as_object() else {
                    return Err(CaeError::contract("intent goals must be objects"));
                };
                let pick = |a: &str, b: &str| {
                    row.get(a).filter(|v| truthy(v)).or_else(|| row.get(b).filter(|v| truthy(v)))
                };
                let response = pick("response", "name").map(py_str).unwrap_or_default().trim().to_string();
                if response.is_empty() {
                    return Err(CaeError::contract("intent goal requires response"));
                }
                let rel =
                    pick("relation", "sense").map_or_else(|| "minimize".to_string(), py_str).to_lowercase();
                let rel = match rel.as_str() {
                    "minimise" => "minimize",
                    "maximise" => "maximize",
                    "upper" | "<=" => "less_equal",
                    "lower" | ">=" => "greater_equal",
                    "target" => "equal",
                    other => other,
                }
                .to_string();
                if !RELATIONS.contains(&rel.as_str()) {
                    return Err(CaeError::contract(format!(
                        "unsupported intent relation {}",
                        repr_str(&rel)
                    )));
                }
                let val = match row.get("value") {
                    Some(v) => Some(v),
                    None => match row.get("target") {
                        Some(v) => Some(v),
                        None => row.get("bound"),
                    },
                };
                let val = val.filter(|v| !v.is_null());
                if matches!(rel.as_str(), "less_equal" | "greater_equal" | "equal") && val.is_none() {
                    return Err(CaeError::contract(format!("{response}: {rel} requires a value")));
                }
                let value = val.map(py_float).transpose()?;
                let weight = py_float(row.get("weight").unwrap_or(&json!(1.0)))?;
                out.push(IntentGoal::new(&response, &rel, value, weight)?);
            }
            Ok(out)
        };
        let g = goals("goals")?;
        let c = goals("constraints")?;
        if g.is_empty() && c.is_empty() {
            return Err(CaeError::contract("engineering intent requires at least one goal or constraint"));
        }
        let version = match map.get("contract_version") {
            None => STRICT_CONTRACT_VERSION,
            Some(Value::Number(n)) if !n.is_f64() && matches!(n.as_i64(), Some(1 | 2)) => {
                n.as_i64().unwrap_or(2)
            }
            Some(v) => {
                return Err(CaeError::contract(format!(
                    "unsupported engineering intent contract version {}",
                    crate::pyobj::repr(v)
                )));
            }
        };
        if version == STRICT_CONTRACT_VERSION {
            const ALLOWED: [&str; 15] = [
                "contract_version",
                "goals",
                "constraints",
                "authoring",
                "application",
                "applications",
                "fidelity",
                "provider_overrides",
                "providerOverrides",
                "excluded_addins",
                "excludedAddins",
                "expert_mode",
                "external_ports",
                "active_design_coordinates",
                "strict_fidelity",
            ];
            let mut unknown: Vec<&String> = map.keys().filter(|k| !ALLOWED.contains(&k.as_str())).collect();
            unknown.sort();
            if !unknown.is_empty() {
                return Err(CaeError::contract(format!(
                    "strict engineering intent has unknown keys {}",
                    list_repr(&unknown)
                )));
            }
            if let Some(Value::Object(a)) = map.get("authoring")
                && a.contains_key("external_ports")
            {
                return Err(CaeError::contract(
                    "strict engineering intent requires external_ports at the top level",
                ));
            }
        }
        let fidelity = match map.get("fidelity") {
            None => Fidelity::Intermediate,
            Some(v) => Fidelity::from_value(v)?,
        };
        let app_raw = map
            .get("application")
            .filter(|v| truthy(v))
            .or_else(|| map.get("applications").filter(|v| truthy(v)));
        let app: Vec<Value> = match app_raw {
            None => Vec::new(),
            Some(Value::String(s)) => vec![Value::String(s.clone())],
            Some(Value::Array(items)) => items.clone(),
            Some(_) => {
                return Err(CaeError::contract("engineering intent application must be a list of ids"));
            }
        };
        let expert = map.get("expert_mode").cloned().unwrap_or(Value::Bool(false));
        let expert =
            crate::contracts::require_contract_bool(&expert, "engineering intent expert_mode", false)?
                .unwrap_or(false);
        let strict_fid = map.get("strict_fidelity").cloned().unwrap_or(Value::Bool(true));
        let strict_fid = crate::contracts::require_contract_bool(
            &strict_fid,
            "engineering intent strict_fidelity",
            false,
        )?
        .unwrap_or(true);
        let authoring = match map.get("authoring").filter(|v| truthy(v)) {
            None => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(CaeError::contract("engineering intent authoring must be an object")),
        };
        let external_raw = match map.get("external_ports") {
            Some(v) => v.clone(),
            None => authoring.get("external_ports").cloned().unwrap_or(Value::Array(Vec::new())),
        };
        let external_rows = match external_raw {
            v if !truthy(&v) => Vec::new(),
            Value::Array(items) => items,
            _ => return Err(CaeError::contract("engineering intent external_ports must be a list")),
        };
        let external =
            external_rows.iter().map(ExternalPortValue::from_mapping).collect::<CaeResult<Vec<_>>>()?;
        let overrides_raw = map
            .get("provider_overrides")
            .filter(|v| truthy(v))
            .or_else(|| map.get("providerOverrides").filter(|v| truthy(v)));
        let excluded_raw = map
            .get("excluded_addins")
            .filter(|v| truthy(v))
            .or_else(|| map.get("excludedAddins").filter(|v| truthy(v)));
        let overrides = match overrides_raw {
            None => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return Err(CaeError::contract("engineering intent provider_overrides must be an object"));
            }
        };
        let excluded: Vec<Value> = match excluded_raw {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => return Err(CaeError::contract("engineering intent excluded_addins must be a list")),
        };
        if version == STRICT_CONTRACT_VERSION
            && (app.iter().any(|x| !x.is_string())
                || excluded.iter().any(|x| !x.is_string())
                || overrides.values().any(|v| !v.is_string()))
        {
            return Err(CaeError::contract("strict engineering intent ids must be text"));
        }
        let coordinates = match map.get("active_design_coordinates") {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => {
                return Err(CaeError::contract(
                    "engineering intent active_design_coordinates must be a list of ids",
                ));
            }
        };
        if coordinates.iter().any(|v| !v.is_string()) {
            return Err(CaeError::contract(
                "engineering intent active_design_coordinates must contain text ids",
            ));
        }
        if map.contains_key("active_design_coordinates") && coordinates.is_empty() {
            return Err(CaeError::contract(
                "active_design_coordinates, when supplied, must be a nonempty explicit subset",
            ));
        }
        let mut provider_overrides = BTreeMap::new();
        for (k, v) in &overrides {
            let Some(v) = v.as_str() else {
                return Err(CaeError::contract(
                    "engineering intent provider_overrides must map response ids to add-in ids",
                ));
            };
            provider_overrides.insert(k.clone(), v.to_string());
        }
        let intent = Self {
            goals: g,
            constraints: c,
            authoring,
            application: app.iter().map(py_str).collect(),
            fidelity,
            provider_overrides,
            excluded_addins: excluded.iter().map(py_str).collect(),
            expert_mode: expert,
            external_ports: external,
            contract_version: version,
            active_design_coordinates: coordinates.iter().map(py_str).collect(),
            strict_fidelity: strict_fid,
        };
        intent.validate()?;
        Ok(intent)
    }

    #[must_use]
    pub fn version_is(&self, value: &Value) -> bool {
        py_eq(&json!(self.contract_version), value)
    }
}

