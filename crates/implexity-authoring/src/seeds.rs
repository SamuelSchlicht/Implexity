// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use implexity_geometry::document::{self as D, OutputRef, arrays};
use implexity_geometry::eval::{self as EV, EvalOptions};
use implexity_geometry::field_registration::GridRegistration;
use implexity_geometry::lattice::assembly::ControlledAssembly;
use implexity_geometry::lattice::controls::{CONTROL_SCHEMA, control_contract};
use implexity_geometry::lattice::node::ControlledLattice;
use implexity_geometry::node::NodeRef;
use implexity_geometry::value::{ArrayData, NdArray};
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::model_manager::ModelManager;
use crate::py::{jf, py_str, repr, truthy};
use crate::sync::lock;

pub const SCHEMA: &str = "implexity-geometry-seeds/1";
pub const PROVENANCE_SCHEMA: &str = "implexity-geometry-seed-provenance/1";
pub const BAKE_SCHEMA: &str = "implexity-editable-occupancy-bake/1";
pub const MODES: [&str; 2] = ["parametric", "bake_editable"];
pub const BAKE_SPACING_MM: f64 = 0.5;
pub const BAKE_PADDING_CELLS: i64 = 2;
pub const MAX_BAKE_CELLS: usize = 4_000_000;
pub const BAKE_CHUNK_POINTS: usize = 131_072;

const COMMIT_GUARDS: [&str; 3] =
    ["expected_content_id", "expected_preview_content_id", "expected_preview_sha256"];

#[must_use]
pub fn seed_error(problems: Vec<String>) -> AuthoringError {
    AuthoringError::problems("SeedError", problems)
}

fn serr(message: impl Into<String>) -> AuthoringError {
    seed_error(vec![message.into()])
}

fn ident_ok(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.len() <= 96
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn version_ok(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_digit())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn sha_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[must_use]
pub fn finite_number(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        _ => false,
    }
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn list_of(items: &[String]) -> String {
    repr(&json!(items))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeedParameter {
    pub key: String,
    pub label: String,
    pub ty: String,
    pub units: String,
    pub default: Value,
    pub min: Value,
    pub max: Value,
    pub step: Value,
    pub help: String,
    pub optimisable: bool,
    pub default_free: bool,
}

impl SeedParameter {

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: &str,
        label: &str,
        ty: &str,
        units: &str,
        default: Value,
        min: Value,
        max: Value,
        step: Value,
        help: &str,
        optimisable: bool,
        default_free: bool,
    ) -> AResult<Self> {
        let p = Self {
            key: key.into(),
            label: label.into(),
            ty: ty.into(),
            units: units.into(),
            default,
            min,
            max,
            step,
            help: help.into(),
            optimisable,
            default_free,
        };
        let verr = |m: String| AuthoringError::value("ValueError", m);
        if !ident_ok(key) {
            return Err(verr(format!("seed parameter key {} is not a stable identifier", repr(&json!(key)))));
        }
        if !matches!(ty, "number" | "integer" | "boolean") {
            return Err(verr(format!("seed parameter {key} has unsupported type {}", repr(&json!(ty)))));
        }
        if label.trim().is_empty() {
            return Err(verr(format!("seed parameter {key} has no label")));
        }
        p.normalise(&p.default)?;
        if ty != "boolean" {
            for (bound, value) in [("min", &p.min), ("max", &p.max), ("step", &p.step)] {
                if !value.is_null() && !finite_number(value) {
                    return Err(verr(format!("seed parameter {key}.{bound} must be finite")));
                }
            }
            if !p.min.is_null() && !p.max.is_null() && num(&p.min) > num(&p.max) {
                return Err(verr(format!("seed parameter {key} has min above max")));
            }
            if !p.step.is_null() && num(&p.step) <= 0.0 {
                return Err(verr(format!("seed parameter {key}.step must be positive")));
            }
        }
        if default_free && !optimisable {
            return Err(verr(format!("seed parameter {key} is default_free but not optimisable")));
        }
        Ok(p)
    }


    pub fn normalise(&self, value: &Value) -> AResult<Value> {
        if self.ty == "boolean" {
            return value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| serr(format!("parameters.{} must be a boolean", self.key)));
        }
        if !finite_number(value) {
            return Err(serr(format!("parameters.{} must be a finite {}", self.key, self.ty)));
        }
        let out = if self.ty == "integer" {
            let f = num(value);
            let i = value.as_i64().unwrap_or_else(|| {
                #[allow(clippy::cast_possible_truncation)]
                let t = f.trunc() as i64;
                t
            });
            #[allow(clippy::cast_precision_loss)]
            if f != i as f64 {
                return Err(serr(format!("parameters.{} must be an integer", self.key)));
            }
            json!(i)
        } else {
            jf(num(value))
        };
        let o = num(&out);
        if !self.min.is_null() && o < num(&self.min) {
            return Err(serr(format!(
                "parameters.{} is {}, below its minimum {}",
                self.key,
                py_str(&out),
                py_str(&self.min)
            )));
        }
        if !self.max.is_null() && o > num(&self.max) {
            return Err(serr(format!(
                "parameters.{} is {}, above its maximum {}",
                self.key,
                py_str(&out),
                py_str(&self.max)
            )));
        }
        Ok(out)
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        json!({"key": self.key, "label": self.label, "type": self.ty, "units": self.units,
            "default": self.default, "min": self.min, "max": self.max, "step": self.step,
            "help": self.help, "optimisable": self.optimisable, "default_free": self.default_free})
    }
}

pub type SeedBuilder = Arc<dyn Fn(&Map<String, Value>) -> AResult<Value> + Send + Sync>;

#[derive(Clone)]
pub struct SeedDefinition {
    pub id: String,
    pub version: String,
    pub label: String,
    pub description: String,
    pub category: String,
    pub addin_id: String,
    pub parameters: Vec<SeedParameter>,
    pub builder: SeedBuilder,
    pub modes: Vec<String>,
    pub creation_only: bool,
}

impl std::fmt::Debug for SeedDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SeedDefinition({} {})", self.id, self.version)
    }
}

impl SeedDefinition {

    pub fn validate(&self) -> AResult<()> {
        let verr = |m: String| AuthoringError::value("ValueError", m);
        for (field, value) in [("id", &self.id), ("category", &self.category), ("addin_id", &self.addin_id)] {
            if !ident_ok(value) {
                return Err(verr(format!("seed {field} {} is not a stable identifier", repr(&json!(value)))));
            }
        }
        if !version_ok(&self.version) {
            return Err(verr(format!("seed version {} is not a stable version", repr(&json!(self.version)))));
        }
        if self.label.trim().is_empty() || self.description.trim().is_empty() {
            return Err(verr(format!("seed {} needs a label and description", self.id)));
        }
        let keys: BTreeSet<&str> = self.parameters.iter().map(|p| p.key.as_str()).collect();
        if keys.len() != self.parameters.len() {
            return Err(verr(format!("seed {} has duplicate parameter keys", self.id)));
        }
        if self.modes.is_empty() || self.modes.iter().any(|m| !MODES.contains(&m.as_str())) {
            return Err(verr(format!(
                "seed {} modes must be drawn from {}",
                self.id,
                list_of(&MODES.map(String::from))
            )));
        }
        let unique: BTreeSet<&String> = self.modes.iter().collect();
        if unique.len() != self.modes.len() {
            return Err(verr(format!("seed {} has duplicate modes", self.id)));
        }
        Ok(())
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        json!({"id": self.id, "version": self.version, "label": self.label,
            "description": self.description, "category": self.category, "addin_id": self.addin_id,
            "parameters": self.parameters.iter().map(SeedParameter::describe).collect::<Vec<_>>(),
            "modes": self.modes, "creation_only": self.creation_only})
    }
}

