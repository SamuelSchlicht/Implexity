// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::py::{get, jf, jfs, norm3, obj_mut, py_float, py_str, repr, uuid_hex};

pub use crate::surface_regions::{
    migrate_surface_patches as migrate_surface_patches_v2,
    normalize_surface_patch as normalize_surface_patch_v2, surface_patch_weights as surface_patch_weights_v2,
};

fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("SpatialAuthoringError", message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceHit {
    pub point: [f64; 3],
    pub normal: [f64; 3],
    pub model_id: Option<String>,
    pub node_id: Option<String>,
}

impl SurfaceHit {

    pub fn validated(&self) -> AResult<Self> {
        if !self.point.iter().chain(&self.normal).all(|v| v.is_finite()) {
            return Err(err("Surface hit needs finite three-dimensional point and normal"));
        }
        let n = norm3(self.normal);
        if n <= 1e-12 {
            return Err(err("Surface normal is degenerate"));
        }
        Ok(Self {
            point: self.point,
            normal: self.normal.map(|v| v / n),
            model_id: self.model_id.clone(),
            node_id: self.node_id.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SnapSettings {
    pub enabled: bool,
    pub linear_increment: f64,
    pub angular_increment_deg: f64,
    pub magnitude_increment: f64,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self { enabled: true, linear_increment: 0.1, angular_increment_deg: 5.0, magnitude_increment: 1.0 }
    }
}

#[must_use]
pub fn snap_scalar(value: f64, increment: Option<f64>) -> f64 {
    match increment {
        Some(i) if i > 0.0 => (value / i).round_ties_even() * i,
        _ => value,
    }
}


pub fn snap_vector(vector: [f64; 3], increment: Option<f64>, axis: Option<&str>) -> AResult<[f64; 3]> {
    if !vector.iter().all(|v| v.is_finite()) {
        return Err(err("Vector must contain three finite components"));
    }
    let mut vec = vector;
    if let Some(a) = axis.filter(|a| !a.is_empty()) {
        let i = match a.to_lowercase().as_str() {
            "x" => 0,
            "y" => 1,
            "z" => 2,
            _ => return Err(err("Axis lock must be x, y or z")),
        };
        let mut locked = [0.0; 3];
        locked[i] = vec[i];
        vec = locked;
    }
    if let Some(inc) = increment.filter(|i| *i > 0.0) {
        vec = vec.map(|v| (v / inc).round_ties_even() * inc);
    }
    Ok(vec)
}

fn upsert(problem: &mut Map<String, Value>, name: &str, obj: &Value) -> AResult<()> {
    let coll = problem.entry(name.to_string()).or_insert_with(|| Value::Array(Vec::new()));
    let oid = py_str(&obj["id"]);
    match coll {
        Value::Object(m) => {
            m.insert(oid, obj.clone());
        }
        Value::Array(items) => {
            if let Some(slot) = items
                .iter_mut()
                .find(|e| e.is_object() && get(e, "id").map_or_else(String::new, py_str) == oid)
            {
                *slot = obj.clone();
            } else {
                items.push(obj.clone());
            }
        }
        _ => {
            return Err(err(format!(
                "Problem collection {} is neither a list nor a mapping",
                repr(&Value::from(name))
            )));
        }
    }
    Ok(())
}


pub fn surface_patch(
    hit: &SurfaceHit,
    radius: f64,
    patch_id: Option<&str>,
    label: Option<&str>,
) -> AResult<Value> {
    let hit = hit.validated()?;
    if radius <= 0.0 || !radius.is_finite() {
        return Err(err("Patch radius must be finite and positive"));
    }
    let pid = patch_id
        .filter(|p| !p.is_empty())
        .map_or_else(|| format!("surface_patch_{}", &uuid_hex()[..12]), ToString::to_string);
    Ok(json!({
        "id": pid,
        "name": label.filter(|l| !l.is_empty()).unwrap_or("Surface patch"),
        "type": "surface_patch",
        "kind": "boundary",
        "method": "implicit_surface_patch",
        "model_id": hit.model_id,
        "node_id": hit.node_id,
        "point": jfs(&hit.point),
        "normal": jfs(&hit.normal),
        "radius": jf(radius),
        "level": 0.0,
        "side": "visible",
        "follows_geometry": true,
    }))
}


pub fn add_surface_patch(problem: &Value, patch: &Value) -> AResult<Value> {
    let mut out = problem.clone();
    upsert(obj_mut(&mut out)?, "regions", patch)?;
    Ok(out)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectValues {
    pub magnitude: Option<f64>,
    pub vector: Option<[f64; 3]>,
    pub value: Option<f64>,
    pub units: Option<String>,
}

fn title_case(s: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in s.chars() {
        if c.is_alphabetic() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start = false;
        } else {
            out.push(c);
            start = true;
        }
    }
    out
}


pub fn engineering_object(
    kind: &str,
    region_id: &str,
    object_id: Option<&str>,
    name: Option<&str>,
    values: &ObjectValues,
) -> AResult<Value> {
    if !["pressure", "traction", "heat_flux", "prescribed_temperature", "clamp"].contains(&kind) {
        return Err(err(format!("Unsupported engineering object {}", repr(&Value::from(kind)))));
    }
    let mut obj = Map::new();
    obj.insert(
        "id".into(),
        Value::from(
            object_id
                .filter(|o| !o.is_empty())
                .map_or_else(|| format!("{kind}_{}", &uuid_hex()[..12]), ToString::to_string),
        ),
    );
    obj.insert(
        "name".into(),
        Value::from(
            name.filter(|n| !n.is_empty())
                .map_or_else(|| title_case(&kind.replace('_', " ")), ToString::to_string),
        ),
    );
    obj.insert("type".into(), Value::from(kind));
    obj.insert("region".into(), Value::from(region_id));
    obj.insert("enabled".into(), Value::Bool(true));
    obj.insert("source".into(), Value::from("viewport"));
    if kind == "pressure" || kind == "traction" {
        obj.insert("state_load".into(), Value::Bool(true));
    }
    if let Some(m) = values.magnitude {
        if !m.is_finite() {
            return Err(err("Magnitude must be finite"));
        }
        obj.insert("magnitude".into(), jf(m));
    }
    if let Some(v) = values.vector {
        obj.insert("vector".into(), jfs(&snap_vector(v, None, None)?));
    }
    if let Some(v) = values.value {
        if !v.is_finite() {
            return Err(err("Value must be finite"));
        }
        obj.insert("value".into(), jf(v));
    }
    if let Some(u) = values.units.as_ref().filter(|u| !u.is_empty()) {
        obj.insert("units".into(), Value::from(u.clone()));
    }
    if kind == "clamp" {
        obj.entry("components").or_insert_with(|| json!(["x", "y", "z"]));
    }
    Ok(Value::Object(obj))
}


pub fn add_engineering_object(problem: &Value, obj: &Value) -> AResult<Value> {
    let mut out = problem.clone();
    let t = get(obj, "type").and_then(Value::as_str);
    let name =
        if matches!(t, Some("prescribed_temperature" | "clamp")) { "boundary_conditions" } else { "loads" };
    upsert(obj_mut(&mut out)?, name, obj)?;
    Ok(out)
}


pub fn author_at_surface(
    problem: &Value,
    hit: &SurfaceHit,
    object_type: &str,
    radius: f64,
    values: &ObjectValues,
) -> AResult<(Value, Value, Value)> {
    let patch = surface_patch(hit, radius, None, None)?;
    let out = add_surface_patch(problem, &patch)?;
    let obj = engineering_object(object_type, &py_str(&patch["id"]), None, None, values)?;
    let out = add_engineering_object(&out, &obj)?;
    Ok((out, patch, obj))
}

fn patch_vec(patch: &Value, key: &str) -> AResult<[f64; 3]> {
    let v = patch.get(key).ok_or_else(|| AuthoringError::Key(repr(&Value::from(key))))?;
    let a = crate::py::Arr::from_json(v)?;
    a.vec3().ok_or_else(|| err(format!("{key} must contain three values")))
}


pub fn patch_weights(points: &[[f64; 3]], normals: Option<&[[f64; 3]]>, patch: &Value) -> AResult<Vec<f64>> {
    let center = patch_vec(patch, "point")?;
    let normal = patch_vec(patch, "normal")?;
    let radius = py_float(patch.get("radius").ok_or_else(|| AuthoringError::Key("'radius'".into()))?)?;
    let mut w: Vec<f64> = points
        .iter()
        .map(|p| {
            let delta: [f64; 3] = std::array::from_fn(|a| p[a] - center[a]);
            let off = crate::py::dot3(delta, normal);
            let tang: [f64; 3] = std::array::from_fn(|a| delta[a] - off * normal[a]);
            let q = crate::py::row_norm3(tang) / radius;
            if q < 1.0 { (1.0 - q * q).powi(2) } else { 0.0 }
        })
        .collect();
    if let Some(nrm) = normals {
        if nrm.len() != points.len() {
            return Err(err("Normals must match points"));
        }
        for (wi, n) in w.iter_mut().zip(nrm) {
            *wi *= crate::py::clip(crate::py::dot3(*n, normal), 0.0, 1.0);
        }
    }
    Ok(w)
}


pub fn nodal_surface_load(
    points: &[[f64; 3]],
    normals: &[[f64; 3]],
    nodal_areas: &[f64],
    patch: &Value,
    load: &Value,
) -> AResult<Vec<[f64; 3]>> {
    if normals.len() != points.len() || nodal_areas.len() != points.len() {
        return Err(err("Nodal surface arrays have incompatible shapes"));
    }
    let w = patch_weights(points, Some(normals), patch)?;
    let weighted: Vec<f64> = w.iter().zip(nodal_areas).map(|(a, b)| a * b).collect();
    match get(load, "type").and_then(Value::as_str) {
        Some("pressure") => {
            let magnitude =
                py_float(get(load, "magnitude").or_else(|| get(load, "value")).unwrap_or(&json!(0.0)))?;
            Ok(normals.iter().zip(&weighted).map(|(n, wa)| n.map(|v| -v * (magnitude * wa))).collect())
        }
        Some("traction") => {
            let vector = crate::py::Arr::from_json(get(load, "vector").unwrap_or(&json!([0.0, 0.0, 0.0])))?
                .vec3()
                .ok_or_else(|| err("Traction vector must contain three values"))?;
            let magnitude = py_float(get(load, "magnitude").unwrap_or(&json!(1.0)))?;
            let n = norm3(vector);
            if n <= 1e-15 {
                return Err(err("Traction vector is zero"));
            }
            let unit = vector.map(|v| v / n);
            Ok(weighted.iter().map(|wa| unit.map(|u| (wa * magnitude) * u)).collect())
        }
        _ => Err(err("Only pressure and traction form mechanical nodal loads")),
    }
}
