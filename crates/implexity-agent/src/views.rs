// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeSet;

use implexity_core::pyobj::list_repr;
use serde_json::{Map, Value, json};

use crate::error::{AgentError, AgentResult};
use crate::pyval::{is_hex, is_int, is_number, number};

const MONITOR_DESIGN_VALUE_KEYS: [&str; 6] = ["start", "start_document", "value", "values", "data", "array"];

pub const RENDER_SECTION_MAX_PIXELS: i64 = 160_000;
pub const RENDER_SECTION_MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
const RENDER_SECTION_KEYS: [&str; 15] = [
    "plane",
    "position",
    "width_px",
    "height_px",
    "field",
    "bbox_mm",
    "source",
    "palette",
    "value_range",
    "overlay_regions",
    "fit",
    "max_dimension_px",
    "background",
    "scalarization",
    "annotations",
];

fn compact_design_descriptors(
    raw: Option<&Value>,
    path: &str,
) -> AgentResult<(Vec<Value>, BTreeSet<String>, i64)> {
    let entries = match raw {
        None | Some(Value::Null) => return Ok((Vec::new(), BTreeSet::new(), 0)),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(AgentError::contract(format!(
                "authoritative {path} design coordinates are not an array"
            )));
        }
    };
    let mut descriptors = Vec::new();
    let mut omitted = BTreeSet::new();
    let mut declared = 0_i64;
    for entry in entries {
        let Some(e) = entry.as_object() else {
            return Err(AgentError::contract(format!(
                "authoritative {path} design-coordinate entry is not an object"
            )));
        };
        let mut descriptor = Map::new();
        for (key, value) in e {
            if MONITOR_DESIGN_VALUE_KEYS.contains(&key.as_str()) {
                omitted.insert(format!("{path}[].{key}"));
                continue;
            }
            if value.as_array().is_some_and(|a| a.len() > 64) {
                omitted.insert(format!("{path}[].{key}"));
                continue;
            }
            descriptor.insert(key.clone(), value.clone());
        }
        let mut size = e.get("size").filter(|s| is_int(s)).and_then(Value::as_i64).filter(|s| *s >= 0);
        if size.is_none() {
            let shape = e.get("shape").and_then(Value::as_array).filter(|s| {
                !s.is_empty() && s.iter().all(|x| is_int(x) && x.as_i64().is_some_and(|n| n >= 0))
            });
            size = match shape {
                Some(s) => Some(s.iter().filter_map(Value::as_i64).product()),
                None => MONITOR_DESIGN_VALUE_KEYS
                    .iter()
                    .find_map(|k| e.get(*k).and_then(Value::as_array))
                    .map(|a| i64::try_from(a.len()).unwrap_or(i64::MAX)),
            };
        }
        if let Some(s) = size.filter(|s| *s >= 0) {
            descriptor.entry("size").or_insert_with(|| json!(s));
            declared += s;
        }
        descriptors.push(Value::Object(descriptor));
    }
    Ok((descriptors, omitted, declared))
}

fn monitor_view(raw: &Value, key: &str, schema: &str, what: &str) -> AgentResult<Value> {
    let Some(m) = raw.as_object() else {
        return Err(AgentError::contract(format!("authoritative {what} returned a non-object")));
    };
    let mut result = m.clone();
    let (compact, omitted, declared) = compact_design_descriptors(m.get(key), key)?;
    let n = compact.len();
    result.insert(key.into(), Value::Array(compact));
    result.insert("view".into(), json!("monitor"));
    result.insert(
        "monitor_view".into(),
        json!({"schema": schema, "full_view_available": true, "declared_design_coordinates": n,
               "declared_design_scalars": declared, "omitted_paths": omitted.into_iter().collect::<Vec<_>>()}),
    );
    Ok(Value::Object(result))
}