#[derive(Default)]
pub struct SeedRegistry {
    items: Mutex<BTreeMap<String, Arc<SeedDefinition>>>,
}

impl SeedRegistry {

    pub fn register(&self, definition: SeedDefinition) -> AResult<Arc<SeedDefinition>> {
        definition.validate()?;
        let mut items = lock(&self.items);
        if let Some(old) = items.get(&definition.id) {
            return Err(AuthoringError::value(
                "ValueError",
                format!(
                    "geometry seed {} is already registered by {} at version {}",
                    repr(&json!(definition.id)),
                    old.addin_id,
                    old.version
                ),
            ));
        }
        let d = Arc::new(definition);
        items.insert(d.id.clone(), Arc::clone(&d));
        Ok(d)
    }


    pub fn get(&self, seed_id: &str) -> AResult<Arc<SeedDefinition>> {
        let items = lock(&self.items);
        items.get(seed_id).cloned().ok_or_else(|| {
            let names: Vec<&str> = items.keys().map(String::as_str).collect();
            serr(format!(
                "unknown seed_id {}; available seeds are {}",
                repr(&json!(seed_id)),
                if names.is_empty() { "none".to_string() } else { names.join(", ") }
            ))
        })
    }

    #[must_use]
    pub fn values(&self) -> Vec<Arc<SeedDefinition>> {
        lock(&self.items).values().cloned().collect()
    }
}

pub fn registry() -> &'static SeedRegistry {
    static REG: OnceLock<SeedRegistry> = OnceLock::new();
    REG.get_or_init(|| {
        let reg = SeedRegistry::default();
        for d in core_seeds() {

            let _ = reg.register(d);
        }
        reg
    })
}


pub fn register(definition: SeedDefinition) -> AResult<Arc<SeedDefinition>> {
    registry().register(definition)
}

