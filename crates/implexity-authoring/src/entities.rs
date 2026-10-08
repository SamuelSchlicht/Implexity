// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_core::backends::{self, PhysicsBackend};
use implexity_core::contributions::ContributionRegistry;

use crate::error::{AResult, AuthoringError};
use crate::py::{canonical_ascii, get, py_str, repr, sha256_hex, truthy};
use crate::spatial_selection::normalize_spatial_selection;
use crate::surface_regions::{normalize_surface_patch, normalize_surface_patch_set};

pub const PROBLEM_SCHEMA: &str = "implexity-differentiable-problem/2";

#[must_use]
pub fn unset_materials() -> Value {
    json!({"catalog": {}, "mixture": null})
}

const REGION_TYPES: [&str; 13] = [
    "face",
    "slab",
    "box",
    "everywhere",
    "nowhere",
    "and",
    "or",
    "not",
    "face_group",
    "ref",
    "surface_patch",
    "surface_patch_set",
    "cell_selection",
];


pub trait ProblemAdapter: Send + Sync {
    fn load_kinds(&self) -> Vec<String>;
    fn boundary_condition_kinds(&self) -> Vec<String>;
    fn analysis_overrides(&self) -> Vec<String>;
    fn analysis_types(&self) -> Vec<String>;

    fn normalise_materials(&self, value: &Value) -> AResult<Value>;
    fn material_capabilities(&self) -> Value;

    fn compile_problem(&self, normal: &Value, base_case: &Value) -> AResult<Value>;
    fn capabilities(&self) -> Value;
    fn case_bound_capabilities(&self) -> Value;
}

pub struct ProblemAdapterHandle(pub Arc<dyn ProblemAdapter>);

#[must_use]
pub fn adapter_of(backend: &dyn PhysicsBackend) -> Option<Arc<dyn ProblemAdapter>> {
    let any = backend.problem()?;
    any.downcast_ref::<ProblemAdapterHandle>().map(|h| Arc::clone(&h.0))
}

#[must_use]
pub fn contributions() -> &'static ContributionRegistry {
    &implexity_core::registries::global().contributions
}

pub(crate) fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("EntitySpecError", message)
}

fn canonical_id(value: &Value, prefix: &str) -> String {
    format!("{prefix}-{}", &sha256_hex(canonical_ascii(value).as_bytes())[..24])
}

fn strict_keys(value: &Map<String, Value>, allowed: &[&str], path: &str) -> AResult<()> {
    let mut extra: Vec<&String> = value.keys().filter(|k| !allowed.contains(&k.as_str())).collect();
    if extra.is_empty() {
        return Ok(());
    }
    extra.sort();
    Err(err(format!(
        "{path} has unsupported keys: {}",
        extra.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
    )))
}

fn ident(value: &Value, path: &str) -> AResult<String> {
    let out = py_str(value).trim().to_string();
    let b = out.as_bytes();
    let ok = !b.is_empty()
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'));
    if !ok {
        return Err(err(format!(
            "{path} must start with a letter and contain only letters, numbers, '.', '_' or '-'"
        )));
    }
    Ok(out)
}


pub fn normalise_named_collection(value: Option<&Value>, path: &str) -> AResult<Vec<Map<String, Value>>> {
    let seq: Vec<Value> = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(m)) => {
            let mut seq = Vec::new();
            for (key, item) in m {
                let Some(obj) = item.as_object() else {
                    return Err(err(format!("{path}.{key} must be an object")));
                };
                let mut obj = obj.clone();
                obj.entry("id").or_insert_with(|| Value::from(key.clone()));
                seq.push(Value::Object(obj));
            }
            seq
        }
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(err(format!("{path} must be a list or object"))),
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let Some(obj) = item.as_object() else { return Err(err(format!("{path}[{i}] must be an object"))) };
        let entity_id = ident(obj.get("id").unwrap_or(&Value::from("")), &format!("{path}[{i}].id"))?;
        if !seen.insert(entity_id.clone()) {
            return Err(err(format!("{path} contains duplicate id {}", repr(&Value::from(entity_id)))));
        }
        let mut obj = obj.clone();
        obj.insert("id".into(), Value::from(entity_id));
        out.push(obj);
    }
    Ok(out)
}


