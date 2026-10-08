// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use implexity_core::pyobj::repr as py_repr;
use implexity_mesh::mc::{GradientDirection, marching_cubes};
use implexity_mesh::numeric::{gradient3, interp, linspace, searchsorted};
use implexity_mesh::pyfmt::fmt_g;
use implexity_mesh::{Field3, raster};
use serde_json::{Map, Value, json};

use crate::RenderError;

#[must_use]
pub fn is_model_field(s: &str) -> bool {
    let seg = |p: &str| {
        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    let Some((path, name)) = s.rsplit_once(':') else { return false };
    let mut parts = path.split('/');
    parts.next() == Some("model") && parts.all(seg) && seg(name)
}

#[must_use]
pub fn is_artifact_field(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

pub const CAMERA_PRESETS: [&str; 7] = ["isometric", "front", "rear", "left", "right", "top", "bottom"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualityPreset {
    pub max_axis_samples: usize,
    pub max_samples: usize,
    pub max_triangles: usize,
}

pub const QUALITY: [(&str, QualityPreset); 4] = [
    ("preview", QualityPreset { max_axis_samples: 24, max_samples: 24_000, max_triangles: 60_000 }),
    ("draft", QualityPreset { max_axis_samples: 36, max_samples: 64_000, max_triangles: 90_000 }),
    ("standard", QualityPreset { max_axis_samples: 56, max_samples: 180_000, max_triangles: 220_000 }),
    ("high", QualityPreset { max_axis_samples: 72, max_samples: 196_000, max_triangles: 360_000 }),
];

pub const NATIVE_RESULT_QUALITIES: [&str; 2] = ["native", "analysis"];
pub const NATIVE_RESULT_MAX_SOURCE_SAMPLES: usize = 100_352;
pub const NATIVE_RESULT_MAX_WORKING_SAMPLES: usize = 125_000;
pub const NATIVE_RESULT_MAX_TRIANGLES: usize = 12 * NATIVE_RESULT_MAX_WORKING_SAMPLES;
pub const GEOMETRY_QUALITIES: [(&str, usize); 2] = [("geometry", 1), ("geometry_fine", 2)];
pub const GEOMETRY_MAX_SAMPLES: usize = 3_000_000;
pub const GEOMETRY_MAX_TRIANGLES: usize = 3_000_000;
pub const GEOMETRY_ANALYTIC_AXIS_SAMPLES: usize = 128;
pub const MIN_IMAGE_SIDE_PX: i64 = 128;
pub const MAX_IMAGE_SIDE_PX: i64 = 2048;
pub const MAX_IMAGE_PIXELS: i64 = 2048 * 2048;
pub const ANTIALIAS_LEVELS: [i64; 3] = [1, 2, 3];
pub const MAX_RASTER_PIXELS: usize = 8_388_608;
pub const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RASTER_TILE_SAMPLES: usize = 160_000_000;
pub const MAX_COLOR_STOPS: usize = 32;

const SEQUENTIAL: [[f64; 3]; 4] = raster::SEQUENTIAL;
const DIVERGING: [[f64; 3]; 5] = raster::DIVERGING;
pub const BASE_COLOUR: [f64; 3] = [52.0, 181.0, 193.0];

fn err(m: impl Into<String>) -> RenderError {
    RenderError::Invalid(m.into())
}

fn quality_preset(name: &str) -> Option<QualityPreset> {
    QUALITY.iter().find(|(n, _)| *n == name).map(|(_, q)| *q)
}

fn geometry_refinement(name: &str) -> Option<usize> {
    GEOMETRY_QUALITIES.iter().find(|(n, _)| *n == name).map(|(_, r)| *r)
}

#[must_use]
pub fn is_native_quality(name: &str) -> bool {
    NATIVE_RESULT_QUALITIES.contains(&name)
}

#[derive(Clone, Debug, PartialEq)]
pub struct CategoricalPalette {
    pub thresholds: Vec<f64>,
    pub categories: Vec<(String, String, [f64; 3])>,
    pub projection: Map<String, Value>,
}

impl CategoricalPalette {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "thresholds": self.thresholds,
            "categories": self.categories.iter()
                .map(|(id, label, rgb)| json!({"id": id, "label": label, "color_rgb": rgb}))
                .collect::<Vec<_>>(),
            "projection": self.projection,
        })
    }

    #[must_use]
    pub fn colour(&self, v: f64) -> [f64; 3] {
        self.categories[searchsorted(&self.thresholds, v, true).min(self.categories.len() - 1)].2
    }
}

fn finite_number(v: &Value) -> Option<f64> {
    if v.is_boolean() {
        return None;
    }
    v.as_f64().filter(|x| x.is_finite())
}



pub fn normalise_categorical_palette(raw: Option<&Value>) -> Result<CategoricalPalette, RenderError> {
    let Some(raw) = raw.and_then(Value::as_object) else {
        return Err(err("categorical rendering requires a declared categorical palette"));
    };
    let mut unknown: Vec<&str> = raw
        .keys()
        .map(String::as_str)
        .filter(|k| !["thresholds", "categories", "projection"].contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(err(format!("categorical palette has unknown fields {}", py_list(&unknown))));
    }
    let Some(thresholds_raw) = raw.get("thresholds").and_then(Value::as_array) else {
        return Err(err("categorical thresholds must be an array"));
    };
    let categories_raw = raw.get("categories").and_then(Value::as_array).filter(|c| !c.is_empty());
    let Some(categories_raw) = categories_raw else {
        return Err(err("categorical palette requires at least one category"));
    };

    let mut thresholds = Vec::with_capacity(thresholds_raw.len());
    for t in thresholds_raw {
        let v = match t {
            Value::Number(n) => n.as_f64(),
            Value::Bool(b) => Some(f64::from(u8::from(*b))),
            Value::String(s) => python_float(s),
            _ => None,
        };
        let Some(v) = v else { return Err(err("categorical thresholds must be finite numbers")) };
        thresholds.push(v);
    }
    if thresholds.len() + 1 != categories_raw.len()
        || thresholds.iter().any(|t| !t.is_finite())
        || thresholds.windows(2).any(|p| p[1] <= p[0])
    {
        return Err(err(
            "categorical thresholds must be finite, increasing, and one fewer than the categories",
        ));
    }
    let mut categories = Vec::with_capacity(categories_raw.len());
    let mut seen: Vec<String> = Vec::new();
    for (index, item) in categories_raw.iter().enumerate() {
        let Some(item) = item.as_object() else {
            return Err(err(format!("categorical category {index} must be an object")));
        };
        if item.keys().any(|k| !["id", "label", "color_rgb"].contains(&k.as_str())) {
            return Err(err(format!("categorical category {index} has unknown fields")));
        }
        let id = item.get("id").and_then(Value::as_str);
        let id = id.filter(|i| !i.is_empty() && !seen.iter().any(|s| s == i) && i.chars().count() <= 80);
        let Some(id) = id else { return Err(err("categorical category ids must be unique bounded strings")) };
        let label =
            item.get("label").and_then(Value::as_str).filter(|l| !l.is_empty() && l.chars().count() <= 160);
        let Some(label) = label else {
            return Err(err("categorical category labels must be bounded strings"));
        };
        let rgb = item.get("color_rgb").and_then(Value::as_array).filter(|a| a.len() == 3);
        let rgb: Option<Vec<f64>> = rgb
            .and_then(|a| a.iter().map(|v| finite_number(v).filter(|x| (0.0..=255.0).contains(x))).collect());
        let Some(rgb) = rgb else {
            return Err(err("categorical category colors must be three RGB values in [0, 255]"));
        };
        seen.push(id.to_string());
        categories.push((id.to_string(), label.to_string(), [rgb[0], rgb[1], rgb[2]]));
    }
    let projection = match raw.get("projection") {
        None => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(err("categorical projection metadata must be an object")),
    };
    Ok(CategoricalPalette { thresholds, categories, projection })
}

fn python_float(s: &str) -> Option<f64> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if matches!(unsigned, "inf" | "infinity" | "nan") {
        let v = if unsigned == "nan" { f64::NAN } else { f64::INFINITY };
        return Some(if lower.starts_with('-') { -v } else { v });
    }
    if t.contains("__") || t.starts_with('_') || t.ends_with('_') {
        return None;
    }
    t.replace('_', "").parse().ok()
}

fn py_list(items: &[&str]) -> String {
    py_repr(&Value::Array(items.iter().map(|s| json!(s)).collect()))
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    if s == 0.0 {
        return [v, v, v];
    }
    let i = (h * 6.0).trunc();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match (i as i64).rem_euclid(6) {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn round6(x: f64) -> f64 {
    format!("{x:.6}").parse().unwrap_or(x)
}



pub fn categorical_palette_from_values(
    values: &[f64],
    maximum: usize,
) -> Result<CategoricalPalette, RenderError> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err(err("registered categorical ids must be a finite three-dimensional array"));
    }
    let mut unique = values.to_vec();
    unique.sort_by(f64::total_cmp);

    #[allow(clippy::float_cmp)]
    unique.dedup_by(|a, b| a == b);
    if unique.len() > maximum {
        return Err(err(format!(
            "registered categorical field has {} ids, above the bounded {maximum}-id palette limit; the declaring \
             source must provide a grouped palette",
            unique.len()
        )));
    }
    let thresholds: Vec<f64> = unique.windows(2).map(|p| p[0] + 0.5 * (p[1] - p[0])).collect();
    let categories: Vec<Value> = unique
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            let hue = (0.08 + 0.618_033_988_749_894_9 * index as f64).rem_euclid(1.0);
            let rgb = hsv_to_rgb(hue, 0.58, 0.86);
            let text = fmt_g(value, 12);
            json!({"id": format!("value:{text}"), "label": format!("ID {text}"),
                   "color_rgb": rgb.map(|c| round6(255.0 * c))})
        })
        .collect();
    normalise_categorical_palette(Some(&json!({
        "thresholds": thresholds, "categories": categories,
        "projection": {"kind": "registered_scalar_ids", "source": "unique_registered_values",
                       "exact_values": unique, "maximum_categories": maximum, "status": "render_only"},
    })))
}

