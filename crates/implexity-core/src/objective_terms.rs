// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::contributions::{ContributionError, ContributionRegistry, ContributionValue};
use crate::py_repr::repr_str;

pub const KIND: &str = "objective_terms";
pub const DIRECTIONS: [&str; 1] = ["minimise"];
pub const IMPLEMENTATION: &str = "implexity.objective_terms.TermSpec";

#[derive(Debug, Clone, PartialEq)]
pub struct Knob {
    pub default: Value,
    pub unit: String,
    pub doc: String,
}

pub type ApplicabilityFn = dyn Fn(&Value) -> Option<String> + Send + Sync;

#[derive(Clone)]
pub struct TermSpec {
    pub name: String,
    pub backend: String,
    pub family: String,
    pub kind: String,
    pub units: String,
    pub doc: String,
    pub knobs: Vec<(String, Knob)>,
    pub reads: Vec<String>,
    pub direction: String,
    pub calibrated: bool,
    pub reference_key: Option<String>,
    pub provenance: Option<String>,
    pub applicability: Option<Arc<ApplicabilityFn>>,
    pub diagnostics: Vec<(String, String, String)>,
    pub build: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for TermSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermSpec")
            .field("name", &self.name)
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl TermSpec {

    pub fn validate(&self) -> Result<(), ContributionError> {
        let name = repr_str(&self.name);
        for (key, value) in [
            ("name", &self.name),
            ("backend", &self.backend),
            ("family", &self.family),
            ("kind", &self.kind),
            ("units", &self.units),
        ] {
            if value.trim().is_empty() {
                return Err(ContributionError(format!("objective term {name}: {key} must be nonempty text")));
            }
        }
        if !DIRECTIONS.contains(&self.direction.as_str()) {
            return Err(ContributionError(format!(
                "objective term {name}: direction must be one of ('minimise',)"
            )));
        }
        if !self.knobs.iter().any(|(k, _)| k == "weight") {
            return Err(ContributionError(format!(
                "objective term {name}: every term declares a 'weight' knob carrying its default weight"
            )));
        }
        Ok(())
    }
}


pub fn register_term(
    reg: &ContributionRegistry,
    spec: TermSpec,
    owner_id: &str,
) -> Result<Arc<TermSpec>, ContributionError> {
    spec.validate()?;
    let name = spec.name.clone();
    let value = Arc::new(spec);
    reg.register(KIND, &name, ContributionValue::new(Arc::clone(&value), IMPLEMENTATION), owner_id)?;
    Ok(value)
}

#[must_use]
pub fn terms(reg: &ContributionRegistry, backend: Option<&str>) -> Vec<Arc<TermSpec>> {
    reg.entries(KIND)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| v.downcast::<TermSpec>())
        .filter(|t| backend.is_none_or(|b| t.backend == b))
        .collect()
}

#[must_use]
pub fn get(reg: &ContributionRegistry, name: &str) -> Option<Arc<TermSpec>> {
    reg.get(KIND, name).ok().flatten().and_then(|v| v.downcast::<TermSpec>())
}

#[must_use]
pub fn describe(spec: &TermSpec) -> Value {
    let mut m = Map::new();
    m.insert("term".into(), json!(spec.name));
    m.insert("backend".into(), json!(spec.backend));
    m.insert("family".into(), json!(spec.family));
    m.insert("kind".into(), json!(spec.kind));
    m.insert("units".into(), json!(spec.units));
    m.insert("description".into(), json!(spec.doc));
    m.insert("transcribed_from".into(), json!(spec.provenance));
    m.insert("calibrated".into(), json!(spec.calibrated));
    m.insert("reference_key".into(), json!(spec.reference_key));
    m.insert("direction".into(), json!(spec.direction));
    m.insert("reads".into(), json!(spec.reads));
    m.insert(
        "diagnostics".into(),
        Value::Array(
            spec.diagnostics.iter().map(|(k, u, d)| json!({"key": k, "units": u, "doc": d})).collect(),
        ),
    );
    m.insert(
        "knobs".into(),
        Value::Array(
            spec.knobs
                .iter()
                .map(|(n, k)| json!({"name": n, "default": k.default, "units": k.unit, "doc": k.doc}))
                .collect(),
        ),
    );
    Value::Object(m)
}

#[must_use]
pub fn catalogue(reg: &ContributionRegistry, backend: Option<&str>) -> Vec<Value> {
    terms(reg, backend).iter().map(|t| describe(t)).collect()
}

#[must_use]
pub fn applicability(
    reg: &ContributionRegistry,
    names: &[String],
    probe: &Value,
    backend: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut ok = Vec::new();
    let mut refusals = Vec::new();
    for name in names {
        let spec = get(reg, name);
        match spec {
            Some(spec) if backend.is_none_or(|b| spec.backend == b) => {
                if let Some(reason) =
                    spec.applicability.as_ref().and_then(|f| f(probe)).filter(|r| !r.is_empty())
                {
                    refusals.push(reason);
                } else {
                    ok.push(name.clone());
                }
            }
            _ => {
                let mut active: Vec<String> = terms(reg, backend).iter().map(|t| t.name.clone()).collect();
                active.sort();
                refusals.push(format!(
                    "objective term {} is not an active term{}; active: {}",
                    repr_str(name),
                    backend.map_or_else(String::new, |b| format!(" of backend {}", repr_str(b))),
                    crate::pyobj::list_repr(&active)
                ));
            }
        }
    }
    (ok, refusals)
}

#[must_use]
pub fn row_diagnostics(
    reg: &ContributionRegistry,
    names: &[String],
    aux: &BTreeMap<String, f64>,
) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for name in names {
        let Some(spec) = get(reg, name) else { continue };
        for (key, _, _) in &spec.diagnostics {
            if let Some(v) = aux.get(key) {
                out.insert(key.clone(), *v);
            }
        }
    }
    out
}