pub fn optimization_job_monitor_view(raw: &Value) -> AgentResult<Value> {
    let Some(m) = raw.as_object() else {
        return Err(AgentError::contract("authoritative optimization-job inspection returned a non-object"));
    };
    let mut compact: Map<String, Value> = m
        .iter()
        .filter(|(key, _)| !["history", "previews"].contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if let Some(first) = m.get("history").and_then(Value::as_array).and_then(|rows| rows.first()) {
        let mut origin = Map::new();
        if let Some(value) = first.get("L_first") {
            origin.insert("L_first".into(), value.clone());
        }
        compact.insert("history".into(), json!([origin]));
    }
    let mut result = monitor_view(
        &Value::Object(compact), "free", "implexity-optimization-job-monitor/1", "optimization-job inspection",
    )?;
    let omitted = result["monitor_view"]["omitted_paths"]
        .as_array_mut()
        .ok_or_else(|| AgentError::contract("monitor-view omission list is unavailable"))?;
    for key in ["history", "previews"] {
        if m.contains_key(key) {
            omitted.push(json!(key));
        }
    }
    result["monitor_view"]["history_rows"] = json!(m.get("history").and_then(Value::as_array).map_or(0, Vec::len));
    Ok(result)
}



pub fn optimization_preflight_monitor_view(raw: &Value) -> AgentResult<Value> {
    monitor_view(raw, "free_plan", "implexity-optimization-preflight-monitor/1", "optimization preflight")
}

fn finite_number(v: &Value) -> Option<f64> {
    if is_number(v) { number(v).filter(|x| x.is_finite()) } else { None }
}

fn int_in(v: &Value, lo: i64, hi: i64) -> Option<i64> {
    v.as_i64().filter(|x| is_int(v) && (lo..=hi).contains(x))
}

const PARAM_REF_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-";



#[allow(clippy::too_many_lines)]
pub fn normalise_render_section_request(raw: &Value) -> AgentResult<Map<String, Value>> {
    let Some(request) = raw.as_object() else {
        return Err(AgentError::contract("render section payload must be an object"));
    };
    let mut unknown: Vec<&String> =
        request.keys().filter(|k| !RENDER_SECTION_KEYS.contains(&k.as_str())).collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(AgentError::contract(format!(
            "render section has unknown fields {}",
            list_repr(&unknown)
        )));
    }
    let c = AgentError::contract;
    let plane = request.get("plane").cloned().unwrap_or_else(|| json!("z"));
    let Some(plane) = plane.as_str().filter(|p| ["x", "y", "z"].contains(p)).map(str::to_owned) else {
        return Err(c("render section plane must be x, y, or z"));
    };
    let position = request.get("position").cloned().unwrap_or_else(|| json!(0.5));
    let Some(position) = finite_number(&position).filter(|p| (0.0..=1.0).contains(p)) else {
        return Err(c("render section position must be a finite number in [0, 1]"));
    };
    let fit = request.get("fit").filter(|v| !v.is_null());
    if fit.is_some_and(|f| f.as_str() != Some("physical_aspect")) {
        return Err(c("render section fit must be physical_aspect when supplied"));
    }
    let physical = fit.is_some();
    let (width, height, max_dimension) = if physical {
        if request.contains_key("width_px") || request.contains_key("height_px") {
            return Err(c("physical_aspect fit cannot be combined with width_px or height_px"));
        }
        let m = request.get("max_dimension_px").cloned().unwrap_or_else(|| json!(384));
        let Some(m) = int_in(&m, 64, 384) else {
            return Err(c("render section max_dimension_px must be an integer in [64, 384]"));
        };
        (None, None, Some(m))
    } else {
        if request.contains_key("max_dimension_px") {
            return Err(c("render section max_dimension_px requires fit=physical_aspect"));
        }
        let w = request.get("width_px").cloned().unwrap_or_else(|| json!(320));
        let h = request.get("height_px").cloned().unwrap_or_else(|| json!(320));
        let Some(w) = int_in(&w, 64, 384) else {
            return Err(c("render section width_px must be an integer in [64, 384]"));
        };
        let Some(h) = int_in(&h, 64, 384) else {
            return Err(c("render section height_px must be an integer in [64, 384]"));
        };
        if w * h > RENDER_SECTION_MAX_PIXELS {
            return Err(c("render section raster exceeds the 160000-pixel bound"));
        }
        (Some(w), Some(h), None)
    };
    let field = request.get("field").cloned().unwrap_or_else(|| json!("model_boundary"));
    let Some(field) = field.as_str().filter(|f| !f.is_empty() && f.chars().count() <= 240).map(str::to_owned)
    else {
        return Err(c("render section field must be a bounded string"));
    };
    let palette = request.get("palette").cloned().unwrap_or_else(|| json!("auto"));
    if !palette.as_str().is_some_and(|p| ["auto", "sequential", "diverging", "categorical"].contains(&p)) {
        return Err(c("render section palette must be auto, sequential, diverging, or categorical"));
    }
    let value_range = match request.get("value_range").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(v) => {
            let pair = v.as_array().filter(|a| a.len() == 2).and_then(|a| {
                let lo = finite_number(&a[0])?;
                let hi = finite_number(&a[1])?;
                (hi > lo).then_some([lo, hi])
            });
            let Some(pair) = pair else {
                return Err(c("render section value_range must be two increasing finite numbers"));
            };
            json!(pair)
        }
    };
    let overlay = request.get("overlay_regions").cloned().unwrap_or_else(|| json!([]));
    let overlay_ok = overlay.as_array().is_some_and(|a| {
        let mut seen = BTreeSet::new();
        a.len() <= 24
            && a.iter().all(|x| {
                x.as_str().is_some_and(|s| !s.is_empty() && s.chars().count() <= 160 && seen.insert(s))
            })
    });
    if !overlay_ok {
        return Err(c("render section overlay_regions must contain up to 24 unique ids"));
    }
    let overlay_regions = overlay.as_array().cloned().unwrap_or_default();
    let background = request.get("background").cloned().unwrap_or_else(|| json!("dark"));
    let Some(background) = background.as_str().filter(|b| ["dark", "white"].contains(b)).map(str::to_owned)
    else {
        return Err(c("render section background must be dark or white"));
    };
    let bbox = match request.get("bbox_mm").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(b) => json!(normalise_bbox(b)?),
    };
    let source = match request.get("source") {
        None => json!({"kind": "current_model"}),
        Some(Value::Object(m)) => Value::Object(m.clone()),
        Some(_) => return Err(c("render section source must be an object")),
    };
    let s = source.as_object().cloned().unwrap_or_default();
    let keys: BTreeSet<&str> = s.keys().map(String::as_str).collect();
    let kind = s.get("kind").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "current_model" if s.get("kind").is_some_and(Value::is_string) => {
            if keys != BTreeSet::from(["kind"]) {
                return Err(c("current_model render source takes only its kind"));
            }
        }
        "current_optimization_state" => {
            if keys != BTreeSet::from(["kind", "job_id", "epoch"]) {
                return Err(c("current_optimization_state source requires exactly kind, job_id, and epoch"));
            }
            if !s.get("job_id").is_some_and(crate::pyval::is_job_id) {
                return Err(c(
                    "render section optimization source requires a 12-character lowercase hexadecimal job_id",
                ));
            }
            let epoch = s.get("epoch").cloned().unwrap_or(Value::Null);
            if !(epoch == "current" || (is_int(&epoch) && epoch.as_i64().is_some_and(|e| e >= 0))) {
                return Err(c("render section epoch must be current or a nonnegative integer"));
            }
        }
        "result_artifact" => {
            if keys != BTreeSet::from(["kind", "artifact_id"]) {
                return Err(c("result_artifact source requires exactly kind and artifact_id"));
            }
            let ok = s
                .get("artifact_id")
                .and_then(Value::as_str)
                .and_then(|a| a.strip_prefix("result-"))
                .is_some_and(|h| is_hex(h, 32));
            if !ok {
                return Err(c("result_artifact source requires a content-addressed artifact_id"));
            }
            if field == "model_boundary" {
                return Err(c("result_artifact render source requires a stored field"));
            }
            implexity_render::artifact::validate_field_name(Some(&json!(field)), "field")
                .map_err(|e| AgentError::contract(e.to_string()))?;
            if !overlay_regions.is_empty() {
                return Err(c("historical result artifacts cannot use current-model region overlays"));
            }
        }
        _ => {
            return Err(c(
                "render section source kind must be current_model, current_optimization_state, or result_artifact",
            ));
        }
    }
    if kind != "result_artifact" && field != "model_boundary" {
        let parsed = implexity_geometry::node::ParamRef::parse(&field)
            .map_err(|_| c("render section field is not a model parameter ref"))?;
        let part_ok = |p: &str| !p.is_empty() && p.chars().all(|ch| PARAM_REF_CHARS.contains(ch));
        if parsed.path.first().map(String::as_str) != Some("model")
            || !parsed.path.iter().all(|p| part_ok(p))
            || !part_ok(&parsed.name)
        {
            return Err(c("render section field must be model[/child...]:parameter"));
        }
    }
    if let Some(a) = request.get("annotations")
        && !a.is_boolean()
    {
        return Err(c("section annotations must be boolean"));
    }
    if let Some(selector) = request.get("scalarization") {
        let ok = selector.as_object().is_some_and(|m| {
            (m.len() == 1
                && m.get("component").and_then(Value::as_str).is_some_and(|x| ["x", "y", "z"].contains(&x)))
                || (m.len() == 1 && m.get("magnitude") == Some(&Value::Bool(true)))
        });
        if !ok {
            return Err(c("section scalarization requires exactly component x/y/z or magnitude true"));
        }
    }
    let annotations_on = request.get("annotations") == Some(&Value::Bool(true));
    if kind != "result_artifact" && (request.contains_key("scalarization") || annotations_on) {
        return Err(c("section scalarization and annotations currently require result_artifact source"));
    }
    let mut out = Map::new();
    out.insert("plane".into(), json!(plane));
    out.insert("position".into(), json!(position));
    out.insert("field".into(), json!(field));
    out.insert("palette".into(), palette);
    out.insert("value_range".into(), value_range);
    out.insert("overlay_regions".into(), Value::Array(overlay_regions));
    out.insert("bbox_mm".into(), bbox);
    out.insert("source".into(), source);
    out.insert("background".into(), json!(background));
    if let Some(s) = request.get("scalarization") {
        out.insert("scalarization".into(), s.clone());
    }
    if let Some(a) = request.get("annotations") {
        out.insert("annotations".into(), a.clone());
    }
    if physical {
        out.insert("fit".into(), json!("physical_aspect"));
        out.insert("max_dimension_px".into(), json!(max_dimension));
    } else {
        out.insert("width_px".into(), json!(width));
        out.insert("height_px".into(), json!(height));
    }
    Ok(out)
}



