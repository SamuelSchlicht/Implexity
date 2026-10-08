// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use implexity_authoring::error::{AResult, AuthoringError};
use implexity_authoring::services::{Authoring, NativeStores, ServiceContext};
use implexity_core::route_tables::{
    BodyPolicy, RouteBody, RouteDecl, RouteFailure, RouteHandler, RouteReply, RouteRequest, RouteService,
    RouteTable, RouteTableError,
};
use implexity_io::heavy_lease::HeavyOperationLease;
use serde_json::{Map, Value, json};

use crate::error::{JobError, JobResult};
use crate::manager::{ModelOptimizeManager, OptimizeHost};
use crate::result_arrays::{self, ArrayChunk, ArrayReadError};
use crate::studies::{self, StudyStore, StudyStoreError};

pub const MANAGER_KEY: &str = "implexity.jobs.opt_manager";
pub const STUDIES_KEY: &str = "implexity.jobs.study_store";
pub const HOOKS_KEY: &str = "implexity.jobs.service_hooks";
const MODULE: &str = "implexity.implicit.api";
const RESULT_ARRAYS_MODULE: &str = "implexity.implicit.result_arrays_http";

pub type Hook<T> = Arc<dyn Fn() -> T + Send + Sync>;
pub type GuardHook = Arc<dyn Fn(&Value, &mut dyn FnMut() -> JobResult<()>) -> JobResult<()> + Send + Sync>;

#[derive(Clone, Default)]
pub struct JobsHooks {
    pub eval_lock: Option<Hook<std::io::Result<Arc<HeavyOperationLease>>>>,
    pub broadcast: Option<Arc<dyn Fn(&Value) + Send + Sync>>,
    pub physics_runtime_guard: Option<GuardHook>,
    pub normalise_computation_effort_request:
        Option<Arc<dyn Fn(Option<&Value>) -> Result<Value, String> + Send + Sync>>,
    pub current_case: Option<Arc<dyn Fn(&str) -> Option<Value> + Send + Sync>>,
    pub service_version: Option<Hook<Value>>,
    pub backend_name: Option<Hook<String>>,
    pub worker_command: Option<Vec<String>>,
    pub worker_cwd: Option<PathBuf>,
}

#[derive(Default)]
struct HooksSlot(RwLock<JobsHooks>);

fn hooks_slot(svc: &dyn RouteService) -> Arc<dyn Any + Send + Sync> {
    svc.extension_state(HOOKS_KEY, &|| Arc::new(HooksSlot::default()))
}

pub fn install_host_hooks(svc: &dyn RouteService, hooks: JobsHooks) {
    let slot = hooks_slot(svc);
    if let Some(s) = slot.downcast_ref::<HooksSlot>() {
        *s.0.write().unwrap_or_else(std::sync::PoisonError::into_inner) = hooks;
    }
}

pub struct ServiceHost {
    state_dir: PathBuf,
    authoring: Arc<Authoring>,
    hooks: Arc<dyn Any + Send + Sync>,
    lease: Mutex<Option<Arc<HeavyOperationLease>>>,
}

impl std::fmt::Debug for ServiceHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceHost").field("state_dir", &self.state_dir).finish_non_exhaustive()
    }
}

impl ServiceHost {

    pub fn of(svc: &dyn RouteService) -> AResult<Self> {
        Ok(Self {
            state_dir: svc.state_dir(),
            authoring: implexity_authoring::services::authoring(svc)?,
            hooks: hooks_slot(svc),
            lease: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn new(state_dir: &Path, authoring: Arc<Authoring>, hooks: JobsHooks) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            authoring,
            hooks: Arc::new(HooksSlot(RwLock::new(hooks))),
            lease: Mutex::new(None),
        }
    }

    fn hooks(&self) -> JobsHooks {
        self.hooks
            .downcast_ref::<HooksSlot>()
            .map(|s| s.0.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
            .unwrap_or_default()
    }

    fn stores(&self) -> NativeStores<'_> {
        NativeStores { ctx: self, a: &self.authoring }
    }
}

fn backend_env() -> Option<String> {
    std::env::var("IMPLEXITY_PHYSICS_BACKEND").ok().filter(|v| !v.trim().is_empty())
}

impl ServiceContext for ServiceHost {
    fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    fn current_case(&self) -> Option<Value> {
        let backend = self.physics_backend()?;
        self.hooks().current_case.and_then(|h| h(&backend))
    }

