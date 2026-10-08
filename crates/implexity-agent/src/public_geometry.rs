// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use crate::{AgentError, AgentManager, AgentModel, AgentResult, HostOp};
use base64::Engine as _;
use implexity_geometry::{
    NodeRef,
    eval::{self, EvalOptions},
    node::Mode,
};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static EXPORT_ID: AtomicU64 = AtomicU64::new(0);
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_POINTS: u64 = 2_097_152;

fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::contract(message)
}
fn failure(error: impl std::fmt::Display) -> AgentError {
    AgentError::refused(error.to_string())
}
fn only(p: &Map<String, Value>, keys: &[&str]) -> AgentResult<()> {
    if let Some(key) = p.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(invalid(format!("unknown argument {key}")));
    }
    Ok(())
}
fn vector(value: &Value) -> AgentResult<[f64; 3]> {
    let a = value
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or_else(|| invalid("expected three finite coordinates"))?;
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = a[i]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| invalid("expected three finite coordinates"))?;
    }
    Ok(out)
}
fn bbox(value: &Value) -> AgentResult<([f64; 3], [f64; 3])> {
    let a = value
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or_else(|| invalid("bbox_mm requires two coordinate triples"))?;
    let lo = vector(&a[0])?;
    let hi = vector(&a[1])?;
    if (0..3).any(|i| lo[i] >= hi[i]) {
        return Err(invalid("bbox_mm maxima must exceed minima"));
    }
    Ok((lo, hi))
}
fn integer(
    p: &Map<String, Value>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> AgentResult<u64> {
    let n = match p.get(key) {
        None => default,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| invalid(format!("{key} must be an integer")))?,
    };
    if n < min || n > max {
        return Err(invalid(format!("{key} must be in {min}..{max}")));
    }
    Ok(n)
}

pub(crate) fn handles(action: &str) -> bool {
    matches!(
        action,
        "inspect_geometry_kinds"
            | "inspect_model_document"
            | "evaluate_geometry"
            | "export_geometry"
            | "import_mesh_geometry"
            | "prepare_run_comparison"
            | "check_gradients"
    )
}

