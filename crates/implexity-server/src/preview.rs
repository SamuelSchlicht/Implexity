// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::http::{Reply, RouteError, body_get, json_ascii, py_float, py_truthy};
use crate::jobs::{CancelToken, Job, JobError, ResultCache};
use crate::lod::{self, Budget, Lod, py_round};

pub use implexity_mesh::surface::{PreviewMesh, encode_mesh, encode_polylines};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldPayload {
    pub values: Vec<f32>,
    pub shape: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewResult {
    pub meta: Map<String, Value>,
    pub polylines: Option<Arc<Vec<Vec<[f32; 3]>>>>,
    pub png: Option<Arc<Vec<u8>>>,
    pub mesh: Option<Arc<PreviewMesh>>,
    pub field: Option<Arc<FieldPayload>>,
}

impl PreviewResult {
    #[must_use]
    pub fn from_meta(meta: Map<String, Value>) -> Self {
        Self { meta, ..Self::default() }
    }

    #[must_use]
    pub fn rough_bytes(&self) -> u64 {
        let mut n = 512usize;
        if let Some(p) = &self.polylines {
            n += p.iter().map(|l| l.len() * 12).sum::<usize>();
        }
        if let Some(png) = &self.png {
            n += png.len();
        }
        if let Some(m) = &self.mesh {
            n +=
                m.vertices.len() * 12 + m.normals.len() * 12 + m.triangles.len() * 12 + m.attribute.len() * 4;
        }
        if let Some(f) = &self.field {
            n += f.values.len() * 4;
        }
        n as u64
    }

    fn inline(&mut self, with_field: bool) {
        if let Some(p) = self.polylines.take() {
            let lines: Vec<Value> = p
                .iter()
                .map(|l| Value::Array(l.iter().map(|v| json!([v[0], v[1], v[2]])).collect()))
                .collect();
            self.meta.insert("polylines".into(), Value::Array(lines));
        }
        if let Some(m) = self.mesh.take() {
            let flat3 =
                |a: &[[f32; 3]]| -> Value { a.iter().flat_map(|v| v.iter().map(|x| json!(x))).collect() };
            self.meta.insert("vertices".into(), flat3(&m.vertices));
            self.meta.insert("normals".into(), flat3(&m.normals));
            self.meta.insert(
                "triangles".into(),
                m.triangles.iter().flat_map(|t| t.iter().map(|x| json!(x))).collect(),
            );
            if !m.attribute.is_empty() {
                self.meta.insert("material".into(), m.attribute.iter().map(|x| json!(x)).collect());
            }
        }
        match self.field.take() {
            Some(f) if with_field => {
                self.meta.insert("field".into(), f.values.iter().map(|x| json!(x)).collect());
                self.meta.insert("field_shape".into(), json!(f.shape));
            }
            _ => {}
        }
        if let Some(png) = self.png.take() {
            self.meta
                .insert("png_base64".into(), json!(base64::engine::general_purpose::STANDARD.encode(&*png)));
        }
    }
}

fn metadata_headers(meta: &Map<String, Value>) -> Vec<(String, String)> {
    vec![("X-Implexity-Meta".to_owned(), json_ascii(&Value::Object(meta.clone())))]
}

fn ms(d: Duration) -> f64 {
    py_round(d.as_secs_f64() * 1000.0, 2)
}



pub fn finish(job: &Job<PreviewResult>, req: &Value) -> Result<Reply, RouteError> {
    let timeout = match body_get(req, "timeout_s")? {
        Some(v) => py_float(v)?,
        None => 30.0,
    };
    let wait = if timeout.is_finite() && timeout > 0.0 {
        Duration::try_from_secs_f64(timeout).unwrap_or(Duration::MAX)
    } else {
        Duration::ZERO
    };
    if !job.wait(wait) {
        job.cancel();
        return Ok(Reply::err(504, "preview timed out", json!({"timeout_s": timeout})));
    }
    let Some((outcome, times)) = job.take_outcome() else {
        return Ok(Reply::err(500, "preview result already consumed", Value::Null));
    };
    let mut res = match outcome {
        Err(JobError::Superseded(_)) => {
            return Ok(Reply::json(&json!({"superseded": true, "seq": job.seq()}), 409));
        }
        Err(JobError::NotImplemented(m)) => return Ok(Reply::err(501, &m, Value::Null)),
        Err(JobError::Case(problems)) => {
            return Ok(Reply::json(&json!({"error": "case rejected", "problems": problems}), 422));
        }
        Err(JobError::Failed { kind, message }) => {
            return Ok(Reply::err(500, &message, json!(format!("{kind}: {message}\n"))));
        }
        Ok(r) => r,
    };
    res.meta.insert("queue_ms".into(), json!(ms(times.started.saturating_duration_since(times.submitted))));
    res.meta.insert("total_ms".into(), json!(ms(times.ended.saturating_duration_since(times.submitted))));
    let fmt = body_get(req, "format")?.cloned().unwrap_or_else(|| json!("auto"));
    if fmt == "png"
        && let Some(png) = res.png.take()
    {
        let headers = metadata_headers(&res.meta);
        return Ok(Reply::send(200, png.to_vec(), "image/png", headers));
    }
    if fmt == "binary" {
        if let Some(mesh) = res.mesh.take() {
            return Ok(Reply::send(
                200,
                encode_mesh(&mesh),
                "application/octet-stream",
                metadata_headers(&res.meta),
            ));
        }
        if let Some(polys) = res.polylines.take() {
            let headers = metadata_headers(&res.meta);
            return Ok(Reply::send(200, encode_polylines(&polys), "application/octet-stream", headers));
        }
    }
    res.inline(true);
    Ok(Reply::json(&Value::Object(res.meta), 200))
}

#[must_use]
pub fn ws_result(job: &Job<PreviewResult>, id: &Value) -> Option<Value> {
    if !job.wait(Duration::from_mins(1)) {
        return None;
    }
    let (outcome, _) = job.take_outcome()?;
    Some(match outcome {
        Err(JobError::Superseded(_)) => json!({"id": id, "superseded": true}),
        Err(e) => json!({"id": id, "error": e.to_string()}),
        Ok(mut res) => {
            res.inline(false);
            res.meta.insert("id".into(), id.clone());
            Value::Object(res.meta)
        }
    })
}

pub struct PreviewContext<'a> {
    pub cancel: &'a CancelToken,
    pub budget: &'a Mutex<Budget>,
    pub cache: &'a ResultCache<PreviewResult>,
}