#[must_use]
pub fn catalogue() -> Value {
    let rows: Vec<Value> = registry()
        .values()
        .iter()
        .map(|d| {
            let mut row = d.describe();
            if d.id == "lattice.controlled-volume"
                && let Some(o) = row.as_object_mut()
            {
                o.insert("spatial_control_contract".into(), control_contract());
            }
            row
        })
        .collect();
    json!({
        "kind": "implicit_geometry_seed_catalogue", "schema": SCHEMA,
        "seeds": rows, "count": rows.len(), "modes": MODES,
        "bake_current": {
            "endpoint": "POST /v1/implicit/seeds/bake-current",
            "preview_endpoint": "POST /v1/implicit/seeds/bake-current/preview",
            "request": {"expected_content_id": "<current content_id>",
                "expected_preview_content_id": "<preview content_id>",
                "expected_preview_sha256": "<preview sha256>",
                "bake": {"spacing_mm": [0.5, 0.5, 0.5], "padding_cells": 2, "iso_mm": 0.0}},
            "note": "Tune a parametric model first, then explicitly replace its root with registered editable occupancy; the exact source DAG remains a named output.",
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BakeConfig {
    pub spacing: [f64; 3],
    pub padding: i64,
    pub iso: f64,
}

impl Default for BakeConfig {
    fn default() -> Self {
        Self { spacing: [BAKE_SPACING_MM; 3], padding: BAKE_PADDING_CELLS, iso: 0.0 }
    }
}

fn sorted_unknown(obj: &Map<String, Value>, allowed: &[&str]) -> Vec<String> {
    let mut u: Vec<String> = obj.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
    u.sort();
    u
}


pub fn normalise_bake_config(raw: Option<&Value>) -> AResult<BakeConfig> {
    let Some(obj) = raw.and_then(Value::as_object) else {
        return Err(serr("bake must be an object with spacing_mm, padding_cells and iso_mm"));
    };
    let unknown = sorted_unknown(obj, &["spacing_mm", "padding_cells", "iso_mm"]);
    if !unknown.is_empty() {
        return Err(serr(format!(
            "unknown bake field(s) {}; allowed fields are spacing_mm, padding_cells and iso_mm",
            list_of(&unknown)
        )));
    }
    let Some(spacing_raw) = obj.get("spacing_mm") else { return Err(serr("bake.spacing_mm is required")) };
    let spacing: Vec<f64> = if finite_number(spacing_raw) {
        vec![num(spacing_raw); 3]
    } else {
        let a = crate::py::Arr::from_json(spacing_raw)
            .map_err(|_| serr("bake.spacing_mm must be one positive number or three positive numbers"))?;
        if a.data.len() != 3 {
            return Err(serr("bake.spacing_mm must have exactly three values"));
        }
        a.data
    };
    if !spacing.iter().all(|s| s.is_finite()) || spacing.iter().any(|s| *s <= 0.0) {
        return Err(serr("bake.spacing_mm must be finite and positive on every axis"));
    }
    let padding = match obj.get("padding_cells") {
        None => BAKE_PADDING_CELLS,
        Some(v) if (v.is_i64() || v.is_u64()) && v.as_i64().is_some_and(|p| (0..=64).contains(&p)) => {
            v.as_i64().unwrap_or(0)
        }
        Some(_) => return Err(serr("bake.padding_cells must be an integer from 0 to 64")),
    };
    let iso = match obj.get("iso_mm") {
        None => 0.0,
        Some(v) if finite_number(v) => num(v),
        Some(_) => return Err(serr("bake.iso_mm must be finite")),
    };
    Ok(BakeConfig { spacing: [spacing[0], spacing[1], spacing[2]], padding, iso })
}

struct Normalised {
    definition: Arc<SeedDefinition>,
    values: Map<String, Value>,
    free: Vec<String>,
    mode: String,
    bake: BakeConfig,
}

fn normalise_request(request: &Value) -> AResult<Normalised> {
    let Some(obj) = request.as_object() else { return Err(serr("the seed request body must be an object")) };
    let allowed = ["seed_id", "version", "parameters", "free", "free_bounds", "mode", "bake"];
    let unknown = sorted_unknown(obj, &allowed);
    if !unknown.is_empty() {
        let mut al: Vec<String> = allowed.iter().map(|s| (*s).to_string()).collect();
        al.sort();
        return Err(serr(format!(
            "unknown seed request field(s) {}; allowed fields are {}",
            list_of(&unknown),
            list_of(&al)
        )));
    }
    let seed_id = match obj.get("seed_id") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(serr("seed_id is required and must be a non-empty string")),
    };
    let definition = registry().get(&seed_id)?;
    let version = obj.get("version").cloned().unwrap_or(Value::Null);
    if version.as_str() != Some(definition.version.as_str()) {
        return Err(serr(format!(
            "seed {} version must be exactly {}, got {}",
            definition.id,
            repr(&json!(definition.version)),
            repr(&version)
        )));
    }
    let mode_v = obj.get("mode").cloned().unwrap_or_else(|| json!("parametric"));
    let mode = match mode_v.as_str() {
        Some(m) if definition.modes.iter().any(|x| x == m) => m.to_string(),
        _ => {
            return Err(serr(format!(
                "seed {} does not support mode {}; it supports {}",
                definition.id,
                repr(&mode_v),
                list_of(&definition.modes)
            )));
        }
    };
    if mode == "parametric" && obj.contains_key("bake") {
        return Err(serr("bake configuration is only valid in bake_editable mode"));
    }
    let bake = if mode == "bake_editable" && obj.contains_key("bake") {
        normalise_bake_config(obj.get("bake"))?
    } else {
        BakeConfig::default()
    };
    let supplied = match obj.get("parameters") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(serr("parameters must be an object of seed key to value")),
    };
    let by_key: BTreeMap<&str, &SeedParameter> =
        definition.parameters.iter().map(|p| (p.key.as_str(), p)).collect();
    let mut extra: Vec<String> =
        supplied.keys().filter(|k| !by_key.contains_key(k.as_str())).cloned().collect();
    extra.sort();
    if !extra.is_empty() {
        let have: Vec<String> = by_key.keys().map(|k| (*k).to_string()).collect();
        return Err(serr(format!(
            "seed {} has no parameter(s) {}; it has {}",
            definition.id,
            list_of(&extra),
            list_of(&have)
        )));
    }
    let mut values = Map::new();
    for p in &definition.parameters {
        let v = supplied.get(&p.key).unwrap_or(&p.default);
        values.insert(p.key.clone(), p.normalise(v)?);
    }
    let mut free: Vec<String> = if let Some(raw) = obj.get("free") {
        let Some(list) = raw.as_array() else {
            return Err(serr("free must be a list of seed parameter keys"));
        };
        list.iter().map(py_str).collect()
    } else {
        definition.parameters.iter().filter(|p| p.default_free).map(|p| p.key.clone()).collect()
    };
    let unique: BTreeSet<&String> = free.iter().collect();
    if unique.len() != free.len() {
        return Err(serr("free contains a duplicate parameter key"));
    }
    let mut unknown_free: Vec<String> =
        free.iter().filter(|k| !by_key.contains_key(k.as_str())).cloned().collect();
    unknown_free.sort();
    if !unknown_free.is_empty() {
        return Err(serr(format!("free names unknown seed parameter(s) {}", list_of(&unknown_free))));
    }
    let mut locked: Vec<String> = free.iter().filter(|k| !by_key[k.as_str()].optimisable).cloned().collect();
    locked.sort();
    if !locked.is_empty() {
        return Err(serr(format!("free names non-optimisable seed parameter(s) {}", list_of(&locked))));
    }
    free.sort();
    if mode == "bake_editable" && !free.is_empty() {
        return Err(serr(format!(
            "bake_editable cannot retain free source parameters {}: the baked occupancy is the editable root and source parameters no longer drive it; use free=[] or choose parametric mode",
            list_of(&free)
        )));
    }
    Ok(Normalised { definition, values, free, mode, bake })
}

fn apply_parameters(
    doc: &mut Value,
    definition: &SeedDefinition,
    values: &Map<String, Value>,
    free: &[String],
    bounds: Option<&Value>,
) -> AResult<()> {
    let empty = json!({});
    let bounds = match bounds {
        None | Some(Value::Null) => &empty,
        Some(b) => b,
    };
    let bounds_ok = bounds.as_object().is_some_and(|b| b.keys().all(|k| free.contains(k)));
    if !bounds_ok {
        return Err(serr("free_bounds must contain only selected free parameter keys"));
    }
    let bmap = bounds.as_object().cloned().unwrap_or_default();
    if definition.creation_only {
        if !free.is_empty() || !bmap.is_empty() {
            return Err(serr(
                "creation-only volume settings cannot be optimization coordinates; edit the spatial controls",
            ));
        }
        return Ok(());
    }
    let params = doc.get_mut("parameters");
    if definition.parameters.is_empty() && params.as_ref().is_none_or(|p| p.is_null()) {
        return Ok(());
    }
    let Some(params) = params.and_then(Value::as_object_mut) else {
        return Err(serr(format!("seed {} document has no named parameter table", definition.id)));
    };
    let mut problems = Vec::new();
    for descriptor in &definition.parameters {
        let key = &descriptor.key;
        let Some(entry) = params
            .get_mut(key)
            .and_then(Value::as_object_mut)
            .filter(|e| !e.contains_key("expr") && e.contains_key("value"))
        else {
            problems.push(format!(
                "seed {} parameter {key} must map to one literal document parameter",
                definition.id
            ));
            continue;
        };
        entry.insert("value".into(), values[key].clone());
        if free.contains(key) {
            let pair = bmap.get(key).cloned().unwrap_or_else(|| {
                json!({"min": entry.get("min").cloned().unwrap_or(Value::Null),
                       "max": entry.get("max").cloned().unwrap_or(Value::Null)})
            });
            let v = num(&values[key]);
            let ok = pair.as_object().is_some_and(|p| {
                let mut ks: Vec<&str> = p.keys().map(String::as_str).collect();
                ks.sort_unstable();
                ks == ["max", "min"]
                    && finite_number(&p["min"])
                    && finite_number(&p["max"])
                    && num(&p["min"]) < num(&p["max"])
                    && num(&p["min"]) <= v
                    && v <= num(&p["max"])
            });
            if !ok {
                return Err(serr(format!(
                    "free parameter {key} requires explicit finite min < max containing its value"
                )));
            }
            descriptor.normalise(&pair["min"])?;
            descriptor.normalise(&pair["max"])?;
            entry.insert("min".into(), pair["min"].clone());
            entry.insert("max".into(), pair["max"].clone());
            entry.insert("free".into(), json!(true));
        } else {
            entry.shift_remove("free");
        }
    }
    if !problems.is_empty() {
        return Err(seed_error(problems));
    }
    Ok(())
}

fn with_seed_meta(doc: &Value, seed_meta: &Value) -> AResult<Value> {
    let mut out = doc.clone();
    let o = out.as_object_mut().ok_or_else(|| serr("generated document must be an object"))?;
    let meta = o.entry("meta").or_insert_with(|| json!({}));
    if meta.is_null() {
        *meta = json!({});
    }
    let Some(meta) = meta.as_object_mut() else {
        return Err(serr("generated document meta must be an object"));
    };
    let implexity = meta.entry("implexity").or_insert_with(|| json!({}));
    let Some(implexity) = implexity.as_object_mut() else {
        return Err(serr("generated document meta.implexity must be an object"));
    };
    implexity.insert("seed".into(), seed_meta.clone());
    Ok(out)
}

fn identity(model: &D::Model) -> AResult<Value> {
    Ok(json!({"schema": model.doc["schema"].clone(), "structure_id": model.structure_id(),
        "content_id": model.content_id(), "sha256": model.sha256()?,
        "root": model.doc.get("root").cloned().unwrap_or(Value::Null), "nodes": model.node_table().len()}))
}

fn fresh(base: &str, mapping: &Map<String, Value>) -> String {
    if !mapping.contains_key(base) {
        return base.to_string();
    }
    let mut i = 2;
    while mapping.contains_key(&format!("{base}_{i}")) {
        i += 1;
    }
    format!("{base}_{i}")
}

fn f3(v: [f64; 3]) -> Value {
    Value::Array(v.iter().map(|x| jf(*x)).collect())
}

fn bake_editable(
    source_model: &D::Model,
    source_doc: &Value,
    seed_meta: &Value,
    cfg: &BakeConfig,
) -> AResult<(Value, Value)> {
    let Some(root) = source_model.root() else { return Err(serr("bake_editable requires a seed root")) };
    let Some((lo, hi)) = implexity_mesh::interop::subtree_box(&root) else {
        return Err(serr(
            "bake_editable requires a bounded seed root; this seed has no provable finite extent",
        ));
    };
    if !lo.iter().chain(hi.iter()).all(|v| v.is_finite()) || (0..3).any(|a| hi[a] <= lo[a]) {
        return Err(serr(format!(
            "bake_editable received an invalid finite root extent {}",
            repr(&json!([f3(lo), f3(hi)]))
        )));
    }
    let spacing = cfg.spacing;
    if !spacing.iter().all(|s| s.is_finite()) || spacing.iter().any(|s| *s <= 0.0) {
        return Err(serr("bake_editable spacing must be three finite positive numbers"));
    }
    if !(0..=64).contains(&cfg.padding) {
        return Err(serr("bake_editable padding_cells must be from 0 to 64"));
    }
    if !cfg.iso.is_finite() {
        return Err(serr("bake_editable iso_mm must be finite"));
    }
    let iso = cfg.iso;
    let scaled_lo = [lo[0] / spacing[0], lo[1] / spacing[1], lo[2] / spacing[2]];
    let scaled_hi = [hi[0] / spacing[0], hi[1] / spacing[1], hi[2] / spacing[2]];
    if !scaled_lo.iter().chain(scaled_hi.iter()).all(|v| v.is_finite()) {
        return Err(serr(
            "bake_editable spacing makes the registered lattice indices non-finite; use a larger finite spacing",
        ));
    }
    let floor_lo = scaled_lo.map(f64::floor);
    let ceil_hi = scaled_hi.map(f64::ceil);
    #[allow(clippy::cast_precision_loss)]
    let (lower, upper) = ((i64::MIN + cfg.padding) as f64, (i64::MAX - cfg.padding) as f64);
    if floor_lo.iter().chain(ceil_hi.iter()).any(|v| *v < lower || *v > upper) {
        return Err(serr(
            "bake_editable registered lattice indices exceed the supported signed 64-bit range",
        ));
    }
    #[allow(clippy::cast_possible_truncation)]
    let start: [i64; 3] = floor_lo.map(|v| v as i64 - cfg.padding);
    #[allow(clippy::cast_possible_truncation)]
    let stop: [i64; 3] = ceil_hi.map(|v| v as i64 + cfg.padding);
    let shape_i: [i64; 3] = [stop[0] - start[0], stop[1] - start[1], stop[2] - start[2]];
    if shape_i.iter().any(|n| *n < 2) {
        return Err(serr(format!(
            "bake_editable grid must have at least two cells per axis, got [{}, {}, {}]",
            shape_i[0], shape_i[1], shape_i[2]
        )));
    }
    let cell_count_i = i128::from(shape_i[0]) * i128::from(shape_i[1]) * i128::from(shape_i[2]);
    if cell_count_i > MAX_BAKE_CELLS as i128 {
        let min_s = spacing.iter().copied().fold(f64::INFINITY, f64::min);
        return Err(serr(format!(
            "bake_editable needs {cell_count_i} cells for this seed at {} mm; the deterministic preview limit is {MAX_BAKE_CELLS}.  Keep it parametric or register a deliberately bounded seed",
            implexity_geometry::pyfmt::fmt_g(min_s, 3)
        )));
    }
    let shape: [usize; 3] = shape_i.map(|n| usize::try_from(n).unwrap_or(0));
    let cell_count = shape[0] * shape[1] * shape[2];
    #[allow(clippy::cast_precision_loss)]
    let origin = [start[0] as f64 * spacing[0], start[1] as f64 * spacing[1], start[2] as f64 * spacing[2]];
    if !origin.iter().all(|v| v.is_finite()) {
        return Err(serr("bake_editable registered lattice origin is not finite"));
    }
    let registration = GridRegistration::new(
        shape,
        origin,
        [[spacing[0], 0.0, 0.0], [0.0, spacing[1], 0.0], [0.0, 0.0, spacing[2]]],
        "cell",
        "xyz",
        "model",
    )?
    .to_wire();
    let (ny, nz) = (shape[1], shape[2]);
    let slab = (BAKE_CHUNK_POINTS / (ny * nz).max(1)).max(1);
    let mut occupancy: Vec<u8> = Vec::with_capacity(cell_count);
    let opts = EvalOptions { validate: false, ..EvalOptions::exact() };
    let mut i0 = 0;
    while i0 < shape[0] {
        let i1 = shape[0].min(i0 + slab);
        let mut points = Vec::with_capacity((i1 - i0) * ny * nz);
        for i in i0..i1 {
            for j in 0..ny {
                for k in 0..nz {
                    #[allow(clippy::cast_precision_loss)]
                    let idx = [i as f64, j as f64, k as f64];
                    points.push([
                        origin[0] + (idx[0] + 0.5) * spacing[0],
                        origin[1] + (idx[1] + 0.5) * spacing[1],
                        origin[2] + (idx[2] + 0.5) * spacing[2],
                    ]);
                }
            }
        }
        let phi = EV::eval_points(&root, &points, &opts).map_err(|e| {
            serr(format!(
                "bake_editable could not evaluate the seed root at registered cell centres: {}: {}",
                AuthoringError::from(e.clone()).class(),
                e
            ))
        })?;
        if phi.len() != points.len() || !phi.iter().all(|v| v.is_finite()) {
            return Err(serr(format!(
                "bake_editable root returned non-finite values or shape [{}] for {} cell centres",
                phi.len(),
                points.len()
            )));
        }
        occupancy.extend(phi.iter().map(|v| u8::from(*v <= iso)));
        i0 = i1;
    }
    let occ = NdArray::new(shape.to_vec(), ArrayData::U8(occupancy))
        .ok_or_else(|| serr("occupancy shape mismatch"))?;
    let (entry, raw) = arrays::encode_array(&occ);
    let mut out = source_doc.clone();
    let o = out.as_object_mut().ok_or_else(|| serr("generated document must be an object"))?;
    let arrays_tbl = o.entry("arrays").or_insert_with(|| json!({}));
    let arrays_map = arrays_tbl.as_object_mut().ok_or_else(|| serr("arrays must be an object"))?;
    let array_key = fresh("seed_editable_occupancy", arrays_map);
    arrays_map.insert(array_key.clone(), arrays::inline_entry(&entry, &raw));
    let topology_ref = "model:samples";
    let nodes = o.entry("nodes").or_insert_with(|| json!({}));
    let nodes_map = nodes.as_object_mut().ok_or_else(|| serr("nodes must be an object"))?;
    let node_id = fresh("seed_editable", nodes_map);
    let source_root = o.get("root").cloned().unwrap_or(Value::Null);
    let occ_sha = entry.get("sha256").cloned().unwrap_or(Value::Null);
    let nodes_map =
        o.get_mut("nodes").and_then(Value::as_object_mut).ok_or_else(|| serr("nodes must be an object"))?;
    nodes_map.insert(
        node_id.clone(),
        json!({
            "kind": "cell_grid_field",
            "params": {"samples": {"array": array_key}, "origin": f3(origin), "spacing": f3(spacing),
                "scale": -1.0, "offset": -0.5},
            "attrs": {
                "source": {"schema": BAKE_SCHEMA, "seed_id": seed_meta["seed_id"].clone(),
                    "seed_version": seed_meta["version"].clone(), "source_root": source_root,
                    "source_content_id": seed_meta["source_geometry"]["content_id"].clone(),
                    "source_sha256": seed_meta["source_geometry"]["sha256"].clone(),
                    "array_key": array_key, "occupancy_sha256": occ_sha,
                    "topology_ref": topology_ref, "registration": registration,
                    "field_semantics": "occupancy", "design_role": "topology", "representation": "occupancy"},
                "measurement": {"mapping": "rho = 1 if source_f_mm <= iso_mm else 0", "centering": "cell",
                    "iso_value": 0.5, "interpolated_field": "0.5 - rho", "source_iso_mm": jf(iso),
                    "spacing_mm": f3(spacing), "padding_cells": cfg.padding},
            },
            "doc": "editable cell-centred occupancy baked from the retained parametric seed",
            "children": [],
        }),
    );
    let outputs = o.entry("outputs").or_insert_with(|| json!({}));
    if outputs.is_null() {
        *outputs = json!({});
    }
    let outputs_map = outputs.as_object_mut().ok_or_else(|| serr("outputs must be an object"))?;
    let source_output = fresh("seed_parametric_source", outputs_map);
    outputs_map.insert(source_output.clone(), source_root);
    let editable_output = fresh("editable_topology", outputs_map);
    outputs_map.insert(editable_output.clone(), json!(node_id));
    o.insert("root".into(), json!(node_id));
    if let Some(params) = o.get_mut("parameters").and_then(Value::as_object_mut) {
        for p in params.values_mut() {
            if let Some(pm) = p.as_object_mut() {
                pm.shift_remove("free");
            }
        }
    }
    let bake = json!({
        "schema": BAKE_SCHEMA, "centering": "cell", "spacing_mm": f3(spacing), "origin_mm": f3(origin),
        "shape": shape, "cells": cell_count, "source_iso_mm": jf(iso),
        "occupancy_mapping": "rho = 1 if source_f_mm <= iso_mm else 0",
        "occupancy_sha256": occ_sha, "array_key": array_key, "registration": registration,
        "node": node_id, "topology_ref": topology_ref, "source_output": source_output,
        "editable_output": editable_output, "padding_cells": cfg.padding,
        "source_extent_mm": [f3(lo), f3(hi)],
    });
    let mut meta = seed_meta.clone();
    if let Some(m) = meta.as_object_mut() {
        m.insert("bake".into(), bake.clone());
    }
    let mut out = with_seed_meta(&out, &meta)?;
    if let Some(imp) = out.pointer_mut("/meta/implexity").and_then(Value::as_object_mut) {
        imp.insert(
            "topology".into(),
            json!({"coordinate": "model:control", "ref": topology_ref, "lower": 0.0, "upper": 1.0,
                "centering": "cell", "array_key": array_key, "occupancy_sha256": occ_sha,
                "registration": registration, "registration_schema": registration["schema"].clone(),
                "registration_id": registration["registration_id"].clone()}),
        );
    }
    Ok((out, bake))
}


pub fn preview(request: &Value) -> AResult<Value> {
    let n = normalise_request(request)?;
    let def = &n.definition;
    let mut source_doc = (def.builder)(&n.values)?;
    if !source_doc.is_object() {
        return Err(serr(format!(
            "seed {} builder returned {}, not a model document",
            def.id,
            crate::py::type_name(&source_doc)
        )));
    }
    apply_parameters(&mut source_doc, def, &n.values, &n.free, request.get("free_bounds"))?;
    let source_model = D::build(&source_doc, None, None).map_err(|e| match e {
        implexity_geometry::GeometryError::ModelDoc(p) => {
            seed_error(p.iter().map(|x| format!("seed {} generated an invalid model: {x}", def.id)).collect())
        }
        other => AuthoringError::Geometry(other),
    })?;
    let source_norm = source_model.to_doc()?;
    let source_identity = identity(&source_model)?;
    let seed_meta = json!({"schema": PROVENANCE_SCHEMA, "seed_id": def.id, "version": def.version,
        "addin_id": def.addin_id, "mode": n.mode, "parameters": n.values, "free": n.free,
        "source_geometry": source_identity});
    let (document, bake) = if n.mode == "parametric" {
        (with_seed_meta(&source_norm, &seed_meta)?, None)
    } else {
        let (d, b) = bake_editable(&source_model, &source_norm, &seed_meta, &n.bake)?;
        (d, Some(b))
    };
    let model = D::build(&document, None, None).map_err(|e| match e {
        implexity_geometry::GeometryError::ModelDoc(p) => seed_error(
            p.iter().map(|x| format!("seed {} {} output is invalid: {x}", def.id, n.mode)).collect(),
        ),
        other => AuthoringError::Geometry(other),
    })?;
    let document = model.to_doc()?;
    let mut out = json!({"kind": "implicit_geometry_seed_preview", "schema": SCHEMA,
        "seed": def.describe(), "mode": n.mode, "document": document,
        "structure_id": model.structure_id(), "content_id": model.content_id(),
        "sha256": model.sha256()?, "parameters": n.values, "free": n.free,
        "warnings": model.warnings, "reusable_as_model": true, "source_geometry": source_identity});
    if let (Some(b), Some(o)) = (bake, out.as_object_mut()) {
        o.insert("bake".into(), b);
    }
    Ok(out)
}

fn commit_expectations(
    request: &Map<String, Value>,
    action: &str,
    allow_empty_current: bool,
) -> AResult<(Option<String>, String, String)> {
    let missing: Vec<&str> = COMMIT_GUARDS.iter().copied().filter(|k| !request.contains_key(*k)).collect();
    if !missing.is_empty() {
        return Err(serr(format!(
            "{action} requires {} from the current model and the exact non-mutating preview",
            missing.join(", ")
        )));
    }
    let expected = request.get("expected_content_id").cloned().unwrap_or(Value::Null);
    let expected = if allow_empty_current {
        match expected {
            Value::Null => None,
            Value::String(s) => Some(s),
            _ => return Err(serr("expected_content_id must be a string or null")),
        }
    } else {
        match expected {
            Value::String(s) if !s.is_empty() => Some(s),
            _ => {
                return Err(serr(
                    "expected_content_id must be the non-empty content_id of the live parametric model",
                ));
            }
        }
    };
    let preview_content = match request.get("expected_preview_content_id") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(serr(
                "expected_preview_content_id must be the non-empty content_id returned by preview",
            ));
        }
    };
    let preview_sha = match request.get("expected_preview_sha256") {
        Some(Value::String(s)) if sha_ok(s) => s.clone(),
        _ => {
            return Err(serr(
                "expected_preview_sha256 must be the full lowercase SHA-256 returned by preview",
            ));
        }
    };
    Ok((expected, preview_content, preview_sha))
}