pub(crate) fn validate(action: &str, p: &Map<String, Value>) -> AgentResult<()> {
    let common = ["source", "node", "mode", "expected_model"];
    let mut keys = common.to_vec();
    match action {
        "inspect_geometry_kinds" => keys.clear(),
        "inspect_model_document" => keys = vec!["source", "expected_model", "maximum_bytes"],
        "evaluate_geometry" => keys.extend([
            "points_mm",
            "grid_n",
            "bbox_mm",
            "pad_mm",
            "smooth_r_mm",
            "spatial_gradients",
            "maximum_points",
        ]),
        "export_geometry" => keys.extend([
            "format",
            "bbox_mm",
            "spacing_mm",
            "pad_mm",
            "step_schema",
            "merge_coplanar",
            "maximum_bytes",
        ]),
        "prepare_run_comparison" => keys = vec!["job_ids", "render_settings"],
        "import_mesh_geometry" => {
            keys = vec![
                "filename",
                "data_base64",
                "units",
                "spacing_mm",
                "feature_id",
                "make_root",
                "expected_model",
                "measurement_samples",
            ]
        }
        "check_gradients" => keys = vec!["spec", "slot", "index", "step", "tolerance"],
        _ => return Err(invalid("unknown geometry action")),
    }
    only(p, &keys)?;
    for key in ["node", "slot"] {
        if let Some(v) = p.get(key) {
            if !v.as_str().is_some_and(|s| !s.is_empty()) {
                return Err(invalid(format!("{key} must be nonempty text")));
            }
        }
    }
    if let Some(v) = p.get("mode") {
        if !v.as_str().is_some_and(|s| ["exact", "smooth"].contains(&s)) {
            return Err(invalid("mode must be exact or smooth"));
        }
    }
    for key in ["spacing_mm", "smooth_r_mm", "step", "tolerance"] {
        if let Some(v) = p.get(key) {
            if !v.as_f64().is_some_and(|n| n.is_finite() && n > 0.0) {
                return Err(invalid(format!("{key} must be finite and positive")));
            }
        }
    }
    if let Some(v) = p.get("pad_mm") {
        if !v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) {
            return Err(invalid("pad_mm must be finite and nonnegative"));
        }
    }
    for key in ["merge_coplanar", "spatial_gradients"] {
        if p.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(invalid(format!("{key} must be boolean")));
        }
    }
    if let Some(v) = p.get("step_schema") {
        if !v
            .as_str()
            .is_some_and(|s| ["AP203", "AP214", "AP242"].contains(&s))
        {
            return Err(invalid("unknown STEP schema"));
        }
    }
    if let Some(v) = p.get("source") {
        let s = v
            .as_object()
            .ok_or_else(|| invalid("source must be an object"))?;
        match s.get("kind").and_then(Value::as_str) {
            Some("current") => only(s, &["kind"])?,
            Some("optimization_epoch") => {
                only(s, &["kind", "job_id", "epoch"])?;
                if !s.get("job_id").is_some_and(crate::pyval::is_job_id) {
                    return Err(invalid("source job_id must identify an optimization job"));
                }
                integer(s, "epoch", u64::MAX, 0, i64::MAX as u64)?;
            }
            _ => return Err(invalid("source kind must be current or optimization_epoch")),
        }
    }
    if let Some(v) = p.get("expected_model") {
        let e = v
            .as_object()
            .ok_or_else(|| invalid("expected_model must be an identity object"))?;
        only(e, &["structure_id", "content_id"])?;
        if ["structure_id", "content_id"].iter().any(|k| {
            !e.get(*k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        }) {
            return Err(invalid(
                "expected_model requires both model identity strings",
            ));
        }
    }
    if p.contains_key("bbox_mm") {
        bbox(&p["bbox_mm"])?;
    }
    integer(p, "maximum_bytes", 16 * 1024 * 1024, 1, MAX_BYTES)?;
    if action == "evaluate_geometry" {
        if p.contains_key("points_mm") == p.contains_key("grid_n") {
            return Err(invalid("supply exactly one of points_mm or grid_n"));
        }
        let maximum = integer(p, "maximum_points", 262_144, 1, MAX_POINTS)?;
        if let Some(v) = p.get("points_mm") {
            if p.contains_key("bbox_mm") || p.contains_key("pad_mm") {
                return Err(invalid("bbox_mm and pad_mm apply only to a grid"));
            }
            let a = v
                .as_array()
                .filter(|a| !a.is_empty())
                .ok_or_else(|| invalid("points_mm must be a nonempty array"))?;
            if a.len() as u64 > maximum {
                return Err(invalid("points_mm exceeds maximum_points"));
            }
            for point in a {
                vector(point)?;
            }
        } else {
            let n = integer(p, "grid_n", 0, 2, 128)?;
            if n.pow(3) > maximum {
                return Err(invalid("grid exceeds maximum_points"));
            }
            if !p.contains_key("bbox_mm") {
                return Err(invalid("grid evaluation requires bbox_mm"));
            }
        }
    }
    if action == "export_geometry" {
        if !p
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|s| implexity_mesh::interop::EXPORT_FORMATS.contains(&s))
        {
            return Err(invalid("format must be stl, ply, 3mf or step"));
        }
        if !p.contains_key("bbox_mm") {
            return Err(invalid("export_geometry requires bbox_mm"));
        }
    }
    if action == "prepare_run_comparison" {
        let ids = p
            .get("job_ids")
            .and_then(Value::as_array)
            .filter(|a| (1..=4).contains(&a.len()))
            .ok_or_else(|| invalid("choose one to four optimization jobs"))?;
        let mut seen = std::collections::BTreeSet::new();
        for id in ids {
            if !crate::pyval::is_job_id(id) || !seen.insert(id.as_str().unwrap_or_default()) {
                return Err(invalid(
                    "job_ids must contain distinct canonical optimization jobs",
                ));
            }
        }
        let mut settings = p.get("render_settings").cloned().unwrap_or_else(
            || json!({"width_px":960,"height_px":540,"quality":"preview","background":"white"}),
        );
        if !settings.is_object()
            || settings.get("source").is_some()
            || settings.get("region").is_some()
        {
            return Err(invalid(
                "comparison render settings must omit source and region",
            ));
        }
        settings["source"] = json!({"kind":"optimization_epoch","job_id":ids[0],"epoch":0});
        implexity_render::render3d::normalise_request(&settings).map_err(failure)?;
    }
    if action == "import_mesh_geometry" {
        let filename = p
            .get("filename")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("filename is required"))?;
        if filename.len() > 256
            || !filename.to_ascii_lowercase().ends_with(".stl")
                && !filename.to_ascii_lowercase().ends_with(".ply")
        {
            return Err(invalid("filename must end in .stl or .ply"));
        }
        let encoded = p
            .get("data_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("data_base64 is required"))?;
        if encoded.is_empty() || encoded.len() > 44_739_244 {
            return Err(invalid("mesh data exceeds 32 MiB"));
        }
        if !p
            .get("units")
            .and_then(Value::as_str)
            .is_some_and(|v| ["mm", "m"].contains(&v))
        {
            return Err(invalid("units must be mm or m"));
        }
        let id = p
            .get("feature_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("feature_id is required"))?;
        if id.is_empty()
            || id.len() > 64
            || !id.chars().enumerate().all(|(i, c)| {
                if i == 0 {
                    c.is_ascii_alphabetic() || c == '_'
                } else {
                    c.is_ascii_alphanumeric() || "_.:-".contains(c)
                }
            })
        {
            return Err(invalid("feature_id must be a valid feature name"));
        }
        if !p.contains_key("spacing_mm") || !p.contains_key("expected_model") {
            return Err(invalid(
                "mesh import requires spacing_mm and expected_model",
            ));
        }
        if p.get("make_root").is_some_and(|v| !v.is_boolean()) {
            return Err(invalid("make_root must be boolean"));
        }
        integer(p, "measurement_samples", 2000, 100, 20000)?;
    }
    if action == "check_gradients" {
        if !p.get("spec").is_some_and(Value::is_object) {
            return Err(invalid("spec must be an optimization job specification"));
        }
        integer(p, "index", 0, 0, i64::MAX as u64)?;
    }
    Ok(())
}

