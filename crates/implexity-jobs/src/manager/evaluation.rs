// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use implexity_authoring::progressive_fields::{FieldIdentity, Reducer};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{py_str, truthy};
use implexity_geometry::NdArray;
use implexity_geometry::value::ArrayData;
use implexity_io::npy::{NpyArray, NpyData};
use serde_json::{Map, Value, json};

use super::declare::Declaration;
use super::job::{Event, JobMeta};
use super::{
    ModelOptimizeManager, attach_managed_transport_timing, child_env, managed_exact_parent_resources,
    validate_managed_transport,
};
use crate::artifacts::{
    ARTIFACT_SCHEMA, Limits, ResultArtifactStore, canonical_f8_sha256, scan_tree_bounded,
};
use crate::error::{JobError, JobResult};
use crate::managed_evaluation::{
    ManagedEvaluationControl, ManagedEvaluationManager, ManagedEvaluationPolicy, StartOptions,
};
use crate::managed_workers::{RESULT_FILE, RESULT_SCHEMA};
use crate::private::{canonical_text, perf_counter_ns, sha256_hex};

pub(crate) const FIELD_VIEW_SAMPLE_LIMIT: usize = 500_000;

fn opt1<T>(problem: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(problem))
}

fn verr(message: impl Into<String>) -> JobError {
    JobError::value(message)
}

fn now_ns() -> u64 {
    u64::try_from(perf_counter_ns()).unwrap_or(0)
}

fn expected_responses(request: &Map<String, Value>) -> Vec<String> {
    match request.get("responses").and_then(Value::as_array) {
        Some(r) if !r.is_empty() => r.iter().map(py_str).collect(),
        _ => request.get("response").filter(|v| !v.is_null()).map(py_str).into_iter().collect(),
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactValidation {
    pub expected_design_coordinates: Option<Vec<String>>,
    pub expected_design_values: Option<BTreeMap<String, ndarray::ArrayD<f64>>>,
    pub limits: Limits,
}

pub struct ManagedProviderEvaluationCapsule {
    pub operation_kind: String,
    owner: ModelOptimizeManager,
    mode: String,
    request: Map<String, Value>,
    request_file: PathBuf,
    request_sha256: String,
    meta: JobMeta,
    public_request: Map<String, Value>,
    artifact_output: bool,
    timeout: Option<f64>,
    memory_limit: Option<i64>,
    state: Mutex<CapsuleState>,
    lease_released: Arc<Event>,
    frozen_content_id: String,
    frozen_design_state_id: String,
    frozen_design_values: Option<BTreeMap<String, ndarray::ArrayD<f64>>>,
}

#[derive(Default)]
struct CapsuleState {
    started: bool,
    finalized: bool,
    control: Option<ManagedEvaluationControl>,
}

impl std::fmt::Debug for ManagedProviderEvaluationCapsule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<private managed exact-evaluation capsule>")
    }
}

impl ManagedProviderEvaluationCapsule {
    #[must_use]
    pub fn request(&self) -> &Map<String, Value> {
        &self.request
    }

    #[must_use]
    pub fn mode(&self) -> &str {
        &self.mode
    }

    #[must_use]
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    #[must_use]
    pub fn artifact_validation_options(&self) -> ArtifactValidation {
        let hard_total: u64 = 768 * 1024 * 1024;
        let total = match self.memory_limit {
            None => hard_total,
            Some(m) => hard_total.min((8 * 1024 * 1024_u64).max(u64::try_from(m).unwrap_or(0) / 4)),
        };
        ArtifactValidation {
            expected_design_coordinates: Some(
                self.request
                    .get("design_coordinates")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(py_str).collect())
                    .unwrap_or_default(),
            ),
            expected_design_values: self.frozen_design_values.clone(),
            limits: Limits {
                max_payload_bytes: (512 * 1024 * 1024).min(total),
                max_uncompressed_bytes: total,
                max_field_bytes: (384 * 1024 * 1024).min(total),
                max_fields: 4096,
                max_compression_ratio: 20_000.0,
                max_header_bytes: 64 * 1024,
                require_numeric: true,
                ..Limits::default()
            },
        }
    }


    pub fn read_terminal_result(&self, directory: &Path) -> JobResult<Map<String, Value>> {
        let meta = std::fs::symlink_metadata(directory)
            .map_err(|_| verr("managed exact terminal directory is missing"))?;
        if !meta.file_type().is_dir() {
            return Err(verr("managed exact terminal directory is unsafe"));
        }
        let path = directory.join(RESULT_FILE);
        let stat = std::fs::symlink_metadata(&path).map_err(|_| verr("managed exact result is missing"))?;
        if !stat.file_type().is_file() || stat.len() < 2 || stat.len() > 64 * 1024 * 1024 {
            return Err(verr("managed exact result file is unsafe"));
        }
        let text = std::fs::read_to_string(&path)?;
        let envelope: Value =
            serde_json::from_str(&text).map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?;
        let ok = envelope.as_object().is_some_and(|e| {
            e.len() == 4
                && e.get("schema").and_then(Value::as_str) == Some(RESULT_SCHEMA)
                && e.get("mode").and_then(Value::as_str) == Some(self.mode.as_str())
                && e.get("request_sha256").and_then(Value::as_str) == Some(self.request_sha256.as_str())
                && e.get("result").is_some_and(Value::is_object)
        });
        if !ok {
            return Err(verr("managed exact result envelope drifted"));
        }
        Ok(envelope["result"].as_object().cloned().unwrap_or_default())
    }

    fn validate_terminal(&self, partial: &Path) -> JobResult<()> {
        let result = self.read_terminal_result(partial)?;
        implexity_runtime::provider_job_authority::validate_exact_effort_evidence(
            &Value::Object(result.clone()),
            self.meta.get("computation_effort"),
        )?;
        self.owner.validate_managed_provider_result(self, &result)?;
        let artifact_root = partial.join("artifacts");
        if self.artifact_output {
            self.owner.validate_managed_provider_artifacts(
                &result,
                &artifact_root,
                &ArtifactExpectation {
                    design_state_id: &self.frozen_design_state_id,
                    model_content_id: &self.frozen_content_id,
                    provider: &self.meta.s("physics_provider"),
                    solve_id: &self.meta.s("solve_id"),
                    responses: &expected_responses(&self.request),
                },
                &self.artifact_validation_options(),
            )
        } else if artifact_root.exists() {
            Err(verr("managed operation published undeclared artifacts"))
        } else {
            Ok(())
        }
    }


    pub fn start(
        self: &Arc<Self>,
        supervisor: &ManagedEvaluationManager,
    ) -> JobResult<ManagedEvaluationControl> {
        {
            let mut st = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if st.started {
                return Err(JobError::runtime("managed exact-evaluation capsule is one-shot"));
            }
            st.started = true;
        }
        let lease = self.owner.inner.host.eval_lock()?;
        if !lease.acquire(false, None).map_err(|e| JobError::runtime(e.0))? {
            return Err(JobError::runtime("another heavy operation is already running"));
        }
        let release = |lease: &implexity_io::heavy_lease::HeavyOperationLease| {
            let _ = lease.release();
        };
        let mut command = self.owner.inner.host.worker_command();
        command.extend([
            crate::worker_cli::MANAGED_PROVIDER.to_string(),
            "--mode".into(),
            self.mode.clone(),
            "--request-file".into(),
            self.request_file.to_string_lossy().into_owned(),
            "--request-sha256".into(),
            self.request_sha256.clone(),
        ]);
        if self.artifact_output {
            command.push("--artifact-output".into());
        }
        let started = (|| -> JobResult<ManagedEvaluationControl> {
            let policy = ManagedEvaluationPolicy::with_timeout(self.timeout)?;
            let policy =
                ManagedEvaluationPolicy { memory_limit_bytes: self.memory_limit, ..policy }.checked()?;
            let me = Arc::clone(self);
            let validator: crate::managed_evaluation::TerminalValidator =
                Arc::new(move |partial: &Path| me.validate_terminal(partial).map_err(|e| e.describe()));
            let mut options = StartOptions::new(policy, validator);
            options.cwd = Some(self.owner.inner.host.worker_cwd());
            options.env = child_env()?;
            options.replace_env = true;
            options.stdin = lease.inheritable_stdio().map_err(|e| JobError::runtime(e.0))?;
            Ok(supervisor.start(&command, options)?)
        })();
        let control = match started {
            Ok(c) => c,
            Err(e) => {
                release(&lease);
                self.lease_released.set();
                return Err(e);
            }
        };
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).control = Some(control.clone());
        let operation_id = control.operation_id().to_string();
        let watcher_supervisor = self.owner.clone();
        let released = Arc::clone(&self.lease_released);
        std::thread::Builder::new()
            .name(format!("managed-exact-lease-{}", &operation_id[..12.min(operation_id.len())]))
            .spawn(move || {
                let _ = watcher_supervisor.inner.supervisor.wait(&operation_id, None);
                release(&lease);
                released.set();
            })
            .map_err(JobError::from)?;
        Ok(control)
    }


    pub fn finalize(&self, committed_directory: &Path) -> JobResult<Value> {
        {
            let mut st = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !st.started || st.control.is_none() {
                return Err(JobError::runtime("managed exact-evaluation capsule was not started"));
            }
            if st.finalized {
                return Err(JobError::runtime("managed exact-evaluation capsule was already finalized"));
            }
            st.finalized = true;
        }
        self.lease_released.wait(None);
        let outcome = self.owner.finalize_managed_provider_evaluation(self, committed_directory);
        if outcome.is_err() {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).finalized = false;
        }
        outcome
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ArtifactExpectation<'a> {
    pub design_state_id: &'a str,
    pub model_content_id: &'a str,
    pub provider: &'a str,
    pub solve_id: &'a str,
    pub responses: &'a [String],
}

#[must_use]
pub fn managed_artifact_rows(value: &Value) -> Vec<(String, Map<String, Value>)> {
    fn visit(node: &Value, rows: &mut Vec<(String, Map<String, Value>)>) {
        match node {
            Value::Object(m) => {
                for (key, child) in m {
                    if (key == "artifact" || key == "design_artifact")
                        && let Value::Object(o) = child
                    {
                        rows.push((key.clone(), o.clone()));
                    }
                    visit(child, rows);
                }
            }
            Value::Array(items) => items.iter().for_each(|c| visit(c, rows)),
            _ => {}
        }
    }
    let mut rows = Vec::new();
    visit(value, &mut rows);
    rows
}


