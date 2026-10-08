// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use base64::Engine as _;
use implexity_core::json::{DumpOptions, dumps, sha256_hex};
use implexity_mesh::model_view::{GridSampler, ModelView, registered_field_sampler};
use serde_json::{Map, Value, json};

use crate::RenderError;

pub const SCHEMA: &str = "implexity-native-scene/1";
pub const MIN_SAMPLE_COUNT: i64 = 16;
pub const MAX_SAMPLE_COUNT: i64 = 256;
pub const DEFAULT_SAMPLE_COUNT: i64 = 64;
pub const MIN_AXIS_SAMPLES: usize = 4;
pub const MAX_SCENE_TEXTURE_BYTES: usize = 32 * 1024 * 1024;
pub const SUPERSAMPLE_LEVELS: [i64; 3] = [1, 2, 3];

fn err(m: impl Into<String>) -> RenderError {
    RenderError::Invalid(m.into())
}

#[must_use]
pub fn scene_input_schema() -> Value {
    let vector = json!({"type": "array", "minItems": 3, "maxItems": 3,
                        "items": {"type": "number", "minimum": -1e9, "maximum": 1e9}});
    json!({
        "type": "object", "required": ["schema"], "additionalProperties": false,
        "properties": {
            "schema": {"const": SCHEMA},
            "surface": {"enum": ["solid", "complement"]},
            "sample_count": {"type": "integer", "minimum": MIN_SAMPLE_COUNT, "maximum": MAX_SAMPLE_COUNT,
                             "description": "samples on the longest bbox axis; other axes are proportional"},
            "quality": {"type": "integer", "minimum": 0, "maximum": 2},
            "supersample": {"type": "integer", "minimum": 1, "maximum": 3,
                            "description": "raymarch k x k rays per pixel at the refined level, box-filtered in linear light"},
            "bbox_mm": {"type": "array", "minItems": 2, "maxItems": 2, "items": vector},
            "camera": {"type": "object", "additionalProperties": false, "properties": {
                "projection": {"enum": ["perspective", "orthographic"]},
                "target_mm": vector,
                "distance_mm": {"type": "number", "exclusiveMinimum": 0, "maximum": 1e9},
                "vertical_span_mm": {"type": "number", "exclusiveMinimum": 0, "maximum": 1e9},
                "yaw_rad": {"type": "number", "minimum": -100, "maximum": 100},
                "pitch_rad": {"type": "number", "minimum": -1.5, "maximum": 1.5},
                "fov_deg": {"type": "number", "minimum": 5, "maximum": 100}}},
            "clip": {"anyOf": [{"type": "null"}, {"type": "object", "additionalProperties": false,
                "required": ["normal", "offset_mm"], "properties": {
                    "normal": vector, "offset_mm": {"type": "number", "minimum": -1e9, "maximum": 1e9},
                    "cap": {"type": "object", "additionalProperties": false,
                            "required": ["complement_color_rgb"],
                            "description": "fill the cut plane inside bbox_mm where the traced surface does not \
                                reach it (the complement) with an explicit display sRGB colour in 0..1; solid cut \
                                faces keep the surface/field colouring",
                            "properties": {"complement_color_rgb": {
                                "type": "array", "minItems": 3, "maxItems": 3,
                                "items": {"type": "number", "minimum": 0, "maximum": 1}}}}}}]},
            "color": {"anyOf": [{"type": "null"}, {"type": "object", "additionalProperties": false,
                "required": ["field"], "properties": {
                    "field": {"type": "string", "minLength": 1, "maxLength": 240},
                    "range": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number"}},
                    "palette": {"enum": ["sequential", "diverging", "two_color", "two_category"]},
                    "colors_rgb": {"type": "array", "minItems": 2, "maxItems": 2, "items": {
                        "type": "array", "minItems": 3, "maxItems": 3,
                        "items": {"type": "number", "minimum": 0, "maximum": 1}}},
                    "threshold": {"type": "number"},
                    "label": {"type": "string", "maxLength": 160}}}]},
            "surface_color_rgb": {"type": "array", "minItems": 3, "maxItems": 3,
                "items": {"type": "number", "minimum": 0, "maximum": 1},
                "description": "uniform display sRGB colour (0..1) of the traced surface and its cut faces, e.g. a \
                    fluid complement; mutually exclusive with color"},
            "background": {"enum": ["white", "light"]},
        },
    })
}

