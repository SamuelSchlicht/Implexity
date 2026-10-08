// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use implexity_core::backends;

use crate::entities::{adapter_of, contributions};
use crate::error::{AResult, AuthoringError};
use crate::py::{
    Arr, get, jf, jfs, norm3, obj_mut, py_float, py_str, repr, setdefault_list, sha_unicode, truthy,
};
use crate::surface_regions::{as_region_definition, normalize_surface_patch};

pub const SCHEMA: &str = "implexity-engineering-glyph/1";
pub const SUPPORTED_KINDS: [&str; 5] = ["pressure", "traction", "heat_flux", "temperature", "clamp"];

#[must_use]
pub fn entity_kind(kind: &str) -> &'static str {
    match kind {
        "pressure" => "pressure",
        "traction" => "traction",
        "heat_flux" => "heat_flux",
        "temperature" => "dirichlet_T",
        _ => "clamp",
    }
}

#[must_use]
pub fn available_kinds(backend: Option<&str>) -> Vec<String> {
    let Some(selected) = backends::selected_physics(contributions(), backend) else { return Vec::new() };
    let Some(adapter) = adapter_of(selected.as_ref()) else { return Vec::new() };
    let accepted: BTreeSet<String> =
        adapter.load_kinds().into_iter().chain(adapter.boundary_condition_kinds()).collect();
    let mut out: Vec<String> = SUPPORTED_KINDS
        .iter()
        .filter(|k| accepted.contains(entity_kind(k)))
        .map(|k| (*k).to_string())
        .collect();
    out.sort();
    out
}

fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("EngineeringGlyphError", message)
}

fn vec3(value: Option<&Value>, name: &str) -> AResult<[f64; 3]> {
    let bad = || err(format!("{name} must contain three finite values"));
    let a = Arr::from_opt(value).map_err(|_| bad())?;
    match a.vec3() {
        Some(v) if v.iter().all(|x| x.is_finite()) => Ok(v),
        _ => Err(bad()),
    }
}

