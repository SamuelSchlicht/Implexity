// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#[cfg(feature = "viewer-capture")]
pub mod cdp;
#[cfg(feature = "dynamic")]
pub mod panel;
#[cfg(feature = "viewer-capture")]
mod ws;

use serde_json::Value;

use crate::RenderError;

pub const SCHEMA: &str = "implexity-native-viewer-capture/1";
pub const CAPTURE_LAYOUTS: [&str; 2] = ["canvas", "gui"];
pub const DEVICE_SCALE_FACTORS: [i64; 3] = [1, 2, 3];
pub const DEFAULT_DEVICE_SCALE_FACTOR: i64 = 2;
pub const MAX_CAPTURE_PIXELS: i64 = 4 * 1024 * 1024;
pub const MAX_CAPTURE_PNG_BYTES: usize = 8 * 1024 * 1024;
pub const READY_TIMEOUT_MS: u64 = 150_000;
pub const SCREENSHOT_TIMEOUT_MS: u64 = 60_000;
pub const CANVAS_VIEWPORT_RANGE_PX: (i64, i64) = (256, 2048);
pub const GUI_VIEWPORT_RANGE_PX: ((i64, i64), (i64, i64)) = ((800, 1920), (600, 1200));

pub const CANVAS_LAYOUT_SCRIPT: &str = r#"(() => {
  const css = "html[data-capture-layout=canvas] body{visibility:hidden!important;overflow:hidden!important}"
    + "html[data-capture-layout=canvas] canvas#glview{visibility:visible!important;position:fixed!important;"
    + "inset:0!important;width:100vw!important;height:100vh!important;z-index:2147483647!important;"
    + "margin:0!important;border:0!important}";
  const apply = () => {
    const root = document.documentElement;
    if (!root) return false;
    root.dataset.captureLayout = "canvas";
    const style = document.createElement("style");
    style.textContent = css;
    root.appendChild(style);
    return true;
  };
  if (!apply()) {
    const observer = new MutationObserver(() => { if (apply()) observer.disconnect(); });
    observer.observe(document, {childList: true});
  }
})();"#;

#[derive(Debug, Clone, PartialEq)]
pub struct CaptureRequest {
    pub expected: String,
    pub width: i64,
    pub height: i64,
    pub device_scale_factor: i64,
    pub capture_layout: String,
    pub backend: String,
    pub scene: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    Contract(String),
    Failed(String),
}

fn refuse(m: impl Into<String>) -> CaptureError {
    CaptureError::Contract(m.into())
}



