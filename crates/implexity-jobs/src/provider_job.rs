// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use implexity_core::contracts::{
    CaeProvider, LegacySingleArrayOptimizationSettings, ProviderCapabilities, ProviderProblem, ResponseSpec,
};
use implexity_core::{CaeError, CaeResult};
use implexity_io::npy::NpyArray;
use implexity_optim::bounds::{
    BoundMultiplierState, augmented_objective, lookup, normalise_response_normalization,
};
use implexity_optim::candidate_events::CandidateAdmission;
use implexity_optim::coordinate_bounds::{BoxMasks, normalise_mask};
use implexity_optim::optimizer::{
    array_response_data, evaluation_response_values, legacy_settings_value, objective,
};
use implexity_optim::provider_ops::{DesignOp, design_operations, provides};
use implexity_optim::search::{
    BoxCoordinate, Candidate, ExactPoint, ProjectedSearch, SearchHooks, SearchResult, TERMINAL_REASONS,
    TrialValues,
};
use implexity_optim::{NamedArrays, design_identity};
use implexity_runtime::computation_effort::{ComputationEffortPolicy, ComputationMode};
use implexity_runtime::provider_job_authority::{
    ProviderFacts, attach_exact_effort_evidence, make_server_effort_binding,
    private_provider_effort_validation, require_single_provider_schedule, runtime_source_binding,
    stable_package_identity, validate_effort_binding, validate_initial_control_input,
};
use implexity_solve::approximation::ApproximationLane;
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value};

use crate::effort::{physics_snapshot, provider_computation_effort_scope};
use crate::error::{JobError, JobResult};
use crate::preview::{
    Ranked, interactive_correction_plan, preview_policy, preview_ranked_candidates, preview_record,
    preview_wire,
};
use crate::private::{canonical_text, compact_text, perf_counter};
use crate::provider_worker::{emit_line, error_payload};
use crate::solver_telemetry::{
    NumericalEvent, numerical_deviation_scope, raised, validate_numerical_deviation_policy,
};

pub const SCHEMA: &str = "implexity-provider-job/1";
pub const SCALAR_HISTORY_SCHEMA: &str = "implexity-provider-job-history/4";
pub const SCALAR_CHECKPOINT_SCHEMA: &str = "implexity-scalar-provider-checkpoint/4";
pub const SCALAR_RUN_IDENTITY_SCHEMA: &str = "implexity-scalar-provider-run-identity/1";

const SCALAR_HISTORY_REQUIRED_FIELDS: [&str; 32] = [
    "i",
    "iteration",
    "L",
    "objective",
    "L_first",
    "L_best",
    "base_objective",
    "gradient_norm",
    "stationarity",
    "accepted",
    "step_fraction",
    "trust_step",
    "solid_fraction_mean",
    "terms",
    "max_scaled_bound_violation",
    "bound_measure",
    "bound_multipliers",
    "multiplier_updates",
    "trial_evaluations",
    "armijo_rejections",
    "search_state",
    "provider",
    "topology_coordinate",
    "design_state_id",
    "best_design_state_id",
    "run_fingerprint",
    "requested_policy_digest",
    "effective_effort_digest",
    "operation_context_digest",
    "runtime_source_sha256",
    "continuable",
    "push",
];
pub const SCALAR_HISTORY_OPTIONAL_ENGINE_FIELDS: [&str; 8] = [
    "step_policy",
    "reason",
    "numerical_trial_rejections",
    "numerical_rejections",
    "last_numerical_trial_rejection",
    "non_descent_trials",
    "armijo",
    "accepted_value_max_relative_difference",
];
const SCALAR_HISTORY_FINITE_FIELDS: [&str; 11] = [
    "L",
    "objective",
    "L_first",
    "L_best",
    "base_objective",
    "gradient_norm",
    "step_fraction",
    "trust_step",
    "solid_fraction_mean",
    "max_scaled_bound_violation",
    "bound_measure",
];

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn repr(s: &str) -> String {
    implexity_core::py_repr::repr_str(s)
}

#[must_use]
pub fn canonical_payload_text(payload: &Value) -> String {
    canonical_text(payload)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Text(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
}


pub fn strict_checkpoint_scalar(
    archive: &implexity_io::npz::Npz,
    name: &str,
    kind: &str,
) -> CaeResult<Scalar> {
    use implexity_io::npy::NpyData;
    let Some(raw) = archive.get(name) else {
        return contract(format!("resume checkpoint omitted required {}", repr(name)));
    };
    if !raw.shape.is_empty() {
        return contract(format!("resume checkpoint {} must be a zero-dimensional scalar", repr(name)));
    }
    match kind {
        "text" => match raw.as_scalar_str() {
            Some(s) if !s.is_empty() => Ok(Scalar::Text(s.to_string())),
            _ => contract(format!("resume checkpoint {} must be non-empty text", repr(name))),
        },
        "integer" => match &raw.data {
            NpyData::I64(v) => Ok(Scalar::Integer(v[0])),
            NpyData::I32(v) => Ok(Scalar::Integer(i64::from(v[0]))),
            NpyData::I16(v) => Ok(Scalar::Integer(i64::from(v[0]))),
            NpyData::I8(v) => Ok(Scalar::Integer(i64::from(v[0]))),
            NpyData::U8(v) => Ok(Scalar::Integer(i64::from(v[0]))),
            NpyData::U16(v) => Ok(Scalar::Integer(i64::from(v[0]))),
            NpyData::U32(v) => Ok(Scalar::Integer(i64::from(v[0]))),
            NpyData::U64(v) if i64::try_from(v[0]).is_ok() => {
                Ok(Scalar::Integer(i64::try_from(v[0]).unwrap_or(0)))
            }
            _ => contract(format!("resume checkpoint {} must be an exact integer scalar", repr(name))),
        },
        "finite_number" => {
            let value = match &raw.data {
                NpyData::F64(v) => Some(v[0]),
                NpyData::F32(v) => Some(f64::from(v[0])),
                NpyData::I64(v) => Some(v[0] as f64),
                NpyData::I32(v) => Some(f64::from(v[0])),
                _ => None,
            };
            match value {
                Some(v) if v.is_finite() => Ok(Scalar::Number(v)),
                _ => contract(format!("resume checkpoint {} must be a finite numeric scalar", repr(name))),
            }
        }
        "boolean" => match &raw.data {
            NpyData::Bool(v) => Ok(Scalar::Boolean(v[0])),
            _ => contract(format!("resume checkpoint {} must be an exact boolean scalar", repr(name))),
        },
        other => contract(format!("unsupported checkpoint scalar contract {}", repr(other))),
    }
}


pub fn strict_text(archive: &implexity_io::npz::Npz, name: &str) -> CaeResult<String> {
    match strict_checkpoint_scalar(archive, name, "text")? {
        Scalar::Text(s) => Ok(s),
        _ => contract(format!("resume checkpoint {} must be non-empty text", repr(name))),
    }
}


pub fn strict_integer(archive: &implexity_io::npz::Npz, name: &str) -> CaeResult<i64> {
    match strict_checkpoint_scalar(archive, name, "integer")? {
        Scalar::Integer(i) => Ok(i),
        _ => contract(format!("resume checkpoint {} must be an exact integer scalar", repr(name))),
    }
}


pub fn strict_number(archive: &implexity_io::npz::Npz, name: &str) -> CaeResult<f64> {
    match strict_checkpoint_scalar(archive, name, "finite_number")? {
        Scalar::Number(v) => Ok(v),
        _ => contract(format!("resume checkpoint {} must be a finite numeric scalar", repr(name))),
    }
}

#[must_use]
pub fn public_scalar_job_spec(spec: &Map<String, Value>) -> Map<String, Value> {
    let mut public = spec.clone();
    if let Some(Value::Object(transport)) = public.get_mut("matching_time_guess") {
        for operation in ["consume", "produce"] {
            if let Some(Value::Object(row)) = transport.get_mut(operation) {
                row.shift_remove("store_root");
            }
        }
    }
    public
}

fn fingerprint(value: &Value) -> String {
    implexity_core::wire::fingerprint_value(value)
}


#[allow(clippy::too_many_arguments)]
pub fn scalar_run_fingerprint(
    spec: &Map<String, Value>,
    settings: &LegacySingleArrayOptimizationSettings,
    initial_design_state_id: &str,
    provider_name: &str,
    capabilities: &ProviderCapabilities,
    package_snapshot: &Value,
    effort_binding: &Map<String, Value>,
    runtime_source: &Value,
) -> CaeResult<String> {
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(SCALAR_RUN_IDENTITY_SCHEMA.into()));
    m.insert("job".into(), implexity_core::wire::to_wire(&Value::Object(public_scalar_job_spec(spec)))?);
    m.insert("parsed_settings".into(), settings_identity(spec, settings));
    m.insert("initial_design_state_id".into(), Value::String(initial_design_state_id.into()));
    m.insert("provider".into(), Value::String(provider_name.into()));
    m.insert("provider_descriptor".into(), Value::Object(capabilities.to_map()));
    m.insert("package_identity".into(), stable_package_identity(package_snapshot));
    for key in ["requested_policy_digest", "effective_effort_digest", "operation_context_digest"] {
        m.insert(key.into(), effort_binding.get(key).cloned().unwrap_or(Value::Null));
    }
    m.insert("runtime_source_identity".into(), runtime_source.clone());
    Ok(fingerprint(&Value::Object(m)))
}

fn scalar_history_payload(history: &[Value]) -> Value {
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(SCALAR_HISTORY_SCHEMA.into()));
    m.insert("history".into(), Value::Array(history.to_vec()));
    Value::Object(m)
}

fn scalar_history_digest(history: &[Value]) -> String {
    crate::private::sha256_hex(canonical_text(&scalar_history_payload(history)).as_bytes())
}


pub fn require_raw_job_authority(spec: &Value) -> CaeResult<(Map<String, Value>, String)> {
    let Some(checked) = spec.as_object() else {
        return contract("provider job specification must be an object");
    };
    if checked.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return contract(format!("provider job schema must be {}", repr(SCHEMA)));
    }
    let provider = checked
        .get("provider")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .map(|v| implexity_core::pyobj::py_str(v).trim().to_string())
        .unwrap_or_default();
    if provider.is_empty() {
        return contract("provider job requires a provider");
    }
    require_single_provider_schedule(&Value::String(provider.clone()), checked.get("schedule"))?;
    Ok((checked.clone(), provider))
}


