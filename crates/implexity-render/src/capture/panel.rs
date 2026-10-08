// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use super::CaptureError;

pub const PANEL_PATH: &str = "/viewer/rust_ext/dynamic_results.html";
pub const PANEL_KEYS: [&str; 22] = [
    "store",
    "clip",
    "field",
    "component",
    "mode",
    "colormap",
    "range",
    "background",
    "plane",
    "position",
    "occupancy",
    "fill_occupancy",
    "flow_style",
    "flow_field",
    "deform_field",
    "deform_scale",
    "deform_mask",
    "deform_mask_threshold",
    "deform_colour",
    "iso_field",
    "cycle",
    "index",
];

fn refuse(m: impl Into<String>) -> CaptureError {
    CaptureError::Contract(m.into())
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            out.push(char::from(b));
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}



pub fn panel_link(pairs: &[(&str, String)]) -> Result<String, CaptureError> {
    let mut query = Vec::new();
    for (k, v) in pairs {
        if !PANEL_KEYS.contains(k) {
            return Err(refuse(format!("the dynamic panel link does not take {k:?}")));
        }
        if v.len() > 128 || !v.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err(refuse(format!("the dynamic panel link value of {k} is malformed")));
        }
        query.push(format!("{k}={}", quote(v)));
    }
    Ok(format!("{PANEL_PATH}?{}", query.join("&")))
}



#[cfg(feature = "viewer-capture")]
pub fn capture_panel(
    origin: &str,
    link: &str,
    store: &str,
    width: u32,
    height: u32,
) -> Result<(Vec<u8>, Value), CaptureError> {
    use std::time::Duration;

    use base64::Engine as _;
    use serde_json::json;

    if !super::loopback_origin_ok(origin) {
        return Err(refuse("the dynamic panel capture requires this service's loopback HTTP viewer"));
    }
    if !link.starts_with(PANEL_PATH) || link.contains("..") || link.contains('#') {
        return Err(refuse("the dynamic panel capture only opens the dynamic results panel"));
    }
    if !(640..=1920).contains(&width) || !(480..=1400).contains(&height) {
        return Err(refuse("the dynamic panel capture viewport must be 640..1920 x 480..1400"));
    }
    let Some(executable) = super::cdp::find_chromium() else {
        return Err(refuse(
            "the dynamic panel capture requires an installed Chromium browser; render_dynamic_frames with the software renderer remains available",
        ));
    };
    let failed = |e: super::cdp::CdpError| CaptureError::Failed(e.0);
    let t30 = Duration::from_secs(30);
    let ready = Duration::from_mins(3);
    let mut browser = super::cdp::Browser::launch(&executable, true, origin).map_err(failed)?;
    browser.open_page().map_err(failed)?;
    browser
        .command(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
            true,
            t30,
        )
        .map_err(failed)?;
    browser.clear_events();
    browser.command("Page.navigate", json!({"url": format!("{origin}{link}")}), true, t30).map_err(failed)?;
    browser.wait_event("Page.domContentEventFired", t30).map_err(failed)?;
    let expected = Value::from(store).to_string();
    browser
        .wait_for(
            &format!(
                "(() => {{ const d = globalThis.ImplexityDynamicResults; const i = document.querySelector('#drImage');
                   return !!d && d.state.loaded === {expected} && !!i && i.complete && i.naturalWidth > 0; }})()"
            ),
            ready,
        )
        .map_err(failed)?;

    browser
        .evaluate(
            "new Promise(resolve => setTimeout(() => requestAnimationFrame(() => resolve(true)), 400))",
            ready,
        )
        .map_err(failed)?;
    let status = browser.evaluate("document.querySelector('#drStatus').textContent", t30).map_err(failed)?;
    if status.as_str().is_some_and(|s| s.starts_with("render refused") || s.starts_with("could not open")) {
        return Err(CaptureError::Failed(format!(
            "the panel refused the view: {}",
            status.as_str().unwrap_or_default()
        )));
    }
    let state = browser
        .evaluate(
            "(() => { const d = globalThis.ImplexityDynamicResults.state; const f = d.frames[d.pos] || null;
                      const r = document.querySelector('section.dr-view').getBoundingClientRect();
                      return {store: d.loaded, frame: f, info: document.querySelector('#drInfo').textContent,
                              clip: [r.x, r.y, r.width, r.height]}; })()",
            t30,
        )
        .map_err(failed)?;
    let c: Vec<f64> =
        state["clip"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    if c.len() != 4 || c[2] <= 0.0 || c[3] <= 0.0 {
        return Err(CaptureError::Failed("the panel view has no area".into()));
    }
    let shot = browser
        .command(
            "Page.captureScreenshot",
            json!({"format": "png", "clip": {"x": c[0], "y": c[1], "width": c[2], "height": c[3], "scale": 1},
                   "captureBeyondViewport": true}),
            true,
            Duration::from_mins(1),
        )
        .map_err(failed)?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(shot["data"].as_str().unwrap_or_default())
        .map_err(|e| CaptureError::Failed(e.to_string()))?;
    let mut record = state;
    if let Some(m) = record.as_object_mut() {
        m.remove("clip");
        m.insert("browser_version".into(), Value::from(browser.version.clone()));
        m.insert("link".into(), Value::from(link));
    }
    drop(browser);
    Ok((png, record))
}



#[cfg(not(feature = "viewer-capture"))]
pub fn capture_panel(
    _origin: &str,
    _link: &str,
    _store: &str,
    _width: u32,
    _height: u32,
) -> Result<(Vec<u8>, Value), CaptureError> {
    Err(refuse("this build has no browser capture (feature viewer-capture)"))
}

