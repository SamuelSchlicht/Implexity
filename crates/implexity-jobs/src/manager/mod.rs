// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




pub mod analyses;
pub mod declare;
pub mod evaluation;
pub mod job;
pub mod lifecycle;
pub mod run;
pub mod terminal;
mod stored;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use implexity_authoring::model_manager::ModelManager;
use implexity_authoring::services::Authoring;
use implexity_geometry::ParamValue;
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use crate::error::{JobError, JobResult};
use crate::managed_evaluation::{ManagedEvaluationControl, ManagedEvaluationManager, TerminalValidator};
use crate::private::{canonical_text, epoch_seconds, sha256_hex};
pub use job::{Event, JobMeta, ModelOptJob, RecoveredModelOptJob, TERMINAL, is_terminal};

#[must_use]
pub fn max_opt_grid() -> i64 {
    env_i64("IMPLEXITY_MAX_OPT_GRID", 12)
}

#[must_use]
pub fn safe_opt_grid() -> i64 {
    env_i64("IMPLEXITY_SAFE_IMPLICIT_GRID", 6)
}

#[must_use]
pub fn live_apply_hz() -> f64 {
    std::env::var("IMPLEXITY_LIVE_APPLY_HZ").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(4.0)
}

pub const MAX_PUBLIC_EPOCH_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

pub const IGNORED_KEYS: [(&str, &str); 4] = [
    (
        "physics_generation",
        "validated atomically by the active physics-package session before this declaration is compiled",
    ),
    (
        "stages",
        "the model driver has ONE stage: the geometry map's continuation schedule belongs to the lattice's design variables, which this node does not optimise (docs/IMPLICIT_OPTIMISATION.md s9)",
    ),
    (
        "design",
        "a model run's start design IS the model's current parameter values; there is no neutral design to start from",
    ),
    (
        "volume_tol",
        "the volume budget here is a two-sided penalty, not a projection, so there is no tolerance to hold it to -- the drift is reported instead",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParentLimits {
    pub default_wall_time_s: f64,
    pub wall_time_cap_s: f64,
    pub memory_ceiling_bytes: i64,
}

#[must_use]
pub fn parent_limits(operation_kind: &str) -> Option<ParentLimits> {
    let gib18 = 18 * 1024_i64.pow(3);
    match operation_kind {
        "evaluate" | "sensitivity" => Some(ParentLimits {
            default_wall_time_s: 28_800.0,
            wall_time_cap_s: 28_800.0,
            memory_ceiling_bytes: gib18,
        }),
        "preflight" => Some(ParentLimits {
            default_wall_time_s: 10_800.0,
            wall_time_cap_s: 10_800.0,
            memory_ceiling_bytes: gib18,
        }),
        "optimize" => Some(ParentLimits {
            default_wall_time_s: 604_800.0,
            wall_time_cap_s: 2_592_000.0,
            memory_ceiling_bytes: gib18,
        }),
        _ => None,
    }
}

const MANAGED_MAX_STREAM_FIELDS: usize = 1024;
const MANAGED_MAX_FIELD_NAME_LENGTH: usize = 256;
const MANAGED_MAX_TILE_AXIS: i64 = 512;
const MANAGED_MIN_PREVIEW_VOXELS: i64 = 8;
const MANAGED_MAX_PREVIEW_VOXELS: i64 = 4 * 1024 * 1024;


pub const CHILD_ENV_ALLOWLIST: [&str; 45] = [
    "BLIS_NUM_THREADS",
    "RAYON_NUM_THREADS",
    "CUDA_CACHE_MAXSIZE",
    "CUDA_CACHE_PATH",
    "CUDA_HOME",
    "CUDA_PATH",
    "CUDA_VISIBLE_DEVICES",
    "DYLD_LIBRARY_PATH",
    "HIP_VISIBLE_DEVICES",
    "JAX_COMPILATION_CACHE_DIR",
    "JAX_ENABLE_X64",
    "JAX_PLATFORM_NAME",
    "JAX_PLATFORMS",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LD_LIBRARY_PATH",
    "MALLOC_ARENA_MAX",
    "METAL_DEVICE_WRAPPER_TYPE",
    "MKL_NUM_THREADS",
    "NVIDIA_VISIBLE_DEVICES",
    "NUMEXPR_NUM_THREADS",
    "OMP_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "PATH",
    "PYTHONHASHSEED",
    "PYTHONHOME",
    "PYTHONPATH",
    "ROCR_VISIBLE_DEVICES",
    "SYSTEMROOT",
    "TEMP",
    "TF_NUM_INTEROP_THREADS",
    "TF_NUM_INTRAOP_THREADS",
    "TMP",
    "TMPDIR",
    "VECLIB_MAXIMUM_THREADS",
    "VIRTUAL_ENV",
    "WINDIR",
    "XDG_CACHE_HOME",
    "XLA_FLAGS",
    "IMPLEXITY_PLUGINS",
    "IMPLEXITY_WORKER_RUNTIME_PROFILE_SCHEMA",
    "IMPLEXITY_WORKER_RUNTIME_PROFILE_SHA256",
    "IMPLEXITY_RETAINED_FACTORIZATION_BUDGET_BYTES",
    "IMPLEXITY_CHECKPOINT_RAM_BUDGET_BYTES",
];

#[must_use]
pub fn child_env() -> JobResult<BTreeMap<String, String>> {
    if let Ok(value) = std::env::var("RAYON_NUM_THREADS") {
        if value.parse::<std::num::NonZeroUsize>().is_err() {
            return Err(JobError::value("RAYON_NUM_THREADS must be a positive integer"));
        }
    }
    let mut env: BTreeMap<String, String> =
        std::env::vars().filter(|(k, _)| CHILD_ENV_ALLOWLIST.contains(&k.as_str())).collect();
    env.entry("PATH".into()).or_insert_with(|| "/bin:/usr/bin".into());
    env.insert("MALLOC_ARENA_MAX".into(), "2".into());
    env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    env.insert("PYTHONNOUSERSITE".into(), "1".into());
    let selected = implexity_core::packages::global().selected();
    env.insert("IMPLEXITY_PHYSICS_PACKAGES".into(), selected.join(","));
    Ok(env)
}

fn contract_err<T>(message: &str) -> JobResult<T> {
    Err(JobError::optimize1(message))
}

fn valid_transport_name(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        !s.trim().is_empty() && s.chars().count() <= MANAGED_MAX_FIELD_NAME_LENGTH && !s.contains('\0')
    })
}