impl PreviewContext<'_> {


    pub fn pick_lod(&self, req: &Value, extent_mm: &[f64], thickness: i64) -> Result<&'static Lod, JobError> {
        let field = |k: &str| body_get(req, k).map_err(|e| JobError::failed("AttributeError", e.to_string()));
        if let Some(name) = field("lod")?.filter(|v| py_truthy(v) && *v != "auto") {
            let name = match name {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return Ok(lod::get(&name));
        }
        let target = match field("target_ms")? {
            Some(v) => py_float(v).map_err(|e| JobError::failed("ValueError", e.to_string()))?,
            None => 60.0,
        };
        let name_of = |v: Option<&Value>, default: &str| match v {
            None => default.to_owned(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        let floor = name_of(field("lod_floor")?, "drag");
        let ceiling = name_of(field("lod_ceiling")?, "read");
        self.budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .choose(extent_mm, target, thickness, &floor, &ceiling)
            .map_err(|e| JobError::failed("ValueError", e.to_string()))
    }

    pub fn observe(&self, stride: i64, n_core: f64, seconds: f64) {
        self.budget.lock().unwrap_or_else(PoisonError::into_inner).observe(stride, n_core, seconds);
    }



    pub fn cached<F>(
        &self,
        kind: &str,
        design_version: i64,
        backend: &str,
        keyparts: &[String],
        use_cache: bool,
        build: F,
    ) -> Result<PreviewResult, JobError>
    where
        F: FnOnce() -> Result<PreviewResult, JobError>,
    {
        if !use_cache {
            let mut out = build()?;
            out.meta.insert("cache".into(), json!("off"));
            return Ok(out);
        }
        let mut parts = vec![kind.to_owned(), design_version.to_string(), backend.to_owned()];
        parts.extend(keyparts.iter().cloned());
        let key = ResultCache::<PreviewResult>::key(&parts);
        if let Some(mut hit) = self.cache.get(&key) {
            hit.meta.insert("cache".into(), json!("hit"));
            return Ok(hit);
        }
        let mut out = build()?;
        out.meta.insert("cache".into(), json!("miss"));
        let bytes = out.rough_bytes();
        self.cache.put(&key, out.clone(), bytes);
        Ok(out)
    }
}


pub trait PreviewBackend: Send + Sync {
    fn name(&self) -> String;
    fn design_version(&self) -> i64;


    fn model_info(&self) -> Result<Value, JobError>;


    fn fidelity(&self) -> Result<Value, JobError>;


    fn set_params(&self, req: &Value) -> Result<Value, JobError>;


    fn field_record(&self, req: &Value) -> Result<Value, JobError>;


    fn section(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError>;


    fn slab(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError>;


    fn probe(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError>;


    fn compare(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewOp {
    Section,
    Slab,
    Probe,
    Compare,
}

impl PreviewOp {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Slab => "slab",
            Self::Probe => "probe",
            Self::Compare => "compare",
        }
    }



    pub fn run(
        self,
        backend: &dyn PreviewBackend,
        req: &Value,
        ctx: &PreviewContext<'_>,
    ) -> Result<PreviewResult, JobError> {
        match self {
            Self::Section => backend.section(req, ctx),
            Self::Slab => backend.slab(req, ctx),
            Self::Probe => backend.probe(req, ctx),
            Self::Compare => backend.compare(req, ctx),
        }
    }
}