fn number(value: Option<&Value>, label: &str, lo: f64, hi: f64, positive: bool) -> Result<f64, RenderError> {
    let v = value.filter(|v| !v.is_boolean()).and_then(Value::as_f64).filter(|x| x.is_finite());
    let Some(x) = v else { return Err(err(format!("{label} must be finite numeric data"))) };
    if x < lo || x > hi || (positive && x <= 0.0) {
        return Err(err(format!("{label} is outside the permitted range")));
    }
    Ok(x)
}

fn closed<'a>(
    value: Option<&'a Value>,
    keys: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>, RenderError> {
    let obj = value.and_then(Value::as_object).filter(|o| o.keys().all(|k| keys.contains(&k.as_str())));
    obj.ok_or_else(|| err(format!("{label} contains unsupported fields")))
}

fn vector(value: Option<&Value>, label: &str) -> Result<[f64; 3], RenderError> {
    let a = value.and_then(Value::as_array).filter(|a| a.len() == 3);
    let Some(a) = a else { return Err(err(format!("{label} requires three coordinates"))) };
    Ok([
        number(Some(&a[0]), label, -1e9, 1e9, false)?,
        number(Some(&a[1]), label, -1e9, 1e9, false)?,
        number(Some(&a[2]), label, -1e9, 1e9, false)?,
    ])
}

fn int_value(v: &Value) -> Option<i64> {
    if v.is_i64() || v.is_u64() { v.as_i64() } else { None }
}



