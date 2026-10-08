// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::py::{first, get, jf, jfs, norm3, py_float, py_str, repr, sha_unicode, truthy};

pub const SCHEMA: &str = "implexity-surface-patch/2";
pub const PATCH_SET_SCHEMA: &str = "implexity-surface-patch-set/1";

const KIND_ALIASES: [&str; 5] =
    ["surface_patch", "implicit_surface_patch", "local_surface", "local_surface_patch", "surface-region"];

const CLASS: &str = "SurfacePatchError";

fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value(CLASS, message)
}

fn vec3(value: Option<&Value>, name: &str) -> AResult<[f64; 3]> {
    let bad = || err(format!("{name} must contain exactly three finite numbers"));
    let items: Vec<Value> = match value {
        Some(Value::Object(m)) => vec![
            m.get("x").cloned().unwrap_or(Value::Null),
            m.get("y").cloned().unwrap_or(Value::Null),
            m.get("z").cloned().unwrap_or(Value::Null),
        ],
        Some(Value::Array(a)) if a.len() == 3 => a.clone(),
        _ => return Err(bad()),
    };
    let mut out = [0.0; 3];
    for (i, v) in items.iter().enumerate() {
        out[i] = py_float(v)?;
    }
    if !out.iter().all(|v| v.is_finite()) {
        return Err(bad());
    }
    Ok(out)
}

fn unit(value: Option<&Value>, name: &str) -> AResult<[f64; 3]> {
    let v = vec3(value, name)?;
    let n = norm3(v);
    if !n.is_finite() || n <= 1.0e-14 {
        return Err(err(format!("{name} must have non-zero length")));
    }
    Ok(v.map(|x| x / n))
}


pub fn unit_vec(v: [f64; 3], name: &str) -> AResult<[f64; 3]> {
    unit(Some(&jfs(&v)), name)
}

fn positive(value: &Value, name: &str, allow_zero: bool) -> AResult<f64> {
    let out = py_float(value)?;
    if !out.is_finite() || out < 0.0 || (out == 0.0 && !allow_zero) {
        let q = if allow_zero { "non-negative" } else { "positive" };
        return Err(err(format!("{name} must be a finite {q} value")));
    }
    Ok(out)
}

#[must_use]
pub fn surface_patch_id(patch: &Value) -> String {
    let mut payload = patch.as_object().cloned().unwrap_or_default();
    payload.shift_remove("id");
    payload.shift_remove("legacy");
    payload.shift_remove("definition_id");
    format!("sp_{}", &sha_unicode(&Value::Object(payload))[..24])
}

fn has_coordinates(source: &Value) -> bool {
    ["anchor_mm", "anchor", "point", "center", "origin"].iter().any(|k| get(source, k).is_some())
}