pub fn normalise_region_selector(value: &Value, path: &str) -> AResult<Value> {
    let Some(obj) = value.as_object() else { return Err(err(format!("{path} must be an object"))) };
    let mut raw = obj.clone();
    let declared_kind =
        raw.get("type").or_else(|| raw.get("kind")).map_or_else(String::new, py_str).trim().to_string();
    let rawv = Value::Object(raw.clone());
    if declared_kind == "surface_patch" {
        return normalize_surface_patch(&rawv, None, false);
    }
    if declared_kind == "surface_patch_set" {
        return normalize_surface_patch_set(&rawv);
    }
    if declared_kind == "cell_selection"
        || raw.get("schema").and_then(Value::as_str) == Some("implexity-spatial-selection/1")
    {
        return normalize_spatial_selection(&rawv);
    }
    if !raw.contains_key("type") {
        if raw.len() != 1 {
            return Err(err(format!("{path} must contain 'type' or exactly one case region kind")));
        }
        let (legacy_kind, payload) =
            raw.iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap_or_default();
        if !REGION_TYPES.contains(&legacy_kind.as_str()) || legacy_kind == "ref" {
            return Err(err(format!("{path} has unknown region kind {}", repr(&Value::from(legacy_kind)))));
        }
        raw = match legacy_kind.as_str() {
            "everywhere" | "nowhere" => {
                if payload != Value::Bool(true) {
                    return Err(err(format!("{path}.{legacy_kind} must be true")));
                }
                json!({"type": legacy_kind}).as_object().cloned().unwrap_or_default()
            }
            "and" | "or" => {
                json!({"type": legacy_kind, "regions": payload}).as_object().cloned().unwrap_or_default()
            }
            "not" => json!({"type": "not", "region": payload}).as_object().cloned().unwrap_or_default(),
            _ => {
                let Some(p) = payload.as_object() else {
                    return Err(err(format!("{path}.{legacy_kind} must be an object")));
                };
                let mut m = Map::new();
                m.insert("type".into(), Value::from(legacy_kind));
                for (k, v) in p {
                    m.insert(k.clone(), v.clone());
                }
                m
            }
        };
    }
    let typ = raw.get("type").map_or_else(String::new, py_str).trim().to_string();
    if !REGION_TYPES.contains(&typ.as_str()) {
        let mut names = REGION_TYPES.to_vec();
        names.sort_unstable();
        return Err(err(format!("{path}.type must be one of {}", names.join(", "))));
    }
    let rawv = Value::Object(raw.clone());
    match typ.as_str() {
        "surface_patch" => return normalize_surface_patch(&rawv, None, false),
        "surface_patch_set" => return normalize_surface_patch_set(&rawv),
        "cell_selection" => return normalize_spatial_selection(&rawv),
        _ => {}
    }
    let finite_number = |name: &str, required: bool| -> AResult<Option<f64>> {
        let v = raw.get(name);
        if v.is_none_or(Value::is_null) && !required {
            return Ok(None);
        }
        match v {
            Some(Value::Number(n)) if n.as_f64().is_some_and(f64::is_finite) => Ok(n.as_f64()),
            _ => Err(err(format!("{path}.{name} must be a finite number"))),
        }
    };
    let mut out = Map::new();
    out.insert("type".into(), Value::from(typ.clone()));
    match typ.as_str() {
        "ref" => {
            strict_keys(&raw, &["type", "id"], path)?;
            out.insert(
                "id".into(),
                Value::from(ident(raw.get("id").unwrap_or(&Value::from("")), &format!("{path}.id"))?),
            );
        }
        "face" => {
            strict_keys(&raw, &["type", "axis", "side"], path)?;
            let axis = raw.get("axis").map_or_else(String::new, py_str).to_lowercase();
            let side = raw.get("side").map_or_else(String::new, py_str).to_lowercase();
            let side = match side.as_str() {
                "low" | "min" | "-" => "lo".to_string(),
                "high" | "max" | "+" => "hi".to_string(),
                _ => side,
            };
            if !["x", "y", "z"].contains(&axis.as_str()) {
                return Err(err(format!("{path}.axis must be x, y or z")));
            }
            if side != "lo" && side != "hi" {
                return Err(err(format!("{path}.side must be lo or hi")));
            }
            out.insert("axis".into(), Value::from(axis));
            out.insert("side".into(), Value::from(side));
        }
        "slab" => {
            strict_keys(&raw, &["type", "axis", "lo_mm", "hi_mm"], path)?;
            let axis = raw.get("axis").map_or_else(String::new, py_str).to_lowercase();
            if !["x", "y", "z"].contains(&axis.as_str()) {
                return Err(err(format!("{path}.axis must be x, y or z")));
            }
            out.insert("axis".into(), Value::from(axis));
            let lo = finite_number("lo_mm", false)?;
            let hi = finite_number("hi_mm", false)?;
            if let Some(l) = lo {
                out.insert("lo_mm".into(), crate::py::jf(l));
            }
            if let Some(h) = hi {
                out.insert("hi_mm".into(), crate::py::jf(h));
            }
            if let (Some(l), Some(h)) = (lo, hi)
                && l > h
            {
                return Err(err(format!("{path}.lo_mm must not exceed hi_mm")));
            }
        }
        "box" => {
            strict_keys(&raw, &["type", "lo_mm", "hi_mm"], path)?;
            let (lo, hi) = (raw.get("lo_mm"), raw.get("hi_mm"));
            let three = |v: Option<&Value>| v.and_then(Value::as_array).filter(|a| a.len() == 3).cloned();
            let (Some(lo), Some(hi)) = (three(lo), three(hi)) else {
                return Err(err(format!("{path} needs lo_mm and hi_mm as three-component vectors")));
            };
            let conv = |a: &[Value]| -> AResult<Vec<f64>> {
                a.iter()
                    .map(|v| {
                        crate::py::py_float(v).map_err(|_| err(format!("{path} bounds must be numeric")))
                    })
                    .collect()
            };
            let lov = conv(&lo)?;
            let hiv = conv(&hi)?;
            if !lov.iter().chain(&hiv).all(|v| v.is_finite()) {
                return Err(err(format!("{path} bounds must be finite")));
            }
            if lov.iter().zip(&hiv).any(|(a, b)| a > b) {
                return Err(err(format!("{path}.lo_mm must not exceed hi_mm")));
            }
            out.insert("lo_mm".into(), crate::py::jfs(&lov));
            out.insert("hi_mm".into(), crate::py::jfs(&hiv));
        }
        "everywhere" | "nowhere" => strict_keys(&raw, &["type"], path)?,
        "face_group" => {
            strict_keys(&raw, &["type", "id"], path)?;
            match raw.get("id") {
                Some(Value::Number(n)) if n.is_i64() || n.is_u64() => {
                    let g = n.as_i64().unwrap_or(-1);
                    if g < 0 {
                        return Err(err(format!("{path}.id must be an integer >= 0")));
                    }
                    out.insert("id".into(), Value::from(g));
                }
                _ => return Err(err(format!("{path}.id must be an integer >= 0"))),
            }
        }
        "and" | "or" => {
            strict_keys(&raw, &["type", "regions", "children"], path)?;
            let children = raw.get("regions").or_else(|| raw.get("children"));
            let Some(children) = children.and_then(Value::as_array).filter(|c| c.len() >= 2) else {
                return Err(err(format!("{path} requires at least two child regions")));
            };
            let norm: Vec<Value> = children
                .iter()
                .enumerate()
                .map(|(i, c)| normalise_region_selector(c, &format!("{path}.regions[{i}]")))
                .collect::<AResult<_>>()?;
            out.insert("regions".into(), Value::Array(norm));
        }
        _ => {
            strict_keys(&raw, &["type", "region", "child"], path)?;
            let child = raw.get("region").or_else(|| raw.get("child")).filter(|c| !c.is_null());
            let Some(child) = child else { return Err(err(format!("{path}.region is required"))) };
            out.insert("region".into(), normalise_region_selector(child, &format!("{path}.region"))?);
        }
    }
    Ok(Value::Object(out))
}


