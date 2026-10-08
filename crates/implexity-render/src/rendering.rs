// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use implexity_mesh::model_view::{Centering, ModelView};
use serde_json::{Map, Value, json};

use crate::RenderError;

fn err(m: impl Into<String>) -> RenderError {
    RenderError::Invalid(m.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldSection {
    pub values: Vec<f64>,
    pub outside: Vec<bool>,
    pub points: Vec<[f64; 3]>,
    pub at_mm: f64,
    pub pixel_size_mm: f64,
    pub node: Value,
    pub content_id: Value,
}



pub fn sample_field_section(
    view: &dyn ModelView,
    field: &str,
    plane: &str,
    position: f64,
    bbox: [[f64; 3]; 2],
    width: usize,
    height: usize,
) -> Result<FieldSection, RenderError> {
    let grid = view.resolve_registered_field(field)?;
    let axis = match plane {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        _ => return Err(err("invalid section plane")),
    };
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let (lower, upper) = (bbox[0], bbox[1]);
    let at = lower[axis] + position * (upper[axis] - lower[axis]);
    let pixel = ((upper[u] - lower[u]) / width as f64).max((upper[v] - lower[v]) / height as f64);
    let us = implexity_mesh::numeric::linspace(
        (lower[u] + upper[u] - pixel * width as f64) / 2.0 + pixel / 2.0,
        (lower[u] + upper[u] + pixel * width as f64) / 2.0 - pixel / 2.0,
        width,
    );
    let vs = implexity_mesh::numeric::linspace(
        (lower[v] + upper[v] - pixel * height as f64) / 2.0 + pixel / 2.0,
        (lower[v] + upper[v] + pixel * height as f64) / 2.0 - pixel / 2.0,
        height,
    );
    let centred = grid.centering == Centering::Cell;
    let shape = grid.values.shape;
    let mut values = Vec::with_capacity(width * height);
    let mut outside = Vec::with_capacity(width * height);
    let mut points = Vec::with_capacity(width * height);
    for &uu in &us {
        for &vv in &vs {
            let mut p = [0.0; 3];
            p[axis] = at;
            p[u] = uu;
            p[v] = vv;
            let idx: [i64; 3] = std::array::from_fn(|a| {
                let c = (p[a] - grid.origin[a]) / grid.spacing[a] + if centred { 0.0 } else { 0.5 };
                implexity_mesh::cast::trunc_i64(c.floor())
            });
            let valid = (0..3).all(|a| idx[a] >= 0 && (idx[a] as usize) < shape[a]);
            values.push(if valid {
                grid.values.at(idx[0] as usize, idx[1] as usize, idx[2] as usize)
            } else {
                f64::NAN
            });
            outside.push(!valid || uu < lower[u] || uu > upper[u] || vv < lower[v] || vv > upper[v]);
            points.push(p);
        }
    }
    Ok(FieldSection {
        values,
        outside,
        points,
        at_mm: at,
        pixel_size_mm: pixel,
        node: grid.record.get("node").cloned().unwrap_or(Value::Null),
        content_id: grid.record.get("content_id").cloned().unwrap_or(Value::Null),
    })
}



pub fn sample_field_points(
    view: &dyn ModelView,
    field: &str,
    points: &[[f64; 3]],
) -> Result<(Vec<f64>, Vec<bool>), RenderError> {
    let grid = view.resolve_registered_field(field)?;
    let shape = grid.values.shape;
    let centred = grid.centering == Centering::Cell;
    let mut values = Vec::with_capacity(points.len());
    let mut valid = Vec::with_capacity(points.len());
    for p in points {
        let idx: [i64; 3] = std::array::from_fn(|a| {
            let c = (p[a] - grid.origin[a]) / grid.spacing[a] + if centred { 0.0 } else { 0.5 };
            implexity_mesh::cast::trunc_i64(c.floor())
        });
        valid.push((0..3).all(|a| idx[a] >= 0 && (idx[a] as usize) < shape[a]));
        let clipped: [usize; 3] = std::array::from_fn(|a| idx[a].clamp(0, shape[a] as i64 - 1) as usize);
        values.push(grid.values.at(clipped[0], clipped[1], clipped[2]));
    }
    Ok((values, valid))
}

fn problem_document(problem: &Value) -> Map<String, Value> {
    let Some(obj) = problem.as_object() else { return Map::new() };
    if obj.get("schema").and_then(Value::as_str) == Some("implexity-provider-engineering-problem/1") {
        let nested = obj.get("problem").or_else(|| obj.get("document"));
        return nested.and_then(Value::as_object).cloned().unwrap_or_default();
    }
    obj.clone()
}

#[must_use]
pub fn declared_collections(problem: &Value) -> Value {
    let doc = problem_document(problem);
    if doc.is_empty() {
        return json!({"boundary_regions": [], "operating_points": []});
    }
    let boundaries: Vec<Value> = doc
        .get("boundaries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|row| {
            let mut out = Map::new();
            for k in ["region", "role", "name", "selector"] {
                if let Some(v) = row.get(k).filter(|v| !v.is_null()) {
                    out.insert(k.into(), v.clone());
                }
            }
            Value::Object(out)
        })
        .collect();
    let points: Vec<Value> = doc
        .get("mission")
        .and_then(Value::as_object)
        .and_then(|m| m.get("operatingPoints"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|x| x.is_object())
        .cloned()
        .collect();
    json!({"boundary_regions": boundaries, "operating_points": points})
}

fn axis_index(v: Option<&Value>) -> Option<usize> {
    match v.and_then(Value::as_str) {
        Some("x") => Some(0),
        Some("y") => Some(1),
        Some("z") => Some(2),
        _ => None,
    }
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => implexity_core::pyobj::repr(other),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

struct MaskContext<'a> {
    named: Map<String, Value>,
    points: &'a [[f64; 3]],
    tolerance: f64,
    bbox: [[f64; 3]; 2],
    view: &'a dyn ModelView,
}

impl MaskContext<'_> {
    #[allow(clippy::too_many_lines)]
    fn mask_of(&self, raw: Option<&Value>, depth: usize) -> Result<Option<Vec<bool>>, RenderError> {
        let Some(raw) = raw.and_then(Value::as_object) else { return Ok(None) };
        if depth > 64 {
            return Ok(None);
        }
        let mut raw = raw.clone();
        if !raw.contains_key("type") && raw.len() == 1 {
            let (kind, payload) = raw.iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap_or_default();
            let mut m = Map::new();
            m.insert("type".into(), json!(kind));
            if let Some(p) = payload.as_object() {
                for (k, v) in p {
                    m.insert(k.clone(), v.clone());
                }
            }
            raw = m;
        }
        let kind = raw.get("type").or_else(|| raw.get("kind")).map_or_else(String::new, py_str);
        let pts = self.points;
        match kind.as_str() {
            "ref" => {
                let id = raw.get("id").map_or_else(|| "None".to_string(), py_str);
                self.mask_of(self.named.get(&id), depth + 1)
            }
            "field_threshold" => {
                let mut unknown: Vec<&str> = raw
                    .keys()
                    .map(String::as_str)
                    .filter(|k| !["type", "field", "op", "value"].contains(k))
                    .collect();
                if !unknown.is_empty() {
                    unknown.sort_unstable();
                    let list = Value::Array(unknown.iter().map(|k| json!(k)).collect());
                    return Err(err(format!(
                        "field_threshold has unknown keys {}",
                        implexity_core::pyobj::repr(&list)
                    )));
                }
                let field = raw.get("field").and_then(Value::as_str).filter(|f| !f.is_empty());
                let op = raw.get("op").and_then(Value::as_str).filter(|o| *o == "ge" || *o == "le");
                let threshold = raw
                    .get("value")
                    .filter(|v| !v.is_boolean())
                    .and_then(Value::as_f64)
                    .filter(|t| t.is_finite());
                let (Some(field), Some(op), Some(threshold)) = (field, op, threshold) else {
                    return Err(err("field_threshold requires a model field, ge/le, and a finite value"));
                };
                let (sampled, valid) = sample_field_points(self.view, field, pts)?;
                Ok(Some(
                    sampled
                        .iter()
                        .zip(valid)
                        .map(|(s, ok)| ok && if op == "ge" { *s >= threshold } else { *s <= threshold })
                        .collect(),
                ))
            }
            "face" => {
                let Some(axis) = axis_index(raw.get("axis")) else { return Ok(None) };
                let side = raw.get("side").map_or_else(String::new, py_str);
                let bound = if ["lo", "low", "min", "-"].contains(&side.as_str()) {
                    self.bbox[0][axis]
                } else {
                    self.bbox[1][axis]
                };
                let tol = (self.tolerance * 1.5).max(1e-9);
                Ok(Some(pts.iter().map(|p| (p[axis] - bound).abs() <= tol).collect()))
            }
            "slab" => {
                let Some(axis) = axis_index(raw.get("axis")) else { return Ok(None) };
                let lo = raw.get("lo_mm").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY);
                let hi = raw.get("hi_mm").and_then(Value::as_f64).unwrap_or(f64::INFINITY);
                Ok(Some(pts.iter().map(|p| p[axis] >= lo && p[axis] <= hi).collect()))
            }
            "box" => {
                let vec3 = |k: &str| -> Option<[f64; 3]> {
                    let a = raw.get(k)?.as_array()?;
                    if a.len() != 3 {
                        return None;
                    }
                    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
                };
                let (Some(lo), Some(hi)) = (vec3("lo_mm"), vec3("hi_mm")) else { return Ok(None) };
                Ok(Some(pts.iter().map(|p| (0..3).all(|a| p[a] >= lo[a] && p[a] <= hi[a])).collect()))
            }
            "and" | "or" => {
                let children = raw.get("regions").or_else(|| raw.get("children"));
                let mut masks = Vec::new();
                for c in children.and_then(Value::as_array).into_iter().flatten() {
                    if let Some(m) = self.mask_of(Some(c), depth + 1)? {
                        masks.push(m);
                    }
                }
                if masks.is_empty() {
                    return Ok(None);
                }
                let and = kind == "and";
                Ok(Some(
                    (0..pts.len())
                        .map(|i| if and { masks.iter().all(|m| m[i]) } else { masks.iter().any(|m| m[i]) })
                        .collect(),
                ))
            }
            "not" => {
                let child = raw.get("region").or_else(|| raw.get("child"));
                Ok(self.mask_of(child, depth + 1)?.map(|m| m.iter().map(|b| !b).collect()))
            }
            _ => Ok(None),
        }
    }
}