    fn physics_backend(&self) -> Option<String> {
        implexity_core::backends::selected_physics_name(
            &implexity_core::registries::global().contributions,
            backend_env().as_deref(),
        )
    }

    fn package_generation(&self) -> AResult<Value> {
        Ok(json!(implexity_core::packages::global().generation()))
    }

    fn endpoints(&self) -> Vec<String> {
        Vec::new()
    }

    fn authoring(&self) -> AResult<Arc<Authoring>> {
        Ok(Arc::clone(&self.authoring))
    }
}

impl OptimizeHost for ServiceHost {
    fn eval_lock(&self) -> std::io::Result<Arc<HeavyOperationLease>> {
        if let Some(h) = self.hooks().eval_lock {
            return h();
        }
        let mut slot = self.lease.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(l) = slot.as_ref() {
            return Ok(Arc::clone(l));
        }
        let lease =
            Arc::new(HeavyOperationLease::new(&self.state_dir).map_err(|e| std::io::Error::other(e.0))?);
        *slot = Some(Arc::clone(&lease));
        Ok(lease)
    }

    fn broadcast(&self, message: &Value) {
        if let Some(h) = self.hooks().broadcast {
            h(message);
        }
    }

    fn interaction_field(&self, request: &Value) -> Result<Value, String> {
        self.authoring.interactions.field(&self.stores(), request).map_err(|e| e.to_string())
    }

    fn validate_guided_launch(&self, request: &Map<String, Value>) -> JobResult<()> {
        Ok(implexity_authoring::guided_setup::validate_launch(self, &Value::Object(request.clone()))?)
    }

    fn normalise_computation_effort_request(&self, raw: Option<&Value>) -> Result<Value, String> {
        match self.hooks().normalise_computation_effort_request {
            Some(h) => h(raw),
            None => {
                Err("the computation-effort contract (implexity_agent::contracts) is not installed in this \
                         service (implexity_jobs::api::install_host_hooks)"
                    .into())
            }
        }
    }

    fn current_case(&self, backend: &str) -> Option<Value> {
        self.hooks().current_case.and_then(|h| h(backend))
    }

    fn service_version(&self) -> Value {
        self.hooks().service_version.map_or(Value::Null, |h| h())
    }

    fn backend_name(&self) -> String {
        self.hooks().backend_name.map_or_else(|| "none".to_string(), |h| h())
    }

    fn worker_command(&self) -> Vec<String> {
        self.hooks().worker_command.unwrap_or_else(|| {
            let exe = std::env::current_exe()
                .map_or_else(|_| "implexity".into(), |p| p.to_string_lossy().into_owned());
            vec![exe, "worker".into()]
        })
    }

    fn worker_cwd(&self) -> PathBuf {
        self.hooks()
            .worker_cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| self.state_dir.clone()))
    }

    fn physics_runtime_guard(
        &self,
        request: &Value,
        body: &mut dyn FnMut() -> JobResult<()>,
    ) -> JobResult<()> {
        if let Some(h) = self.hooks().physics_runtime_guard {
            return h(request, body);
        }
        let packages = implexity_core::packages::global();
        let _held = packages.hold();
        let expected = request.as_object().and_then(|m| m.get("physics_generation"));
        implexity_core::package_session::check_generation(expected, packages.generation())?;
        body()
    }

    fn history_bundle(&self) -> JobResult<Value> {
        Ok(implexity_authoring::interaction_runtime::InteractionRuntime::bundle(&self.stores())?)
    }

    fn record_external_history(&self, before: &Value, label: &str, origin: &str) -> Result<Value, String> {
        self.authoring
            .interactions
            .record_external(&self.stores(), before, label, origin)
            .map_err(|e| e.to_string())
    }
}

#[derive(Default)]
struct ManagerSlot(Mutex<Option<Arc<ModelOptimizeManager>>>);


pub fn opt_manager(svc: &dyn RouteService) -> JobResult<Arc<ModelOptimizeManager>> {
    let slot = svc.extension_state(MANAGER_KEY, &|| Arc::new(ManagerSlot::default()));
    let slot = slot
        .downcast_ref::<ManagerSlot>()
        .ok_or_else(|| JobError::runtime("optimisation manager slot type mismatch"))?;
    let mut g = slot.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(m) = g.as_ref() {
        return Ok(Arc::clone(m));
    }
    crate::optimize::node::register_kind();
    let host = ServiceHost::of(svc)?;
    let authoring = Arc::clone(&host.authoring);
    let manager = Arc::new(ModelOptimizeManager::new(Arc::new(host), authoring)?);
    *g = Some(Arc::clone(&manager));
    Ok(manager)
}