pub fn normalise_regions(value: Option<&Value>) -> AResult<Vec<Value>> {
    let mut out = Vec::new();
    for (i, item) in normalise_named_collection(value, "regions")?.iter().enumerate() {
        strict_keys(
            item,
            &[
                "id",
                "name",
                "selector",
                "description",
                "kind",
                "type",
                "selection",
                "surface_patch",
                "surface_patch_set",
                "enabled",
            ],
            &format!("regions[{i}]"),
        )?;
        let selector = if item.contains_key("selector") {
            item.get("selector")
        } else if item.contains_key("selection") {
            item.get("selection")
        } else if item.contains_key("surface_patch") {
            item.get("surface_patch")
        } else {
            item.get("surface_patch_set")
        };
        let Some(selector) = selector.filter(|s| !s.is_null()) else {
            return Err(err(format!("regions[{i}].selector is required")));
        };
        let id = item["id"].clone();
        out.push(json!({
            "id": id,
            "name": item.get("name").filter(|v| truthy(v)).map_or_else(|| py_str(&id), py_str),
            "description": item.get("description").filter(|v| truthy(v)).map_or_else(String::new, py_str),
            "selector": normalise_region_selector(selector, &format!("regions[{i}].selector"))?,
        }));
    }
    Ok(out)
}