fn require_preview_identity(
    built: &Value,
    expected_content: &str,
    expected_sha: &str,
    action: &str,
) -> AResult<()> {
    let s = |k: &str| built.get(k).filter(|v| truthy(v)).map(py_str).unwrap_or_default();
    let (actual_content, actual_sha) = (s("content_id"), s("sha256"));
    if actual_content != expected_content || actual_sha != expected_sha {
        return Err(serr(format!(
            "{action} preview identity guard failed: expected content_id {} and sha256 {expected_sha}, rebuilt content_id is {} and sha256 is {actual_sha}; preview again and do not persist this output",
            repr(&json!(expected_content)),
            repr(&json!(actual_content))
        )));
    }
    Ok(())
}

fn opt_repr(v: Option<&str>) -> String {
    v.map_or_else(|| "None".to_string(), |s| repr(&json!(s)))
}


pub fn commit(manager: &ModelManager, request: &Value) -> AResult<Value> {
    let Some(obj) = request.as_object() else { return Err(serr("the seed request body must be an object")) };
    let (expected, preview_content, preview_sha) = commit_expectations(obj, "seed commit", true)?;
    let seed_request: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !COMMIT_GUARDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let _g = manager.live_lock();
    let current = manager.model().map(|m| m.content_id().unwrap_or_else(|| "None".into()));
    if expected != current {
        return Err(serr(format!(
            "seed commit content guard failed: expected {}, current content_id is {}; preview again before replacing the model",
            opt_repr(expected.as_deref()),
            opt_repr(current.as_deref())
        )));
    }
    let built = preview(&Value::Object(seed_request))?;
    require_preview_identity(&built, &preview_content, &preview_sha, "seed commit")?;
    let mut status = manager.put(&built["document"])?;
    let receipt = json!({"kind": "implicit_geometry_seed_commit", "schema": SCHEMA,
        "seed_id": built["seed"]["id"].clone(), "version": built["seed"]["version"].clone(),
        "mode": built["mode"].clone(), "previous_content_id": current, "expected_content_id": expected,
        "expected_preview_content_id": preview_content, "expected_preview_sha256": preview_sha,
        "content_id": status["content_id"].clone(), "structure_id": status["structure_id"].clone(),
        "sha256": status["sha256"].clone()});
    if let Some(o) = status.as_object_mut() {
        o.insert("seed_commit".into(), receipt);
    }
    Ok(status)
}