fn unit(v: [f64; 3], name: &str) -> AResult<[f64; 3]> {
    if !v.iter().all(|x| x.is_finite()) {
        return Err(err(format!("{name} must contain three finite values")));
    }
    let l = norm3(v);
    if l <= 1.0e-14 {
        return Err(err(format!("{name} must have non-zero length")));
    }
    Ok(v.map(|x| x / l))
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineeringGlyph {
    pub kind: String,
    pub patch: Value,
    pub magnitude: f64,
    pub direction: Option<[f64; 3]>,
    pub label: Option<String>,
    pub units: Option<String>,
    pub enabled: bool,
    pub glyph_id: Option<String>,
}

impl EngineeringGlyph {

    pub fn from_mapping(value: &Value) -> AResult<Self> {
        let kind = get(value, "kind")
            .or_else(|| get(value, "type"))
            .map_or_else(String::new, py_str)
            .trim()
            .to_lowercase();
        if !SUPPORTED_KINDS.contains(&kind.as_str()) {
            return Err(err(format!("unsupported engineering glyph kind: {}", repr(&Value::from(kind)))));
        }
        let source =
            get(value, "patch").or_else(|| get(value, "selection")).or_else(|| get(value, "surface_patch"));
        let Some(source) = source.filter(|s| s.is_object()) else {
            return Err(err("engineering glyph requires a surface patch"));
        };
        let patch = normalize_surface_patch(source, None, true)?;
        let magnitude =
            py_float(get(value, "magnitude").or_else(|| get(value, "value")).unwrap_or(&json!(0.0)))?;
        if !magnitude.is_finite() {
            return Err(err("glyph magnitude must be finite"));
        }
        let direction = match get(value, "direction").filter(|v| !v.is_null()) {
            None => None,
            Some(d) => Some(unit(vec3(Some(d), "direction")?, "direction")?),
        };
        let opt_str = |k: &str| get(value, k).filter(|v| !v.is_null()).map(py_str);
        let mut glyph = Self {
            kind: kind.clone(),
            magnitude,
            direction,
            label: opt_str("label"),
            units: opt_str("units"),
            enabled: get(value, "enabled").is_none_or(truthy),
            glyph_id: opt_str("id"),
            patch,
        };
        if glyph.direction.is_none() && (kind == "pressure" || kind == "heat_flux") {
            glyph.direction = Some(vec3(get(&glyph.patch, "normal"), "normal")?);
        }
        Ok(glyph)
    }


    pub fn serialise(&self) -> AResult<Value> {
        let direction = match self.direction {
            None => Value::Null,
            Some(d) => jfs(&unit(d, "direction")?),
        };
        let mut payload = Map::new();
        payload.insert("schema".into(), json!(SCHEMA));
        payload.insert("kind".into(), json!(self.kind));
        payload.insert("patch".into(), normalize_surface_patch(&self.patch, None, true)?);
        payload.insert("magnitude".into(), jf(self.magnitude));
        payload.insert("direction".into(), direction);
        payload.insert("label".into(), self.label.clone().map_or(Value::Null, Value::from));
        payload.insert("units".into(), self.units.clone().map_or(Value::Null, Value::from));
        payload.insert("enabled".into(), Value::Bool(self.enabled));
        let id = self
            .glyph_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("glyph_{}", &sha_unicode(&Value::Object(payload.clone()))[..24]));
        payload.insert("id".into(), Value::from(id));
        Ok(Value::Object(payload))
    }


    pub fn drag_anchor(
        &mut self,
        delta_mm: &Value,
        project: Option<&dyn Fn(&Value) -> AResult<Value>>,
    ) -> AResult<()> {
        let mut patch = normalize_surface_patch(&self.patch, None, true)?;
        let a = vec3(get(&patch, "anchor_mm"), "anchor")?;
        let d = vec3(Some(delta_mm), "delta_mm")?;
        let anchor: [f64; 3] = std::array::from_fn(|i| a[i] + d[i]);
        if let Some(m) = patch.as_object_mut() {
            m.insert("anchor_mm".into(), jfs(&anchor));
            m.insert("point".into(), jfs(&anchor));
            m.insert("center".into(), jfs(&anchor));
        }
        if let Some(p) = project {
            patch = normalize_surface_patch(&p(&patch)?, None, true)?;
        }
        self.patch = patch;
        Ok(())
    }


    pub fn drag_direction(&mut self, delta: &Value, gain: f64) -> AResult<()> {
        let base = match self.direction {
            Some(d) => d,
            None => vec3(get(&self.patch, "normal"), "normal")?,
        };
        let d = vec3(Some(delta), "direction delta")?;
        self.direction = Some(unit(std::array::from_fn(|i| base[i] + gain * d[i]), "direction")?);
        Ok(())
    }


    pub fn drag_magnitude(&mut self, scalar_delta: f64, sensitivity: f64, snap: Option<f64>) -> AResult<()> {
        let mut value = self.magnitude + sensitivity * scalar_delta;
        if let Some(s) = snap.filter(|s| *s > 0.0) {
            value = (value / s).round_ties_even() * s;
        }
        if !value.is_finite() {
            return Err(err("drag produced a non-finite magnitude"));
        }
        self.magnitude = value;
        Ok(())
    }


    pub fn drag_radius(
        &mut self,
        scalar_delta_mm: f64,
        minimum_mm: f64,
        snap_mm: Option<f64>,
    ) -> AResult<()> {
        let mut patch = normalize_surface_patch(&self.patch, None, true)?;
        let current = patch["radius_mm"].as_f64().unwrap_or(1.0);
        let mut radius = minimum_mm.max(current + scalar_delta_mm);
        if let Some(s) = snap_mm.filter(|s| *s > 0.0) {
            radius = minimum_mm.max((radius / s).round_ties_even() * s);
        }
        if let Some(m) = patch.as_object_mut() {
            m.insert("radius_mm".into(), jf(radius));
            m.insert("radius".into(), jf(radius));
        }
        self.patch = patch;
        Ok(())
    }


    pub fn to_problem_objects(&self, region_id: Option<&str>, object_id: Option<&str>) -> AResult<Value> {
        let encoded = self.serialise()?;
        let eid = py_str(&encoded["id"]);
        let rid =
            region_id.filter(|s| !s.is_empty()).map_or_else(|| format!("region_{eid}"), ToString::to_string);
        let oid = object_id
            .filter(|s| !s.is_empty())
            .map_or_else(|| format!("{}_{eid}", self.kind), ToString::to_string);
        let name = self.label.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| rid.clone());
        let region = as_region_definition(&self.patch, Some(&rid), Some(&name))?;
        let direction = match self.direction {
            None => None,
            Some(d) => Some(jfs(&unit(d, "direction")?)),
        };
        if ["pressure", "traction", "heat_flux"].contains(&self.kind.as_str()) {
            let mut load = Map::new();
            load.insert("id".into(), Value::from(oid));
            load.insert("kind".into(), Value::from(self.kind.clone()));
            load.insert("region".into(), Value::from(rid));
            load.insert("magnitude".into(), jf(self.magnitude));
            load.insert("units".into(), self.units.clone().map_or(Value::Null, Value::from));
            load.insert("enabled".into(), Value::Bool(self.enabled));
            load.insert("glyph".into(), encoded);
            if let Some(d) = direction {
                load.insert("direction".into(), d);
            }
            if self.kind == "pressure" {
                load.entry("follows_surface_normal").or_insert(Value::Bool(true));
            }
            return Ok(json!({"region": region, "load": load, "boundary_condition": null}));
        }
        let mut bc = Map::new();
        bc.insert("id".into(), Value::from(oid));
        bc.insert(
            "kind".into(),
            Value::from(if self.kind == "temperature" { "dirichlet_T" } else { "clamp" }),
        );
        bc.insert("region".into(), Value::from(rid));
        bc.insert("enabled".into(), Value::Bool(self.enabled));
        bc.insert("glyph".into(), encoded);
        if self.kind == "temperature" {
            bc.insert("value_K".into(), jf(self.magnitude));
            bc.insert(
                "units".into(),
                Value::from(self.units.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| "K".into())),
            );
        } else {
            bc.insert("components".into(), Value::from("xyz"));
            bc.insert(
                "units".into(),
                Value::from(self.units.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| "m".into())),
            );
        }
        Ok(json!({"region": region, "load": null, "boundary_condition": bc}))
    }
}


pub fn upsert_glyph_problem(problem: &Value, glyph: &EngineeringGlyph) -> AResult<Value> {
    let mut out = problem.clone();
    let objects = glyph.to_problem_objects(None, None)?;
    let root = obj_mut(&mut out)?;
    for (collection, key) in
        [("regions", "region"), ("loads", "load"), ("boundary_conditions", "boundary_condition")]
    {
        let item = &objects[key];
        if item.is_null() {
            continue;
        }
        let list = setdefault_list(root, collection)?;
        list.retain(|x| x.get("id") != item.get("id"));
        list.push(item.clone());
    }
    Ok(out)
}
