// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::MeshError;
use crate::bodyexport::{
    self as bx, BodyEvaluator, BoxCap, BuildBodyOptions, Cap, Components, DesignSnapshot, ExtractOptions,
    MeshCap, RecipeWriter, SdfKernel,
};
use crate::numeric::py_round_digits;
use crate::pyfmt::{fmt_e, fmt_f, fmt_g};
use crate::topology::{Tri, Vec3};
use implexity_core::py_repr::{repr_float, repr_str};
use implexity_core::pyobj::{py_str, truthy};

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(default)
}

#[must_use]
pub fn min_tolerance_mm() -> f64 {
    env_f64("IMPLEXITY_MIN_BODY_TOLERANCE", 0.001)
}
#[must_use]
pub fn step_cap_merge_ratio() -> f64 {
    env_f64("IMPLEXITY_STEP_CAP_RATIO", 6.5)
}
#[must_use]
pub fn step_lattice_merge_ratio() -> f64 {
    env_f64("IMPLEXITY_STEP_LATTICE_RATIO", 1.02)
}
#[must_use]
pub fn step_cap_tri_per_area() -> f64 {
    env_f64("IMPLEXITY_STEP_CAP_TRI_PER_AREA", 0.80)
}
#[must_use]
pub fn step_bytes_per_face() -> f64 {
    env_f64("IMPLEXITY_STEP_BYTES_PER_FACE", 2550.0)
}

#[must_use]
pub fn ctype_for(ext: &str) -> String {
    let mut map = crate::exporters::mime_types();
    map.insert("json".into(), "application/json".into());
    let step = map.get("step").cloned().unwrap_or_else(|| "application/step".into());
    map.entry("stp".into()).or_insert(step);
    map.get(ext).cloned().unwrap_or_else(|| "application/octet-stream".into())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BodyError {
    #[error("{}", implexity_core::error::CaseError::new(.0.clone()))]
    Case(Vec<String>),
    #[error("{0}")]
    NotFound(String),
    #[error("{}", repr_str(.0))]
    Key(String),
    #[error("{0}")]
    Failed(String),
}

impl From<MeshError> for BodyError {
    fn from(e: MeshError) -> Self {
        match e {
            MeshError::Case(p) => Self::Case(p),
            MeshError::Key(k) => Self::Key(k),
            other => Self::Failed(other.to_string()),
        }
    }
}

fn case(msg: impl Into<String>) -> BodyError {
    BodyError::Case(vec![msg.into()])
}

#[derive(Clone, Debug, Default)]
pub struct PartDomain {
    pub vertices: Vec<Vec3>,
    pub faces: Vec<Tri>,
    pub mesh_path: String,
    pub labels: Vec<i64>,
    pub grid_h: f64,
    pub groups: Vec<Value>,
    pub current: Value,
}

pub type SharedRecipeWriter =
    Arc<dyn Fn(&DesignSnapshot, &Value, &Path) -> Result<Option<Value>, String> + Send + Sync>;

pub type SharedEvaluator = Arc<dyn BodyEvaluator + Send + Sync>;
pub type BodyWork =
    Box<dyn FnOnce(&(dyn Fn() -> bool + Sync)) -> Result<Map<String, Value>, BodyError> + Send>;
pub type BodyDone = Box<dyn FnOnce(Result<Map<String, Value>, BodyError>) + Send>;

pub trait BodyHost: Send + Sync + 'static {
    fn current_case(&self) -> Value;

    fn part_domain(&self) -> Result<Option<PartDomain>, String>;
    fn domain_mm(&self) -> Vec3;

    fn served_evaluator(&self) -> Result<SharedEvaluator, BodyError>;
    fn backend_name(&self) -> String;

    fn load_design(&self, path: &Path) -> Result<SharedEvaluator, BodyError>;
    fn design_params(&self, ev: &dyn BodyEvaluator, snap: &DesignSnapshot) -> BTreeMap<String, ArrayD<f64>>;
    fn sdf_kernel(&self) -> Option<Arc<dyn SdfKernel + Send + Sync>>;
    fn recipe_writer(&self) -> Option<SharedRecipeWriter>;
    fn eval_lock(&self) -> crate::model_view::LiveGuard<'_>;
    fn submit(&self, channel: &str, seq: i64, work: BodyWork, done: BodyDone);
    fn broadcast(&self, event: Value);
    fn service_version(&self) -> Option<String>;
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn job_id() -> String {
    let bytes = implexity_io::atomic::os_random_bytes(6).unwrap_or_else(|_| {
        let t = implexity_io::atomic::unique_token();
        t.bytes().take(6).collect()
    });
    hex::encode(bytes)
}

#[derive(Debug)]
pub struct BodyJob {
    pub id: String,
    pub channel: String,
    pub seq: i64,
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub report: Option<Map<String, Value>>,
    pub error: Option<String>,
    pub t_submit: f64,
    pub t_start: Option<f64>,
    pub t_end: Option<f64>,
    pub req: Value,
    pub out_dir: PathBuf,
    pub cancelled: Arc<AtomicBool>,
    pub record: Option<Value>,
}

impl BodyJob {
    #[must_use]
    pub fn record_pointer(&self) -> Value {
        let Some(r) = &self.record else { return Value::Null };
        let embedded: Map<String, Value> = r
            .get("stamped")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.get("carries").cloned().unwrap_or_else(|| json!("nothing"))))
                    .collect()
            })
            .unwrap_or_default();
        json!({"schema": r["schema"], "record_id": r["record_id"], "url": format!("/v1/body/jobs/{}/record", self.id),
               "sidecar": r.get("links").and_then(|l| l.get("sidecar")).cloned().unwrap_or(Value::Null),
               "embedded": embedded,
               "warnings": r.get("warnings").filter(|w| truthy(w)).cloned().unwrap_or_else(|| json!([]))})
    }

    #[must_use]
    pub fn as_dict(&self, full: bool) -> Value {
        let t = now();
        let mut d = Map::new();
        d.insert("job_id".into(), json!(self.id));
        d.insert("channel".into(), json!(self.channel));
        d.insert("seq".into(), json!(self.seq));
        d.insert("status".into(), json!(self.status));
        d.insert("progress".into(), json!(py_round_digits(self.progress, 3)));
        d.insert("message".into(), json!(self.message));
        d.insert("request".into(), self.req.clone());
        d.insert("queued_s".into(), json!(py_round_digits(self.t_start.unwrap_or(t) - self.t_submit, 2)));
        if let Some(ts) = self.t_start.filter(|v| *v != 0.0) {
            d.insert("elapsed_s".into(), json!(py_round_digits(self.t_end.unwrap_or(t) - ts, 1)));
        }
        if let Some(e) = &self.error {
            d.insert("error".into(), json!(e));
        }
        if let Some(rep) = &self.report {
            d.insert("accepted".into(), rep.get("accepted").cloned().unwrap_or(Value::Null));
            let mut files = Map::new();
            if let Some(fs) = rep.get("files").and_then(Value::as_object) {
                for (k, v) in fs {
                    if let (Value::Object(o), Some(p)) = (v, v.get("path").and_then(Value::as_str)) {
                        let mut e = o.clone();
                        e.insert(
                            "download".into(),
                            json!(format!("/v1/body/jobs/{}/files/{}", self.id, basename(p))),
                        );
                        files.insert(k.clone(), Value::Object(e));
                    }
                }
            }
            d.insert("files".into(), Value::Object(files));
            if full {
                d.insert("report".into(), Value::Object(rep.clone()));
            } else {
                d.insert("provenance".into(), rep.get("provenance").cloned().unwrap_or(Value::Null));
            }
        }
        if self.record.is_some() {
            d.insert("record".into(), self.record_pointer());
        }
        Value::Object(d)
    }
}