pub fn preview_current(manager: &ModelManager, request: &Value) -> AResult<Value> {
    let _g = manager.live_lock();
    prepare_current_bake(manager, request)
}


pub fn bake_current(manager: &ModelManager, request: &Value) -> AResult<Value> {
    let Some(obj) = request.as_object() else {
        return Err(serr("the bake-current request body must be an object"));
    };
    let (_expected, preview_content, preview_sha) = commit_expectations(obj, "bake-current", false)?;
    let bake_request: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "expected_preview_content_id" | "expected_preview_sha256"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let _g = manager.live_lock();
    let built = prepare_current_bake(manager, &Value::Object(bake_request))?;
    require_preview_identity(&built, &preview_content, &preview_sha, "bake-current")?;
    let mut status = manager.put(&built["document"])?;
    let receipt = json!({"kind": "implicit_geometry_seed_bake", "schema": SCHEMA,
        "previous_content_id": built["source_geometry"]["content_id"].clone(),
        "expected_content_id": built["expected_content_id"].clone(),
        "expected_preview_content_id": preview_content, "expected_preview_sha256": preview_sha,
        "content_id": status["content_id"].clone(), "structure_id": status["structure_id"].clone(),
        "sha256": status["sha256"].clone(), "source_geometry": built["source_geometry"].clone(),
        "registration": built["bake"].clone(),
        "deactivated_free_parameters": built["deactivated_free_parameters"].clone(),
        "parametric_source_retained": true});
    if let Some(o) = status.as_object_mut() {
        o.insert("seed_bake".into(), receipt);
    }
    Ok(status)
}