#[must_use]
pub fn existing_opt_manager(svc: &dyn RouteService) -> Option<Arc<ModelOptimizeManager>> {
    let slot = svc.extension_state(MANAGER_KEY, &|| Arc::new(ManagerSlot::default()));
    slot.downcast_ref::<ManagerSlot>()
        .and_then(|s| s.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
}

#[derive(Default)]
struct StudiesSlot(OnceLock<Arc<StudyStore>>);


pub fn study_store(svc: &dyn RouteService) -> JobResult<Arc<StudyStore>> {
    let slot = svc.extension_state(STUDIES_KEY, &|| Arc::new(StudiesSlot::default()));
    let slot = slot
        .downcast_ref::<StudiesSlot>()
        .ok_or_else(|| JobError::runtime("study store slot type mismatch"))?;
    if let Some(s) = slot.0.get() {
        return Ok(Arc::clone(s));
    }
    let a = implexity_authoring::services::authoring(svc)?;
    let store = Arc::new(StudyStore::new(a.models.dir()).map_err(study_store_error)?);
    Ok(Arc::clone(slot.0.get_or_init(|| store)))
}

fn study_store_error(e: StudyStoreError) -> JobError {
    match e {
        StudyStoreError::Study(s) => {
            JobError::Problems { class: "StudyError".into(), message: s.to_string(), problems: s.problems }
        }
        StudyStoreError::Missing(m) => JobError::of("KeyError", m),
        StudyStoreError::Io(m) => JobError::of("OSError", m),
    }
}

fn json_reply(status: u16, value: &Value) -> RouteReply {
    RouteReply::json(status, value)
}

fn err_reply(status: u16, message: &str, detail: Option<&str>) -> RouteReply {
    RouteReply::error(status, message, detail.map(|d| json!(d)).as_ref())
}

fn problems_reply(error: &str, problems: &[String]) -> RouteReply {
    json_reply(422, &json!({"error": error, "problems": problems}))
}

fn internal(e: &JobError) -> RouteReply {
    if let Some(report) = e.solver_recovery() { return json_reply(500, &json!({"error": "Solver needs attention", "problems": [e.message()], "solver_recovery": report})); }
    err_reply(500, &e.message(), Some(&e.describe()))
}

fn opt_encode(e: &JobError) -> RouteReply {
    if e.solver_recovery().is_some() { return internal(e); }
    match e.python_class() {
        "OptimizeError" | "ManipulationError" => {
            problems_reply("the optimisation was refused", &e.problems())
        }
        "CaseError" => problems_reply("the case was rejected", &e.problems()),
        "KeyError" => err_reply(404, "no such model optimisation job", Some(&e.message())),
        "ModelDocError" => problems_reply("model document rejected", &e.problems()),
        "StudyError" => problems_reply("study rejected", &e.problems()),
        "ModelError" => problems_reply("the optimisation was refused", &[e.message()]),
        _ => internal(e),
    }
}

fn opt_call(f: impl FnOnce() -> JobResult<Value>) -> RouteReply {
    match f() {
        Ok(v) => json_reply(200, &v),
        Err(e) => opt_encode(&e),
    }
}

fn call(f: impl FnOnce() -> JobResult<Value>) -> RouteReply {
    match f() {
        Ok(v) => json_reply(200, &v),
        Err(e) => match e.python_class() {
            "ModelDocError" | "InteropError" | "ModelError" => {
                problems_reply("model document rejected", &e.problems())
            }
            "SeedError" => problems_reply("geometry seed rejected", &e.problems()),
            "StudyError" => problems_reply("study rejected", &e.problems()),
            _ => internal(&e),
        },
    }
}

fn body_of(req: &RouteRequest) -> Value {
    match &req.body {
        RouteBody::Json(v) => v.clone(),
        RouteBody::Raw(_) => json!({}),
    }
}

fn object_of(body: &Value) -> JobResult<Map<String, Value>> {
    body.as_object()
        .cloned()
        .ok_or_else(|| JobError::model_doc(vec!["the request body must be a JSON object".into()]))
}

fn ident(req: &RouteRequest) -> String {
    req.ident.clone().unwrap_or_default()
}

fn truthy(v: &Value) -> bool {
    implexity_core::pyobj::truthy(v)
}

type Op = fn(&RouteRequest, &dyn RouteService) -> RouteReply;

fn with_manager(svc: &dyn RouteService, f: impl FnOnce(&ModelOptimizeManager) -> RouteReply) -> RouteReply {
    match opt_manager(svc) {
        Ok(m) => f(&m),
        Err(e) => opt_encode(&e),
    }
}

fn get_results(_req: &RouteRequest, _svc: &dyn RouteService) -> RouteReply {
    let reg = &implexity_core::registries::global().contributions;
    let backend = implexity_core::backends::selected_physics_name(reg, backend_env().as_deref());
    call(|| Ok(implexity_geometry::result_fields::catalogue(reg, backend.as_deref())))
}

fn post_results(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    with_manager(svc, |m| opt_call(|| m.results(&object_of(&body)?)))
}

fn get_derivatives(_req: &RouteRequest, _svc: &dyn RouteService) -> RouteReply {
    call(|| Ok(implexity_geometry::derivatives::catalogue()))
}

fn post_derivatives(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    if let Err(e) = implexity_geometry::derivatives::normalise(&body) {
        return problems_reply("derivative request rejected", &e.problems);
    }
    with_manager(svc, |m| opt_call(|| m.derivative(&object_of(&body)?)))
}

fn attach_run_status(m: &ModelOptimizeManager, run: &mut Value, with_record: bool) {
    let jid = run.get("job_id").and_then(Value::as_str).map(str::to_string);
    let status = jid.as_deref().and_then(|j| m.job_status(j));
    if let Some(r) = run.as_object_mut() {
        r.insert("status".into(), json!(status.clone().unwrap_or_else(|| "not-loaded".into())));
        if with_record && status.is_some() {
            r.insert(
                "record".into(),
                json!(format!("/v1/implicit/optimize/jobs/{}/record", jid.unwrap_or_default())),
            );
        }
    }
}

fn get_studies(_req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    call(|| {
        let mut rows = study_store(svc)?.list();
        let m = opt_manager(svc)?;
        for r in &mut rows {
            if let Some(runs) = r.get_mut("runs").and_then(Value::as_array_mut) {
                for run in runs {
                    attach_run_status(&m, run, false);
                }
            }
        }
        let count = rows.len();
        Ok(
            json!({"kind": "implicit_studies", "schema": studies::STORE_SCHEMA, "studies": rows, "count": count}),
        )
    })
}

fn post_studies(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    opt_call(|| {
        let a = implexity_authoring::services::authoring(svc)?;
        let identity = a.model_identity()?;
        let norm = studies::normalise(&body, Some(&identity)).map_err(|s| JobError::Problems {
            class: "StudyError".into(),
            message: s.to_string(),
            problems: s.problems,
        })?;
        let om = opt_manager(svc)?;
        let mut validated = Vec::new();
        for v in norm.get("variants").and_then(Value::as_array).cloned().unwrap_or_default() {
            let decl = om.declare(v.get("request").unwrap_or(&Value::Null))?;
            let free: Vec<Value> = decl
                .meta
                .get("plan")
                .as_array()
                .map(|p| p.iter().map(|x| x.get("ref").cloned().unwrap_or(Value::Null)).collect())
                .unwrap_or_default();
            validated.push(json!({
                "name": v.get("name").cloned().unwrap_or(Value::Null),
                "solve_id": decl.meta.get("solve_id").clone(),
                "free": free,
                "objective": decl.meta.get("objective_terms").clone(),
            }));
        }
        let mut rec = study_store(svc)?.create(&body, norm.get("model")).map_err(study_store_error)?;
        if let Some(r) = rec.as_object_mut() {
            r.insert("validated".into(), Value::Array(validated));
        }
        Ok(rec)
    })
}

fn get_study(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let result = (|| -> JobResult<Result<Value, RouteReply>> {
        let mut rec = match study_store(svc)?.get(&id) {
            Ok(r) => r,
            Err(StudyStoreError::Missing(_)) => {
                return Ok(Err(err_reply(404, "no such implicit study", Some(&id))));
            }
            Err(e) => return Err(study_store_error(e)),
        };
        let m = opt_manager(svc)?;
        if let Some(runs) = rec.get_mut("runs").and_then(Value::as_array_mut) {
            for run in runs {
                attach_run_status(&m, run, true);
            }
        }
        Ok(Ok(rec))
    })();
    match result {
        Ok(Ok(v)) => json_reply(200, &v),
        Ok(Err(reply)) => reply,
        Err(e) => internal(&e),
    }
}

fn post_study(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let body = body_of(req);
    let op = body.get("op").filter(|v| truthy(v)).map(implexity_core::pyobj::py_str).unwrap_or_default();
    let st = match study_store(svc) {
        Ok(s) => s,
        Err(e) => return internal(&e),
    };
    if op == "delete" {
        return match st.delete(&id) {
            Ok(rec) => json_reply(200, &json!({"kind": "implicit_study_deleted", "id": id, "study": rec})),
            Err(StudyStoreError::Missing(_)) => err_reply(404, "no such implicit study", Some(&id)),
            Err(e) => internal(&study_store_error(e)),
        };
    }
    if op == "run" {
        let variant =
            body.get("variant").filter(|v| truthy(v)).map(implexity_core::pyobj::py_str).unwrap_or_default();
        let mut run_req = match st.variant_request(&id, &variant) {
            Ok(Value::Object(r)) => r,
            Ok(_) => Map::new(),
            Err(StudyStoreError::Missing(_)) => return err_reply(404, "no such implicit study", Some(&id)),
            Err(StudyStoreError::Study(s)) => return problems_reply("study rejected", &s.problems),
            Err(e) => return internal(&study_store_error(e)),
        };
        let overrides = body.get("overrides").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
        let Some(overrides) = overrides.as_object() else {
            return err_reply(400, "study run overrides must be an object", None);
        };
        for (k, v) in overrides {
            run_req.insert(k.clone(), v.clone());
        }
        let m = match opt_manager(svc) {
            Ok(m) => m,
            Err(e) => return opt_encode(&e),
        };
        let mut rep = match m.start(&run_req, &crate::manager::run::StartArgs::default()) {
            Ok(r) => r,
            Err(e) => return opt_encode(&e),
        };
        let job_id = rep.get("job_id").map(implexity_core::pyobj::py_str).unwrap_or_default();
        let solve_id = rep.get("solve_id").and_then(Value::as_str).map(str::to_string);
        if let Err(e) = st.attach_run(&id, &variant, &job_id, solve_id.as_deref()) {
            return internal(&study_store_error(e));
        }
        if let Some(r) = rep.as_object_mut() {
            r.insert("study".into(), json!(id));
            r.insert("variant".into(), json!(variant));
        }
        return json_reply(200, &rep);
    }
    err_reply(
        400,
        "POST /v1/implicit/studies/<id> takes {'op':'run','variant':'...'} or {'op':'delete'}",
        None,
    )
}

fn post_optimize(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    with_manager(svc, |m| {
        opt_call(|| m.start(&object_of(&body)?, &crate::manager::run::StartArgs::default()))
    })
}

fn post_preflight(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    with_manager(svc, |m| opt_call(|| m.preflight(&object_of(&body)?)))
}

fn post_sensitivity(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    with_manager(svc, |m| opt_call(|| m.sensitivity(&object_of(&body)?)))
}

fn get_jobs(_req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    with_manager(svc, |m| opt_call(|| Ok(m.jobs_list())))
}

fn get_job(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    with_manager(svc, |m| {
        for suffix in ["/record", "/before"] {
            if let Some(jid) = id.strip_suffix(suffix) {
                if m.job_status(jid).is_none() {
                    return err_reply(404, "no such model optimisation job", Some(&req.path));
                }
                return opt_call(|| if suffix == "/record" { m.record(jid, false) } else { m.before(jid) });
            }
        }
        if m.job_status(&id).is_none() {
            return err_reply(404, "no such model optimisation job", Some(&req.path));
        }
        opt_call(|| m.job_info(&id))
    })
}

fn post_steer(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let body = body_of(req);
    with_manager(svc, |m| {
        if m.job_status(&id).is_none() {
            return err_reply(404, "no such model optimisation job", Some(&id));
        }
        opt_call(|| {
            let payload = body.as_object().cloned().ok_or_else(|| {
                JobError::of(
                    "AttributeError",
                    format!("'{}' object has no attribute 'get'", python_type_name(&body)),
                )
            })?;
            m.steer(&id, &payload)
        })
    })
}

fn python_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

const JOB_OPS: [&str; 6] = ["pause", "intervene", "resume", "stop", "accept", "discard"];

fn post_job(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let body = body_of(req);
    with_manager(svc, |m| {
        let op = body.get("op").and_then(Value::as_str).filter(|o| JOB_OPS.contains(o));
        let Some(op) = op else {
            return err_reply(
                400,
                "POST /v1/implicit/optimize/jobs/<id> takes {\"op\": \"pause|intervene|resume|stop|accept|discard\"}",
                None,
            );
        };
        if m.job_status(&id).is_none() {
            return err_reply(404, "no such model optimisation job", Some(&id));
        }
        opt_call(|| m.op(&id, op))
    })
}

fn post_branch(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let body = body_of(req);
    with_manager(svc, |m| {
        if m.job_status(&id).is_none() {
            return err_reply(404, "no such model optimisation job", Some(&id));
        }
        let request = body.get("request").and_then(Value::as_object);
        opt_call(|| m.branch_after_intervention(&id, request))
    })
}

fn post_numerical_attention(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let body = body_of(req);
    with_manager(svc, |m| {
        if m.job_status(&id).is_none() {
            return err_reply(404, "no such model optimisation job", Some(&id));
        }
        let empty = Map::new();
        let payload = body.as_object().unwrap_or(&empty);
        if payload.keys().any(|k| k != "event_token" && k != "action") {
            return err_reply(400, "numerical attention payload has unsupported fields", None);
        }
        let token = payload.get("event_token").and_then(Value::as_str).unwrap_or("");
        let action = payload.get("action").and_then(Value::as_str);
        match action {
            Some("continue_exploratory") => opt_call(|| m.branch_after_numerical_attention(&id, token)),
            Some(a @ ("retry_exact" | "discard")) => {
                opt_call(|| m.resolve_numerical_attention(&id, token, a))
            }
            _ => err_reply(
                400,
                "numerical attention action must be retry_exact, continue_exploratory, or discard",
                None,
            ),
        }
    })
}

fn get_history(_req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    match implexity_authoring::services::authoring(svc)
        .map_err(JobError::from)
        .and_then(|a| Ok(a.history.list(250)?))
    {
        Ok(v) => json_reply(200, &v),
        Err(e) => internal(&e),
    }
}

fn post_history_snapshot(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let body = body_of(req);
    let empty = Map::new();
    let payload = body.as_object().unwrap_or(&empty);
    let label = payload
        .get("label")
        .filter(|v| truthy(v))
        .map_or_else(|| "Engineering snapshot".to_string(), implexity_core::pyobj::py_str);
    let details = payload.get("details").filter(|v| truthy(v));
    let result = (|| -> JobResult<Value> {
        let details = match details {
            None => None,
            Some(Value::Object(d)) => Some(d),
            Some(other) => {
                return Err(JobError::of(
                    "TypeError",
                    format!("'{}' object is not a mapping", python_type_name(other)),
                ));
            }
        };
        let a = implexity_authoring::services::authoring(svc)?;
        Ok(a.history.snapshot(&label, details, None)?)
    })();
    match result {
        Ok(v) => json_reply(200, &v),
        Err(e) => internal(&e),
    }
}

fn post_history_restore(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let result = (|| -> JobResult<Value> {
        let a = implexity_authoring::services::authoring(svc)?;
        let history = Arc::clone(&a.history);
        Ok(a.manipulation.run_reserved_idle("restore an engineering snapshot", None, || {
            history.restore_snapshot(&id).map_err(AuthoringError::from)
        })?)
    })();
    match result {
        Ok(v) => json_reply(200, &v),
        Err(e) if e.is_value_error() || e.python_class() == "KeyError" => json_reply(
            422,
            &json!({"ok": false, "error": "engineering snapshot restore was refused", "problems": [e.message()]}),
        ),
        Err(e) => internal(&e),
    }
}

fn parse_qs(query: &str) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    let h = std::str::from_utf8(&bytes[i + 1..i + 3])
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok());
                    if let Some(b) = h {
                        out.push(b);
                        i += 2;
                    } else {
                        out.push(b'%');
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    let mut out = Vec::new();
    for part in query.split(['&', ';']) {
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        out.push((decode(k), decode(v)));
    }
    out
}

