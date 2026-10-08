// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::Path;

use implexity_io::npy::NpyArray;
use serde_json::{Map, Value, json};

use crate::shaders::ShaderSources;

pub const MAX_EGL_RASTER_PIXELS: usize = 16_777_216;
pub const MAX_EGL_RASTER_SIDE_PX: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerError {
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Runtime(String),
    #[error("{0}")]
    Key(String),
}

impl WorkerError {
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            Self::Value(_) => "ValueError",
            Self::Runtime(_) => "RuntimeError",
            Self::Key(_) => "KeyError",
        }
    }
}

fn value(m: impl Into<String>) -> WorkerError {
    WorkerError::Value(m.into())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Texture {
        unit: u32,
        name: &'static str,
        shape: [usize; 3],
        values: Vec<f32>,
    },
    Float(&'static str, Vec<f32>),
    Int(&'static str, i32),
    Mat3(&'static str, [f32; 9]),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    pub viewport: [usize; 2],
    pub raster: [usize; 2],
    pub supersample: usize,
    pub shape: [usize; 3],
    pub steps: Vec<Step>,
    pub fixed_step_mm: f64,
    pub surface: String,
    pub section_fill: Value,
    pub field_range: [f32; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Readback {
    pub gl_renderer: String,
    pub gl_version: String,
    pub pixels: Vec<u8>,
    pub steps: Vec<f32>,
}

fn f(x: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let y = x as f32;
    y
}

fn floats(xs: &[f64]) -> Vec<f32> {
    xs.iter().copied().map(f).collect()
}

fn num(v: &Value, what: &str) -> Result<f64, WorkerError> {
    v.as_f64().ok_or_else(|| value(format!("scene {what} is not a number")))
}

fn vec3(v: &Value, what: &str) -> Result<[f64; 3], WorkerError> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or_else(|| value(format!("scene {what} is not a 3-vector")))?;
    Ok([num(&a[0], what)?, num(&a[1], what)?, num(&a[2], what)?])
}

fn py_int(v: &Value) -> Option<i64> {
    if v.is_i64() || v.is_u64() { v.as_i64() } else { None }
}

fn as_f32(array: &NpyArray) -> Result<(Vec<usize>, Vec<f32>), WorkerError> {
    let a = array.to_f64().ok_or_else(|| value("invalid finite scalar grid"))?;
    let shape = a.shape().to_vec();
    Ok((shape, a.iter().copied().map(f).collect()))
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}



#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
pub fn plan(
    field: &NpyArray,
    color: Option<&NpyArray>,
    scene: &Value,
    width: &Value,
    height: &Value,
    sample_shape: Option<&Value>,
) -> Result<RenderPlan, WorkerError> {
    let s = implexity_render::viewer_scene::validate_scene(scene).map_err(|e| value(e.to_string()))?;
    let camera = &s["camera"];
    if s["bbox_mm"].is_null()
        || ["target_mm", "distance_mm", "vertical_span_mm"].iter().any(|k| camera.get(k).is_none())
    {
        return Err(value("render requires a fully bound native scene"));
    }
    let k = usize::try_from(py_int(&s["supersample"]).unwrap_or(1)).unwrap_or(1);
    let (Some(w), Some(h)) = (py_int(width), py_int(height)) else {
        return Err(value("invalid offscreen raster size"));
    };
    let (Ok(out_w), Ok(out_h)) = (usize::try_from(w), usize::try_from(h)) else {
        return Err(value("invalid offscreen raster size"));
    };
    if out_w < 1
        || out_h < 1
        || out_w.saturating_mul(out_h).saturating_mul(k * k) > MAX_EGL_RASTER_PIXELS
        || out_w.max(out_h).saturating_mul(k) > MAX_EGL_RASTER_SIDE_PX
    {
        return Err(value("invalid offscreen raster size"));
    }
    let q = py_int(&s["sample_count"]).unwrap_or(0);
    let shape = implexity_render::viewer_scene::scene_sample_shape(&s["bbox_mm"], q)
        .map_err(|e| value(e.to_string()))?;
    if let Some(declared) = sample_shape.filter(|v| !v.is_null()) {
        #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
        let same = declared.as_array().is_some_and(|a| {
            a.len() == 3 && a.iter().zip(shape).all(|(v, n)| v.as_f64().is_some_and(|x| x == n as f64))
        });
        if !same {
            return Err(value("packet sample shape disagrees with the scene"));
        }
    }
    let (field_shape, field) = as_f32(field)?;
    if field_shape != shape || field.iter().any(|x| !x.is_finite()) {
        return Err(value("invalid finite scalar grid"));
    }
    if color.is_none() != s["color"].is_null() {
        return Err(value("color field/scene disagreement"));
    }
    let color = match color {
        None => None,
        Some(c) => {
            let (color_shape, values) = as_f32(c).map_err(|_| value("invalid color field"))?;
            if color_shape != shape || values.iter().any(|x| !x.is_finite()) {
                return Err(value("invalid color field"));
            }
            Some(values)
        }
    };
    let (width, height) = (out_w * k, out_h * k);
    let bbox = s["bbox_mm"].as_array().map_or(&[][..], Vec::as_slice);
    let (lo, hi) = (vec3(&bbox[0], "bbox")?, vec3(&bbox[1], "bbox")?);
    let span: [f64; 3] = std::array::from_fn(|i| hi[i] - lo[i]);
    #[allow(clippy::cast_precision_loss)]
    let shape_f: [f64; 3] = shape.map(|n| n as f64);
    let mut steps = Vec::new();
    let field_range = [
        field.iter().copied().fold(f32::INFINITY, f32::min),
        field.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    ];
    steps.push(Step::Texture { unit: 1, name: "u_sampled", shape, values: field });

    steps.push(Step::Float("u_sampled_lo", floats(&lo)));
    steps.push(Step::Float("u_sampled_hi", floats(&hi)));
    steps.push(Step::Float("u_sampled_dim", floats(&[shape_f[2], shape_f[1], shape_f[0]])));
    let surface = s["surface"].as_str().unwrap_or("solid").to_owned();
    steps.push(Step::Float("u_surfaceSign", vec![if surface == "solid" { 1.0 } else { -1.0 }]));
    let clip = s["clip"].as_object().filter(|c| !c.is_empty());
    steps.push(Step::Int("u_cutOn", i32::from(!s["clip"].is_null())));
    if let Some(c) = clip {
        steps.push(Step::Float("u_cutN", floats(&vec3(&c["normal"], "clip normal")?)));
        steps.push(Step::Float("u_cutD", vec![f(num(&c["offset_mm"], "clip offset")?)]));
    }

    let cap = clip.and_then(|c| c.get("cap")).filter(|v| !v.is_null()).cloned();
    steps.push(Step::Int("u_sectionFillOn", i32::from(cap.is_some())));
    if let (Some(cap), Some(c)) = (&cap, clip) {
        steps.push(Step::Float("u_sectionN", floats(&vec3(&c["normal"], "clip normal")?)));
        steps.push(Step::Float("u_sectionD", vec![f(num(&c["offset_mm"], "clip offset")?)]));
        steps.push(Step::Float("u_sectionLo", floats(&lo)));
        steps.push(Step::Float("u_sectionHi", floats(&hi)));
        steps.push(Step::Float("u_sectionFill", floats(&vec3(&cap["complement_color_rgb"], "cap RGB")?)));
    }
    let base = s.get("surface_color_rgb").filter(|v| !v.is_null());
    steps.push(Step::Int("u_baseColourOn", i32::from(base.is_some())));
    if let Some(b) = base {
        steps.push(Step::Float("u_baseColour", floats(&vec3(b, "surface RGB")?)));
    }
    let colour_on = color.is_some();
    steps.push(Step::Texture {
        unit: 0,
        name: "u_field",
        shape: if colour_on { shape } else { [2, 2, 2] },
        values: color.unwrap_or_else(|| vec![0.0; 8]),
    });
    steps.push(Step::Int("u_colorOn", i32::from(colour_on)));
    steps.push(Step::Int("u_fieldRegistered", i32::from(colour_on)));
    if colour_on {
        let cfg = &s["color"];
        steps.push(Step::Float("u_fieldLo", floats(&lo)));
        steps.push(Step::Float("u_fieldHi", floats(&hi)));
        steps.push(Step::Float("u_fieldDim", floats(&shape_f)));
        let range = cfg["range"].as_array().map_or(&[][..], Vec::as_slice);
        let range = range.iter().map(|v| num(v, "field range")).collect::<Result<Vec<f64>, _>>()?;
        steps.push(Step::Float("u_fieldRange", floats(&range)));
        let palette = match cfg["palette"].as_str() {
            Some("sequential") => 0,
            Some("diverging") => 1,
            Some("two_color") => 2,
            Some("two_category") => 3,
            other => return Err(value(format!("unsupported palette {other:?}"))),
        };
        steps.push(Step::Int("u_fieldPalette", palette));
        let default = json!([[0, 0, 0], [1, 1, 1]]);
        let colours = cfg.get("colors_rgb").unwrap_or(&default);
        steps.push(Step::Float("u_fieldColorLow", floats(&vec3(&colours[0], "RGB")?)));
        steps.push(Step::Float("u_fieldColorHigh", floats(&vec3(&colours[1], "RGB")?)));
        let threshold = cfg.get("threshold").map_or(Ok(0.5), |t| num(t, "threshold"))?;
        steps.push(Step::Float("u_fieldThreshold", vec![f(threshold)]));
        let inv: [f32; 3] = std::array::from_fn(|i| f((shape_f[i] - 1.0) / span[i]));
        let mut m = [0.0f32; 9];
        for i in 0..3 {
            m[4 * i] = inv[i];
        }
        steps.push(Step::Mat3("u_fieldWorldToIndex", m));
        let offset: [f64; 3] = std::array::from_fn(|i| -f64::from(inv[i]) * lo[i]);
        steps.push(Step::Float("u_fieldIndexOffset", floats(&offset)));
        steps.push(Step::Float("u_fieldIndexDim", floats(&shape_f)));
    }
    let yaw = num(&camera["yaw_rad"], "yaw")?;
    let pitch = num(&camera["pitch_rad"], "pitch")?;
    let (cp, sp, cy, sy) = (pitch.cos(), pitch.sin(), yaw.cos(), yaw.sin());
    let direction = [cp * cy, cp * sy, sp];
    let fwd = direction.map(|x| -x);
    let right = [-sy, cy, 0.0];
    let up = [-sp * cy, -sp * sy, cp];
    let target = vec3(&camera["target_mm"], "camera target")?;
    let distance = num(&camera["distance_mm"], "camera distance")?;
    let eye: [f64; 3] = std::array::from_fn(|i| target[i] + distance * direction[i]);
    let diag = norm(span);
    let cell = (0..3).map(|i| span[i] / (shape_f[i] - 1.0)).fold(f64::INFINITY, f64::min);
    let eps = cell * 0.25;
    let quality = usize::try_from(py_int(&s["quality"]).unwrap_or(2)).unwrap_or(2).min(2);
    let nsteps: i32 = [56, 110, 220][quality];
    let far = distance + 2.0 * diag;
    let near = (distance - 1.05 * diag).max(0.0);

    #[allow(clippy::manual_midpoint)]
    let centre: [f64; 3] = std::array::from_fn(|i| (lo[i] + hi[i]) / 2.0);
    for (name, v) in [("u_eye", eye), ("u_fwd", fwd), ("u_right", right), ("u_up", up), ("u_centre", centre)]
    {
        steps.push(Step::Float(name, floats(&v)));
    }
    #[allow(clippy::cast_precision_loss)]
    steps.push(Step::Float("u_res", floats(&[width as f64, height as f64])));
    let fov = num(&camera["fov_deg"], "fov")?;
    steps.push(Step::Float("u_tanHalfFov", vec![f((fov * std::f64::consts::PI / 360.0).tan())]));
    steps.push(Step::Float("u_orthoSpan", vec![f(num(&camera["vertical_span_mm"], "vertical span")?)]));
    steps.push(Step::Int("u_orthographic", i32::from(camera["projection"] == "orthographic")));
    steps.push(Step::Int("u_backgroundWhite", i32::from(s["background"] == "white")));

    let first = (far - near) / f64::from((nsteps - 2).max(8));
    let fixed_step = if eps * 2.0 > first { eps * 2.0 } else { first };
    steps.push(Step::Int("u_traceFixed", 1));
    steps.push(Step::Float("u_stepFactor", vec![1.0]));
    steps.push(Step::Float("u_t0", vec![f(near)]));
    steps.push(Step::Float("u_fixedStep", vec![f(fixed_step)]));
    steps.push(Step::Float("u_eps", vec![f(eps)]));
    steps.push(Step::Float("u_epsGrowth", vec![1.0]));
    steps.push(Step::Float("u_tmax", vec![f(far)]));
    steps.push(Step::Int("u_maxSteps", nsteps));
    steps.push(Step::Float("u_scale", vec![f(diag)]));
    steps.push(Step::Int("u_clipOn", 0));
    steps.push(Step::Int("u_shadows", i32::from(quality == 2)));
    steps.push(Step::Int("u_ao", i32::from(quality >= 1)));
    steps.push(Step::Int("u_groundOn", 1));
    steps.push(Step::Float("u_groundZ", vec![f(lo[2] - 0.012 * diag)]));
    let mut light: [f64; 3] = std::array::from_fn(|i| -0.30 * fwd[i] - 0.66 * right[i] + 0.60 * up[i]);
    light[2] += 0.18;
    let n = norm(light);
    steps.push(Step::Float("u_lightDir", floats(&light.map(|x| x / n))));
    Ok(RenderPlan {
        viewport: [out_w, out_h],
        raster: [width, height],
        supersample: k,
        shape,
        steps,
        fixed_step_mm: fixed_step,
        surface,
        section_fill: cap.unwrap_or(Value::Null),
        field_range,
    })
}

#[must_use]
pub fn flipud(pixels: &[u8], row_len: usize) -> Vec<u8> {
    pixels.chunks_exact(row_len.max(1)).rev().flatten().copied().collect()
}

#[must_use]
pub fn downsample_linear(
    pixels: &[u8],
    width: usize,
    height: usize,
    factor: usize,
) -> (Vec<u8>, usize, usize) {
    if factor <= 1 {
        return (pixels.to_vec(), width, height);
    }
    let (h, w) = (height / factor, width / factor);
    let mut out = vec![0u8; h * w * 4];
    #[allow(clippy::cast_precision_loss)]
    let count = (factor * factor) as f64;
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut sum = 0.0;
                for dy in 0..factor {
                    for dx in 0..factor {
                        let v = pixels[((y * factor + dy) * width + x * factor + dx) * 4 + c];
                        sum += (f64::from(v) / 255.0).powf(2.2);
                    }
                }
                let mean = sum / count;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let byte = (255.0 * mean.powf(1.0 / 2.2) + 0.5).clamp(0.0, 255.0) as u8;
                out[(y * w + x) * 4 + c] = byte;
            }
            out[(y * w + x) * 4 + 3] = 255;
        }
    }
    (out, w, h)
}

fn pairwise_sum_f32(a: &[f32]) -> f32 {
    const BLOCK: usize = 128;
    let n = a.len();
    if n < 8 {
        let mut res = 0.0f32;
        for x in a {
            res += *x;
        }
        res
    } else if n <= BLOCK {
        let mut r = [0.0f32; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise_sum_f32(&a[..n2]) + pairwise_sum_f32(&a[n2..])
    }
}

#[must_use]
pub fn mean_f32(a: &[f32]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = a.len() as f32;
    f64::from(pairwise_sum_f32(a) / n)
}



pub fn finish(
    plan: &RenderPlan,
    sources: &ShaderSources,
    back: &Readback,
) -> Result<(Vec<u8>, Value), WorkerError> {
    let [width, height] = plan.raster;
    if back.pixels.len() != width * height * 4
        || back.steps.len() != width * height
        || back.steps.iter().any(|x| !x.is_finite())
    {
        return Err(WorkerError::Runtime("native pixel read failed".into()));
    }
    let flipped = flipud(&back.pixels, width * 4);
    let (pixels, w, h) = downsample_linear(&flipped, width, height, plan.supersample);
    let mut report = Map::new();
    for (key, v) in [
        ("backend", json!("egl_opengles3")),
        ("renderer", json!("native_shared_shader_egl")),
        ("browser_capture", json!(false)),
        ("shader_source", json!("existing_GUI_buildShaderSources_and_SAMPLED_SDF")),
        ("gl_renderer", json!(back.gl_renderer)),
        ("gl_version", json!(back.gl_version)),
        ("shader_hashes", sources.hashes.clone()),
        ("model_geometry_modified", json!(false)),
        ("sampling", json!("endpoint_node_grid_trilinear")),
        ("trace_mode", json!("fixed_sign_change_bisection")),
        ("thin_features_below_sampling_or_step_not_certified", json!(true)),
        ("sample_shape", json!(plan.shape)),
        ("viewport", json!(plan.viewport)),
        ("raster", json!(plan.raster)),
        ("supersample", json!(plan.supersample)),
        (
            "downsample_filter",
            if plan.supersample == 1 { Value::Null } else { json!("box_mean_in_linear_light_gamma_2_2") },
        ),
        ("steps_mean", json!(mean_f32(&back.steps))),
        ("steps_max", json!(f64::from(back.steps.iter().copied().fold(f32::NEG_INFINITY, f32::max)))),
        ("field_range", json!([f64::from(plan.field_range[0]), f64::from(plan.field_range[1])])),
        ("fixed_step_mm", json!(plan.fixed_step_mm)),
        ("surface", json!(plan.surface)),
        ("section_fill", plan.section_fill.clone()),
    ] {
        report.insert(key.into(), v);
    }
    let image = implexity_io::png_io::Image {
        width: u32::try_from(w).map_err(|e| WorkerError::Runtime(e.to_string()))?,
        height: u32::try_from(h).map_err(|e| WorkerError::Runtime(e.to_string()))?,
        channels: implexity_io::png_io::Channels::Rgba,
        pixels,
    };
    let png = implexity_io::png_io::encode(&image).map_err(|e| WorkerError::Runtime(e.to_string()))?;
    Ok((png, Value::Object(report)))
}

#[derive(Debug)]
pub struct Inputs {
    pub packet: Value,
    pub field: NpyArray,
    pub color: Option<NpyArray>,
    pub sources: ShaderSources,
}



pub fn read_inputs(packet: &Path, arrays: &Path, viewer: &Path) -> Result<Inputs, WorkerError> {
    let io = |p: &Path, e: std::io::Error| WorkerError::Runtime(format!("{}: {e}", p.display()));
    let text = std::fs::read_to_string(packet).map_err(|e| io(packet, e))?;
    let packet: Value = serde_json::from_str(&text).map_err(|e| value(format!("invalid packet: {e}")))?;
    let npz = implexity_io::npz::load_file(arrays).map_err(|e| value(e.to_string()))?;
    let field = npz
        .get("field")
        .cloned()
        .ok_or_else(|| WorkerError::Key("'field is not a file in the archive'".into()))?;
    let color = npz.get("color").cloned();
    let raymarch = viewer.join("raymarch.js");
    let model = viewer.join("model.html");
    let raymarch = std::fs::read_to_string(&raymarch).map_err(|e| io(&raymarch, e))?;
    let model = std::fs::read_to_string(&model).map_err(|e| io(&model, e))?;
    Ok(Inputs {
        packet,
        field,
        color,
        sources: crate::shaders::shader_sources(&raymarch, &model).map_err(value)?,
    })
}



pub fn plan_inputs(inputs: &Inputs) -> Result<RenderPlan, WorkerError> {
    let p = &inputs.packet;
    plan(&inputs.field, inputs.color.as_ref(), &p["scene"], &p["width"], &p["height"], p.get("sample_shape"))
}