fn normalise_action(item: &Map<String, Value>, path: &str, allowed: &BTreeSet<String>) -> AResult<Value> {
    let mut item = item.clone();
    let entity_id = ident(&item.shift_remove("id").unwrap_or(Value::from("")), &format!("{path}.id"))?;
    let name = item.shift_remove("name").map_or_else(|| entity_id.clone(), |v| py_str(&v));
    let enabled = item.shift_remove("enabled").is_none_or(|v| truthy(&v));
    let kind = item.get("kind").map_or_else(String::new, py_str).trim().to_string();
    if !allowed.contains(&kind) {
        return Err(err(format!(
            "{path}.kind must be one of {}",
            allowed.iter().cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    match item.get("region").cloned() {
        Some(Value::String(s)) => {
            item.insert(
                "region".into(),
                json!({"type": "ref", "id": ident(&Value::from(s), &format!("{path}.region"))?}),
            );
        }
        Some(Value::Null) | None => return Err(err(format!("{path}.region is required"))),
        Some(region) => {
            item.insert("region".into(), normalise_region_selector(&region, &format!("{path}.region"))?);
        }
    }
    Ok(json!({"id": entity_id, "name": name, "enabled": enabled, "spec": Value::Object(item)}))
}


pub fn normalise_actions(
    value: Option<&Value>,
    path: &str,
    allowed: &BTreeSet<String>,
) -> AResult<Vec<Value>> {
    normalise_named_collection(value, path)?
        .iter()
        .enumerate()
        .map(|(i, item)| normalise_action(item, &format!("{path}[{i}]"), allowed))
        .collect()
}


pub fn problem_adapter(backend: Option<&str>) -> AResult<(String, Arc<dyn ProblemAdapter>)> {
    let selected = backends::physics(contributions(), backend).map_err(|e| err(e.to_string()))?;
    let Some(adapter) = adapter_of(selected.as_ref()) else {
        return Err(err(format!(
            "physics backend {} declares no typed engineering-problem adapter; author its provider problem instead",
            repr(&Value::from(selected.name()))
        )));
    };
    Ok((selected.name().to_string(), adapter))
}

fn denormalise_actions(items: Option<&Value>) -> AResult<Value> {
    let items = match items {
        Some(v) if truthy(v) => v,
        _ => return Ok(Value::Array(Vec::new())),
    };
    let list: Vec<Value> = match items {
        Value::Array(a) => a.clone(),

        Value::Object(m) => m.keys().map(|k| Value::from(k.clone())).collect(),
        Value::String(s) => s.chars().map(|c| Value::from(c.to_string())).collect(),
        other => {
            return Err(crate::py::type_error(format!(
                "'{}' object is not iterable",
                crate::py::type_name(other)
            )));
        }
    };
    Ok(Value::Array(
        list.iter()
            .map(|item| match item.get("spec") {
                Some(Value::Object(spec)) => {
                    let mut merged = spec.clone();
                    merged.insert("id".into(), item.get("id").cloned().unwrap_or(Value::Null));
                    merged.insert("name".into(), item.get("name").cloned().unwrap_or(Value::Null));
                    merged
                        .insert("enabled".into(), item.get("enabled").cloned().unwrap_or(Value::Bool(true)));
                    Value::Object(merged)
                }
                _ => item.clone(),
            })
            .collect(),
    ))
}


pub fn normalise_problem(value: &Value, model: Option<&Value>, case: Option<&Value>) -> AResult<Value> {
    let Some(raw) = value.as_object() else { return Err(err("engineering problem must be an object")) };
    let mut raw = raw.clone();
    strict_keys(
        &raw,
        &[
            "schema",
            "name",
            "description",
            "backend",
            "model",
            "regions",
            "materials",
            "loads",
            "boundary_conditions",
            "analysis",
            "base_case",
            "inherit_case_loads",
            "problem_id",
            "compiled_case",
            "compiled_case_id",
            "entity_counts",
        ],
        "problem",
    )?;
    let schema = raw.get("schema").cloned().unwrap_or_else(|| Value::from(PROBLEM_SCHEMA));
    raw.insert("schema".into(), schema.clone());
    if schema.as_str() != Some(PROBLEM_SCHEMA) {
        return Err(err(format!(
            "problem.schema must be {}, got {}",
            repr(&Value::from(PROBLEM_SCHEMA)),
            repr(&schema)
        )));
    }
    let backend_req = raw.get("backend").filter(|v| truthy(v)).map(py_str);
    let (backend, adapter) = problem_adapter(backend_req.as_deref())?;
    let load_kinds: BTreeSet<String> = adapter.load_kinds().into_iter().collect();
    let bc_kinds: BTreeSet<String> = adapter.boundary_condition_kinds().into_iter().collect();
    let regions = normalise_regions(raw.get("regions"))?;
    let loads = normalise_actions(Some(&denormalise_actions(raw.get("loads"))?), "loads", &load_kinds)?;
    let bcs = normalise_actions(
        Some(&denormalise_actions(raw.get("boundary_conditions"))?),
        "boundary_conditions",
        &bc_kinds,
    )?;
    let analysis = raw.get("analysis").cloned().unwrap_or_else(|| json!({}));
    let Some(analysis) = analysis.as_object().cloned() else {
        return Err(err("problem.analysis must be an object"));
    };
    strict_keys(&analysis, &["overrides", "load_mode", "description"], "problem.analysis")?;
    let inherit = raw.get("inherit_case_loads").is_none_or(truthy);
    let load_mode = analysis
        .get("load_mode")
        .map_or_else(|| if inherit { "append".to_string() } else { "replace".to_string() }, py_str);
    if load_mode != "append" && load_mode != "replace" {
        return Err(err("problem.analysis.load_mode must be append or replace"));
    }
    let overrides = analysis.get("overrides").cloned().unwrap_or_else(|| json!({}));
    let Some(overrides) = overrides.as_object().cloned() else {
        return Err(err("problem.analysis.overrides must be an object"));
    };
    let allowed: Vec<String> = adapter.analysis_overrides();
    let mut unknown: Vec<&String> = overrides.keys().filter(|k| !allowed.contains(k)).collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(err(format!(
            "problem.analysis.overrides has unsupported sections: {}",
            unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }
    let model_source = raw.get("model").filter(|v| truthy(v)).or(model.filter(|v| truthy(v)));
    let mut model_identity = Map::new();
    if let Some(Value::Object(ms)) = model_source {
        for key in ["structure_id", "content_id", "model_id"] {
            if let Some(v) = ms.get(key).filter(|v| !v.is_null()) {
                model_identity.insert(key.into(), v.clone());
            }
        }
    }
    let base_case = raw.get("base_case").filter(|v| truthy(v)).or(case).cloned();
    let materials = match raw.get("materials").filter(|v| !v.is_null()) {
        None => unset_materials(),
        Some(v) => adapter.normalise_materials(v)?,
    };
    let mut out = Map::new();
    out.insert("schema".into(), Value::from(PROBLEM_SCHEMA));
    out.insert(
        "name".into(),
        Value::from(
            raw.get("name")
                .filter(|v| truthy(v))
                .map_or_else(|| "Differentiable engineering problem".to_string(), py_str),
        ),
    );
    out.insert(
        "description".into(),
        Value::from(raw.get("description").filter(|v| truthy(v)).map_or_else(String::new, py_str)),
    );
    out.insert("backend".into(), Value::from(backend));
    out.insert("model".into(), Value::Object(model_identity));
    out.insert("regions".into(), Value::Array(regions));
    out.insert("materials".into(), materials);
    out.insert("loads".into(), Value::Array(loads));
    out.insert("boundary_conditions".into(), Value::Array(bcs));
    out.insert(
        "analysis".into(),
        json!({"load_mode": load_mode, "overrides": overrides,
            "description": analysis.get("description").filter(|v| truthy(v)).map_or_else(String::new, py_str)}),
    );
    if let Some(b) = base_case.filter(|v| !v.is_null()) {
        out.insert("base_case".into(), b);
    }
    let id = canonical_id(&Value::Object(out.clone()), "problem");
    out.insert("problem_id".into(), Value::from(id));
    Ok(Value::Object(out))
}

#[must_use]
pub fn capabilities(backend: Option<&str>) -> Value {
    let selected = backends::selected_physics(contributions(), backend);
    let adapter = selected.as_ref().and_then(|b| adapter_of(b.as_ref()));
    let materials = adapter.as_ref().map_or_else(
        || json!({"effective": false, "models": [], "systems": [], "presets": [], "fraction_field": null}),
        |a| a.material_capabilities(),
    );
    let mut region_types: Vec<&str> = REGION_TYPES.iter().copied().filter(|t| *t != "ref").collect();
    region_types.sort_unstable();
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let has = adapter.is_some();
    json!({
        "schema": PROBLEM_SCHEMA,
        "backend": if has { selected.as_ref().map_or(Value::Null, |b| Value::from(b.name())) } else { Value::Null },
        "setup_mode": if has { "typed_entities" } else { "unset" },
        "regions": {"named": true, "references": true, "types": region_types},
        "materials": materials,
        "loads": adapter.as_ref().map_or_else(Vec::new, |a| sorted(a.load_kinds())),
        "boundary_conditions": adapter.as_ref().map_or_else(Vec::new, |a| sorted(a.boundary_condition_kinds())),
        "analysis_overrides": adapter.as_ref().map_or_else(Vec::new, |a| a.analysis_overrides()),
        "derivative_semantics": {
            "setup_effective": has,
            "geometry_response_derivatives": has,
            "engineering_values_as_free_coordinates": false,
            "note": if has {
                "accepted entities compile into the same case consumed by forward, Jacobian, JVP, VJP and optimisation operations; geometry is differentiated under that setup, not the material/load values themselves"
            } else {
                "no physics backend with a typed problem adapter is active"
            },
        },
    })
}

#[must_use]
pub fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    get(value, key)
}