fn models_dir(svc: &dyn RouteService) -> Result<PathBuf, String> {
    implexity_authoring::services::authoring(svc)
        .map(|a| a.models.dir().to_path_buf())
        .map_err(|e| e.to_string())
}

fn get_result_manifest(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let dir = match models_dir(svc) {
        Ok(d) => d,
        Err(e) => return err_reply(422, &e, None),
    };
    let result = result_arrays::resolve_store(&dir, &id)
        .and_then(|store| store.get(&id).map_err(|e| ArrayReadError::Value(e.to_string())));
    match result {
        Ok(m) => json_reply(200, &Value::Object(m)),
        Err(ArrayReadError::NotFound(_)) => err_reply(404, "unknown result artifact", None),
        Err(ArrayReadError::Value(m)) => err_reply(422, &m, None),
    }
}

fn py_int_text(text: &str) -> Result<i64, ArrayReadError> {
    let t = text.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    let valid = !digits.is_empty()
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits.chars().all(|c| c.is_ascii_digit() || c == '_');
    let parsed = if valid { t.replace('_', "").parse::<i64>().ok() } else { None };
    parsed.ok_or_else(|| {
        ArrayReadError::Value(format!(
            "invalid literal for int() with base 10: {}",
            implexity_core::py_repr::repr_str(text)
        ))
    })
}

