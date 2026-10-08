// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use implexity_io::npy::{NpyArray, NpyData};
use implexity_mesh::model_view::{ModelView, evaluate_model_blocks};
use implexity_render::capture::{self as viewer, CaptureError, CaptureRequest};
use implexity_render::viewer_scene;
use serde_json::{Value, json};

use crate::error::{AgentError, AgentResult};
use crate::host::AgentModel;
use crate::runtime::AgentManager;

pub const MAX_EGL_RASTER_PIXELS: i64 = 16_777_216;
pub const MAX_EGL_RASTER_SIDE_PX: i64 = 8192;
const EGL_WORKER_TIMEOUT: Duration = Duration::from_mins(2);

fn contract(e: impl std::fmt::Display) -> AgentError {
    AgentError::contract(e.to_string())
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

fn inline_image(png: &[u8], width: u32, height: u32) -> Value {
    json!({"schema": "implexity-inline-image/1", "mime_type": "image/png",
           "data_base64": base64::engine::general_purpose::STANDARD.encode(png), "bytes": png.len(),
           "sha256": implexity_io::digest::sha256_hex(png), "width_px": width, "height_px": height})
}

fn scene_binding(bound: &Value, extra: Option<(&str, Value)>) -> Value {
    let color_sha = bound["color_texture"].get("sha256").cloned().unwrap_or(Value::Null);
    let mut b = json!({"schema": bound["specification"]["schema"], "specification": bound["specification"],
                       "sha256": bound["sha256"], "sample_shape": bound.get("sample_shape"),
                       "framing": bound.get("framing"), "color_field_sha256": color_sha});
    if let Some((k, v)) = extra {
        b[k] = v;
    }
    b["interpretation"] = bound["interpretation"].clone();
    b
}



pub fn capture(manager: &AgentManager, payload: &Value) -> AgentResult<Value> {
    let request = viewer::validate_request(payload).map_err(contract)?;
    let models =
        manager.host().model().ok_or_else(|| crate::host::HostOp::InspectParameters.unavailable())?;
    let before = models.status_value()?;
    if before.get("loaded") != Some(&Value::Bool(true))
        || before.get("content_id").and_then(Value::as_str) != Some(request.expected.as_str())
    {
        return Err(AgentError::contract("model changed before viewer capture; inspect it again"));
    }
    let root = models.geometry_model().and_then(|g| g.root());
    let derived = root.map(implexity_render::model_fields::RootDerived);
    let derived_ref = derived.as_ref().map(|d| d as &dyn viewer_scene::DerivedFieldSource);
    let mut bound = match &request.scene {
        Some(scene) => Some(
            viewer_scene::prepare_scene(models.as_ref(), derived_ref, scene, &request.expected)
                .map_err(contract)?,
        ),
        None => None,
    };
    if request.backend == "egl_offscreen" {
        let raw_scene = request.scene.clone().unwrap_or_else(|| json!({}));
        let prepared = match bound.take() {
            Some(b) => b,
            None => viewer_scene::prepare_scene(
                models.as_ref(),
                derived_ref,
                &json!({"schema": viewer_scene::SCHEMA}),
                &request.expected,
            )
            .map_err(contract)?,
        };
        #[allow(clippy::cast_precision_loss)]
        let aspect = request.width as f64 / request.height as f64;
        let fitted = viewer_scene::fit_capture_scene(&prepared, &raw_scene, aspect).map_err(contract)?;
        return capture_egl(manager, models.as_ref(), &request, &fitted, &before);
    }
    let origin = manager.host().viewer_capture_origin().unwrap_or_default();
    let raw_scene = request.scene.clone();
    let mut fit = |aspect: f64| -> Result<Option<Value>, CaptureError> {
        match (&bound, &raw_scene) {
            (Some(b), Some(raw)) => viewer_scene::fit_capture_scene(b, raw, aspect)
                .map(Some)
                .map_err(|e| CaptureError::Contract(e.to_string())),
            _ => Ok(None),
        }
    };
    let shot = match viewer::capture_browser(&origin, &request, &mut fit) {
        Ok(s) => s,
        Err(CaptureError::Contract(m)) => return Err(AgentError::contract(m)),
        Err(CaptureError::Failed(m)) => {
            return Err(AgentError::contract(format!(
                "native GUI raymarch capture failed (no renderer substitution): {}",
                clip(&m, 1200)
            )));
        }
    };
    let after = models.status_value()?;
    if after.get("content_id").and_then(Value::as_str) != Some(request.expected.as_str())
        || after.get("structure_id") != before.get("structure_id")
    {
        return Err(AgentError::contract("authoritative model changed during capture"));
    }
    let Some((w, h)) = viewer::png_size(&shot.png) else {
        return Err(AgentError::contract("viewer did not return a PNG"));
    };
    if i64::from(w) * i64::from(h) > viewer::MAX_CAPTURE_PIXELS
        || shot.png.len() > viewer::MAX_CAPTURE_PNG_BYTES
    {
        return Err(AgentError::contract("viewer capture exceeds the bounded inline image size"));
    }
    Ok(json!({
        "schema": viewer::SCHEMA, "truth_status": "render_only_not_physical_validation",
        "source": {"content_id": request.expected, "structure_id": before.get("structure_id")},
        "renderer": "native_webgl_raymarcher", "scene": shot.scene,
        "capture": {"backend": "browser", "capture_layout": request.capture_layout,
                    "viewport_css_px": [request.width, request.height],
                    "device_scale_factor": request.device_scale_factor,
                    "gui_overlays_included": request.capture_layout == "gui",
                    "image_px": [w, h]},
        "scene_binding": shot.bound_scene.as_ref().map_or(Value::Null, |b| scene_binding(b, None)),
        "image": inline_image(&shot.png, w, h),
    }))
}

fn worker_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("IMPLEXITY_EGL_WORKER").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "implexity-egl-worker.exe" } else { "implexity-egl-worker" };
    [dir.join(name), dir.parent().map(|p| p.join(name)).unwrap_or_default()].into_iter().find(|p| p.is_file())
}