#[allow(clippy::too_many_lines)]
pub fn validate_scene(raw: &Value) -> Result<Value, RenderError> {
    let allowed = [
        "schema",
        "surface",
        "sample_count",
        "quality",
        "supersample",
        "bbox_mm",
        "camera",
        "clip",
        "color",
        "surface_color_rgb",
        "background",
    ];
    let s = closed(Some(raw), &allowed, "scene")?;
    if s.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(err("unsupported native scene schema"));
    }
    let surface = s.get("surface").map_or(Some("solid"), Value::as_str);
    let surface = surface.filter(|x| *x == "solid" || *x == "complement");
    let Some(surface) = surface else { return Err(err("unknown surface mode")) };
    let q = s.get("sample_count").map_or(Some(DEFAULT_SAMPLE_COUNT), int_value);
    let q = q.filter(|q| (MIN_SAMPLE_COUNT..=MAX_SAMPLE_COUNT).contains(q));
    let Some(q) = q else {
        return Err(err(format!(
            "sample_count must be an integer in {MIN_SAMPLE_COUNT}..{MAX_SAMPLE_COUNT}"
        )));
    };
    let quality = s.get("quality").map_or(Some(2), int_value).filter(|v| (0..=2).contains(v));
    let Some(quality) = quality else { return Err(err("quality must be 0, 1 or 2")) };
    let supersample =
        s.get("supersample").map_or(Some(1), int_value).filter(|v| SUPERSAMPLE_LEVELS.contains(v));
    let Some(supersample) = supersample else { return Err(err("supersample must be 1, 2 or 3")) };
    let bbox = match s.get("bbox_mm").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(b) => {
            let a = b.as_array().filter(|a| a.len() == 2);
            let Some(a) = a else { return Err(err("bbox_mm requires lower and upper corners")) };
            let lo = vector(Some(&a[0]), "bbox_mm")?;
            let hi = vector(Some(&a[1]), "bbox_mm")?;
            if (0..3).any(|i| hi[i] <= lo[i]) {
                return Err(err("bbox_mm must have positive extents"));
            }
            json!([lo, hi])
        }
    };
    let camera_keys =
        ["projection", "target_mm", "distance_mm", "vertical_span_mm", "yaw_rad", "pitch_rad", "fov_deg"];
    let empty = json!({});
    let c = closed(Some(s.get("camera").unwrap_or(&empty)), &camera_keys, "camera")?;
    let projection = c.get("projection").map_or(Some("orthographic"), Value::as_str);
    let projection = projection.filter(|p| *p == "orthographic" || *p == "perspective");
    let Some(projection) = projection else { return Err(err("unsupported projection")) };
    let mut camera = Map::new();
    camera.insert("projection".into(), json!(projection));
    camera.insert(
        "yaw_rad".into(),
        json!(number(Some(c.get("yaw_rad").unwrap_or(&json!(-0.62))), "yaw", -100.0, 100.0, false)?),
    );
    camera.insert(
        "pitch_rad".into(),
        json!(number(Some(c.get("pitch_rad").unwrap_or(&json!(0.42))), "pitch", -1.5, 1.5, false)?),
    );
    camera.insert(
        "fov_deg".into(),
        json!(number(Some(c.get("fov_deg").unwrap_or(&json!(32.0))), "fov", 5.0, 100.0, false)?),
    );
    for key in ["distance_mm", "vertical_span_mm"] {
        if let Some(v) = c.get(key) {
            camera.insert(key.into(), json!(number(Some(v), key, -1e9, 1e9, true)?));
        }
    }
    if let Some(t) = c.get("target_mm") {
        camera.insert("target_mm".into(), json!(vector(Some(t), "camera target")?));
    }
    let clip = match s.get("clip").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(clip) => {
            let clip = closed(Some(clip), &["normal", "offset_mm", "cap"], "clip")?;
            let n = vector(clip.get("normal"), "clip normal")?;
            let norm = implexity_mesh::numeric::py_hypot(&n);
            if norm < 1e-12 {
                return Err(err("clip normal must be nonzero"));
            }
            let mut out = Map::new();
            out.insert("normal".into(), json!(n.map(|x| x / norm)));
            out.insert(
                "offset_mm".into(),
                json!(number(clip.get("offset_mm"), "clip offset", -1e9, 1e9, false)? / norm),
            );
            if let Some(cap) = clip.get("cap").filter(|v| !v.is_null()) {
                let cap = closed(Some(cap), &["complement_color_rgb"], "clip cap")?;
                if !cap.contains_key("complement_color_rgb") {
                    return Err(err("clip cap requires complement_color_rgb"));
                }
                let rgb = vector(cap.get("complement_color_rgb"), "clip cap RGB")?;
                let mut checked = [0.0; 3];
                for (i, v) in rgb.iter().enumerate() {
                    checked[i] = number(Some(&json!(v)), "clip cap RGB", 0.0, 1.0, false)?;
                }
                out.insert("cap".into(), json!({"complement_color_rgb": checked}));
            }
            Value::Object(out)
        }
    };
    let color = match s.get("color").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(color) => {
            let color = closed(
                Some(color),
                &["field", "range", "palette", "colors_rgb", "threshold", "label"],
                "color",
            )?;
            let field =
                color.get("field").and_then(Value::as_str).filter(|f| (1..=240).contains(&f.chars().count()));
            let Some(field) = field else { return Err(err("color requires a declared field id")) };
            let palette = color.get("palette").map_or(Some("sequential"), Value::as_str);
            let palette =
                palette.filter(|p| ["sequential", "diverging", "two_color", "two_category"].contains(p));
            let Some(palette) = palette else { return Err(err("unsupported palette")) };
            let default_range = json!([0.0, 1.0]);
            let ran = color.get("range").unwrap_or(&default_range).as_array().filter(|a| a.len() == 2);
            let Some(ran) = ran else { return Err(err("field range requires two values")) };
            let r0 = number(Some(&ran[0]), "field range", -1e9, 1e9, false)?;
            let r1 = number(Some(&ran[1]), "field range", -1e9, 1e9, false)?;
            if r1 <= r0 {
                return Err(err("field range must be ordered"));
            }
            let mut out = Map::new();
            out.insert("field".into(), json!(field));
            out.insert("range".into(), json!([r0, r1]));
            out.insert("palette".into(), json!(palette));
            if palette.starts_with("two_") {
                let colors = color.get("colors_rgb").and_then(Value::as_array).filter(|a| a.len() == 2);
                let Some(colors) = colors else {
                    return Err(err("two-color palettes require two explicit RGB colors"));
                };
                let mut rows = Vec::new();
                for row in colors {
                    let v = vector(Some(row), "RGB")?;
                    let mut c = [0.0; 3];
                    for (i, x) in v.iter().enumerate() {
                        c[i] = number(Some(&json!(x)), "RGB", 0.0, 1.0, false)?;
                    }
                    rows.push(json!(c));
                }
                out.insert("colors_rgb".into(), Value::Array(rows));
            } else if color.contains_key("colors_rgb") {
                return Err(err("colors_rgb requires a two-color palette"));
            }
            if palette == "two_category" {
                let midpoint = json!(f64::midpoint(r0, r1));
                let t =
                    number(Some(color.get("threshold").unwrap_or(&midpoint)), "threshold", r0, r1, false)?;
                out.insert("threshold".into(), json!(t));
            } else if color.contains_key("threshold") {
                return Err(err("threshold requires two_category"));
            }
            if let Some(label) = color.get("label") {
                let l = label.as_str().filter(|l| l.chars().count() <= 160);
                let Some(l) = l else { return Err(err("invalid color label")) };
                out.insert("label".into(), json!(l));
            }
            Value::Object(out)
        }
    };
    let surface_colour = match s.get("surface_color_rgb").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(sc) => {
            if !color.is_null() {
                return Err(err("surface_color_rgb and color are mutually exclusive"));
            }
            let v = vector(Some(sc), "surface RGB")?;
            let mut c = [0.0; 3];
            for (i, x) in v.iter().enumerate() {
                c[i] = number(Some(&json!(x)), "surface RGB", 0.0, 1.0, false)?;
            }
            json!(c)
        }
    };
    let background = s.get("background").map_or(Some("white"), Value::as_str);
    let background = background.filter(|b| *b == "white" || *b == "light");
    let Some(background) = background else { return Err(err("native scenes use a light background")) };
    Ok(json!({
        "schema": SCHEMA, "surface": surface, "sample_count": q, "quality": quality,
        "supersample": supersample, "bbox_mm": bbox, "camera": camera, "clip": clip,
        "color": color, "surface_color_rgb": surface_colour, "background": background,
    }))
}