pub fn refuse_stale_scalar_outputs(output_dir: &Path) -> CaeResult<()> {
    let fixed = [
        "best.npz",
        "ckpt.npz",
        "history.json",
        "summary.json",
        "best.npz.tmp.npz",
        "ckpt.npz.tmp.npz",
        "history.json.tmp",
        "summary.json.tmp",
        crate::hierarchical_job::RESUME_WARM_START_FILES[0],
        crate::hierarchical_job::RESUME_WARM_START_FILES[1],
        crate::hierarchical_job::RESUME_WARM_START_FILES[2],
        crate::hierarchical_job::RESUME_WARM_START_TEMPORARIES[0],
        crate::hierarchical_job::RESUME_WARM_START_TEMPORARIES[1],
        crate::hierarchical_job::RESUME_WARM_START_TEMPORARIES[2],
    ];
    validate_initial_control_input(output_dir)?;
    let mut stale: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(output_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if fixed.contains(&name.as_str())
                || name == crate::epoch_state::EPOCH_STATE_DIR
                || (name.starts_with("live_") && (name.ends_with(".npz") || name.ends_with(".tmp.npz")))
            {
                stale.push(name);
            }
        }
    }
    if !stale.is_empty() {
        stale.sort();
        return contract(format!(
            "fresh provider job directory contains authoritative scalar artifacts; use a new job directory or request validated resume: {}",
            implexity_core::pyobj::list_repr(&stale)
        ));
    }
    Ok(())
}


pub fn atomic_json(path: &Path, payload: &Value) -> std::io::Result<()> {
    implexity_io::atomic::write_atomic(path, canonical_text(payload).as_bytes()).map_err(|e| std::io::Error::other(e.to_string()))
}


pub fn atomic_npz(path: &Path, arrays: &[(&str, NpyArray)]) -> std::io::Result<()> {
    let members: Vec<(&str, &NpyArray)> = arrays.iter().map(|(k, v)| (*k, v)).collect();
    let bytes = implexity_io::npz::save(&members).map_err(|e| std::io::Error::other(e.to_string()))?;
    implexity_io::atomic::write_atomic(path, &bytes).map_err(|e| std::io::Error::other(e.to_string()))
}


pub fn write_design(path: &Path, topology: &ArrayD<f64>, solve_id: &str) -> std::io::Result<()> {
    atomic_npz(
        path,
        &[
            ("p_topology", NpyArray::from_f64(topology)),
            ("refs", NpyArray::strings(&["model:control".to_string()])),
            ("slots", NpyArray::strings(&["topology".to_string()])),
            ("units", NpyArray::strings(&["-".to_string()])),
            ("solve_id", NpyArray::scalar_str(solve_id)),
        ],
    )
}

#[must_use]
pub fn control(job_dir: &Path) -> Option<String> {
    let path = job_dir.join("control.json");
    if !path.is_file() {
        return None;
    }
    let op =
        std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|d| {
            d.get("op")
                .filter(|v| implexity_core::pyobj::truthy(v))
                .map(implexity_core::pyobj::py_str)
                .unwrap_or_default()
                .to_lowercase()
        });
    op.as_ref()?;
    let _ = std::fs::remove_file(&path);
    op.filter(|o| o == "pause" || o == "stop")
}


