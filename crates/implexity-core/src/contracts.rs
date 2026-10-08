// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use ndarray::ArrayD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{CaeError, CaeResult};
use crate::orchestration::PublishedContract;
use crate::py_repr::repr_str;
use crate::pyobj::{PyNum, list_repr, repr};

pub const TOPOLOGY_COORDINATE: &str = "model:control";

pub const PROVIDER_API_VERSION: i64 = 1;



pub fn require_contract_bool(value: &Value, path: &str, allow_unknown: bool) -> CaeResult<Option<bool>> {
    match value {
        Value::Null if allow_unknown => Ok(None),
        Value::Bool(b) => Ok(Some(*b)),
        _ => {
            let suffix = if allow_unknown { " or null/unknown" } else { "" };
            Err(CaeError::contract(format!("{path} must be a boolean{suffix}")))
        }
    }
}

fn check_ids(provider: &str, key: &str, values: &[String], allow_duplicates: bool) -> CaeResult<()> {
    if values.iter().any(String::is_empty) {
        return Err(CaeError::contract(format!(
            "provider {}: {key} must be a tuple of ids",
            repr_str(provider)
        )));
    }
    if !allow_duplicates {
        let mut seen = std::collections::BTreeSet::new();
        if !values.iter().all(|v| seen.insert(v)) {
            return Err(CaeError::contract(format!(
                "provider {}: {key} contains duplicate ids",
                repr_str(provider)
            )));
        }
    }
    Ok(())
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub name: String,
    pub analyses: Vec<String>,
    pub responses: Vec<String>,
    pub fields: Vec<String>,
    pub sensitivities: bool,
    pub nonlinear: bool,
    pub notes: Vec<String>,
    pub design_coordinates: Vec<String>,
    pub distributable: bool,
    pub mathematical_structures: Vec<String>,
    pub response_metadata: Map<String, Value>,
    pub traits: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub presentation: Map<String, Value>,
}

impl ProviderDescriptor {
    pub fn new(name: impl Into<String>, analyses: Vec<String>, responses: Vec<String>) -> Self {
        Self {
            name: name.into(),
            analyses,
            responses,
            fields: Vec::new(),
            sensitivities: true,
            nonlinear: false,
            notes: Vec::new(),
            design_coordinates: Vec::new(),
            distributable: true,
            mathematical_structures: Vec::new(),
            response_metadata: Map::new(),
            traits: Map::new(),
            presentation: Map::new(),
        }
    }


    pub fn with_presentation(mut self, editor: Value, execution: &str) -> CaeResult<Self> {
        self.presentation.insert("editor".into(), editor);
        self.presentation.insert("execution".into(), json!(execution));
        self.checked()
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.name.trim().is_empty() {
            return Err(CaeError::contract("provider capability name must be non-empty text"));
        }
        for (key, values) in [
            ("analyses", &self.analyses),
            ("responses", &self.responses),
            ("fields", &self.fields),
            ("notes", &self.notes),
            ("design_coordinates", &self.design_coordinates),
            ("mathematical_structures", &self.mathematical_structures),
        ] {
            check_ids(&self.name, key, values, key == "notes")?;
        }
        let name = repr_str(&self.name);
        for (response, metadata) in &self.response_metadata {
            if !self.responses.contains(response) {
                return Err(CaeError::contract(format!(
                    "provider {name}: metadata names undeclared response {}",
                    repr_str(response)
                )));
            }
            let Some(metadata) = metadata.as_object() else {
                return Err(CaeError::contract(format!(
                    "provider {name}: response metadata for {} must be a mapping",
                    repr_str(response)
                )));
            };
            for key in ["label", "unit", "description", "family"] {
                if metadata.get(key).is_some_and(|v| !v.is_string()) {
                    return Err(CaeError::contract(format!(
                        "provider {name}: response {} metadata {} must be text",
                        repr_str(response),
                        repr_str(key)
                    )));
                }
            }
            for key in ["differentiable", "design_reachable"] {
                if let Some(v) = metadata.get(key) {
                    require_contract_bool(
                        v,
                        &format!(
                            "provider {name}: response {} metadata {}",
                            repr_str(response),
                            repr_str(key)
                        ),
                        false,
                    )?;
                }
            }
            if let Some(dep) = metadata.get("depends_on")
                && !dep.as_array().is_some_and(|a| a.iter().all(Value::is_string))
            {
                return Err(CaeError::contract(format!(
                    "provider {name}: response {} metadata 'depends_on' must be text entries",
                    repr_str(response)
                )));
            }
        }
        Ok(())
    }


    pub fn checked(self) -> CaeResult<Self> {
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.name));
        m.insert("analyses".into(), json!(self.analyses));
        m.insert("responses".into(), json!(self.responses));
        m.insert("fields".into(), json!(self.fields));
        m.insert("sensitivities".into(), json!(self.sensitivities));
        m.insert("nonlinear".into(), json!(self.nonlinear));
        m.insert("notes".into(), json!(self.notes));
        m.insert("design_coordinates".into(), json!(self.design_coordinates));
        m.insert("distributable".into(), json!(self.distributable));
        m.insert("mathematical_structures".into(), json!(self.mathematical_structures));
        m.insert("response_metadata".into(), Value::Object(self.response_metadata.clone()));
        m.insert("traits".into(), Value::Object(self.traits.clone()));
        for (k, v) in &self.presentation {
            m.insert(k.clone(), v.clone());
        }
        m
    }
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacySingleArrayProviderCapabilities {
    pub base: ProviderDescriptor,
    pub topology_coordinate: String,
    pub provider_api: i64,
    pub execution: String,
    pub editor: Map<String, Value>,
    pub condition_types: Vec<String>,
    pub material_model: Option<String>,
    pub compatibility_routes: Vec<String>,
    pub compatibility_contract: String,
}