pub fn normalize_surface_patch(
    value: &Value,
    model_identity: Option<&Value>,
    include_legacy_aliases: bool,
) -> AResult<Value> {
    if !value.is_object() {
        return Err(err("surface patch must be an object"));
    }
    let mut source = value.clone();
    if !has_coordinates(&source) {
        let nested = get(&source, "selection").or_else(|| get(&source, "surface_patch")).cloned();
        if let Some(Value::Object(mut nested_source)) = nested {
            let label = get(&source, "label").filter(|v| !v.is_null()).cloned();
            if let Some(l) = label
                && nested_source.get("label").is_none_or(Value::is_null)
            {
                nested_source.insert("label".into(), l);
            }
            source = Value::Object(nested_source);
        }
    }
    let raw_kind = first(&source, &["kind", "type"])
        .map_or_else(|| "surface_patch".to_string(), py_str)
        .trim()
        .to_lowercase();
    if !KIND_ALIASES.contains(&raw_kind.as_str()) {
        return Err(err(format!("unsupported surface patch kind: {}", repr(&Value::from(raw_kind)))));
    }
    let anchor = vec3(first(&source, &["anchor_mm", "anchor", "point", "center", "origin"]), "anchor")?;
    let normal = match first(&source, &["normal", "surface_normal"]) {
        Some(v) => unit(Some(v), "normal")?,
        None => [0.0, 0.0, 1.0],
    };
    let radius = positive(
        first(&source, &["radius_mm", "radius", "patch_radius"]).unwrap_or(&json!(1.0)),
        "radius_mm",
        false,
    )?;
    let band_default = jf((radius * 0.08).max(1.0e-6));
    let band = positive(
        first(&source, &["band_mm", "surface_band_mm", "thickness_mm", "tolerance_mm"])
            .unwrap_or(&band_default),
        "band_mm",
        false,
    )?;
    let falloff_raw = first(&source, &["falloff"])
        .cloned()
        .unwrap_or_else(|| json!({"kind": "smoothstep", "exponent": 2.0}));
    let falloff = match &falloff_raw {
        Value::String(s) => json!({"kind": s, "exponent": 2.0}),
        Value::Object(_) => falloff_raw.clone(),
        _ => return Err(err("falloff must be a string or object")),
    };
    let falloff_kind =
        get(&falloff, "kind").map_or_else(|| "smoothstep".to_string(), py_str).trim().to_lowercase();
    if !["constant", "linear", "smoothstep", "gaussian"].contains(&falloff_kind.as_str()) {
        return Err(err(format!("unsupported falloff: {}", repr(&Value::from(falloff_kind)))));
    }
    let exponent = positive(get(&falloff, "exponent").unwrap_or(&json!(2.0)), "falloff.exponent", false)?;
    let side = first(&source, &["side"]).map_or_else(|| "both".to_string(), py_str).trim().to_lowercase();
    if !["both", "positive", "negative"].contains(&side.as_str()) {
        return Err(err("side must be 'both', 'positive' or 'negative'"));
    }
    let normal_mode =
        first(&source, &["normal_mode"]).map_or_else(|| "front".to_string(), py_str).trim().to_lowercase();
    if normal_mode != "front" && normal_mode != "unsigned" {
        return Err(err("normal_mode must be 'front' or 'unsigned'"));
    }
    let mna = positive(
        first(&source, &["minimum_normal_alignment"]).unwrap_or(&json!(0.05)),
        "minimum_normal_alignment",
        true,
    )?;
    if mna > 1.0 {
        return Err(err("minimum_normal_alignment must not exceed one"));
    }
    let tracking_source = first(&source, &["tracking"]).cloned().unwrap_or_else(|| json!({}));
    let Value::Object(mut tracking) = tracking_source else { return Err(err("tracking must be an object")) };
    if !tracking.contains_key("mode") {
        let mode = first(&source, &["tracking_mode"]).map_or_else(|| "implicit".to_string(), py_str);
        tracking.insert("mode".into(), Value::from(mode));
    }
    for key in ["node_id", "structure_id", "content_id", "revision"] {
        if let Some(c) = first(&source, &[key])
            && !tracking.contains_key(key)
        {
            tracking.insert(key.into(), c.clone());
        }
    }
    if let Some(identity) = model_identity.filter(|v| truthy(v)) {
        for key in ["structure_id", "content_id", "revision"] {
            if let Some(v) = get(identity, key).filter(|v| !v.is_null())
                && !tracking.contains_key(key)
            {
                tracking.insert(key.into(), v.clone());
            }
        }
    }
    if let Some(selector) = first(&source, &["selector", "node_selector", "graph_path"])
        && !tracking.contains_key("selector")
    {
        tracking.insert("selector".into(), selector.clone());
    }
    let mut out = Map::new();
    out.insert("schema".into(), json!(SCHEMA));
    out.insert("kind".into(), json!("surface_patch"));
    out.insert("anchor_mm".into(), jfs(&anchor));
    out.insert("normal".into(), jfs(&normal));
    out.insert("radius_mm".into(), jf(radius));
    out.insert("band_mm".into(), jf(band));
    out.insert("falloff".into(), json!({"kind": falloff_kind, "exponent": jf(exponent)}));
    out.insert("side".into(), json!(side));
    out.insert("normal_mode".into(), json!(normal_mode));
    out.insert("minimum_normal_alignment".into(), jf(mna));
    out.insert("connected".into(), Value::Bool(first(&source, &["connected"]).is_none_or(truthy)));
    out.insert("tracking".into(), Value::Object(tracking));
    if let Some(label) = first(&source, &["label", "name"]).filter(|v| truthy(v)) {
        out.insert("label".into(), Value::from(py_str(label)));
    }
    let def_id = surface_patch_id(&Value::Object(out.clone()));
    out.insert("definition_id".into(), Value::from(def_id.clone()));
    let id = get(&source, "id").filter(|v| truthy(v)).map_or(def_id, py_str);
    out.insert("id".into(), Value::from(id));
    if include_legacy_aliases {
        out.insert("type".into(), json!("surface_patch"));
        out.insert("point".into(), jfs(&anchor));
        out.insert("center".into(), jfs(&anchor));
        out.insert("radius".into(), jf(radius));
        out.insert("surface_normal".into(), jfs(&normal));
    }
    Ok(Value::Object(out))
}