fn run_worker(exe: &Path, dir: &Path) -> Result<(Vec<u8>, Value), String> {
    let mut child = Command::new(exe)
        .arg("--packet")
        .arg(dir.join("packet.json"))
        .arg("--arrays")
        .arg(dir.join("arrays.npz"))
        .arg("--viewer")
        .arg(dir.join("viewer"))
        .arg("--output")
        .arg(dir)
        .env("EGL_PLATFORM", std::env::var("EGL_PLATFORM").unwrap_or_else(|_| "surfaceless".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("graphics worker could not start: {e}"))?;
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > EGL_WORKER_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Command timed out after {} seconds", EGL_WORKER_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let stderr = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("graphics worker failed: {}", tail(&stderr, 1600)));
    }
    let png = std::fs::read(dir.join("image.png")).map_err(|e| e.to_string())?;
    let graphics: Value =
        serde_json::from_slice(&std::fs::read(dir.join("graphics.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    Ok((png, graphics))
}

fn f32_le(values: &[f64]) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation)]
    values.iter().map(|v| *v as f32).collect()
}

#[allow(clippy::too_many_lines)]
fn capture_egl(
    manager: &AgentManager,
    models: &dyn AgentModel,
    request: &CaptureRequest,
    bound: &Value,
    before: &Value,
) -> AgentResult<Value> {
    let expected = request.expected.as_str();
    let s = &bound["specification"];
    let shape: Vec<usize> = bound["sample_shape"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).filter_map(|x| usize::try_from(x).ok()).collect())
        .unwrap_or_default();
    if shape.len() != 3 {
        return Err(AgentError::contract("native scene has no sample shape"));
    }
    let bbox: Vec<Vec<f64>> = s["bbox_mm"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    r.as_array().map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    if bbox.len() != 2 || bbox.iter().any(|r| r.len() != 3) {
        return Err(AgentError::contract("native scene has no bounds"));
    }
    let scale = request.device_scale_factor;
    let supersample = s["supersample"].as_i64().unwrap_or(1);
    let (out_w, out_h) = (request.width * scale, request.height * scale);
    let (raster_w, raster_h) = (out_w * supersample, out_h * supersample);
    if raster_w * raster_h > MAX_EGL_RASTER_PIXELS || raster_w.max(raster_h) > MAX_EGL_RASTER_SIDE_PX {
        return Err(AgentError::contract(format!(
            "EGL raster {raster_w}x{raster_h} exceeds its {MAX_EGL_RASTER_PIXELS}-pixel/{MAX_EGL_RASTER_SIDE_PX}-px bound; reduce supersample, device_scale_factor or the viewport"
        )));
    }
    let field: Vec<f32> = {
        let _live = models.live_lock();
        if models.status_value()?.get("content_id").and_then(Value::as_str) != Some(expected) {
            return Err(AgentError::contract("model changed before EGL sampling"));
        }
        let axes: Vec<Vec<f64>> =
            (0..3).map(|a| implexity_mesh::numeric::linspace(bbox[0][a], bbox[1][a], shape[a])).collect();
        let mut xyz = Vec::with_capacity(shape.iter().product());
        for &x in &axes[0] {
            for &y in &axes[1] {
                for &z in &axes[2] {
                    xyz.push([x, y, z]);
                }
            }
        }
        let (values, _record) = evaluate_model_blocks(models as &dyn ModelView, &xyz, expected)
            .map_err(|e| AgentError::contract(format!("EGL field sampling failed: {e}")))?;
        let field = f32_le(&values);
        if field.iter().any(|v| !v.is_finite()) {
            return Err(AgentError::contract("native geometry field is not finite"));
        }
        field
    };
    let mut arrays: Vec<(String, NpyArray)> = vec![(
        "field".into(),
        NpyArray::new(shape.clone(), NpyData::F32(field.clone()))
            .map_err(|e| AgentError::failed(e.to_string()))?,
    )];
    let texture = &bound["color_texture"];
    if !texture.is_null() {
        let data = base64::engine::general_purpose::STANDARD
            .decode(texture["data_base64"].as_str().unwrap_or_default())
            .map_err(|e| AgentError::contract(e.to_string()))?;
        if texture["content_id"] != expected
            || implexity_io::digest::sha256_hex(&data) != texture["sha256"].as_str().unwrap_or("")
        {
            return Err(AgentError::contract("native color texture identity/hash mismatch"));
        }
        let colour: Vec<f32> =
            data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        arrays.push((
            "color".into(),
            NpyArray::new(shape.clone(), NpyData::F32(colour))
                .map_err(|e| AgentError::contract(e.to_string()))?,
        ));
    }
    let raymarch = manager.host().viewer_asset("raymarch.js");
    let model_html = manager.host().viewer_asset("model.html");
    let (Some(raymarch), Some(model_html)) = (raymarch, model_html) else {
        return Err(AgentError::contract("installed native viewer shader assets are unavailable"));
    };
    let outcome = (|| -> Result<(Vec<u8>, Value), String> {
        let exe = worker_executable()
            .ok_or("the graphics worker implexity-egl-worker is not installed next to this executable")?;
        let dir = std::env::temp_dir()
            .join(format!("implexity-native-egl-{}", implexity_io::atomic::unique_token()));
        std::fs::create_dir_all(dir.join("viewer")).map_err(|e| e.to_string())?;
        let result = (|| {
            std::fs::write(dir.join("viewer").join("raymarch.js"), &raymarch).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("viewer").join("model.html"), &model_html).map_err(|e| e.to_string())?;
            let packet = json!({"scene": s, "width": out_w, "height": out_h, "sample_shape": shape});
            std::fs::write(dir.join("packet.json"), packet.to_string()).map_err(|e| e.to_string())?;
            let members: Vec<(&str, &NpyArray)> = arrays.iter().map(|(k, v)| (k.as_str(), v)).collect();
            let npz = implexity_io::npz::save(&members).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("arrays.npz"), npz).map_err(|e| e.to_string())?;
            run_worker(&exe, &dir)
        })();
        let _ = std::fs::remove_dir_all(&dir);
        result
    })();
    let (png, graphics) = outcome.map_err(|m| {
        AgentError::contract(format!("explicit native EGL capture failed (no fallback): {}", clip(&m, 1800)))
    })?;
    let after = models.status_value()?;
    if after.get("content_id").and_then(Value::as_str) != Some(expected)
        || after.get("structure_id") != before.get("structure_id")
    {
        return Err(AgentError::contract("authoritative model changed during EGL capture"));
    }
    let size = viewer::png_size(&png).map(|(w, h)| (i64::from(w), i64::from(h)));
    if size != Some((out_w, out_h)) {
        return Err(AgentError::contract("native EGL worker returned an invalid image"));
    }
    let field_bytes: Vec<u8> = field.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (w, h) = (u32::try_from(out_w).unwrap_or(0), u32::try_from(out_h).unwrap_or(0));
    Ok(json!({
        "schema": viewer::SCHEMA, "truth_status": "render_only_not_physical_validation",
        "source": {"content_id": expected, "structure_id": before.get("structure_id")},
        "renderer": "native_shared_shader_egl", "scene": graphics,
        "capture": {"backend": "egl_offscreen", "capture_layout": "canvas",
                    "viewport_css_px": [request.width, request.height], "device_scale_factor": scale,
                    "gui_overlays_included": false, "image_px": [out_w, out_h]},
        "scene_binding": scene_binding(bound, Some(("geometry_field_sha256", json!(implexity_io::digest::sha256_hex(&field_bytes))))),
        "image": inline_image(&png, w, h),
    }))
}