pub fn region_masks(
    view: &dyn ModelView,
    problem: &Value,
    requested: &[String],
    points: &[[f64; 3]],
    tolerance: f64,
    bbox: [[f64; 3]; 2],
) -> Result<(Vec<Vec<bool>>, Vec<Value>), RenderError> {
    if requested.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let doc = problem_document(problem);
    let mut named = Map::new();
    for row in doc.get("regions").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object)
    {
        if let (Some(id), Some(sel)) =
            (row.get("id").filter(|v| truthy(v)), row.get("selector").filter(|s| s.is_object()))
        {
            named.insert(py_str(id), sel.clone());
        }
    }
    let mut boundaries: Vec<(String, Map<String, Value>, Option<Value>)> = Vec::new();
    for row in
        doc.get("boundaries").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object)
    {
        let region = row.get("region");
        let rid = if let Some(Value::String(s)) = region {
            s.clone()
        } else {
            let pick = row.get("id").filter(|v| truthy(v)).or_else(|| row.get("role").filter(|v| truthy(v)));
            pick.map_or_else(String::new, py_str)
        };
        let mut selector = row.get("selector").filter(|s| !s.is_null()).cloned();
        if let Some(r @ Value::Object(_)) = region {
            selector = Some(r.clone());
        }
        if selector.is_none() {
            selector = named.get(&rid).cloned();
        }
        if let Some(existing) = boundaries.iter_mut().find(|(id, _, _)| *id == rid) {
            *existing = (rid, row.clone(), selector);
        } else {
            boundaries.push((rid, row.clone(), selector));
        }
    }
    let selected: Vec<String> = if requested == ["*"] {
        boundaries.iter().map(|(id, _, _)| id.clone()).collect()
    } else {
        requested.to_vec()
    };
    let ctx = MaskContext { named, points, tolerance, bbox, view };
    let mut masks = Vec::new();
    let mut legend = Vec::new();
    for rid in selected {
        let found = boundaries.iter().find(|(id, _, _)| *id == rid);
        let (row, selector) = found.map_or((Map::new(), None), |(_, r, s)| (r.clone(), s.clone()));
        match ctx.mask_of(selector.as_ref(), 0)? {
            None => legend.push(json!({"id": rid, "status": "selector_unavailable"})),
            Some(m) => {
                masks.push(m);
                let label = row
                    .get("name")
                    .filter(|v| truthy(v))
                    .or_else(|| row.get("role").filter(|v| truthy(v)))
                    .map_or_else(|| rid.clone(), py_str);
                legend.push(
                    json!({"id": rid, "status": "rendered", "label": label, "colour_index": masks.len() - 1}),
                );
            }
        }
    }
    Ok((masks, legend))
}