fn get_result_array(req: &RouteRequest, svc: &dyn RouteService) -> RouteReply {
    let id = ident(req);
    let result = (|| -> Result<RouteReply, ArrayReadError> {
        let qs = parse_qs(&req.query);
        if qs.iter().any(|(k, _)| !["field", "offset", "count", "encoding"].contains(&k.as_str())) {
            return Err(ArrayReadError::Value("unknown array query parameter".into()));
        }
        let first = |k: &str| qs.iter().find(|(q, _)| q == k).map(|(_, v)| v.clone());
        let field = first("field").unwrap_or_default();
        let offset = py_int_text(&first("offset").unwrap_or_else(|| "0".into()))?;
        let count = first("count").map(|c| py_int_text(&c)).transpose()?;
        let encoding = first("encoding").unwrap_or_else(|| "raw".into());
        let dir = models_dir(svc).map_err(ArrayReadError::Value)?;
        let store = result_arrays::resolve_store(&dir, &id)?;
        let (header, data) = result_arrays::read_array(&store, &id, &field, offset, count, &encoding)?;
        Ok(match data {
            ArrayChunk::Json(values) => {
                let mut out = header;
                out.insert("values".into(), Value::Array(values));
                json_reply(200, &Value::Object(out))
            }
            ArrayChunk::Raw(bytes) => {
                let header_text = implexity_core::json::dumps(
                    &Value::Object(header.clone()),
                    &implexity_core::json::DumpOptions::compact(),
                );
                let etag =
                    format!("\"{}\"", header.get("chunk_sha256").and_then(Value::as_str).unwrap_or(""));
                RouteReply {
                    status: 200,
                    content_type: "application/vnd.implexity.result-array+raw".into(),
                    body: bytes,
                    headers: vec![
                        ("X-Implexity-Array".into(), header_text),
                        ("ETag".into(), etag),
                        ("Cache-Control".into(), "private, immutable, max-age=31536000".into()),
                    ],
                }
            }
        })
    })();
    match result {
        Ok(r) => r,
        Err(ArrayReadError::NotFound(_)) => err_reply(404, "unknown result artifact or field", None),
        Err(ArrayReadError::Value(m)) => err_reply(422, &m, None),
    }
}

