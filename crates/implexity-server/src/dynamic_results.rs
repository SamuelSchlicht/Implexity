// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use base64::Engine as _;
use implexity_runtime::dynamic_frames::capture::{self, CaptureRoot, DEFAULT_BUDGET_BYTES};
use implexity_runtime::dynamic_frames::catalogue::SERVICE_ROOT;
use serde_json::{Map, Value, json};

use crate::http::{Reply, Request, RouteError, json_ascii};
use crate::routes::{BodyPolicy, Registry, RouteDeclarationError, RouteGate};
pub use crate::rust_extension::catalogue;
use crate::service::Service;

pub const OWNER: &str = "rust-extension:dynamic_results:v1";
pub const TABLE_KEY: &str = "rust_extension.dynamic_results";
pub const BUDGET_ENV: &str = "IMPLEXITY_DYNAMIC_CAPTURE_BUDGET_BYTES";
const MODULE: &str = "implexity.rust_extension.dynamic_results";

pub fn install_capture_root(service: &Service) {
    let Ok(state) = service.state_dir() else { return };
    let budget_bytes = std::env::var(BUDGET_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|b| *b >= implexity_runtime::dynamic_frames::manifest::MIN_BYTE_LIMIT)
        .unwrap_or(DEFAULT_BUDGET_BYTES);
    capture::install(Some(CaptureRoot { dir: state.join(SERVICE_ROOT), budget_bytes }));
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[must_use]
pub fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (unquote(k), unquote(v))
        })
        .collect()
}

fn number(v: &str) -> Value {
    if let Ok(i) = v.parse::<i64>() {
        return json!(i);
    }
    v.parse::<f64>().ok().filter(|x| x.is_finite()).map_or_else(|| json!(v), |x| json!(x))
}

fn list(v: &str) -> Value {
    Value::Array(v.split(',').filter(|s| !s.is_empty()).map(number).collect())
}

#[must_use]
pub fn view_payload(pairs: &[(String, String)]) -> Map<String, Value> {
    let mut p = Map::new();
    let mut flow = Map::new();
    let mut deform = Map::new();
    let mut iso = Map::new();
    let mut kymo = Map::new();
    let mut clip = Map::new();
    let (mut vmin, mut vmax) = (None, None);
    for (k, v) in pairs {
        match k.as_str() {
            "field" | "component" | "mode" | "plane" | "colormap" | "occupancy" | "range" | "background"
            | "format" => {
                p.insert(k.clone(), json!(v));
            }
            "position" | "width_px" | "height_px" | "phase_bins" | "fps" | "frames_per_cycle" | "cycles"
            | "max_bytes" | "columns" => {
                p.insert(k.clone(), number(v));
            }
            "cycle" => {
                p.insert(k.clone(), if v == "last" { json!("last") } else { number(v) });
            }
            "fill_occupancy" => {
                p.insert(k.clone(), json!(v == "1" || v == "true"));
            }
            "flow_field" => {
                flow.insert("field".into(), json!(v));
            }
            "flow_style" => {
                flow.insert("style".into(), json!(v));
            }
            "separation_px" | "length_px" => {
                flow.insert(k.clone(), number(v));
            }
            "deform_field" => {
                deform.insert("field".into(), json!(v));
            }
            "deform_scale" => {
                deform.insert("scale".into(), number(v));
            }
            "deform_mask" => {
                deform.insert("mask".into(), json!(v));
            }
            "deform_mask_threshold" => {
                deform.insert("mask_threshold".into(), number(v));
            }
            "deform_colour" => {
                deform.insert("colour".into(), json!(v));
            }
            "grid_every" => {
                deform.insert("grid_every".into(), number(v));
            }
            "iso_field" => {
                iso.insert("field".into(), json!(v));
            }
            "iso_value" => {
                iso.insert("value".into(), number(v));
            }
            "camera" => {
                iso.insert("camera".into(), json!(v));
            }
            "clip_point" => {
                clip.insert("point".into(), list(v));
            }
            "clip_normal" => {
                clip.insert("normal".into(), list(v));
            }
            "from" | "to" => {
                kymo.insert(k.clone(), list(v));
            }
            "samples" => {
                kymo.insert(k.clone(), number(v));
            }
            "vmin" => vmin = Some(number(v)),
            "vmax" => vmax = Some(number(v)),

            other if !matches!(other, "id" | "ids" | "index" | "phases" | "_") => {
                p.insert(other.to_owned(), json!(v));
            }
            _ => {}
        }
    }
    if !flow.is_empty() {
        p.insert("flow".into(), Value::Object(flow));
    }
    if !deform.is_empty() {
        p.insert("deformation".into(), Value::Object(deform));
    }
    if !clip.is_empty() {
        iso.insert("clip".into(), Value::Object(clip));
    }
    if !iso.is_empty() {
        p.insert("iso".into(), Value::Object(iso));
    }
    if !kymo.is_empty() {
        p.insert("kymograph".into(), Value::Object(kymo));
    }
    if let (Some(a), Some(b)) = (vmin, vmax) {
        p.insert("value_range".into(), json!([a, b]));
    }
    p
}

fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn agent_reply(e: &implexity_agent::AgentError) -> Reply {
    match e {
        implexity_agent::AgentError::Failed(m) => Reply::err(500, m, json!("dynamic results")),
        other => Reply::err(400, other.message(), json!("dynamic results")),
    }
}