impl LegacySingleArrayProviderCapabilities {
    pub fn new(name: impl Into<String>, analyses: Vec<String>, responses: Vec<String>) -> Self {
        let mut base = ProviderDescriptor::new(name, analyses, responses);
        base.design_coordinates = vec![TOPOLOGY_COORDINATE.to_string()];
        Self {
            base,
            topology_coordinate: TOPOLOGY_COORDINATE.into(),
            provider_api: PROVIDER_API_VERSION,
            execution: "array".into(),
            editor: Map::new(),
            condition_types: Vec::new(),
            material_model: None,
            compatibility_routes: Vec::new(),
            compatibility_contract: "legacy_single_array_v1".into(),
        }
    }


    pub fn validate(&self) -> CaeResult<()> {
        self.base.validate()?;
        let name = repr_str(&self.base.name);
        for (key, values) in
            [("condition_types", &self.condition_types), ("compatibility_routes", &self.compatibility_routes)]
        {
            if values.iter().any(String::is_empty) {
                return Err(CaeError::contract(format!(
                    "legacy provider {name}: {key} must be a tuple of ids"
                )));
            }
            let mut seen = std::collections::BTreeSet::new();
            if !values.iter().all(|v| seen.insert(v)) {
                return Err(CaeError::contract(format!(
                    "legacy provider {name}: {key} contains duplicate ids"
                )));
            }
        }
        if self.provider_api != PROVIDER_API_VERSION {
            return Err(CaeError::contract(format!(
                "legacy provider {name}: unsupported provider API {}; runtime supports {PROVIDER_API_VERSION}",
                self.provider_api
            )));
        }
        if self.execution != "array" && self.execution != "implicit_job" {
            return Err(CaeError::contract(format!(
                "legacy provider {name}: execution must be 'array' or 'implicit_job'"
            )));
        }
        if self.compatibility_contract != "legacy_single_array_v1" {
            return Err(CaeError::contract(format!(
                "legacy provider {name}: compatibility contract marker is immutable"
            )));
        }
        if self.topology_coordinate.is_empty() {
            return Err(CaeError::contract(format!(
                "legacy provider {name}: topology_coordinate must be text"
            )));
        }
        if self.base.design_coordinates.iter().filter(|c| **c == self.topology_coordinate).count() != 1 {
            return Err(CaeError::contract(format!(
                "legacy provider {name}: topology coordinate must occur exactly once; its position has no canonical significance"
            )));
        }
        for (response, metadata) in &self.base.response_metadata {
            if let Some(v) = metadata.get("topology_reachable") {
                require_contract_bool(
                    v,
                    &format!(
                        "legacy provider {name}: response {} metadata 'topology_reachable'",
                        repr_str(response)
                    ),
                    false,
                )?;
            }
        }
        Ok(())
    }