fn basename(p: &str) -> String {
    Path::new(p).file_name().map_or_else(|| p.to_string(), |s| s.to_string_lossy().into_owned())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

enum CapSpec {
    Box(BoxCap),
    Mesh(PartDomain),
}

type EstimateKey = (String, i64, String, String);

pub struct BodyManager<H: BodyHost> {
    host: Arc<H>,
    pub state_dir: PathBuf,
    pub out_root: PathBuf,
    jobs: Mutex<Vec<Arc<Mutex<BodyJob>>>>,
    est_cache: Mutex<Vec<(EstimateKey, Map<String, Value>)>>,
}

impl<H: BodyHost> BodyManager<H> {

    pub fn new(host: Arc<H>, state_dir: &Path) -> Result<Arc<Self>, BodyError> {
        let out_root = state_dir.join("bodies");
        std::fs::create_dir_all(&out_root)
            .map_err(|e| BodyError::Failed(format!("creating {}: {e}", out_root.display())))?;
        Ok(Arc::new(Self {
            host,
            state_dir: state_dir.to_path_buf(),
            out_root,
            jobs: Mutex::new(Vec::new()),
            est_cache: Mutex::new(Vec::new()),
        }))
    }

    fn find(&self, id: &str) -> Option<Arc<Mutex<BodyJob>>> {
        lock(&self.jobs).iter().find(|j| lock(j).id == id).cloned()
    }

    fn cap(&self, domain_aware: bool) -> Result<(CapSpec, Value, &'static str), BodyError> {
        let case_doc = self.host.current_case();
        let mesh = case_doc
            .get("domain")
            .filter(|d| truthy(d))
            .and_then(|d| d.get("mesh"))
            .cloned()
            .unwrap_or(Value::Null);
        if domain_aware && truthy(&mesh) {
            let ctx = match self.host.part_domain() {
                Ok(Some(ctx)) => ctx,
                Ok(None) => {
                    return Err(self.cap_refusal(&mesh, "the selected physics backend holds no part mesh"));
                }
                Err(e) => return Err(self.cap_refusal(&mesh, &e)),
            };
            let fg = json!({"group_of_triangle": ctx.labels, "band_m": ctx.grid_h * 0.25,
                            "groups": ctx.groups.iter().map(|g| json!({"id": g.get("id"), "area_m2": g.get("area_m2"),
                                                                        "normal": g.get("normal")})).collect::<Vec<_>>()});
            return Ok((CapSpec::Mesh(ctx), fg, "part"));
        }
        let dom = self.host.domain_mm().map(|v| v * bx::MM);
        Ok((CapSpec::Box(BoxCap { lo: [0.0; 3], hi: dom }), json!({}), "envelope box"))
    }

    #[allow(clippy::unused_self)]
    fn cap_refusal(&self, mesh: &Value, exc: &str) -> BodyError {
        case(format!(
            "domain_aware requested and the case names a domain mesh ({}) but it could not be loaded: {exc}. Upload \
             the part first, or send domain_aware=false to cap on the envelope box",
            py_str(mesh)
        ))
    }

    fn design_for(&self, which: &Value) -> Result<(SharedEvaluator, String), BodyError> {
        match which {
            Value::Null => Ok((self.host.served_evaluator()?, "current".into())),
            Value::String(s) if s.is_empty() || s == "current" => {
                Ok((self.host.served_evaluator()?, "current".into()))
            }
            other => {
                let raw = py_str(other);
                let p = std::path::absolute(&raw).unwrap_or_else(|_| PathBuf::from(&raw));
                if !p.is_file() {
                    return Err(case(format!("design {}: no such file", implexity_core::pyobj::repr(other))));
                }
                Ok((self.host.load_design(&p)?, p.display().to_string()))
            }
        }
    }

    fn formats(req: &Value) -> Result<Vec<String>, BodyError> {
        let f = req.get("format").cloned().unwrap_or_else(|| json!("stl"));
        let fmts: Vec<String> = match &f {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a.iter().map(py_str).collect(),
            other => vec![py_str(other)],
        };
        let known = crate::exporters::names();
        let bad: Vec<&String> = fmts.iter().filter(|x| !known.contains(x)).collect();
        if !bad.is_empty() {
            return Err(case(format!(
                "format {}: formats are {}",
                implexity_core::pyobj::list_repr(&bad),
                known.join(", ")
            )));
        }
        Ok(fmts)
    }

    fn tolerance(req: &Value) -> Result<f64, BodyError> {
        let raw = req.get("tolerance_mm").cloned().unwrap_or_else(|| json!(0.02));
        let t = py_float(&raw)?;
        let floor = min_tolerance_mm();
        if t.partial_cmp(&floor).is_none_or(std::cmp::Ordering::is_lt) {
            return Err(case(format!(
                "tolerance_mm {}: below this service's floor of {} mm (the grid it needs grows as 1/tolerance^1.5; \
                 raise IMPLEXITY_MIN_BODY_TOLERANCE and IMPLEXITY_MAX_BODY_SAMPLES together if you mean it)",
                repr_float(t),
                repr_float(floor)
            )));
        }
        Ok(t)
    }

    fn components(req: &Value) -> Result<String, BodyError> {
        let c = req.get("components").cloned().unwrap_or_else(|| json!("all"));
        if c == "all" {
            return Ok("all".into());
        }
        let s = py_str(&c);
        if !s.starts_with("connected_to:") {
            return Err(case(format!(
                "components {}: 'all' or 'connected_to:<face_group>' (a face-group id on a part domain, or one of x_lo \
                 x_hi y_lo y_hi z_lo z_hi on a box)",
                implexity_core::pyobj::repr(&c)
            )));
        }
        Ok(s)
    }

    fn make_cap<'k>(
        spec: &CapSpec,
        kernel: Option<&'k (dyn SdfKernel + Send + Sync)>,
    ) -> Result<Cap<'k>, BodyError> {
        match spec {
            CapSpec::Box(b) => Ok(Cap::Box(b.clone())),
            CapSpec::Mesh(ctx) => {
                let k = kernel.ok_or_else(|| {
                    BodyError::Failed("a part-domain cap needs the domain SDF kernel".into())
                })?;
                Ok(Cap::Mesh(MeshCap::new(
                    ctx.vertices.clone(),
                    ctx.faces.clone(),
                    &basename(&ctx.mesh_path),
                    k,
                )))
            }
        }
    }


    #[allow(clippy::too_many_lines)]
    pub fn estimate(&self, req: &Value) -> Result<Value, BodyError> {
        let (ev, dname) = self.design_for(req.get("design").unwrap_or(&Value::Null))?;
        let domain_aware = req.get("domain_aware").is_none_or(truthy);
        let (spec, _fg, capname) = self.cap(domain_aware)?;
        let snap = ev.snapshot();
        let key: EstimateKey = (
            dname.clone(),
            snap.version,
            capname.to_string(),
            repr_float(py_round_digits(ev.h_design(), 12)),
        );
        let hit = lock(&self.est_cache).iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let hit = if let Some(h) = hit {
            h
        } else {
            let kernel = self.host.sdf_kernel();
            let cap = Self::make_cap(&spec, kernel.as_deref())?;
            let (d1, d2) = bx::calibration_divisors(ev.as_ref(), &cap);
            let mut levels: Vec<Map<String, Value>> = Vec::new();
            {
                let _guard = self.host.eval_lock();
                for div in [d1, d2] {
                    let x = bx::extract(
                        ev.as_ref(),
                        &cap,
                        ev.h_design() * 1e3 / div,
                        &ExtractOptions::default(),
                    )?;
                    let sc = &x.stats;
                    let chord = sc.get("chord").ok_or_else(|| {
                        BodyError::Failed(
                            "a calibration extraction produced no triangles, so no chord could be measured"
                                .into(),
                        )
                    })?;
                    let mut l = Map::new();
                    for (k, v) in [
                        ("spacing_mm", sc["spacing_mm"].clone()),
                        ("triangles", sc["triangles"].clone()),
                        ("chord_area_weighted_mm", chord["area_weighted_mm"].clone()),
                        ("watertight", sc["watertight"].clone()),
                        ("components", sc["component_count"].clone()),
                        ("genus", sc["topology_genus"].clone()),
                        ("seconds", sc["seconds"].clone()),
                        ("volume", sc["volume"].clone()),
                        ("cap", sc["cap"].clone()),
                    ] {
                        l.insert(k.into(), v);
                    }
                    levels.push(l);
                }
            }
            for level in &levels {
                if level["chord_area_weighted_mm"].is_null() {
                    return Err(BodyError::Case(vec![
                        format!(
                            "the tolerance law cannot be fitted on this design: at the calibration spacing {} mm every \
                             extracted facet belongs to the domain cap, so the lattice chord error is not defined",
                            fmt_f(level["spacing_mm"].as_f64().unwrap_or(0.0), 4)
                        ),
                        format!(
                            "the design's level set contributes no surface at that spacing -- ask for a body directly \
                             with an explicit tolerance_mm, or use a design whose lattice is resolved at h_design/{}",
                            fmt_g(d1, 6)
                        ),
                    ]));
                }
            }
            let lv = |i: usize, k: &str| levels[i][k].as_f64().unwrap_or(0.0);
            let (p_e, q_e) = bx::fit_law(
                lv(0, "spacing_mm"),
                lv(0, "chord_area_weighted_mm"),
                lv(0, "triangles"),
                lv(1, "spacing_mm"),
                lv(1, "chord_area_weighted_mm"),
                lv(1, "triangles"),
            );
            let mut hit = levels[1].clone();
            hit.insert("levels".into(), json!(levels));
            hit.insert("chord_exponent".into(), json!(p_e));
            hit.insert("triangle_exponent".into(), json!(q_e));
            let mut cache = lock(&self.est_cache);
            cache.push((key, hit.clone()));
            if cache.len() > 8 {
                cache.remove(0);
            }
            hit
        };
        let eps0 = hit["chord_area_weighted_mm"].as_f64().unwrap_or(0.0);
        let tol: Vec<f64> = match req.get("tolerances").filter(|t| truthy(t)) {
            Some(Value::Array(a)) => a.iter().map(py_float).collect::<Result<_, _>>()?,
            Some(other) => {
                return Err(BodyError::Failed(format!("tolerances must be a list, not {}", py_str(other))));
            }
            None => vec![0.05, 0.02, 0.01, 0.005],
        };
        let mut table = bx::extrapolate(
            hit["spacing_mm"].as_f64().unwrap_or(0.0),
            eps0,
            hit["triangles"].as_f64().unwrap_or(0.0),
            &tol,
            hit["chord_exponent"].as_f64().unwrap_or(2.0),
            hit["triangle_exponent"].as_f64().unwrap_or(-2.0),
        );
        let mut cap_frac =
            hit.get("cap").and_then(|c| c.get("area_fraction")).and_then(Value::as_f64).unwrap_or(0.0);
        if cap_frac == 0.0 {
            cap_frac = hit
                .get("chord")
                .and_then(|c| c.get("cap_area_fraction"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
        }
        let (cap_ratio, lat_ratio, per_area, per_face) = (
            step_cap_merge_ratio(),
            step_lattice_merge_ratio(),
            step_cap_tri_per_area(),
            step_bytes_per_face(),
        );
        let dom = self.host.domain_mm();
        let budget = bx::max_samples();
        let max_tri = crate::step::max_step_triangles();
        for row in table.values_mut() {
            let tri = row["triangles"].as_i64().unwrap_or(0);
            #[allow(clippy::cast_precision_loss)]
            let trif = tri as f64;
            let spacing = row["spacing_mm"].as_f64().unwrap_or(0.0);
            let cap_tri = (cap_frac * per_area).clamp(0.0, 1.0);
            let merged = crate::numeric::py_round(trif * (cap_tri / cap_ratio + (1.0 - cap_tri) / lat_ratio));
            let over = spacing * bx::MM > 0.0 && {
                let n: f64 =
                    dom.iter().map(|d| (d * bx::MM / (spacing * bx::MM)).ceil() + 2.0 * 2.0 + 1.0).product();
                #[allow(clippy::cast_precision_loss)]
                let b = budget as f64;
                n > b
            };
            if let Value::Object(r) = row {
                r.insert("stl_bytes_estimate".into(), json!(50 + 50 * tri));
                r.insert("step_faces_unmerged".into(), json!(tri));
                r.insert("step_faces_merged_estimate".into(), json!(crate::cast::trunc_i64(merged)));
                r.insert("step_bytes_estimate".into(), json!(crate::cast::trunc_i64(merged * per_face)));
                r.insert("step_over_face_budget".into(), json!(usize::try_from(tri).unwrap_or(0) > max_tri));
                r.insert("over_budget".into(), json!(over));
            }
        }
        let (ok, detail) = crate::step::have_writer();
        let mut schemas: Vec<&str> = crate::step::SCHEMAS.iter().map(|(k, _)| *k).collect();
        schemas.sort_unstable();
        Ok(json!({
            "kind": "body_estimate", "design": dname, "cap": capname, "calibration": hit,
            "step": {"available": ok, "detail": detail, "occt": Value::Null, "writer": crate::step::p21::PREPROCESSOR,
                     "schema_default": crate::step::DEFAULT_SCHEMA, "schemas": schemas, "max_triangles": max_tri,
                     "cap_area_fraction": cap_frac, "cap_triangle_fraction": cap_frac * per_area,
                     "cap_triangles_per_area": per_area, "merge_ratio_cap": cap_ratio,
                     "merge_ratio_lattice": lat_ratio, "bytes_per_face": per_face,
                     "note": "faces and bytes are PREDICTED from merge ratios measured on these designs -- cap and \
                              lattice separately, because they behave completely differently -- combined with THIS \
                              design's own measured cap area fraction.  The ratios are the ones measured at the \
                              coarsest tolerance, and the cap ratio improves with refinement, so this is an UPPER bound \
                              on faces and bytes.  The job reports what was achieved, and those are the numbers to quote"},
            "law": format!("chord ~ C h^{}, triangles ~ K h^{}; exponents and constants FITTED on this design at two \
                            spacings, not assumed",
                           fmt_f(hit["chord_exponent"].as_f64().unwrap_or(0.0), 2),
                           fmt_f(hit["triangle_exponent"].as_f64().unwrap_or(0.0), 2)),
            "at_tolerance": table, "min_tolerance_mm": min_tolerance_mm(),
        }))
    }


    #[allow(clippy::too_many_lines)]
    pub fn start(self: &Arc<Self>, req: &Value) -> Result<Value, BodyError> {
        let fmts = Self::formats(req)?;
        let tol = Self::tolerance(req)?;
        let domain_aware = req.get("domain_aware").is_none_or(truthy);
        let (spec, fg, capname) = self.cap(domain_aware)?;
        let comps = Self::components(req)?;
        let (ev, dname) = self.design_for(req.get("design").unwrap_or(&Value::Null))?;
        let channel = req.get("channel").map_or_else(|| "body".to_string(), py_str);
        let step_merge = req.get("step_merge").is_none_or(truthy);
        let step_schema = req.get("step_schema").map_or_else(|| "AP214".to_string(), py_str).to_uppercase();
        if fmts.iter().any(|f| f == "step") && !crate::step::SCHEMAS.iter().any(|(k, _)| *k == step_schema) {
            let mut known: Vec<&str> = crate::step::SCHEMAS.iter().map(|(k, _)| *k).collect();
            known.sort_unstable();
            return Err(case(format!("step_schema {}: one of {}", repr_str(&step_schema), known.join(", "))));
        }
        let seq = req.get("seq").and_then(Value::as_i64).unwrap_or(0);
        let request = json!({"design": dname, "format": fmts, "tolerance_mm": tol, "domain_aware": domain_aware,
                             "cap": capname, "components": comps, "step_merge": step_merge, "step_schema": step_schema});
        let id = job_id();
        let out_dir = self.out_root.join(&id);
        let name: String = {
            let raw = req.get("name").filter(|n| truthy(n)).map_or_else(|| "body".to_string(), py_str);
            raw.replace('/', "_").chars().take(48).collect()
        };
        let job = Arc::new(Mutex::new(BodyJob {
            id: id.clone(),
            channel: channel.clone(),
            seq,
            status: "queued".into(),
            progress: 0.0,
            message: "queued".into(),
            report: None,
            error: None,
            t_submit: now(),
            t_start: None,
            t_end: None,
            req: request,
            out_dir: out_dir.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
            record: None,
        }));
        {
            let mut jobs = lock(&self.jobs);
            jobs.push(Arc::clone(&job));
            let mut order: Vec<Arc<Mutex<BodyJob>>> = jobs.clone();
            order.sort_by(|a, b| lock(a).t_submit.total_cmp(&lock(b).t_submit));
            let keep = order.len().saturating_sub(24);
            for old in &order[..keep] {
                let (status, dir, oid) = {
                    let o = lock(old);
                    (o.status.clone(), o.out_dir.clone(), o.id.clone())
                };
                if status == "completed" || status == "failed" {
                    let _ = std::fs::remove_dir_all(&dir);
                    jobs.retain(|j| lock(j).id != oid);
                }
            }
        }
        let me = Arc::clone(self);
        let job_run = Arc::clone(&job);
        let components = Components::parse(&comps)?;
        let work: BodyWork = Box::new(move |cancel: &(dyn Fn() -> bool + Sync)| {
            let cancelled = Arc::clone(&lock(&job_run).cancelled);
            let stop = move || cancel() || cancelled.load(Ordering::Relaxed);
            {
                let mut j = lock(&job_run);
                j.status = "running".into();
                j.t_start = Some(now());
                j.message = "calibrating the chord constant".into();
            }
            me.event(&job_run);
            let job_prog = Arc::clone(&job_run);
            let me_prog = Arc::clone(&me);
            let prog = move |f: f64| {
                let fire = {
                    let mut j = lock(&job_prog);
                    j.progress = f;
                    if f > 0.5 && j.message.starts_with("calibrat") {
                        j.message = "extracting at the chosen spacing".into();
                        true
                    } else {
                        false
                    }
                };
                if fire {
                    me_prog.event(&job_prog);
                }
            };
            let case_doc = me.host.current_case();
            let kernel = me.host.sdf_kernel();
            let recipe = me.host.recipe_writer();
            let recipe_ref = recipe.as_ref().map(|r| {
                let f: RecipeWriter<'_> = r.as_ref();
                f
            });
            let cap = Self::make_cap(&spec, kernel.as_deref())?;
            let (rep, bbox) = {
                let _guard = me.host.eval_lock();
                let opts = BuildBodyOptions {
                    tolerance_mm: tol,
                    formats: fmts.clone(),
                    components: components.clone(),
                    face_group: fg.clone(),
                    name: name.clone(),
                    cancel: Some(&stop),
                    progress: Some(&prog),
                    step_merge,
                    step_schema: step_schema.clone(),
                    recipe: recipe_ref,
                    kernel: kernel.as_deref().map(|k| k as &dyn SdfKernel),
                    ..BuildBodyOptions::default()
                };
                let (rep, v, _f) = bx::build_body(ev.as_ref(), &case_doc, &cap, &out_dir, &opts)?;
                let bbox = if v.is_empty() {
                    Value::Null
                } else {
                    let lo: Vec<f64> =
                        (0..3).map(|a| v.iter().map(|p| p[a]).fold(f64::INFINITY, f64::min) * 1e3).collect();
                    let hi: Vec<f64> = (0..3)
                        .map(|a| v.iter().map(|p| p[a]).fold(f64::NEG_INFINITY, f64::max) * 1e3)
                        .collect();
                    json!([lo, hi])
                };
                (rep, bbox)
            };
            let mut rep = rep;
            if let Err(e) = me.stamp(&job_run, &mut rep, ev.as_ref(), &case_doc, &name, &bbox) {
                rep.insert("record_error".into(), json!(e));
            }
            Ok(rep)
        });
        let me = Arc::clone(self);
        let job_done = Arc::clone(&job);
        let done: BodyDone = Box::new(move |res| {
            {
                let mut j = lock(&job_done);
                j.t_end = Some(now());
                match res {
                    Err(e) => {
                        j.status = "failed".into();
                        j.error = Some(error_repr(&e));
                        j.message = format!("failed: {e}");
                    }
                    Ok(rep) => {
                        j.message = completion_message(&rep);
                        j.report = Some(rep);
                        j.status = "completed".into();
                        j.progress = 1.0;
                    }
                }
            }
            me.event(&job_done);
        });
        self.host.submit(&channel, seq, work, done);
        self.event(&job);
        let j = lock(&job);
        Ok(
            json!({"kind": "body", "job_id": j.id, "status": j.status, "poll": format!("/v1/body/jobs/{}", j.id),
                  "request": j.req}),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn stamp(
        &self,
        job: &Arc<Mutex<BodyJob>>,
        rep: &mut Map<String, Value>,
        ev: &dyn BodyEvaluator,
        case_doc: &Value,
        name: &str,
        bbox: &Value,
    ) -> Result<(), String> {
        let snap = ev.snapshot();
        let params = self.host.design_params(ev, &snap);
        let dom =
            if case_doc.get("domain").filter(|d| truthy(d)).and_then(|d| d.get("mesh")).is_some_and(truthy) {
                self.host
                    .part_domain()
                    .ok()
                    .flatten()
                    .map_or(Value::Null, |c| if c.current.is_null() { json!({}) } else { c.current })
            } else {
                Value::Null
            };
        let (id, out_dir, req) = {
            let j = lock(job);
            (j.id.clone(), j.out_dir.clone(), j.req.clone())
        };
        let base = format!("/v1/body/jobs/{id}");
        let stem = out_dir.join(name);
        let files_rep = rep.get("files").cloned().unwrap_or_else(|| json!({}));
        let mut files: Vec<(String, Option<String>)> = Vec::new();
        if let Some(fmts) = req.get("format").and_then(Value::as_array) {
            for f in fmts.iter().filter_map(Value::as_str) {
                if let Some(p) = files_rep
                    .get(f)
                    .and_then(|x| x.get("path"))
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                {
                    files.push((f.to_string(), Some(p.to_string())));
                }
            }
        }
        let recipe_link = files_rep
            .get("recipe")
            .and_then(|r| r.get("path"))
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map_or(Value::Null, |p| json!(format!("{base}/files/{}", basename(p))));
        let file_links: Map<String, Value> = files
            .iter()
            .map(|(f, p)| {
                (f.clone(), json!(format!("{base}/files/{}", basename(p.as_deref().unwrap_or("")))))
            })
            .collect();
        let links = json!({"job": base, "record": format!("{base}/record"), "files": file_links, "recipe": recipe_link});
        let report = Value::Object(rep.clone());
        let meta = Value::Object(snap.meta.clone());
        let service_version = self.host.service_version();
        let backend = self.host.backend_name();
        let inputs = implexity_io::provenance::records::BodyRecordInputs {
            design_params: &params,
            design_meta: &meta,
            design_version: snap.version,
            case_doc,
            report: &report,
            request: &req,
            service_version: service_version.as_deref(),
            backend: Some(&backend),
            h_design_mm: Some(ev.h_design() * 1e3),
            domain: &dom,
            links: &links,
            bbox_mm: bbox,
        };
        let mut rec = implexity_io::provenance::body_record(&inputs).map_err(|e| e.to_string())?;
        let rp = PathBuf::from(format!("{}_record.json", stem.display()));
        let urls: Map<String, Value> =
            files.iter().map(|(f, _)| (f.clone(), json!(format!("{base}/record")))).collect();
        implexity_io::provenance::stamp(&mut rec, &files, Some(&rp), &urls, true)
            .map_err(|e| e.to_string())?;
        if let Some(stamped) = rec.get("stamped").and_then(Value::as_object) {

            for (fmt, st) in stamped {
                let facts = [
                    ("bytes_on_disk", st.get("bytes").cloned().unwrap_or(Value::Null)),
                    ("record_embedded", json!(st.get("embedded").is_some_and(truthy))),
                    ("record_carries", st.get("carries").cloned().unwrap_or_else(|| json!("nothing"))),
                ];
                if !matches!(rep.get("files").and_then(|f| f.get(fmt.as_str())), Some(Value::Object(_))) {
                    continue;
                }
                let files = rep.get_mut("files").and_then(|f| f.get_mut(fmt.as_str()));
                if let Some(Value::Object(entry)) = files {
                    for (k, v) in &facts {
                        entry.insert((*k).into(), v.clone());
                    }
                }
                let aliased = rep
                    .get_mut("provenance")
                    .and_then(|p| p.get_mut("files"))
                    .and_then(|f| f.get_mut(fmt.as_str()));
                if let Some(Value::Object(entry)) = aliased {
                    for (k, v) in &facts {
                        entry.insert((*k).into(), v.clone());
                    }
                }
            }
        }
        let pp = PathBuf::from(format!("{}_provenance.json", stem.display()));
        let prov = rep.get("provenance").filter(|p| truthy(p)).cloned().unwrap_or_else(|| json!({}));
        let text =
            implexity_core::json::dumps(&prov, &implexity_core::json::DumpOptions::indented(1).sorted(true));
        if implexity_io::atomic::write_atomic(&pp, text.as_bytes()).is_ok()
            && let Some(Value::Object(p)) = rep.get_mut("files").and_then(|f| f.get_mut("provenance"))
        {
            p.insert("bytes".into(), json!(std::fs::metadata(&pp).map_or(0, |m| m.len())));
        }
        let rec_bytes = std::fs::metadata(&rp).map_or(0, |m| m.len());
        let files_obj = rep.entry("files").or_insert_with(|| json!({}));
        if let Value::Object(fs) = files_obj {
            fs.insert(
                "record".into(),
                json!({"path": rp.display().to_string(), "bytes": rec_bytes, "schema": rec["schema"], "record_id": rec["record_id"]}),
            );
        }
        lock(job).record = Some(rec);
        Ok(())
    }


    pub fn record(&self, job_id: &str) -> Result<Value, BodyError> {
        let j = self.find(job_id).ok_or_else(|| BodyError::NotFound(job_id.into()))?;
        let j = lock(&j);
        j.record.clone().ok_or_else(|| {
            case(format!(
                "body job {job_id} is {}: a provenance record describes an artefact that exists, and this job has not \
                 produced one yet",
                j.status
            ))
        })
    }


    pub fn job_info(&self, job_id: &str, full: bool) -> Result<Value, BodyError> {
        let j = self.find(job_id).ok_or_else(|| BodyError::NotFound(job_id.into()))?;
        let d = lock(&j).as_dict(full);
        Ok(d)
    }

    #[must_use]
    pub fn jobs_list(&self) -> Value {
        let mut jobs: Vec<Arc<Mutex<BodyJob>>> = lock(&self.jobs).clone();
        jobs.sort_by(|a, b| lock(b).t_submit.total_cmp(&lock(a).t_submit));
        json!({"kind": "body_jobs", "jobs": jobs.iter().map(|j| lock(j).as_dict(false)).collect::<Vec<_>>()})
    }


    pub fn cancel(&self, job_id: &str) -> Result<Value, BodyError> {
        let j = self.find(job_id).ok_or_else(|| BodyError::NotFound(job_id.into()))?;
        let mut j = lock(&j);
        j.cancelled.store(true, Ordering::Relaxed);
        j.message = "cancel requested".into();
        Ok(j.as_dict(false))
    }


    pub fn artifact(&self, job_id: &str, name: &str) -> Result<(Vec<u8>, String, String), BodyError> {
        let j = self.find(job_id).ok_or_else(|| BodyError::NotFound(job_id.into()))?;
        let base = std::path::absolute(&lock(&j).out_dir)
            .map_err(|_| BodyError::NotFound(format!("{job_id}/{name}")))?;
        let leaf = basename(name);
        let p = base.join(&leaf);
        if leaf.is_empty()
            || leaf == "."
            || leaf == ".."
            || !p.starts_with(&base)
            || p == base
            || !p.is_file()
        {
            return Err(BodyError::NotFound(format!("{job_id}/{name}")));
        }
        let data = std::fs::read(&p).map_err(|_| BodyError::NotFound(format!("{job_id}/{name}")))?;
        let ext = leaf.rsplit('.').next().unwrap_or("").to_lowercase();
        Ok((data, ctype_for(&ext), leaf))
    }

    fn event(&self, job: &Arc<Mutex<BodyJob>>) {
        let ev = {
            let j = lock(job);
            json!({"event": "body_job", "job_id": j.id, "status": j.status, "progress": j.progress, "message": j.message})
        };
        self.host.broadcast(ev);
    }
}

fn py_float(v: &Value) -> Result<f64, BodyError> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64().ok_or_else(|| BodyError::Failed("not a number".into())),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| BodyError::Failed(format!("could not convert string to float: {}", repr_str(s)))),
        other => Err(BodyError::Failed(format!(
            "float() argument must be a string or a real number, not '{}'",
            implexity_core::pyobj::type_name(other)
        ))),
    }
}