pub fn require_finite_json(value: &Value, label: &str) -> JobResult<()> {
    match value {
        Value::Number(n) if !n.as_f64().is_some_and(f64::is_finite) => {
            Err(verr(format!("{label} contains a nonfinite number")))
        }
        Value::Array(items) => {
            for (i, c) in items.iter().enumerate() {
                require_finite_json(c, &format!("{label}[{i}]"))?;
            }
            Ok(())
        }
        Value::Object(m) => {
            for (k, c) in m {
                require_finite_json(c, &format!("{label}.{k}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}


pub fn finite_scalar(value: Option<&Value>, label: &str) -> JobResult<f64> {
    match value.and_then(|v| v.as_f64().filter(|_| v.is_number())) {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(verr(format!("{label} must be a finite scalar"))),
    }
}


#[allow(clippy::float_cmp)]
pub fn validate_provider_worker_timing(
    value: Option<&Value>,
    operation: &str,
    evidence: Option<&Value>,
) -> JobResult<()> {
    let keys = ["schema", "operation", "clock", "accounting", "nanoseconds", "measurements"];
    let Some(v) = value
        .and_then(Value::as_object)
        .filter(|v| v.len() == keys.len() && keys.iter().all(|k| v.contains_key(*k)))
    else {
        return Err(verr("managed provider worker timing shape drifted"));
    };
    let accounting =
        "wall_time_ns=provider_setup_wall_ns+provider_numerical_wall_ns+transport_artifact_wall_ns";
    if v["schema"] != "implexity-provider-worker-timing/1"
        || v["operation"] != operation
        || v["clock"] != "time.perf_counter_ns"
        || v["accounting"] != accounting
    {
        return Err(verr("managed provider worker timing identity drifted"));
    }
    let ns_keys = [
        "provider_setup_wall_ns",
        "provider_numerical_wall_ns",
        "transport_artifact_wall_ns",
        "wall_time_ns",
    ];
    let Some(ns) = v["nanoseconds"]
        .as_object()
        .filter(|n| n.len() == ns_keys.len() && ns_keys.iter().all(|k| n.get(*k).is_some_and(Value::is_u64)))
    else {
        return Err(verr("managed provider worker timing values drifted"));
    };
    let get = |k: &str| ns[k].as_u64().unwrap_or(0);
    if get("provider_setup_wall_ns") + get("provider_numerical_wall_ns") + get("transport_artifact_wall_ns")
        != get("wall_time_ns")
    {
        return Err(verr("managed provider worker timing accounting identity failed"));
    }
    if operation == "preflight"
        && (get("provider_numerical_wall_ns") != 0 || get("transport_artifact_wall_ns") != 0)
    {
        return Err(verr("managed provider preflight reported numerical timing"));
    }
    let Some(measurements) = v["measurements"].as_object() else {
        return Err(verr("managed provider worker timing measurements drifted"));
    };
    let expected: BTreeMap<String, f64> = ns_keys
        .iter()
        .map(|k| {
            #[allow(clippy::cast_precision_loss)]
            let s = get(k) as f64 / 1_000_000_000.0;
            (format!("{}_s", k.trim_end_matches("_ns")), s)
        })
        .collect();
    let ok = measurements.len() == expected.len()
        && expected
            .iter()
            .all(|(k, e)| measurements.get(k).is_some_and(|m| m.is_f64() && m.as_f64() == Some(*e)));
    if !ok {
        return Err(verr("managed provider worker timing measurements drifted"));
    }
    let observed = evidence.and_then(|e| e.get("observed")).and_then(|o| o.get("observed"));
    let wall = observed.and_then(|o| o.get("wall_time_s"));
    let ok = wall.is_some_and(|w| {
        w.is_f64()
            && w.as_f64().is_some_and(|x| x.is_finite() && x >= 0.0 && x + 1e-9 >= expected["wall_time_s"])
    });
    if !ok {
        return Err(verr("managed provider worker timing exceeds sealed observation"));
    }
    Ok(())
}

pub(crate) fn to_ndarray(array: &NpyArray, stream: bool) -> Option<NdArray> {
    let shape = array.shape.clone();
    let complex = |data: Vec<f64>, f32_: bool| -> Option<NdArray> {
        let mut s = shape.clone();
        s.push(2);
        let s = if s.len() == 3 {
            s
        } else if stream {
            let components: usize = s[3..].iter().product();
            vec![s[0], s[1], s[2], components]
        } else {
            s
        };
        if f32_ {
            #[allow(clippy::cast_possible_truncation)]
            NdArray::new(s, ArrayData::F32(data.iter().map(|v| *v as f32).collect()))
        } else {
            NdArray::from_f64(s, data)
        }
    };
    let flat = |s: Vec<usize>| -> Vec<usize> {
        if stream && s.len() > 3 {
            let components: usize = s[3..].iter().product();
            vec![s[0], s[1], s[2], components]
        } else {
            s
        }
    };
    match &array.data {
        NpyData::F64(v) => NdArray::from_f64(flat(shape.clone()), v.clone()),
        NpyData::F32(v) => NdArray::new(flat(shape.clone()), ArrayData::F32(v.clone())),
        NpyData::I64(v) => NdArray::new(flat(shape.clone()), ArrayData::I64(v.clone())),
        NpyData::I32(v) => NdArray::new(flat(shape.clone()), ArrayData::I32(v.clone())),
        NpyData::I16(v) => NdArray::new(flat(shape.clone()), ArrayData::I16(v.clone())),
        NpyData::I8(v) => NdArray::new(flat(shape.clone()), ArrayData::I8(v.clone())),
        NpyData::U8(v) => NdArray::new(flat(shape.clone()), ArrayData::U8(v.clone())),
        NpyData::Bool(v) => NdArray::new(flat(shape.clone()), ArrayData::Bool(v.clone())),
        NpyData::C64(v) => complex(v.iter().flat_map(|c| [f64::from(c[0]), f64::from(c[1])]).collect(), true),
        NpyData::C128(v) => complex(v.iter().flat_map(|c| [c[0], c[1]]).collect(), false),
        _ => array
            .to_f64()
            .map(|a| a.iter().copied().collect())
            .and_then(|d| NdArray::from_f64(flat(shape.clone()), d)),
    }
}

pub(crate) fn field_array(array: &NpyArray) -> implexity_geometry::field_views::FieldArray {
    let complex = matches!(array.data, NpyData::C64(_) | NpyData::C128(_));
    let data = match &array.data {
        NpyData::C64(v) => v.iter().flat_map(|c| [f64::from(c[0]), f64::from(c[1])]).collect(),
        NpyData::C128(v) => v.iter().flat_map(|c| [c[0], c[1]]).collect(),
        _ => array.to_f64().map(|a| a.iter().copied().collect()).unwrap_or_default(),
    };
    implexity_geometry::field_views::FieldArray { shape: array.shape.clone(), data, complex }
}

fn triplet(value: Option<&Value>) -> [i64; 3] {
    let v: Vec<i64> = value
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if v.len() == 3 { [v[0], v[1], v[2]] } else { [32, 32, 32] }
}

fn stream_ref(publication: &Value, registration: &Value, extra: Map<String, Value>) -> Value {
    let manifest = &publication["manifest"];
    let mut m = Map::new();
    m.insert("schema".into(), json!("implexity-field-stream-ref/1"));
    m.insert("field_id".into(), publication["field_id"].clone());
    for (k, v) in extra {
        m.insert(k, v);
    }
    m.insert("parent_field_id".into(), manifest.get("parent_field_id").cloned().unwrap_or(Value::Null));
    for k in ["manifest", "delta", "tile"] {
        m.insert(k.into(), manifest.get("endpoints").and_then(|e| e.get(k)).cloned().unwrap_or(Value::Null));
    }
    m.insert("visual_range".into(), Value::Null);
    m.insert("registration".into(), registration.clone());
    m.insert("exact_delta".into(), manifest.get("exact_delta").cloned().unwrap_or(Value::Null));
    Value::Object(m)
}

impl ModelOptimizeManager {

    pub(crate) fn enrich_provider_worker_report(
        &self,
        rep: &Map<String, Value>,
        meta: &JobMeta,
        seconds: Option<f64>,
    ) -> JobResult<Map<String, Value>> {
        let Some(binding) = Some(meta.get("computation_effort")).filter(|v| !v.is_null()) else {
            return opt1("provider worker result has no server-bound computation effort");
        };
        let bound = rep
            .get("computation_evidence")
            .and_then(|e| e.get("payload"))
            .and_then(|p| p.get("bound_keys"))
            .and_then(Value::as_array);
        let ok = bound.is_some_and(|b| {
            let keys: BTreeSet<String> =
                b.iter().map(py_str).chain(std::iter::once("computation_evidence".to_string())).collect();
            rep.keys().cloned().collect::<BTreeSet<_>>() == keys
        });
        if !ok {
            return opt1("provider worker result contains unbound transport fields");
        }
        let mut rep = implexity_runtime::provider_job_authority::validate_exact_effort_evidence(
            &Value::Object(rep.clone()),
            binding,
        )
        .map_err(|e| JobError::optimize1(format!("provider worker computation evidence: {}", e.message())))?;
        let seconds = seconds.or_else(|| {
            rep.get("computation_evidence")
                .and_then(|e| e.get("observed"))
                .and_then(|o| o.get("observed"))
                .and_then(|o| o.get("wall_time_s"))
                .filter(|v| v.is_number())
                .and_then(Value::as_f64)
        });
        let Some(seconds) = seconds.filter(|s| s.is_finite() && *s >= 0.0) else {
            return opt1("provider worker reported an invalid exact elapsed time");
        };
        let fv = implexity_optim::numeric::float_value;
        rep.insert("seconds".into(), fv(implexity_mesh::numeric::py_round_digits(seconds, 2)));
        rep.insert("node".into(), meta.get("node").clone());
        rep.insert("declared_by".into(), meta.get("declared_by").clone());
        rep.insert("case_source".into(), meta.get("case_source").clone());
        rep.insert("free_plan".into(), meta.get("plan").clone());
        rep.insert("warnings".into(), meta.get("warnings").clone());
        rep.insert("ignored".into(), meta.get("ignored").clone());
        rep.insert("model".into(), json!({"content_id": meta.get("content_id")}));
        Ok(rep)
    }


    #[allow(clippy::too_many_lines)]
    pub(crate) fn validate_managed_provider_result(
        &self,
        capsule: &ManagedProviderEvaluationCapsule,
        result: &Map<String, Value>,
    ) -> JobResult<()> {
        let evidence = result.get("computation_evidence");
        let bound = evidence
            .and_then(|e| e.get("payload"))
            .and_then(|p| p.get("bound_keys"))
            .and_then(Value::as_array);
        let ok = bound.is_some_and(|b| {
            let keys: BTreeSet<String> =
                b.iter().map(py_str).chain(std::iter::once("computation_evidence".to_string())).collect();
            result.keys().cloned().collect::<BTreeSet<_>>() == keys
        });
        if !ok {
            return Err(verr("managed exact provider result has unbound fields"));
        }
        let mode = capsule.mode.as_str();
        let expected_provider = capsule.meta.s("physics_provider");
        let expected_coordinates = capsule.request.get("design_coordinates").cloned().unwrap_or(json!([]));
        let (schemas, kinds): (&[&str], &[&str]) = match mode {
            "preflight" => (&["implexity-provider-preflight/1"], &["implicit_provider_preflight"]),
            "evaluate" => (&["implexity-provider-results/2"], &["implicit_results"]),
            _ => (
                &["implexity-provider-sensitivity/2", "implexity-provider-sensitivities/1"],
                &["implicit_sensitivity", "implicit_sensitivities"],
            ),
        };
        let schema = result.get("schema").and_then(Value::as_str).unwrap_or("");
        let kind = result.get("kind").and_then(Value::as_str).unwrap_or("");
        if !schemas.contains(&schema) || !kinds.contains(&kind) {
            return Err(verr("managed exact provider result schema drifted"));
        }
        for (key, expected) in [
            ("provider", json!(expected_provider)),
            ("design_state_id", json!(capsule.frozen_design_state_id)),
            ("design_coordinates", expected_coordinates.clone()),
        ] {
            if result.get(key) != Some(&expected) {
                return Err(verr(format!("managed exact provider result {key} drifted")));
            }
        }
        if let Some(report) = result.get("solver_recovery") {
            let report = crate::solver_recovery::validate(report)?;
            if mode == "preflight" || report.get("status").and_then(Value::as_str) != Some("recovered") { return Err(verr("successful provider result has an inconsistent recovery report")); }
        }
        validate_provider_worker_timing(result.get("execution_timing"), mode, evidence)?;
        crate::provider_worker::validate_provider_admission(
            result.get("provider_admission"),
            result.get("execution_timing"),
        )?;
        require_finite_json(&Value::Object(result.clone()), "managed exact result")?;
        let mut artifact_values = Vec::new();
        fn collect(node: &Value, out: &mut Vec<Value>) {
            match node {
                Value::Object(m) => {
                    for (k, c) in m {
                        if k == "artifact" || k == "design_artifact" {
                            out.push(c.clone());
                        }
                        collect(c, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|c| collect(c, out)),
                _ => {}
            }
        }
        collect(&Value::Object(result.clone()), &mut artifact_values);
        if artifact_values.iter().any(|v| !v.is_null() && !v.is_object()) {
            return Err(verr("managed exact artifact descriptor is malformed"));
        }
        if !capsule.artifact_output && artifact_values.iter().any(|v| !v.is_null()) {
            return Err(verr("managed operation returned undeclared artifact references"));
        }
        if mode == "preflight" {
            let coupling = result.get("couplingReport");
            if result.get("ok") != Some(&Value::Bool(true))
                || !coupling.is_some_and(|c| c.get("ok") == Some(&Value::Bool(true)))
            {
                return Err(verr("managed exact provider preflight did not succeed"));
            }
            return Ok(());
        }
        if mode == "evaluate" {
            let declared: Vec<String> = capsule
                .meta
                .get("provider_responses")
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|r| {
                            r.get("name")
                                .filter(|v| truthy(v))
                                .or_else(|| r.get("response").filter(|v| truthy(v)))
                                .map(py_str)
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let responses = result.get("responses").and_then(Value::as_object);
            let unique: BTreeSet<&String> = declared.iter().collect();
            let ok = !declared.is_empty()
                && unique.len() == declared.len()
                && responses.is_some_and(|r| r.keys().collect::<BTreeSet<_>>() == unique)
                && result.get("diagnostics").is_some_and(Value::is_object)
                && result.get("fields").is_some_and(Value::is_object)
                && result.contains_key("artifact")
                && result.contains_key("design_artifact")
                && result.get("field_registration").is_none_or(|v| v.is_null() || v.is_object());
            if !ok {
                return Err(verr("managed exact provider evaluation shape drifted"));
            }
            for (response, value) in responses.cloned().unwrap_or_default() {
                finite_scalar(Some(&value), &format!("managed exact response {response}"))?;
            }
            return Ok(());
        }
        let single = capsule.request.get("response").filter(|v| !v.is_null());
        let batch = capsule.request.get("responses").filter(|v| !v.is_null());
        if single.is_some() == batch.is_some() {
            return Err(verr("managed sensitivity request contract drifted"));
        }
        let value_map_ok = |map: Option<&Value>, name: &str, value: f64, label: &str| -> JobResult<bool> {
            let Some(m) = map.and_then(Value::as_object).filter(|m| m.len() == 1 && m.contains_key(name))
            else {
                return Ok(false);
            };
            let Some(inner) = m[name].as_object().filter(|i| i.len() == 1 && i.contains_key("value")) else {
                return Ok(false);
            };
            #[allow(clippy::float_cmp)]
            Ok(finite_scalar(inner.get("value"), label)? == value)
        };
        if let Some(single) = single {
            let name = py_str(single);
            let ok = schema == "implexity-provider-sensitivity/2"
                && result.get("response") == Some(single)
                && result.get("diagnostics").is_some_and(Value::is_object)
                && result.get("fields").is_some_and(Value::is_object)
                && result.get("gradient_coordinates").is_some_and(Value::is_object)
                && result.contains_key("artifact")
                && result.contains_key("design_artifact");
            if !ok {
                return Err(verr("managed single-sensitivity response drifted"));
            }
            let value = finite_scalar(result.get("value"), "managed exact sensitivity value")?;
            if !value_map_ok(result.get("responses"), &name, value, "managed exact nested sensitivity value")?
            {
                return Err(verr("managed single-sensitivity value map drifted"));
            }
            return Ok(());
        }
        let batch: Vec<String> =
            batch.and_then(Value::as_array).map(|a| a.iter().map(py_str).collect()).unwrap_or_default();
        let batch_set: BTreeSet<&String> = batch.iter().collect();
        let members = result.get("sensitivities").and_then(Value::as_object);
        if schema != "implexity-provider-sensitivities/1"
            || result.get("response_order") != Some(&json!(batch))
            || members.is_none_or(|m| m.keys().collect::<BTreeSet<_>>() != batch_set)
        {
            return Err(verr("managed sensitivity batch contract drifted"));
        }
        let members = members.cloned().unwrap_or_default();
        let response_values = result.get("responses").and_then(Value::as_object);
        if response_values.is_none_or(|r| r.keys().collect::<BTreeSet<_>>() != batch_set)
            || !result.get("diagnostics").is_some_and(Value::is_object)
            || !result.contains_key("design_artifact")
        {
            return Err(verr("managed sensitivity batch response map drifted"));
        }
        let response_values = response_values.cloned().unwrap_or_default();
        for response in &batch {
            let member = &members[response];
            let ok = member.as_object().is_some_and(|m| {
                m.get("schema").and_then(Value::as_str) == Some("implexity-provider-sensitivity/2")
                    && m.get("provider") == Some(&json!(expected_provider))
                    && m.get("design_state_id") == Some(&json!(capsule.frozen_design_state_id))
                    && m.get("design_coordinates") == Some(&expected_coordinates)
                    && m.get("response") == Some(&json!(response))
                    && m.get("diagnostics").is_some_and(Value::is_object)
                    && m.get("fields").is_some_and(Value::is_object)
                    && m.get("gradient_coordinates").is_some_and(Value::is_object)
                    && m.contains_key("artifact")
                    && m.contains_key("design_artifact")
            });
            if !ok {
                return Err(verr("managed sensitivity batch member drifted"));
            }
            let value = finite_scalar(
                member.get("value"),
                &format!("managed exact sensitivity value for {response}"),
            )?;
            let top = finite_scalar(
                response_values.get(response),
                &format!("managed exact batch response {response}"),
            )?;
            #[allow(clippy::float_cmp)]
            if top != value {
                return Err(verr("managed sensitivity batch response value drifted"));
            }
            if !value_map_ok(
                member.get("responses"),
                response,
                value,
                "managed exact nested sensitivity value",
            )? {
                return Err(verr("managed sensitivity batch member value map drifted"));
            }
        }
        Ok(())
    }


    #[allow(clippy::too_many_lines)]
    pub(crate) fn validate_managed_provider_artifacts(
        &self,
        result: &Map<String, Value>,
        artifact_root: &Path,
        expect: &ArtifactExpectation<'_>,
        options: &ArtifactValidation,
    ) -> JobResult<()> {
        let initial_tree = scan_tree_bounded(artifact_root, 16_384, 32)
            .map_err(|_| verr("managed exact artifact root cannot be safely inspected"))?;
        if !initial_tree.contains("design_states") {
            return Err(verr("managed exact design artifact store is missing"));
        }
        let limits = options.limits;
        let total_payload = limits.max_payload_bytes;
        let total_uncompressed = limits.max_uncompressed_bytes;
        let total_fields = limits.max_fields;
        let mut aggregate = (0_u64, 0_u64, 0_usize);
        let mut admit = |manifest: &Map<String, Value>,
                         inspected: &BTreeMap<String, crate::artifacts::InspectedField>|
         -> JobResult<()> {
            let payload =
                manifest.get("payload").and_then(|p| p.get("bytes")).and_then(Value::as_u64).unwrap_or(0);
            let proposed = (
                aggregate.0 + payload,
                aggregate.1 + inspected.values().map(|r| r.archive_bytes).sum::<u64>(),
                aggregate.2 + inspected.len(),
            );
            if proposed.0 > total_payload || proposed.1 > total_uncompressed || proposed.2 > total_fields {
                return Err(verr("managed exact artifact set exceeds its aggregate budget"));
            }
            aggregate = proposed;
            Ok(())
        };
        let coordinates = options.expected_design_coordinates.clone();
        if let Some(c) = &coordinates
            && (c.is_empty()
                || c.iter().any(String::is_empty)
                || c.iter().collect::<BTreeSet<_>>().len() != c.len())
        {
            return Err(verr("managed exact design coordinate contract drifted"));
        }
        let Some(top_design) = result.get("design_artifact").and_then(Value::as_object).cloned() else {
            return Err(verr("managed exact design artifact is missing"));
        };
        let mut design_occurrences = vec![top_design.clone()];
        let mut result_occurrences: Vec<(Map<String, Value>, Option<String>, Value, Value, Value)> =
            Vec::new();
        if result.get("schema").and_then(Value::as_str) == Some("implexity-provider-sensitivities/1") {
            let members = result.get("sensitivities").and_then(Value::as_object).cloned().unwrap_or_default();
            let want: BTreeSet<&String> = expect.responses.iter().collect();
            if members.keys().collect::<BTreeSet<_>>() != want
                || result.get("response_order") != Some(&json!(expect.responses))
            {
                return Err(verr("managed exact sensitivity artifact response drifted"));
            }
            for response in expect.responses {
                let member = &members[response];
                if member.get("design_artifact") != Some(&Value::Object(top_design.clone())) {
                    return Err(verr("managed exact batch design artifact drifted"));
                }
                design_occurrences.push(top_design.clone());
                let Some(descriptor) = member.get("artifact").and_then(Value::as_object).cloned() else {
                    return Err(verr("managed exact sensitivity artifact is missing"));
                };
                result_occurrences.push((
                    descriptor,
                    Some(response.clone()),
                    member.get("fields").cloned().unwrap_or(Value::Null),
                    member.get("field_registration").cloned().unwrap_or(Value::Null),
                    member.get("gradient_coordinates").cloned().unwrap_or(Value::Null),
                ));
            }
        } else {
            let descriptor = result.get("artifact").filter(|v| !v.is_null());
            let response = (result.get("schema").and_then(Value::as_str)
                == Some("implexity-provider-sensitivity/2"))
            .then(|| result.get("response").map(py_str))
            .flatten();
            if let Some(r) = &response
                && !expect.responses.contains(r)
            {
                return Err(verr("managed exact sensitivity artifact response drifted"));
            }
            if response.is_some() && !descriptor.is_some_and(Value::is_object) {
                return Err(verr("managed exact sensitivity artifact is missing"));
            }
            if let Some(d) = descriptor {
                let Some(d) = d.as_object() else {
                    return Err(verr("managed exact artifact descriptor drifted"));
                };
                result_occurrences.push((
                    d.clone(),
                    response,
                    result.get("fields").cloned().unwrap_or(Value::Null),
                    result.get("field_registration").cloned().unwrap_or(Value::Null),
                    result.get("gradient_coordinates").cloned().unwrap_or(Value::Null),
                ));
            }
        }
        let canonical_row =
            |(k, v): &(String, Map<String, Value>)| (k.clone(), canonical_text(&Value::Object(v.clone())));
        let mut expected_rows: Vec<(String, String)> = design_occurrences
            .iter()
            .map(|v| canonical_row(&("design_artifact".to_string(), v.clone())))
            .chain(result_occurrences.iter().map(|r| canonical_row(&("artifact".to_string(), r.0.clone()))))
            .collect();
        let mut actual_rows: Vec<(String, String)> =
            managed_artifact_rows(&Value::Object(result.clone())).iter().map(canonical_row).collect();
        expected_rows.sort();
        actual_rows.sort();
        if expected_rows != actual_rows {
            return Err(verr("managed exact artifact descriptor set drifted"));
        }
        let design_id = top_design.get("artifact_id").and_then(Value::as_str).map(str::to_string);
        let Some(design_id) =
            design_id.filter(|_| top_design.get("schema").and_then(Value::as_str) == Some(ARTIFACT_SCHEMA))
        else {
            return Err(verr("managed exact design artifact descriptor drifted"));
        };
        let design_store = ResultArtifactStore::new(&artifact_root.join("design_states"))?;
        let (design_manifest, design_fields) = design_store.inspect_bounded(&design_id, &limits)?;
        admit(&design_manifest, &design_fields)?;
        if design_manifest != top_design {
            return Err(verr("managed exact design artifact manifest drifted"));
        }
        if design_manifest.get("identities")
            != Some(
                &json!({"design_state_id": expect.design_state_id, "model_content_id": expect.model_content_id}),
            )
        {
            return Err(verr("managed exact design artifact identity drifted"));
        }
        let design_metadata = design_manifest.get("metadata").and_then(Value::as_object);
        let mapping = design_metadata
            .and_then(|m| m.get("coordinates"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let slots: BTreeSet<String> = mapping.values().map(py_str).collect();
        let ok = design_metadata.is_some_and(|m| m.len() == 1)
            && !mapping.is_empty()
            && mapping.iter().all(|(k, v)| !k.is_empty() && v.as_str().is_some_and(|s| !s.is_empty()))
            && slots.len() == mapping.len()
            && slots == design_fields.keys().cloned().collect::<BTreeSet<_>>();
        if !ok {
            return Err(verr("managed exact design artifact coordinate map drifted"));
        }
        let coordinates = coordinates.unwrap_or_else(|| {
            let mut c: Vec<String> = mapping.keys().cloned().collect();
            c.sort();
            c
        });
        if mapping.keys().collect::<BTreeSet<_>>() != coordinates.iter().collect::<BTreeSet<_>>() {
            return Err(verr("managed exact design artifact coordinate set drifted"));
        }
        if let Some(values) = &options.expected_design_values
            && values.keys().collect::<BTreeSet<_>>() != coordinates.iter().collect::<BTreeSet<_>>()
        {
            return Err(verr("managed exact frozen design set drifted"));
        }
        let by_slot: BTreeMap<String, String> = mapping.iter().map(|(k, v)| (py_str(v), k.clone())).collect();
        let mut design_rows: BTreeMap<String, Value> = BTreeMap::new();
        let mut design_shapes: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (slot, array) in design_store.read_arrays_bounded(&design_id, None, &limits)? {
            let name = by_slot.get(&slot).cloned().unwrap_or_default();
            let sha = canonical_f8_sha256(&array)?;
            design_rows.insert(
                name.clone(),
                json!({"coordinate": name, "shape": array.shape, "dtype": "<f8", "sha256": sha}),
            );
            design_shapes.insert(name.clone(), array.shape.clone());
            if let Some(values) = &options.expected_design_values {
                let expected = values.get(&name).cloned().unwrap_or_default();
                if expected.shape() != array.shape.as_slice()
                    || canonical_f8_sha256(&NpyArray::from_f64(&expected))? != sha
                {
                    return Err(verr("managed exact design artifact payload drifted"));
                }
            }
        }
        let identity_rows: Vec<Value> =
            coordinates.iter().map(|n| design_rows.get(n).cloned().unwrap_or(Value::Null)).collect();
        let reconstructed = format!(
            "design-{}",
            sha256_hex(
                implexity_core::json::dumps(
                    &Value::Array(identity_rows),
                    &implexity_core::json::DumpOptions::canonical()
                )
                .as_bytes()
            )
        );
        if reconstructed != expect.design_state_id {
            return Err(verr("managed exact design artifact payload identity drifted"));
        }
        let mut declared_directories: BTreeSet<String> =
            [format!("design_states/{design_id}")].into_iter().collect();
        for (descriptor, response, field_metadata, registration, gradients) in &result_occurrences {
            let artifact_id = descriptor.get("artifact_id").and_then(Value::as_str).map(str::to_string);
            let Some(artifact_id) = artifact_id
                .filter(|_| descriptor.get("schema").and_then(Value::as_str) == Some(ARTIFACT_SCHEMA))
            else {
                return Err(verr("managed exact artifact descriptor drifted"));
            };
            let store = ResultArtifactStore::new(artifact_root)?;
            let (manifest, inspected) = store.inspect_bounded(&artifact_id, &limits)?;
            admit(&manifest, &inspected)?;
            if manifest != *descriptor {
                return Err(verr("managed exact artifact manifest drifted"));
            }
            let mut identities = Map::new();
            identities.insert("provider".into(), json!(expect.provider));
            identities.insert("model_content_id".into(), json!(expect.model_content_id));
            identities.insert("solve_id".into(), json!(expect.solve_id));
            identities.insert("design_state_id".into(), json!(expect.design_state_id));
            if let Some(r) = response {
                identities.insert("response".into(), json!(r));
            }
            if manifest.get("identities") != Some(&Value::Object(identities)) {
                return Err(verr(if response.is_some() {
                    "managed sensitivity artifact response drifted"
                } else {
                    "managed exact artifact identity drifted"
                }));
            }
            let metadata = manifest.get("metadata").and_then(Value::as_object);
            let field_meta = field_metadata.as_object();
            let ok = metadata.is_some_and(|m| {
                m.get("design_artifact_id") == Some(&json!(design_id))
                    && m.len() == 3
                    && m.contains_key("field_registration")
                    && m.contains_key("fields")
                    && m.get("fields") == Some(field_metadata)
                    && m.get("field_registration") == Some(registration)
            }) && field_meta.is_some_and(|f| {
                f.keys().collect::<BTreeSet<_>>() == inspected.keys().collect::<BTreeSet<_>>()
            });
            if !ok {
                return Err(verr(
                    "managed exact result artifact design artifact link or field metadata drifted",
                ));
            }
            let field_meta = field_meta.cloned().unwrap_or_default();
            for (name, details) in &inspected {
                let declared = &field_meta[name];
                if !declared.is_object() || declared.get("shape") != Some(&json!(details.shape)) {
                    return Err(verr("managed exact result field metadata drifted"));
                }
            }
            let mut gradient_fields: BTreeMap<String, String> = BTreeMap::new();
            let mut component_fields: BTreeMap<String, (String, usize)> = BTreeMap::new();
            if response.is_some() {
                let Some(gradients) = gradients.as_object().filter(|g| {
                    g.keys().collect::<BTreeSet<_>>() == coordinates.iter().collect::<BTreeSet<_>>()
                }) else {
                    return Err(verr("managed exact gradient coordinate set drifted"));
                };
                for coordinate in &coordinates {
                    let summary = &gradients[coordinate];
                    let allowed: BTreeSet<&str> =
                        ["shape", "l2", "max_abs", "field", "component_fields"].into_iter().collect();
                    let ok = summary.as_object().is_some_and(|s| {
                        ["shape", "l2", "max_abs", "field"].iter().all(|k| s.contains_key(*k))
                            && s.keys().all(|k| allowed.contains(k.as_str()))
                            && s.get("field").is_some_and(Value::is_string)
                            && !gradient_fields.contains_key(s["field"].as_str().unwrap_or(""))
                            && s.get("shape")
                                == Some(&json!(design_shapes.get(coordinate).cloned().unwrap_or_default()))
                    });
                    if !ok {
                        return Err(verr("managed exact gradient summary drifted"));
                    }
                    let field_name = py_str(&summary["field"]);
                    gradient_fields.insert(field_name.clone(), coordinate.clone());
                    if !inspected.contains_key(&field_name) {
                        return Err(verr("managed exact gradient field is missing"));
                    }
                    if let Some(components) = summary.get("component_fields") {
                        let names: Vec<String> =
                            components.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default();
                        let views =
                            implexity_solve::gradient_fields::validate_registered_gradient_components(
                                &field_name,
                                field_meta[&field_name].as_object().unwrap_or(&Map::new()),
                                &names,
                                &field_meta,
                                design_shapes.get(coordinate).map_or(&[][..], Vec::as_slice),
                            )?;
                        for (name, index) in views {
                            if component_fields.contains_key(&name) || gradient_fields.contains_key(&name) {
                                return Err(verr("managed component-gradient view has duplicate ownership"));
                            }
                            component_fields.insert(name, (coordinate.clone(), index));
                        }
                    }
                }
                let g: BTreeSet<&String> = gradient_fields.keys().collect();
                let c: BTreeSet<&String> = component_fields.keys().collect();
                let all: BTreeSet<&String> = g.union(&c).copied().collect();
                if !g.is_disjoint(&c) || all != inspected.keys().collect::<BTreeSet<_>>() {
                    return Err(verr("managed exact gradient field set drifted"));
                }
            }
            let mut seen = BTreeSet::new();
            let mut component_hashes: BTreeMap<String, String> = BTreeMap::new();
            let mut actual_component_hashes: BTreeMap<String, String> = BTreeMap::new();
            for (field_name, array) in store.read_arrays_bounded(&artifact_id, None, &limits)? {
                seen.insert(field_name.clone());
                if response.is_none() {
                    continue;
                }
                let complex = matches!(array.data, NpyData::C64(_) | NpyData::C128(_));
                if let Some((coordinate, _)) = component_fields.get(&field_name) {
                    let shape = design_shapes.get(coordinate).cloned().unwrap_or_default();
                    if complex || array.shape.as_slice() != shape.get(1..).unwrap_or(&[]) {
                        return Err(verr("managed component-gradient payload shape drifted"));
                    }
                    actual_component_hashes.insert(field_name.clone(), canonical_f8_sha256(&array)?);
                    continue;
                }
                let coordinate = gradient_fields.get(&field_name).cloned().unwrap_or_default();
                let summary = &gradients[&coordinate];
                let field = &field_meta[&field_name];
                let values = array.to_f64();
                let ok = !complex
                    && Some(&array.shape) == design_shapes.get(&coordinate)
                    && field.get("response") == Some(&json!(response))
                    && field.get("parameter") == Some(&json!(coordinate))
                    && values.as_ref().is_some_and(|v| {
                        let l2 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
                        let max_abs = v.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
                        #[allow(clippy::float_cmp)]
                        let same = finite_scalar(summary.get("l2"), "managed exact gradient l2").ok()
                            == Some(l2)
                            && finite_scalar(summary.get("max_abs"), "managed exact gradient max_abs").ok()
                                == Some(max_abs);
                        same
                    });
                if !ok {
                    return Err(verr("managed exact gradient payload drifted"));
                }
                if let (Some(names), Some(v)) =
                    (summary.get("component_fields").and_then(Value::as_array), values)
                {
                    for name in names.iter().map(py_str) {
                        let index = component_fields.get(&name).map_or(0, |c| c.1);
                        let slice = v.index_axis(ndarray::Axis(0), index).to_owned();
                        component_hashes.insert(name, canonical_f8_sha256(&NpyArray::from_f64(&slice))?);
                    }
                }
            }
            if component_hashes != actual_component_hashes {
                return Err(verr("managed component-gradient views differ from the full tensor"));
            }
            if seen != inspected.keys().cloned().collect::<BTreeSet<_>>() {
                return Err(verr("managed exact result field set drifted"));
            }
            declared_directories.insert(artifact_id);
        }
        let mut allowed: BTreeSet<String> =
            [".".to_string(), "design_states".to_string()].into_iter().collect();
        for directory in &declared_directories {
            allowed.insert(directory.clone());
            allowed.insert(format!("{directory}/manifest.json"));
            allowed.insert(format!("{directory}/fields.npz"));
        }
        let final_tree = scan_tree_bounded(artifact_root, 16_384, 32)?;
        if final_tree != initial_tree {
            return Err(verr("managed exact artifact directory changed during validation"));
        }
        if final_tree != allowed {
            return Err(verr("managed exact artifact directory contains undeclared output"));
        }
        Ok(())
    }


    pub(crate) fn materialize_managed_provider_artifacts(
        &self,
        result: &Map<String, Value>,
        source_root: &Path,
        destination_root: &Path,
    ) -> JobResult<()> {
        let mut plans = Vec::new();
        let mut seen = BTreeSet::new();
        for (kind, descriptor) in managed_artifact_rows(&Value::Object(result.clone())) {
            let artifact_id = descriptor.get("artifact_id").map(py_str).unwrap_or_default();
            if !seen.insert((kind.clone(), artifact_id.clone())) {
                continue;
            }
            let (source, destination) = if kind == "design_artifact" {
                (source_root.join("design_states"), destination_root.join("design_states"))
            } else {
                (source_root.to_path_buf(), destination_root.to_path_buf())
            };
            let source_store = ResultArtifactStore::new(&source)?;
            if source_store.get(&artifact_id)? != descriptor {
                return Err(verr("managed exact artifact changed before public materialization"));
            }
            plans.push((descriptor, artifact_id, source_store, destination));
        }
        for (descriptor, artifact_id, source_store, destination) in plans {
            let published =
                ResultArtifactStore::new(&destination)?.import_verified(&source_store, &artifact_id)?;
            for key in
                ["schema", "artifact_id", "identities", "fields", "metadata", "payload_sha256", "payload"]
            {
                if published.get(key) != descriptor.get(key) {
                    return Err(verr("managed exact artifact public materialization drifted"));
                }
            }
        }
        Ok(())
    }


    #[allow(clippy::too_many_lines)]
    pub fn prepare_managed_evaluation(
        &self,
        operation_kind: &str,
        req: &Value,
    ) -> JobResult<Arc<ManagedProviderEvaluationCapsule>> {
        if !matches!(operation_kind, "preflight" | "evaluate" | "sensitivity") {
            return opt1("managed exact operation is unsupported");
        }
        let Some(req) = req.as_object() else {
            return opt1("managed exact request must be an object");
        };
        validate_managed_transport(operation_kind, req)?;
        let transport: &[&str] = match operation_kind {
            "preflight" => &["exact_state_handoff"],
            "evaluate" => &[
                "fields",
                "include_values",
                "max_inline_values",
                "stream",
                "stream_fields",
                "previous_field_ids",
                "tile_shape",
                "max_preview_voxels",
                "exact_state_handoff",
            ],
            _ => &[
                "stream",
                "stream_fields",
                "stream_response",
                "response",
                "sensitivity_responses",
                "previous_field_ids",
                "tile_shape",
                "max_preview_voxels",
                "exact_state_handoff",
            ],
        };
        let declaration: Map<String, Value> = req
            .iter()
            .filter(|(k, _)| !transport.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let _live = self.inner.models.live_lock();
        let Declaration { meta, .. } = self.declare(&Value::Object(declaration))?;
        if meta.get("provider_execution").as_str() != Some("array") {
            return opt1("managed exact operations require a registered array provider");
        }
        let (timeout, memory) =
            managed_exact_parent_resources(operation_kind, Some(meta.get("computation_effort")))?;
        let raw_handoff = req.get("exact_state_handoff");
        if raw_handoff.and_then(|h| h.get("produce")) == Some(&Value::Bool(true)) {
            return opt1(
                "managed exact evaluation cannot publish a reusable matching-time state before authoritative terminal commit",
            );
        }
        let handoff = self.matching_time_handoff(raw_handoff, false)?;
        if handoff.as_ref().is_some_and(|h| h.get("produce").is_some_and(truthy)) {
            return opt1("managed exact evaluation cannot produce reusable state");
        }
        let mut worker_response: Option<String> = None;
        let mut worker_responses: Option<Vec<String>> = None;
        if operation_kind == "sensitivity" {
            let declared: Vec<String> = meta
                .get("provider_responses")
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|r| {
                            r.get("name")
                                .filter(|v| truthy(v))
                                .or_else(|| r.get("response").filter(|v| truthy(v)))
                                .map(py_str)
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let unknown: Vec<String> = if let Some(batch) =
                req.get("sensitivity_responses").filter(|v| !v.is_null())
            {
                let ok = batch.as_array().is_some_and(|a| {
                    !a.is_empty()
                        && a.iter().all(|n| n.as_str().is_some_and(|s| !s.trim().is_empty()))
                        && a.iter().map(py_str).collect::<BTreeSet<_>>().len() == a.len()
                }) && req.get("response").is_none_or(Value::is_null)
                    && req.get("stream_response").is_none_or(Value::is_null);
                if !ok {
                    return opt1("managed sensitivity batch contract is invalid");
                }
                let names: Vec<String> =
                    batch.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default();
                let unknown = names.iter().filter(|n| !declared.contains(n)).cloned().collect();
                worker_responses = Some(names);
                unknown
            } else {
                let response = req
                    .get("stream_response")
                    .filter(|v| truthy(v))
                    .or_else(|| req.get("response").filter(|v| truthy(v)))
                    .map(py_str)
                    .or_else(|| {
                        meta.get("provider_responses")
                            .as_array()
                            .and_then(|r| r.first())
                            .and_then(|r| r.get("name"))
                            .filter(|v| truthy(v))
                            .map(py_str)
                    });
                let Some(response) = response else {
                    return opt1("provider sensitivity needs a response name");
                };
                let unknown = if declared.contains(&response) { Vec::new() } else { vec![response.clone()] };
                worker_response = Some(response);
                unknown
            };
            if !unknown.is_empty() {
                return opt1(format!(
                    "provider sensitivity responses are not declared by the analysis: {}",
                    implexity_core::pyobj::list_repr(&unknown)
                ));
            }
        }
        let design = self.provider_current_design(&meta)?;
        let named =
            implexity_optim::NamedArrays::from_pairs(design.iter().map(|(k, v)| (k.clone(), v.clone())));
        let design_state_id = implexity_optim::design_identity(&named)?;
        let current = self.inner.models.require()?;
        let document_sha256 = sha256_hex(&implexity_geometry::document::canonical_bytes(&current.to_doc()?));
        if document_sha256 != meta.s("before_sha256") {
            return opt1("authoritative model changed during managed preparation");
        }
        let managed_inputs = self.inner.dir.join("managed_inputs");
        std::fs::create_dir_all(&managed_inputs)?;
        if std::fs::symlink_metadata(&managed_inputs)?.file_type().is_symlink() {
            return opt1("managed input root is unsafe");
        }
        implexity_io::fsguard::set_owner_only(&managed_inputs, true)?;
        let input_directory = managed_inputs.join(crate::private::token_hex(16)?);
        std::fs::create_dir(&input_directory)?;
        implexity_io::fsguard::set_owner_only(&input_directory, true)?;
        let design_file = input_directory.join("design.npz");
        let request_file = input_directory.join("request.json");
        crate::hierarchical_job::write_design(&design_file, &named, &meta.s("solve_id"))?;
        implexity_io::fsguard::set_owner_only(&design_file, false)?;
        let mut worker_request = Map::new();
        worker_request
            .insert("runtime_packages".into(), json!(implexity_core::packages::global().selected()));
        worker_request.insert("provider".into(), meta.get("physics_provider").clone());
        worker_request.insert("problem".into(), meta.get("provider_problem").clone());
        worker_request.insert("design_file".into(), json!(design_file.to_string_lossy()));
        worker_request.insert("design_coordinates".into(), json!(named.names()));
        worker_request.insert("design_state_id".into(), json!(design_state_id));
        worker_request.insert("artifact_root".into(), Value::Null);
        worker_request.insert("solve_id".into(), meta.get("solve_id").clone());
        worker_request.insert("model_content_id".into(), meta.get("content_id").clone());
        worker_request.insert("physics_snapshot".into(), meta.get("physics_snapshot").clone());
        worker_request.insert("computation_effort".into(), meta.get("computation_effort").clone());
        if let Some(r) = &worker_response {
            worker_request.insert("response".into(), json!(r));
        }
        if let Some(r) = &worker_responses {
            worker_request.insert("responses".into(), json!(r));
        }
        if let Some(h) = &handoff {
            worker_request.insert("matching_time_guess".into(), h.clone());
        }
        let mut encoded = canonical_text(&Value::Object(worker_request.clone())).into_bytes();
        encoded.push(b'\n');
        let mut file = crate::private::create_exclusive(&request_file, 0o600)?;
        crate::private::write_all_sync(&mut file, &encoded)?;
        drop(file);
        let request_sha256 = sha256_hex(&encoded);

        let _qualification_active =
            implexity_runtime::qualification_benchmark_authority::parent_qualification_active();
        let _production =
            implexity_runtime::exact_acceleration_production_authority::production_admission_inputs_present();
        let artifact_output = operation_kind != "preflight" && req.get("stream").is_none_or(truthy);
        Ok(Arc::new(ManagedProviderEvaluationCapsule {
            operation_kind: operation_kind.to_string(),
            owner: self.clone(),
            mode: operation_kind.to_string(),
            request: worker_request,
            request_file,
            request_sha256,
            frozen_content_id: meta.s("content_id"),
            frozen_design_state_id: design_state_id,
            frozen_design_values: Some(design),
            meta,
            public_request: req.clone(),
            artifact_output,
            timeout,
            memory_limit: Some(memory),
            state: Mutex::new(CapsuleState::default()),
            lease_released: Event::new(),
        }))
    }


    pub(crate) fn finalize_managed_provider_evaluation(
        &self,
        capsule: &ManagedProviderEvaluationCapsule,
        committed_directory: &Path,
    ) -> JobResult<Value> {
        let started = now_ns();
        let result = capsule.read_terminal_result(committed_directory)?;
        self.validate_managed_provider_result(capsule, &result)?;
        let artifact_source = committed_directory.join("artifacts");
        let _live = self.inner.models.live_lock();
        let model = self.inner.models.require()?;
        let document_sha256 = sha256_hex(&implexity_geometry::document::canonical_bytes(&model.to_doc()?));
        if document_sha256 != capsule.meta.s("before_sha256")
            || model.node(&capsule.meta.s("node"))?.content_id() != capsule.frozen_content_id
        {
            return opt1("authoritative model changed before managed exact commit");
        }
        let current = self.provider_current_design(&capsule.meta)?;
        let named = implexity_optim::NamedArrays::from_pairs(current);
        if implexity_optim::design_identity(&named)? != capsule.frozen_design_state_id {
            return opt1("authoritative design changed before managed exact commit");
        }
        if capsule.artifact_output {
            self.validate_managed_provider_artifacts(
                &result,
                &artifact_source,
                &ArtifactExpectation {
                    design_state_id: &capsule.frozen_design_state_id,
                    model_content_id: &capsule.frozen_content_id,
                    provider: &capsule.meta.s("physics_provider"),
                    solve_id: &capsule.meta.s("solve_id"),
                    responses: &expected_responses(&capsule.request),
                },
                &capsule.artifact_validation_options(),
            )?;
        }
        let mut report = self.enrich_provider_worker_report(&result, &capsule.meta, None)?;
        if capsule.mode == "preflight" {
            report.insert("start".into(), json!("POST /v1/implicit/optimize with the same body"));
            report.insert(
                "driver".into(),
                json!("managed mature implicit runtime + modular CAE provider preflight"),
            );
            report.insert("physics_provider".into(), capsule.meta.get("physics_provider").clone());
            report.insert("topology_coordinate".into(), json!("model:control"));
            report.insert(
                "computation_effort".into(),
                Value::Object(implexity_runtime::provider_job_authority::public_effort_view(
                    capsule.meta.get("computation_effort"),
                )?),
            );
            let boundary = now_ns();
            attach_managed_transport_timing(&mut report, started, boundary, boundary)?;
            return Ok(Value::Object(report));
        }
        let public_started = now_ns();
        let destination = self.inner.dir.join(if capsule.mode == "sensitivity" {
            "sensitivity_artifacts_v24_provider"
        } else {
            "result_artifacts_v24_provider"
        });
        if capsule.artifact_output {
            self.materialize_managed_provider_artifacts(&result, &artifact_source, &destination)?;
        }
        if capsule.mode == "evaluate" {
            if capsule.artifact_output {
                self.publish_provider_results(
                    &mut report,
                    &capsule.meta,
                    &capsule.public_request,
                    &destination,
                )?;
            }
        } else if let Some(names) = capsule.request.get("responses").filter(|v| !v.is_null()) {
            let names: Vec<String> =
                names.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default();
            if capsule.artifact_output {
                self.publish_provider_sensitivity_batch(
                    &mut report,
                    &capsule.meta,
                    &capsule.public_request,
                    &names,
                    &destination,
                )?;
            }
        } else if capsule.artifact_output {
            let response = capsule.request.get("response").map(py_str).unwrap_or_default();
            self.publish_provider_sensitivity(
                &mut report,
                &capsule.meta,
                &capsule.public_request,
                &response,
                &destination,
            )?;
        }
        let finished = now_ns();
        attach_managed_transport_timing(&mut report, started, public_started, finished)?;
        Ok(Value::Object(report))
    }

    pub(crate) fn heavy_busy_suffix(&self) -> String {
        let state = self.state();
        state
            .active
            .as_ref()
            .and_then(|id| state.jobs.get(id))
            .filter(|j| !super::is_terminal(&j.status()))
            .map(|j| format!(" (optimisation job {}, {})", j.id(), j.status()))
            .unwrap_or_default()
    }


    #[allow(clippy::too_many_lines)]
    pub(crate) fn run_provider_worker(
        &self,
        meta: &JobMeta,
        mode: &str,
        response: Option<&str>,
        responses: Option<&[String]>,
        artifact_root: Option<&Path>,
        matching_time_guess: Option<&Value>,
    ) -> JobResult<Map<String, Value>> {
        let lease = self.inner.host.eval_lock()?;
        if !lease.acquire(false, None).map_err(|e| JobError::runtime(e.0))? {
            return opt1(format!(
                "a heavy process is already running{}; provider state/adjoint evaluation admits one heavy process at a time",
                self.heavy_busy_suffix()
            ));
        }
        let prepared = (|| -> JobResult<PathBuf> {
            let tmp = self.inner.dir.join(format!("provider_{mode}"));
            std::fs::create_dir_all(&tmp)?;
            let design_file = tmp.join("design.npz");
            let request_file = tmp.join("request.json");
            let design = self.provider_current_design(meta)?;
            let named = implexity_optim::NamedArrays::from_pairs(design);
            crate::hierarchical_job::write_design(&design_file, &named, &meta.s("solve_id"))?;
            let mut request = Map::new();
            request.insert("runtime_packages".into(), json!(implexity_core::packages::global().selected()));
            request.insert("provider".into(), meta.get("physics_provider").clone());
            request.insert("problem".into(), meta.get("provider_problem").clone());
            request.insert("design_file".into(), json!(design_file.to_string_lossy()));
            request.insert("design_coordinates".into(), json!(named.names()));
            request.insert("design_state_id".into(), json!(implexity_optim::design_identity(&named)?));
            request.insert(
                "artifact_root".into(),
                artifact_root.map_or(Value::Null, |p| json!(p.to_string_lossy())),
            );
            request.insert("solve_id".into(), meta.get("solve_id").clone());
            request.insert("model_content_id".into(), meta.get("content_id").clone());
            request.insert("physics_snapshot".into(), meta.get("physics_snapshot").clone());
            request.insert("computation_effort".into(), meta.get("computation_effort").clone());
            if response.is_some() && responses.is_some() {
                return opt1("provider sensitivity accepts response or responses, not both");
            }
            if let Some(r) = response {
                request.insert("response".into(), json!(r));
            }
            if let Some(r) = responses {
                request.insert("responses".into(), json!(r));
            }
            if let Some(g) = matching_time_guess {
                request.insert("matching_time_guess".into(), g.clone());
            }
            std::fs::write(
                &request_file,
                implexity_core::json::dumps(
                    &Value::Object(request),
                    &implexity_core::json::DumpOptions::default(),
                ),
            )?;
            Ok(request_file)
        })();
        let request_file = match prepared {
            Ok(f) => f,
            Err(e) => {
                let _ = lease.release();
                return Err(e);
            }
        };
        let started = std::time::Instant::now();
        let mut timeout: f64 = std::env::var("IMPLEXITY_IMPLICIT_SENSITIVITY_TIMEOUT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(1200.0);
        if let Some(binding) = Some(meta.get("computation_effort")).filter(|v| !v.is_null()) {
            let (selected, _b) =
                implexity_runtime::provider_job_authority::validate_effort_binding(binding, None)?;
            if let Some(w) = selected.effective.wall_time_budget_s {
                timeout = timeout.min(w);
            }
        }
        let mut command = self.inner.host.worker_command();
        command.extend([
            crate::worker_cli::PROVIDER.to_string(),
            "--mode".into(),
            mode.to_string(),
            "--request-file".into(),
            request_file.to_string_lossy().into_owned(),
        ]);
        let out = crate::worker_cli::run_captured(
            &command,
            &self.inner.host.worker_cwd(),
            &child_env()?,
            crate::worker_cli::duration_from_secs(timeout),
        );
        let _ = lease.release();
        let Some(out) = out? else {
            return opt1(format!(
                "the provider {mode} child exceeded its {} second hard wall-time limit before publishing exact computation evidence",
                implexity_optim::numeric::format_g(timeout, 6)
            ));
        };
        let prefix = match mode {
            "preflight" => "PREFLIGHT ",
            "evaluate" => "RESULTS ",
            "sensitivity" => "SENSITIVITY ",
            other => return opt1(format!("unknown isolated provider worker operation {}", repr_str(other))),
        };
        let mut rep: Option<Map<String, Value>> = None;
        let mut err: Option<Value> = None;
        for line in out.stdout.lines() {
            if let Some(body) = line.strip_prefix(prefix) {
                rep = Some(
                    serde_json::from_str(body).map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?,
                );
            } else if let Some(body) = line.strip_prefix("ERROR ") {
                err = Some(serde_json::from_str(body).unwrap_or_else(|_| json!({"problems": [body]})));
            }
        }
        let Some(rep) = rep else {
            let mut problems: Vec<String> = err
                .as_ref()
                .and_then(|e| e.get("problems"))
                .and_then(Value::as_array)
                .filter(|p| !p.is_empty())
                .map_or_else(
                    || {
                        vec![format!(
                            "the provider {mode} child exited {} and printed no {} line",
                            out.returncode,
                            prefix.trim()
                        )]
                    },
                    |p| p.iter().map(py_str).collect(),
                );
            let tail: String =
                out.stderr.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
            if !tail.is_empty() && err.is_none() {
                problems.push(format!("stderr tail: {tail}"));
            }
            if let Some(report) = err.as_ref().and_then(|e| e.get("solver_recovery")) {
                let report = crate::solver_recovery::validate(report)?;
                let message = report.get("message").and_then(Value::as_str).unwrap_or("Solver needs attention").to_string();
                return Err(JobError::Recovery { cause: Box::new(JobError::optimize(problems)), report, message });
            }
            return Err(JobError::optimize(problems));
        };
        self.enrich_provider_worker_report(&rep, meta, Some(started.elapsed().as_secs_f64()))
    }


    pub(crate) fn matching_time_handoff(
        &self,
        raw: Option<&Value>,
        producer_requires_accepted: bool,
    ) -> JobResult<Option<Value>> {
        let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(None) };
        let Some(r) = raw
            .as_object()
            .filter(|r| r.keys().all(|k| matches!(k.as_str(), "schema" | "consume" | "produce")))
        else {
            return opt1("exact_state_handoff must contain only schema, consume, and produce");
        };
        if let Some(schema) = r.get("schema").filter(|v| !v.is_null())
            && schema.as_str() != Some("implexity-exact-state-handoff/1")
        {
            return opt1("unsupported exact_state_handoff schema");
        }
        let root = self.inner.dir.join("matching_time_newton_guesses");
        let mut internal = Map::new();
        if let Some(consume) = r.get("consume").filter(|v| !v.is_null()) {
            let Some(c) =
                consume.as_object().filter(|c| c.keys().all(|k| k == "capsule_id" || k == "required"))
            else {
                return opt1("exact_state_handoff.consume requires capsule_id and optional required");
            };
            let required = c.get("required").cloned().unwrap_or(Value::Bool(true));
            let (Some(required), Some(capsule_id)) =
                (required.as_bool(), c.get("capsule_id").and_then(Value::as_str))
            else {
                return opt1("exact_state_handoff consume capsule_id/required are invalid");
            };
            let store = implexity_solve::matching_time_guess::MatchingTimeGuessStore::new(&root)
                .map_err(JobError::from)?;
            match store.inspect(capsule_id) {
                Ok(_) => {
                    internal.insert(
                        "consume".into(),
                        json!({"capsule_id": capsule_id, "store_root": root.to_string_lossy(), "required": required}),
                    );
                }
                Err(implexity_solve::matching_time_guess::GuessError::Missing(m)) => {
                    if required {
                        return opt1(format!("required matching-time Newton guess: {m}"));
                    }
                }
                Err(implexity_solve::matching_time_guess::GuessError::Invalid(m)) => {
                    return opt1(format!("matching-time Newton guess is corrupt or incompatible: {m}"));
                }
            }
        }
        let produce = r.get("produce").cloned().unwrap_or(Value::Bool(false));
        let Some(produce) = produce.as_bool() else {
            return opt1("exact_state_handoff.produce must be boolean");
        };
        if produce {
            internal.insert(
                "produce".into(),
                json!({"store_root": root.to_string_lossy(), "require_accepted": producer_requires_accepted}),
            );
        }
        Ok(if internal.is_empty() { None } else { Some(Value::Object(internal)) })
    }


    #[allow(clippy::too_many_lines)]
    pub(crate) fn publish_provider_results(
        &self,
        rep: &mut Map<String, Value>,
        meta: &JobMeta,
        req: &Map<String, Value>,
        artifact_root: &Path,
    ) -> JobResult<()> {
        let Some(artifact) = rep.get("artifact").filter(|v| truthy(v)).and_then(Value::as_object).cloned()
        else {
            return Ok(());
        };
        let store = ResultArtifactStore::new(artifact_root)?;
        let artifact_id = artifact.get("artifact_id").map(py_str).unwrap_or_default();
        let (_manifest, inspected) = store.inspect_bounded(&artifact_id, &Limits::default())?;
        let fields = rep.get("fields").and_then(Value::as_object).cloned().unwrap_or_default();
        rep.insert(
            "field_arrays".into(),
            Value::Object(crate::result_arrays::array_references(&artifact, &fields)),
        );
        let registration = rep
            .get("field_registration")
            .filter(|v| truthy(v))
            .cloned()
            .or_else(|| artifact.get("metadata").and_then(|m| m.get("field_registration")).cloned())
            .unwrap_or(Value::Null);
        let previous = req.get("previous_field_ids").and_then(Value::as_object).cloned().unwrap_or_default();
        let requested: BTreeSet<String> = match req.get("stream_fields").filter(|v| !v.is_null()) {
            None => inspected.keys().cloned().collect(),
            Some(v) => v.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default(),
        };
        let tile = triplet(req.get("tile_shape"));
        let voxels = req.get("max_preview_voxels").and_then(Value::as_i64).unwrap_or(64_000);
        let field_store = self.field_store()?;
        let mut streams = Map::new();
        let mut omitted = Map::new();
        for (ident, array) in store.read_arrays_bounded(&artifact_id, None, &Limits::default())? {
            if !requested.contains(&ident) {
                continue;
            }
            let field_meta = fields.get(&ident).and_then(Value::as_object).cloned().unwrap_or_default();
            let field_registration =
                field_meta.get("registration").cloned().unwrap_or_else(|| registration.clone());
            let reg_shape: Vec<u64> = field_registration
                .get("shape")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            let spatial: Vec<u64> = array.shape.iter().take(3).map(|v| *v as u64).collect();
            if field_registration.is_null() || spatial != reg_shape {
                omitted
                    .insert(ident, json!("the field shape does not match the exact analysis registration"));
                continue;
            }
            let fa = field_array(&array);
            if let Err(e) = implexity_geometry::field_views::prepare_stream_array(&fa) {
                omitted.insert(ident, json!(e.to_string()));
                continue;
            }
            let units = field_meta.get("units").map(py_str).unwrap_or_default();
            if units.is_empty() || units == "unspecified" {
                omitted.insert(ident, json!("provider did not declare physical field units"));
                continue;
            }
            let association = field_meta.get("association").map_or_else(|| "cell".to_string(), py_str);
            let centering = field_registration.get("centering").map_or_else(|| "cell".to_string(), py_str);
            if association != centering {
                omitted.insert(ident, json!("field association disagrees with its explicit registration"));
                continue;
            }
            let rank = field_meta.get("rank").map_or_else(|| "scalar".to_string(), py_str);
            let labels =
                implexity_geometry::field_views::component_labels_from_metadata(Some(&field_meta), Some(&fa));
            let (sshape, sdata) = match implexity_geometry::field_views::prepare_stream_array(&fa) {
                Ok(v) => v,
                Err(e) => {
                    omitted.insert(ident, json!(e.to_string()));
                    continue;
                }
            };
            let stream_fa =
                implexity_geometry::field_views::FieldArray { shape: sshape, data: sdata, complex: false };
            let view_spec = match implexity_geometry::field_views::field_views(
                &stream_fa,
                &rank,
                labels.as_deref(),
                field_meta.get("signed").is_some_and(truthy),
                FIELD_VIEW_SAMPLE_LIMIT,
            ) {
                Ok(v) => v,
                Err(e) => {
                    omitted.insert(ident, json!(e.to_string()));
                    continue;
                }
            };
            let default_id = view_spec.get("default_view").cloned().unwrap_or(Value::Null);
            let default_view = view_spec
                .get("views")
                .and_then(Value::as_array)
                .and_then(|v| v.iter().find(|x| x.get("id") == Some(&default_id)))
                .cloned()
                .unwrap_or(Value::Null);
            let identity = FieldIdentity {
                model_id: rep
                    .get("model")
                    .and_then(|m| m.get("content_id"))
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
                problem_id: meta.s("solve_id"),
                field_name: ident.clone(),
                registration_id: field_registration
                    .get("registration_id")
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
                response_id: ident.clone(),
                optimisation_iteration: None,
            };
            let Some(nd) = to_ndarray(&array, true) else {
                omitted.insert(ident, json!("the field is not a numeric array"));
                continue;
            };
            let publication = field_store.publish_revision(
                &nd,
                &identity,
                &field_registration,
                previous.get(&ident).and_then(Value::as_str),
                Reducer::Mean,
                voxels,
                false,
                tile,
            )?;
            let complex = matches!(array.data, NpyData::C64(_) | NpyData::C128(_));
            let mut extra = Map::new();
            extra.insert("field_name".into(), json!(ident));
            extra.insert("rank".into(), json!(rank));
            extra.insert(
                "value_representation".into(),
                json!(if complex { "interleaved_real_imaginary" } else { "real" }),
            );
            extra.insert("components".into(), view_spec.get("components").cloned().unwrap_or(Value::Null));
            extra.insert("views".into(), view_spec.get("views").cloned().unwrap_or(Value::Null));
            extra.insert("default_view".into(), default_id);
            extra.insert("component".into(), default_view.get("component").cloned().unwrap_or(Value::Null));
            extra.insert("units".into(), field_meta.get("units").cloned().unwrap_or(json!("1")));
            let mut r = stream_ref(&publication, &field_registration, extra);
            if let Value::Object(m) = &mut r {
                m.insert("visual_range".into(), default_view.get("range").cloned().unwrap_or(Value::Null));
            }
            streams.insert(ident, r);
        }
        rep.insert("field_stream_count".into(), json!(streams.len()));
        rep.insert("field_streams".into(), Value::Object(streams));
        if !omitted.is_empty() {
            rep.insert("field_streams_omitted".into(), Value::Object(omitted));
        }

        reorder_after(rep, "field_streams", "field_stream_count");
        Ok(())
    }


    #[allow(clippy::too_many_lines)]
    pub(crate) fn publish_provider_sensitivity(
        &self,
        rep: &mut Map<String, Value>,
        meta: &JobMeta,
        req: &Map<String, Value>,
        response: &str,
        artifact_root: &Path,
    ) -> JobResult<()> {
        let Some(artifact) = rep.get("artifact").filter(|v| truthy(v)).and_then(Value::as_object).cloned()
        else {
            return Ok(());
        };
        let store = ResultArtifactStore::new(artifact_root)?;
        let artifact_id = artifact.get("artifact_id").map(py_str).unwrap_or_default();
        store.inspect_bounded(&artifact_id, &Limits::default())?;
        let fields = rep.get("fields").and_then(Value::as_object).cloned().unwrap_or_default();
        rep.insert(
            "field_arrays".into(),
            Value::Object(crate::result_arrays::array_references(&artifact, &fields)),
        );
        let registration = rep
            .get("field_registration")
            .filter(|v| truthy(v))
            .cloned()
            .or_else(|| artifact.get("metadata").and_then(|m| m.get("field_registration")).cloned())
            .unwrap_or(Value::Null);
        let metadata = artifact
            .get("metadata")
            .and_then(|m| m.get("fields"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let requested: Option<BTreeSet<String>> = req
            .get("stream_fields")
            .filter(|v| !v.is_null())
            .map(|v| v.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default());
        let previous = req.get("previous_field_ids").and_then(Value::as_object).cloned().unwrap_or_default();
        let tile = triplet(req.get("tile_shape"));
        let voxels = req.get("max_preview_voxels").and_then(Value::as_i64).unwrap_or(64_000);
        let field_store = self.field_store()?;
        let mut streams = Map::new();
        for (field_name, array) in store.read_arrays_bounded(&artifact_id, None, &Limits::default())? {
            let field_meta =
                metadata.get(&field_name).and_then(Value::as_object).cloned().unwrap_or_default();
            if requested.as_ref().is_some_and(|r| !r.contains(&field_name)) {
                continue;
            }
            let field_registration =
                if field_meta.get("parameter").and_then(Value::as_str) == Some("model:control") {
                    field_meta.get("registration").cloned().unwrap_or_else(|| registration.clone())
                } else {
                    field_meta.get("registration").cloned().unwrap_or(Value::Null)
                };
            let reg_shape: Vec<u64> = field_registration
                .get("shape")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            let shape: Vec<u64> = array.shape.iter().map(|v| *v as u64).collect();
            if array.shape.len() != 3 || field_registration.is_null() || shape != reg_shape {
                continue;
            }
            let response_id =
                field_meta.get("response").filter(|v| truthy(v)).map_or_else(|| response.to_string(), py_str);
            let identity = FieldIdentity {
                model_id: rep
                    .get("model")
                    .and_then(|m| m.get("content_id"))
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
                problem_id: meta.s("solve_id"),
                field_name: field_name.clone(),
                registration_id: field_registration
                    .get("registration_id")
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
                response_id: response_id.clone(),
                optimisation_iteration: None,
            };
            let Some(nd) = to_ndarray(&array, false) else { continue };
            let publication = field_store.publish_revision(
                &nd,
                &identity,
                &field_registration,
                previous.get(&field_name).and_then(Value::as_str),
                Reducer::MaxAbsSigned,
                voxels,
                true,
                tile,
            )?;
            let view_spec = implexity_geometry::field_views::field_views(
                &field_array(&array),
                "scalar",
                None,
                true,
                FIELD_VIEW_SAMPLE_LIMIT,
            )?;
            let default_view = view_spec
                .get("views")
                .and_then(Value::as_array)
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or(Value::Null);
            let mut extra = Map::new();
            extra.insert("field_name".into(), json!(field_name));
            extra.insert("response_id".into(), json!(response_id));
            extra.insert(
                "parameter".into(),
                field_meta.get("parameter").cloned().unwrap_or(json!("model:control")),
            );
            extra.insert("rank".into(), json!("scalar"));
            extra.insert("components".into(), json!(1));
            extra.insert("units".into(), field_meta.get("units").cloned().unwrap_or(json!("1")));
            extra.insert("signed".into(), json!(true));
            extra.insert("views".into(), view_spec.get("views").cloned().unwrap_or(Value::Null));
            extra
                .insert("default_view".into(), view_spec.get("default_view").cloned().unwrap_or(Value::Null));
            extra.insert("component".into(), default_view.get("component").cloned().unwrap_or(Value::Null));
            let mut r = stream_ref(&publication, &field_registration, extra);
            if let Value::Object(m) = &mut r {
                m.insert("visual_range".into(), default_view.get("range").cloned().unwrap_or(Value::Null));
            }
            streams.insert(field_name, r);
        }
        rep.insert("sensitivity_stream_count".into(), json!(streams.len()));
        rep.insert("sensitivity_streams".into(), Value::Object(streams));
        reorder_after(rep, "sensitivity_streams", "sensitivity_stream_count");
        Ok(())
    }


    pub(crate) fn publish_provider_sensitivity_batch(
        &self,
        rep: &mut Map<String, Value>,
        meta: &JobMeta,
        req: &Map<String, Value>,
        names: &[String],
        artifact_root: &Path,
    ) -> JobResult<()> {
        let members = rep.get("sensitivities").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut presentations = Map::new();
        let mut count = 0_i64;
        for name in names {
            let mut member = members.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
            member.insert("seconds".into(), rep.get("seconds").cloned().unwrap_or(Value::Null));
            for key in ["node", "declared_by", "case_source"] {
                member.insert(key.into(), meta.get(key).clone());
            }
            member.insert("free_plan".into(), meta.get("plan").clone());
            member.insert("warnings".into(), meta.get("warnings").clone());
            member.insert("ignored".into(), meta.get("ignored").clone());
            member.insert("model".into(), json!({"content_id": meta.get("content_id")}));
            self.publish_provider_sensitivity(&mut member, meta, req, name, artifact_root)?;
            count += member.get("sensitivity_stream_count").and_then(Value::as_i64).unwrap_or(0);
            presentations.insert(name.clone(), Value::Object(member));
        }
        rep.insert("sensitivity_presentations".into(), Value::Object(presentations));
        rep.insert("sensitivity_stream_count".into(), json!(count));
        Ok(())
    }

    pub(crate) fn field_store(
        &self,
    ) -> JobResult<Arc<implexity_authoring::incremental_fields::IncrementalProgressiveFieldStore>> {
        Ok(self.inner.authoring.field_store()?)
    }
}

pub(crate) fn reorder_after(map: &mut Map<String, Value>, first: &str, second: &str) {
    if let (Some(a), Some(b)) = (map.shift_remove(first), map.shift_remove(second)) {
        map.insert(first.into(), a);
        map.insert(second.into(), b);
    }
}