fn bounds(bbox: &Value) -> Result<([f64; 3], [f64; 3]), RenderError> {
    let a =
        bbox.as_array().filter(|a| a.len() == 2).ok_or_else(|| err("bounds requires three coordinates"))?;
    Ok((vector(Some(&a[0]), "bounds")?, vector(Some(&a[1]), "bounds")?))
}



pub fn scene_sample_shape(bbox: &Value, sample_count: i64) -> Result<[usize; 3], RenderError> {
    let (lo, hi) = bounds(bbox)?;
    let span: [f64; 3] = std::array::from_fn(|a| hi[a] - lo[a]);
    if span.iter().any(|x| x.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)) {
        return Err(err("scene bounds must have positive extents"));
    }
    let q = usize::try_from(sample_count).unwrap_or(0);
    let longest = span.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    #[allow(clippy::float_cmp, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let shape: [usize; 3] = span.map(|x| {
        if x == longest {
            q
        } else {
            let n = (((q as f64 - 1.0) * x / longest - 1e-9).ceil() as i64 + 1).max(0) as usize;
            MIN_AXIS_SAMPLES.max(q.min(n))
        }
    });
    let samples = shape[0] * shape[1] * shape[2];
    if 4 * samples > MAX_SCENE_TEXTURE_BYTES {
        return Err(err(format!(
            "scene field grid {}x{}x{} needs {} bytes per float32 texture, above the {MAX_SCENE_TEXTURE_BYTES}-byte \
             bound; reduce sample_count",
            shape[0],
            shape[1],
            shape[2],
            4 * samples
        )));
    }
    Ok(shape)
}