fn error_repr(e: &BodyError) -> String {
    match e {
        BodyError::Case(p) => format!("CaseError({})", implexity_core::pyobj::list_repr(p)),
        BodyError::NotFound(m) | BodyError::Key(m) => format!("KeyError({})", repr_str(m)),
        BodyError::Failed(m) => format!("ValueError({})", repr_str(m)),
    }
}

fn completion_message(rep: &Map<String, Value>) -> String {
    let a = rep.get("accepted").cloned().unwrap_or(Value::Null);
    let tris = rep.get("extraction").and_then(|x| x.get("triangles")).map_or_else(|| "None".into(), py_str);
    let chord = a["chord_area_weighted_mm"].as_f64().map_or_else(|| "None".into(), |c| fmt_f(c, 5));
    let genus = a["genus"].as_f64().map_or_else(|| py_str(&a["genus"]), |g| fmt_g(g, 6));
    let mut msg = format!(
        "{tris} triangles, chord {chord} mm, watertight={}, {} component(s), genus {genus}",
        py_str(&a["watertight"]),
        py_str(&a["components"])
    );
    if a.get("step_faces").is_some() {
        let rel = a["step_volume_vs_mesh_rel"].as_f64().map_or_else(|| "None".into(), |r| fmt_e(r, 1));
        let _ = write!(
            msg,
            " | STEP {}: {} solid(s), {} faces (from {}), valid={}, volume vs mesh {rel}",
            py_str(&a["step_schema"]),
            py_str(&a["step_solids"]),
            py_str(&a["step_faces"]),
            py_str(&a["step_faces_unmerged"]),
            py_str(&a["step_valid_solid"])
        );
    }
    msg
}