pub fn load_topology(path: &Path) -> CaeResult<ArrayD<f64>> {
    let npz = implexity_io::npz::load_file(path).map_err(|e| CaeError::contract(e.to_string()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(raw) = npz.get("topology").or_else(|| npz.get("p_topology")) else {
        return contract(format!("{name} contains no topology array"));
    };
    if matches!(raw.data, implexity_io::npy::NpyData::C64(_) | implexity_io::npy::NpyData::C128(_)) {
        return contract("model:control must not contain complex data");
    }
    match raw.to_f64() {
        Some(x) if x.ndim() == 3 && x.iter().all(|v| v.is_finite()) => Ok(x),
        _ => contract("model:control must be a finite three-dimensional array"),
    }
}

fn strings_equal(archive: &implexity_io::npz::Npz, name: &str, expected: &[&str]) -> bool {
    use implexity_io::npy::NpyData;
    archive.get(name).is_some_and(|a| {
        a.shape == [expected.len()]
            && matches!(&a.data, NpyData::Unicode { values, .. } if values.iter().map(String::as_str).eq(expected.iter().copied()))
    })
}


pub fn load_scalar_live_snapshot(path: &Path, solve_id: &str) -> CaeResult<ArrayD<f64>> {
    let name = repr(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let data = implexity_io::npz::load_file(path)
        .map_err(|_| CaeError::contract(format!("resume live snapshot {name} is unreadable")))?;
    let mut files: Vec<&str> = data.files();
    files.sort_unstable();
    if files != ["p_topology", "refs", "slots", "solve_id", "units"] {
        return contract(format!("resume live snapshot {name} has an invalid layout"));
    }
    if !strings_equal(&data, "refs", &["model:control"])
        || !strings_equal(&data, "slots", &["topology"])
        || !strings_equal(&data, "units", &["-"])
        || strict_text(&data, "solve_id")? != solve_id
    {
        return contract(format!("resume live snapshot {name} metadata drifted"));
    }
    let raw = data
        .get("p_topology")
        .ok_or_else(|| CaeError::contract(format!("resume live snapshot {name} is unreadable")))?;
    if matches!(raw.data, implexity_io::npy::NpyData::C64(_) | implexity_io::npy::NpyData::C128(_)) {
        return contract(format!("resume live snapshot {name} contains complex data"));
    }
    let Some(value) = raw.to_f64() else {
        return contract(format!("resume live snapshot {name} is not numeric"));
    };
    if value.ndim() != 3 || !value.iter().all(|v| v.is_finite()) {
        return contract(format!("resume live snapshot {name} is not a finite three-dimensional array"));
    }
    Ok(value)
}

fn scalar_merit(row: &Value, bound_tolerance: f64) -> (i32, f64) {
    let violation = row.get("max_scaled_bound_violation").and_then(Value::as_f64).unwrap_or(f64::NAN);
    if violation <= bound_tolerance {
        (0, row.get("base_objective").and_then(Value::as_f64).unwrap_or(f64::NAN))
    } else {
        (1, violation)
    }
}

fn merit_less(a: (i32, f64), b: (i32, f64)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
}

#[must_use]
pub fn scalar_best_index(history: &[Value], bound_tolerance: f64) -> usize {
    let mut best = 0;
    for index in 1..history.len() {
        if merit_less(
            scalar_merit(&history[index], bound_tolerance),
            scalar_merit(&history[best], bound_tolerance),
        ) {
            best = index;
        }
    }
    best
}

#[derive(Debug, Clone)]
pub struct ScalarResume {
    pub topology: ArrayD<f64>,
    pub start: i64,
    pub history: Vec<Value>,
    pub first: f64,
    pub best: f64,
    pub best_design_state_id: String,
    pub responses: Vec<ResponseSpec>,
    pub normalization: Option<Value>,
    pub search_state: implexity_optim::search::SearchState,
}

#[derive(Debug, Clone)]
pub struct ScalarResumeIdentity<'a> {
    pub initial_design_state_id: &'a str,
    pub run_fingerprint: &'a str,
    pub provider_name: &'a str,
    pub solve_id: &'a str,
    pub requested_policy_digest: &'a str,
    pub effective_effort_digest: &'a str,
    pub operation_context_digest: &'a str,
    pub runtime_source_sha256: &'a str,
    pub exact_correction_budget: i64,
}

fn finite_field(row: &Map<String, Value>, name: &str) -> bool {
    row.get(name).is_some_and(|v| v.is_number() && v.as_f64().is_some_and(f64::is_finite))
}


#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn load_scalar_resume_state(
    checkpoint_path: &Path,
    history_path: &Path,
    output_dir: &Path,
    settings: &LegacySingleArrayOptimizationSettings,
    responses: &[ResponseSpec],
    response_normalization: Option<&Value>,
    initial: &ArrayD<f64>,
    id: &ScalarResumeIdentity<'_>,
) -> CaeResult<ScalarResume> {
    let required = [
        "checkpoint_schema",
        "topology",
        "it_next",
        "design_state_id",
        "initial_design_state_id",
        "run_fingerprint",
        "best_design_state_id",
        "history_length",
        "history_digest",
        "L_first",
        "L_best",
        "requested_policy_digest",
        "effective_effort_digest",
        "operation_context_digest",
        "runtime_source_sha256",
        "continuable",
        "response_normalization",
        "search_state",
    ];
    let data = implexity_io::npz::load_file(checkpoint_path)
        .map_err(|_| CaeError::contract("resume checkpoint is unreadable"))?;
    let fields: Vec<&str> = data.files();
    if !required.iter().all(|k| fields.contains(k)) {
        return contract(
            "scalar checkpoint lacks authoritative resume identity or search state; start a fresh job",
        );
    }
    if fields.len() != required.len() {
        return contract("resume checkpoint contains undeclared scalar state");
    }
    if strict_text(&data, "checkpoint_schema")? != SCALAR_CHECKPOINT_SCHEMA {
        return contract("resume checkpoint schema is unsupported");
    }
    let stored_requested = strict_text(&data, "requested_policy_digest")?;
    let stored_effective = strict_text(&data, "effective_effort_digest")?;
    let stored_context = strict_text(&data, "operation_context_digest")?;
    let stored_source = strict_text(&data, "runtime_source_sha256")?;
    if stored_requested != id.requested_policy_digest
        || stored_effective != id.effective_effort_digest
        || stored_context != id.operation_context_digest
    {
        return contract("resume checkpoint effort identity drifted");
    }
    if stored_source != id.runtime_source_sha256 {
        return contract("resume checkpoint runtime source identity drifted");
    }
    let stored_run = strict_text(&data, "run_fingerprint")?;
    let stored_initial = strict_text(&data, "initial_design_state_id")?;
    let stored_design = strict_text(&data, "design_state_id")?;
    let stored_best_design = strict_text(&data, "best_design_state_id")?;
    let history_digest = strict_text(&data, "history_digest")?;
    let start = strict_integer(&data, "it_next")?;
    let history_length = strict_integer(&data, "history_length")?;
    let first = strict_number(&data, "L_first")?;
    let best = strict_number(&data, "L_best")?;
    let Scalar::Boolean(continuable) = strict_checkpoint_scalar(&data, "continuable", "boolean")? else {
        return contract("resume checkpoint 'continuable' must be an exact boolean scalar");
    };
    let stored_normalization = strict_text(&data, "response_normalization")?;
    let stored_search_state = strict_text(&data, "search_state")?;
    let raw_topology =
        data.get("topology").cloned().ok_or_else(|| CaeError::contract("resume checkpoint is unreadable"))?;
    if stored_run != id.run_fingerprint {
        return contract("resume checkpoint run identity drifted");
    }
    let parse = |t: &str| serde_json::from_str::<Value>(t);
    let (Ok(normalization_record), Ok(search_state_record)) =
        (parse(&stored_normalization), parse(&stored_search_state))
    else {
        return contract("resume checkpoint normalization or search state is malformed");
    };
    if canonical_text(&normalization_record) != stored_normalization
        || canonical_text(&search_state_record) != stored_search_state
    {
        return contract("resume checkpoint normalization or search state is not canonical");
    }
    let (responses, normalization) = implexity_optim::bounds::restore_response_normalization(
        responses,
        response_normalization,
        Some(&normalization_record),
    )?;
    let coordinate = settings.as_coordinate_settings();
    let search_state = ProjectedSearch::state_from_wire(&search_state_record, &responses, &coordinate)?;
    if stored_initial != id.initial_design_state_id {
        return contract("resume checkpoint initial design identity drifted");
    }
    if !(0..=id.exact_correction_budget).contains(&start) {
        return contract("resume checkpoint iteration is outside the declared budget");
    }
    if history_length != start {
        return contract("resume checkpoint history length disagrees with its iteration");
    }
    if !continuable {
        return contract("terminal scalar checkpoint cannot be resumed");
    }
    if matches!(raw_topology.data, implexity_io::npy::NpyData::C64(_) | implexity_io::npy::NpyData::C128(_)) {
        return contract("resume checkpoint topology contains complex data");
    }
    let Some(topology) = raw_topology.to_f64() else {
        return contract("resume checkpoint topology is not numeric");
    };
    let lo = coordinate.coordinate_lower.as_f64();
    let hi = coordinate.coordinate_upper.as_f64();
    if topology.shape() != initial.shape() || topology.iter().any(|v| !v.is_finite() || *v < lo || *v > hi) {
        return contract("resume checkpoint topology violates shape, finiteness, or bounds");
    }
    let checkpoint_design_id = design_identity(&NamedArrays::single("model:control", topology.clone()))?;
    if stored_design != checkpoint_design_id {
        return contract("resume checkpoint design identity drifted");
    }
    if !history_path.is_file() {
        return contract("resume history sidecar is missing");
    }
    let payload: Value = std::fs::read_to_string(history_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| CaeError::contract("resume history sidecar is unreadable"))?;
    let well_formed = payload.as_object().is_some_and(|p| {
        p.len() == 2
            && p.get("schema").and_then(Value::as_str) == Some(SCALAR_HISTORY_SCHEMA)
            && p.get("history").is_some_and(Value::is_array)
    });
    if !well_formed {
        return contract("resume history sidecar schema is malformed");
    }
    let history: Vec<Value> = payload["history"].as_array().cloned().unwrap_or_default();
    if i64::try_from(history.len()).ok() != Some(history_length) {
        return contract("resume history length disagrees with the checkpoint commit marker");
    }
    if history.is_empty() {
        return contract("resume checkpoint has no committed scalar history generation");
    }
    let bound_tolerance = coordinate.bound_tolerance.as_f64();
    for (index, row) in history.iter().enumerate() {
        let Some(row_map) = row.as_object() else {
            return contract("resume history row is malformed");
        };
        let keys_ok = SCALAR_HISTORY_REQUIRED_FIELDS.iter().all(|k| row_map.contains_key(*k))
            && row_map.keys().all(|k| {
                SCALAR_HISTORY_REQUIRED_FIELDS.contains(&k.as_str())
                    || SCALAR_HISTORY_OPTIONAL_ENGINE_FIELDS.contains(&k.as_str())
            });
        if !keys_ok {
            return contract("resume history row schema is malformed");
        }
        let int_at = |k: &str| row_map.get(k).filter(|v| v.is_i64()).and_then(Value::as_i64);
        if int_at("i") != i64::try_from(index).ok() || int_at("iteration") != i64::try_from(index).ok() {
            return contract("resume history iteration sequence is malformed");
        }
        if row_map.get("provider").and_then(Value::as_str) != Some(id.provider_name)
            || row_map.get("topology_coordinate").and_then(Value::as_str) != Some("model:control")
            || row_map.get("run_fingerprint").and_then(Value::as_str) != Some(id.run_fingerprint)
        {
            return contract("resume history run identity drifted");
        }
        if row_map.get("requested_policy_digest").and_then(Value::as_str) != Some(id.requested_policy_digest)
            || row_map.get("effective_effort_digest").and_then(Value::as_str)
                != Some(id.effective_effort_digest)
            || row_map.get("operation_context_digest").and_then(Value::as_str)
                != Some(id.operation_context_digest)
            || row_map.get("runtime_source_sha256").and_then(Value::as_str) != Some(id.runtime_source_sha256)
        {
            return contract("resume history execution identity drifted");
        }
        if row_map.get("design_state_id").and_then(Value::as_str).is_none_or(str::is_empty) {
            return contract("resume history omitted a design identity");
        }
        let terms_ok = row_map.get("terms").and_then(Value::as_array).is_some_and(|terms| {
            !terms.is_empty()
                && terms.iter().all(|t| {
                    t.as_object().is_some_and(|t| {
                        t.len() == 3
                            && t.get("response").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                            && finite_field(t, "value")
                            && finite_field(t, "objective_contribution")
                    })
                })
        });
        #[allow(clippy::float_cmp)]
        let record_ok = row_map.get("accepted").is_some_and(Value::is_boolean)
            && row_map.get("continuable").is_some_and(Value::is_boolean)
            && SCALAR_HISTORY_FINITE_FIELDS.iter().all(|k| finite_field(row_map, k))
            && terms_ok
            && row_map.get("multiplier_updates").is_some_and(Value::is_array)
            && row_map["L"].as_f64() == row_map["objective"].as_f64()
            && row_map["L_first"].as_f64() == Some(first);
        if !record_ok {
            return contract("resume history objective record is malformed");
        }
        let checked_state = ProjectedSearch::state_from_wire(row_map.get("search_state").unwrap_or(&Value::Null), &responses, &coordinate)?;
        if row_map.get("step_policy").is_some_and(|v| v.as_str() != Some(checked_state.step_policy.as_str()))
            || (row_map["search_state"]["schema"].as_str() == Some(implexity_optim::optimizer::SEARCH_STATE_SCHEMA) && row_map.get("step_policy").is_none()) {
            return contract("resume history step policy disagrees with checkpoint");
        }
        let accepted = row_map["accepted"].as_bool() == Some(true);
        let reason = row_map.get("reason").filter(|v| !v.is_null());
        let updates_empty = row_map["multiplier_updates"].as_array().is_none_or(Vec::is_empty);
        let reason_ok = !((accepted && reason.is_some())
            || reason.is_some_and(|r| !r.as_str().is_some_and(|s| TERMINAL_REASONS.contains(&s)))
            || (!accepted && reason.is_none() && updates_empty));
        if !reason_ok {
            return contract("resume history terminal reason is malformed");
        }
        let expected_continuable =
            reason.is_none() && i64::try_from(index + 1).unwrap_or(i64::MAX) < id.exact_correction_budget;
        if row_map["continuable"].as_bool() != Some(expected_continuable) {
            return contract("resume history continuation state is malformed");
        }
        let best_index = scalar_best_index(&history[..=index], bound_tolerance);
        if row_map["L_best"].as_f64() != history[best_index]["L"].as_f64()
            || row_map.get("best_design_state_id") != history[best_index].get("design_state_id")
        {
            return contract("resume history best-design sequence drifted");
        }
        let expected_name = format!("live_{index:06}.npz");
        let push_ok = row_map.get("push").and_then(Value::as_object).is_some_and(|p| {
            p.len() == 3
                && p.get("file").and_then(Value::as_str) == Some(expected_name.as_str())
                && p.get("bytes").filter(|v| v.is_i64()).and_then(Value::as_i64).is_some_and(|b| b >= 1)
                && p.get("save_ms")
                    .is_some_and(|v| v.is_number() && v.as_f64().is_some_and(|s| s.is_finite() && s >= 0.0))
        });
        if !push_ok {
            return contract("resume history live snapshot reference is malformed");
        }
    }
    if scalar_history_digest(&history) != history_digest {
        return contract("resume history digest drifted");
    }
    let final_row = history.last().cloned().unwrap_or(Value::Null);
    if final_row["design_state_id"].as_str() != Some(checkpoint_design_id.as_str()) {
        return contract("resume history final design identity disagrees with checkpoint");
    }
    if final_row["continuable"].as_bool() != Some(continuable) {
        return contract("resume checkpoint continuation state disagrees with history");
    }
    if final_row.get("search_state") != Some(&search_state_record) {
        return contract("resume checkpoint search state disagrees with history");
    }
    let expected_name = final_row["push"]["file"].as_str().unwrap_or("").to_string();
    let live_path = output_dir.join(&expected_name);
    let Ok(meta) = std::fs::metadata(&live_path) else {
        return contract(format!("resume live snapshot {} is missing", repr(&expected_name)));
    };
    if Some(meta.len()) != final_row["push"]["bytes"].as_u64() {
        return contract(format!("resume live snapshot {} size drifted", repr(&expected_name)));
    }
    let live = load_scalar_live_snapshot(&live_path, id.solve_id)?;
    if live != topology {
        return contract("resume final live snapshot disagrees with checkpoint topology");
    }
    let best_index = scalar_best_index(&history, bound_tolerance);
    let expected_best = history[best_index]["design_state_id"].as_str().unwrap_or("").to_string();
    if stored_best_design != expected_best || history[best_index]["L"].as_f64() != Some(best) {
        return contract("resume checkpoint best-design identity drifted");
    }
    let best_path = output_dir.join("best.npz");
    if !best_path.is_file() {
        return contract("resume best snapshot is missing");
    }
    let best_topology = load_scalar_live_snapshot(&best_path, id.solve_id)?;
    if best_topology.shape() != initial.shape()
        || best_topology.iter().any(|v| *v < lo || *v > hi)
        || design_identity(&NamedArrays::single("model:control", best_topology.clone()))?
            != stored_best_design
    {
        return contract("resume best snapshot identity drifted");
    }
    Ok(ScalarResume {
        topology,
        start,
        history,
        first,
        best,
        best_design_state_id: stored_best_design,
        responses,
        normalization,
        search_state,
    })
}


pub fn project_with_masks(
    x: &ArrayD<f64>,
    lo: f64,
    hi: f64,
    masks: &BoxMasks,
    initial: &ArrayD<f64>,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
) -> CaeResult<ArrayD<f64>> {
    let shape = x.shape().to_vec();
    let solid = normalise_mask(masks.fixed_solid.as_ref(), &shape, "fixed_solid")?;
    let void = normalise_mask(masks.fixed_void.as_ref(), &shape, "fixed_void")?;
    let preserve = normalise_mask(masks.preserve.as_ref(), &shape, "preserve")?;
    let designable = normalise_mask(masks.designable.as_ref(), &shape, "designable")?;
    if let (Some(s), Some(v)) = (&solid, &void)
        && s.iter().zip(v.iter()).any(|(a, b)| *a && *b)
    {
        return contract("fixed_solid and fixed_void overlap");
    }
    let apply = |y: &mut ArrayD<f64>| {
        if let Some(d) = &designable {
            ndarray::Zip::from(&mut *y).and(d).and(initial).for_each(|y, d, i| {
                if !*d {
                    *y = *i;
                }
            });
        }
        if let Some(p) = &preserve {
            ndarray::Zip::from(&mut *y).and(p).and(initial).for_each(|y, p, i| {
                if *p {
                    *y = *i;
                }
            });
        }
        if let Some(v) = &void {
            ndarray::Zip::from(&mut *y).and(v).for_each(|y, v| {
                if *v {
                    *y = lo;
                }
            });
        }
        if let Some(s) = &solid {
            ndarray::Zip::from(&mut *y).and(s).for_each(|y, s| {
                if *s {
                    *y = hi;
                }
            });
        }
    };
    let mut y = x.mapv(|v| v.max(lo).min(hi));
    apply(&mut y);
    if let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::ProjectTopology)) {
        let projected = ops.project_topology(problem, &y, initial)?;
        if projected.shape() != shape.as_slice() || projected.iter().any(|v| !v.is_finite()) {
            return contract("provider topology projection returned an invalid field");
        }
        let mut y2 = projected.mapv(|v| v.max(lo).min(hi));
        apply(&mut y2);
        let check = ops.project_topology(problem, &y2, initial)?;
        if check.shape() != shape.as_slice() || check != y2 {
            return contract("manual/fixed masks conflict with provider topology invariants");
        }
        y = y2;
    }
    Ok(y)
}