struct ExportDirectory(PathBuf);
impl Drop for ExportDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl AgentManager {
    fn public_source(&self, p: &Map<String, Value>) -> AgentResult<(Arc<dyn AgentModel>, Value)> {
        let source = p
            .get("source")
            .cloned()
            .unwrap_or_else(|| json!({"kind":"current"}));
        let (view, mut identity) = if source["kind"] == "optimization_epoch" {
            let exported = self.host.call(HostOp::ExportEpoch(
                source["job_id"].as_str().unwrap_or_default().into(),
                source["epoch"].as_i64().unwrap_or(0),
            ))?;
            let view = self.host.detached_model(&exported["document"])?;
            let status = view.status().map_err(failure)?;
            if exported["content_id"] != json!(status.content_id) {
                return Err(failure("saved epoch identity drifted"));
            }
            let mut identity = exported
                .as_object()
                .cloned()
                .ok_or_else(|| invalid("epoch export did not return an object"))?;
            identity.remove("document");
            identity.insert("kind".into(), json!("optimization_epoch"));
            (view, Value::Object(identity))
        } else {
            let view = self.require_model()?;
            let status = view.status_value()?;
            if status["loaded"] != json!(true) {
                return Err(invalid("a geometry model must be loaded"));
            }
            let geometry = view
                .geometry_model()
                .ok_or_else(|| invalid("this model has no implicit geometry document"))?;
            let status = view.status().map_err(failure)?;
            let identity = json!({"kind":"current", "structure_id":status.structure_id,"content_id":status.content_id,"document_sha256":view.document_sha256(),"name":geometry.doc.get("name")});
            (view, identity)
        };
        if let Some(expected) = p.get("expected_model") {
            for key in ["structure_id", "content_id"] {
                if expected[key] != identity[key] {
                    return Err(failure(
                        "expected_model does not match the selected geometry source",
                    ));
                }
            }
        }
        identity["live_model_mutated"] = json!(false);
        Ok((view, identity))
    }