    pub fn checked(self) -> CaeResult<Self> {
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let mut m = self.base.to_map();
        m.insert("topology_coordinate".into(), json!(self.topology_coordinate));
        m.insert("provider_api".into(), json!(self.provider_api));
        m.insert("execution".into(), json!(self.execution));
        m.insert("editor".into(), Value::Object(self.editor.clone()));
        m.insert("condition_types".into(), json!(self.condition_types));
        m.insert("material_model".into(), json!(self.material_model));
        m.insert("compatibility_routes".into(), json!(self.compatibility_routes));
        m.insert("compatibility_contract".into(), json!(self.compatibility_contract));
        m
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderCapabilities {
    Descriptor(Box<ProviderDescriptor>),
    Legacy(Box<LegacySingleArrayProviderCapabilities>),
    Mapping(Map<String, Value>),
}

impl ProviderCapabilities {
    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        match self {
            Self::Descriptor(d) => d.to_map(),
            Self::Legacy(l) => l.to_map(),
            Self::Mapping(m) => m.clone(),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<Value> {
        self.to_map().get(key).cloned()
    }

    #[must_use]
    pub fn design_coordinates(&self) -> Vec<String> {
        match self {
            Self::Descriptor(d) => d.design_coordinates.clone(),
            Self::Legacy(l) => l.base.design_coordinates.clone(),
            Self::Mapping(m) => m
                .get("design_coordinates")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        }
    }
}

pub const REMOVED_HARD_CONSTRAINT_KEYS: [(&str, &str); 7] = [
    (
        "enforcement",
        "response 'enforcement' no longer exists; every bounded response (sense upper/lower/equal) is a penalty term weighted by 'weight'",
    ),
    (
        "tolerance",
        "response acceptance 'tolerance' no longer exists; write the admissible limit directly as the response bound",
    ),
    (
        "constraint_search",
        "the explicit elastic-SQP 'constraint_search' no longer exists; optimization always uses the penalty search",
    ),
    (
        "volume_fraction_upper",
        "the mean/volume-fraction cap projection no longer exists; declare a volume response with sense 'upper'",
    ),
    (
        "volume_fraction",
        "the mean/volume-fraction cap projection no longer exists; declare a volume response with sense 'upper'",
    ),
    (
        "bounded_mean_upper",
        "the bounded-mean cap projection no longer exists; declare the mean as a response with sense 'upper'",
    ),
    (
        "design_feasibility",
        "the design-only feasibility projection no longer exists; declare the design-only response with sense 'upper'",
    ),
];


pub fn reject_removed_constraint_keys(value: &Value, where_: &str) -> CaeResult<()> {
    let Some(map) = value.as_object() else { return Ok(()) };
    let stale: Vec<(&str, &str)> =
        REMOVED_HARD_CONSTRAINT_KEYS.iter().copied().filter(|(k, _)| map.contains_key(*k)).collect();
    if stale.is_empty() {
        return Ok(());
    }
    let details: Vec<String> = stale.iter().map(|(k, m)| format!("{}: {m}", repr_str(k))).collect();
    let keys: Vec<&str> = stale.iter().map(|(k, _)| *k).collect();
    Err(CaeError::contract(format!(
        "{where_}: hard constraints were removed from Implexity and response bounds are penalty terms only. Remove {}. {}.",
        list_repr(&keys),
        details.join("; ")
    )))
}

pub const RESPONSE_SENSES: [&str; 5] = ["minimise", "maximise", "upper", "lower", "equal"];


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseSpec {
    pub name: String,
    pub weight: PyNum,
    pub sense: String,
    pub target: Option<PyNum>,
    pub scale: PyNum,
}

impl ResponseSpec {

    pub fn new(
        name: &str,
        weight: PyNum,
        sense: &str,
        target: Option<PyNum>,
        scale: PyNum,
    ) -> CaeResult<Self> {
        let spec = Self { name: name.to_string(), weight, sense: sense.to_string(), target, scale };
        spec.validate()?;
        Ok(spec)
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.name.trim().is_empty() {
            return Err(CaeError::contract("every response requires a nonempty name"));
        }
        let name = repr_str(&self.name);
        if !RESPONSE_SENSES.contains(&self.sense.as_str()) {
            return Err(CaeError::contract(format!(
                "response {name}: unsupported sense {}",
                repr_str(&self.sense)
            )));
        }
        for (key, value) in
            [("weight", Some(self.weight)), ("scale", Some(self.scale)), ("target", self.target)]
        {
            if let Some(v) = value
                && !v.is_finite()
            {
                return Err(CaeError::contract(format!(
                    "response {name}: {key} must be a finite real number"
                )));
            }
        }
        if self.weight.as_f64() < 0.0 || self.scale.as_f64() <= 0.0 {
            return Err(CaeError::contract(format!(
                "response {name}: weight must be non-negative and scale positive"
            )));
        }
        let bounded = matches!(self.sense.as_str(), "upper" | "lower" | "equal");
        if bounded && self.target.is_none() {
            return Err(CaeError::contract(format!(
                "response {name}: sense {} requires target/bound",
                repr_str(&self.sense)
            )));
        }
        if bounded && self.weight.as_f64() <= 0.0 {
            return Err(CaeError::contract(format!(
                "response {name}: bounded sense {} requires a positive weight; the weight sets the bound's initial augmented-Lagrangian penalty 2*weight",
                repr_str(&self.sense)
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn is_bounded(&self) -> bool {
        matches!(self.sense.as_str(), "upper" | "lower" | "equal")
    }


    #[allow(clippy::too_many_lines)]
    pub fn from_dict(value: &Value) -> CaeResult<Self> {
        const ALLOWED: [&str; 8] =
            ["name", "response", "response_id", "weight", "sense", "target", "bound", "scale"];
        let Some(map) = value.as_object() else {
            return Err(CaeError::contract("response specification must be a mapping"));
        };
        let label = ["name", "response", "response_id"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()));
        let where_ = label
            .map_or_else(|| "response specification".to_string(), |l| format!("response {}", repr_str(l)));
        reject_removed_constraint_keys(value, &where_)?;
        if map.keys().any(|k| !ALLOWED.contains(&k.as_str())) {
            return Err(CaeError::contract("response specification contains unknown fields"));
        }
        let aliases: Vec<&Value> =
            ["name", "response", "response_id"].iter().filter_map(|k| map.get(*k)).collect();
        let mut distinct: Vec<&str> = Vec::new();
        for alias in &aliases {
            match alias.as_str() {
                Some(s) if !s.trim().is_empty() => {
                    if !distinct.contains(&s) {
                        distinct.push(s);
                    }
                }
                _ => {
                    return Err(CaeError::contract(
                        "response names must be nonempty consistent text aliases",
                    ));
                }
            }
        }
        if distinct.len() > 1 {
            return Err(CaeError::contract("response names must be nonempty consistent text aliases"));
        }
        if let (Some(t), Some(b)) = (map.get("target"), map.get("bound"))
            && !crate::pyobj::py_eq(t, b)
        {
            return Err(CaeError::contract("response target/bound aliases conflict"));
        }
        let name = ["name", "response", "response_id"]
            .iter()
            .find_map(|k| map.get(*k).filter(|v| crate::pyobj::truthy(v)))
            .map(crate::pyobj::py_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if name.is_empty() {
            return Err(CaeError::contract("every response requires name/response/response_id"));
        }
        let raw_sense = map
            .get("sense")
            .filter(|v| crate::pyobj::truthy(v))
            .map_or_else(|| "minimise".to_string(), crate::pyobj::py_str)
            .to_lowercase();
        let sense = match raw_sense.as_str() {
            "minimize" => "minimise",
            "maximize" => "maximise",
            "target" | "=" => "equal",
            "<=" => "upper",
            ">=" => "lower",
            other => other,
        }
        .to_string();
        if !RESPONSE_SENSES.contains(&sense.as_str()) {
            return Err(CaeError::contract(format!(
                "response {}: unsupported sense {}",
                repr_str(&name),
                repr_str(&sense)
            )));
        }
        let target_raw = match map.get("target") {
            Some(v) => Some(v),
            None => map.get("bound"),
        };
        let target_raw = target_raw.filter(|v| !v.is_null());
        if matches!(sense.as_str(), "upper" | "lower" | "equal") && target_raw.is_none() {
            return Err(CaeError::contract(format!(
                "response {}: sense {} requires target/bound",
                repr_str(&name),
                repr_str(&sense)
            )));
        }
        let number = |key: &str, raw: Option<&Value>, default: f64| -> CaeResult<PyNum> {
            match raw {
                None => Ok(PyNum::Float(default)),
                Some(v) => PyNum::from_value(v).ok_or_else(|| {
                    CaeError::contract(format!(
                        "response {}: {key} must be a finite real number",
                        repr_str(&name)
                    ))
                }),
            }
        };
        let scale = number("scale", map.get("scale"), 1.0)?;
        let weight = number("weight", map.get("weight"), 1.0)?;
        let target = match target_raw {
            None => None,
            Some(v) => Some(PyNum::from_value(v).ok_or_else(|| {
                CaeError::contract(format!(
                    "response {}: target must be a finite real number",
                    repr_str(&name)
                ))
            })?),
        };
        Self::new(&name, weight, &sense, target, scale)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "weight": self.weight.to_value(),
            "sense": self.sense,
            "target": self.target.map_or(Value::Null, PyNum::to_value),
            "scale": self.scale.to_value(),
        })
    }
}

pub const RETIRED_OPTIMIZATION_SETTINGS: [(&str, &str); 1] = [(
    "gradient_clip_norm",
    "'gradient_clip_norm' no longer exists: the search direction is the max-norm-normalised projected gradient and the carried trust step, bounded by 'move_limit', sets every update; remove the key or set it to null",
)];


pub fn drop_retired_optimization_settings(
    value: &Map<String, Value>,
    where_: &str,
) -> CaeResult<Map<String, Value>> {
    let mut out = value.clone();
    for (key, message) in RETIRED_OPTIMIZATION_SETTINGS {
        match out.get(key) {
            None => {}
            Some(Value::Null) => {
                out.shift_remove(key);
            }
            Some(_) => return Err(CaeError::contract(format!("{where_}: {message}"))),
        }
    }
    Ok(out)
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoordinateOptimizationSettings {
    #[serde(default = "legacy_step_policy")]
    pub step_policy: String,
    pub iterations: i64,
    pub step_fraction: PyNum,
    pub minimum_step_fraction: PyNum,
    pub backtracking: PyNum,
    pub armijo: PyNum,
    pub move_limit: PyNum,
    pub coordinate_lower: PyNum,
    pub coordinate_upper: PyNum,
    pub step_growth: PyNum,
    pub stationarity_tolerance: PyNum,
    pub bound_tolerance: PyNum,
    pub penalty_growth: PyNum,
    pub penalty_limit: PyNum,
    pub violation_reduction: PyNum,
    pub multiplier_update_interval: i64,
}

pub const COORDINATE_SETTINGS_FIELDS: [&str; 16] = [
    "step_policy",
    "iterations",
    "step_fraction",
    "minimum_step_fraction",
    "backtracking",
    "armijo",
    "move_limit",
    "coordinate_lower",
    "coordinate_upper",
    "step_growth",
    "stationarity_tolerance",
    "bound_tolerance",
    "penalty_growth",
    "penalty_limit",
    "violation_reduction",
    "multiplier_update_interval",
];

fn legacy_step_policy() -> String { "backtracking".into() }

impl Default for CoordinateOptimizationSettings {
    fn default() -> Self {
        Self {
            step_policy: "fixed_step".into(),
            iterations: 40,
            step_fraction: PyNum::Float(0.05),
            minimum_step_fraction: PyNum::Float(1e-5),
            backtracking: PyNum::Float(0.5),
            armijo: PyNum::Float(1e-4),
            move_limit: PyNum::Float(0.10),
            coordinate_lower: PyNum::Float(0.0),
            coordinate_upper: PyNum::Float(1.0),
            step_growth: PyNum::Float(2.0),
            stationarity_tolerance: PyNum::Float(1e-3),
            bound_tolerance: PyNum::Float(1e-3),
            penalty_growth: PyNum::Float(10.0),
            penalty_limit: PyNum::Float(1e6),
            violation_reduction: PyNum::Float(0.25),
            multiplier_update_interval: 10,
        }
    }
}

struct SearchFields<'a> {
    backtracking_policy: bool,
    iterations: i64,
    multiplier_update_interval: i64,
    numeric: [(&'a str, PyNum); 13],
}

fn check_search(f: &SearchFields<'_>, lower_name: &str, upper_name: &str) -> CaeResult<()> {
    if f.iterations < 1 {
        return Err(CaeError::contract("iterations must be an integer >= 1"));
    }
    if f.multiplier_update_interval < 1 {
        return Err(CaeError::contract("multiplier_update_interval must be an integer >= 1"));
    }
    for (name, value) in &f.numeric {
        if !value.is_finite() {
            return Err(CaeError::contract(format!("{name} must be a finite number")));
        }
    }
    let get = |n: &str| f.numeric.iter().find(|(k, _)| *k == n).map_or(f64::NAN, |(_, v)| v.as_f64());
    let step = get("step_fraction");
    let min_step = get("minimum_step_fraction");
    let back = get("backtracking");
    let armijo = get("armijo");
    let move_limit = get("move_limit");
    if !(0.0 < step && step <= 1.0) {
        return Err(CaeError::contract("step_fraction must lie in (0,1]"));
    }
    if f.backtracking_policy && !(0.0 < min_step && min_step <= step) {
        return Err(CaeError::contract("minimum_step_fraction must lie in (0, step_fraction]"));
    }
    if f.backtracking_policy && !(0.0 < back && back < 1.0) {
        return Err(CaeError::contract("backtracking must lie in (0,1)"));
    }
    if f.backtracking_policy && !(0.0..1.0).contains(&armijo) {
        return Err(CaeError::contract("armijo must lie in [0,1)"));
    }
    if !(0.0 < move_limit && move_limit <= 1.0) {
        return Err(CaeError::contract("move_limit must lie in (0,1]"));
    }
    if f.backtracking_policy && min_step > move_limit {
        return Err(CaeError::contract("minimum_step_fraction must not exceed move_limit"));
    }
    if f.backtracking_policy && get("step_growth") < 1.0 {
        return Err(CaeError::contract("step_growth must be >= 1"));
    }
    let stat = get("stationarity_tolerance");
    if !(0.0 < stat && stat < 1.0) {
        return Err(CaeError::contract("stationarity_tolerance must lie in (0,1)"));
    }
    if get("bound_tolerance") <= 0.0 {
        return Err(CaeError::contract("bound_tolerance must be > 0"));
    }
    if get("penalty_growth") <= 1.0 {
        return Err(CaeError::contract("penalty_growth must be > 1"));
    }
    if get("penalty_limit") < 1.0 {
        return Err(CaeError::contract("penalty_limit must be >= 1"));
    }
    let vr = get("violation_reduction");
    if !(0.0 < vr && vr < 1.0) {
        return Err(CaeError::contract("violation_reduction must lie in (0,1)"));
    }
    if get(lower_name) >= get(upper_name) {
        return Err(CaeError::contract(format!("{lower_name} must be below {upper_name}")));
    }
    Ok(())
}

fn settings_value(map: &Map<String, Value>, key: &str, default: PyNum, integer: bool) -> CaeResult<PyNum> {
    match map.get(key) {
        None => Ok(default),
        Some(v) => match (PyNum::from_value(v), integer) {
            (Some(PyNum::Int(i)), _) => Ok(PyNum::Int(i)),
            (Some(PyNum::Float(f)), false) => Ok(PyNum::Float(f)),
            _ if integer => Err(CaeError::contract(if key == "iterations" {
                "iterations must be an integer >= 1".to_string()
            } else {
                format!("{key} must be an integer >= 1")
            })),
            _ => Err(CaeError::contract(format!("{key} must be a finite number"))),
        },
    }
}

impl CoordinateOptimizationSettings {

    pub fn validate(&self) -> CaeResult<()> {
        if !["fixed_step", "backtracking"].contains(&self.step_policy.as_str()) {
            return Err(CaeError::contract("step_policy must be fixed_step or backtracking"));
        }
        check_search(&self.fields(), "coordinate_lower", "coordinate_upper")
    }

    fn fields(&self) -> SearchFields<'static> {
        SearchFields {
            backtracking_policy: self.step_policy == "backtracking",
            iterations: self.iterations,
            multiplier_update_interval: self.multiplier_update_interval,
            numeric: [
                ("step_fraction", self.step_fraction),
                ("minimum_step_fraction", self.minimum_step_fraction),
                ("backtracking", self.backtracking),
                ("armijo", self.armijo),
                ("move_limit", self.move_limit),
                ("step_growth", self.step_growth),
                ("stationarity_tolerance", self.stationarity_tolerance),
                ("bound_tolerance", self.bound_tolerance),
                ("penalty_growth", self.penalty_growth),
                ("penalty_limit", self.penalty_limit),
                ("violation_reduction", self.violation_reduction),
                ("coordinate_lower", self.coordinate_lower),
                ("coordinate_upper", self.coordinate_upper),
            ],
        }
    }


    pub fn from_dict(value: Option<&Value>) -> CaeResult<Self> {
        let empty = Map::new();
        let map = match value {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(m)) => m,
            Some(_) => return Err(CaeError::contract("coordinate optimization settings must be a mapping")),
        };
        reject_removed_constraint_keys(&Value::Object(map.clone()), "coordinate optimization settings")?;
        let map = drop_retired_optimization_settings(map, "coordinate optimization settings")?;
        let mut unknown: Vec<&String> =
            map.keys().filter(|k| !COORDINATE_SETTINGS_FIELDS.contains(&k.as_str())).collect();
        unknown.sort();
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!(
                "coordinate optimization settings have unknown keys {}",
                list_repr(&unknown)
            )));
        }
        let d = Self::default();

        let iterations = int_of(settings_value(&map, "iterations", PyNum::Int(d.iterations), true)?);
        let multiplier_update_interval = int_of(settings_value(
            &map,
            "multiplier_update_interval",
            PyNum::Int(d.multiplier_update_interval),
            true,
        )?);
        let step_fraction = settings_value(&map, "step_fraction", d.step_fraction, false)?;
        let minimum_step_fraction =
            settings_value(&map, "minimum_step_fraction", d.minimum_step_fraction, false)?;
        let backtracking = settings_value(&map, "backtracking", d.backtracking, false)?;
        let armijo = settings_value(&map, "armijo", d.armijo, false)?;
        let move_limit = settings_value(&map, "move_limit", d.move_limit, false)?;
        let step_growth = settings_value(&map, "step_growth", d.step_growth, false)?;
        let stationarity_tolerance =
            settings_value(&map, "stationarity_tolerance", d.stationarity_tolerance, false)?;
        let bound_tolerance = settings_value(&map, "bound_tolerance", d.bound_tolerance, false)?;
        let penalty_growth = settings_value(&map, "penalty_growth", d.penalty_growth, false)?;
        let penalty_limit = settings_value(&map, "penalty_limit", d.penalty_limit, false)?;
        let violation_reduction = settings_value(&map, "violation_reduction", d.violation_reduction, false)?;
        let coordinate_lower = settings_value(&map, "coordinate_lower", d.coordinate_lower, false)?;
        let coordinate_upper = settings_value(&map, "coordinate_upper", d.coordinate_upper, false)?;
        let step_policy = match map.get("step_policy") {
            None => d.step_policy,
            Some(Value::String(v)) => v.clone(),
            Some(_) => return Err(CaeError::contract("step_policy must be fixed_step or backtracking")),
        };
        let s = Self {
            step_policy,
            iterations,
            step_fraction,
            minimum_step_fraction,
            backtracking,
            armijo,
            move_limit,
            coordinate_lower,
            coordinate_upper,
            step_growth,
            stationarity_tolerance,
            bound_tolerance,
            penalty_growth,
            penalty_limit,
            violation_reduction,
            multiplier_update_interval,
        };
        s.validate()?;
        Ok(s)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("step_policy".into(), json!(self.step_policy));
        m.insert("iterations".into(), json!(self.iterations));
        for (k, v) in self.fields().numeric {
            m.insert(k.into(), v.to_value());
        }
        m.insert("multiplier_update_interval".into(), json!(self.multiplier_update_interval));

        let mut ordered = Map::new();
        for k in COORDINATE_SETTINGS_FIELDS {
            if let Some(v) = m.get(k) {
                ordered.insert(k.into(), v.clone());
            }
        }
        Value::Object(ordered)
    }
}

fn int_of(v: PyNum) -> i64 {
    match v {
        PyNum::Int(i) => i,
        #[allow(clippy::cast_possible_truncation)]
        PyNum::Float(f) => f as i64,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacySingleArrayOptimizationSettings {
    pub settings: CoordinateOptimizationSettings,
}

impl LegacySingleArrayOptimizationSettings {

    pub fn from_dict(value: Option<&Value>) -> CaeResult<Self> {
        let empty = Map::new();
        let map = match value {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(m)) => m,
            Some(_) => return Err(CaeError::contract("optimization settings must be a mapping")),
        };
        reject_removed_constraint_keys(&Value::Object(map.clone()), "optimization settings")?;
        let map = drop_retired_optimization_settings(map, "optimization settings")?;
        let mut coordinate = Map::new();
        for (k, v) in &map {
            match k.as_str() {
                "topology_lower" => {
                    coordinate.insert("coordinate_lower".into(), v.clone());
                }
                "topology_upper" => {
                    coordinate.insert("coordinate_upper".into(), v.clone());
                }
                "coordinate_lower" | "coordinate_upper" => {}
                other if COORDINATE_SETTINGS_FIELDS.contains(&other) => {
                    coordinate.insert(k.clone(), v.clone());
                }
                _ => {}
            }
        }
        let settings =
            CoordinateOptimizationSettings::from_dict(Some(&Value::Object(coordinate))).map_err(|e| {
                CaeError::contract(
                    e.message()
                        .replace("coordinate_lower", "topology_lower")
                        .replace("coordinate_upper", "topology_upper"),
                )
            })?;
        Ok(Self { settings })
    }

    #[must_use]
    pub fn as_coordinate_settings(&self) -> CoordinateOptimizationSettings {
        self.settings.clone()
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn optimization_settings_schema() -> Value {
    let span = "fraction of the coordinate span";
    let number = |title: &str, unit: &str, description: &str, limits: &[(&str, i64)]| {
        let mut m = Map::new();
        m.insert("type".into(), json!("number"));
        m.insert("title".into(), json!(title));
        m.insert("unit".into(), json!(unit));
        m.insert("description".into(), json!(description));
        for (k, v) in limits {
            m.insert((*k).into(), json!(v));
        }
        m
    };
    let mut properties = Map::new();
    properties.insert("step_policy".into(), json!({"type":"string","enum":["fixed_step","backtracking"],"title":"Step policy","default":"fixed_step","description":"Fixed-step direct gradient descent evaluates one candidate per epoch. Backtracking uses sufficient-decrease trials."}));
    let mut iterations = Map::new();
    iterations.insert("type".into(), json!("integer"));
    iterations.insert("minimum".into(), json!(1));
    iterations.insert("title".into(), json!("Update budget"));
    iterations.insert("unit".into(), json!("updates"));
    iterations.insert("description".into(), json!("Committed search iterations."));
    properties.insert("iterations".into(), Value::Object(iterations));
    let rows: Vec<(&str, Map<String, Value>)> = vec![
        (
            "step_fraction",
            number(
                "Initial trust step",
                span,
                "First trust step: largest per-entry change of a trial.",
                &[("exclusiveMinimum", 0), ("maximum", 1)],
            ),
        ),
        (
            "move_limit",
            number(
                "Per-update move limit",
                span,
                "Upper bound of the carried trust step.",
                &[("exclusiveMinimum", 0), ("maximum", 1)],
            ),
        ),
        (
            "minimum_step_fraction",
            number(
                "Minimum trial step",
                span,
                "Backtracking stops below this step; at most the initial step and the move limit.",
                &[("exclusiveMinimum", 0), ("maximum", 1)],
            ),
        ),
        (
            "backtracking",
            number(
                "Backtracking factor",
                "factor",
                "A rejected trial step is multiplied by this factor.",
                &[("exclusiveMinimum", 0), ("exclusiveMaximum", 1)],
            ),
        ),
        (
            "armijo",
            number(
                "Sufficient-decrease coefficient",
                "factor",
                "Armijo coefficient along the actual projected step.",
                &[("minimum", 0), ("exclusiveMaximum", 1)],
            ),
        ),
        (
            "step_growth",
            number(
                "Trust-step growth",
                "factor",
                "The trust step grows by this factor after an accepted step.",
                &[("minimum", 1)],
            ),
        ),
        (
            "stationarity_tolerance",
            number(
                "Stationarity tolerance",
                "relative to the first projected-gradient norm",
                "Projected-gradient max-norm, relative to its first value, at which a stage is stationary.",
                &[("exclusiveMinimum", 0), ("exclusiveMaximum", 1)],
            ),
        ),
        (
            "bound_tolerance",
            number(
                "Bound tolerance",
                "scaled response units (value/scale)",
                "Admissible scaled bound violation/complementarity for convergence.",
                &[("exclusiveMinimum", 0)],
            ),
        ),
        (
            "penalty_growth",
            number(
                "Penalty growth",
                "factor",
                "A bound's penalty grows by this factor when its violation does not shrink enough.",
                &[("exclusiveMinimum", 1)],
            ),
        ),
        (
            "penalty_limit",
            number(
                "Penalty limit",
                "multiple of the initial penalty",
                "No penalty exceeds this multiple of its initial value 2*weight.",
                &[("minimum", 1)],
            ),
        ),
        (
            "violation_reduction",
            number(
                "Required violation reduction",
                "factor",
                "Penalties grow unless the bound measure fell below this factor times its previous value.",
                &[("exclusiveMinimum", 0), ("exclusiveMaximum", 1)],
            ),
        ),
    ];
    for (k, v) in rows {
        properties.insert(k.into(), Value::Object(v));
    }
    let mut interval = Map::new();
    interval.insert("type".into(), json!("integer"));
    interval.insert("minimum".into(), json!(1));
    interval.insert("title".into(), json!("Multiplier update interval"));
    interval.insert("unit".into(), json!("accepted steps"));
    interval
        .insert("description".into(), json!("Multipliers are also updated after this many accepted steps."));
    properties.insert("multiplier_update_interval".into(), Value::Object(interval));
    let mut clip = Map::new();
    clip.insert("type".into(), json!("null"));
    clip.insert("title".into(), json!("Retired gradient clipping"));
    clip.insert("description".into(), json!(RETIRED_OPTIMIZATION_SETTINGS[0].1));
    properties.insert("gradient_clip_norm".into(), Value::Object(clip));
    let defaults = CoordinateOptimizationSettings::default().to_value();
    for (name, row) in &mut properties {
        if let (Some(d), Some(row)) = (defaults.get(name), row.as_object_mut())
            && !d.is_null()
        {
            row.insert("default".into(), d.clone());
        }
    }
    json!({
        "type": "object",
        "title": "Projected-search settings",
        "description": "Settings of the projected augmented-Lagrangian direct-gradient search; bounded responses are soft and converge within bound_tolerance.",
        "properties": Value::Object(properties),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Array(ArrayD<f64>),
    Json(Value),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Evaluation {
    pub provider: String,
    pub responses: BTreeMap<String, f64>,
    pub diagnostics: Map<String, Value>,
    pub fields: BTreeMap<String, FieldValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sensitivity {
    pub provider: String,
    pub response: String,
    pub value: f64,
    pub gradient: ArrayD<f64>,
    pub diagnostics: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBoundNumericalGuess {
    pub schema: String,
    pub destination_design_sha256: String,
    pub destination_initial_state_sha256: String,
    pub time_coordinates_s: Vec<f64>,
    pub states: Vec<Vec<f64>>,
    pub source_archive_sha256: String,
    pub source_design_sha256: String,
    pub provenance: Map<String, Value>,
}

impl SourceBoundNumericalGuess {
    pub fn from_value(value: &Value) -> CaeResult<Self> {
        let guess: Self = serde_json::from_value(value.clone())
            .map_err(|e| CaeError::contract(format!("numerical initial guess: {e}")))?;
        let hashes = [&guess.destination_design_sha256, &guess.destination_initial_state_sha256,
            &guess.source_archive_sha256, &guess.source_design_sha256];
        if guess.schema != "implexity-source-bound-numerical-guess/1"
            || hashes.iter().any(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
            || guess.states.is_empty() || guess.states.len() != guess.time_coordinates_s.len()
            || guess.states.iter().any(|s| s.is_empty() || s.iter().any(|v| !v.is_finite()))
            || guess.time_coordinates_s.iter().any(|v| !v.is_finite())
            || guess.time_coordinates_s.windows(2).any(|w| w[1] <= w[0])
            || guess.provenance.is_empty()
        {
            return Err(CaeError::contract("invalid source-bound numerical initial guess"));
        }
        Ok(guess)
    }
}


#[derive(Debug, Clone, PartialEq)]
pub struct MatchingTimeNewtonGuess {
    states: Vec<Arc<[f64]>>,
    provider_identity: Map<String, Value>,
    provenance: Map<String, Value>,
}

impl MatchingTimeNewtonGuess {

    pub fn new(
        states: Vec<Vec<f64>>,
        provider_identity: Map<String, Value>,
        provenance: Map<String, Value>,
    ) -> CaeResult<Self> {
        if states.is_empty() {
            return Err(CaeError::contract("matching-time Newton guess requires a nonempty tuple of states"));
        }
        let mut owned = Vec::with_capacity(states.len());
        for (index, state) in states.into_iter().enumerate() {
            if state.is_empty() || !state.iter().all(|v| v.is_finite()) {
                return Err(CaeError::contract(format!(
                    "matching-time Newton guess state {index} must be a finite, nonempty real vector"
                )));
            }
            owned.push(Arc::from(state.into_boxed_slice()));
        }
        if provider_identity.is_empty() {
            return Err(CaeError::contract(
                "matching-time Newton guess requires a provider-owned identity mapping",
            ));
        }
        Ok(Self { states: owned, provider_identity, provenance })
    }

    #[must_use]
    pub fn states(&self) -> &[Arc<[f64]>] {
        &self.states
    }

    #[must_use]
    pub fn provider_identity(&self) -> &Map<String, Value> {
        &self.provider_identity
    }

    #[must_use]
    pub fn provenance(&self) -> &Map<String, Value> {
        &self.provenance
    }
}

pub type ProviderProblem = Arc<dyn Any + Send + Sync>;


pub trait CaeProvider: Send + Sync + 'static {
    fn requires_explicit_selection(&self) -> bool { false }
    fn provider_dependencies(&self) -> Vec<String> { Vec::new() }

    fn published_schemas(&self) -> &'static [crate::schemas::SchemaDescriptor] {
        &[]
    }

    fn name(&self) -> &str;

    fn provider_id(&self) -> Option<&str> {
        None
    }

    fn implementation(&self) -> &str;

    fn orchestration_meta(&self) -> bool {
        false
    }


    fn capabilities(&self) -> CaeResult<ProviderCapabilities>;

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        None
    }

    fn application_scope(&self) -> Vec<String> {
        vec!["*".to_string()]
    }


    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem>;


    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>>;


    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation>;


    fn sensitivity(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity>;

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        None
    }

    fn coupling_validation(
        &self,
        _problem: Option<&ProviderProblem>,
        _for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        None
    }

    fn coupling_inventory(&self, _problem: Option<&ProviderProblem>) -> Option<Value> {
        None
    }

    fn mathematical_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<Value> {
        None
    }

    fn semantic_physics_contract(&self, _problem: Option<&ProviderProblem>) -> Option<Value> {
        None
    }

    fn runtime_support(&self) -> Option<Map<String, Value>> {
        None
    }

    fn component_slots(&self) -> Option<Map<String, Value>> {
        None
    }

    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        None
    }

    fn interface(&self, _name: &str) -> Option<&(dyn Any + Send + Sync)> {
        None
    }

    fn as_any(&self) -> &dyn Any;
}

#[must_use]
pub fn provider_key(provider: &dyn CaeProvider) -> String {
    provider.provider_id().filter(|s| !s.is_empty()).unwrap_or_else(|| provider.name()).to_string()
}

#[must_use]
pub fn provider_ptr(provider: &Arc<dyn CaeProvider>) -> usize {
    Arc::as_ptr(provider).cast::<()>() as usize
}

#[must_use]
pub fn value_repr(value: &Value) -> String {
    repr(value)
}