pub fn scalar_coordinate(x: &ArrayD<f64>, lo: f64, hi: f64, masks: &BoxMasks) -> CaeResult<BoxCoordinate> {
    let shape = x.shape().to_vec();
    let mut free = ArrayD::from_elem(IxDyn(&shape), true);
    if let Some(d) = normalise_mask(masks.designable.as_ref(), &shape, "designable")? {
        ndarray::Zip::from(&mut free).and(&d).for_each(|f, d| *f &= *d);
    }
    for (name, mask) in [
        ("preserve", &masks.preserve),
        ("fixed_void", &masks.fixed_void),
        ("fixed_solid", &masks.fixed_solid),
    ] {
        if let Some(m) = normalise_mask(mask.as_ref(), &shape, name)? {
            ndarray::Zip::from(&mut free).and(&m).for_each(|f, m| *f &= !*m);
        }
    }
    Ok(BoxCoordinate {
        name: "model:control".into(),
        lower: ArrayD::from_elem(IxDyn(&shape), lo),
        upper: ArrayD::from_elem(IxDyn(&shape), hi),
        designable: free,
    })
}


pub fn settings(raw: Option<&Value>) -> CaeResult<LegacySingleArrayOptimizationSettings> {
    let d = match raw {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return contract("provider optimization settings must be an object"),
    };
    implexity_core::contracts::reject_removed_constraint_keys(
        &Value::Object(d.clone()),
        "provider optimization settings",
    )?;
    let mut d =
        implexity_core::contracts::drop_retired_optimization_settings(&d, "provider optimization settings")?;
    for (old, new) in [("iters", "iterations"), ("lr", "step_fraction")] {
        if d.contains_key(old) && d.contains_key(new) {
            return contract(format!(
                "provider optimization settings cannot contain both {} and {}",
                repr(old),
                repr(new)
            ));
        }
        if let Some(v) = d.shift_remove(old) {
            d.insert(new.into(), v);
        }
    }
    let allowed = [
        "iterations",
        "step_policy",
        "step_fraction",
        "minimum_step_fraction",
        "backtracking",
        "armijo",
        "move_limit",
        "topology_lower",
        "topology_upper",
        "step_growth",
        "stationarity_tolerance",
        "bound_tolerance",
        "penalty_growth",
        "penalty_limit",
        "violation_reduction",
        "multiplier_update_interval",
    ];
    let mut unknown: Vec<&String> = d.keys().filter(|k| !allowed.contains(&k.as_str())).collect();
    unknown.sort();
    if !unknown.is_empty() {
        return contract(format!(
            "provider optimization settings have unknown keys {}",
            implexity_core::pyobj::list_repr(&unknown)
        ));
    }
    if d.get("iterations").is_some_and(|v| !(v.is_i64() || v.is_u64())) {
        return contract("provider optimization iterations must be an integer");
    }
    for (name, value) in &d {
        if name == "iterations" || name == "step_policy" || value.is_null() {
            continue;
        }
        if !value.is_number() {
            return contract(format!(
                "provider optimization setting {} must be a finite number or null",
                repr(name)
            ));
        }
        if !value.as_f64().is_some_and(f64::is_finite) {
            return contract(format!("provider optimization setting {} must be finite", repr(name)));
        }
    }
    LegacySingleArrayOptimizationSettings::from_dict(Some(&Value::Object(d)))
}

pub fn settings_for_spec(spec: &Map<String, Value>, resume: bool, out_dir: &Path) -> CaeResult<LegacySingleArrayOptimizationSettings> {
    let mut parsed = settings(spec.get("settings"))?;
    if resume && spec.get("settings").and_then(|v| v.get("step_policy")).is_none() {
        parsed.settings.step_policy = "backtracking".into();
        let history_path = out_dir.join("history.json");
        if history_path.is_file() {
            let raw = std::fs::read(&history_path).map_err(|e| CaeError::contract(format!("resume history is unreadable: {e}")))?;
            let history: Value = serde_json::from_slice(&raw).map_err(|_| CaeError::contract("resume history is malformed"))?;
            if let Some(state) = history.get("history").and_then(Value::as_array).and_then(|v| v.last()).and_then(|v| v.get("search_state")) {
                if state.get("schema").and_then(Value::as_str) == Some(implexity_optim::optimizer::SEARCH_STATE_SCHEMA) {
                    parsed.settings.step_policy = state.get("step_policy").and_then(Value::as_str).ok_or_else(|| CaeError::contract("resume state omits step policy"))?.into();
                    parsed.settings.validate()?;
                }
            }
        }
    }
    Ok(parsed)
}

pub fn settings_identity(spec: &Map<String, Value>, settings: &LegacySingleArrayOptimizationSettings) -> Value {
    let mut value = legacy_settings_value(settings);
    if settings.settings.step_policy == "backtracking" && spec.get("settings").and_then(|v| v.get("step_policy")).is_none() {
        if let Some(m) = value.as_object_mut() { m.shift_remove("step_policy"); }
    }
    value
}

#[derive(Clone)]
struct ScalarContext {
    provider: Arc<dyn CaeProvider>,
    problem: ProviderProblem,
    names: Vec<String>,
    exact_values: Rc<RefCell<Vec<BTreeMap<String, f64>>>>,
}

impl ScalarContext {
    fn sensitivities(&self, design: &NamedArrays, trial: bool) -> SearchResult<ExactPoint> {
        let array = design.get("model:control").cloned().unwrap_or_default();
        let data = crate::solver_recovery::once("provider_sensitivity", || array_response_data(self.provider.as_ref(), &self.problem, &array, &self.names, 0, trial))?;
        let mut values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        let mut diagnostics = Map::new();
        for (name, row) in data {
            values.insert(name.clone(), row.value);
            gradients.insert(name.clone(), NamedArrays::single("model:control", row.gradient));
            diagnostics.insert(name, Value::Object(row.diagnostics));
        }
        Ok(ExactPoint { design: NamedArrays::single("model:control", array), values, gradients, diagnostics })
    }

    fn values(&self, design: &NamedArrays) -> SearchResult<TrialValues> {
        if !provides(self.provider.as_ref(), DesignOp::Evaluate) {
            let point = self.sensitivities(design, true)?;
            return Ok((point.values.clone(), point.diagnostics.clone(), Some(point)));
        }
        let array = design.get("model:control").cloned().unwrap_or_default();
        let raw = crate::solver_recovery::once("provider_evaluation", || self.provider.evaluate(&self.problem, &array))?;
        let (values, diagnostics) = evaluation_response_values(&raw, &self.names, "provider evaluation")?;
        Ok((values, diagnostics, None))
    }
}

struct ScalarHooks {
    ctx: ScalarContext,
    responses: Vec<ResponseSpec>,
    lo: f64,
    hi: f64,
    masks: BoxMasks,
    initial: ArrayD<f64>,
    requested_preview: Option<ComputationEffortPolicy>,
    preview_cadence: i64,
    exact_correction_budget: i64,
    iterations: i64,
    iteration: i64,
    preview_record: Option<Map<String, Value>>,
    preview_lane: Option<ApproximationLane>,
    ranked: Vec<Ranked>,
    ranked_count: usize,
}

impl ScalarHooks {
    fn begin_iteration(&mut self, iteration: i64) {
        self.iteration = iteration;
        self.preview_record = None;
    }

