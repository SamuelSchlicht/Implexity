// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use implexity_core::component_manifests::{
    HistoryResponseManifest, load_history_catalog, validate_history_manifest,
};
use implexity_core::error::{CaeError, CaeResult};
use implexity_core::orchestration::AddInRegistry;
use serde_json::{Map, Value, json};

use crate::convergence::repr_name_list;

pub const COMPONENT_KIND: &str = "history_response_observer";
pub const HISTORY_OBSERVER_INTERFACE: &str = "history_response_observer";

pub type SampleFn = Arc<dyn Fn(usize, &[f64], &[f64]) -> CaeResult<BTreeMap<String, Vec<f64>>> + Send + Sync>;

#[derive(Clone)]
pub struct HistorySampleContract {
    pub sample: SampleFn,
    pub sample_names: Vec<String>,
    pub times_s: Vec<f64>,
}

impl std::fmt::Debug for HistorySampleContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistorySampleContract")
            .field("sample_names", &self.sample_names)
            .field("times_s", &self.times_s)
            .finish_non_exhaustive()
    }
}

pub trait HistoryResponseObserver: Send + Sync {
    fn response_units(&self) -> Vec<(String, String)>;
    fn requires(&self) -> Vec<String>;


    fn validate(&self, settings: &Value) -> CaeResult<Value>;


    fn bind(
        &self,
        contract: &HistorySampleContract,
        settings: &Value,
    ) -> CaeResult<Box<dyn Any + Send + Sync>>;
}

#[derive(Clone)]
pub struct HistoryObserverHandle(pub Arc<dyn HistoryResponseObserver>);

impl std::fmt::Debug for HistoryObserverHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryObserverHandle")
    }
}

pub struct BoundObserver {
    pub component: String,
    pub required_sample_names: Vec<String>,
    pub observer: Box<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for BoundObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundObserver")
            .field("component", &self.component)
            .field("required_sample_names", &self.required_sample_names)
            .finish_non_exhaustive()
    }
}

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn manifests() -> CaeResult<&'static BTreeMap<String, HistoryResponseManifest>> {
    static CACHE: OnceLock<Result<BTreeMap<String, HistoryResponseManifest>, String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            load_history_catalog(implexity_core::distributions::global()).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| err(e.clone()))
}

#[derive(Clone, Copy)]
pub struct ObserverScope<'a> {
    pub addins: &'a AddInRegistry,
    pub manifests: Option<&'a BTreeMap<String, HistoryResponseManifest>>,
}

impl ObserverScope<'static> {
    #[must_use]
    pub fn global() -> Self {
        Self { addins: &implexity_core::registries::global().addins, manifests: None }
    }
}

impl ObserverScope<'_> {
    fn manifests(&self) -> CaeResult<&BTreeMap<String, HistoryResponseManifest>> {
        match self.manifests {
            Some(m) => Ok(m),
            None => manifests(),
        }
    }
}



pub fn selected_observer(
    scope: &ObserverScope<'_>,
    name: &str,
) -> CaeResult<Arc<dyn HistoryResponseObserver>> {
    let entry = scope.addins.get(name)?;
    let not_observer = || err(format!("{name}: not a native history response/admissibility component"));
    let adapter = entry.adapter.as_ref().ok_or_else(not_observer)?;
    if adapter.component_kind().as_deref() != Some(COMPONENT_KIND) {
        return Err(not_observer());
    }
    let handle = adapter
        .interface(HISTORY_OBSERVER_INTERFACE)
        .and_then(|a| a.downcast_ref::<HistoryObserverHandle>())
        .ok_or_else(not_observer)?;
    let observer = Arc::clone(&handle.0);
    let implementation =
        HistoryResponseManifest { response_units: observer.response_units(), requires: observer.requires() };
    validate_history_manifest(scope.manifests()?, name, &implementation)?;
    Ok(observer)
}



pub fn declarations(scope: &ObserverScope<'_>, rows: Option<&Value>) -> CaeResult<Vec<Value>> {
    let Some(rows) = rows.filter(|r| !r.is_null()) else {
        return Ok(Vec::new());
    };
    let rows = rows.as_array().ok_or_else(|| err("history_observers must be an explicit list"))?;
    let (mut out, mut ids, mut responses) = (Vec::new(), BTreeSet::new(), BTreeSet::new());
    for row in rows {
        let m = row
            .as_object()
            .filter(|m| {
                m.len() == 2 && m.contains_key("settings") && m.get("component").is_some_and(Value::is_string)
            })
            .ok_or_else(|| err("history observer requires component and settings"))?;
        let name = m["component"].as_str().unwrap_or_default().to_string();
        let observer = selected_observer(scope, &name)?;
        if ids.contains(&name) {
            return Err(err("duplicate history observer"));
        }
        let config = observer.validate(&m["settings"])?;
        ids.insert(name.clone());
        let units: BTreeSet<String> = observer.response_units().into_iter().map(|(k, _)| k).collect();
        let collisions: Vec<String> = responses.intersection(&units).cloned().collect();
        if !collisions.is_empty() {
            return Err(err(format!("ambiguous observer responses: {}", repr_name_list(&collisions))));
        }
        responses.extend(units);
        out.push(json!({"component": name, "settings": config}));
    }
    Ok(out)
}



pub fn available_response_units(scope: &ObserverScope<'_>) -> CaeResult<Map<String, Value>> {
    let mut out = Map::new();
    for entry in scope.addins.snapshot().entries {
        let Some(adapter) = entry.adapter.as_ref() else { continue };
        if adapter.component_kind().as_deref() != Some(COMPONENT_KIND) {
            continue;
        }
        let units = adapter
            .interface(HISTORY_OBSERVER_INTERFACE)
            .and_then(|a| a.downcast_ref::<HistoryObserverHandle>())
            .map(|h| h.0.response_units())
            .or_else(|| adapter.response_units().map(|u| u.into_iter().collect()))
            .unwrap_or_default();
        for (name, unit) in units {
            if out.contains_key(&name) {
                return Err(err(format!("ambiguous installed observer response {name}")));
            }
            out.insert(name, json!(unit));
        }
    }
    Ok(out)
}



pub fn selected_response_units(
    scope: &ObserverScope<'_>,
    rows: Option<&Value>,
) -> CaeResult<Map<String, Value>> {
    let mut out = Map::new();
    for row in declarations(scope, rows)? {
        let observer = selected_observer(scope, row["component"].as_str().unwrap_or_default())?;
        for (k, v) in observer.response_units() {
            out.insert(k, json!(v));
        }
    }
    Ok(out)
}



pub fn bind(
    scope: &ObserverScope<'_>,
    rows: Option<&Value>,
    contract: &HistorySampleContract,
) -> CaeResult<Vec<BoundObserver>> {
    let mut out = Vec::new();
    for row in declarations(scope, rows)? {
        let name = row["component"].as_str().unwrap_or_default().to_string();
        let observer = selected_observer(scope, &name)?;
        let requires = observer.requires();
        let available: BTreeSet<&String> = contract.sample_names.iter().collect();
        let absent: Vec<String> = requires
            .iter()
            .filter(|r| !available.contains(r))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !absent.is_empty() {
            return Err(err(format!(
                "{name}: host omits required history samples {}",
                repr_name_list(&absent)
            )));
        }
        let bound = observer.bind(contract, &row["settings"])?;
        out.push(BoundObserver { component: name, required_sample_names: requires, observer: bound });
    }
    Ok(out)
}