fn prepare_current_bake(manager: &ModelManager, request: &Value) -> AResult<Value> {
    let Some(obj) = request.as_object() else {
        return Err(serr("the bake-current request body must be an object"));
    };
    let unknown = sorted_unknown(obj, &["expected_content_id", "bake"]);
    if !unknown.is_empty() {
        return Err(serr(format!(
            "unknown bake-current field(s) {}; allowed fields are expected_content_id and bake",
            list_of(&unknown)
        )));
    }
    if !obj.contains_key("expected_content_id") {
        return Err(serr("expected_content_id is required for bake-current"));
    }
    let expected = match obj.get("expected_content_id") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(serr(
                "expected_content_id must be the non-empty content_id of the live parametric model",
            ));
        }
    };
    let cfg = normalise_bake_config(obj.get("bake"))?;
    let source_model = manager.require()?;
    if source_model.root().is_none() {
        return Err(serr("bake-current requires a model with a root node"));
    }
    let current = source_model.content_id().unwrap_or_else(|| "None".into());
    if expected != current {
        return Err(serr(format!(
            "bake-current content guard failed: expected {}, current content_id is {}; fetch the current model and deliberately retry",
            repr(&json!(expected)),
            repr(&json!(current))
        )));
    }
    let source_doc = source_model.to_doc()?;
    let canonical_len = D::canonical_bytes(&source_doc).len();
    let mut source_identity = identity(&source_model)?;
    if let Some(o) = source_identity.as_object_mut() {
        o.insert("canonical_bytes".into(), json!(canonical_len));
    }
    let mut free: Vec<String> = source_doc
        .get("parameters")
        .and_then(Value::as_object)
        .map(|p| {
            p.iter().filter(|(_, v)| v.get("free").is_some_and(truthy)).map(|(k, _)| k.clone()).collect()
        })
        .unwrap_or_default();
    free.sort();
    let prior_implexity = source_doc.pointer("/meta/implexity").cloned();
    if prior_implexity
        .as_ref()
        .and_then(|p| p.as_object())
        .is_some_and(|p| p.get("topology").is_some_and(|t| !t.is_null()))
    {
        return Err(serr(
            "bake-current is a one-way handoff and the live model already declares an authoritative topology field; edit that shared field instead of baking it again",
        ));
    }
    let prior_seed = prior_implexity
        .as_ref()
        .and_then(Value::as_object)
        .map(|p| p.get("seed").cloned().unwrap_or(Value::Null));
    let parameters: Map<String, Value> =
        source_model.parameter_table().iter().map(|r| (py_str(&r["name"]), r["value"].clone())).collect();
    let bake_meta = json!({
        "schema": PROVENANCE_SCHEMA, "seed_id": "current-model", "version": "1",
        "addin_id": "implexity.core.geometry-seeds", "mode": "bake_current",
        "parameters": parameters, "free": free, "source_geometry": source_identity,
        "source_document": {"canonical_sha256": D::sha256_of(&source_doc), "canonical_bytes": canonical_len,
            "retained_as_named_output": true, "root": source_doc.get("root").cloned().unwrap_or(Value::Null),
            "outputs": source_doc.get("outputs").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}))},
    });
    let (mut document, bake) = bake_editable(&source_model, &source_doc, &bake_meta, &cfg)?;
    if let Some(imp) = document.pointer_mut("/meta/implexity").and_then(Value::as_object_mut) {
        let seed = imp.shift_remove("seed").unwrap_or(Value::Null);
        imp.insert("seed_bake".into(), seed);
        if let Some(prior) = prior_seed.filter(|p| !p.is_null()) {
            imp.insert("seed".into(), prior);
        }
    }
    let baked = D::build(&document, Some(manager.dir()), None).map_err(|e| match e {
        implexity_geometry::GeometryError::ModelDoc(p) => {
            seed_error(p.iter().map(|x| format!("bake-current output is invalid: {x}")).collect())
        }
        other => AuthoringError::Geometry(other),
    })?;
    Ok(json!({"kind": "implicit_geometry_seed_bake_preview", "schema": SCHEMA,
        "expected_content_id": expected, "document": baked.to_doc()?,
        "structure_id": baked.structure_id(), "content_id": baked.content_id(), "sha256": baked.sha256()?,
        "source_geometry": source_identity, "bake": bake, "deactivated_free_parameters": free,
        "parametric_source_retained": true, "warnings": baked.warnings}))
}

fn np(key: &str, label: &str, default: f64, low: f64, high: f64, step: f64, help: &str) -> SeedParameter {
    np_full(key, label, default, low, high, step, help, "mm", true)
}

#[allow(clippy::too_many_arguments)]
fn np_full(
    key: &str,
    label: &str,
    default: f64,
    low: f64,
    high: f64,
    step: f64,
    help: &str,
    units: &str,
    optimisable: bool,
) -> SeedParameter {
    SeedParameter {
        key: key.into(),
        label: label.into(),
        ty: "number".into(),
        units: units.into(),
        default: jf(default),
        min: jf(low),
        max: jf(high),
        step: jf(step),
        help: help.into(),
        optimisable,
        default_free: false,
    }
}