    fn base_trial(
        &mut self,
        search: &ProjectedSearch,
        direction: &NamedArrays,
        step: f64,
    ) -> CaeResult<NamedArrays> {
        let mut raw = NamedArrays::new();
        for c in &search.coordinates {
            let (Some(value), Some(dir)) = (search.current.point.design.get(&c.name), direction.get(&c.name))
            else {
                continue;
            };
            let span = c.span();
            let mut moved = value.clone();
            ndarray::Zip::from(&mut moved)
                .and(dir)
                .and(&span)
                .and(&c.lower)
                .and(&c.upper)
                .and(&c.designable)
                .for_each(|m, d, s, lo, hi, des| {
                    if *des {
                        *m = (*m + step * d * s).max(*lo).min(*hi);
                    }
                });
            raw.insert(c.name.clone(), moved);
        }
        self.project(&search.current.point.design, raw)
    }
}

impl SearchHooks for ScalarHooks {
    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues> {
        self.ctx.values(design)
    }

    fn sensitivities(&mut self, design: &NamedArrays) -> SearchResult<ExactPoint> {
        self.ctx.sensitivities(design, true)
    }

    fn project(&mut self, _current: &NamedArrays, unprojected: NamedArrays) -> CaeResult<NamedArrays> {
        let x = unprojected.get("model:control").cloned().unwrap_or_default();
        let y = project_with_masks(
            &x,
            self.lo,
            self.hi,
            &self.masks,
            &self.initial,
            self.ctx.provider.as_ref(),
            &self.ctx.problem,
        )?;
        Ok(NamedArrays::single("model:control", y))
    }

    fn candidates(
        &mut self,
        search: &ProjectedSearch,
        direction: &NamedArrays,
        step: f64,
    ) -> CaeResult<Option<Vec<Candidate>>> {
        let Some(requested) = self.requested_preview.clone() else { return Ok(None) };
        let x = search.current.point.design.get("model:control").cloned().unwrap_or_default();
        let d = direction.get("model:control").cloned().unwrap_or_default();
        let span = self.hi - self.lo;
        let interactive = requested.mode == ComputationMode::InteractivePreview;
        let settings = &search.settings;
        let mut candidates: Vec<(f64, ArrayD<f64>)> = Vec::new();
        let mut update_count = 1;
        if interactive {
            let mut proposal = x.clone();
            let move_limit = settings.move_limit.as_f64();
            let mv = move_limit * span;
            update_count = self.preview_cadence.min(self.iterations - self.iteration * self.preview_cadence);
            #[allow(clippy::cast_precision_loss)]
            let update_step = step.min(move_limit / update_count as f64);
            for update in 0..update_count {
                let mut unprojected = proposal.clone();
                ndarray::Zip::from(&mut unprojected).and(&d).and(&x).for_each(|u, d, x| {
                    *u = (*u + update_step * span * d).max(x - mv).min(x + mv);
                });
                let trial = project_with_masks(
                    &unprojected,
                    self.lo,
                    self.hi,
                    &self.masks,
                    &self.initial,
                    self.ctx.provider.as_ref(),
                    &self.ctx.problem,
                )?;
                if trial == proposal {
                    break;
                }
                #[allow(clippy::cast_precision_loss)]
                let s = ((update + 1) as f64 * update_step).min(move_limit);
                candidates.push((s, trial.clone()));
                proposal = trial;
            }
        } else {
            let mut trial_step = step;
            while trial_step >= settings.minimum_step_fraction.as_f64() {
                let trial = self.base_trial(search, direction, trial_step)?;
                let t = trial.get("model:control").cloned().unwrap_or_default();
                if t != x {
                    candidates.push((trial_step, t));
                }
                trial_step *= settings.backtracking.as_f64();
            }
        }
        let ctx = self.ctx.clone();
        let responses = search.responses.clone();
        let bound_state: BoundMultiplierState = search.bound_state.clone();
        let exact: implexity_solve::approximation::ExactEvaluateFn = Box::new(move |target: &ArrayD<f64>| {
            let design = NamedArrays::single("model:control", target.clone());
            let (values, _d, _p) = ctx.values(&design).map_err(CaeError::from)?;
            ctx.exact_values.borrow_mut().push(values.clone());
            let a = augmented_objective(&responses, &lookup(&values), &bound_state, None)?;
            Ok(ArrayD::from_elem(IxDyn(&[1]), a.total))
        });
        let gradient = search.current.gradients.get("model:control").cloned().unwrap_or_default();
        let span_array = ArrayD::from_elem(IxDyn(x.shape()), span);
        let (ranked, ranked_count, lane) = preview_ranked_candidates(
            &x,
            &gradient,
            search.current.total,
            candidates,
            &requested,
            exact,
            &span_array,
        )?;
        let anchor_id = design_identity(&NamedArrays::single("model:control", x.clone()))?;
        let unflatten = |t: &ArrayD<f64>| -> CaeResult<NamedArrays> {
            Ok(NamedArrays::single("model:control", t.clone()))
        };
        let record = preview_record(
            &requested,
            &anchor_id,
            &ranked,
            ranked_count,
            &unflatten,
            self.iteration * self.preview_cadence,
            update_count,
            self.iteration,
            self.exact_correction_budget,
        )?;
        self.preview_record = Some(record);
        self.preview_lane = Some(lane);
        self.ranked_count = ranked_count;
        let ordered: Vec<Ranked> =
            if interactive { ranked.iter().rev().cloned().collect() } else { ranked.clone() };
        self.ranked.clone_from(&ordered);
        Ok(Some(
            ordered
                .into_iter()
                .enumerate()
                .map(|(i, (s, t, _))| (s, NamedArrays::single("model:control", t), Value::from(i)))
                .collect(),
        ))
    }

    fn evaluate_candidate(
        &mut self,
        _search: &ProjectedSearch,
        step: f64,
        design: &NamedArrays,
        meta: &Value,
    ) -> SearchResult<TrialValues> {
        let index = meta.as_u64().and_then(|i| usize::try_from(i).ok()).unwrap_or(usize::MAX);
        let Some((_, _, preview)) = self.ranked.get(index).cloned() else {
            return self.values(design);
        };
        let Some(preview) = preview else {
            return self.values(design);
        };
        self.ctx.exact_values.borrow_mut().clear();
        let lane =
            self.preview_lane.as_mut().ok_or_else(|| CaeError::contract("preview lane is unavailable"))?;
        let corrected = lane.correct(&preview, false).map_err(CaeError::from)?;
        let values = self
            .ctx
            .exact_values
            .borrow_mut()
            .pop()
            .ok_or_else(|| CaeError::contract("exact correction produced no values"))?;
        let requested = self
            .requested_preview
            .clone()
            .ok_or_else(|| CaeError::contract("preview policy is unavailable"))?;
        let wire = preview_wire(
            &requested,
            &preview,
            &corrected,
            step,
            index + 1,
            self.ranked_count,
            &design_identity(design)?,
        );
        if let Some(Value::Array(corrections)) =
            self.preview_record.as_mut().and_then(|r| r.get_mut("exact_corrections"))
        {
            corrections.push(wire);
        }
        Ok((values, Map::new(), None))
    }

    fn admit(
        &mut self,
        _current: &NamedArrays,
        _trial: &NamedArrays,
    ) -> CaeResult<Option<CandidateAdmission>> {
        Ok(None)
    }
}

fn scalar_point(x: &ArrayD<f64>, evaluation: &implexity_optim::optimizer::ObjectiveEvaluation) -> ExactPoint {
    let mut values = BTreeMap::new();
    let mut gradients = BTreeMap::new();
    for (name, row) in &evaluation.response_data {
        values.insert(name.clone(), row.value);
        gradients.insert(name.clone(), NamedArrays::single("model:control", row.gradient.clone()));
    }
    ExactPoint {
        design: NamedArrays::single("model:control", x.clone()),
        values,
        gradients,
        diagnostics: evaluation.diagnostics.clone(),
    }
}

pub fn emit(prefix: &str, payload: &Value) {
    emit_line(prefix, payload);
}