const COORDINATE_FIELDS: [(&str, &str); 7] = [
    ("raw_gradient_l2", "gradient_l2"),
    ("applied_delta_l2", "l2"),
    ("applied_delta_max_abs", "max_abs"),
    ("free_entries", "free_entries"),
    ("bound_gradient_rms", "bound_gradient_rms"),
    ("update_denominator", "update_denominator"),
    ("predicted_descent", "predicted_descent"),
];

fn py_num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn is_number(v: Option<&Value>) -> bool {
    py_num(v).is_some()
}

#[must_use]
pub fn available_history_series(rows: &[Value]) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for row in rows.iter().filter_map(Value::as_object) {
        for (key, value) in row {
            if value.as_f64().is_some_and(f64::is_finite) {
                names.insert(key.clone());
            }
        }
    }
    for row in rows.iter().filter_map(Value::as_object) {
        for term in
            row.get("terms").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object)
        {
            if let (Some(r), true) =
                (term.get("response").and_then(Value::as_str), is_number(term.get("value")))
            {
                names.insert(r.to_string());
            }
        }
        if let Some(updates) = row.get("coordinate_updates").and_then(Value::as_object) {
            for (coordinate, update) in updates {
                let Some(update) = update.as_object() else { continue };
                let prefix = format!("coordinate::{coordinate}::");
                for (field, source) in COORDINATE_FIELDS {
                    if is_number(update.get(source)) {
                        names.insert(format!("{prefix}{field}"));
                    }
                }
                if is_number(update.get("gradient_l2")) {
                    names.insert(format!("{prefix}raw_gradient_energy_share"));
                }
                if is_number(update.get("l2")) {
                    names.insert(format!("{prefix}applied_delta_energy_share"));
                }
            }
        }
    }
    let preferred: Vec<String> = ["objective", "gradient_norm", "step_fraction"]
        .iter()
        .filter(|x| names.contains(**x))
        .map(|x| (*x).to_string())
        .collect();
    let mut out = preferred.clone();
    out.extend(names.into_iter().filter(|n| !preferred.contains(n)));
    out
}