fn unique_names(values: &[Value]) -> bool {
    let set: std::collections::BTreeSet<&str> = values.iter().filter_map(Value::as_str).collect();
    set.len() == values.len()
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}


pub fn validate_exact_state_handoff_transport(raw: Option<&Value>) -> JobResult<()> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(()) };
    let Some(m) =
        raw.as_object().filter(|m| m.keys().all(|k| matches!(k.as_str(), "schema" | "consume" | "produce")))
    else {
        return contract_err("exact_state_handoff must contain only schema, consume, and produce");
    };
    if let Some(schema) = m.get("schema").filter(|v| !v.is_null())
        && schema.as_str() != Some("implexity-exact-state-handoff/1")
    {
        return contract_err("unsupported exact_state_handoff schema");
    }
    if let Some(consume) = m.get("consume").filter(|v| !v.is_null()) {
        let Some(c) = consume.as_object().filter(|c| c.keys().all(|k| k == "capsule_id" || k == "required"))
        else {
            return contract_err("exact_state_handoff.consume requires capsule_id and optional required");
        };
        let capsule_ok = c.get("capsule_id").and_then(Value::as_str).is_some_and(|id| {
            id.len() == 70
                && id.starts_with("guess-")
                && id[6..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if !capsule_ok || !c.get("required").is_none_or(Value::is_boolean) {
            return contract_err("exact_state_handoff consume capsule_id/required are invalid");
        }
    }
    if let Some(p) = m.get("produce")
        && !p.is_boolean()
    {
        return contract_err("exact_state_handoff.produce must be boolean");
    }
    Ok(())
}


#[allow(clippy::too_many_lines)]
pub fn validate_managed_transport(operation_kind: &str, req: &Map<String, Value>) -> JobResult<()> {
    if !matches!(operation_kind, "preflight" | "evaluate" | "sensitivity") {
        return contract_err("managed exact operation is unsupported");
    }
    let streaming = matches!(operation_kind, "evaluate" | "sensitivity");
    if streaming && req.get("stream").is_some_and(|v| !v.is_boolean()) {
        return contract_err("managed stream must be boolean");
    }
    let name_list_ok = |v: &Value, nonempty: bool| {
        v.as_array().is_some_and(|a| {
            (!nonempty || !a.is_empty())
                && a.len() <= MANAGED_MAX_STREAM_FIELDS
                && a.iter().all(valid_transport_name)
                && unique_names(a)
        })
    };
    if streaming
        && let Some(v) = req.get("stream_fields")
        && !name_list_ok(v, false)
    {
        return contract_err("managed stream_fields must be an array of unique nonempty strings");
    }
    if streaming && let Some(previous) = req.get("previous_field_ids") {
        let ok = previous.as_object().is_some_and(|p| {
            p.len() <= MANAGED_MAX_STREAM_FIELDS
                && p.keys().all(|k| valid_transport_name(&Value::String(k.clone())))
                && p.values().all(|v| v.as_str().is_some_and(|s| is_lower_hex(s, 64)))
        });
        if !ok {
            return contract_err(
                "managed previous_field_ids must map field names to lowercase 64-hex field identities",
            );
        }
    }
    if streaming && let Some(tile) = req.get("tile_shape") {
        let ok = tile.as_array().is_some_and(|t| {
            t.len() == 3
                && t.iter().all(|v| {
                    (v.is_i64() || v.is_u64())
                        && v.as_i64().is_some_and(|n| (1..=MANAGED_MAX_TILE_AXIS).contains(&n))
                })
        });
        if !ok {
            return contract_err("managed tile_shape must contain exactly three positive bounded integers");
        }
    }
    if streaming && let Some(voxels) = req.get("max_preview_voxels") {
        let ok = (voxels.is_i64() || voxels.is_u64())
            && voxels
                .as_i64()
                .is_some_and(|n| (MANAGED_MIN_PREVIEW_VOXELS..=MANAGED_MAX_PREVIEW_VOXELS).contains(&n));
        if !ok {
            return contract_err("managed max_preview_voxels must be a bounded positive integer");
        }
    }
    if operation_kind == "evaluate" {
        if let Some(fields) = req.get("fields")
            && !name_list_ok(fields, false)
        {
            return contract_err("managed fields must be an array of unique nonempty strings");
        }
        if req.get("include_values").is_some_and(|v| !v.is_boolean()) {
            return contract_err("managed include_values must be boolean");
        }
        if let Some(count) = req.get("max_inline_values") {
            let ok =
                (count.is_i64() || count.is_u64()) && count.as_i64().is_some_and(|n| (1..=4096).contains(&n));
            if !ok {
                return contract_err("managed max_inline_values must be an integer from 1 through 4096");
            }
        }
    }
    if operation_kind == "sensitivity" {
        for key in ["response", "stream_response"] {
            if let Some(v) = req.get(key).filter(|v| !v.is_null())
                && !valid_transport_name(v)
            {
                return Err(JobError::optimize1(format!("managed {key} must be a nonempty bounded string")));
            }
        }
        if let Some(responses) = req.get("sensitivity_responses") {
            if !name_list_ok(responses, true) {
                return contract_err(
                    "managed sensitivity_responses must be a nonempty array of unique nonempty strings",
                );
            }
            if req.get("response").is_some_and(|v| !v.is_null())
                || req.get("stream_response").is_some_and(|v| !v.is_null())
            {
                return contract_err(
                    "managed sensitivity batch cannot also select response or stream_response",
                );
            }
        }
    }
    validate_exact_state_handoff_transport(req.get("exact_state_handoff"))
}


pub fn managed_exact_parent_resources(
    operation_kind: &str,
    effort_binding: Option<&Value>,
) -> JobResult<(Option<f64>, i64)> {
    let Some(limit) = parent_limits(operation_kind) else {
        return contract_err("managed exact operation is unsupported");
    };
    let Some(binding) = effort_binding.filter(|v| !v.is_null()) else {
        return Ok((Some(limit.default_wall_time_s), limit.memory_ceiling_bytes));
    };
    let (selected, _binding) =
        implexity_runtime::provider_job_authority::validate_effort_binding(binding, None)?;
    let effective = &selected.effective;
    let timeout = if effective.wall_time_mode == "unlimited" {
        None
    } else {
        Some(effective.wall_time_budget_s.map_or(limit.default_wall_time_s, |w| w.min(limit.wall_time_cap_s)))
    };
    let memory = effective
        .memory_budget_bytes
        .map_or(limit.memory_ceiling_bytes, |m| m.min(limit.memory_ceiling_bytes));
    if timeout.is_some_and(|t| !t.is_finite() || t <= 0.0) || memory <= 0 {
        return contract_err("managed exact operation has no positive parent resource budget");
    }
    Ok((timeout, memory))
}


pub fn managed_transport_timing(
    started: u64,
    artifact_started: u64,
    artifact_finished: u64,
    finished: u64,
) -> JobResult<Value> {
    if !(started <= artifact_started
        && artifact_started <= artifact_finished
        && artifact_finished <= finished)
    {
        return Err(JobError::value("managed transport timing boundaries are invalid"));
    }
    let ns = [
        ("commit_validation_wall_ns", artifact_started - started),
        ("public_transport_artifact_wall_ns", artifact_finished - artifact_started),
        ("response_finalize_wall_ns", finished - artifact_finished),
        ("wall_time_ns", finished - started),
    ];
    let mut nanoseconds = Map::new();
    let mut measurements = Map::new();
    for (k, v) in ns {
        nanoseconds.insert(k.into(), json!(v));
        #[allow(clippy::cast_precision_loss)]
        let seconds = v as f64 / 1_000_000_000.0;
        measurements.insert(
            format!("{}_s", k.trim_end_matches("_ns")),
            implexity_optim::numeric::float_value(seconds),
        );
    }
    Ok(json!({
        "schema": "implexity-managed-public-transport-timing/1",
        "clock": "time.perf_counter_ns",
        "truth_scope": "parent_transport_only_not_provider_numerical",
        "accounting": "wall_time_ns=commit_validation_wall_ns+public_transport_artifact_wall_ns+response_finalize_wall_ns",
        "nanoseconds": nanoseconds,
        "measurements": measurements,
    }))
}


pub fn attach_managed_transport_timing(
    report: &mut Map<String, Value>,
    started: u64,
    artifact_started: u64,
    artifact_finished: u64,
) -> JobResult<()> {
    if report.contains_key("managed_transport_timing") {
        return Err(JobError::value("managed transport timing field is reserved"));
    }
    let finished = u64::try_from(crate::private::perf_counter_ns()).unwrap_or(0);
    report.insert(
        "managed_transport_timing".into(),
        managed_transport_timing(started, artifact_started, artifact_finished, finished)?,
    );
    Ok(())
}

#[must_use]
pub fn param_to_array(value: &ParamValue) -> Option<ArrayD<f64>> {
    let (shape, data) = value.to_f64_array().ok()?;
    ArrayD::from_shape_vec(IxDyn(&shape), data).ok()
}

pub trait OptimizeHost: Send + Sync {

    fn eval_lock(&self) -> std::io::Result<Arc<implexity_io::heavy_lease::HeavyOperationLease>>;
    fn broadcast(&self, message: &Value);

    fn interaction_field(&self, request: &Value) -> Result<Value, String>;

    fn validate_guided_launch(&self, request: &Map<String, Value>) -> JobResult<()>;

    fn normalise_computation_effort_request(&self, raw: Option<&Value>) -> Result<Value, String>;
    fn current_case(&self, backend: &str) -> Option<Value>;
    fn service_version(&self) -> Value;
    fn backend_name(&self) -> String;
    fn worker_command(&self) -> Vec<String>;
    fn worker_cwd(&self) -> PathBuf;

    fn physics_runtime_guard(
        &self,
        request: &Value,
        body: &mut dyn FnMut() -> JobResult<()>,
    ) -> JobResult<()>;

    fn history_bundle(&self) -> JobResult<Value>;

    fn record_external_history(&self, before: &Value, label: &str, origin: &str) -> Result<Value, String>;
}

pub type LiveJob = Arc<Mutex<ModelOptJob>>;
pub type RecoveredJob = Arc<Mutex<RecoveredModelOptJob>>;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Debug)]
pub enum JobEntry {
    Live(LiveJob),
    Recovered(RecoveredJob),
}