    pub(crate) fn public_geometry_action(
        &self,
        action: &str,
        p: &Map<String, Value>,
    ) -> AgentResult<Value> {
        validate(action, p)?;
        if action == "inspect_geometry_kinds" {
            return Ok(
                json!({"schema":"implexity-geometry-kind-catalogue/1","kinds":implexity_geometry::document::model::catalogue(None)}),
            );
        }
        if action == "check_gradients" {
            let _lease = self.host.eval_lock()?;
            return self
                .host
                .call(HostOp::CheckGradients(Value::Object(p.clone())));
        }
        if action == "prepare_run_comparison" {
            let settings = p.get("render_settings").cloned().unwrap_or_else(
                || json!({"width_px":960,"height_px":540,"quality":"preview","background":"white"}),
            );
            let mut jobs = Vec::new();
            for id in p["job_ids"].as_array().into_iter().flatten() {
                let info = self
                    .host
                    .call(HostOp::JobInfo(id.as_str().unwrap_or_default().into()))?;
                let mut epochs: Vec<i64> = info["history"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r["i"].as_i64().filter(|n| *n >= 0))
                    .collect();
                epochs.sort_unstable();
                epochs.dedup();
                jobs.push((
                    id.clone(),
                    info.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(id.as_str().unwrap_or_default())
                        .to_string(),
                    epochs,
                ));
            }
            let common: Vec<i64> = jobs
                .first()
                .map(|(_, _, epochs)| {
                    epochs
                        .iter()
                        .copied()
                        .filter(|e| jobs.iter().all(|(_, _, other)| other.contains(e)))
                        .collect()
                })
                .unwrap_or_default();
            let mut runs = Vec::new();
            for region in ["solid", "complement"] {
                for (id, label, _) in &jobs {
                    let mut options = settings.clone();
                    options["region"] = json!(region);
                    if region == "complement" {
                        options.as_object_mut().unwrap().remove("clip");
                    }
                    let frames: Vec<Value> = common.iter().map(|epoch| json!({"epoch":epoch,"source":{"kind":"optimization_epoch","job_id":id,"epoch":epoch},"render_settings":options})).collect();
                    runs.push(json!({"id":format!("{}-{region}",id.as_str().unwrap_or_default()),"label":format!("{label} / {}",if region=="solid" {"Solid"} else {"Complement"}),"frames":frames}));
                }
            }
            return Ok(
                json!({"schema":"implexity-saved-job-comparison/1","epochs":common,"runs":runs,"live_model_mutated":false}),
            );
        }
        if action == "import_mesh_geometry" {
            let current = self.require_model()?.status_value()?;
            for key in ["structure_id", "content_id"] {
                if current[key] != p["expected_model"][key] {
                    return Err(failure(
                        "the model changed since the mesh import was prepared",
                    ));
                }
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(p["data_base64"].as_str().unwrap_or_default())
                .map_err(|error| invalid(error.to_string()))?;
            if bytes.is_empty() || bytes.len() > 33_554_432 {
                return Err(invalid("mesh data must contain at most 32 MiB"));
            }
            let filename = p["filename"].as_str().unwrap_or_default();
            let options = implexity_mesh::interop::MeshSdfOptions {
                units_mm: p["units"] == "mm",
                spacing_mm: p["spacing_mm"].as_f64().unwrap_or(0.0),
                measure_samples: integer(p, "measurement_samples", 2000, 100, 20000)? as usize,
                name: Some(filename),
                ..Default::default()
            };
            let (node, measurement) = implexity_mesh::interop::mesh_sdf(
                &implexity_mesh::interop::MeshSource::Bytes(&bytes),
                &options,
            )
            .map_err(failure)?;
            let params: Map<String, Value> = node
                .params()
                .iter()
                .map(|(key, value)| (key.clone(), value.to_json()))
                .collect();
            let attrs: Map<String, Value> = node
                .op()
                .doc_attrs()
                .into_iter()
                .map(|(key, value)| (key, value.to_json()))
                .collect();
            let id = p["feature_id"].as_str().unwrap_or_default();
            let mut operations = vec![
                json!({"op":"add_node","id":id,"kind":"mesh_sdf","params":params,"attrs":attrs,"children":[]}),
            ];
            if p.get("make_root") != Some(&json!(false)) {
                operations.push(json!({"op":"set_root","id":id}));
            }
            let result = self.host.call(HostOp::EditGraph(
                json!({"operations":operations,"expected_model":p["expected_model"]}),
            ))?;
            return Ok(
                json!({"schema":"implexity-mesh-geometry-import/1","feature_id":id,"source_sha256":implexity_io::digest::sha256_hex(&bytes),"coordinate_units":"mm","spacing_mm":options.spacing_mm,"measurement":measurement,"edit":result}),
            );
        }
        let (view, identity) = self.public_source(p)?;
        let _guard = view.live_lock();
        let before = view.status().map_err(failure)?;
        if json!(before.content_id) != identity["content_id"] {
            return Err(failure("selected geometry changed before the operation"));
        }
        let model = view
            .geometry_model()
            .ok_or_else(|| invalid("this source has no implicit geometry document"))?;
        if action == "inspect_model_document" {
            let bytes = serde_json::to_vec(&model.doc).map_err(failure)?.len() as u64;
            if bytes > integer(p, "maximum_bytes", 16 * 1024 * 1024, 1, MAX_BYTES)? {
                return Err(invalid("model document exceeds maximum_bytes"));
            }
            return Ok(
                json!({"schema":"implexity-model-document-inspection/1","source":identity,"document":model.doc,"bytes":bytes}),
            );
        }
        let node: NodeRef = match p.get("node").and_then(Value::as_str) {
            Some(name) => model.node(name).map_err(failure)?,
            None => model
                .root()
                .ok_or_else(|| invalid("the selected model has no root"))?,
        };
        if node.walk().iter().any(|(_, n)| n.kind() == "optimize") {
            return Err(invalid(
                "select an authored geometry subtree without optimize nodes; use explicit optimization actions to solve it",
            ));
        }
        let mode = if p.get("mode").and_then(Value::as_str) == Some("smooth") {
            Mode::Smooth
        } else {
            Mode::Exact
        };
        let result = if action == "evaluate_geometry" {
            let (points, shape, bounds) = if let Some(v) = p.get("points_mm") {
                let points = v
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(vector)
                    .collect::<AgentResult<Vec<_>>>()?;
                let n = points.len();
                (points, json!([n]), Value::Null)
            } else {
                let n = p["grid_n"].as_u64().unwrap() as usize;
                let (mut lo, mut hi) = bbox(&p["bbox_mm"])?;
                let pad = p.get("pad_mm").and_then(Value::as_f64).unwrap_or(0.0);
                for i in 0..3 {
                    lo[i] -= pad;
                    hi[i] += pad;
                }
                if lo.iter().chain(hi.iter()).any(|v| !v.is_finite()) {
                    return Err(invalid("padded bbox_mm is not finite"));
                }
                let mut points = Vec::with_capacity(n * n * n);
                for i in 0..n {
                    for j in 0..n {
                        for k in 0..n {
                            points.push(std::array::from_fn(|a| {
                                let index = [i, j, k][a];
                                lo[a] + (hi[a] - lo[a]) * (index as f64) / (n - 1) as f64
                            }));
                        }
                    }
                }
                (points, json!([n, n, n]), json!([lo, hi]))
            };
            let options = EvalOptions {
                mode,
                smooth_r_mm: p.get("smooth_r_mm").and_then(Value::as_f64),
                ..EvalOptions::default()
            };
            let values = eval::eval_points(&node, &points, &options).map_err(failure)?;
            if values.iter().any(|v| !v.is_finite()) {
                return Err(failure("geometry evaluation returned nonfinite values"));
            }
            let gradients = if p.get("spatial_gradients") == Some(&json!(true)) {
                let g = eval::grad_x(&node, &points, &options).map_err(failure)?;
                if g.iter().flatten().any(|v| !v.is_finite()) {
                    return Err(failure("geometry evaluation returned nonfinite gradients"));
                }
                json!(g)
            } else {
                Value::Null
            };
            json!({"schema":"implexity-geometry-evaluation/1","source":identity,"node":p.get("node"),"shape":shape,"order":"C","mode":if mode==Mode::Exact {"exact"} else {"smooth"},"coordinate_units":"mm","field_units":"mm","gradient_units":"1","bbox_mm":bounds,"values":values,"spatial_gradients":gradients,"field_class":eval::field_class_of(&node,mode).map_err(failure)?.as_json()})
        } else {
            let format = p["format"].as_str().unwrap();
            let root = self.host.state_dir().join("exports");
            std::fs::create_dir_all(&root).map_err(failure)?;
            let path = root.join(format!(
                "{}-{}",
                std::process::id(),
                EXPORT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).map_err(failure)?;
            let directory = ExportDirectory(path);
            let path = directory.0.join(format!("geometry.{format}"));
            let options = implexity_mesh::interop::ExportOptions {
                fmt: Some(format),
                spacing_mm: p.get("spacing_mm").and_then(Value::as_f64),
                bbox_mm: Some(bbox(&p["bbox_mm"])?),
                pad_mm: p.get("pad_mm").and_then(Value::as_f64),
                mode,
                step: implexity_mesh::step::StepOptions {
                    schema: p
                        .get("step_schema")
                        .and_then(Value::as_str)
                        .unwrap_or("AP214"),
                    merge: p
                        .get("merge_coplanar")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    ..Default::default()
                },
                ..Default::default()
            };
            let report =
                implexity_mesh::interop::export_subtree(&node, &path, &options).map_err(failure)?;
            if !path.is_file() {
                return Ok(
                    json!({"schema":"implexity-geometry-export/1","source":identity,"report":report,"delivery":{"files":[]}}),
                );
            }
            let length = std::fs::metadata(&path).map_err(failure)?.len();
            if length > integer(p, "maximum_bytes", 16 * 1024 * 1024, 1, MAX_BYTES)? {
                return Err(invalid(
                    "export exceeds maximum_bytes; choose a coarser spacing or a larger explicit delivery bound",
                ));
            }
            let bytes = std::fs::read(&path).map_err(failure)?;
            let mime = match format {
                "stl" => "model/stl",
                "ply" => "application/octet-stream",
                "3mf" => "model/3mf",
                _ => "application/step",
            };
            json!({"schema":"implexity-geometry-export/1","source":identity,"report":report,"delivery":{"files":[{"filename":format!("geometry.{format}"),"mime_type":mime,"bytes":length,"sha256":implexity_io::digest::sha256_hex(&bytes),"schema":"implexity-inline-file/1","data_base64":base64::engine::general_purpose::STANDARD.encode(&bytes)}]}})
        };
        if view.status().map_err(failure)?.content_id != before.content_id {
            return Err(failure(
                "geometry changed while the operation was executing",
            ));
        }
        Ok(result)
    }
}