fn camera_basis(camera: &Map<String, Value>) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let yaw = camera["yaw_rad"].as_f64().unwrap_or(0.0);
    let pitch = camera["pitch_rad"].as_f64().unwrap_or(0.0);
    let direction = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()];
    let right = [-yaw.sin(), yaw.cos(), 0.0];
    let up = [-pitch.sin() * yaw.cos(), -pitch.sin() * yaw.sin(), pitch.cos()];
    (direction, right, up)
}

fn corners(lo: [f64; 3], hi: [f64; 3]) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(8);
    for x in [lo[0], hi[0]] {
        for y in [lo[1], hi[1]] {
            for z in [lo[2], hi[2]] {
                out.push([x, y, z]);
            }
        }
    }
    out
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {

    implexity_mesh::numeric::py_sum([a[0] * b[0], a[1] * b[1], a[2] * b[2]])
}

fn vec3(v: &Value) -> [f64; 3] {
    std::array::from_fn(|i| v[i].as_f64().unwrap_or(0.0))
}



pub fn framing(specification: &Value, aspect: f64) -> Result<Value, RenderError> {
    let camera = specification["camera"].as_object().cloned().unwrap_or_default();
    let (direction, right, up) = camera_basis(&camera);
    let (lo, hi) = bounds(&specification["bbox_mm"])?;
    let target = vec3(&camera["target_mm"]);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for corner in corners(lo, hi) {
        let p: [f64; 3] = std::array::from_fn(|a| corner[a] - target[a]);
        if camera["projection"] == "orthographic" {
            let half = 0.5 * camera["vertical_span_mm"].as_f64().unwrap_or(1.0);
            xs.push(dot(p, right) / (half * aspect));
            ys.push(dot(p, up) / half);
        } else {
            let depth = camera["distance_mm"].as_f64().unwrap_or(1.0) - dot(p, direction);
            if depth <= 1e-9 {
                return Ok(json!({"box_in_view": true, "camera_inside_or_near_box": true}));
            }
            let tangent = (camera["fov_deg"].as_f64().unwrap_or(32.0).to_radians() * 0.5).tan();
            xs.push(dot(p, right) / (depth * tangent * aspect));
            ys.push(dot(p, up) / (depth * tangent));
        }
    }
    let min = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (x0, x1, y0, y1) = (min(&xs).max(-1.0), max(&xs).min(1.0), min(&ys).max(-1.0), max(&ys).min(1.0));
    if x1 <= x0 || y1 <= y0 {
        return Err(err(
            "the scene camera does not see the scene bounds (target_mm/distance_mm frame empty space); omit \
             target_mm and distance_mm/vertical_span_mm to frame the model",
        ));
    }
    let fraction = (x1 - x0) * (y1 - y0) / 4.0;
    let mut record = json!({
        "box_in_view": true, "camera_inside_or_near_box": false,
        "box_view_extent_ndc": [[min(&xs), min(&ys)], [max(&xs), max(&ys)]],
        "visible_view_fraction": fraction,
    });
    if fraction < 0.02 {
        record["warning"] = json!(
            "the scene bounds cover under 2% of the view; the authored camera target/distance frame mostly empty space"
        );
    }
    Ok(record)
}

fn scene_sha256(spec: &Value) -> String {
    sha256_hex(dumps(spec, &DumpOptions::canonical()).as_bytes())
}