const ROUTES: &[(&str, &str, BodyPolicy, &str, &str, Op)] = &[
    (
        "GET",
        "/v1/implicit/results",
        BodyPolicy::None,
        "Result fields the selected physics backend actually produces.",
        MODULE,
        get_results,
    ),
    (
        "POST",
        "/v1/implicit/results",
        BodyPolicy::Json,
        "Evaluate selected CAE state fields at the current model/case.",
        MODULE,
        post_results,
    ),
    (
        "GET",
        "/v1/implicit/derivatives",
        BodyPolicy::None,
        "Public derivative operators over engineering responses.",
        MODULE,
        get_derivatives,
    ),
    (
        "POST",
        "/v1/implicit/derivatives",
        BodyPolicy::Json,
        "Apply Jacobian, JVP or VJP at the current model/case.",
        MODULE,
        post_derivatives,
    ),
    (
        "GET",
        "/v1/implicit/studies",
        BodyPolicy::None,
        "``GET /v1/implicit/studies``: every study with its runs' status.",
        MODULE,
        get_studies,
    ),
    (
        "POST",
        "/v1/implicit/studies",
        BodyPolicy::Json,
        "``POST /v1/implicit/studies``: declare every variant, then create.",
        MODULE,
        post_studies,
    ),
    (
        "GET",
        "/v1/implicit/studies/<id>",
        BodyPolicy::None,
        "``GET /v1/implicit/studies/<id>``: one study with its run status.",
        MODULE,
        get_study,
    ),
    (
        "POST",
        "/v1/implicit/studies/<id>",
        BodyPolicy::Json,
        "``POST /v1/implicit/studies/<id>``: ``op`` ``run`` or ``delete``.",
        MODULE,
        post_study,
    ),
    (
        "POST",
        "/v1/implicit/optimize",
        BodyPolicy::Json,
        "Start an optimisation of the MODEL's own parameters.",
        MODULE,
        post_optimize,
    ),
    (
        "POST",
        "/v1/implicit/optimize/preflight",
        BodyPolicy::Json,
        "What the run would do, and what it cannot score -- MEASURED.",
        MODULE,
        post_preflight,
    ),
    (
        "POST",
        "/v1/implicit/sensitivity",
        BodyPolicy::Json,
        "One-point coupled CAE value and exact AD gradient; no design update.",
        MODULE,
        post_sensitivity,
    ),
    (
        "GET",
        "/v1/implicit/optimize/jobs",
        BodyPolicy::None,
        "Every model optimisation, newest first.",
        MODULE,
        get_jobs,
    ),
    (
        "GET",
        "/v1/implicit/optimize/jobs/<id>",
        BodyPolicy::None,
        "One job; ``<id>/record`` and ``<id>/before`` behind the same prefix.",
        MODULE,
        get_job,
    ),
    (
        "POST",
        "/v1/implicit/optimize/jobs/<id>/steer",
        BodyPolicy::Json,
        "Move a FIXED model parameter while the job runs.",
        MODULE,
        post_steer,
    ),
    (
        "POST",
        "/v1/implicit/optimize/jobs/<id>",
        BodyPolicy::Json,
        "``{\"op\": \"pause|resume|stop|accept|discard\"}`` against one job.",
        MODULE,
        post_job,
    ),
    (
        "POST",
        "/v1/implicit/optimize/jobs/<id>/branch",
        BodyPolicy::Json,
        "Re-preflight and start a provenance-linked job after manual intervention.",
        MODULE,
        post_branch,
    ),
    (
        "POST",
        "/v1/implicit/optimize/jobs/<id>/numerical-attention",
        BodyPolicy::Json,
        "Resolve one typed bounded numerical deviation without narrative data.",
        MODULE,
        post_numerical_attention,
    ),
    (
        "GET",
        "/v1/implicit/history",
        BodyPolicy::None,
        "``GET .../engineering/history``: the engineering history list.",
        MODULE,
        get_history,
    ),
    (
        "POST",
        "/v1/implicit/history/snapshot",
        BodyPolicy::Json,
        "Record a labelled engineering snapshot with optional details.",
        MODULE,
        post_history_snapshot,
    ),
    (
        "POST",
        "/v1/implicit/history/<id>/restore",
        BodyPolicy::Json,
        "Restore an engineering snapshot under idle model authority.",
        MODULE,
        post_history_restore,
    ),
    (
        "GET",
        "/v1/implicit/result-artifact/<id>/manifest",
        BodyPolicy::None,
        "",
        RESULT_ARRAYS_MODULE,
        get_result_manifest,
    ),
    (
        "GET",
        "/v1/implicit/result-artifact/<id>/array",
        BodyPolicy::None,
        "",
        RESULT_ARRAYS_MODULE,
        get_result_array,
    ),
];


pub fn route_table() -> Result<RouteTable, RouteTableError> {
    crate::optimize::node::register_kind();
    let mut table = RouteTable::new();
    for (method, pattern, body, doc, module, op) in ROUTES {
        let op = *op;
        let handler: RouteHandler =
            Arc::new(move |req: &RouteRequest, svc: &dyn RouteService| -> Result<RouteReply, RouteFailure> {
                Ok(op(req, svc))
            });
        table.add(RouteDecl::new(method, pattern, *body, doc, module, handler)?)?;
    }
    Ok(table)
}

#[must_use]
pub fn jobs_list(svc: &dyn RouteService) -> Value {
    existing_opt_manager(svc).map_or_else(|| json!({"jobs": []}), |m| m.jobs_list())
}