#[allow(clippy::too_many_lines)]
pub fn run_in_effort_scope(
    spec: &Map<String, Value>,
    job_dir: &Path,
    resume: bool,
    execution_started: Instant,
    source_owned_candidates: Option<&Value>,
) -> JobResult<Map<String, Value>> {
    let (mut spec, provider_name) = require_raw_job_authority(&Value::Object(spec.clone()))?;
    if let Some(matching) = spec.get_mut("matching_time_guess").filter(|v| !v.is_null()) {
        let Value::Object(matching) = matching else {
            return Err(JobError::contract("matching-time guess job transport is malformed"));
        };
        let private_root = std::env::var("IMPLEXITY_MATCHING_TIME_GUESS_ROOT").ok().filter(|s| !s.is_empty());
        for operation in ["consume", "produce"] {
            match matching.get_mut(operation) {
                None | Some(Value::Null | Value::Bool(false)) => {}
                Some(Value::Object(row)) => {
                    if !row.contains_key("store_root") {
                        let Some(root) = &private_root else {
                            return Err(JobError::contract(
                                "matching-time guess private store routing is unavailable",
                            ));
                        };
                        row.insert("store_root".into(), Value::String(root.clone()));
                    }
                }
                Some(_) => {
                    return Err(JobError::contract(format!(
                        "matching-time guess {operation} transport is malformed"
                    )));
                }
            }
        }
    }
    if let Some(snapshot) = spec.get("runtime_packages") {
        implexity_core::packages::global().activate_snapshot_value(snapshot)?;
    }
    if let Some(imports) = spec.get("physics_snapshot").and_then(|s|s.get("imported_providers")) { implexity_runtime::provider_import::restore_snapshot(imports)?; }
    if spec.get("physics_snapshot").is_some_and(implexity_core::pyobj::truthy) {
        let current = physics_snapshot()?;
        if spec["physics_snapshot"].get("registry_fingerprint") != current.get("registry_fingerprint") {
            return Err(JobError::contract(
                "job physics snapshot is stale; numerical/constitutive contracts changed",
            ));
        }
    }
    if spec.get("design_coordinates").is_some_and(implexity_core::pyobj::truthy)
        || spec.get("schedule").is_some_and(implexity_core::pyobj::truthy)
    {
        require_single_provider_schedule(spec.get("provider").unwrap_or(&Value::Null), spec.get("schedule"))?;
        let parsed = settings_for_spec(&spec, resume, job_dir)?;
        return crate::hierarchical_job::run(&spec, job_dir, resume, &parsed, execution_started);
    }
    if !resume {
        refuse_stale_scalar_outputs(job_dir)?;
    }
    let provider = implexity_core::registries::global().providers.get(&provider_name)?;
    let caps = provider.capabilities()?;
    let ProviderCapabilities::Legacy(legacy) = &caps else {
        return Err(JobError::contract(format!(
            "canonical provider {} requires explicit named design_coordinates in the provider-job specification",
            repr(&provider_name)
        )));
    };
    if legacy.execution != "array" {
        return Err(JobError::contract(format!(
            "provider {} declares execution={}; the provider child is only for array providers",
            repr(&provider_name),
            repr(&legacy.execution)
        )));
    }
    let current_physics = physics_snapshot()?;
    let effort_binding = match spec.get("computation_effort") {
        Some(b) if spec.contains_key("computation_effort") => b.clone(),
        _ => {
            let solve = spec
                .get("solve_id")
                .filter(|v| implexity_core::pyobj::truthy(v))
                .map_or_else(|| "provider-job".to_string(), implexity_core::pyobj::py_str);
            let mut scope = Map::new();
            scope.insert("provider".into(), Value::String(provider_name.clone()));
            scope.insert("solve_id".into(), Value::String(solve));
            let mut context = Map::new();
            context.insert("operation".into(), Value::String("provider_optimization".into()));
            context.insert("scope_digest".into(), Value::String(fingerprint(&Value::Object(scope))));
            Value::Object(make_server_effort_binding(
                None,
                &provider_name,
                &caps,
                &current_physics,
                Some(&context),
                None,
            )?)
        }
    };
    let facts = ProviderFacts {
        provider_name: &provider_name,
        capabilities: &caps,
        physics: &current_physics,
        candidates: source_owned_candidates,
    };
    let (selection, effort_binding) = validate_effort_binding(&effort_binding, Some(&facts))?;
    let digest = |k: &str| effort_binding.get(k).map(implexity_core::pyobj::py_str).unwrap_or_default();
    let requested_policy_digest = digest("requested_policy_digest");
    let effective_effort_digest = digest("effective_effort_digest");
    let operation_context_digest = digest("operation_context_digest");
    let (runtime_source, runtime_source_sha256) =
        runtime_source_binding(&implexity_solve::matching_time_guess::runtime_source_identity())?;
    let problem = provider.normalise_problem(
        spec.get("problem")
            .filter(|v| implexity_core::pyobj::truthy(v))
            .unwrap_or(&Value::Object(Map::new())),
    )?;
    let mut responses: Vec<ResponseSpec> = spec
        .get("responses")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(ResponseSpec::from_dict).collect::<CaeResult<Vec<_>>>())
        .transpose()?
        .unwrap_or_default();
    if responses.is_empty() {
        return Err(JobError::contract("provider job requires at least one differentiable response"));
    }
    let normalization_spec = spec.get("response_normalization").filter(|v| !v.is_null()).cloned();
    normalise_response_normalization(normalization_spec.as_ref(), &responses)?;
    let parsed = settings_for_spec(&spec, resume, job_dir)?;
    let coordinate_settings = parsed.as_coordinate_settings();
    let lo = coordinate_settings.coordinate_lower.as_f64();
    let hi = coordinate_settings.coordinate_upper.as_f64();
    let requested_preview = preview_policy(&selection);
    let (preview_cadence, exact_correction_budget) =
        interactive_correction_plan(requested_preview.as_ref(), coordinate_settings.iterations);
    let solve_id = spec
        .get("solve_id")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .map_or_else(|| "provider-job".to_string(), implexity_core::pyobj::py_str);
    let topology_file = spec
        .get("topology_file")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .map_or_else(|| "topology_initial.npz".to_string(), implexity_core::pyobj::py_str);
    let initial = load_topology(&job_dir.join(topology_file))?;
    let masks_value = spec.get("masks").and_then(Value::as_object).cloned().unwrap_or_default();
    let masks = BoxMasks::from_value(&masks_value)?;
    let initial_design_state_id = design_identity(&NamedArrays::single("model:control", initial.clone()))?;
    let run_fingerprint = scalar_run_fingerprint(
        &spec,
        &parsed,
        &initial_design_state_id,
        &provider_name,
        &caps,
        &current_physics,
        &effort_binding,
        &runtime_source,
    )?;
    let ckpt = job_dir.join("ckpt.npz");
    let hist_path = job_dir.join("history.json");
    let epoch_provenance = crate::epoch_state::run_provenance(
        &spec,
        &provider_name,
        &serde_json::json!({
            "run_fingerprint": run_fingerprint,
            "runtime_source_sha256": runtime_source_sha256,
            "requested_policy_digest": requested_policy_digest,
            "effective_effort_digest": effective_effort_digest,
            "operation_context_digest": operation_context_digest,
            "solve_id": solve_id,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    );
    let mut history: Vec<Value> = Vec::new();
    let mut start = 0_i64;
    let mut first: Option<f64> = None;
    let mut best = f64::INFINITY;
    let mut best_design_state_id;
    let mut normalization_record: Option<Value> = None;
    let mut search_state = None;
    let mut x;
    if resume {
        let recovery_identities = epoch_provenance["identities"].as_object().cloned().unwrap_or_default().into_iter().filter(|(key,_)| key != "solve_id").collect();
        if let Some(recovery) = crate::epoch_state::restore_committed_generation(job_dir, &recovery_identities, &solve_id)? { emit("GENERATION_RECOVERY ", &recovery); }
        if !ckpt.is_file() {
            return Err(JobError::contract("resume requested but ckpt.npz does not exist"));
        }
        let restored = load_scalar_resume_state(
            &ckpt,
            &hist_path,
            job_dir,
            &parsed,
            &responses,
            normalization_spec.as_ref(),
            &initial,
            &ScalarResumeIdentity {
                initial_design_state_id: &initial_design_state_id,
                run_fingerprint: &run_fingerprint,
                provider_name: &provider_name,
                solve_id: &solve_id,
                requested_policy_digest: &requested_policy_digest,
                effective_effort_digest: &effective_effort_digest,
                operation_context_digest: &operation_context_digest,
                runtime_source_sha256: &runtime_source_sha256,
                exact_correction_budget,
            },
        )?;
        x = restored.topology;
        start = restored.start;
        history = restored.history;
        let moved = crate::epoch_state::set_aside_uncommitted(job_dir, history.len())?;
        if !moved.is_empty() {
            emit("EPOCH_STATE_SET_ASIDE ", &serde_json::json!({"files": moved}));
        }
        first = Some(restored.first);
        best = restored.best;
        best_design_state_id = restored.best_design_state_id;
        responses = restored.responses;
        normalization_record = restored.normalization;
        search_state = Some(restored.search_state);
        let projected = project_with_masks(&x, lo, hi, &masks, &initial, provider.as_ref(), &problem)?;
        if projected != x {
            return Err(JobError::contract("resume checkpoint topology requires projection repair"));
        }
    } else {
        x = project_with_masks(&initial, lo, hi, &masks, &initial, provider.as_ref(), &problem)?;
        best_design_state_id = design_identity(&NamedArrays::single("model:control", x.clone()))?;
    }
    let preflight = provider.preflight(&problem, Some(&x))?;
    if !preflight.get("ok").is_some_and(implexity_core::pyobj::truthy) {
        let issues = preflight
            .get("issues")
            .filter(|v| implexity_core::pyobj::truthy(v))
            .or_else(|| preflight.get("errors").filter(|v| implexity_core::pyobj::truthy(v)))
            .cloned()
            .unwrap_or_else(|| Value::Object(preflight.clone()));
        return Err(JobError::contract(format!(
            "provider preflight refused the topology: {}",
            implexity_core::pyobj::py_str(&issues)
        )));
    }
    let coupling = implexity_core::coupling_graph::validate_provider_couplings(
        provider.as_ref(),
        Some(&problem),
        &implexity_core::registries::global().extensions,
        true,
    );
    if !coupling.get("ok").is_some_and(implexity_core::pyobj::truthy) {
        return Err(JobError::contract(format!(
            "automatic multiphysics coupling preflight refused optimization: {}",
            implexity_core::pyobj::repr(coupling.get("errors").unwrap_or(&Value::Null))
        )));
    }
    if !resume {
        write_design(&job_dir.join("best.npz"), &x, &solve_id)?;
    }

    let mut committed_warm = None;
    let mut warm_evidence = Value::Null;
    if resume {
        let committed = NamedArrays::single("model:control", x.clone());
        committed_warm =
            crate::hierarchical_job::read_resume_warm_start(job_dir, &design_identity(&committed)?)?;
        if let Some(guess) = &committed_warm {
            warm_evidence = crate::hierarchical_job::install_resume_warm_start(
                provider.as_ref(),
                &problem,
                &committed,
                guess,
            )?;
        }
    }
    let initial_eval = objective(
        provider.as_ref(),
        &problem,
        &x,
        &responses,
        if resume { None } else { normalization_spec.as_ref() },
        0,
        None,
    )?;
    if !resume && normalization_spec.is_some() {
        responses.clone_from(&initial_eval.resolved_responses);
        normalization_record.clone_from(&initial_eval.response_normalization);
    }
    let names = implexity_optim::bounds::response_names(&responses);
    let mut hooks = ScalarHooks {
        ctx: ScalarContext {
            provider: Arc::clone(&provider),
            problem: problem.clone(),
            names,
            exact_values: Rc::new(RefCell::new(Vec::new())),
        },
        responses: responses.clone(),
        lo,
        hi,
        masks: masks.clone(),
        initial: initial.clone(),
        requested_preview: requested_preview.clone(),
        preview_cadence,
        exact_correction_budget,
        iterations: coordinate_settings.iterations,
        iteration: 0,
        preview_record: None,
        preview_lane: None,
        ranked: Vec::new(),
        ranked_count: 0,
    };
    let coordinate = scalar_coordinate(&x, lo, hi, &masks)?;
    let mut search = ProjectedSearch::new(
        hooks.responses.clone(),
        coordinate_settings.clone(),
        vec![coordinate],
        scalar_point(&x, &initial_eval),
        search_state,
    )?;
    if resume {
        let last = history.last().cloned().unwrap_or(Value::Null);
        let evidence = implexity_runtime::replay_validation::validate_objective_replay(
            &last["L"],
            &last["terms"],
            &Value::from(search.current.total),
            &Value::Array(search.current.terms.clone()),
        )?;
        emit("RESUME_REPLAY ", &evidence);
        search.current.point.diagnostics.insert("optimizer_resume_replay".into(), evidence);
        search.current.point.diagnostics.insert("resume_warm_start".into(), warm_evidence);
    }
    let first = first.unwrap_or(search.current.total);
    let bound_tolerance = coordinate_settings.bound_tolerance.as_f64();
    for i in start..exact_correction_budget {
        hooks.begin_iteration(i);
        let warm_before = crate::hierarchical_job::accepted_warm_start(
            provider.as_ref(),
            &problem,
            &NamedArrays::single("model:control", x.clone()),
            &[0],
        );
        let engine = search.iterate(&mut hooks, i)?;
        let accepted = engine["accepted"].as_bool() == Some(true);
        if accepted {
            committed_warm = warm_before;
        }
        x = search.current.point.design.get("model:control").cloned().unwrap_or_default();
        let l = search.current.total;
        let design_state_id = design_identity(&NamedArrays::single("model:control", x.clone()))?;
        let previous_live = history.last().and_then(|r| r["push"]["file"].as_str()).map(str::to_string);
        let live_name = format!("live_{i:06}.npz");
        let t_save = perf_counter();
        write_design(&job_dir.join(&live_name), &x, &solve_id)?;
        let save_ms = (perf_counter() - t_save) * 1e3;
        let bytes = std::fs::metadata(job_dir.join(&live_name))?.len();
        let mut row = Map::new();
        row.insert("i".into(), Value::from(i));
        row.insert("iteration".into(), Value::from(i));
        row.insert("L".into(), Value::from(l));
        row.insert("objective".into(), Value::from(l));
        row.insert("L_first".into(), Value::from(first));
        row.insert("base_objective".into(), engine["base_objective"].clone());
        row.insert("gradient_norm".into(), engine["gradient_norm"].clone());
        row.insert("stationarity".into(), engine["stationarity"].clone());
        row.insert("accepted".into(), Value::Bool(accepted));
        row.insert("step_fraction".into(), engine["step_fraction"].clone());
        row.insert("trust_step".into(), engine["trust_step"].clone());
        row.insert("solid_fraction_mean".into(), Value::from(implexity_optim::numeric::array_mean(&x)));
        row.insert("terms".into(), Value::Array(search.current.terms.clone()));
        let cached = design_operations(provider.as_ref()).and_then(|ops| ops.cached_evaluation_design(&problem, &search.current.point.design, 0));
        let computed = match cached { Some(Ok(implexity_optim::provider_ops::CachedEvaluation::Available(e))) => e.responses, _ => search.current.point.values.clone() };
        row.insert("diagnostic_responses".into(), crate::hierarchical_job::diagnostic_response_rows(provider.as_ref(), &computed, 0));
        row.insert("max_scaled_bound_violation".into(), engine["max_scaled_bound_violation"].clone());
        row.insert("bound_measure".into(), engine["bound_measure"].clone());
        row.insert("bound_multipliers".into(), engine["bound_multipliers"].clone());
        row.insert("multiplier_updates".into(), engine["multiplier_updates"].clone());
        row.insert("trial_evaluations".into(), engine["trial_evaluations"].clone());
        row.insert("armijo_rejections".into(), engine["armijo_rejections"].clone());
        row.insert("search_state".into(), search.state_wire());
        row.insert("provider".into(), Value::String(provider_name.clone()));
        row.insert("topology_coordinate".into(), Value::String("model:control".into()));
        row.insert("design_state_id".into(), Value::String(design_state_id.clone()));
        row.insert("run_fingerprint".into(), Value::String(run_fingerprint.clone()));
        row.insert("requested_policy_digest".into(), Value::String(requested_policy_digest.clone()));
        row.insert("effective_effort_digest".into(), Value::String(effective_effort_digest.clone()));
        row.insert("operation_context_digest".into(), Value::String(operation_context_digest.clone()));
        row.insert("runtime_source_sha256".into(), Value::String(runtime_source_sha256.clone()));
        let terminal = engine["terminal"].as_bool() == Some(true);
        row.insert("continuable".into(), Value::Bool(!terminal && i + 1 < exact_correction_budget));
        row.insert(
            "push".into(),
            serde_json::json!({"file": live_name, "bytes": bytes, "save_ms": implexity_mesh::numeric::py_round_digits(save_ms, 3)}),
        );
        for key in SCALAR_HISTORY_OPTIONAL_ENGINE_FIELDS {
            if let Some(v) = engine.get(key) {
                row.insert(key.into(), v.clone());
            }
        }
        let mut candidates = history.clone();
        candidates.push(Value::Object(row.clone()));
        let best_index = scalar_best_index(&candidates, bound_tolerance);
        row.insert("L_best".into(), candidates[best_index]["L"].clone());
        row.insert("best_design_state_id".into(), candidates[best_index]["design_state_id"].clone());
        if best_index == history.len() {
            write_design(&job_dir.join("best.npz"), &x, &solve_id)?;
        }
        best = row["L_best"].as_f64().unwrap_or(f64::NAN);
        best_design_state_id = row["best_design_state_id"].as_str().unwrap_or("").to_string();
        if let Some(mut record) = hooks.preview_record.take() {
            record.insert("final_result_authority".into(), Value::String("exact".into()));
            record.insert("accepted_after_exact_correction".into(), Value::Bool(accepted));
            emit("PREVIEW ", &Value::Object(record));
        }
        history.push(Value::Object(row.clone()));
        let history_digest = scalar_history_digest(&history);
        if accepted || i == 0 {
            crate::hierarchical_job::write_resume_warm_start(
                job_dir,
                committed_warm.as_ref(),
                &design_state_id,
            )?;
        }
        let checkpoint: Vec<(String, NpyArray)> = [
            ("checkpoint_schema", NpyArray::scalar_str(SCALAR_CHECKPOINT_SCHEMA)),
            ("topology", NpyArray::from_f64(&x)),
            ("it_next", NpyArray::scalar_i64(i + 1)),
            ("design_state_id", NpyArray::scalar_str(&design_state_id)),
            ("initial_design_state_id", NpyArray::scalar_str(&initial_design_state_id)),
            ("run_fingerprint", NpyArray::scalar_str(&run_fingerprint)),
            ("best_design_state_id", NpyArray::scalar_str(&best_design_state_id)),
            ("history_length", NpyArray::scalar_i64(i64::try_from(history.len()).unwrap_or(i64::MAX))),
            ("history_digest", NpyArray::scalar_str(&history_digest)),
            ("L_first", NpyArray::scalar_f64(first)),
            ("L_best", NpyArray::scalar_f64(best)),
            ("requested_policy_digest", NpyArray::scalar_str(&requested_policy_digest)),
            ("effective_effort_digest", NpyArray::scalar_str(&effective_effort_digest)),
            ("operation_context_digest", NpyArray::scalar_str(&operation_context_digest)),
            ("runtime_source_sha256", NpyArray::scalar_str(&runtime_source_sha256)),
            ("continuable", NpyArray::scalar_bool(row["continuable"].as_bool() == Some(true))),
            (
                "response_normalization",
                NpyArray::scalar_str(&canonical_text(normalization_record.as_ref().unwrap_or(&Value::Null))),
            ),
            ("search_state", NpyArray::scalar_str(&canonical_text(&row["search_state"]))),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        crate::epoch_state::write_epoch_state(
            job_dir,
            &crate::epoch_state::EpochRecord {
                epoch: history.len() - 1,
                checkpoint: &checkpoint,
                warm_start: committed_warm.as_ref(),
                history_row: history.last().unwrap_or(&Value::Null),
                history_payload: &scalar_history_payload(&history),
                provenance: &epoch_provenance,
                seeded_from: None,
            },
        )?;
        atomic_json(&hist_path, &scalar_history_payload(&history))?;
        let refs: Vec<(&str, NpyArray)> = checkpoint.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        atomic_npz(&ckpt, &refs)?;
        if let Some(previous) = previous_live.filter(|p| *p != live_name) {
            let _ = std::fs::remove_file(job_dir.join(previous));
        }
        emit("ITER ", &Value::Object(row.clone()));
        if row["continuable"].as_bool() != Some(true) {
            break;
        }
        if let Some(op) = control(job_dir) {
            let design_id = design_identity(&NamedArrays::single("model:control", x.clone()))?;
            let mut halted = Map::new();
            halted.insert("op".into(), Value::String(op.clone()));
            halted.insert("iterations".into(), Value::from(history.len()));
            halted
                .insert("requested_optimization_updates".into(), Value::from(coordinate_settings.iterations));
            halted.insert("exact_corrections".into(), Value::from(history.len()));
            halted.insert("design_state_id".into(), Value::String(design_id.clone()));
            halted.insert("best_design_state_id".into(), Value::String(best_design_state_id.clone()));
            halted.insert("initial_design_state_id".into(), Value::String(initial_design_state_id.clone()));
            halted.insert("run_fingerprint".into(), Value::String(run_fingerprint.clone()));
            halted.insert("requested_policy_digest".into(), Value::String(requested_policy_digest.clone()));
            halted.insert("effective_effort_digest".into(), Value::String(effective_effort_digest.clone()));
            halted.insert("operation_context_digest".into(), Value::String(operation_context_digest.clone()));
            halted
                .insert("response_normalization".into(), normalization_record.clone().unwrap_or(Value::Null));
            let halted = attach_exact_effort_evidence(
                &halted,
                &Value::Object(effort_binding.clone()),
                execution_started,
                history.len() as u64,
            )?;
            emit("HALTED ", &Value::Object(halted.clone()));
            let mut result = Map::new();
            result.insert("status".into(), Value::String(op));
            result.insert("history".into(), Value::Array(history.clone()));
            result.insert("design_state_id".into(), Value::String(design_id));
            result.insert("best_design_state_id".into(), Value::String(best_design_state_id.clone()));
            result.insert("initial_design_state_id".into(), Value::String(initial_design_state_id.clone()));
            result.insert("run_fingerprint".into(), Value::String(run_fingerprint.clone()));
            result.insert("requested_policy_digest".into(), Value::String(requested_policy_digest.clone()));
            result.insert("effective_effort_digest".into(), Value::String(effective_effort_digest.clone()));
            result.insert("operation_context_digest".into(), Value::String(operation_context_digest.clone()));
            result
                .insert("response_normalization".into(), normalization_record.clone().unwrap_or(Value::Null));
            result.insert(
                "halted_computation_evidence".into(),
                halted.get("computation_evidence").cloned().unwrap_or(Value::Null),
            );
            return Ok(result);
        }
    }
    let mut summary = Map::new();
    summary.insert("status".into(), Value::String("completed".into()));
    summary.insert("provider".into(), Value::String(provider_name.clone()));
    summary.insert("topology_coordinate".into(), Value::String("model:control".into()));
    summary.insert("iterations".into(), Value::from(history.len()));
    summary.insert("requested_optimization_updates".into(), Value::from(coordinate_settings.iterations));
    summary.insert("exact_corrections".into(), Value::from(history.len()));
    summary.insert("L_first".into(), Value::from(first));
    summary.insert("L_best".into(), Value::from(best));
    summary.insert(
        "L_last".into(),
        history.last().map_or_else(|| Value::from(search.current.total), |r| r["L"].clone()),
    );
    summary.insert("history".into(), Value::Array(history.clone()));
    summary.insert("responses".into(), Value::Array(search.current.terms.clone()));
    summary.insert("search_outcome".into(), search.outcome(&history)?);
    summary.insert("search_state".into(), search.state_wire());
    summary.insert("initial_design_state_id".into(), Value::String(initial_design_state_id));
    summary.insert(
        "final_design_state_id".into(),
        Value::String(design_identity(&NamedArrays::single("model:control", x.clone()))?),
    );
    summary.insert("best_design_state_id".into(), Value::String(best_design_state_id));
    summary.insert("run_fingerprint".into(), Value::String(run_fingerprint));
    summary.insert("requested_policy_digest".into(), Value::String(requested_policy_digest));
    summary.insert("effective_effort_digest".into(), Value::String(effective_effort_digest));
    summary.insert("operation_context_digest".into(), Value::String(operation_context_digest));
    summary.insert("response_normalization".into(), normalization_record.unwrap_or(Value::Null));
    let summary = attach_exact_effort_evidence(
        &summary,
        &Value::Object(effort_binding),
        execution_started,
        history.len() as u64,
    )?;
    atomic_json(&job_dir.join("summary.json"), &Value::Object(summary.clone()))?;
    let mut done = summary.clone();
    done.shift_remove("history");
    emit("DONE ", &Value::Object(done));
    Ok(summary)
}


pub fn read_spec(spec_file: &Path, expected_sha256: Option<&str>) -> CaeResult<Map<String, Value>> {
    let raw = implexity_runtime::worker_preimport_bootstrap::read_regular_bytes(
        spec_file,
        "provider optimization specification",
        64 * 1024 * 1024,
    )
    .map_err(|e| CaeError::contract(e.to_string()))?;
    if let Some(expected) = expected_sha256
        && crate::private::sha256_hex(&raw) != expected
    {
        return contract("provider optimization specification identity drifted");
    }
    implexity_runtime::exact_acceleration_production_authority::parse_strict_production_json(
        &raw,
        "provider optimization specification",
    )
}


pub fn run(spec_file: &Path, job_dir: &Path, resume: bool) -> JobResult<Map<String, Value>> {
    run_expected(spec_file, job_dir, resume, None)
}


pub fn run_expected(
    spec_file: &Path,
    job_dir: &Path,
    resume: bool,
    expected_sha256: Option<&str>,
) -> JobResult<Map<String, Value>> {
    let execution_started = Instant::now();
    let parsed = read_spec(spec_file, expected_sha256)?;
    let (spec, provider_name) = require_raw_job_authority(&Value::Object(parsed))?;
    if let Some(snapshot) = spec.get("runtime_packages") {
        implexity_core::packages::global().activate_snapshot_value(snapshot)?;
    }
    if let Some(imports) = spec.get("physics_snapshot").and_then(|s|s.get("imported_providers")) { implexity_runtime::provider_import::restore_snapshot(imports)?; }
    let provider = implexity_core::registries::global().providers.get(&provider_name)?;
    let capabilities = provider.capabilities()?;
    let current_physics = physics_snapshot()?;
    let effort_binding = if let Some(b) = spec.get("computation_effort").filter(|v| !v.is_null()) {
        b.clone()
    } else {
        let solve = spec
            .get("solve_id")
            .filter(|v| implexity_core::pyobj::truthy(v))
            .map_or_else(|| "provider-job".to_string(), implexity_core::pyobj::py_str);
        let mut scope = Map::new();
        scope.insert("provider".into(), Value::String(provider_name.clone()));
        scope.insert("solve_id".into(), Value::String(solve));
        let mut context = Map::new();
        context.insert("operation".into(), Value::String("provider_optimization".into()));
        context.insert("scope_digest".into(), Value::String(fingerprint(&Value::Object(scope))));
        Value::Object(make_server_effort_binding(
            None,
            &provider_name,
            &capabilities,
            &current_physics,
            Some(&context),
            None,
        )?)
    };
    let (effort_binding, candidates) = match private_provider_effort_validation(
        &provider_name,
        provider.as_ref(),
        &capabilities,
        &current_physics,
        &effort_binding,
    )? {
        Some((rebound, candidates)) => (Value::Object(rebound), Some(candidates)),
        None => (effort_binding, None),
    };
    let facts = ProviderFacts {
        provider_name: &provider_name,
        capabilities: &capabilities,
        physics: &current_physics,
        candidates: candidates.as_ref(),
    };
    let (_selection, effort_binding) = validate_effort_binding(&effort_binding, Some(&facts))?;
    let mut spec = spec;
    spec.insert("computation_effort".into(), Value::Object(effort_binding.clone()));
    let mut deviation_policy = spec.get("numerical_deviation_policy").filter(|v| !v.is_null()).cloned();
    let decision_path = job_dir.join("numerical_attention_decision.json");
    if decision_path.is_file() {
        if !resume || deviation_policy.is_some() {
            return Err(JobError::contract("numerical attention decision is valid only for exact resume"));
        }
        let decision: Value = std::fs::read_to_string(&decision_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .ok_or_else(|| CaeError::contract("numerical attention decision is unreadable"))?;
        let checked = validate_numerical_deviation_policy(Some(&decision))?;
        if checked.as_ref().and_then(|p| p.get("action")).and_then(Value::as_str) != Some("retry_exact") {
            return Err(JobError::contract("resume numerical attention decision must be retry_exact"));
        }
        deviation_policy = checked.map(Value::Object);
        std::fs::remove_file(&decision_path)
            .map_err(|_| CaeError::contract("numerical attention decision could not be consumed"))?;
    }
    let binding = Value::Object(effort_binding);
    let outcome = RefCell::new(None);
    let result = implexity_runtime::numerical_progress::managed_numerical_progress(|| {
        numerical_deviation_scope(deviation_policy.as_ref(), |_| {
            let _scope = provider_computation_effort_scope(provider.as_ref(), Some(&binding))?;
            let _numerical = match provider
                .as_any()
                .downcast_ref::<implexity_runtime::intent_orchestrated::IntentOrchestratedProvider>(
            ) {
                Some(intent) => {
                    intent.numerical_computation_effort_scope(spec.get("problem").unwrap_or(&Value::Null))?
                }
                None => None,
            };
            let r = run_in_effort_scope(&spec, job_dir, resume, execution_started, candidates.as_ref());
            match r {
                Ok(v) => Ok(v),
                Err(JobError::Cae(e)) => Err(e),
                Err(other) => {
                    let message = other.message();
                    *outcome.borrow_mut() = Some(other);
                    Err(CaeError::contract(message))
                }
            }
        })
    });
    match result {
        Ok(v) => Ok(v),
        Err(e) => Err(outcome.into_inner().unwrap_or(JobError::Cae(e))),
    }
}

#[must_use]
pub fn main(args: &[String]) -> i32 {
    let mut spec_file = None;
    let mut job_dir = None;
    let mut resume = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--spec-file" => spec_file = it.next().cloned(),
            "--job-dir" => job_dir = it.next().cloned(),
            "--resume" => resume = true,
            other => {
                emit("ERROR ", &error_payload(&JobError::value(format!("unrecognized arguments: {other}"))));
                return 2;
            }
        }
    }
    let (Some(spec_file), Some(job_dir)) = (spec_file, job_dir) else {
        emit(
            "ERROR ",
            &error_payload(&JobError::value("the following arguments are required: --spec-file, --job-dir")),
        );
        return 2;
    };
    crate::solver_telemetry::clear_raised();
    match run(Path::new(&spec_file), Path::new(&job_dir), resume) {
        Ok(_) => 0,
        Err(error) => boundary(&error, Path::new(&job_dir)),
    }
}

#[must_use]
pub fn boundary(error: &JobError, job_dir: &Path) -> i32 {
    let event = match error {
        JobError::Cae(e) => raised(e),
        JobError::Other { .. } | JobError::Problems { .. } | JobError::Recovery { .. } => None,
    };
    match event {
        Some(NumericalEvent::Deviation(evidence)) => {
            match crate::hierarchical_job::write_numerical_attention_checkpoint(job_dir, &evidence) {
                Ok(payload) => {
                    emit("ATTENTION ", &Value::Object(payload));
                    0
                }
                Err(boundary_error) => {
                    let payload = serde_json::json!({
                        "error": format!("NumericalCertificationDeviation: {}", error.message()),
                        "problems": [error.message(), format!("safe numerical-attention boundary unavailable: {}", boundary_error.message())],
                    });
                    emit("ERROR ", &payload);
                    2
                }
            }
        }
        Some(NumericalEvent::Failure(evidence)) => {
            let payload = serde_json::json!({
                "error": format!("NumericalSolverFailure: {}", error.message()),
                "problems": [error.message()],
                "numerical_solver_failure": evidence,
            });
            emit("ERROR ", &payload);
            2
        }
        None => {
            emit("ERROR ", &error_payload(error));
            2
        }
    }
}

#[doc(hidden)]
#[must_use]
pub fn line(prefix: &str, payload: &Value) -> String {
    format!("{prefix}{}", compact_text(payload))
}