fn failure(e: BodyError) -> implexity_core::route_tables::RouteFailure {
    use implexity_core::route_tables::RouteFailure;
    match e {
        BodyError::Case(p) => RouteFailure::Case(implexity_core::error::CaseError::new(p)),
        BodyError::NotFound(m) => RouteFailure::Status(404, m, None),

        BodyError::Key(k) => RouteFailure::Internal(repr_str(&k)),
        BodyError::Failed(m) => RouteFailure::Contract(m),
    }
}

fn json_body(req: &implexity_core::route_tables::RouteRequest) -> Value {
    match &req.body {
        implexity_core::route_tables::RouteBody::Json(v) => v.clone(),
        implexity_core::route_tables::RouteBody::Raw(_) => json!({}),
    }
}


pub fn route_table<H: BodyHost>(
    manager: &Arc<BodyManager<H>>,
) -> Result<implexity_core::route_tables::RouteTable, implexity_core::route_tables::RouteTableError> {
    use implexity_core::route_tables::{BodyPolicy, RouteDecl, RouteReply, RouteTable};
    const MODULE: &str = "implexity.bodyapi";
    let mut t = RouteTable::new();
    let m = Arc::clone(manager);
    t.add(RouteDecl::new(
        "GET",
        "/v1/body/jobs",
        BodyPolicy::None,
        "Every export job, newest first.",
        MODULE,
        Arc::new(move |_req, _svc| Ok(RouteReply::json(200, &m.jobs_list()))),
    )?)?;
    let m = Arc::clone(manager);
    t.add(RouteDecl::new(
        "GET",
        "/v1/body/jobs/<id>",
        BodyPolicy::None,
        "One export job, its provenance record, and its artefacts.",
        MODULE,
        Arc::new(move |req, _svc| {
            let ident = req.ident.clone().unwrap_or_default();
            let (jid, tail) = ident.split_once('/').map_or((ident.as_str(), ""), |(a, b)| (a, b));
            if let Some(name) = tail.strip_prefix("files/") {
                return Ok(match m.artifact(jid, name) {
                    Ok((data, ctype, file)) => RouteReply {
                        status: 200,
                        content_type: ctype,
                        body: data,
                        headers: vec![(
                            "Content-Disposition".into(),
                            format!("attachment; filename=\"{file}\""),
                        )],
                    },
                    Err(_) => RouteReply::error(404, "no such artefact", Some(&json!(req.path))),
                });
            }
            if tail == "record" {
                if m.job_info(jid, false).is_err() {
                    return Ok(RouteReply::error(404, "no such body job", Some(&json!(jid))));
                }
                return m.record(jid).map(|r| RouteReply::json(200, &r)).map_err(failure);
            }
            if tail.is_empty() || tail == "files" {
                return Ok(match m.job_info(jid, true) {
                    Ok(v) => RouteReply::json(200, &v),
                    Err(_) => RouteReply::error(404, "no such body job", Some(&json!(jid))),
                });
            }
            Ok(RouteReply::error(404, "no such endpoint", Some(&json!(req.path))))
        }),
    )?)?;
    let m = Arc::clone(manager);
    t.add(RouteDecl::new(
        "POST",
        "/v1/body",
        BodyPolicy::Json,
        "Start an export job.",
        MODULE,
        Arc::new(move |req, _svc| {
            m.start(&json_body(req)).map(|v| RouteReply::json(200, &v)).map_err(failure)
        }),
    )?)?;
    let m = Arc::clone(manager);
    t.add(RouteDecl::new(
        "POST",
        "/v1/body/estimate",
        BodyPolicy::Json,
        "Triangles and chord at a tolerance, from one cached coarse pass.",
        MODULE,
        Arc::new(move |req, _svc| {
            m.estimate(&json_body(req)).map(|v| RouteReply::json(200, &v)).map_err(failure)
        }),
    )?)?;
    let m = Arc::clone(manager);
    t.add(RouteDecl::new(
        "POST",
        "/v1/body/jobs/<id>",
        BodyPolicy::Json,
        "{\"op\": \"cancel\"} against one export job.",
        MODULE,
        Arc::new(move |req, _svc| {
            let jid = req.ident.clone().unwrap_or_default();
            if json_body(req).get("op") == Some(&json!("cancel")) {
                return Ok(match m.cancel(&jid) {
                    Ok(v) => RouteReply::json(200, &v),
                    Err(_) => RouteReply::error(404, "no such body job", Some(&json!(jid))),
                });
            }
            Ok(RouteReply::error(400, "POST /v1/body/jobs/<id> takes {\"op\": \"cancel\"}", None))
        }),
    )?)?;
    Ok(t)
}