pub fn normalise_bbox(b: &Value) -> AgentResult<[[f64; 3]; 2]> {
    let rows = b
        .as_array()
        .filter(|r| r.len() == 2 && r.iter().all(|row| row.as_array().is_some_and(|x| x.len() == 3)))
        .ok_or_else(|| AgentError::contract("render section bbox_mm must be [[x0,y0,z0],[x1,y1,z1]]"))?;
    let mut out = [[0.0; 3]; 2];
    for (i, row) in rows.iter().enumerate() {
        for (j, v) in row.as_array().into_iter().flatten().enumerate() {
            let x = finite_number(v).filter(|x| x.abs() <= 1_000_000.0).ok_or_else(|| {
                AgentError::contract("render section bbox_mm values must be finite and within +/-1000000 mm")
            })?;
            out[i][j] = x;
        }
    }
    if (0..3).any(|a| out[1][a] <= out[0][a]) {
        return Err(AgentError::contract("render section bbox_mm upper bounds must exceed lower bounds"));
    }
    Ok(out)
}



pub fn render_section_dimensions(
    request: &Map<String, Value>,
    bbox: &[[f64; 3]; 2],
) -> AgentResult<(usize, usize, Map<String, Value>)> {
    let axis = match request.get("plane").and_then(Value::as_str) {
        Some("x") => 0,
        Some("y") => 1,
        _ => 2,
    };
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let span_u = bbox[1][u] - bbox[0][u];
    let span_v = bbox[1][v] - bbox[0][v];
    if !span_u.is_finite() || !span_v.is_finite() || span_u <= 0.0 || span_v <= 0.0 {
        return Err(AgentError::contract(
            "render section in-plane physical bounds must be finite and positive",
        ));
    }
    let physical = request.get("fit").and_then(Value::as_str) == Some("physical_aspect");
    let (width, height, maximum, mode) = if physical {
        let maximum = request.get("max_dimension_px").and_then(Value::as_i64).unwrap_or(384);
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let scaled = |ratio: f64| 64_i64.max((maximum as f64 * ratio).round_ties_even() as i64);
        let (w, h) = if span_u >= span_v {
            (maximum, scaled(span_v / span_u))
        } else {
            (scaled(span_u / span_v), maximum)
        };
        (w.min(maximum), h.min(maximum), Some(maximum), "physical_aspect")
    } else {
        (
            request.get("width_px").and_then(Value::as_i64).unwrap_or(320),
            request.get("height_px").and_then(Value::as_i64).unwrap_or(320),
            None,
            "fixed_pixels",
        )
    };
    if width * height > RENDER_SECTION_MAX_PIXELS {
        return Err(AgentError::contract("render section raster exceeds the 160000-pixel bound"));
    }
    let names = ["x", "y", "z"];
    let mut framing = Map::new();
    framing.insert("mode".into(), json!(mode));
    framing.insert("in_plane_axes".into(), json!([names[u], names[v]]));
    framing.insert("physical_span_mm".into(), json!([span_u, span_v]));
    framing.insert("actual_dimensions_px".into(), json!([width, height]));
    framing.insert("max_dimension_px".into(), json!(maximum));
    framing.insert("pixel_limit".into(), json!(RENDER_SECTION_MAX_PIXELS));
    framing.insert("minimum_dimension_px".into(), json!(64));
    Ok((usize::try_from(width).unwrap_or(0), usize::try_from(height).unwrap_or(0), framing))
}

