// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::contracts::require_contract_bool;
use crate::coupling_graph::CouplingEdge as PhysicsCouplingEdge;
use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, sha256_of};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sync::{ReLock, lock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricSpec {
    pub metric_id: String,
    pub unit: String,
    pub required: bool,
}

impl MetricSpec {

    pub fn new(metric_id: &str, unit: &str, required: bool) -> CaeResult<Self> {
        if metric_id.trim().is_empty() {
            return Err(CaeError::contract("metric_id required"));
        }
        if unit.is_empty() {
            return Err(CaeError::contract("metric unit required"));
        }
        Ok(Self { metric_id: metric_id.into(), unit: unit.into(), required })
    }


    pub fn from_mapping(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("metric specification must be an object"));
        };
        let mut unknown: Vec<&String> =
            map.keys().filter(|k| !["metric_id", "unit", "required"].contains(&k.as_str())).collect();
        unknown.sort();
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!(
                "metric specification has unknown keys {}",
                list_repr(&unknown)
            )));
        }
        let Some(id) = map.get("metric_id") else {
            return Err(CaeError::contract(
                "MetricSpec.__init__() missing 1 required positional argument: 'metric_id'",
            ));
        };
        let id = match id.as_str() {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => return Err(CaeError::contract("metric_id required")),
        };
        let unit = match map.get("unit") {
            None => "-".to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(_) => return Err(CaeError::contract("metric unit required")),
        };
        let required = match map.get("required") {
            None => true,
            Some(v) => require_contract_bool(v, &format!("metric {}: required", repr_str(&id)), false)?
                .unwrap_or(true),
        };
        Ok(Self { metric_id: id, unit, required })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouplingRule {
    pub trigger: BTreeSet<String>,
    pub edges: Vec<PhysicsCouplingEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackError {
    pub kind: String,
    pub message: String,
}

pub type MonitorFn = dyn Fn(&Value, Option<&Value>) -> Result<Value, CallbackError> + Send + Sync;

pub type CaseBuilderFn = dyn Fn(&Value) -> Result<Value, CallbackError> + Send + Sync;

#[derive(Clone)]
pub struct RegisteredRegimeMonitor {
    pub monitor_id: String,
    pub owner_id: String,
    pub metrics: Vec<MetricSpec>,
    pub callback: Arc<MonitorFn>,
    pub compatibility_mode: bool,
}

#[derive(Clone)]
pub struct RegisteredCaseAuthoring {
    pub schema: String,
    pub owner_id: String,
    pub label: String,
    pub description: String,
    pub builder: Arc<CaseBuilderFn>,
    pub implementation: String,
    pub report_keys: Vec<String>,
}

impl PartialEq for RegisteredCaseAuthoring {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.owner_id == other.owner_id
            && self.label == other.label
            && self.description == other.description
            && Arc::ptr_eq(&self.builder, &other.builder)
            && self.report_keys == other.report_keys
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionRegistryToken {
    pub generation: u64,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct ExtensionRegistrySnapshot {
    pub coupling: Vec<(String, Arc<Vec<CouplingRule>>)>,
    pub monitors: Vec<(String, Arc<RegisteredRegimeMonitor>)>,
    pub case_authoring: Vec<(String, Arc<RegisteredCaseAuthoring>)>,
    pub token: ExtensionRegistryToken,
}

#[derive(Default, Clone)]
struct Data {
    coupling: BTreeMap<String, Arc<Vec<CouplingRule>>>,
    monitors: BTreeMap<String, Arc<RegisteredRegimeMonitor>>,
    cases: BTreeMap<String, Arc<RegisteredCaseAuthoring>>,
}

#[derive(Default)]
struct State {
    data: Data,
    generation: u64,
}

fn ptr<T: ?Sized>(a: &Arc<T>) -> usize {
    Arc::as_ptr(a).cast::<()>() as usize
}

fn token_of(state: &State) -> ExtensionRegistryToken {
    let d = &state.data;
    let coupling: Vec<Value> = d.coupling.iter().map(|(k, v)| json!([k, [["tuple", ptr(v)]]])).collect();
    let monitors: Vec<Value> = d
        .monitors
        .iter()
        .map(|(k, r)| {
            let metrics: Vec<Value> =
                r.metrics.iter().map(|m| json!([m.metric_id, m.unit, m.required])).collect();
            json!([k, r.owner_id, metrics, ptr(&r.callback), r.compatibility_mode])
        })
        .collect();
    let cases: Vec<Value> = d
        .cases
        .iter()
        .map(|(k, r)| json!([k, r.owner_id, r.label, r.report_keys, ptr(&r.builder)]))
        .collect();
    let rows = json!({"coupling": coupling, "monitors": monitors, "case_authoring": cases});
    ExtensionRegistryToken {
        generation: state.generation,
        fingerprint: sha256_of(&rows, &DumpOptions::canonical()),
    }
}

#[derive(Default)]
pub struct ExtensionRegistry {
    relock: ReLock,
    state: Mutex<State>,
}

impl std::fmt::Debug for ExtensionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRegistry").field("generation", &self.generation()).finish()
    }
}

impl ExtensionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        let _g = self.relock.lock();
        lock(&self.state).generation
    }

    #[must_use]
    pub fn binding_token(&self) -> ExtensionRegistryToken {
        let _g = self.relock.lock();
        token_of(&lock(&self.state))
    }

    #[must_use]
    pub fn snapshot(&self) -> ExtensionRegistrySnapshot {
        let _g = self.relock.lock();
        let s = lock(&self.state);
        ExtensionRegistrySnapshot {
            coupling: s.data.coupling.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect(),
            monitors: s.data.monitors.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect(),
            case_authoring: s.data.cases.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect(),
            token: token_of(&s),
        }
    }


    pub fn restore(&self, state: &ExtensionRegistrySnapshot) -> CaeResult<()> {
        let coupling: BTreeMap<_, _> = state.coupling.iter().cloned().collect();
        let monitors: BTreeMap<_, _> = state.monitors.iter().cloned().collect();
        let cases: BTreeMap<_, _> = state.case_authoring.iter().cloned().collect();
        if coupling.len() != state.coupling.len()
            || monitors.len() != state.monitors.len()
            || cases.len() != state.case_authoring.len()
        {
            return Err(CaeError::contract("extension snapshot contains duplicate ids"));
        }
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        s.data = Data { coupling, monitors, cases };
        s.generation += 1;
        Ok(())
    }


    pub fn transaction<T, E>(&self, body: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let _g = self.relock.lock();
        let before = lock(&self.state).data.clone();
        let result = body();
        if result.is_err() {
            let mut s = lock(&self.state);
            s.data = before;
            s.generation += 1;
        }
        result
    }


    pub fn register_coupling_rules(&self, module_id: &str, rules: Vec<CouplingRule>) -> CaeResult<()> {
        let key = module_id.trim();
        if key.is_empty() {
            return Err(CaeError::contract("module_id required"));
        }
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        if let Some(existing) = s.data.coupling.get(key) {
            if **existing == rules {
                return Ok(());
            }
            return Err(CaeError::contract(format!(
                "coupling-rule owner {} attempted a divergent replacement",
                repr_str(key)
            )));
        }
        s.data.coupling.insert(key.to_string(), Arc::new(rules));
        s.generation += 1;
        Ok(())
    }

    #[must_use]
    pub fn coupling_rules(&self) -> Vec<CouplingRule> {
        let _g = self.relock.lock();
        lock(&self.state).data.coupling.values().flat_map(|v| v.iter().cloned()).collect()
    }


    pub fn register_regime_monitor(
        &self,
        name: &str,
        monitor: Arc<MonitorFn>,
        metrics: Option<Vec<MetricSpec>>,
        owner_id: Option<&str>,
    ) -> CaeResult<()> {
        let key = name.trim();
        if key.is_empty() {
            return Err(CaeError::contract("monitor name required"));
        }
        let compatibility = metrics.is_none();
        let specs = metrics.unwrap_or_default();
        let ids: Vec<&str> = specs.iter().map(|m| m.metric_id.as_str()).collect();
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        if unique.len() != ids.len() {
            return Err(CaeError::contract(format!(
                "regime monitor {} declares duplicate metrics",
                repr_str(key)
            )));
        }
        let owner = owner_id.unwrap_or(key).trim().to_string();
        if owner.is_empty() {
            return Err(CaeError::contract("monitor owner_id is required"));
        }
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        for (other_id, other) in &s.data.monitors {
            if other_id == key {
                continue;
            }
            let others: BTreeSet<&str> = other.metrics.iter().map(|m| m.metric_id.as_str()).collect();
            let overlap: Vec<&str> = unique.intersection(&others).copied().collect();
            if !overlap.is_empty() {
                return Err(CaeError::contract(format!(
                    "duplicate regime metric ownership {}",
                    list_repr(&overlap)
                )));
            }
        }
        if let Some(existing) = s.data.monitors.get(key) {
            if Arc::ptr_eq(&existing.callback, &monitor)
                && existing.owner_id == owner
                && existing.metrics == specs
            {
                return Ok(());
            }
            return Err(CaeError::contract(format!(
                "regime monitor {} already registered by {}",
                repr_str(key),
                repr_str(&existing.owner_id)
            )));
        }
        let row = RegisteredRegimeMonitor {
            monitor_id: key.to_string(),
            owner_id: owner,
            metrics: specs,
            callback: monitor,
            compatibility_mode: compatibility,
        };
        s.data.monitors.insert(key.to_string(), Arc::new(row));
        s.generation += 1;
        Ok(())
    }


    #[allow(clippy::too_many_arguments)]
    pub fn register_case_authoring(
        &self,
        schema: &str,
        builder: Arc<CaseBuilderFn>,
        implementation: &str,
        owner_id: &str,
        label: &str,
        description: &str,
        report_keys: Vec<String>,
    ) -> CaeResult<()> {
        if schema.trim().is_empty() || schema != schema.trim() {
            return Err(CaeError::contract("case authoring schema must be nonempty trimmed text"));
        }
        if owner_id.trim().is_empty() {
            return Err(CaeError::contract("case authoring owner_id is required"));
        }
        if label.trim().is_empty() {
            return Err(CaeError::contract("case authoring label is required"));
        }
        let unique: BTreeSet<&String> = report_keys.iter().collect();
        if report_keys.iter().any(String::is_empty) || unique.len() != report_keys.len() {
            return Err(CaeError::contract("case authoring report_keys must be unique nonempty text"));
        }
        let row = RegisteredCaseAuthoring {
            schema: schema.to_string(),
            owner_id: owner_id.trim().to_string(),
            label: label.trim().to_string(),
            description: description.to_string(),
            builder,
            implementation: implementation.to_string(),
            report_keys,
        };
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        if let Some(existing) = s.data.cases.get(schema) {
            if **existing == row {
                return Ok(());
            }
            return Err(CaeError::contract(format!(
                "case authoring schema {} already registered by {}",
                repr_str(schema),
                repr_str(&existing.owner_id)
            )));
        }
        s.data.cases.insert(schema.to_string(), Arc::new(row));
        s.generation += 1;
        Ok(())
    }


    pub fn case_authoring(&self, schema: &str) -> CaeResult<Arc<RegisteredCaseAuthoring>> {
        let _g = self.relock.lock();
        let s = lock(&self.state);
        s.data.cases.get(schema).cloned().ok_or_else(|| {
            let available: Vec<&String> = s.data.cases.keys().collect();
            let listed = if available.is_empty() { "'none'".to_string() } else { list_repr(&available) };
            CaeError::contract(format!(
                "no loaded package registers case authoring for schema {}; load the application package that provides it (registered: {})",
                repr_str(schema),
                listed.trim_matches('\'')
            ))
        })
    }

    #[must_use]
    pub fn case_authoring_catalogue(&self) -> Vec<Value> {
        let _g = self.relock.lock();
        lock(&self.state)
            .data
            .cases
            .values()
            .map(|r| {
                json!({"schema": r.schema, "owner_id": r.owner_id, "label": r.label,
                       "description": r.description, "report_keys": r.report_keys})
            })
            .collect()
    }

    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn evaluate_regime_monitors(
        &self,
        problem: &Value,
        diagnostics: Option<&Value>,
        limits: Option<&Map<String, Value>>,
    ) -> Value {
        let (monitors, generation_at_start) = {
            let _g = self.relock.lock();
            let s = lock(&self.state);
            (s.data.monitors.values().cloned().collect::<Vec<_>>(), s.generation)
        };
        let mut values: Map<String, Value> = Map::new();
        let mut units: Map<String, Value> = Map::new();
        let mut owners: Map<String, Value> = Map::new();
        let mut issues: Vec<String> = Vec::new();
        let mut engineering: Vec<String> = Vec::new();
        for monitor in &monitors {
            let raw = match (monitor.callback)(problem, diagnostics) {
                Ok(v) => v,
                Err(e) => {
                    issues.push(format!(
                        "monitor {} failed: {}: {}",
                        repr_str(&monitor.monitor_id),
                        e.kind,
                        e.message
                    ));
                    continue;
                }
            };
            let Some(raw) = raw.as_object() else {
                issues.push(format!(
                    "monitor {} did not return a metric mapping",
                    repr_str(&monitor.monitor_id)
                ));
                continue;
            };
            let declared: BTreeMap<&str, &MetricSpec> =
                monitor.metrics.iter().map(|m| (m.metric_id.as_str(), m)).collect();
            if !monitor.compatibility_mode {
                let mut unknown: Vec<&String> =
                    raw.keys().filter(|k| !declared.contains_key(k.as_str())).collect();
                unknown.sort();
                let mut missing: Vec<&str> = monitor
                    .metrics
                    .iter()
                    .filter(|m| m.required && !raw.contains_key(&m.metric_id))
                    .map(|m| m.metric_id.as_str())
                    .collect();
                missing.sort_unstable();
                if !unknown.is_empty() {
                    issues.push(format!(
                        "monitor {} returned undeclared metrics {}",
                        repr_str(&monitor.monitor_id),
                        list_repr(&unknown)
                    ));
                }
                if !missing.is_empty() {
                    issues.push(format!(
                        "monitor {} omitted required metrics {}",
                        repr_str(&monitor.monitor_id),
                        list_repr(&missing)
                    ));
                }
            }
            for (name, raw_value) in raw {
                if values.contains_key(name) {
                    issues.push(format!(
                        "metric {} has duplicate owners {} and {}",
                        repr_str(name),
                        repr_str(owners[name].as_str().unwrap_or_default()),
                        repr_str(&monitor.owner_id)
                    ));
                    continue;
                }
                let spec = declared.get(name.as_str());
                let parsed: Result<(f64, String), String> = match raw_value {
                    Value::Bool(_) => Err("boolean is not a metric value".into()),
                    Value::Object(m) if m.contains_key("value") => match &m["value"] {
                        Value::Bool(_) => Err("boolean is not a metric value".into()),
                        v => {
                            crate::orchestration::py_float(v).map_err(|e| e.message().to_string()).map(|f| {
                                let unit = m
                                    .get("unit")
                                    .filter(|u| crate::pyobj::truthy(u))
                                    .map_or_else(|| "-".to_string(), crate::pyobj::py_str);
                                (f, unit)
                            })
                        }
                    },
                    v => crate::orchestration::py_float(v)
                        .map_err(|e| e.message().to_string())
                        .map(|f| (f, spec.map_or_else(|| "-".to_string(), |s| s.unit.clone()))),
                };
                let (value, unit) = match parsed {
                    Ok(p) => p,
                    Err(e) => {
                        issues.push(format!(
                            "metric {} from {} is not numeric: {e}",
                            repr_str(name),
                            repr_str(&monitor.monitor_id)
                        ));
                        continue;
                    }
                };
                if let Some(spec) = spec
                    && unit != spec.unit
                {
                    issues.push(format!(
                        "metric {} unit {} does not match declared {}",
                        repr_str(name),
                        repr_str(&unit),
                        repr_str(&spec.unit)
                    ));
                }
                if !value.is_finite() {
                    issues.push(format!("metric {} is non-finite", repr_str(name)));
                }
                values.insert(name.clone(), float_json(value));
                units.insert(name.clone(), json!(unit));
                owners.insert(name.clone(), json!(monitor.owner_id));
            }
        }
        for (name, bound) in limits.into_iter().flatten() {
            let Some(b) = bound.as_object() else {
                issues.push(format!("metric limit {} must be an object", repr_str(name)));
                continue;
            };
            let Some(v) = values.get(name) else {
                issues.push(format!("bounded metric {} has no registered value", repr_str(name)));
                continue;
            };
            let unit = units[name].as_str().unwrap_or_default().to_string();
            match b.get("unit") {
                Some(Value::String(u)) if *u != unit => {
                    issues.push(format!(
                        "bounded metric {} unit {} does not match {}",
                        repr_str(name),
                        repr_str(u),
                        repr_str(&unit)
                    ));
                    continue;
                }
                None | Some(Value::Null | Value::String(_)) => {}
                Some(_) => {
                    issues.push(format!("bounded metric {} unit must be text", repr_str(name)));
                    continue;
                }
            }
            let v = v.as_f64().unwrap_or(f64::NAN);
            let check = || -> Result<Vec<String>, String> {
                if matches!(b.get("min"), Some(Value::Bool(_)))
                    || matches!(b.get("max"), Some(Value::Bool(_)))
                {
                    return Err("boolean bounds are not numeric limits".into());
                }
                let parse = |k: &str| -> Result<Option<f64>, String> {
                    match b.get(k) {
                        None | Some(Value::Null) => Ok(None),
                        Some(x) => {
                            crate::orchestration::py_float(x).map(Some).map_err(|e| e.message().to_string())
                        }
                    }
                };
                let low = parse("min")?;
                let high = parse("max")?;
                if low.is_some_and(|l| !l.is_finite()) || high.is_some_and(|h| !h.is_finite()) {
                    return Err("bounds must be finite".into());
                }
                if let (Some(l), Some(h)) = (low, high)
                    && l > h
                {
                    return Err("minimum must not exceed maximum".into());
                }
                let mut out = Vec::new();
                if v.is_finite() {
                    if low.is_some_and(|l| v < l) {
                        out.push(format!(
                            "{name}={} below {}",
                            format_g6(v),
                            crate::pyobj::py_str(&b["min"])
                        ));
                    }
                    if high.is_some_and(|h| v > h) {
                        out.push(format!(
                            "{name}={} above {}",
                            format_g6(v),
                            crate::pyobj::py_str(&b["max"])
                        ));
                    }
                }
                Ok(out)
            };
            match check() {
                Ok(v) => engineering.extend(v),
                Err(e) => issues.push(format!("metric limit {} is not numeric: {e}", repr_str(name))),
            }
        }
        let contract_issues = issues.clone();
        let mut all = contract_issues.clone();
        all.extend(engineering.iter().cloned());
        json!({
            "ok": all.is_empty(),
            "issues": all,
            "values": Value::Object(values),
            "units": Value::Object(units),
            "owners": Value::Object(owners),
            "assessment_schema": "implexity-regime-monitor-assessment/1",
            "computable": contract_issues.is_empty(),
            "contract_issues": contract_issues,
            "engineering_satisfied": engineering.is_empty(),
            "engineering_violations": engineering,
            "generation": generation_at_start,
        })
    }

    pub fn unregister_extensions(&self, owner_id: &str) {
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        let mut changed = s.data.coupling.remove(owner_id).is_some();
        let before = s.data.monitors.len() + s.data.cases.len();
        s.data.monitors.retain(|_, r| r.owner_id != owner_id);
        s.data.cases.retain(|_, r| r.owner_id != owner_id);
        changed |= s.data.monitors.len() + s.data.cases.len() != before;
        if changed {
            s.generation += 1;
        }
    }

    pub fn hold(&self) -> crate::sync::ReLockGuard<'_> {
        self.relock.lock()
    }
}

fn float_json(v: f64) -> Value {
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}

#[must_use]
pub fn format_g6(v: f64) -> String {
    format_g(v, 6)
}

#[must_use]
pub fn format_g(v: f64, precision: usize) -> String {
    if !v.is_finite() {
        return if v.is_nan() {
            "nan".into()
        } else if v > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let p = precision.max(1);
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{:.*e}", p - 1, v);
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let p_i = i32::try_from(p).unwrap_or(i32::MAX);
    if exp < -4 || exp >= p_i {
        let mant = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.') } else { mant };
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        let decimals = usize::try_from(p_i - 1 - exp).unwrap_or(0);
        let fixed = format!("{v:.decimals$}");
        if fixed.contains('.') {
            fixed.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            fixed
        }
    }
}