pub fn validate_request(payload: &Value) -> Result<CaptureRequest, RenderError> {
    let bad = |m: &str| RenderError::Invalid(m.to_owned());
    let keys = [
        "expected_content_id",
        "viewport_width_px",
        "viewport_height_px",
        "scene",
        "backend",
        "device_scale_factor",
        "capture_layout",
    ];
    let Some(p) = payload
        .as_object()
        .filter(|p| p.keys().all(|k| keys.contains(&k.as_str())) && p.contains_key("expected_content_id"))
    else {
        return Err(bad("native viewer capture requires the expected current model identity"));
    };
    let backend = p.get("backend").cloned().unwrap_or_else(|| Value::from("browser"));
    let Some(backend) =
        backend.as_str().filter(|b| *b == "browser" || *b == "egl_offscreen").map(str::to_owned)
    else {
        return Err(bad("native capture backend must be browser or explicitly selected egl_offscreen"));
    };
    let Some(expected) = p
        .get("expected_content_id")
        .and_then(Value::as_str)
        .filter(|s| (1..=128).contains(&s.chars().count()))
    else {
        return Err(bad("invalid expected model identity"));
    };
    let layout = p.get("capture_layout").cloned().unwrap_or_else(|| Value::from("canvas"));
    let Some(layout) = layout.as_str().filter(|l| CAPTURE_LAYOUTS.contains(l)).map(str::to_owned) else {
        return Err(bad("capture_layout must be canvas or gui"));
    };
    if backend == "egl_offscreen" && layout != "canvas" {
        return Err(bad("egl_offscreen renders the viewport canvas only; capture_layout must be canvas"));
    }
    let int = |v: &Value| v.as_i64().filter(|_| v.is_i64() || v.is_u64());
    let scale =
        p.get("device_scale_factor").cloned().unwrap_or_else(|| Value::from(DEFAULT_DEVICE_SCALE_FACTOR));
    let Some(scale) = int(&scale).filter(|s| DEVICE_SCALE_FACTORS.contains(s)) else {
        return Err(bad("device_scale_factor must be 1, 2 or 3"));
    };
    let width = p.get("viewport_width_px").cloned().unwrap_or_else(|| Value::from(1200));
    let height = p.get("viewport_height_px").cloned().unwrap_or_else(|| Value::from(800));
    let ((w0, w1), (h0, h1)) = if layout == "gui" {
        GUI_VIEWPORT_RANGE_PX
    } else {
        (CANVAS_VIEWPORT_RANGE_PX, CANVAS_VIEWPORT_RANGE_PX)
    };
    let (Some(width), Some(height)) = (int(&width), int(&height)) else {
        return Err(RenderError::Invalid(format!(
            "{layout} capture viewport must be {w0}..{w1} by {h0}..{h1} CSS pixels"
        )));
    };
    if !(w0..=w1).contains(&width) || !(h0..=h1).contains(&height) {
        return Err(RenderError::Invalid(format!(
            "{layout} capture viewport must be {w0}..{w1} by {h0}..{h1} CSS pixels"
        )));
    }
    if width * height * scale * scale > MAX_CAPTURE_PIXELS {
        return Err(RenderError::Invalid(format!(
            "capture output {}x{} exceeds the {MAX_CAPTURE_PIXELS}-pixel image bound; reduce the viewport or device_scale_factor",
            width * scale,
            height * scale
        )));
    }
    let scene = p.get("scene").cloned();
    if let Some(s) = &scene {
        crate::viewer_scene::validate_scene(s)?;
    }
    Ok(CaptureRequest {
        expected: expected.to_owned(),
        width,
        height,
        device_scale_factor: scale,
        capture_layout: layout,
        backend,
        scene,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrowserCapture {
    pub png: Vec<u8>,
    pub scene: Value,
    pub bound_scene: Option<Value>,
}

#[must_use]
pub fn loopback_origin_ok(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else { return false };
    let (host, port) = if let Some(r) = rest.strip_prefix("[::1]:") {
        ("::1", r)
    } else if let Some(r) = rest.strip_prefix("127.0.0.1:") {
        ("127.0.0.1", r)
    } else {
        return false;
    };
    !host.is_empty()
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p > 0)
}

#[cfg(feature = "viewer-capture")]
fn js_string(s: &str) -> String {
    Value::from(s).to_string()
}



#[cfg(feature = "viewer-capture")]
#[allow(clippy::too_many_lines)]
pub fn capture_browser(
    origin: &str,
    request: &CaptureRequest,
    fit_scene: &mut dyn FnMut(f64) -> Result<Option<Value>, CaptureError>,
) -> Result<BrowserCapture, CaptureError> {
    use std::time::Duration;

    use base64::Engine as _;
    use serde_json::json;

    if !loopback_origin_ok(origin) {
        return Err(refuse("native capture requires this service's loopback HTTP viewer"));
    }
    let Some(executable) = cdp::find_chromium() else {
        return Err(refuse(
            "native viewer capture requires the optional Playwright dependency and an installed Chromium browser; render_3d remains a separately labelled CPU renderer",
        ));
    };
    let software = std::env::var("IMPLEXITY_VIEWER_SOFTWARE_WEBGL").is_ok_and(|v| v == "1");
    let failed = |e: cdp::CdpError| CaptureError::Failed(e.0);
    let ready = Duration::from_millis(READY_TIMEOUT_MS);
    let mut browser = cdp::Browser::launch(&executable, software, origin).map_err(failed)?;
    browser.open_page().map_err(failed)?;
    let t30 = Duration::from_secs(30);
    #[allow(clippy::cast_precision_loss)]
    let metrics = json!({"width": request.width, "height": request.height,
                         "deviceScaleFactor": request.device_scale_factor, "mobile": false});
    browser.command("Emulation.setDeviceMetricsOverride", metrics, true, t30).map_err(failed)?;
    if request.capture_layout == "canvas" {
        browser
            .command(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": CANVAS_LAYOUT_SCRIPT}),
                true,
                t30,
            )
            .map_err(failed)?;
    }
    browser.clear_events();
    browser
        .command("Page.navigate", json!({"url": format!("{origin}/viewer/model.html")}), true, t30)
        .map_err(failed)?;
    browser.wait_event("Page.domContentEventFired", t30).map_err(failed)?;
    let expected = js_string(&request.expected);
    browser
        .wait_for(
            &format!(
                "(() => {{ const expected = {expected}; const a=window.ImplexityViewerAdapter;
                    if(!a || !a.inspectScene) return false;
                    const s=a.inspectScene();
                    return s.ready && s.renderer==='native_webgl_raymarcher' &&
                           s.model_content_id===expected && s.field_content_id===expected; }})()"
            ),
            ready,
        )
        .map_err(failed)?;
    let canvas = browser
        .evaluate(
            "(() => { const c=document.querySelector('#glview'); return [c.clientWidth,c.clientHeight]; })()",
            ready,
        )
        .map_err(failed)?;
    let size: Vec<f64> =
        canvas.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    if size.len() != 2 || size.iter().any(|n| *n <= 0.0) {
        return Err(refuse("native capture canvas has invalid dimensions"));
    }
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (request.width as f64, request.height as f64);
    if request.capture_layout == "canvas" && ((size[0] - w).abs() > 1.0 || (size[1] - h).abs() > 1.0) {
        return Err(refuse(format!(
            "canvas capture layout was not applied (canvas {}x{}, viewport {}x{})",
            py_num(size[0]),
            py_num(size[1]),
            request.width,
            request.height
        )));
    }
    let bound = fit_scene(size[0] / size[1])?;
    if let Some(b) = &bound {
        browser.evaluate(&format!("window.ImplexityViewerAdapter.applyScene({b})"), ready).map_err(failed)?;
        let digest = js_string(b["sha256"].as_str().unwrap_or_default());
        browser
            .wait_for(
                &format!(
                    "(() => {{ const digest = {digest}; const s=window.ImplexityViewerAdapter.inspectScene(); return s.scene_sha256===digest && s.stats.refined && s.stats.rendered_frames>0; }})()"
                ),
                ready,
            )
            .map_err(failed)?;
    }
    browser
        .evaluate(
            "new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))",
            ready,
        )
        .map_err(failed)?;
    let mut scene =
        browser.evaluate("window.ImplexityViewerAdapter.inspectScene()", ready).map_err(failed)?;
    if bound.is_some() && scene["stats"]["gl"]["texture_float_linear"] != Value::Bool(true) {
        return Err(refuse(
            "native scene requires linear floating-point texture sampling; nearest-neighbour substitution is not admitted",
        ));
    }
    if scene.get("errors").is_some_and(truthy) {
        return Err(refuse(format!(
            "native viewer reported graphics errors before capture: {}",
            clip_text(&py_repr(&scene["errors"]), 600)
        )));
    }
    if let Some(m) = scene.as_object_mut() {
        m.insert("browser_version".into(), Value::from(browser.version.clone()));
        m.insert("software_webgl_requested".into(), Value::from(software));
    }
    if scene.get("node_id") != scene.get("root_id") {
        return Err(refuse("native capture is not showing the authoritative root"));
    }
    let clip = browser
        .evaluate(
            "(() => { const r=document.querySelector('#glview').getBoundingClientRect(); return [r.x,r.y,r.width,r.height]; })()",
            ready,
        )
        .map_err(failed)?;
    let c: Vec<f64> =
        clip.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    if c.len() != 4 {
        return Err(CaptureError::Failed("canvas bounds unavailable".into()));
    }
    let shot = browser
        .command(
            "Page.captureScreenshot",
            json!({"format": "png", "clip": {"x": c[0], "y": c[1], "width": c[2], "height": c[3], "scale": 1},
                   "captureBeyondViewport": false}),
            true,
            Duration::from_millis(SCREENSHOT_TIMEOUT_MS),
        )
        .map_err(failed)?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(shot["data"].as_str().unwrap_or_default())
        .map_err(|e| CaptureError::Failed(e.to_string()))?;
    let after = browser.evaluate("window.ImplexityViewerAdapter.inspectScene()", ready).map_err(failed)?;
    if after.get("errors").is_some_and(truthy) {
        return Err(refuse(format!(
            "native viewer reported graphics errors during capture: {}",
            clip_text(&py_repr(&after["errors"]), 600)
        )));
    }
    for key in [
        "model_content_id",
        "field_content_id",
        "node_id",
        "root_id",
        "renderer",
        "field_source",
        "shader_key",
        "scene_sha256",
        "color_field_sha256",
        "scene_content_id",
        "camera",
    ] {
        if after.get(key) != scene.get(key) {
            return Err(refuse("viewer state changed during capture"));
        }
    }
    drop(browser);
    Ok(BrowserCapture { png, scene, bound_scene: bound })
}



#[cfg(not(feature = "viewer-capture"))]
pub fn capture_browser(
    origin: &str,
    _request: &CaptureRequest,
    _fit_scene: &mut dyn FnMut(f64) -> Result<Option<Value>, CaptureError>,
) -> Result<BrowserCapture, CaptureError> {
    if !loopback_origin_ok(origin) {
        return Err(refuse("native capture requires this service's loopback HTTP viewer"));
    }
    Err(refuse(
        "native viewer capture requires the optional Playwright dependency and an installed Chromium browser; render_3d remains a separately labelled CPU renderer",
    ))
}

#[cfg(feature = "viewer-capture")]
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

#[cfg(feature = "viewer-capture")]
fn py_repr(v: &Value) -> String {
    implexity_core::pyobj::repr(v)
}

#[cfg(feature = "viewer-capture")]
fn clip_text(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(feature = "viewer-capture")]
fn py_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        #[allow(clippy::cast_possible_truncation)]
        let i = x as i64;
        i.to_string()
    } else {
        implexity_core::py_repr::repr_float(x)
    }
}

#[must_use]
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