fn run(action: &str, req: &Request, p: &Map<String, Value>) -> Result<Value, Reply> {
    implexity_agent::dynamic::validate(action, p).map_err(|e| agent_reply(&e))?;
    implexity_agent::dynamic::dispatch(&catalogue(&req.service), action, p).map_err(|e| agent_reply(&e))
}

fn image_reply(result: &Value, key: &str) -> Reply {
    let rec = &result[key];
    let rec = if key == "delivery" { &rec["files"][0] } else { rec };
    let Some(data) = rec["data_base64"].as_str() else {
        return Reply::err(500, "the render produced no image", Value::Null);
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return Reply::err(500, "the render produced invalid base64", Value::Null);
    };
    let mime = rec["mime_type"].as_str().unwrap_or("application/octet-stream").to_owned();
    let mut meta = result.clone();
    if let Some(m) = meta.as_object_mut() {
        m.remove("image");
        m.remove("delivery");
        m.remove("frames");
    }
    let mut headers = vec![("X-Implexity-Meta".to_owned(), json_ascii(&meta))];
    if let Some(name) = rec["filename"].as_str() {
        headers.push(("Content-Disposition".to_owned(), format!("attachment; filename=\"{name}\"")));
    }
    Reply::send(200, bytes, &mime, headers)
}

fn handle(req: &Request, what: &str) -> Reply {
    let pairs = query_pairs(&req.query);
    let id = get(&pairs, "id").unwrap_or_default().to_owned();
    let outcome = match what {
        "stores" => {
            let mut p = Map::new();
            if let Some(j) = get(&pairs, "job_id") {
                p.insert("job_id".into(), json!(j));
            }
            if let Some(l) = get(&pairs, "limit") {
                p.insert("limit".into(), number(l));
            }
            run("inspect_dynamic_results", req, &p).map(|v| Reply::json(&v, 200))
        }
        "store" => run("inspect_dynamic_results", req, &Map::from_iter([("store".into(), json!(id))]))
            .map(|v| Reply::json(&v, 200)),
        "series" => {
            let mut p = Map::from_iter([("store".into(), json!(id))]);
            for k in ["max_points", "t_start", "t_end", "last_cycles"] {
                if let Some(v) = get(&pairs, k) {
                    p.insert(k.into(), number(v));
                }
            }
            if let Some(s) = get(&pairs, "series") {
                p.insert("series".into(), json!(s.split(',').filter(|x| !x.is_empty()).collect::<Vec<_>>()));
            }
            if get(&pairs, "spectrum").is_some_and(|v| v == "1") {
                p.insert("spectrum".into(), json!({"window": "hann", "peaks": 5}));
            }
            run("read_time_series", req, &p).map(|v| Reply::json(&v, 200))
        }
        "frame" => {
            let mut p = view_payload(&pairs);
            p.insert("store".into(), json!(id));
            if p.get("mode").and_then(Value::as_str) != Some("phase_average")
                && p.get("mode").and_then(Value::as_str) != Some("kymograph")
            {
                let index = get(&pairs, "index").map_or(json!(0), number);
                p.insert("frames".into(), json!({"indices": [index]}));
            }
            run("render_dynamic_frames", req, &p).map(|v| image_reply(&v, "image"))
        }
        "sheet" => {
            let mut p = view_payload(&pairs);
            let ids: Vec<&str> =
                get(&pairs, "ids").unwrap_or_default().split(',').filter(|s| !s.is_empty()).collect();
            if ids.len() > 1 {
                p.insert("compare_stores".into(), json!(ids));
            } else {
                p.insert("store".into(), json!(ids.first().copied().unwrap_or(id.as_str())));
            }
            if let Some(ph) = get(&pairs, "phases") {
                p.insert("frames".into(), json!({"phases": list(ph)}));
            } else if let Some(n) = get(&pairs, "count") {
                p.insert("frames".into(), json!({"count": number(n)}));
            }
            run("render_dynamic_frames", req, &p).map(|v| image_reply(&v, "image"))
        }
        "animation" => {
            let mut p = view_payload(&pairs);
            p.insert("store".into(), json!(id));
            p.entry("format").or_insert(json!("webp"));
            run("export_animation", req, &p).map(|v| image_reply(&v, "delivery"))
        }
        _ => Ok(Reply::err(404, "unknown dynamic results route", json!(what))),
    };
    outcome.unwrap_or_else(|reply| reply)
}



pub fn routes() -> Result<Registry, RouteDeclarationError> {
    let mut r = Registry::new();
    for what in ["stores", "store", "series", "frame", "sheet", "animation"] {
        r.route(
            "GET",
            &format!("/v1/dynamic/{what}"),
            BodyPolicy::None,
            "Rust extension implexity-rust-extension/1: dynamic results panel",
            MODULE,
            Arc::new(move |req: &Request| -> Result<Reply, RouteError> { Ok(handle(req, what)) }),
        )?;
    }
    Ok(r)
}



pub fn install(service: &Arc<Service>) -> Result<(), RouteDeclarationError> {
    let weak = Arc::downgrade(service);
    let gate = RouteGate::new(move || weak.upgrade().is_some_and(|s| crate::rust_extension::available(&s)));
    service.register_gated_route_table(TABLE_KEY, routes()?, OWNER, gate)?;
    install_capture_root(service);
    Ok(())
}