pub fn fit_capture_scene(bound: &Value, raw_scene: &Value, aspect: f64) -> Result<Value, RenderError> {
    let aspect = number(Some(&json!(aspect)), "capture aspect", -1e9, 1e9, true)?;
    let camera = bound["specification"]["camera"].as_object().cloned().unwrap_or_default();
    let authored = raw_scene.get("camera").and_then(Value::as_object).cloned().unwrap_or_default();
    let (direction, right, up) = camera_basis(&camera);
    let (lo, hi) = bounds(&bound["specification"]["bbox_mm"])?;
    let target = vec3(&camera["target_mm"]);
    let relative: Vec<[f64; 3]> =
        corners(lo, hi).into_iter().map(|p| std::array::from_fn(|a| p[a] - target[a])).collect();
    let mut fitted = bound.clone();
    let mut changed = false;
    if camera["projection"] == "orthographic" && !authored.contains_key("vertical_span_mm") {
        let required = relative
            .iter()
            .map(|p| (2.0 * dot(*p, right).abs() / aspect).max(2.0 * dot(*p, up).abs()))
            .fold(f64::NEG_INFINITY, f64::max);
        fitted["specification"]["camera"]["vertical_span_mm"] = json!((required * 1.08).max(1e-6));
        changed = true;
    }
    if camera["projection"] == "perspective" && !authored.contains_key("distance_mm") {
        let tangent = (camera["fov_deg"].as_f64().unwrap_or(32.0).to_radians() * 0.5).tan();
        let required = relative
            .iter()
            .map(|p| {
                (dot(*p, direction) + 1.08 * dot(*p, up).abs() / tangent)
                    .max(dot(*p, direction) + 1.08 * dot(*p, right).abs() / (tangent * aspect))
            })
            .fold(f64::NEG_INFINITY, f64::max);
        fitted["specification"]["camera"]["distance_mm"] = json!(required.max(1e-6));
        changed = true;
    }
    fitted["framing"] = framing(&fitted["specification"], aspect)?;
    if changed {
        fitted["sha256"] = json!(scene_sha256(&fitted["specification"]));
    }
    Ok(fitted)
}

pub trait DerivedFieldSource {

    fn derived_field_specs(&self) -> Result<Vec<Value>, RenderError>;

    fn derived_field_values(&self, id: &str, points: &[[f64; 3]]) -> Result<Vec<f64>, RenderError>;
}



pub fn derived_catalogue(source: &dyn DerivedFieldSource) -> Result<Vec<Value>, RenderError> {
    let mut out: Vec<Value> = Vec::new();
    for raw in source.derived_field_specs()? {
        let row = closed(Some(&raw), &["id", "label", "units", "description"], "derived field")?;
        let ident = row.get("id").and_then(Value::as_str).filter(|i| {
            !i.is_empty() && {
                let stripped: String = i.chars().filter(|c| *c != '_').collect();
                !stripped.is_empty() && stripped.chars().all(char::is_alphanumeric)
            }
        });
        let Some(ident) = ident else { return Err(err("invalid derived field id")) };
        if out.iter().any(|x| x["id"] == ident) {
            return Err(err("duplicate derived field id"));
        }
        let mut entry = row.clone();
        entry.insert("id".into(), json!(ident));
        entry.insert("field".into(), json!(format!("derived:{ident}")));
        entry.insert("read_only".into(), json!(true));
        entry.insert("source".into(), json!("authoritative_model_evaluation"));
        entry.insert("not_an_optimization_coordinate".into(), json!(true));
        out.push(Value::Object(entry));
    }
    Ok(out)
}