fn ip(key: &str, label: &str, default: i64, low: i64, high: i64, help: &str) -> SeedParameter {
    SeedParameter {
        key: key.into(),
        label: label.into(),
        ty: "integer".into(),
        units: "-".into(),
        default: json!(default),
        min: json!(low),
        max: json!(high),
        step: json!(1),
        help: help.into(),
        optimisable: false,
        default_free: false,
    }
}

fn def(
    id: &str,
    label: &str,
    description: &str,
    category: &str,
    parameters: Vec<SeedParameter>,
    builder: SeedBuilder,
    creation_only: bool,
) -> SeedDefinition {
    SeedDefinition {
        id: id.into(),
        version: "1.0.0".into(),
        label: label.into(),
        description: description.into(),
        category: category.into(),
        addin_id: "implexity.core.geometry-seeds".into(),
        parameters,
        builder,
        modes: if creation_only { vec!["parametric".into()] } else { MODES.map(String::from).to_vec() },
        creation_only,
    }
}

fn example_doc(key: &str) -> AResult<Value> {
    let ex = implexity_geometry::examples::by_key(key).ok_or_else(|| {
        AuthoringError::runtime("RuntimeError", format!("the kernel example {key} is not registered"))
    })?;
    Ok(ex.document()?)
}

fn primitive_box() -> Value {
    json!({
        "schema": D::SCHEMA, "name": "parametric rounded box", "units": "mm",
        "doc": "a dimension-driven rounded box seed",
        "parameters": {
            "width_mm": {"value": 20.0, "units": "mm", "min": 0.5},
            "depth_mm": {"value": 12.0, "units": "mm", "min": 0.5},
            "height_mm": {"value": 8.0, "units": "mm", "min": 0.5},
            "corner_radius_mm": {"value": 1.0, "units": "mm", "min": 0.0}},
        "nodes": {"body": {"kind": "rounded_box",
            "params": {"bx_mm": {"expr": "width_mm / 2"}, "by_mm": {"expr": "depth_mm / 2"},
                "bz_mm": {"expr": "height_mm / 2"}, "radius_mm": {"bind": "corner_radius_mm"}},
            "children": []}},
        "root": "body", "outputs": {"solid": "body"},
    })
}

fn primitive_cylinder() -> Value {
    json!({
        "schema": D::SCHEMA, "name": "parametric cylinder", "units": "mm",
        "doc": "a diameter-and-height driven capped cylinder seed",
        "parameters": {
            "diameter_mm": {"value": 12.0, "units": "mm", "min": 0.5},
            "height_mm": {"value": 20.0, "units": "mm", "min": 0.5}},
        "nodes": {"body": {"kind": "cylinder",
            "params": {"radius_mm": {"expr": "diameter_mm / 2"}, "half_height_mm": {"expr": "height_mm / 2"}},
            "children": []}},
        "root": "body", "outputs": {"solid": "body"},
    })
}

fn construct_node(kind: &str, attrs: &Value) -> AResult<NodeRef> {
    Ok(Arc::new(crate::geometry_sculpt::construct(kind, attrs, BTreeMap::new())?))
}

fn from_graph(node: &NodeRef, name: &str, meta: Value) -> AResult<Value> {
    let outputs: Vec<(String, OutputRef)> = Vec::new();
    let (doc, _sidecars) =
        D::from_graph(node, name, None, &outputs, &BTreeMap::new(), None, Some(meta), None)?;
    Ok(doc)
}

fn f(values: &Map<String, Value>, k: &str) -> f64 {
    num(&values[k])
}

fn i(values: &Map<String, Value>, k: &str) -> i64 {
    values[k].as_i64().unwrap_or(0)
}

fn controlled_volume(values: &Map<String, Value>) -> AResult<Value> {
    let dims = [f(values, "width_mm"), f(values, "depth_mm"), f(values, "height_mm")];
    let (n, c) = (i(values, "analysis_cells"), i(values, "control_nodes"));
    let node = construct_node(
        "lattice.controlled",
        &json!({"domain_mm": f3(dims), "analysis_grid": [n, n, n], "control_grid": [c, c, c],
            "period_mm": jf(f(values, "period_mm")), "interface_mm": jf(f(values, "interface_mm"))}),
    )?;
    let meta = json!({"implexity": {
        "topology": {"coordinate": "model:control", "ref": "model:control", "lower": -8.0, "upper": 8.0,
            "control_schema": CONTROL_SCHEMA, "representation": "spatial_implicit_controls"},
        "spatial_controls": control_contract(),
        "geometry_factory": {"creation_only": true, "settings": values,
            "note": "Volume settings are structure, not disconnected scalar controls. Re-author and preflight after structural edits."}}});
    from_graph(&node, "Spatially controlled design volume", meta)
}