impl JobEntry {
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Live(j) => lock(j).id.clone(),
            Self::Recovered(j) => lock(j).id.clone(),
        }
    }

    #[must_use]
    pub fn status(&self) -> String {
        match self {
            Self::Live(j) => lock(j).status.clone(),
            Self::Recovered(j) => lock(j).status.clone(),
        }
    }

    #[must_use]
    pub fn t_submit(&self) -> f64 {
        match self {
            Self::Live(j) => lock(j).t_submit,
            Self::Recovered(j) => lock(j).t_submit,
        }
    }

    #[must_use]
    pub fn as_dict(&self, with_rows: bool) -> Value {
        match self {
            Self::Live(j) => lock(j).as_dict(with_rows),
            Self::Recovered(j) => lock(j).as_dict(with_rows),
        }
    }

    #[must_use]
    pub fn terminal_event(&self) -> Arc<Event> {
        match self {
            Self::Live(j) => Arc::clone(&lock(j).managed_terminal_event),
            Self::Recovered(j) => Arc::clone(&lock(j).managed_terminal_event),
        }
    }

    #[must_use]
    pub fn live(&self) -> Option<LiveJob> {
        match self {
            Self::Live(j) => Some(Arc::clone(j)),
            Self::Recovered(_) => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ManagerState {
    pub jobs: indexmap::IndexMap<String, JobEntry>,
    pub active: Option<String>,
    pub model_authority_job_id: Option<String>,
    pub lifecycle_locks: BTreeMap<String, Arc<job::Gate>>,
}

pub(crate) struct Inner {
    pub(crate) host: Arc<dyn OptimizeHost>,
    pub(crate) models: Arc<ModelManager>,
    pub(crate) dir: PathBuf,
    pub(crate) state: Mutex<ManagerState>,
    pub(crate) supervisor: ManagedEvaluationManager,
    pub(crate) authoring: Arc<Authoring>,
}

#[derive(Clone)]
pub struct ModelOptimizeManager {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for ModelOptimizeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelOptimizeManager").field("dir", &self.inner.dir).finish_non_exhaustive()
    }
}


pub fn managed_restart_request_sha256(request: &Value) -> JobResult<String> {
    implexity_core::wire::to_wire(request)
        .map_err(|_| JobError::value("managed optimization request is not canonically serializable"))?;
    Ok(sha256_hex(canonical_text(request).as_bytes()))
}

impl ModelOptimizeManager {

    pub fn new(host: Arc<dyn OptimizeHost>, authoring: Arc<Authoring>) -> JobResult<Self> {
        let models = Arc::clone(&authoring.models);
        let dir = models.dir().join("opt");
        std::fs::create_dir_all(&dir)?;
        let supervisor_root = std::fs::canonicalize(&dir)?.join("managed_children");
        let supervisor = ManagedEvaluationManager::new(&supervisor_root, None)?;
        let manager = Self {
            inner: Arc::new(Inner {
                host,
                models,
                dir,
                state: Mutex::new(ManagerState::default()),
                supervisor,
                authoring,
            }),
        };
        manager.rehydrate_managed_optimizations()?;
        Ok(manager)
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    #[must_use]
    pub fn models(&self) -> &Arc<ModelManager> {
        &self.inner.models
    }

    #[must_use]
    pub fn authoring(&self) -> &Arc<Authoring> {
        &self.inner.authoring
    }

    #[must_use]
    pub fn host(&self) -> &Arc<dyn OptimizeHost> {
        &self.inner.host
    }

    #[must_use]
    pub fn supervisor(&self) -> &ManagedEvaluationManager {
        &self.inner.supervisor
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, ManagerState> {
        self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn entry(&self, job_id: &str) -> Option<JobEntry> {
        self.state().jobs.get(job_id).cloned()
    }

    #[must_use]
    pub fn job_status(&self, job_id: &str) -> Option<String> {
        self.entry(job_id).map(|e| e.status())
    }


    pub(crate) fn require_entry(&self, job_id: &str) -> JobResult<JobEntry> {
        self.entry(job_id).ok_or_else(|| JobError::of("KeyError", implexity_core::py_repr::repr_str(job_id)))
    }

    pub(crate) fn lifecycle_lock(&self, job_id: &str) -> Arc<job::Gate> {
        let mut state = self.state();
        Arc::clone(state.lifecycle_locks.entry(job_id.to_string()).or_default())
    }


    pub(crate) fn write_managed_restart_descriptor(
        &self,
        job: &ModelOptJob,
        resume: bool,
    ) -> JobResult<Value> {
        let full = job.as_dict(false);
        let mut snapshot = Map::new();
        for key in [
            "kind",
            "job_id",
            "channel",
            "seq",
            "status",
            "progress",
            "message",
            "node",
            "model_kind",
            "declared_by",
            "case",
            "grid",
            "iters",
            "total_iters",
            "lr",
            "objective_terms",
            "physics_provider",
            "provider_execution",
            "solve_id",
            "structure_id",
            "poll",
            "ops",
        ] {
            if let Some(v) = full.get(key) {
                snapshot.insert(key.into(), v.clone());
            }
        }
        snapshot.insert("free".into(), json!([]));
        let request = Value::Object(job.request.clone());
        let request_sha256 = managed_restart_request_sha256(&request)?;
        let t_submit =
            if job.t_submit == 0.0 { job.t_start.unwrap_or_else(epoch_seconds) } else { job.t_submit };
        let payload = json!({
            "schema": job::MANAGED_OPTIMIZATION_RESTART_SCHEMA,
            "job_id": job.id,
            "owner": {"kind": "implicit_optimize", "id": job.id},
            "request": request,
            "request_sha256": request_sha256,
            "spec_fingerprint": job.managed_spec_fingerprint.clone().unwrap_or(Value::Null),
            "t_submit": implexity_optim::numeric::float_value(t_submit),
            "resume_generation": resume,
            "snapshot": snapshot,
        });
        let job_dir = PathBuf::from(job.job_dir.clone().unwrap_or_default());
        let path = job_dir.join(".managed-restart.json");
        let temporary = job_dir.join(format!(".managed-restart.{}.tmp", crate::private::token_hex(16)?));
        let mut encoded = canonical_text(&payload).into_bytes();
        encoded.push(b'\n');
        let mut file = crate::private::create_exclusive(&temporary, 0o600)?;
        crate::private::write_all_sync(&mut file, &encoded)?;
        drop(file);
        std::fs::rename(&temporary, &path)?;
        crate::private::fsync_dir(&job_dir)?;
        Ok(payload)
    }


    pub(crate) fn read_managed_restart_descriptor(
        &self,
        job_id: &str,
    ) -> JobResult<(Map<String, Value>, PathBuf)> {
        if !is_lower_hex(job_id, 12) {
            return Err(JobError::value("managed recovery job identity is malformed"));
        }
        let job_dir = self.inner.dir.join(job_id);
        let path = job_dir.join(".managed-restart.json");
        let before = implexity_io::fsguard::stat_nofollow(&path)?;
        if !before.is_owned_single_regular() || before.size < 2 || before.size > 16 * 1024 * 1024 {
            return Err(JobError::value("managed recovery descriptor is unsafe"));
        }
        let text = std::fs::read_to_string(&path)?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?;
        let keys = [
            "schema",
            "job_id",
            "owner",
            "request",
            "request_sha256",
            "spec_fingerprint",
            "t_submit",
            "resume_generation",
            "snapshot",
        ];
        let ok = payload.as_object().is_some_and(|p| {
            p.len() == keys.len()
                && keys.iter().all(|k| p.contains_key(*k))
                && p["schema"] == job::MANAGED_OPTIMIZATION_RESTART_SCHEMA
                && p["job_id"] == job_id
                && p["owner"] == json!({"kind": "implicit_optimize", "id": job_id})
                && p["request"].is_object()
                && p["request_sha256"].as_str().is_some_and(|s| is_lower_hex(s, 64))
                && p["spec_fingerprint"].is_object()
                && p["t_submit"].is_number()
                && p["resume_generation"].is_boolean()
                && p["snapshot"].is_object()
                && p["snapshot"].get("job_id") == Some(&Value::String(job_id.into()))
        });
        if !ok {
            return Err(JobError::value("managed recovery descriptor is malformed"));
        }
        let payload = payload.as_object().cloned().unwrap_or_default();
        if managed_restart_request_sha256(&payload["request"])?
            != payload["request_sha256"].as_str().unwrap_or("")
        {
            return Err(JobError::value("managed recovery request identity drifted"));
        }
        if Self::managed_artifact_fingerprint(&job_dir.join("spec.json"), 64 * 1024 * 1024)?
            != payload["spec_fingerprint"]
        {
            return Err(JobError::value("managed recovery specification identity drifted"));
        }
        Ok((payload, job_dir))
    }


    pub(crate) fn managed_artifact_fingerprint(path: &Path, maximum_bytes: u64) -> JobResult<Value> {
        let mut file = crate::private::open_nofollow(path)?;
        let before = implexity_io::fsguard::stat_file(&file)?;
        if !before.is_owned_single_regular() || before.size > maximum_bytes {
            return Err(JobError::value("managed artifact is not a bounded owned regular file"));
        }
        let data = crate::private::read_limited(&mut file, maximum_bytes)?;
        let after = implexity_io::fsguard::stat_file(&file)?;
        if u64::try_from(data.len()).ok() != Some(before.size)
            || (before.dev_ino(), before.size, before.mtime) != (after.dev_ino(), after.size, after.mtime)
        {
            return Err(JobError::value("managed artifact changed while fingerprinted"));
        }
        Ok(json!({"bytes": data.len(), "sha256": sha256_hex(&data)}))
    }

    fn reject_recovered_terminal() -> TerminalValidator {
        Arc::new(|_partial: &Path| {
            Err("restart recovered control authority but not terminal acceptance authority".to_string())
        })
    }

    fn watch_recovered(&self, job_id: &str, operation_id: &str) {
        let status = self.inner.supervisor.wait(operation_id, None);
        let lifecycle = self.lifecycle_lock(job_id);
        let _guard = lifecycle.hold();
        let event = {
            let mut state = self.state();
            let entry = state.jobs.get(job_id).cloned();
            let event = match entry {
                Some(JobEntry::Recovered(job)) => {
                    let mut job = lock(&job);
                    match &status {
                        Ok(s) => {
                            job.managed_supervisor_state.clone_from(&s.state);
                            job.managed_supervisor_reason.clone_from(&s.terminal_reason);
                            if s.state.as_str() == "cancelled" {
                                job.status = "stopped".into();
                                job.message =
                                    "stopped after restart through the original managed operation".into();
                                job.mark("stopped");
                            } else {
                                job.status = "error".into();
                                job.message = format!(
                                    "recovered operation reached terminal state {}; no result was accepted after restart",
                                    s.state.as_str()
                                );
                                job.mark("recovery_terminal");
                            }
                            if s.state.as_str() == "succeeded" {
                                job.progress = 1.0;
                            }
                        }
                        Err(e) => {
                            job.status = "error".into();
                            job.message = format!("recovered operation supervision failed: {e}");
                            job.mark("recovery_terminal");
                        }
                    }
                    Some(Arc::clone(&job.managed_terminal_event))
                }
                _ => None,
            };
            if state.active.as_deref() == Some(job_id) {
                state.active = None;
            }
            if state.model_authority_job_id.as_deref() == Some(job_id) {
                state.model_authority_job_id = None;
            }
            event
        };

        let recovered_dir = match self.state().jobs.get(job_id) {
            Some(JobEntry::Recovered(job)) => Some(PathBuf::from(lock(job).job_dir.clone())),
            _ => None,
        };
        if let Some(dir) = recovered_dir.filter(|d| !d.as_os_str().is_empty()) {
            let _ = crate::checkpoint_scratch::remove_checkpoint_scratch(&dir);
        }
        if self.inner.models.live_optimisation().and_then(|l| l.get("job_id").cloned())
            == Some(Value::String(job_id.into()))
        {
            self.inner.models.set_live_optimisation(None);
        }
        self.event(job_id);
        if let Some(e) = event {
            e.set();
        }
    }

    fn rehydrate_managed_optimizations(&self) -> JobResult<()> {
        let recoveries: Vec<_> = self
            .inner
            .supervisor
            .recoverable_operations()
            .into_iter()
            .filter(|r| r.owner_kind == "implicit_optimize")
            .collect();
        let owners: Vec<&String> = recoveries.iter().map(|r| &r.owner_id).collect();
        let unique: std::collections::BTreeSet<&&String> = owners.iter().collect();
        if unique.len() != owners.len() {
            return Err(JobError::runtime("multiple live managed operations claim one optimization job"));
        }
        for recovery in recoveries {
            if !recovery.root_live {
                continue;
            }
            let owner = recovery.owner_id.clone();
            let (descriptor, job_dir) = self.read_managed_restart_descriptor(&owner)?;
            let control: ManagedEvaluationControl =
                self.inner.supervisor.reattach(&recovery.operation_id, Self::reject_recovered_terminal())?;
            let job = RecoveredModelOptJob::new(
                &descriptor,
                recovery.root_live,
                &recovery.state,
                control,
                &job_dir,
            );
            let id = job.id.clone();
            let t_submit = job.t_submit;
            let operation_id = job.managed_operation_id.clone();
            {
                let mut state = self.state();
                state.jobs.insert(id.clone(), JobEntry::Recovered(Arc::new(Mutex::new(job))));
                state.lifecycle_locks.insert(id.clone(), job::Gate::new());
                state.active = Some(id.clone());
                state.model_authority_job_id = Some(id.clone());
            }
            self.inner.models.set_live_optimisation(Some(json!({
                "job_id": id,
                "since": implexity_optim::numeric::float_value(t_submit),
                "job": format!("/v1/implicit/optimize/jobs/{id}"),
                "free": [],
                "note": "reattached read/stop-only optimization",
            })));
            let me = self.clone();
            std::thread::spawn(move || me.watch_recovered(&id, &operation_id));
        }
        Ok(())
    }

    pub(crate) fn event(&self, job_id: &str) {
        let payload = {
            let state = self.state();
            state.jobs.get(job_id).map(|entry| {
                let (status, progress, message, iterations) = match entry {
                    JobEntry::Live(j) => {
                        let j = lock(j);
                        (j.status.clone(), j.progress, j.message.clone(), j.rows.len())
                    }
                    JobEntry::Recovered(j) => {
                        let j = lock(j);
                        (j.status.clone(), j.progress, j.message.clone(), 0)
                    }
                };
                json!({
                    "event": "opt_job", "kind": "implicit_optimize", "job_id": job_id,
                    "status": status,
                    "progress": implexity_optim::numeric::float_value(implexity_mesh::numeric::py_round_digits(progress, 3)),
                    "message": message, "iterations": iterations,
                })
            })
        };
        if let Some(message) = payload {
            self.inner.host.broadcast(&message);
        }
    }

    pub(crate) fn event_iter(&self, job_id: &str, row: &Value, applied: bool) {
        self.inner.host.broadcast(&json!({
            "event": "opt_iter", "kind": "implicit_optimize", "job_id": job_id,
            "row": row, "document_applied": applied,
        }));
    }
}