#[allow(clippy::too_many_lines)]
pub fn prepare_scene(
    view: &dyn ModelView,
    derived: Option<&dyn DerivedFieldSource>,
    raw: &Value,
    expected: &str,
) -> Result<Value, RenderError> {
    let mut scene = validate_scene(raw)?;
    let (shape, texture) = {
        let _guard = view.live_lock();
        let before = view.status()?;
        if before.content_id != expected {
            return Err(err("model changed before scene sampling"));
        }
        let bbox = if scene["bbox_mm"].is_null() {
            before.aabb.as_ref().and_then(|a| a.get("bbox_mm")).cloned().filter(|b| !b.is_null())
        } else {
            Some(scene["bbox_mm"].clone())
        };
        let Some(bbox) = bbox else {
            return Err(err("an unbounded model requires explicit finite scene bounds"));
        };
        let (lo, hi) = bounds(&bbox)?;
        if (0..3).any(|a| hi[a] <= lo[a]) {
            return Err(err("scene bounds must have positive extents"));
        }
        scene["bbox_mm"] = json!([lo, hi]);
        let diag = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();
        let shape = scene_sample_shape(
            &scene["bbox_mm"],
            scene["sample_count"].as_i64().unwrap_or(DEFAULT_SAMPLE_COUNT),
        )?;
        let cam = &mut scene["camera"];
        if cam.get("target_mm").is_none() {
            cam["target_mm"] = json!([(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5]);
        }
        if cam.get("distance_mm").is_none() {
            let half = cam["fov_deg"].as_f64().unwrap_or(32.0).to_radians() * 0.5;
            cam["distance_mm"] = json!((0.5 * diag / half.sin()).max(diag * 0.6) * 1.08);
        }
        if cam.get("vertical_span_mm").is_none() {
            cam["vertical_span_mm"] = json!(diag * 1.08);
        }
        let mut texture = Value::Null;
        if let Some(c) = scene["color"].as_object() {
            let field = c["field"].as_str().unwrap_or("").to_string();
            let axes: [Vec<f64>; 3] =
                std::array::from_fn(|a| implexity_mesh::numeric::linspace(lo[a], hi[a], shape[a]));
            let mut xyz = Vec::with_capacity(shape.iter().product());
            for &x in &axes[0] {
                for &y in &axes[1] {
                    for &z in &axes[2] {
                        xyz.push([x, y, z]);
                    }
                }
            }
            let value: Vec<f64> = if let Some(id) = field.strip_prefix("derived:") {
                let source =
                    derived.ok_or_else(|| err("color field is not declared by the authoritative root"))?;
                let available = derived_catalogue(source)?;
                if !available.iter().any(|x| x["field"] == field.as_str()) {
                    return Err(err("color field is not declared by the authoritative root"));
                }
                source.derived_field_values(id, &xyz)?
            } else {
                let (sampler, _meta): (GridSampler, Value) = registered_field_sampler(view, &field, false)?;
                let (value, valid) = sampler.sample(&xyz, 0.0)?;
                if !valid.iter().all(|&v| v) {
                    return Err(err("registered color field does not cover the requested scene"));
                }
                value
            };
            if value.len() != xyz.len() || value.iter().any(|v| !v.is_finite()) {
                return Err(err("derived scene field is not a finite scalar grid"));
            }
            let bytes: Vec<u8> =
                value.iter().flat_map(|v| implexity_mesh::cast::f32_of(*v).to_le_bytes()).collect();
            let lo_v = value.iter().copied().fold(f64::INFINITY, f64::min);
            let hi_v = value.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            texture = json!({
                "field": field, "content_id": expected, "shape": shape, "bbox_mm": scene["bbox_mm"],
                "encoding": "base64-float32-le-c-order",
                "data_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "sha256": sha256_hex(&bytes), "value_range": [lo_v, hi_v],
            });
        }
        if view.status()?.content_id != expected {
            return Err(err("model changed during scene sampling"));
        }
        (shape, texture)
    };
    let two_category = scene["color"]["palette"] == "two_category";
    Ok(json!({
        "specification": scene.clone(), "sha256": scene_sha256(&scene), "content_id": expected,
        "color_texture": texture, "sample_shape": shape,
        "interpretation": "display_only_not_physical_or_manufacturing_validation",
        "categorical_color_projection_only": two_category,
    }))
}