fn controlled_assembly(values: &Map<String, Value>) -> AResult<Value> {
    let dims = [f(values, "width_mm"), f(values, "depth_mm"), f(values, "height_mm")];
    let (count, nodes, samples) =
        (i(values, "volume_count"), i(values, "control_nodes"), i(values, "geometry_cells"));
    #[allow(clippy::cast_precision_loss)]
    let local = [dims[0] / count as f64, dims[1], dims[2]];
    let lattice = construct_node(
        "lattice.controlled",
        &json!({"domain_mm": f3(local), "analysis_grid": [samples, samples, samples],
            "geometry_grid": [samples, samples, samples], "control_grid": [nodes, nodes, nodes],
            "period_mm": jf(f(values, "period_mm")), "interface_mm": jf(f(values, "interface_mm"))}),
    )?;
    let spec = lattice.op().as_any().downcast_ref::<ControlledLattice>().map(|l| l.spec.clone()).ok_or_else(
        || AuthoringError::runtime("RuntimeError", "lattice.controlled did not build a ControlledLattice"),
    )?;
    let geometry: Map<String, Value> = spec.doc_attrs().into_iter().map(|(k, a)| (k, a.to_json())).collect();
    let mut volumes = Vec::new();
    let mut composition: Option<Value> = None;
    for idx in 0..count {
        let mut matrix =
            [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        #[allow(clippy::cast_precision_loss)]
        {
            matrix[0][3] = idx as f64 * local[0];
        }
        let name = format!("volume_{:02}", idx + 1);
        volumes.push(json!({"id": name, "geometry": geometry, "local_to_model": matrix.map(|r| r.map(jf))}));
        composition = Some(match composition {
            None => json!(name),
            Some(prev) => json!({"operation": "union", "left": prev, "right": name}),
        });
    }
    let ac = i(values, "analysis_cells");
    let node = construct_node(
        "lattice.controlled_assembly",
        &json!({"domain_mm": f3(dims), "analysis_grid": [ac, ac, ac], "control_grid": [nodes, nodes, nodes],
            "interface_mm": jf(f(values, "interface_mm")), "volumes": volumes, "composition": composition}),
    )?;
    let layout = node
        .op()
        .as_any()
        .downcast_ref::<ControlledAssembly>()
        .ok_or_else(|| {
            AuthoringError::runtime(
                "RuntimeError",
                "lattice.controlled_assembly did not build a ControlledAssembly",
            )
        })?
        .control_layout()?;
    let meta = json!({"implexity": {
        "topology": {"coordinate": "model:control", "ref": "model:control", "lower": -8.0, "upper": 8.0,
            "control_schema": CONTROL_SCHEMA, "representation": "spatial_implicit_controls"},
        "spatial_controls": layout,
        "geometry_factory": {"creation_only": true, "settings": values,
            "note": "Named local frames and Boolean structure are authored. Structural changes require fresh preflight."}}});
    from_graph(&node, "Controlled-volume assembly", meta)
}

#[allow(clippy::too_many_lines)]
fn core_seeds() -> Vec<SeedDefinition> {
    vec![
        def(
            "primitive.box",
            "Rounded box",
            "A dimension-driven solid box with a parametric edge radius.",
            "Primitives",
            vec![
                np("width_mm", "Width", 20.0, 0.5, 10000.0, 0.5, "Overall size along X."),
                np("depth_mm", "Depth", 12.0, 0.5, 10000.0, 0.5, "Overall size along Y."),
                np("height_mm", "Height", 8.0, 0.5, 10000.0, 0.5, "Overall size along Z."),
                np(
                    "corner_radius_mm",
                    "Corner radius",
                    1.0,
                    0.0,
                    1000.0,
                    0.25,
                    "Edge and corner fillet radius.",
                ),
            ],
            Arc::new(|_| Ok(primitive_box())),
            false,
        ),
        def(
            "primitive.cylinder",
            "Cylinder",
            "A capped cylinder controlled by overall diameter and height.",
            "Primitives",
            vec![
                np("diameter_mm", "Diameter", 12.0, 0.5, 10000.0, 0.5, "Overall cylinder diameter."),
                np("height_mm", "Height", 20.0, 0.5, 10000.0, 0.5, "Overall cylinder height."),
            ],
            Arc::new(|_| Ok(primitive_cylinder())),
            false,
        ),
        def(
            "example.bracket",
            "Shelled bracket",
            "A plate, boss, bore, fillet and shell with retained feature relations.",
            "Mechanical",
            vec![
                np("plate_x", "Plate length", 34.0, 8.0, 200.0, 0.5, "Overall plate length."),
                np("plate_y", "Plate width", 20.0, 8.0, 200.0, 0.5, "Overall plate width."),
                np("plate_z", "Plate thickness", 6.0, 2.0, 50.0, 0.25, "Overall plate thickness."),
                np("corner_r", "Plate corner radius", 2.0, 0.0, 20.0, 0.25, "Plate edge and corner radius."),
                np("boss_d", "Boss diameter", 12.0, 4.0, 80.0, 0.5, "Outside diameter of the raised boss."),
                np("boss_h", "Boss height", 9.0, 2.0, 80.0, 0.5, "Height of the raised boss."),
                np("bore_d", "Bore diameter", 5.2, 3.0, 9.0, 0.1, "Clearance-hole diameter."),
                np("wall", "Wall thickness", 1.6, 0.8, 4.0, 0.1, "Shell wall thickness."),
            ],
            Arc::new(|_| example_doc("bracket")),
            false,
        ),
        def(
            "example.lattice-block",
            "Lattice block",
            "A rounded skin surrounding a bounded gyroid sheet infill.",
            "Lattices",
            vec![
                np("bx", "Length", 24.0, 4.0, 200.0, 0.5, "Overall size along X."),
                np("by", "Width", 24.0, 4.0, 200.0, 0.5, "Overall size along Y."),
                np("bz", "Height", 14.0, 4.0, 200.0, 0.5, "Overall size along Z."),
                np("corner_r", "Corner radius", 2.0, 0.0, 20.0, 0.25, "Outer edge radius."),
                np("skin_t", "Skin thickness", 1.2, 0.4, 4.0, 0.1, "Solid skin thickness."),
                np("period", "Gyroid period", 5.0, 2.0, 12.0, 0.25, "Gyroid unit-cell period."),
                np_full(
                    "wall_frac",
                    "Relative wall",
                    0.30,
                    0.05,
                    0.60,
                    0.01,
                    "Sheet wall as a fraction of its period.",
                    "-",
                    true,
                ),
            ],
            Arc::new(|_| example_doc("lattice")),
            false,
        ),
        def(
            "lattice.controlled-volume",
            "Spatial design volume",
            "TPMS-first implicit volume with twenty application-independent spatial control fields and complete discrete geometry derivatives.",
            "Lattices",
            vec![
                np_full(
                    "width_mm",
                    "Volume width",
                    12.0,
                    0.1,
                    10000.0,
                    0.5,
                    "Creation-only X extent.",
                    "mm",
                    false,
                ),
                np_full(
                    "depth_mm",
                    "Volume depth",
                    12.0,
                    0.1,
                    10000.0,
                    0.5,
                    "Creation-only Y extent.",
                    "mm",
                    false,
                ),
                np_full(
                    "height_mm",
                    "Volume height",
                    12.0,
                    0.1,
                    10000.0,
                    0.5,
                    "Creation-only Z extent.",
                    "mm",
                    false,
                ),
                ip(
                    "analysis_cells",
                    "Analysis cells per axis",
                    12,
                    2,
                    128,
                    "Declared geometry sampling resolution; not a validation guarantee.",
                ),
                ip(
                    "control_nodes",
                    "Control nodes per axis",
                    3,
                    2,
                    32,
                    "Twenty spatial fields, not twenty global numbers.",
                ),
                np_full(
                    "period_mm",
                    "Base repeat distance",
                    4.0,
                    0.01,
                    10000.0,
                    0.1,
                    "Initial TPMS period.",
                    "mm",
                    false,
                ),
                np_full(
                    "interface_mm",
                    "Interface smoothing width",
                    0.5,
                    0.001,
                    1000.0,
                    0.05,
                    "At least half the smallest analysis cell; refine instead of silently changing this width.",
                    "mm",
                    false,
                ),
            ],
            Arc::new(controlled_volume),
            true,
        ),
        def(
            "lattice.controlled-assembly",
            "Multiple spatial design volumes",
            "Named affine TPMS volumes with twenty independent fields each, in one authoritative control tensor.",
            "Lattices",
            vec![
                np_full(
                    "width_mm",
                    "Assembly width",
                    24.0,
                    0.1,
                    10000.0,
                    0.5,
                    "Creation-only X envelope.",
                    "mm",
                    false,
                ),
                np_full(
                    "depth_mm",
                    "Assembly depth",
                    12.0,
                    0.1,
                    10000.0,
                    0.5,
                    "Creation-only Y envelope.",
                    "mm",
                    false,
                ),
                np_full(
                    "height_mm",
                    "Assembly height",
                    12.0,
                    0.1,
                    10000.0,
                    0.5,
                    "Creation-only Z envelope.",
                    "mm",
                    false,
                ),
                ip("volume_count", "Volumes along X", 2, 1, 8, "Independent contiguous control blocks."),
                ip(
                    "analysis_cells",
                    "Analysis cells per axis",
                    12,
                    2,
                    128,
                    "Sampling grid; independent from volume geometry grids.",
                ),
                ip(
                    "geometry_cells",
                    "Geometry cells per local axis",
                    12,
                    2,
                    128,
                    "Explicit geometry sampling resolution per volume.",
                ),
                ip(
                    "control_nodes",
                    "Control nodes per local axis",
                    3,
                    2,
                    16,
                    "Twenty local fields per volume.",
                ),
                np_full(
                    "period_mm",
                    "Base repeat distance",
                    4.0,
                    0.01,
                    10000.0,
                    0.1,
                    "Initial local TPMS period.",
                    "mm",
                    false,
                ),
                np_full(
                    "interface_mm",
                    "Interface smoothing width",
                    0.5,
                    0.001,
                    1000.0,
                    0.05,
                    "Must meet each volume geometry-grid requirement.",
                    "mm",
                    false,
                ),
            ],
            Arc::new(controlled_assembly),
            true,
        ),
    ]
}