pub fn normalize(value: &Value) -> AResult<Value> {
    normalize_surface_patch(value, None, true)
}


pub fn migrate_surface_patches(value: &Value) -> AResult<Value> {
    match value {
        Value::Array(a) => Ok(Value::Array(a.iter().map(migrate_surface_patches).collect::<AResult<_>>()?)),
        Value::Object(m) => {
            let raw_kind =
                first(value, &["kind", "type"]).map_or_else(String::new, py_str).trim().to_lowercase();
            if raw_kind == "surface_patch_set"
                || m.get("schema").and_then(Value::as_str) == Some(PATCH_SET_SCHEMA)
            {
                return normalize_surface_patch_set(value);
            }
            if KIND_ALIASES.contains(&raw_kind.as_str()) && has_coordinates(value) {
                return normalize(value);
            }
            Ok(Value::Object(
                m.iter()
                    .map(|(k, v)| Ok((k.clone(), migrate_surface_patches(v)?)))
                    .collect::<AResult<_>>()?,
            ))
        }
        other => Ok(other.clone()),
    }
}

fn falloff_weight(radial: f64, kind: &str, exponent: f64) -> f64 {
    let t = crate::py::clip(1.0 - radial, 0.0, 1.0);
    match kind {
        "constant" => {
            if radial <= 1.0 {
                1.0
            } else {
                0.0
            }
        }
        "linear" => t.powf(exponent),
        "smoothstep" => (t * t * (3.0 - 2.0 * t)).powf(exponent),
        _ => {
            let g = (-4.5 * (radial * radial)).exp();
            let edge = (-4.5f64).exp();
            crate::py::clip((g - edge) / (1.0 - edge), 0.0, 1.0).powf(exponent)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WeightInputs<'a> {
    pub signed_distance_mm: Option<&'a [f64]>,
    pub surface_normals: Option<&'a [[f64; 3]]>,
    pub node_areas_mm2: Option<&'a [f64]>,
}


pub fn surface_patch_weights(
    points: &[[f64; 3]],
    patch: &Value,
    inputs: &WeightInputs<'_>,
    normal_alignment_power: f64,
    normalise_integral: bool,
) -> AResult<Vec<f64>> {
    let spec = normalize_surface_patch(patch, None, false)?;
    let anchor = vec3(get(&spec, "anchor_mm"), "anchor")?;
    let normal = vec3(get(&spec, "normal"), "normal")?;
    let radius = spec["radius_mm"].as_f64().unwrap_or(1.0);
    let band = spec["band_mm"].as_f64().unwrap_or(1.0);
    let fk = spec["falloff"]["kind"].as_str().unwrap_or("smoothstep").to_string();
    let fe = spec["falloff"]["exponent"].as_f64().unwrap_or(2.0);
    let mut weights: Vec<f64> = points
        .iter()
        .map(|p| {
            let delta: [f64; 3] = std::array::from_fn(|a| p[a] - anchor[a]);
            let off = crate::py::dot3(delta, normal);
            let tang: [f64; 3] = std::array::from_fn(|a| delta[a] - off * normal[a]);
            falloff_weight(crate::py::row_norm3(tang) / radius, &fk, fe)
        })
        .collect();
    if let Some(sdf) = inputs.signed_distance_mm {
        if sdf.len() != points.len() {
            return Err(err("signed_distance_mm length must match points_mm"));
        }
        let side = spec["side"].as_str().unwrap_or("both");
        for (w, d) in weights.iter_mut().zip(sdf) {
            *w *= falloff_weight(d.abs() / band, "smoothstep", 1.0);
            if side == "positive" {
                *w *= if *d >= 0.0 { 1.0 } else { 0.0 };
            } else if side == "negative" {
                *w *= if *d <= 0.0 { 1.0 } else { 0.0 };
            }
        }
    }
    if let Some(normals) = inputs.surface_normals {
        if normals.len() != points.len() {
            return Err(err("surface_normals must have shape (n, 3)"));
        }
        let unsigned = spec["normal_mode"].as_str() == Some("unsigned");
        let mna = spec["minimum_normal_alignment"].as_f64().unwrap_or(0.05);
        for (w, n) in weights.iter_mut().zip(normals) {
            let len = crate::py::row_norm3(*n);
            let safe = if len > 1.0e-14 { len } else { 1.0 };
            let unit_n = n.map(|x| x / safe);
            let s = crate::py::clip(crate::py::dot3(unit_n, normal), -1.0, 1.0);
            let mut alignment = if unsigned { s.abs() } else { crate::py::clip(s, 0.0, 1.0) };
            if !(alignment >= mna) {
                alignment = 0.0;
            }
            *w *= alignment.powf(normal_alignment_power);
        }
    }
    if let Some(areas) = inputs.node_areas_mm2 {
        if areas.len() != points.len() || areas.iter().any(|a| *a < 0.0) {
            return Err(err("node_areas_mm2 must be non-negative and match points_mm"));
        }
        weights.iter_mut().zip(areas).for_each(|(w, a)| *w *= a);
    }
    if normalise_integral {
        let total = crate::py::np_sum(&weights);
        if total > 0.0 {
            for w in &mut weights {
                *w /= total;
            }
        }
    }
    Ok(weights)
}


pub fn project_anchor_to_surface(
    patch: &Value,
    evaluate: &dyn Fn([f64; 3]) -> AResult<f64>,
    gradient: &dyn Fn([f64; 3]) -> AResult<[f64; 3]>,
    max_iterations: usize,
    tolerance_mm: f64,
    max_step_mm: Option<f64>,
) -> AResult<Value> {
    let mut out = normalize(patch)?;
    let mut point = vec3(get(&out, "anchor_mm"), "anchor")?;
    let radius = out["radius_mm"].as_f64().unwrap_or(1.0);
    let max_step = max_step_mm.filter(|v| *v != 0.0).unwrap_or(radius);
    for _ in 0..max_iterations {
        let value = evaluate(point)?;
        if value.abs() <= tolerance_mm {
            break;
        }
        let grad = gradient(point)?;
        let g2 = crate::py::dot3(grad, grad);
        if !g2.is_finite() || g2 <= 1.0e-20 {
            return Err(err("cannot project patch: implicit gradient vanished"));
        }
        let s = crate::py::clip(value / g2, -max_step, max_step);
        point = std::array::from_fn(|a| point[a] - s * grad[a]);
    }
    let grad = gradient(point)?;
    let normal = unit(Some(&jfs(&grad)), "projected normal")?;
    if let Some(m) = out.as_object_mut() {
        m.insert("anchor_mm".into(), jfs(&point));
        m.insert("normal".into(), jfs(&normal));
        m.insert("point".into(), jfs(&point));
        m.insert("center".into(), jfs(&point));
        m.insert("surface_normal".into(), jfs(&normal));
    }
    let id = surface_patch_id(&out);
    if let Some(m) = out.as_object_mut() {
        m.insert("definition_id".into(), Value::from(id));
    }
    Ok(out)
}


pub fn as_region_definition(patch: &Value, region_id: Option<&str>, name: Option<&str>) -> AResult<Value> {
    let spec = normalize(patch)?;
    let rid = region_id.map_or_else(|| py_str(&spec["id"]), ToString::to_string);
    let label = get(&spec, "label").filter(|v| truthy(v)).map(py_str);
    let name =
        name.filter(|n| !n.is_empty()).map(ToString::to_string).or(label).unwrap_or_else(|| rid.clone());
    Ok(json!({
        "id": rid,
        "name": name,
        "description": "Cursor-authored connected implicit-surface patch",
        "selector": spec,
        "kind": "surface_patch",
        "type": "surface_patch",
        "selection": spec,
        "surface_patch": spec,
        "enabled": true,
    }))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfacePatchSummary {
    pub identifier: String,
    pub anchor_mm: [f64; 3],
    pub normal: [f64; 3],
    pub radius_mm: f64,
    pub band_mm: f64,
}

impl SurfacePatchSummary {

    pub fn from_mapping(value: &Value) -> AResult<Self> {
        let spec = normalize_surface_patch(value, None, false)?;
        Ok(Self {
            identifier: py_str(&spec["id"]),
            anchor_mm: vec3(get(&spec, "anchor_mm"), "anchor")?,
            normal: vec3(get(&spec, "normal"), "normal")?,
            radius_mm: spec["radius_mm"].as_f64().unwrap_or(1.0),
            band_mm: spec["band_mm"].as_f64().unwrap_or(1.0),
        })
    }
}


pub fn normalize_surface_patch_set(value: &Value) -> AResult<Value> {
    if !value.is_object() {
        return Err(err("surface patch set must be an object"));
    }
    let raw = get(value, "patches").or_else(|| get(value, "samples"));
    let Some(Value::Array(items)) = raw.filter(|v| truthy(v)) else {
        return Err(err("surface patch set requires at least one patch"));
    };
    let patches: Vec<Value> = items.iter().map(normalize).collect::<AResult<_>>()?;
    let combination =
        get(value, "combination").map_or_else(|| "union".to_string(), py_str).trim().to_lowercase();
    if !["union", "sum", "intersection"].contains(&combination.as_str()) {
        return Err(err("surface patch-set combination must be union, sum or intersection"));
    }
    let tracking = get(value, "tracking").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut payload = Map::new();
    payload.insert("schema".into(), json!(PATCH_SET_SCHEMA));
    payload.insert("kind".into(), json!("surface_patch_set"));
    payload.insert("type".into(), json!("surface_patch_set"));
    payload.insert("combination".into(), json!(combination));
    payload.insert("patches".into(), Value::Array(patches));
    payload.insert("tracking".into(), Value::Object(tracking));
    if let Some(l) = get(value, "label").filter(|v| !v.is_null()) {
        payload.insert("label".into(), Value::from(py_str(l)));
    }
    let id = get(value, "id")
        .filter(|v| truthy(v))
        .map_or_else(|| stable_surface_patch_set_id(&Value::Object(payload.clone())), py_str);
    payload.insert("id".into(), Value::from(id));
    Ok(Value::Object(payload))
}

#[must_use]
pub fn stable_surface_patch_set_id(value: &Value) -> String {
    let mut payload = value.as_object().cloned().unwrap_or_default();
    payload.shift_remove("id");
    format!("sps_{}", &sha_unicode(&Value::Object(payload))[..24])
}


pub fn surface_patch_set_weights(
    points: &[[f64; 3]],
    patch_set: &Value,
    inputs: &WeightInputs<'_>,
    normalise_integral: bool,
) -> AResult<(Vec<f64>, Vec<[f64; 3]>)> {
    let spec = normalize_surface_patch_set(patch_set)?;
    let patches = spec["patches"].as_array().cloned().unwrap_or_default();
    let mut per_patch = Vec::new();
    let mut normals_of = Vec::new();
    for p in &patches {
        per_patch.push(surface_patch_weights(points, p, inputs, 1.0, false)?);
        normals_of.push(vec3(get(p, "normal"), "normal")?);
    }
    let combination = spec["combination"].as_str().unwrap_or("union");
    let n = points.len();
    let mut combined = vec![0.0; n];
    for i in 0..n {
        let col = per_patch.iter().map(|w| w[i]);
        combined[i] = match combination {
            "union" => {
                col.fold(f64::NEG_INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) })
            }
            "sum" => {
                let s = col.fold(0.0, |a, b| a + b);
                if s < 0.0 { 0.0 } else { s }
            }
            _ => col.fold(f64::INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }),
        };
    }
    let fallback = normals_of[0];
    let normals: Vec<[f64; 3]> = (0..n)
        .map(|i| {
            let mut s = [0.0; 3];
            for (w, nrm) in per_patch.iter().zip(&normals_of) {
                for a in 0..3 {
                    s[a] += w[i] * nrm[a];
                }
            }
            let len = crate::py::row_norm3(s);
            if len > 1.0e-14 { s.map(|x| x / len) } else { fallback }
        })
        .collect();
    if normalise_integral {
        let total = crate::py::np_sum(&combined);
        if total > 0.0 {
            for w in &mut combined {
                *w /= total;
            }
        }
    }
    Ok((combined, normals))
}


pub fn as_surface_region_definition(
    value: &Value,
    region_id: Option<&str>,
    name: Option<&str>,
) -> AResult<Value> {
    let raw_kind = get(value, "kind")
        .or_else(|| get(value, "type"))
        .map_or_else(|| "surface_patch".to_string(), py_str)
        .trim()
        .to_lowercase();
    if raw_kind == "surface_patch_set"
        || get(value, "schema").and_then(Value::as_str) == Some(PATCH_SET_SCHEMA)
    {
        let spec = normalize_surface_patch_set(value)?;
        let rid = region_id.map_or_else(|| py_str(&spec["id"]), ToString::to_string);
        let label = get(&spec, "label").filter(|v| truthy(v)).map(py_str);
        let name =
            name.filter(|n| !n.is_empty()).map(ToString::to_string).or(label).unwrap_or_else(|| rid.clone());
        return Ok(json!({
            "id": rid,
            "name": name,
            "description": "Cursor-authored connected implicit-surface patch set",
            "selector": spec,
            "kind": "surface_patch_set",
            "type": "surface_patch_set",
            "selection": spec,
            "surface_patch_set": spec,
            "enabled": true,
        }));
    }
    as_region_definition(value, region_id, name)
}