fn number(
    value: Option<&Value>,
    label: &str,
    lower: Option<f64>,
    upper: Option<f64>,
) -> Result<f64, RenderError> {
    let v = value.and_then(finite_number).ok_or_else(|| err(format!("{label} must be a finite number")))?;
    if let Some(lo) = lower
        && v < lo
    {
        return Err(err(format!("{label} must be at least {}", fmt_g(lo, 6))));
    }
    if let Some(hi) = upper
        && v > hi
    {
        return Err(err(format!("{label} must be at most {}", fmt_g(hi, 6))));
    }
    Ok(v)
}

fn vector(value: Option<&Value>, label: &str, nonzero: bool) -> Result<[f64; 3], RenderError> {
    let arr = value.and_then(Value::as_array).filter(|a| a.len() == 3);
    let Some(arr) = arr else { return Err(err(format!("{label} must contain three numbers"))) };
    let mut out = [0.0; 3];
    for (i, v) in arr.iter().enumerate() {
        out[i] = number(Some(v), &format!("{label}[{i}]"), Some(-1_000_000.0), Some(1_000_000.0))?;
    }
    if nonzero && (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt() <= 1e-12 {
        return Err(err(format!("{label} must be nonzero")));
    }
    Ok(out)
}

fn bbox(value: Option<&Value>, label: &str) -> Result<[[f64; 3]; 2], RenderError> {
    let arr = value.and_then(Value::as_array).filter(|a| a.len() == 2);
    let Some(arr) = arr else { return Err(err(format!("{label} must be [[x0,y0,z0],[x1,y1,z1]]"))) };
    let out = [
        vector(Some(&arr[0]), &format!("{label}[0]"), false)?,
        vector(Some(&arr[1]), &format!("{label}[1]"), false)?,
    ];
    if (0..3).any(|a| out[1][a] <= out[0][a]) {
        return Err(err(format!("{label} upper bounds must exceed lower bounds")));
    }
    Ok(out)
}

fn rgb(value: Option<&Value>, label: &str) -> Result<[f64; 3], RenderError> {
    let arr = value.and_then(Value::as_array).filter(|a| a.len() == 3);
    let Some(arr) = arr else { return Err(err(format!("{label} must be three RGB values in [0, 255]"))) };
    let mut out = [0.0; 3];
    for (i, v) in arr.iter().enumerate() {
        out[i] = number(Some(v), &format!("{label}[{i}]"), Some(0.0), Some(255.0))?;
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColorStop {
    pub value: f64,
    pub color_rgb: [f64; 3],
}



pub fn normalise_color_stops(raw: &Value) -> Result<Vec<ColorStop>, RenderError> {
    let arr = raw.as_array().filter(|a| (2..=MAX_COLOR_STOPS).contains(&a.len()));
    let Some(arr) = arr else {
        return Err(err(format!("color_stops must be a list of 2 to {MAX_COLOR_STOPS} stops")));
    };
    let mut stops = Vec::with_capacity(arr.len());
    for (index, item) in arr.iter().enumerate() {
        let obj = item
            .as_object()
            .filter(|o| o.len() == 2 && o.contains_key("value") && o.contains_key("color_rgb"));
        let Some(obj) = obj else {
            return Err(err(format!("color_stops[{index}] requires exactly value and color_rgb")));
        };
        stops.push(ColorStop {
            value: number(obj.get("value"), &format!("color_stops[{index}].value"), Some(-1e12), Some(1e12))?,
            color_rgb: rgb(obj.get("color_rgb"), &format!("color_stops[{index}].color_rgb"))?,
        });
    }
    if stops.windows(2).any(|p| p[1].value <= p[0].value) {
        return Err(err("color_stops values must be strictly increasing"));
    }
    Ok(stops)
}

fn stops_json(stops: &[ColorStop]) -> Value {
    Value::Array(stops.iter().map(|s| json!({"value": s.value, "color_rgb": s.color_rgb})).collect())
}

fn field_name(
    value: Option<&Value>,
    label: &str,
    boundary: bool,
    artifact: bool,
) -> Result<String, RenderError> {
    let s = value.and_then(Value::as_str).filter(|s| !s.is_empty() && s.chars().count() <= 240);
    let Some(s) = s else { return Err(err(format!("{label} must be a bounded field name"))) };
    if boundary && s == "model_boundary" {
        if artifact {
            return Err(err("result_artifact surface_field must name a stored field"));
        }
        return Ok(s.to_string());
    }
    let ok = if artifact { is_artifact_field(s) } else { is_model_field(s) };
    if !ok {
        let suffix = if boundary { " or model_boundary" } else { "" };
        return Err(err(if artifact {
            format!("{label} must be a stored result-artifact field")
        } else {
            format!("{label} must be model[/child...]:parameter{suffix}")
        }));
    }
    Ok(s.to_string())
}

fn source(value: &Value) -> Result<Value, RenderError> {
    let Some(obj) = value.as_object() else { return Err(err("render 3d source must be an object")) };
    let kind = obj.get("kind").and_then(Value::as_str);
    if kind == Some("current_model") {
        if obj.len() != 1 {
            return Err(err("current_model source takes only its kind"));
        }
        return Ok(value.clone());
    }
    if kind == Some("result_artifact") {
        if obj.len() != 2 || !obj.contains_key("artifact_id") {
            return Err(err("result_artifact source requires exactly kind and artifact_id"));
        }
        let id = obj["artifact_id"].as_str().filter(|s| {
            s.len() == 39
                && s.starts_with("result-")
                && s[7..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        });
        if id.is_none() {
            return Err(err("result_artifact source requires a content-addressed artifact_id"));
        }
        return Ok(value.clone());
    }
    if kind == Some("optimization_epoch_field") {
        let required = ["kind", "job_id", "epoch", "field"];
        if !required.iter().all(|k| obj.contains_key(*k))
            || obj.keys().any(|k| !required.contains(&k.as_str()) && k != "operating_point")
            || !obj["job_id"].as_str().is_some_and(|j| j.len() == 12 && j.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
            || !obj["epoch"].as_i64().is_some_and(|e| e >= 0)
            || !obj["field"].as_str().is_some_and(|f| f.chars().count() <= 256 && is_artifact_field(f))
            || obj.get("operating_point").is_some_and(|v| !v.as_i64().is_some_and(|n| (0..=1_000_000).contains(&n)))
        {
            return Err(err("optimization_epoch_field requires a saved job, integer epoch, retained field and optional nonnegative operating_point"));
        }
        let mut normalized = obj.clone();
        normalized.entry("operating_point").or_insert(json!(0));
        return Ok(Value::Object(normalized));
    }
    let keys_ok = obj.len() == 3 && ["kind", "job_id", "epoch"].iter().all(|k| obj.contains_key(*k));
    if !matches!(kind, Some("current_optimization_state" | "optimization_epoch")) || !keys_ok {
        return Err(err("optimization source requires exactly kind, job_id, and epoch"));
    }
    let job_ok = obj["job_id"].as_str().is_some_and(|j| {
        j.len() == 12 && j.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    });
    if !job_ok {
        return Err(err(
            "render 3d optimization source requires a 12-character lowercase hexadecimal job_id",
        ));
    }
    let epoch = &obj["epoch"];
    let epoch_ok = if kind == Some("optimization_epoch") {
        epoch.as_i64().is_some_and(|e| e >= 0)
    } else {
        epoch.as_str() == Some("current") || epoch.is_u64() || epoch.as_i64().is_some_and(|e| e >= 0)
    };
    if !epoch_ok {
        return Err(err(if kind == Some("optimization_epoch") {
            "saved-epoch rendering requires a nonnegative integer epoch"
        } else { "render 3d epoch must be current or a nonnegative integer" }));
    }
    Ok(value.clone())
}

#[derive(Clone, Debug, PartialEq)]
pub enum CameraRequest {
    Preset {
        preset: String,
        fov_deg: f64,
    },
    Explicit {
        eye_mm: [f64; 3],
        target_mm: [f64; 3],
        up: [f64; 3],
        fov_deg: f64,
    },
}

impl CameraRequest {
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Preset { preset, fov_deg } => json!({"preset": preset, "fov_deg": fov_deg}),
            Self::Explicit { eye_mm, target_mm, up, fov_deg } => {
                json!({"eye_mm": eye_mm, "target_mm": target_mm, "up": up, "fov_deg": fov_deg})
            }
        }
    }

    #[must_use]
    pub fn fov_deg(&self) -> f64 {
        match self {
            Self::Preset { fov_deg, .. } | Self::Explicit { fov_deg, .. } => *fov_deg,
        }
    }
}

fn camera(value: &Value) -> Result<CameraRequest, RenderError> {
    let Some(obj) = value.as_object() else { return Err(err("render 3d camera must be an object")) };
    let fov =
        number(Some(obj.get("fov_deg").unwrap_or(&json!(35.0))), "camera fov_deg", Some(15.0), Some(90.0))?;
    if obj.contains_key("preset") {
        if obj.keys().any(|k| k != "preset" && k != "fov_deg") {
            return Err(err("preset camera has unknown fields"));
        }
        let preset = obj["preset"].as_str().filter(|p| CAMERA_PRESETS.contains(p));
        let Some(preset) = preset else {
            return Err(err(format!("camera preset must be one of {}", CAMERA_PRESETS.join(", "))));
        };
        return Ok(CameraRequest::Preset { preset: preset.to_string(), fov_deg: fov });
    }
    if obj.keys().any(|k| !["eye_mm", "target_mm", "up", "fov_deg"].contains(&k.as_str())) {
        return Err(err("explicit camera has unknown fields"));
    }
    if !["eye_mm", "target_mm", "up"].iter().all(|k| obj.contains_key(*k)) {
        return Err(err("explicit camera requires eye_mm, target_mm, and up"));
    }
    let eye = vector(obj.get("eye_mm"), "camera eye_mm", false)?;
    let target = vector(obj.get("target_mm"), "camera target_mm", false)?;
    let up = vector(obj.get("up"), "camera up", true)?;
    let forward = sub(target, eye);
    if norm(forward) <= 1e-9 {
        return Err(err("camera eye_mm and target_mm must differ"));
    }
    if norm(cross(forward, up)) <= 1e-9 * norm(forward) {
        return Err(err("camera up must not be parallel to its view"));
    }
    Ok(CameraRequest::Explicit { eye_mm: eye, target_mm: target, up, fov_deg: fov })
}

#[derive(Clone, Debug, PartialEq)]
pub struct CapRequest {
    pub inside: String,
    pub complement_color_rgb: Option<[f64; 3]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipPlane {
    pub point_mm: [f64; 3],
    pub normal: [f64; 3],
    pub keep_negative: bool,
    pub cap: Option<CapRequest>,
}

impl ClipPlane {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut m = json!({"point_mm": self.point_mm, "normal": self.normal,
                           "keep": if self.keep_negative { "negative" } else { "positive" }});
        if let Some(cap) = &self.cap {
            let mut c = json!({"inside": cap.inside});
            if let Some(rgb) = cap.complement_color_rgb {
                c["complement_color_rgb"] = json!(rgb);
            }
            m["cap"] = c;
        }
        m
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Render3dRequest {
    pub source: Value,
    pub surface_field: String,
    pub iso_value: f64,
    pub color_field: Option<String>,
    pub palette: String,
    pub value_range: Option<[f64; 2]>,
    pub color_stops: Option<Vec<ColorStop>>,
    pub color_categories: Option<CategoricalPalette>,
    pub surface_color_rgb: Option<[f64; 3]>,
    pub region: String,
    pub camera: CameraRequest,
    pub crop_mm: Option<[[f64; 3]; 2]>,
    pub clip: Option<ClipPlane>,
    pub width_px: usize,
    pub height_px: usize,
    pub quality: String,
    pub antialias: usize,
    pub background: String,
}

impl Render3dRequest {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source, "surface_field": self.surface_field, "iso_value": self.iso_value,
            "color_field": self.color_field, "palette": self.palette, "value_range": self.value_range,
            "color_stops": self.color_stops.as_ref().map(|s| stops_json(s)),
            "color_categories": self.color_categories.as_ref().map(CategoricalPalette::to_json),
            "surface_color_rgb": self.surface_color_rgb, "region": self.region,
            "camera": self.camera.to_json(), "crop_mm": self.crop_mm,
            "clip": self.clip.as_ref().map(ClipPlane::to_json),
            "width_px": self.width_px, "height_px": self.height_px, "quality": self.quality,
            "antialias": self.antialias, "presentation_smoothing": "none", "background": self.background,
        })
    }
}

fn int_in(value: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    value.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64).filter(|v| (lo..=hi).contains(v))
}



#[allow(clippy::too_many_lines)]
pub fn normalise_request(raw: &Value) -> Result<Render3dRequest, RenderError> {
    let Some(obj) = raw.as_object() else { return Err(err("render 3d payload must be an object")) };
    let allowed = [
        "source",
        "surface_field",
        "iso_value",
        "color_field",
        "palette",
        "value_range",
        "camera",
        "crop_mm",
        "clip",
        "width_px",
        "height_px",
        "quality",
        "presentation_smoothing",
        "background",
        "antialias",
        "surface_color_rgb",
        "color_stops",
        "color_categories",
        "region",
    ];
    let mut unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !allowed.contains(k)).collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(err(format!("render 3d has unknown fields {}", py_list(&unknown))));
    }
    let side = |key: &str, default: i64| -> Result<i64, RenderError> {
        let v = obj.get(key).cloned().unwrap_or(json!(default));
        int_in(Some(&v), MIN_IMAGE_SIDE_PX, MAX_IMAGE_SIDE_PX).ok_or_else(|| {
            err(format!("render 3d {key} must be an integer in [{MIN_IMAGE_SIDE_PX}, {MAX_IMAGE_SIDE_PX}]"))
        })
    };
    let width = side("width_px", 512)?;
    let height = side("height_px", 384)?;
    if width * height > MAX_IMAGE_PIXELS {
        return Err(err(format!("render 3d image exceeds the {MAX_IMAGE_PIXELS}-pixel bound")));
    }
    let antialias = int_in(Some(obj.get("antialias").unwrap_or(&json!(1))), 1, 3)
        .ok_or_else(|| err("render 3d antialias must be 1, 2, or 3"))?;
    if (width * height * antialias * antialias) as usize > MAX_RASTER_PIXELS {
        return Err(err(format!(
            "render 3d supersampled raster ({}x{}) exceeds the {MAX_RASTER_PIXELS}-pixel bound; reduce antialias or \
             the image size",
            width * antialias,
            height * antialias
        )));
    }
    let quality = obj.get("quality").map_or(Some("standard"), Value::as_str);
    let quality = quality
        .filter(|q| quality_preset(q).is_some() || is_native_quality(q) || geometry_refinement(q).is_some());
    let Some(quality) = quality else {
        return Err(err(
            "render 3d quality must be preview, draft, standard, high, geometry, geometry_fine, native, or analysis",
        ));
    };
    let source = source(obj.get("source").unwrap_or(&json!({"kind": "current_model"})))?;
    let artifact = matches!(source["kind"].as_str(), Some("result_artifact" | "optimization_epoch_field"));
    let surface = field_name(
        Some(obj.get("surface_field").unwrap_or(&json!("model_boundary"))),
        "surface_field",
        true,
        artifact,
    )?;
    if is_native_quality(quality) {
        let native_current = matches!(source["kind"].as_str(), Some("current_model" | "optimization_epoch"))
            && surface != "model_boundary";
        if !artifact && !native_current {
            return Err(err(
                "render 3d native/analysis quality requires either an immutable registered result_artifact field or \
                 an explicitly declared current-model or saved-epoch native analysis field",
            ));
        }
    }
    let default_iso = if surface == "model_boundary" { 0.0 } else { 0.5 };
    let iso = number(
        Some(obj.get("iso_value").unwrap_or(&json!(default_iso))),
        "iso_value",
        Some(-1e12),
        Some(1e12),
    )?;
    let colour = match obj.get("color_field").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(field_name(Some(v), "color_field", false, artifact)?),
    };
    if colour.is_none()
        && ["palette", "value_range", "color_stops", "color_categories"].iter().any(|k| obj.contains_key(*k))
    {
        return Err(err(
            "render 3d palette, value_range, color_stops and color_categories require color_field",
        ));
    }
    let surface_colour = match obj.get("surface_color_rgb").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(rgb(Some(v), "surface_color_rgb")?),
    };
    if surface_colour.is_some() && colour.is_some() {
        return Err(err("render 3d surface_color_rgb is a uniform colour; it excludes color_field"));
    }
    let mut palette = obj.get("palette").map_or(Some("auto"), Value::as_str).unwrap_or("").to_string();
    if !["auto", "sequential", "diverging", "categorical"].contains(&palette.as_str()) {
        return Err(err("render 3d palette must be auto, sequential, diverging, or categorical"));
    }
    let stops = match obj.get("color_stops").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(normalise_color_stops(v)?),
    };
    let categories = match obj.get("color_categories").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let mut c = normalise_categorical_palette(Some(v))?;
            c.projection.insert("source".into(), json!("render_request"));
            c.projection.insert("status".into(), json!("render_only"));
            Some(c)
        }
    };
    if stops.is_some() && categories.is_some() {
        return Err(err("render 3d color_stops and color_categories are alternatives"));
    }
    if (stops.is_some() || categories.is_some())
        && (obj.contains_key("palette") || obj.contains_key("value_range"))
    {
        return Err(err(
            "explicit color_stops/color_categories replace palette and value_range; omit those fields",
        ));
    }
    if stops.is_some() {
        palette = "stops".into();
    } else if categories.is_some() {
        palette = "categorical".into();
    }
    let value_range = match obj.get("value_range").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let arr = v.as_array().filter(|a| a.len() == 2);
            let Some(arr) = arr else {
                return Err(err("render 3d value_range must be two increasing numbers"));
            };
            let a = number(Some(&arr[0]), "value_range[0]", Some(-1e12), Some(1e12))?;
            let b = number(Some(&arr[1]), "value_range[1]", Some(-1e12), Some(1e12))?;
            if b <= a {
                return Err(err("render 3d value_range must be two increasing numbers"));
            }
            Some([a, b])
        }
    };
    let crop = match obj.get("crop_mm").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(bbox(Some(v), "crop_mm")?),
    };
    let clip = match obj.get("clip").filter(|v| !v.is_null()) {
        None => None,
        Some(c) => {
            let Some(c) = c.as_object() else { return Err(err("render 3d clip must be an object")) };
            if c.keys().any(|k| !["point_mm", "normal", "keep", "cap"].contains(&k.as_str())) {
                return Err(err("render 3d clip has unknown fields"));
            }
            if !c.contains_key("point_mm") || !c.contains_key("normal") {
                return Err(err("render 3d clip requires point_mm and normal"));
            }
            let n = vector(c.get("normal"), "clip normal", true)?;
            let len = norm(n);
            let normal = [n[0] / len, n[1] / len, n[2] / len];
            let keep = c.get("keep").map_or(Some("negative"), Value::as_str);
            let keep_negative = match keep {
                Some("negative") => true,
                Some("positive") => false,
                _ => return Err(err("render 3d clip keep must be negative or positive")),
            };
            let point = vector(c.get("point_mm"), "clip point_mm", false)?;
            let cap = match c.get("cap").filter(|v| !v.is_null()) {
                None => None,
                Some(cap) => {
                    let cap = cap
                        .as_object()
                        .filter(|o| o.keys().all(|k| k == "inside" || k == "complement_color_rgb"));
                    let Some(cap) = cap else {
                        return Err(err("render 3d clip cap accepts only inside and complement_color_rgb"));
                    };
                    let inside = cap.get("inside").map_or(Some("auto"), Value::as_str);
                    let inside = inside.filter(|i| ["auto", "below_iso", "above_iso"].contains(i));
                    let Some(inside) = inside else {
                        return Err(err("render 3d clip cap inside must be auto, below_iso or above_iso"));
                    };
                    let complement = match cap.get("complement_color_rgb").filter(|v| !v.is_null()) {
                        None => None,
                        Some(v) => Some(rgb(Some(v), "clip cap complement_color_rgb")?),
                    };
                    Some(CapRequest { inside: inside.to_string(), complement_color_rgb: complement })
                }
            };
            Some(ClipPlane { point_mm: point, normal, keep_negative, cap })
        }
    };
    if obj.get("presentation_smoothing").is_some_and(|v| v.as_str() != Some("none")) {
        return Err(err(
            "presentation_smoothing currently accepts only none; geometry is never moved silently",
        ));
    }
    let background = obj.get("background").map_or(Some("dark"), Value::as_str);
    let background = background.filter(|b| *b == "dark" || *b == "white");
    let Some(background) = background else { return Err(err("render 3d background must be dark or white")) };
    let region = obj.get("region").map_or(Some("solid"), Value::as_str);
    let region = region.filter(|r| *r == "solid" || *r == "complement");
    let Some(region) = region else { return Err(err("render 3d region must be solid or complement")) };
    if region == "complement" && surface != "model_boundary" {
        return Err(err("render 3d complement region requires surface_field model_boundary"));
    }
    Ok(Render3dRequest {
        source,
        surface_field: surface,
        iso_value: iso,
        color_field: colour,
        palette,
        value_range,
        color_stops: stops,
        color_categories: categories,
        surface_color_rgb: surface_colour,
        region: region.to_string(),
        camera: camera(obj.get("camera").unwrap_or(&json!({"preset": "isometric"})))?,
        crop_mm: crop,
        clip,
        width_px: width as usize,
        height_px: height as usize,
        quality: quality.to_string(),
        antialias: antialias as usize,
        background: background.to_string(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualityPolicy {
    pub max_samples: Option<usize>,
    pub max_triangles: usize,
}

#[must_use]
pub fn quality_policy(quality: &str) -> QualityPolicy {
    if geometry_refinement(quality).is_some() {
        return QualityPolicy {
            max_samples: Some(GEOMETRY_MAX_SAMPLES),
            max_triangles: GEOMETRY_MAX_TRIANGLES,
        };
    }
    if is_native_quality(quality) {
        return QualityPolicy { max_samples: None, max_triangles: NATIVE_RESULT_MAX_TRIANGLES };
    }
    let q = quality_preset(quality).unwrap_or(QUALITY[2].1);
    QualityPolicy { max_samples: Some(q.max_samples), max_triangles: q.max_triangles }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpacingHint {
    pub spacing_mm: f64,
    pub source: Option<String>,
    pub declared_by: Vec<String>,
}

fn shape_for(span: [f64; 3], target: f64) -> [usize; 3] {
    span.map(|s| (((s / target - 1e-9).ceil() as i64) + 1).max(3) as usize)
}

fn resolution_shape(
    span: [f64; 3],
    quality: &str,
    hint: Option<&SpacingHint>,
) -> Result<([usize; 3], Value), RenderError> {
    let refinement = geometry_refinement(quality).unwrap_or(1);
    let mut record = Map::new();
    record.insert("refinement".into(), json!(refinement));
    if let Some(hint) = hint {
        let declared = number(Some(&json!(hint.spacing_mm)), "declared sampling spacing", Some(1e-9), None)?;
        let target = declared / refinement as f64;
        record
            .insert("spacing_source".into(), json!(hint.source.clone().unwrap_or_else(|| "declared".into())));
        record.insert("declared_spacing_mm".into(), json!(declared));
        record.insert("target_spacing_mm".into(), json!(target));
        if !hint.declared_by.is_empty() {
            record.insert("declared_by".into(), json!(hint.declared_by));
        }
        let shape = shape_for(span, target);
        let samples: usize = shape.iter().product();
        if samples > GEOMETRY_MAX_SAMPLES {
            return Err(err(format!(
                "render 3d {quality} sampling needs {samples} samples at {} mm, above the explicit \
                 {GEOMETRY_MAX_SAMPLES}-sample bound; crop the view, use quality geometry instead of geometry_fine, \
                 or a named preset",
                fmt_g(target, 6)
            )));
        }
        record.insert("budget_limited".into(), json!(false));
        return Ok((shape, Value::Object(record)));
    }
    let axis_samples = GEOMETRY_ANALYTIC_AXIS_SAMPLES * refinement;
    let longest = span.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut target = longest / (axis_samples - 1) as f64;
    let mut shape = shape_for(span, target);
    let limited = shape.iter().product::<usize>() > GEOMETRY_MAX_SAMPLES;
    while shape.iter().product::<usize>() > GEOMETRY_MAX_SAMPLES {
        target *= 1.02;
        shape = shape_for(span, target);
    }
    record.insert("spacing_source".into(), json!("analytic_default_longest_axis"));
    record.insert("declared_spacing_mm".into(), Value::Null);
    record.insert("target_spacing_mm".into(), json!(target));
    record.insert("analytic_longest_axis_samples".into(), json!(axis_samples));
    record.insert("budget_limited".into(), json!(limited));
    Ok((shape, Value::Object(record)))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SamplingGrid {
    pub axes: [Vec<f64>; 3],
    pub shape: [usize; 3],
    pub record: Value,
}

impl SamplingGrid {
    #[must_use]
    pub fn points(&self) -> Vec<[f64; 3]> {
        let mut out = Vec::with_capacity(self.shape.iter().product());
        for &x in &self.axes[0] {
            for &y in &self.axes[1] {
                for &z in &self.axes[2] {
                    out.push([x, y, z]);
                }
            }
        }
        out
    }
}



pub fn sampling_grid(
    bbox_mm: [[f64; 3]; 2],
    quality: &str,
    hint: Option<&SpacingHint>,
) -> Result<SamplingGrid, RenderError> {
    if is_native_quality(quality) {
        return Err(err("native/analysis sampling requires a registered result array"));
    }
    let span: [f64; 3] = std::array::from_fn(|a| bbox_mm[1][a] - bbox_mm[0][a]);
    if bbox_mm.iter().flatten().any(|v| !v.is_finite()) || span.iter().any(|s| *s <= 0.0) {
        return Err(err("render 3d sampling bounds are invalid"));
    }
    let (shape, resolution, max_axis, max_samples) = if geometry_refinement(quality).is_some() {
        let (shape, res) = resolution_shape(span, quality, hint)?;
        (shape, Some(res), None, GEOMETRY_MAX_SAMPLES)
    } else {
        let policy = quality_preset(quality).ok_or_else(|| err("unknown render quality"))?;
        let maximum = policy.max_axis_samples;
        let longest = span.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut shape: [usize; 3] = span.map(|s| {
            let n = (((maximum - 1) as f64 * s / longest).ceil() as i64 + 1).max(3) as usize;
            n.min(maximum)
        });
        while shape.iter().product::<usize>() > policy.max_samples {
            let candidates: Vec<usize> = (0..3).filter(|&a| shape[a] > 3).collect();
            if candidates.is_empty() {
                return Err(err("render 3d sampling policy cannot fit the box"));
            }

            let mut axis = candidates[0];
            for &a in &candidates {
                if shape[a] > shape[axis] {
                    axis = a;
                }
            }
            shape[axis] -= 1;
        }
        (shape, None, Some(maximum), policy.max_samples)
    };
    let axes: [Vec<f64>; 3] = std::array::from_fn(|a| linspace(bbox_mm[0][a], bbox_mm[1][a], shape[a]));
    let spacing: [f64; 3] = std::array::from_fn(|a| span[a] / (shape[a] - 1) as f64);
    let mut record = json!({
        "quality": quality,
        "sampling_mode": if resolution.is_some() { "source_resolution_uniform_resample" } else { "bounded_uniform_resample" },
        "registered_native_values_used": false,
        "shape": shape,
        "samples": shape.iter().product::<usize>(),
        "bbox_mm": bbox_mm,
        "spacing_mm": spacing,
        "max_spacing_mm": spacing.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "max_samples": max_samples,
        "max_axis_samples": max_axis,
    });
    if let Some(r) = resolution {
        record["resolution"] = r;
    }
    Ok(SamplingGrid { axes, shape, record })
}

#[must_use]
pub fn sample_index_grid(values: &Field3<'_>, c: [f64; 3]) -> f64 {
    let shape = values.shape;
    let mut lo = [0usize; 3];
    let mut hi = [0usize; 3];
    let mut f = [0.0; 3];
    for a in 0..3 {
        let top = (shape[a] - 1) as f64;
        let clipped = c[a].max(0.0).min(top);
        let l = (clipped.floor().max(0.0) as usize).min(shape[a].saturating_sub(2));
        lo[a] = l;
        hi[a] = (l + 1).min(shape[a] - 1);
        f[a] = clipped - l as f64;
    }
    let v = |i: usize, j: usize, k: usize| values.at(i, j, k);
    let (u, w1, w) = (f[0], f[1], f[2]);
    let c00 = v(lo[0], lo[1], lo[2]) * (1.0 - u) + v(hi[0], lo[1], lo[2]) * u;
    let c10 = v(lo[0], hi[1], lo[2]) * (1.0 - u) + v(hi[0], hi[1], lo[2]) * u;
    let c01 = v(lo[0], lo[1], hi[2]) * (1.0 - u) + v(hi[0], lo[1], hi[2]) * u;
    let c11 = v(lo[0], hi[1], hi[2]) * (1.0 - u) + v(hi[0], hi[1], hi[2]) * u;
    (c00 * (1.0 - w1) + c10 * w1) * (1.0 - w) + (c01 * (1.0 - w1) + c11 * w1) * w
}

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

const CUBE_CORNERS: [[usize; 3]; 8] =
    [[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0], [0, 0, 1], [1, 0, 1], [0, 1, 1], [1, 1, 1]];
const CUBE_TETRAHEDRA: [[usize; 4]; 6] =
    [[0, 1, 3, 7], [0, 3, 2, 7], [0, 2, 6, 7], [0, 6, 4, 7], [0, 4, 5, 7], [0, 5, 1, 7]];

fn tet_case_polygons(case: usize) -> Vec<[(usize, usize); 3]> {
    let inside: Vec<usize> = (0..4).filter(|i| case & (1 << i) != 0).collect();
    let outside: Vec<usize> = (0..4).filter(|i| case & (1 << i) == 0).collect();
    match inside.len() {
        0 | 4 => vec![],
        1 => {
            let a = inside[0];
            vec![[(a, outside[0]), (a, outside[1]), (a, outside[2])]]
        }
        3 => {
            let a = outside[0];
            vec![[(a, inside[0]), (a, inside[2]), (a, inside[1])]]
        }
        _ => {
            let (a, b, c, d) = (inside[0], inside[1], outside[0], outside[1]);
            let q = [(a, c), (a, d), (b, d), (b, c)];
            vec![[q[0], q[1], q[2]], [q[0], q[2], q[3]]]
        }
    }
}

type TriangleSoup = (Vec<[f64; 3]>, Vec<[usize; 3]>);

fn marching_tetrahedra(
    array: &Field3<'_>,
    level: f64,
    max_triangles: usize,
) -> Result<TriangleSoup, RenderError> {
    let [n0, n1, n2] = array.shape;
    let mut cells = Vec::new();
    for i in 0..n0 - 1 {
        for j in 0..n1 - 1 {
            for k in 0..n2 - 1 {
                let mut low = f64::INFINITY;
                let mut high = f64::NEG_INFINITY;
                for c in CUBE_CORNERS {
                    let v = array.at(i + c[0], j + c[1], k + c[2]);
                    low = low.min(v);
                    high = high.max(v);
                }
                if low < level && high >= level {
                    cells.push([i, j, k]);
                }
            }
        }
    }
    let mut vertices: Vec<[f64; 3]> = Vec::new();
    let mut count = 0usize;
    for tetra in CUBE_TETRAHEDRA {
        let offsets = tetra.map(|t| CUBE_CORNERS[t]);
        let tet_values: Vec<[f64; 4]> =
            cells.iter().map(|c| offsets.map(|o| array.at(c[0] + o[0], c[1] + o[1], c[2] + o[2]))).collect();
        let cases: Vec<usize> =
            tet_values.iter().map(|v| (0..4).filter(|&q| v[q] < level).map(|q| 1 << q).sum()).collect();
        for case in 1..15 {
            let polys = tet_case_polygons(case);
            if polys.is_empty() {
                continue;
            }
            let selected: Vec<usize> = (0..cells.len()).filter(|&i| cases[i] == case).collect();
            if selected.is_empty() {
                continue;
            }
            for poly in &polys {
                let mut kept = Vec::new();
                for &s in &selected {
                    let cell = cells[s];
                    let vals = tet_values[s];
                    let tri: [[f64; 3]; 3] = poly.map(|(la, lb)| {
                        let (ca, cb) = (offsets[la], offsets[lb]);
                        let (va, vb) = (vals[la], vals[lb]);
                        let den = vb - va;
                        let frac = if den.abs() > 1e-30 { (level - va) / den } else { 0.5 };
                        let frac = frac.clamp(0.0, 1.0);
                        std::array::from_fn(|a| {
                            (cell[a] + ca[a]) as f64 + frac * (cb[a] as f64 - ca[a] as f64)
                        })
                    });
                    let area2 = norm(cross(sub(tri[1], tri[0]), sub(tri[2], tri[0])));
                    if area2 > 1e-14 {
                        kept.push(tri);
                    }
                }
                if kept.is_empty() {
                    continue;
                }
                count += kept.len();
                if count > max_triangles {
                    return Err(err(format!(
                        "render 3d mesh exceeds the quality triangle bound {max_triangles}; reduce quality or crop \
                         the view"
                    )));
                }
                vertices.extend(kept.into_iter().flatten());
            }
        }
    }
    let faces = (0..vertices.len() / 3).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect();
    Ok((vertices, faces))
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderMesh {
    pub vertices: Vec<[f64; 3]>,
    pub normals: Vec<[f64; 3]>,
    pub faces: Vec<[usize; 3]>,
}



pub fn extract_isosurface(
    values: &Field3<'_>,
    bbox_mm: [[f64; 3]; 2],
    iso_value: f64,
    max_triangles: usize,
) -> Result<(RenderMesh, Value), RenderError> {
    if values.shape.iter().any(|&n| n < 2) || values.data.iter().any(|v| !v.is_finite()) {
        return Err(err("render 3d surface samples must be a finite three-dimensional grid"));
    }
    let (lower, upper) = values.min_max();
    if !(lower < iso_value && iso_value <= upper) {
        return Err(err(format!(
            "iso_value does not cross the sampled field (range {} to {})",
            fmt_g(lower, 8),
            fmt_g(upper, 8)
        )));
    }
    let (vertices_i, faces, extractor, interpolation): (Vec<[f64; 3]>, Vec<[usize; 3]>, &str, &str) =
        if iso_value < upper {
            let mc = marching_cubes(values, iso_value, GradientDirection::Descent, false)
                .map_err(|e| err(e.to_string()))?;
            (
                mc.vertices.iter().map(|v| v.map(f64::from)).collect(),
                mc.faces.iter().map(|f| f.map(|i| i as usize)).collect(),
                "service_marching_cubes",
                "linear_edge_interpolation",
            )
        } else {
            let (v, f) = marching_tetrahedra(values, iso_value, max_triangles)?;
            (v, f, "builtin_marching_tetrahedra", "piecewise_linear_tetrahedral")
        };
    if faces.is_empty() {
        return Err(err("render 3d extraction produced no triangles"));
    }
    if faces.len() > max_triangles {
        return Err(err(format!(
            "render 3d mesh has {} triangles, above the quality bound {max_triangles}; reduce quality or crop the view",
            faces.len()
        )));
    }
    let spacing: [f64; 3] =
        std::array::from_fn(|a| (bbox_mm[1][a] - bbox_mm[0][a]) / (values.shape[a] - 1) as f64);
    let vertices: Vec<[f64; 3]> =
        vertices_i.iter().map(|v| std::array::from_fn(|a| bbox_mm[0][a] + v[a] * spacing[a])).collect();
    let gradients: [Vec<f64>; 3] =
        std::array::from_fn(|a| gradient3(values.data, values.shape, spacing[a], a));
    let mut normals: Vec<[f64; 3]> = vertices_i
        .iter()
        .map(|v| {
            std::array::from_fn(|a| {
                sample_index_grid(&Field3 { shape: values.shape, data: &gradients[a] }, *v)
            })
        })
        .collect();
    let missing: Vec<bool> = normals.iter().map(|n| norm(*n) <= 1e-12).collect();
    if missing.iter().any(|&m| m) {
        let mut acc = vec![[0.0; 3]; vertices.len()];
        for f in &faces {
            let t = f.map(|i| vertices[i]);
            let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
            for &i in f {
                for a in 0..3 {
                    acc[i][a] += n[a];
                }
            }
        }
        for (i, m) in missing.iter().enumerate() {
            if *m {
                normals[i] = acc[i];
            }
        }
    }
    for n in &mut normals {
        let l = norm(*n);
        let d = if l > 1e-12 { l } else { 1.0 };
        *n = n.map(|c| c / d);
    }
    let record = json!({
        "sampled_value_range": [lower, upper],
        "vertices": vertices.len(),
        "triangles_before_clip": faces.len(),
        "normal_method": "trilinear_interpolated_scalar_gradient",
        "normal_direction": "toward_increasing_scalar",
        "surface_interpolation": interpolation,
        "geometry_smoothing": "none",
        "extractor": extractor,
    });
    Ok((RenderMesh { vertices, normals, faces }, record))
}



#[allow(clippy::type_complexity)]
pub fn clip_mesh(
    mesh: &RenderMesh,
    attributes: Option<&[f64]>,
    clip: Option<&ClipPlane>,
    max_triangles: usize,
) -> Result<(RenderMesh, Option<Vec<f64>>), RenderError> {
    let Some(clip) = clip else { return Ok((mesh.clone(), attributes.map(<[f64]>::to_vec))) };
    let sign = if clip.keep_negative { 1.0 } else { -1.0 };
    let distance: Vec<f64> =
        mesh.vertices.iter().map(|v| sign * dot(sub(*v, clip.point_mm), clip.normal)).collect();
    let inside: Vec<bool> = distance.iter().map(|d| *d <= 1e-10).collect();
    let mut polys: Vec<Vec<([f64; 3], [f64; 3], f64)>> = Vec::new();
    for f in &mesh.faces {
        let mut out = Vec::with_capacity(4);
        for corner in 0..3 {
            let previous = (corner + 2) % 3;
            let (a, b) = (f[previous], f[corner]);
            if inside[b] != inside[a] {
                let den = distance[a] - distance[b];
                let t = if den.abs() <= 1e-20 { 0.5 } else { distance[a] / den };
                let t = t.clamp(0.0, 1.0);
                let p: [f64; 3] = std::array::from_fn(|q| {
                    mesh.vertices[a][q] + t * (mesh.vertices[b][q] - mesh.vertices[a][q])
                });
                let n: [f64; 3] = std::array::from_fn(|q| {
                    mesh.normals[a][q] + t * (mesh.normals[b][q] - mesh.normals[a][q])
                });
                let at = attributes.map_or(0.0, |x| x[a] + t * (x[b] - x[a]));
                out.push((p, n, at));
            }
            if inside[b] {
                out.push((mesh.vertices[b], mesh.normals[b], attributes.map_or(0.0, |x| x[b])));
            }
        }
        polys.push(out);
    }
    if !polys.iter().any(|p| p.len() >= 3) {
        return Err(err("the clip plane removes the complete rendered surface"));
    }
    let tri_count: usize = polys.iter().map(|p| p.len().saturating_sub(2)).sum();
    if tri_count > max_triangles {
        return Err(err("clipped render mesh exceeds the quality triangle bound"));
    }
    let mut out = RenderMesh { vertices: Vec::new(), normals: Vec::new(), faces: Vec::new() };
    let mut attrs = Vec::new();
    for p in polys.iter().filter(|p| p.len() >= 3) {
        for fan in 1..p.len() - 1 {
            for &c in &[0, fan, fan + 1] {
                let (pos, n, a) = p[c];
                let l = norm(n);
                let d = if l > 1e-12 { l } else { 1.0 };
                out.vertices.push(pos);
                out.normals.push(n.map(|x| x / d));
                attrs.push(a);
            }
            let base = out.vertices.len() - 3;
            out.faces.push([base, base + 1, base + 2]);
        }
    }
    Ok((out, attributes.map(|_| attrs)))
}

fn bbox_corners(b: [[f64; 3]; 2]) -> [[f64; 3]; 8] {
    let mut out = [[0.0; 3]; 8];
    let mut i = 0;
    for x in 0..2 {
        for y in 0..2 {
            for z in 0..2 {
                out[i] = [b[x][0], b[y][1], b[z][2]];
                i += 1;
            }
        }
    }
    out
}

fn normalise(v: [f64; 3]) -> [f64; 3] {
    let n = norm(v);
    v.map(|c| c / n)
}

fn frame(eye: [f64; 3], target: [f64; 3], up: [f64; 3]) -> [[f64; 3]; 3] {
    let forward = normalise(sub(target, eye));
    let right = normalise(cross(forward, up));
    let true_up = normalise(cross(right, forward));
    [right, true_up, forward]
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCamera {
    pub eye_mm: [f64; 3],
    pub target_mm: [f64; 3],
    pub basis: [[f64; 3]; 3],
    pub fov_deg: f64,
    pub record: Value,
}



pub fn resolve_camera(
    request: &CameraRequest,
    bbox_mm: [[f64; 3]; 2],
    width: usize,
    height: usize,
) -> Result<ResolvedCamera, RenderError> {
    let centre: [f64; 3] = std::array::from_fn(|a| 0.5 * (bbox_mm[0][a] + bbox_mm[1][a]));
    let fov = request.fov_deg();
    let aspect = width as f64 / height as f64;
    let (eye, target, up, preset) = match request {
        CameraRequest::Preset { preset, .. } => {
            let (dir, up): ([f64; 3], [f64; 3]) = match preset.as_str() {
                "front" => ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
                "rear" => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
                "left" => ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                "right" => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                "top" => ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
                "bottom" => ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
                _ => ([1.0, -1.0, 0.78], [0.0, 0.0, 1.0]),
            };
            let direction = normalise(dir);
            let provisional: [f64; 3] = std::array::from_fn(|a| centre[a] + direction[a]);
            let basis = frame(provisional, centre, up);
            let tangent = (fov.to_radians() * 0.5).tan();
            let mut required = f64::NEG_INFINITY;
            let (mut r1, mut r2) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            for c in bbox_corners(bbox_mm) {
                let rel = sub(c, centre);
                let cam = [dot(rel, basis[0]), dot(rel, basis[1]), dot(rel, basis[2])];
                r1 = r1.max(cam[1].abs() / tangent - cam[2]);
                r2 = r2.max(cam[0].abs() / (tangent * aspect) - cam[2]);
            }
            required = required.max(r1).max(r2);
            let diag = norm(sub(bbox_mm[1], bbox_mm[0]));
            let distance = (required + 0.025 * diag).max(1e-6);
            let eye: [f64; 3] = std::array::from_fn(|a| centre[a] + direction[a] * distance);
            (eye, centre, up, Some(preset.clone()))
        }
        CameraRequest::Explicit { eye_mm, target_mm, up, .. } => (*eye_mm, *target_mm, *up, None),
    };
    let basis = frame(eye, target, up);
    let depths: Vec<f64> = bbox_corners(bbox_mm).iter().map(|c| dot(sub(*c, eye), basis[2])).collect();
    let scale = norm(sub(bbox_mm[1], bbox_mm[0])).max(1.0);
    let near = depths.iter().copied().fold(f64::INFINITY, f64::min);
    let far = depths.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if near <= 1e-8f64.max(1e-8 * scale) {
        return Err(err("the camera must place the complete render crop in front of its eye"));
    }
    let record = json!({
        "projection": "perspective", "preset": preset,
        "eye_mm": eye, "target_mm": target, "up": basis[1], "fov_deg": fov,
        "near_depth_mm": near, "far_depth_mm": far,
    });
    Ok(ResolvedCamera { eye_mm: eye, target_mm: target, basis, fov_deg: fov, record })
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColourSpec {
    Stops(Vec<ColorStop>),
    Categorical(CategoricalPalette, Option<[f64; 2]>),
    Ramp {
        palette: String,
        value_range: Option<[f64; 2]>,
    },
}



pub fn colour_vertices(values: &[f64], spec: &ColourSpec) -> Result<(Vec<[f64; 3]>, Value), RenderError> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return Err(err("render 3d color values must be finite scalars"));
    }
    match spec {
        ColourSpec::Stops(stops) => {
            let xp: Vec<f64> = stops.iter().map(|s| s.value).collect();
            let channels: [Vec<f64>; 3] =
                std::array::from_fn(|c| stops.iter().map(|s| s.color_rgb[c]).collect());
            let colours =
                values.iter().map(|&v| std::array::from_fn(|c| interp(v, &xp, &channels[c]))).collect();
            Ok((
                colours,
                json!({"display_range": [xp[0], xp[xp.len() - 1]], "palette": "stops",
                       "color_stops": stops_json(stops),
                       "interpolation": "piecewise_linear_srgb_clamped_at_end_stops"}),
            ))
        }
        ColourSpec::Categorical(palette, value_range) => {
            let (lower, upper) = extent_range(values, *value_range);
            let colours = values.iter().map(|&v| palette.colour(v)).collect();
            Ok((
                colours,
                json!({"display_range": [lower, upper], "palette": "categorical",
                       "categorical": palette.to_json(), "display_projection": palette.projection}),
            ))
        }
        ColourSpec::Ramp { palette, value_range } => {
            let (lower, upper) = extent_range(values, *value_range);
            let ramp: &[[f64; 3]] = if palette == "diverging" { &DIVERGING } else { &SEQUENTIAL };
            let colours = values
                .iter()
                .map(|&v| {
                    let position = ((v - lower) / (upper - lower)).clamp(0.0, 1.0) * (ramp.len() - 1) as f64;
                    let lo = position.floor() as usize;
                    let hi = (lo + 1).min(ramp.len() - 1);
                    let f = position - lo as f64;
                    std::array::from_fn(|c| ramp[lo][c] * (1.0 - f) + ramp[hi][c] * f)
                })
                .collect();
            Ok((colours, json!({"display_range": [lower, upper], "palette": palette})))
        }
    }
}

fn extent_range(values: &[f64], value_range: Option<[f64; 2]>) -> (f64, f64) {
    if let Some([a, b]) = value_range {
        return (a, b);
    }
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if hi <= lo {
        let pad = 1e-12f64.max(lo.abs() * 1e-6);
        return (lo - pad, hi + pad);
    }
    (lo, hi)
}



pub fn resolve_colour_mapping(
    request: &Render3dRequest,
    descriptor: &Value,
) -> Result<(ColourSpec, &'static str), RenderError> {
    if let Some(stops) = &request.color_stops {
        return Ok((ColourSpec::Stops(stops.clone()), "request_color_stops"));
    }
    if let Some(c) = &request.color_categories {
        return Ok((ColourSpec::Categorical(c.clone(), request.value_range), "request_color_categories"));
    }
    let mut palette = request.palette.clone();
    if palette == "auto" {
        palette = descriptor["suggested_palette"].as_str().unwrap_or("sequential").to_string();
    }
    if palette == "categorical" {
        let spec = normalise_categorical_palette(descriptor.get("categorical"))?;
        return Ok((ColourSpec::Categorical(spec, request.value_range), "source_or_named_palette"));
    }
    Ok((ColourSpec::Ramp { palette, value_range: request.value_range }, "source_or_named_palette"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapState {
    None,
    Solid,
    Complement,
}

pub type CapClassifier<'a> = Box<dyn Fn(&[[f64; 3]]) -> Vec<CapState> + 'a>;
pub type CapColour<'a> = Box<dyn Fn(&[[f64; 3]]) -> Result<(Vec<[f64; 3]>, Vec<bool>), RenderError> + 'a>;

pub struct SectionCap<'a> {
    pub point_mm: [f64; 3],
    pub normal: [f64; 3],
    pub bounds_mm: [[f64; 3]; 2],
    pub classify: CapClassifier<'a>,
    pub solid_colour: CapColour<'a>,
    pub complement_rgb: Option<[f64; 3]>,
    pub record: Value,
}



#[allow(clippy::too_many_arguments)]
pub fn grid_section_cap<'a>(
    values: &'a Field3<'a>,
    bbox_mm: [[f64; 3]; 2],
    iso_value: f64,
    inside: &str,
    complement_bounds_mm: [[f64; 3]; 2],
    clip: &ClipPlane,
    solid_colour: CapColour<'a>,
    complement_rgb: Option<[f64; 3]>,
) -> Result<SectionCap<'a>, RenderError> {
    let below = match inside {
        "below_iso" => true,
        "above_iso" => false,
        _ => return Err(err("section cap inside must be below_iso or above_iso")),
    };
    let spacing: [f64; 3] =
        std::array::from_fn(|a| (bbox_mm[1][a] - bbox_mm[0][a]) / (values.shape[a] as f64 - 1.0));
    let cbox: [[f64; 3]; 2] = [
        std::array::from_fn(|a| complement_bounds_mm[0][a].max(bbox_mm[0][a])),
        std::array::from_fn(|a| complement_bounds_mm[1][a].min(bbox_mm[1][a])),
    ];
    if (0..3).any(|a| cbox[1][a] <= cbox[0][a]) {
        return Err(err("the section cap complement bounds do not overlap the sampled grid"));
    }
    let largest = bbox_mm.iter().flatten().map(|v| v.abs()).fold(0.0f64, f64::max);
    let tolerance = 1e-9 * largest.max(1.0);
    let has_complement = complement_rgb.is_some();
    let classify = move |points: &[[f64; 3]]| {
        points
            .iter()
            .map(|p| {
                let c: [f64; 3] = std::array::from_fn(|a| (p[a] - bbox_mm[0][a]) / spacing[a]);
                let s = sample_index_grid(values, c);
                let solid = if below { s < iso_value } else { s >= iso_value };
                if solid {
                    CapState::Solid
                } else if has_complement
                    && (0..3).all(|a| p[a] >= cbox[0][a] - tolerance && p[a] <= cbox[1][a] + tolerance)
                {
                    CapState::Complement
                } else {
                    CapState::None
                }
            })
            .collect()
    };
    Ok(SectionCap {
        point_mm: clip.point_mm,
        normal: clip.normal,
        bounds_mm: bbox_mm,
        classify: Box::new(classify),
        solid_colour,
        complement_rgb,
        record: json!({
            "mode": "flat_plane_fill_before_first_kept_surface", "inside": inside, "iso_value": iso_value,
            "solid_bounds_mm": bbox_mm, "complement_bounds_mm": cbox,
            "classification": "trilinear_sampled_surface_grid",
            "complement_color_rgb": complement_rgb,
        }),
    })
}

const KEY_LIGHT: [f64; 3] = [-0.420_758_047_342_952, 0.621_119_022_268_167_3, -0.661_191_217_253_210_4];
const FILL_LIGHT: [f64; 3] = [0.680_374_308_833_110_1, -0.180_099_081_749_940_88, -0.710_390_822_458_100_1];

fn key_light() -> [f64; 3] {
    KEY_LIGHT
}

fn fill_light() -> [f64; 3] {
    FILL_LIGHT
}

fn studio_shade(base: [f64; 3], n: [f64; 3], view: [f64; 3], section: bool) -> [f64; 3] {
    let facing = dot(n, view);
    let n = if facing < 0.0 { n.map(|c| -c) } else { n };
    let facing = facing.abs();
    let kl = key_light();
    let key = if section {
        let k: [f64; 3] = std::array::from_fn(|a| 0.15 * kl[a] + 0.85 * view[a]);
        let l = norm(k);
        k.map(|c| c / l)
    } else {
        kl
    };
    let diffuse = dot(n, key).max(0.0);
    let secondary = dot(n, fill_light()).max(0.0);
    let half: [f64; 3] = std::array::from_fn(|a| key[a] + view[a]);
    let hl = norm(half);
    let half = half.map(|c| c / if hl > 1e-12 { hl } else { 1.0 });
    let specular = if section { 0.0 } else { dot(n, half).max(0.0).powf(32.0) };
    let rim = (1.0 - facing.clamp(0.0, 1.0)).powi(2);
    let light = (0.28 + 0.68 * diffuse + 0.18 * secondary + 0.13 * rim).clamp(0.0, 1.25);
    std::array::from_fn(|a| {
        let linear = (base[a].clamp(0.0, 255.0) / 255.0).powf(2.2);
        let shaded = (linear * light + 0.23 * specular).clamp(0.0, 1.0);
        255.0 * shaded.powf(1.0 / 2.2)
    })
}

struct Setup {
    p: [[f64; 2]; 3],
    z: [f64; 3],
    den: f64,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

pub enum VertexColouring<'a> {
    Uniform,
    Colours(&'a [[f64; 3]]),
    Categorical {
        values: &'a [f64],
        palette: &'a CategoricalPalette,
    },
}

pub struct RasterOptions<'a> {
    pub width: usize,
    pub height: usize,
    pub camera: &'a ResolvedCamera,
    pub colouring: VertexColouring<'a>,
    pub background: &'a str,
    pub supersample: usize,
    pub base_colour: Option<[f64; 3]>,
    pub cap: Option<&'a SectionCap<'a>>,
}

fn silhouette(mask: &[bool], h: usize, w: usize, thickness: usize) -> Vec<bool> {
    let mut inner = mask.to_vec();
    for _ in 0..thickness {
        let prev = inner.clone();
        for r in 0..h {
            for c in 0..w {
                let i = r * w + c;
                inner[i] = prev[i]
                    && prev[((r + h - 1) % h) * w + c]
                    && prev[((r + 1) % h) * w + c]
                    && prev[r * w + (c + w - 1) % w]
                    && prev[r * w + (c + 1) % w];
            }
        }
        for c in 0..w {
            inner[c] = false;
            inner[(h - 1) * w + c] = false;
        }
        for r in 0..h {
            inner[r * w] = false;
            inner[r * w + w - 1] = false;
        }
    }
    mask.iter().zip(&inner).map(|(m, i)| *m && !*i).collect()
}



#[allow(clippy::too_many_lines)]
pub fn rasterize(
    mesh: &RenderMesh,
    opts: &RasterOptions<'_>,
) -> Result<(raster::RgbImage, Value), RenderError> {
    let white = match opts.background {
        "dark" => false,
        "white" => true,
        _ => return Err(err("render 3d background must be dark or white")),
    };
    if !(1..=3).contains(&opts.supersample) {
        return Err(err("render 3d supersampling must be 1, 2, or 3"));
    }
    let k = opts.supersample;
    let (rw, rh) = (opts.width * k, opts.height * k);
    if rw * rh > MAX_RASTER_PIXELS {
        return Err(err(format!("render 3d raster exceeds the {MAX_RASTER_PIXELS}-pixel bound")));
    }
    let uniform = opts.base_colour.unwrap_or(BASE_COLOUR);
    if uniform.iter().any(|c| !c.is_finite()) {
        return Err(err("render 3d base colour must be three finite values"));
    }
    let cam = opts.camera;
    let basis = cam.basis;
    let eye = cam.eye_mm;
    let to_cam = |v: [f64; 3]| {
        let r = sub(v, eye);
        [dot(r, basis[0]), dot(r, basis[1]), dot(r, basis[2])]
    };
    let cv: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| to_cam(*v)).collect();
    if cv.iter().any(|v| v[2] <= 0.0) {
        return Err(err("render mesh crosses the camera plane"));
    }
    let tangent = (cam.fov_deg.to_radians() * 0.5).tan();
    let aspect = rw as f64 / rh as f64;
    let projected: Vec<[f64; 2]> = cv
        .iter()
        .map(|v| {
            let ndc_x = v[0] / (v[2] * tangent * aspect);
            let ndc_y = v[1] / (v[2] * tangent);
            [(ndc_x + 1.0) * 0.5 * (rw - 1) as f64, (1.0 - ndc_y) * 0.5 * (rh - 1) as f64]
        })
        .collect();
    let ncam: Vec<[f64; 3]> =
        mesh.normals.iter().map(|n| [dot(*n, basis[0]), dot(*n, basis[1]), dot(*n, basis[2])]).collect();
    if let VertexColouring::Categorical { values, .. } = &opts.colouring
        && (values.len() != mesh.vertices.len() || values.iter().any(|v| !v.is_finite()))
    {
        return Err(err("categorical raster values must be one finite scalar per vertex"));
    }

    let mut setups: Vec<Option<Setup>> = Vec::with_capacity(mesh.faces.len());
    let mut tile_tests = 0usize;
    for f in &mesh.faces {
        let p = f.map(|i| projected[i]);
        let z = f.map(|i| cv[i][2]);
        let lower = [p[0][0].min(p[1][0]).min(p[2][0]), p[0][1].min(p[1][1]).min(p[2][1])];
        let upper = [p[0][0].max(p[1][0]).max(p[2][0]), p[0][1].max(p[1][1]).max(p[2][1])];
        let on_screen =
            !(upper[0] < 0.0 || lower[0] > (rw - 1) as f64 || upper[1] < 0.0 || lower[1] > (rh - 1) as f64);
        let x0 = lower[0].floor().max(0.0) as i64;
        let x1 = (upper[0].ceil().min((rw - 1) as f64)) as i64;
        let y0 = lower[1].floor().max(0.0) as i64;
        let y1 = (upper[1].ceil().min((rh - 1) as f64)) as i64;
        let den = (p[1][1] - p[2][1]) * (p[0][0] - p[2][0]) + (p[2][0] - p[1][0]) * (p[0][1] - p[2][1]);
        let active = on_screen && x1 >= x0 && y1 >= y0 && den.abs() > 1e-12;
        if active {
            tile_tests += ((x1 - x0 + 1) * (y1 - y0 + 1)) as usize;
            setups.push(Some(Setup {
                p,
                z,
                den,
                x0: x0 as usize,
                x1: x1 as usize,
                y0: y0 as usize,
                y1: y1 as usize,
            }));
        } else {
            setups.push(None);
        }
    }
    if tile_tests > MAX_RASTER_TILE_SAMPLES {
        return Err(err(
            "projected render work exceeds the deterministic tile-pixel budget; use a framing preset, crop, lower \
             quality or antialias",
        ));
    }
    let weights = |s: &Setup, xs: f64, ys: f64| {
        let p = &s.p;
        let w0 = ((p[1][1] - p[2][1]) * (xs - p[2][0]) + (p[2][0] - p[1][0]) * (ys - p[2][1])) / s.den;
        let w1 = ((p[2][1] - p[0][1]) * (xs - p[2][0]) + (p[0][0] - p[2][0]) * (ys - p[2][1])) / s.den;
        (w0, w1, 1.0 - w0 - w1)
    };

    let mut depth = vec![f64::INFINITY; rw * rh];
    let mut owner = vec![usize::MAX; rw * rh];
    for (tri, s) in setups.iter().enumerate() {
        let Some(s) = s else { continue };
        for py in s.y0..=s.y1 {
            for px in s.x0..=s.x1 {
                let (w0, w1, w2) = weights(s, px as f64 + 0.5, py as f64 + 0.5);
                if !(w0 >= -1e-9 && w1 >= -1e-9 && w2 >= -1e-9) {
                    continue;
                }
                let inverse = w0 / s.z[0] + w1 / s.z[1] + w2 / s.z[2];
                if inverse <= 0.0 {
                    continue;
                }
                let candidate = 1.0 / inverse;
                let pixel = py * rw + px;
                if candidate < depth[pixel] {
                    depth[pixel] = candidate;
                    owner[pixel] = tri;
                }
            }
        }
    }

    let pixel_ray = |row: usize, col: usize| {
        let ndc_x = 2.0 * (col as f64 + 0.5) / (rw - 1) as f64 - 1.0;
        let ndc_y = 1.0 - 2.0 * (row as f64 + 0.5) / (rh - 1) as f64;
        [ndc_x * tangent * aspect, ndc_y * tangent, 1.0]
    };
    let cap_hit = |cap: &SectionCap<'_>, row: usize, col: usize| {
        let ray = pixel_ray(row, col);
        let world: [f64; 3] =
            std::array::from_fn(|a| ray[0] * basis[0][a] + ray[1] * basis[1][a] + ray[2] * basis[2][a]);
        let den = dot(world, cap.normal);
        let num = dot(sub(cap.point_mm, eye), cap.normal);
        let safe = den.abs() > 1e-12;
        let d = if safe { num / den } else { -1.0 };
        let point: [f64; 3] = std::array::from_fn(|a| eye[a] + d * world[a]);
        let largest = cap.bounds_mm.iter().flatten().map(|v| v.abs()).fold(0.0f64, f64::max);
        let tol = 1e-9 * largest.max(1.0);
        let valid = safe
            && d > 0.0
            && (0..3).all(|a| point[a] >= cap.bounds_mm[0][a] - tol && point[a] <= cap.bounds_mm[1][a] + tol);
        (d, point, ray, valid)
    };
    let mut cap_state: Option<Vec<CapState>> = None;
    if let Some(cap) = opts.cap {
        let mut state = vec![CapState::None; rw * rh];
        let mut pts = Vec::new();
        let mut where_ = Vec::new();
        for row in 0..rh {
            for col in 0..rw {
                let (d, point, _ray, valid) = cap_hit(cap, row, col);
                if valid && d <= depth[row * rw + col] * (1.0 + 1e-9) {
                    pts.push(point);
                    where_.push(row * rw + col);
                }
            }
        }
        if !pts.is_empty() {
            for (i, s) in where_.iter().zip((cap.classify)(&pts)) {
                state[*i] = s;
            }
        }
        cap_state = Some(state);
    }
    let mask: Vec<bool> = (0..rw * rh)
        .map(|i| owner[i] != usize::MAX || cap_state.as_ref().is_some_and(|c| c[i] != CapState::None))
        .collect();
    let covered = mask.iter().filter(|&&m| m).count();
    if covered < 16 * k * k {
        return Err(err("the camera produced fewer than 16 surface pixels; use a framing preset"));
    }
    let mut sil = silhouette(&mask, rh, rw, k);
    if let Some(state) = &cap_state {
        let cap_mask: Vec<bool> = state.iter().map(|s| *s != CapState::None).collect();
        for (s, c) in sil.iter_mut().zip(silhouette(&cap_mask, rh, rw, k)) {
            *s |= c;
        }
    }
    let edge_factor = if white { 0.52 } else { 0.70 };
    let (top, bottom) = if white { ([255.0; 3], [255.0; 3]) } else { ([17.0, 25.0, 38.0], [5.0, 8.0, 13.0]) };
    let ramp = linspace(0.0, 1.0, rh);
    let mut image = vec![[0.0f64; 3]; rw * rh];
    for row in 0..rh {
        let yy = ramp[row];
        let bg: [f64; 3] = std::array::from_fn(|a| top[a] * (1.0 - yy) + bottom[a] * yy);
        for col in 0..rw {
            image[row * rw + col] = bg;
        }
    }
    let mut solid_px = 0usize;
    let mut complement_px = 0usize;
    for row in 0..rh {
        for col in 0..rw {
            let i = row * rw + col;
            let on_cap = cap_state.as_ref().is_some_and(|c| c[i] != CapState::None);
            if owner[i] != usize::MAX && !on_cap {
                let face = mesh.faces[owner[i]];
                let Some(s) = &setups[owner[i]] else { continue };
                let (w0, w1, w2) = weights(s, col as f64 + 0.5, row as f64 + 0.5);
                let inverse = w0 / s.z[0] + w1 / s.z[1] + w2 / s.z[2];
                let corr = [w0 / s.z[0] / inverse, w1 / s.z[1] / inverse, w2 / s.z[2] / inverse];
                let mut n = [0.0; 3];
                let mut position = [0.0; 3];
                for a in 0..3 {
                    n[a] =
                        corr[0] * ncam[face[0]][a] + corr[1] * ncam[face[1]][a] + corr[2] * ncam[face[2]][a];
                    position[a] =
                        corr[0] * cv[face[0]][a] + corr[1] * cv[face[1]][a] + corr[2] * cv[face[2]][a];
                }
                let nl = norm(n);
                let n = n.map(|c| c / if nl > 1e-12 { nl } else { 1.0 });
                let view = position.map(|c| -c);
                let vl = norm(view);
                let view = view.map(|c| c / if vl > 1e-12 { vl } else { 1.0 });
                let base = match &opts.colouring {
                    VertexColouring::Uniform => uniform,
                    VertexColouring::Colours(colours) => std::array::from_fn(|a| {
                        corr[0] * colours[face[0]][a]
                            + corr[1] * colours[face[1]][a]
                            + corr[2] * colours[face[2]][a]
                    }),
                    VertexColouring::Categorical { values, palette } => {
                        let scalar =
                            corr[0] * values[face[0]] + corr[1] * values[face[1]] + corr[2] * values[face[2]];
                        palette.colour(scalar)
                    }
                };
                image[i] = studio_shade(base, n, view, false);
            }
            if on_cap && let Some(cap) = opts.cap {
                let state = cap_state.as_ref().map_or(CapState::None, |c| c[i]);
                let (_d, point, ray, _valid) = cap_hit(cap, row, col);
                let base = if state == CapState::Solid {
                    let (colours, valid) = (cap.solid_colour)(&[point])?;
                    if colours.len() != 1 || colours[0].iter().any(|c| !c.is_finite()) {
                        return Err(err("section cap colours must be finite RGB rows"));
                    }
                    if !valid[0] {
                        return Err(err("the color field does not cover the section cap"));
                    }
                    solid_px += 1;
                    colours[0]
                } else {
                    complement_px += 1;
                    cap.complement_rgb.unwrap_or([0.0; 3])
                };
                let rl = norm(ray);
                let view = ray.map(|c| -c / rl);
                let n = [dot(basis[0], cap.normal), dot(basis[1], cap.normal), dot(basis[2], cap.normal)];
                image[i] = studio_shade(base, n, view, true);
            }
            if sil[i] {
                image[i] = image[i].map(|c| c * edge_factor);
            }
            image[i] = image[i].map(|c| c.clamp(0.0, 255.0));
        }
    }
    let mut out = raster::RgbImage::filled(opts.width, opts.height, [0, 0, 0]);
    if k == 1 {
        for row in 0..rh {
            for col in 0..rw {
                out.set(row, col, image[row * rw + col].map(implexity_mesh::numeric::to_u8));
            }
        }
    } else {
        let kk = (k * k) as f64;
        for row in 0..opts.height {
            for col in 0..opts.width {
                let mut acc = [0.0; 3];
                for dy in 0..k {
                    for dx in 0..k {
                        let px = image[(row * k + dy) * rw + col * k + dx];
                        for a in 0..3 {
                            acc[a] += (px[a] / 255.0).powf(2.2);
                        }
                    }
                }
                let v = acc.map(|s| (255.0 * (s / kk).powf(1.0 / 2.2)).clamp(0.0, 255.0));
                out.set(row, col, v.map(implexity_mesh::numeric::to_u8));
            }
        }
    }
    let record = json!({
        "renderer": "deterministic_cpu_triangle_rasterizer",
        "rasterizer": "vectorized_face_ordered_zbuffer_deferred_shading",
        "shading": "smooth_gradient_normals_two_sided_studio",
        "background": opts.background,
        "background_rgb": if white { json!([255, 255, 255]) } else { json!([17, 25, 38]) },
        "categorical_interpolation": if matches!(opts.colouring, VertexColouring::Categorical { .. }) {
            json!("threshold_after_perspective_scalar_interpolation") } else { Value::Null },
        "supersample": k,
        "raster_width_px": rw, "raster_height_px": rh,
        "downsample_filter": if k == 1 { Value::Null } else { json!("box_mean_in_linear_light_gamma_2_2") },
        "silhouette_width_raster_px": k,
        "surface_base_color_rgb": opts.base_colour,
        "section_cap": opts.cap.map(|cap| {
            let mut r = cap.record.clone();
            r["solid_raster_pixels"] = json!(solid_px);
            r["complement_raster_pixels"] = json!(complement_px);
            r["shading"] = json!("flat_plane_normal_viewer_facing_key_no_specular");
            r
        }),
        "covered_pixels": covered,
        "covered_fraction": covered as f64 / (rw * rh) as f64,
        "tile_pixel_tests": tile_tests,
        "max_tile_pixel_tests": MAX_RASTER_TILE_SAMPLES,
        "max_raster_pixels": MAX_RASTER_PIXELS,
        "width_px": opts.width, "height_px": opts.height,
    });
    Ok((out, record))
}