#[must_use]
pub fn history_values(rows: &[Value], name: &str) -> Vec<f64> {
    rows.iter()
        .map(|row| {
            let obj = row.as_object();
            let mut value = py_num(obj.and_then(|o| o.get(name)));
            if value.is_none() && name.starts_with("coordinate::") {
                let parts: Vec<&str> = name.splitn(3, "::").collect();
                let (coordinate, field) = if parts.len() == 3 { (parts[1], parts[2]) } else { ("", "") };
                let updates = obj.and_then(|o| o.get("coordinate_updates")).and_then(Value::as_object);
                if let Some(update) = updates.and_then(|u| u.get(coordinate)).and_then(Value::as_object) {
                    if let Some((_, source)) = COORDINATE_FIELDS.iter().find(|(f, _)| *f == field) {
                        value = py_num(update.get(*source));
                    } else if field == "raw_gradient_energy_share" || field == "applied_delta_energy_share" {
                        let source = if field.starts_with("raw_") { "gradient_l2" } else { "l2" };
                        let numerator = py_num(update.get(source)).unwrap_or(0.0).powi(2);
                        let denominator = implexity_mesh::numeric::py_sum(
                            updates
                                .into_iter()
                                .flat_map(|u| u.values())
                                .filter_map(Value::as_object)
                                .filter_map(|item| py_num(item.get(source)))
                                .map(|x| x * x),
                        );
                        value = Some(if denominator > 0.0 { numerator / denominator } else { 0.0 });
                    }
                }
            }
            if value.is_none() {
                value = obj
                    .and_then(|o| o.get("terms"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_object)
                    .find(|t| t.get("response").and_then(Value::as_str) == Some(name))
                    .and_then(|t| py_num(t.get("value")));
            }
            value.unwrap_or(f64::NAN)
        })
        .collect()
}

#[must_use]
pub fn coordinate_history_semantics() -> Value {
    json!({
        "raw_gradient_l2": "Provider derivative norm after active-family and exact design-domain masking, before \
            coordinate scaling, update metric, line search and projection.",
        "raw_gradient_energy_share": "Squared raw-gradient norm divided by the sum across coordinate families.",
        "applied_delta_l2": "Norm of the committed epoch-to-epoch coordinate change after update metric, move \
            limits, exact line search, coordinate boxes and design masks.",
        "applied_delta_max_abs": "Largest absolute committed coordinate change in the epoch.",
        "applied_delta_energy_share": "Squared committed-delta norm divided by the sum across coordinate families.",
        "free_entries": "Entries allowed to move by the coordinate design-domain mask; provider fixed-region \
            composition remains authoritative in addition.",
        "bound_gradient_rms": "RMS derivative on free entries after multiplication by the coordinate bound span, \
            before provider step scale.",
        "update_denominator": "Positive scalar used to normalize this coordinate family for the selected optimizer \
            update metric.",
        "predicted_descent": "Negative raw-gradient dot committed coordinate delta. Positive values are predicted \
            descent contributions; box and mask projection may make an individual family negative while exact \
            total Armijo acceptance holds.",
        "projection_effects": "The difference between raw-gradient and committed-delta diagnostics contains \
            coordinate scaling, optimizer normalization, move caps, exact line search, coordinate boxes, design \
            masks and provider topology invariants. No separate projector-component array is stored, so none is \
            implied.",
    })
}

