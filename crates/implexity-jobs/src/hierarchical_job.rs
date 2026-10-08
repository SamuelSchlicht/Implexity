// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use implexity_core::contracts::{
    CaeProvider, CoordinateOptimizationSettings, LegacySingleArrayOptimizationSettings,
    MatchingTimeNewtonGuess, ProviderCapabilities, ProviderProblem, ResponseSpec,
};
use implexity_core::pyobj::{py_str, truthy};
use implexity_core::{CaeError, CaeResult};
use implexity_io::npy::{NpyArray, NpyData};
use implexity_optim::bounds::{
    BOUND_SENSES, normalise_response_normalization, resolve_initial_response_normalization,
    restore_response_normalization,
};
use implexity_optim::candidate_events::{CandidateAdmission, provider_candidate_admission};
use implexity_optim::constraint_admission::committed_merit;
use implexity_optim::coordinate_bounds::{
    Bound, BoxMasks, array_bounds, coordinate_bounds, project_array_box,
};
use implexity_optim::design_freedom::{
    DesignBlock, FREE_ROLES, active_coordinates, freeze_inactive, validate_blocks,
};
use implexity_optim::numeric::float_value;
use implexity_optim::optimizer::{
    design_sensitivities, evaluation_response_values, legacy_settings_value, lifecycle,
};
use implexity_optim::provider_ops::{
    AdmissionReply, CandidateDesign, DesignOp, check_operating_point, design_operations, legacy_evaluate,
    provides,
};
use implexity_optim::regime::{ScheduleStage, validate_schedule};
use implexity_optim::search::{
    ExactPoint, ProjectedSearch, SearchError, SearchResult, SearchState, TERMINAL_REASONS, TrialFailure,
    TrialInfo, TrialValues,
};
use implexity_optim::{NamedArrays, design_identity};
use implexity_runtime::computation_effort::{ComputationEffortPolicy, ComputationMode};
use implexity_runtime::intent_orchestrated::IntentOrchestratedProvider;
use implexity_runtime::provider_job_authority::{
    ProviderFacts, attach_exact_effort_evidence, make_server_effort_binding,
    require_single_provider_schedule, runtime_source_binding, stable_package_identity,
    validate_effort_binding, validate_initial_control_input,
};
use implexity_solve::adaptive_scheduler::{AdaptivePolicy, assess};
use implexity_solve::approximation::{ApproximateEvaluation, ApproximationLane, ExactEvaluateFn};
use implexity_solve::matching_time_guess::{
    GuessError, MatchingTimeGuessStore, execution_identity, public_descriptor,
};
use ndarray::{ArrayD, IxDyn, Zip};
use serde_json::{Map, Value};

use crate::effort::physics_snapshot;
use crate::epoch_capture::{capture_cached_epoch, retain_epoch_sensitivities};
use crate::epoch_fields::EpochFieldArtifactAdapter;
use crate::error::{JobError, JobResult};
use crate::preview::{
    preview_policy, preview_ranked_candidates, preview_record, preview_wire, trial_progress,
};
use crate::private::{canonical_text, sha256_file, sha256_hex};
use crate::provider_job::{
    atomic_json, control, project_with_masks, strict_integer, strict_number, strict_text,
};
use crate::provider_worker::{
    emit_line, install_acknowledgement_ok, problem_json, provider_lifecycle_verifier,
};
use crate::stage_search::{
    StageCoordinate, StageResponseProgram, StageSearch, StageSearchHooks, normalise_update_metric, point_key,
};

pub const CHECKPOINT_SCHEMA: &str = "implexity-hierarchical-provider-checkpoint/4";
pub const HISTORY_SCHEMA: &str = "implexity-hierarchical-provider-history/4";
pub const RUN_IDENTITY_SCHEMA: &str = "implexity-hierarchical-provider-run-identity/1";

const HISTORY_REQUIRED_FIELDS: [&str; 49] = [
    "i",
    "iteration",
    "stage",
    "stage_index",
    "stage_iteration",
    "provider",
    "fidelity",
    "transition",
    "released_blocks",
    "active_coordinates",
    "operating_points",
    "robust_mode",
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
    "terms",
    "max_scaled_bound_violation",
    "bound_measure",
    "bound_feasible",
    "bound_multipliers",
    "multiplier_updates",
    "trial_evaluations",
    "armijo_rejections",
    "search_state",
    "push",
    "design_state_id",
    "coordinate_design_state_ids",
    "best_design_state_id",
    "best_history_index",
    "run_fingerprint",
    "runtime_source_sha256",
    "package_identity_sha256",
    "provider_descriptor_sha256",
    "provider_profile_sha256",
    "requested_policy_digest",
    "effective_effort_digest",
    "operation_context_digest",
    "solve_id",
    "document_content_id",
    "continuable",
    "coordinate_updates",
    "diagnostics",
];
const ENGINE_OPTIONAL_FIELDS: [&str; 8] = [
    "step_policy",
    "numerical_trial_rejections",
    "numerical_rejections",
    "last_numerical_trial_rejection",
    "non_descent_trials",
    "armijo",
    "accepted_value_max_relative_difference",
    "numerical_recovery",
];
const HISTORY_OPTIONAL_FIELDS: [&str; 6] =
    ["reason", "adaptive", "candidate_admission", "candidate_admission_attempts", "continuation_source", "diagnostic_responses"];

pub const CONTINUATION_SCHEMA: &str = "implexity-optimization-continuation/1";
pub const CONTINUATION_REPLAY_RELATIVE_TOLERANCE: f64 = 1e-6;
const HISTORY_FINITE_FIELDS: [&str; 10] = [
    "L",
    "objective",
    "L_first",
    "L_best",
    "base_objective",
    "gradient_norm",
    "step_fraction",
    "trust_step",
    "max_scaled_bound_violation",
    "bound_measure",
];
const BOUND_ROW_FIELDS: [&str; 11] = [
    "measure",
    "multiplier",
    "operating_point",
    "penalty",
    "response",
    "scale",
    "scaled_constraint",
    "scaled_violation",
    "sense",
    "target",
    "value",
];
const COORDINATE_UPDATE_FIELDS: [&str; 6] =
    ["max_abs", "l2", "free_entries", "projected_gradient_max_abs", "update_denominator", "step_scale"];
const IDENTITY_KEYS: [&str; 10] = [
    "run_fingerprint",
    "runtime_source_sha256",
    "package_identity_sha256",
    "provider_descriptor_sha256",
    "provider_profile_sha256",
    "requested_policy_digest",
    "effective_effort_digest",
    "operation_context_digest",
    "solve_id",
    "document_content_id",
];

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn repr(s: &str) -> String {
    implexity_core::py_repr::repr_str(s)
}

fn fv(x: f64) -> Value {
    float_value(x)
}

fn is_number(v: Option<&Value>) -> bool {
    v.is_some_and(Value::is_number)
}

fn is_int(v: Option<&Value>) -> bool {
    v.is_some_and(|v| v.is_i64() || v.is_u64())
}

fn num(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(f64::NAN)
}

fn finite_number(v: Option<&Value>) -> bool {
    is_number(v) && num(v).is_finite()
}

fn strings(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn ints(values: &[i64]) -> Value {
    Value::Array(values.iter().map(|v| Value::from(*v)).collect())
}

fn to_usize_points(points: &[i64]) -> Vec<usize> {
    points.iter().map(|p| usize::try_from(*p).unwrap_or(0)).collect()
}

fn bound_row(row: &Value) -> Value {
    let mut out = Map::new();
    for key in BOUND_ROW_FIELDS {
        let raw = row.get(key).cloned().unwrap_or(Value::Null);
        let value =
            if matches!(key, "response" | "operating_point" | "sense") { raw } else { fv(num(Some(&raw))) };
        out.insert(key.into(), value);
    }
    Value::Object(out)
}

fn merit(row: &Value) -> CaeResult<(i32, f64)> {
    match row.as_object() {
        Some(m) => committed_merit(m),
        None => contract("committed history row is malformed"),
    }
}


pub fn history_best_index(history: &[Value]) -> CaeResult<usize> {
    let mut best = 0;
    let mut best_merit = match history.first() {
        Some(row) => merit(row)?,
        None => return Ok(0),
    };
    for (index, row) in history.iter().enumerate().skip(1) {
        let m = merit(row)?;
        if m.0 < best_merit.0 || (m.0 == best_merit.0 && m.1 < best_merit.1) {
            best = index;
            best_merit = m;
        }
    }
    Ok(best)
}

fn provider_capabilities(
    provider: &dyn CaeProvider,
) -> CaeResult<(ProviderCapabilities, Vec<String>, Vec<String>, bool)> {
    let raw = provider.capabilities()?;
    let (coordinates, responses, sensitivities) = match &raw {
        ProviderCapabilities::Descriptor(d) => {
            (d.design_coordinates.clone(), d.responses.clone(), d.sensitivities)
        }
        ProviderCapabilities::Legacy(l) => {
            (l.base.design_coordinates.clone(), l.base.responses.clone(), l.base.sensitivities)
        }
        ProviderCapabilities::Mapping(_) => {
            return contract("provider capabilities must be a typed ProviderDescriptor; got dict");
        }
    };
    let unique: BTreeSet<&String> = coordinates.iter().collect();
    if coordinates.is_empty() || unique.len() != coordinates.len() {
        return contract("provider design_coordinates must be nonempty and unique");
    }
    Ok((raw, coordinates, responses, sensitivities))
}

fn require_provider_child_execution(capabilities: &ProviderCapabilities, label: &str) -> CaeResult<()> {
    match capabilities {
        ProviderCapabilities::Mapping(_) => {
            contract(format!("{label}: provider capabilities must be a typed ProviderDescriptor"))
        }
        ProviderCapabilities::Legacy(l) if l.execution != "array" => contract(format!(
            "{label}: legacy provider execution {} cannot run in the provider child",
            repr(&l.execution)
        )),
        _ => Ok(()),
    }
}

fn authoring_coordinates(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    capabilities: &ProviderCapabilities,
    supported: &[String],
) -> CaeResult<(Vec<String>, String)> {
    let hook = design_operations(provider).and_then(|o| o.authoring_design_coordinates(problem));
    let Some(raw) = hook else {
        let primary = match capabilities {
            ProviderCapabilities::Legacy(l) if !l.topology_coordinate.is_empty() => {
                l.topology_coordinate.clone()
            }
            _ => supported.first().cloned().unwrap_or_default(),
        };
        if !supported.contains(&primary) {
            return contract("provider primary design coordinate is not declared in design_coordinates");
        }
        return Ok((supported.to_vec(), primary));
    };
    let raw = raw?;
    let unique: BTreeSet<&String> = raw.iter().collect();
    if raw.is_empty()
        || raw.iter().any(String::is_empty)
        || unique.len() != raw.len()
        || raw.iter().any(|n| !supported.contains(n))
    {
        return contract("provider returned invalid problem-specific authoring coordinates");
    }
    let primary = raw[0].clone();
    Ok((raw, primary))
}

fn validate_design_request(
    provider: &dyn CaeProvider,
    coordinates: &[String],
    responses: &[ResponseSpec],
    problem: &ProviderProblem,
) -> CaeResult<()> {
    let (raw, supported, declared, sensitivities) = provider_capabilities(provider)?;
    let (supported, primary) = authoring_coordinates(provider, problem, &raw, &supported)?;
    if coordinates.is_empty() || coordinates.iter().any(|n| !supported.contains(n)) {
        return contract("job design coordinates are not explicitly supported by provider capabilities");
    }
    if !coordinates.contains(&primary) {
        return contract("provider primary design coordinate is absent from the job design");
    }
    let unique: BTreeSet<&String> = coordinates.iter().collect();
    if unique.len() != coordinates.len() {
        return contract("job design coordinates must be unique");
    }
    let names = implexity_optim::response_program::measurement_names(responses)?;
    let declared = implexity_optim::admitted_responses(provider, problem, &declared)?;
    if names.iter().any(|n| !declared.contains(n)) {
        return contract("job responses must be explicitly declared by provider capabilities");
    }
    if !sensitivities {
        return contract("provider explicitly declares that optimization sensitivities are unavailable");
    }
    Ok(())
}

#[must_use]
pub fn slot(name: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() { "coordinate".into() } else { trimmed.into() }
}

fn atomic_npz_members(path: &Path, members: &[(String, NpyArray)]) -> std::io::Result<()> {
    let refs: Vec<(&str, &NpyArray)> = members.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let bytes = implexity_io::npz::save(&refs).map_err(|e| std::io::Error::other(e.to_string()))?;
    implexity_io::atomic::write_atomic(path, &bytes).map_err(|e| std::io::Error::other(e.to_string()))
}


pub fn write_design(path: &Path, designs: &NamedArrays, solve_id: &str) -> JobResult<()> {
    let refs = designs.names();
    let slots: Vec<String> = refs.iter().map(|r| slot(r)).collect();
    let unique: BTreeSet<&String> = slots.iter().collect();
    if unique.len() != slots.len() {
        return Err(JobError::contract("design coordinate NPZ slots collide"));
    }
    let mut members: Vec<(String, NpyArray)> =
        designs.iter().zip(&slots).map(|((_, v), s)| (format!("p_{s}"), NpyArray::from_f64(v))).collect();
    members.push(("refs".into(), NpyArray::strings(&refs)));
    members.push(("slots".into(), NpyArray::strings(&slots)));
    members.push(("units".into(), NpyArray::strings(&vec!["-".to_string(); refs.len()])));
    members.push(("solve_id".into(), NpyArray::scalar_str(solve_id)));
    atomic_npz_members(path, &members)?;
    Ok(())
}

fn history_payload(history: &[Value]) -> Value {
    let mut m = Map::new();
    m.insert("schema".into(), Value::String(HISTORY_SCHEMA.into()));
    m.insert("history".into(), Value::Array(history.to_vec()));
    Value::Object(m)
}

#[must_use]
pub fn history_digest_of(history: &[Value]) -> String {
    history_digest(history)
}

fn history_digest(history: &[Value]) -> String {
    sha256_hex(canonical_text(&history_payload(history)).as_bytes())
}

fn is_sha256(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(crate::private::is_sha256)
}

fn push_ok(push: Option<&Value>, expected_file: Option<&str>) -> bool {
    push.and_then(Value::as_object).is_some_and(|p| {
        p.len() == 3
            && p.get("file")
                .is_some_and(|f| f.is_string() && expected_file.is_none_or(|e| f.as_str() == Some(e)))
            && is_int(p.get("bytes"))
            && p.get("bytes").and_then(Value::as_i64).is_some_and(|b| b >= 1)
            && is_sha256(p.get("sha256"))
    })
}


pub fn write_numerical_attention_checkpoint(
    out_dir: &Path,
    deviation: &Map<String, Value>,
) -> CaeResult<Map<String, Value>> {
    let history_path = out_dir.join("history.json");
    let checkpoint_path = out_dir.join("ckpt.npz");
    if !history_path.is_file() || !checkpoint_path.is_file() {
        return contract("numerical attention requires a committed checkpoint and history");
    }
    let payload: Value = std::fs::read_to_string(&history_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| CaeError::contract("numerical attention history is unreadable"))?;
    let history = match payload.as_object() {
        Some(p) if p.get("schema").and_then(Value::as_str) == Some(HISTORY_SCHEMA) => {
            match p.get("history") {
                Some(Value::Array(h)) if !h.is_empty() => h.clone(),
                _ => return contract("numerical attention requires nonempty canonical history"),
            }
        }
        _ => return contract("numerical attention requires nonempty canonical history"),
    };
    let last = history.last().cloned().unwrap_or(Value::Null);
    if last.get("accepted") != Some(&Value::Bool(true)) || last.get("continuable") != Some(&Value::Bool(true))
    {
        return contract("numerical attention requires an accepted continuable checkpoint");
    }
    let digest = history_digest(&history);
    let identity_fields = [
        "run_fingerprint",
        "runtime_source_sha256",
        "package_identity_sha256",
        "provider_descriptor_sha256",
        "provider_profile_sha256",
        "requested_policy_digest",
        "effective_effort_digest",
        "operation_context_digest",
        "solve_id",
        "document_content_id",
        "design_state_id",
    ];
    let archive = implexity_io::npz::load_file(&checkpoint_path)
        .map_err(|_| CaeError::contract("numerical attention checkpoint is unreadable"))?;
    if strict_text(&archive, "checkpoint_schema")? != CHECKPOINT_SCHEMA {
        return contract("numerical attention checkpoint schema is unsupported");
    }
    if usize::try_from(strict_integer(&archive, "history_length")?).ok() != Some(history.len()) {
        return contract("numerical attention checkpoint history length drifted");
    }
    if strict_text(&archive, "history_digest")? != digest {
        return contract("numerical attention checkpoint history digest drifted");
    }
    if strict_text(&archive, "continuation_state")? != "continue" {
        return contract("numerical attention checkpoint is not continuable");
    }
    let mut identity = Map::new();
    for key in identity_fields {
        identity.insert(key.into(), Value::String(strict_text(&archive, key)?));
    }
    for (key, value) in &identity {
        if last.get(key) != Some(value) {
            return contract(format!("numerical attention checkpoint {key} drifted"));
        }
    }
    let push = last.get("push").cloned().unwrap_or(Value::Null);
    if !push_ok(Some(&push), None) {
        return contract("numerical attention checkpoint snapshot reference is malformed");
    }
    let file = push["file"].as_str().unwrap_or("");
    let live_name = Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let live_path = out_dir.join(live_name);
    let Ok(meta) = std::fs::metadata(&live_path) else {
        return contract("numerical attention checkpoint snapshot is missing");
    };
    let actual = sha256_file(&live_path)
        .map_err(|_| CaeError::contract("numerical attention checkpoint snapshot is missing"))?;
    if Some(meta.len()) != push["bytes"].as_u64() || Some(actual.as_str()) != push["sha256"].as_str() {
        return contract("numerical attention checkpoint snapshot identity drifted");
    }
    let mut safe = Map::new();
    safe.insert("iteration".into(), Value::from(last.get("iteration").and_then(Value::as_i64).unwrap_or(0)));
    safe.insert("history_length".into(), Value::from(history.len()));
    safe.insert("history_digest".into(), Value::String(digest));
    safe.insert("provider".into(), Value::String(py_str(last.get("provider").unwrap_or(&Value::Null))));
    for (k, v) in identity {
        safe.insert(k, v);
    }
    safe.insert("push".into(), push);
    let mut evidence = Map::new();
    evidence.insert("schema".into(), Value::String("implexity-numerical-attention-checkpoint/1".into()));
    evidence.insert("event".into(), Value::String("numerical_certification_deviation".into()));
    evidence.insert("deviation".into(), Value::Object(deviation.clone()));
    evidence.insert("safe_checkpoint".into(), Value::Object(safe));
    let event_digest = sha256_hex(canonical_text(&Value::Object(evidence.clone())).as_bytes());
    evidence.insert("event_digest".into(), Value::String(event_digest));
    atomic_json(&out_dir.join("numerical_attention.json"), &Value::Object(evidence.clone()))
        .map_err(|e| CaeError::contract(e.to_string()))?;
    Ok(evidence)
}

fn coordinate_identities(designs: &NamedArrays) -> CaeResult<Map<String, Value>> {
    let mut out = Map::new();
    for (name, value) in designs.iter() {
        out.insert(name.into(), Value::String(design_identity(&NamedArrays::single(name, value.clone()))?));
    }
    Ok(out)
}

fn unique_nonempty_strings(value: &Value) -> bool {
    value.as_array().is_some_and(|items| {
        let set: BTreeSet<&str> = items.iter().filter_map(Value::as_str).collect();
        items.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())) && set.len() == items.len()
    })
}

fn completed_coupling_initialization(raw: &Value) -> CaeResult<Option<Value>> {
    if raw.is_null() {
        return Ok(None);
    }
    let Some(row) = raw.as_object() else {
        return contract("coupling initialization evidence must be an object or null");
    };
    let exact = row.get("schema").and_then(Value::as_str) == Some("implexity-optimization-preview-trace/1")
        && row.get("phase").and_then(Value::as_str) == Some("cold_staged_initializer")
        && row.get("authoritative") == Some(&Value::Bool(false))
        && row.get("correction_required") == Some(&Value::Bool(true))
        && row.get("available").is_some_and(Value::is_boolean)
        && row.get("exact_correction_completed") == Some(&Value::Bool(true))
        && row.get("result_truth_status").and_then(Value::as_str) == Some("exact")
        && row.get("active_coupling_state").and_then(Value::as_str) == Some("all_active");
    if !exact {
        return contract("coupling initialization evidence is not exact-restored");
    }
    for key in [
        "provider_laggable_coupling_ids",
        "requested_lagged_coupling_ids",
        "initially_lagged_coupling_ids",
        "lagged_coupling_ids",
        "inactive_coupling_ids",
    ] {
        if let Some(values) = row.get(key)
            && !unique_nonempty_strings(values)
        {
            return contract(format!("coupling initialization {} is malformed", repr(key)));
        }
    }
    let empty = Value::Array(Vec::new());
    if row.get("lagged_coupling_ids") != Some(&empty) || row.get("inactive_coupling_ids") != Some(&empty) {
        return contract("completed coupling initialization retained inactive coupling");
    }
    let final_step = row.get("restoration_schedule").and_then(Value::as_array).and_then(|s| s.last());
    let complete = final_step.and_then(Value::as_object).is_some_and(|f| {
        f.get("stage").and_then(Value::as_str) == Some("exact_correction")
            && f.get("lagged_coupling_ids") == Some(&empty)
            && f.get("inactive_coupling_ids") == Some(&empty)
            && f.get("completed") == Some(&Value::Bool(true))
    });
    if !complete {
        return contract("coupling initialization restoration schedule is incomplete");
    }
    if row.get("available") == Some(&Value::Bool(false))
        && (row.get("fallback").and_then(Value::as_str) != Some("exact_cold_start")
            || row.get("reason").and_then(Value::as_str).is_none_or(str::is_empty))
    {
        return contract("unavailable coupling initialization omitted its exact fallback");
    }
    implexity_core::wire::to_wire(raw)?;
    Ok(Some(raw.clone()))
}

fn unavailable_coupling_initialization(policy: &Value, reason: &str, points: &[i64]) -> Value {
    let requested =
        policy.get("lagged_coupling_ids").filter(|v| truthy(v)).cloned().unwrap_or(Value::Array(Vec::new()));
    serde_json::json!({
        "schema": "implexity-optimization-preview-trace/1",
        "phase": "cold_staged_initializer",
        "truth_status": "exact",
        "authoritative": false,
        "correction_required": true,
        "available": false,
        "fallback": "exact_cold_start",
        "reason": reason,
        "requested_coupling_preset": policy.get("preset").cloned().unwrap_or(Value::Null),
        "requested_lagged_coupling_ids": requested,
        "operating_points": ints(points),
        "lagged_coupling_ids": [],
        "inactive_coupling_ids": [],
        "restoration_schedule": [
            {"stage": "preview_initial_guess", "lagged_coupling_ids": [], "inactive_coupling_ids": [],
             "completed": false, "skipped": true, "reason": reason},
            {"stage": "exact_correction", "lagged_coupling_ids": [], "inactive_coupling_ids": [],
             "method": "selected_provider_authoritative_exact_solve", "completed": false},
        ],
        "exact_correction_completed": false,
        "active_coupling_state": "all_active",
    })
}

fn file_name_of(value: &Value) -> Option<String> {
    Some(value).filter(|v| truthy(v)).map(|v| {
        let text = py_str(v);
        Path::new(&text).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    })
}

#[must_use]
pub fn input_file_names(spec: &Map<String, Value>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(rows) = spec.get("design_coordinates").and_then(Value::as_array) {
        for row in rows {
            if let Some(n) = row.get("file").and_then(file_name_of) {
                names.insert(n);
            }
        }
    }
    if let Some(n) = spec.get("topology_file").and_then(file_name_of) {
        names.insert(n);
    }
    names
}

fn refuse_fresh_outputs(out_dir: &Path, spec: &Map<String, Value>) -> CaeResult<()> {
    validate_initial_control_input(out_dir)?;
    let inputs = input_file_names(spec);
    let mut fixed: BTreeSet<&str> = [
        "best.npz",
        "ckpt.npz",
        "history.json",
        "summary.json",
        "final.npz",
        "initial_design.json",
        "matching_time_guess_consumed.json",
        "numerical_attention.json",
        "numerical_attention_decision.json",
        "continuation.json",
        RESUME_WARM_START_FILES[0],
        RESUME_WARM_START_FILES[1],
        RESUME_WARM_START_FILES[2],
        RESUME_WARM_START_TEMPORARIES[0],
        RESUME_WARM_START_TEMPORARIES[1],
        RESUME_WARM_START_TEMPORARIES[2],
        "best.npz.tmp.npz",
        "ckpt.npz.tmp.npz",
        "history.json.tmp",
        "summary.json.tmp",
        "final.npz.tmp.npz",
        "initial_design.json.tmp",
        "matching_time_guess_consumed.json.tmp",
        "numerical_attention.json.tmp",
        "numerical_attention_decision.json.tmp",
    ]
    .into_iter()
    .collect();
    if !inputs.contains("initial.npz") {
        fixed.insert("initial.npz");
        fixed.insert("initial.npz.tmp.npz");
    }
    let mut stale = Vec::new();
    if let Ok(entries) = std::fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if fixed.contains(name.as_str())
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
            "fresh hierarchical provider job directory contains authoritative artifacts; use a new job directory or validated resume: {}",
            implexity_core::pyobj::list_repr(&stale)
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Bounds {
    raw: (Bound, Bound),
    lower: ArrayD<f64>,
    upper: ArrayD<f64>,
}

fn within(value: &ArrayD<f64>, b: &Bounds, slack: f64) -> bool {
    value
        .iter()
        .zip(b.lower.iter().zip(b.upper.iter()))
        .all(|(v, (lo, hi))| *v >= lo - slack && *v <= hi + slack)
}

fn strings_equal(archive: &implexity_io::npz::Npz, name: &str, expected: &[String]) -> bool {
    archive.get(name).is_some_and(|a| {
        a.shape == [expected.len()]
            && matches!(&a.data, NpyData::Unicode { values, .. } if values.iter().eq(expected.iter()))
    })
}

fn is_complex(a: &NpyArray) -> bool {
    matches!(a.data, NpyData::C64(_) | NpyData::C128(_))
}

fn load_design_snapshot(
    path: &Path,
    coords: &[String],
    shapes: &BTreeMap<String, Vec<usize>>,
    bounds: &BTreeMap<String, Bounds>,
    solve_id: &str,
) -> CaeResult<NamedArrays> {
    let name = repr(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let slots: Vec<String> = coords.iter().map(|c| slot(c)).collect();
    let unique: BTreeSet<&String> = slots.iter().collect();
    if unique.len() != slots.len() {
        return contract("design coordinate NPZ slots collide");
    }
    let archive = implexity_io::npz::load_file(path)
        .map_err(|_| CaeError::contract(format!("resume design snapshot {name} is unreadable")))?;
    let mut expected: BTreeSet<String> =
        ["refs", "slots", "units", "solve_id"].iter().map(|s| (*s).to_string()).collect();
    expected.extend(slots.iter().map(|s| format!("p_{s}")));
    let files: BTreeSet<String> = archive.files().into_iter().map(str::to_string).collect();
    if files != expected {
        return contract(format!("resume design snapshot {name} has an invalid layout"));
    }
    if !strings_equal(&archive, "refs", coords)
        || !strings_equal(&archive, "slots", &slots)
        || !strings_equal(&archive, "units", &vec!["-".to_string(); coords.len()])
        || strict_text(&archive, "solve_id")? != solve_id
    {
        return contract(format!("resume design snapshot {name} metadata drifted"));
    }
    let mut out = NamedArrays::new();
    for (coord, s) in coords.iter().zip(&slots) {
        let raw = archive
            .get(&format!("p_{s}"))
            .ok_or_else(|| CaeError::contract(format!("resume design snapshot {name} is unreadable")))?;
        if is_complex(raw) {
            return contract(format!("resume design snapshot {name} contains complex data"));
        }
        let Some(value) = raw.to_f64() else {
            return contract(format!("resume design snapshot {name} is not numeric"));
        };
        let b = &bounds[coord];
        if Some(&value.shape().to_vec()) != shapes.get(coord)
            || !value.iter().all(|v| v.is_finite())
            || !within(&value, b, 0.0)
        {
            return contract(format!("resume design snapshot {name} violates shape, finiteness, or bounds"));
        }
        out.insert(coord.clone(), value);
    }
    Ok(out)
}

type Cursor = (bool, usize, i64, String);

fn next_cursor(row: &Map<String, Value>, stages: &[ScheduleStage]) -> Cursor {
    let si = usize::try_from(row.get("stage_index").and_then(Value::as_i64).unwrap_or(0)).unwrap_or(0);
    let li = row.get("stage_iteration").and_then(Value::as_i64).unwrap_or(0);
    let leave = row.get("reason").is_some_and(|r| !r.is_null()) || li + 1 >= stages[si].iterations;
    if leave {
        if si + 1 < stages.len() {
            return (true, si + 1, 0, stages[si + 1].id.clone());
        }
        return (false, stages.len(), 0, "terminal".into());
    }
    (true, si, li + 1, stages[si].id.clone())
}

fn coordinate_step_scale(raw: Option<&Value>, name: &str) -> CaeResult<f64> {
    let label = if name.is_empty() { "design coordinate" } else { name };
    let Some(value) = raw.filter(|v| v.is_number()).and_then(Value::as_f64) else {
        return contract(format!("{label}: step_scale must be numeric"));
    };
    if !value.is_finite() || value <= 0.0 {
        return contract(format!("{label}: step_scale must be positive and finite"));
    }
    Ok(value)
}

struct Resumed {
    designs: NamedArrays,
    stage_index: usize,
    local_next: i64,
    termination_reason: String,
    history: Vec<Value>,
    first: f64,
    best: f64,
    best_design_state_id: String,
    best_history_index: usize,
    matching_time_consumed: Option<Value>,
    responses: Vec<ResponseSpec>,
    normalization_record: Option<Value>,
    coupling_initialization: Option<Value>,
}

struct ResumeInputs<'a> {
    coords: &'a [String],
    run_initial: &'a NamedArrays,
    bounds: &'a BTreeMap<String, Bounds>,
    coordinate_masks: &'a BTreeMap<String, ArrayD<bool>>,
    blocks: &'a [DesignBlock],
    stages: &'a [ScheduleStage],
    responses: &'a [ResponseSpec],
    response_normalization: Option<&'a Value>,
    problem_doc: Option<&'a Value>,
    primary: &'a str,
    solve_id: &'a str,
    identities: &'a Map<String, Value>,
    settings: &'a CoordinateOptimizationSettings,
    step_scales: &'a BTreeMap<String, f64>,
}

fn parse_canonical(text: &str, what: &str) -> CaeResult<Value> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| CaeError::contract(format!("resume checkpoint {what} is malformed")))?;
    if canonical_text(&value) != text {
        return contract(format!("resume checkpoint {what} is not canonical"));
    }
    Ok(value)
}

fn eq_arrays(a: &NamedArrays, b: &NamedArrays, coords: &[String]) -> bool {
    coords.iter().all(|n| a.get(n) == b.get(n))
}

fn l2(values: &ArrayD<f64>) -> f64 {
    if values.len() < 16 {
        values.iter().fold(0.0_f64, |s, v| v.mul_add(*v, s)).sqrt()
    } else {
        values.iter().map(|v| v * v).sum::<f64>().sqrt()
    }
}

fn max_abs(values: &ArrayD<f64>) -> f64 {
    values.iter().fold(0.0_f64, |m, v| m.max(v.abs()))
}

#[allow(clippy::float_cmp)]
fn norm_matches(recorded: f64, recomputed: f64) -> bool {
    recorded == recomputed
        || (recorded - recomputed).abs() <= 64.0 * f64::EPSILON * recorded.abs().max(recomputed.abs())
}

#[allow(clippy::too_many_lines, clippy::float_cmp)]
fn load_resume_generation(
    ckpt: &Path,
    history_path: &Path,
    out_dir: &Path,
    r: &ResumeInputs<'_>,
    admit_terminal: bool,
) -> CaeResult<Resumed> {
    let coords = r.coords;
    let stages = r.stages;
    let mut required: BTreeSet<String> = [
        "checkpoint_schema",
        "stage_index",
        "stage_id",
        "local_next",
        "global_next",
        "continuation_state",
        "termination_reason",
        "L_first",
        "L_best",
        "run_fingerprint",
        "runtime_source_sha256",
        "package_identity_sha256",
        "provider_descriptor_sha256",
        "provider_profile_sha256",
        "requested_policy_digest",
        "effective_effort_digest",
        "operation_context_digest",
        "solve_id",
        "document_content_id",
        "initial_design_state_id",
        "design_state_id",
        "initial_coordinate_design_state_ids",
        "coordinate_design_state_ids",
        "best_design_state_id",
        "best_history_index",
        "history_length",
        "history_digest",
        "matching_time_guess_consumed",
        "response_normalization",
        "coupling_initialization",
        "search_state",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    required.extend(coords.iter().map(|c| format!("p_{}", slot(c))));
    let archive = implexity_io::npz::load_file(ckpt)
        .map_err(|_| CaeError::contract("resume checkpoint is unreadable"))?;
    let fields: BTreeSet<String> = archive.files().into_iter().map(str::to_string).collect();
    if !required.is_subset(&fields) {
        return contract(
            "legacy hierarchical checkpoint lacks authoritative resume identity; start a fresh job",
        );
    }
    if let Some(order) = archive.get("coordinate_order") {
        required.insert("coordinate_order".into());
        if order.shape != vec![coords.len()]
            || !matches!(&order.data, NpyData::Unicode { values, .. } if values == coords)
        {
            return contract("resume checkpoint coordinate order drifted");
        }
    }
    if fields != required {
        return contract("resume checkpoint contains undeclared hierarchical state");
    }
    if strict_text(&archive, "checkpoint_schema")? != CHECKPOINT_SCHEMA {
        return contract("resume checkpoint schema is unsupported");
    }
    let mut stored: BTreeMap<&str, String> = BTreeMap::new();
    for key in [
        "run_fingerprint",
        "runtime_source_sha256",
        "package_identity_sha256",
        "provider_descriptor_sha256",
        "provider_profile_sha256",
        "requested_policy_digest",
        "effective_effort_digest",
        "operation_context_digest",
        "solve_id",
        "document_content_id",
        "initial_design_state_id",
        "design_state_id",
        "initial_coordinate_design_state_ids",
        "coordinate_design_state_ids",
        "best_design_state_id",
        "history_digest",
        "matching_time_guess_consumed",
        "response_normalization",
        "coupling_initialization",
        "stage_id",
        "continuation_state",
        "termination_reason",
        "search_state",
    ] {
        stored.insert(key, strict_text(&archive, key)?);
    }
    let start_stage = strict_integer(&archive, "stage_index")?;
    let start_local = strict_integer(&archive, "local_next")?;
    let global_next = strict_integer(&archive, "global_next")?;
    let history_length = strict_integer(&archive, "history_length")?;
    let best_index = strict_integer(&archive, "best_history_index")?;
    let first = strict_number(&archive, "L_first")?;
    let best = strict_number(&archive, "L_best")?;
    for (key, expected) in r.identities {
        if stored.get(key.as_str()).map(|s| Value::String(s.clone())).as_ref() != Some(expected) {
            return contract(format!("resume checkpoint {} drifted", key.replace('_', " ")));
        }
    }
    if stored["solve_id"] != r.solve_id {
        return contract("resume checkpoint solve identity drifted");
    }
    let normalization_record = parse_canonical(&stored["response_normalization"], "response normalization")?;
    let (responses, normalization_record) =
        restore_response_normalization(r.responses, r.response_normalization, Some(&normalization_record))?;
    let coupling = parse_canonical(&stored["coupling_initialization"], "coupling initialization")?;
    let coupling_initialization = completed_coupling_initialization(&coupling)?;
    let continuation = stored["continuation_state"].as_str();
    if continuation != "continue" && continuation != "terminal" {
        return contract("resume checkpoint continuation state is malformed");
    }
    if global_next != history_length || history_length < 1 || best_index < 0 || best_index >= history_length {
        return contract("resume checkpoint history cursor is malformed");
    }
    let stage_count = i64::try_from(stages.len()).unwrap_or(i64::MAX);
    if start_stage < 0 || start_stage > stage_count || start_local < 0 {
        return contract("resume checkpoint schedule cursor is outside the declared budget");
    }
    let start_stage_u = usize::try_from(start_stage).unwrap_or(usize::MAX);
    if continuation == "terminal" {
        if start_stage != stage_count || start_local != 0 || stored["stage_id"] != "terminal" {
            return contract("resume checkpoint terminal cursor is malformed");
        }
    } else if start_stage >= stage_count
        || start_local >= stages[start_stage_u].iterations
        || stored["stage_id"] != stages[start_stage_u].id
    {
        return contract("resume checkpoint schedule cursor is malformed");
    }

    let mut designs = NamedArrays::new();
    for coord in coords {
        let raw = archive
            .get(&format!("p_{}", slot(coord)))
            .ok_or_else(|| CaeError::contract("resume checkpoint is unreadable"))?;
        if is_complex(raw) {
            return contract("resume checkpoint design contains complex data");
        }
        let Some(value) = raw.to_f64() else {
            return contract("resume checkpoint design is not numeric");
        };
        let initial =
            r.run_initial.get(coord).ok_or_else(|| CaeError::contract("resume checkpoint is unreadable"))?;
        if value.shape() != initial.shape()
            || !value.iter().all(|v| v.is_finite())
            || !within(&value, &r.bounds[coord], 0.0)
        {
            return contract("resume checkpoint violates coordinate shape, finiteness, or bounds");
        }
        if let Some(mask) = r.coordinate_masks.get(coord)
            && value.iter().zip(initial.iter()).zip(mask.iter()).any(|((v, i), m)| !*m && v != i)
        {
            return contract("resume checkpoint altered fixed entries");
        }
        designs.insert(coord.clone(), value);
    }
    if stored["initial_design_state_id"] != design_identity(r.run_initial)? {
        return contract("resume checkpoint initial design identity drifted");
    }
    let parse_ids = |text: &str| -> CaeResult<Value> {
        serde_json::from_str(text)
            .map_err(|_| CaeError::contract("resume checkpoint coordinate identities are malformed"))
    };
    let initial_ids = parse_ids(&stored["initial_coordinate_design_state_ids"])?;
    let coordinate_ids = parse_ids(&stored["coordinate_design_state_ids"])?;
    if canonical_text(&initial_ids) != stored["initial_coordinate_design_state_ids"]
        || canonical_text(&coordinate_ids) != stored["coordinate_design_state_ids"]
        || initial_ids != Value::Object(coordinate_identities(r.run_initial)?)
        || coordinate_ids != Value::Object(coordinate_identities(&designs)?)
        || stored["design_state_id"] != design_identity(&designs)?
    {
        return contract("resume checkpoint coordinate design identity drifted");
    }

    if !history_path.is_file() {
        return contract("resume history sidecar is missing");
    }
    let payload: Value = std::fs::read_to_string(history_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| CaeError::contract("resume history sidecar is unreadable"))?;
    let history: Vec<Value> = match payload.as_object() {
        Some(p) if p.len() == 2 && p.get("schema").and_then(Value::as_str) == Some(HISTORY_SCHEMA) => {
            match p.get("history") {
                Some(Value::Array(h)) => h.clone(),
                _ => return contract("resume history sidecar schema is malformed"),
            }
        }
        _ => return contract("resume history sidecar schema is malformed"),
    };
    if i64::try_from(history.len()).ok() != Some(history_length) {
        return contract("resume history length disagrees with the checkpoint commit marker");
    }
    if history_digest(&history) != stored["history_digest"] {
        return contract("resume history digest drifted");
    }
    let expected_live: BTreeSet<String> = (0..history.len()).map(|i| format!("live_{i:06}.npz")).collect();
    let actual_live: BTreeSet<String> = std::fs::read_dir(out_dir)
        .map_err(|_| CaeError::contract("resume output directory is unreadable"))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("live_") && (n.ends_with(".npz") || n.ends_with(".tmp.npz")))
        .collect();
    if actual_live != expected_live {
        return contract("resume live snapshot generation is incomplete or contains stale artifacts");
    }
    let incomplete = [
        "ckpt.npz.tmp.npz",
        "history.json.tmp",
        "best.npz.tmp.npz",
        "initial.npz.tmp.npz",
        "initial_design.json.tmp",
        RESUME_WARM_START_TEMPORARIES[0],
        RESUME_WARM_START_TEMPORARIES[1],
        RESUME_WARM_START_TEMPORARIES[2],
    ];
    if incomplete.iter().any(|n| out_dir.join(n).exists()) {
        return contract("resume generation contains incomplete temporary artifacts");
    }
    if continuation == "continue" && ["summary.json", "final.npz"].iter().any(|n| out_dir.join(n).exists()) {
        return contract("continuable resume generation contains stale terminal artifacts");
    }
    let shapes: BTreeMap<String, Vec<usize>> =
        r.run_initial.iter().map(|(k, v)| (k.to_string(), v.shape().to_vec())).collect();
    let initial_path = out_dir.join("initial.npz");
    if !initial_path.is_file() {
        return contract("resume initial design snapshot is missing");
    }
    let initial_snapshot = load_design_snapshot(&initial_path, coords, &shapes, r.bounds, r.solve_id)?;
    if design_identity(&initial_snapshot)? != design_identity(r.run_initial)?
        || !eq_arrays(&initial_snapshot, r.run_initial, coords)
    {
        return contract("resume initial design snapshot identity drifted");
    }
    let sidecar: Value = std::fs::read_to_string(out_dir.join("initial_design.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| {
            CaeError::contract("resume initial design identity sidecar is missing or unreadable")
        })?;
    let mut expected_sidecar = Map::new();
    expected_sidecar.insert("schema".into(), Value::String("implexity-hierarchical-initial-design/1".into()));
    expected_sidecar
        .insert("design_state_id".into(), Value::String(stored["initial_design_state_id"].clone()));
    expected_sidecar
        .insert("coordinate_design_state_ids".into(), Value::Object(coordinate_identities(r.run_initial)?));
    for (k, v) in r.identities {
        expected_sidecar.insert(k.clone(), v.clone());
    }
    if sidecar != Value::Object(expected_sidecar) {
        return contract("resume initial design identity sidecar drifted");
    }

    let settings = r.settings;
    let bound_tolerance = settings.bound_tolerance.as_f64();
    let mut previous = r.run_initial.clone();
    let mut live_values: Vec<NamedArrays> = Vec::new();
    let mut expected_termination = "iteration_limit".to_string();
    let mut expected_cursor: Cursor = (true, 0, 0, stages[0].id.clone());
    let mut programs: BTreeMap<usize, StageResponseProgram> = BTreeMap::new();
    let required_fields: BTreeSet<&str> = HISTORY_REQUIRED_FIELDS.iter().copied().collect();
    let optional_fields: BTreeSet<&str> =
        HISTORY_OPTIONAL_FIELDS.iter().chain(ENGINE_OPTIONAL_FIELDS.iter()).copied().collect();
    for (index, row_value) in history.iter().enumerate() {
        let Some(row) = row_value.as_object() else {
            return contract("resume history row is malformed");
        };
        if !required_fields.iter().all(|k| row.contains_key(*k))
            || row
                .keys()
                .any(|k| !required_fields.contains(k.as_str()) && !optional_fields.contains(k.as_str()))
        {
            return contract("resume history row schema is malformed");
        }
        if let Some(raw) = row.get("diagnostic_responses") {
            let values = raw.as_array().ok_or_else(|| CaeError::contract("resume diagnostic responses are malformed"))?;
            let mut seen = BTreeSet::new();
            for value in values {
                let valid = value.as_object().is_some_and(|v| {
                    v.keys().all(|k| ["response", "value", "operating_point", "units"].contains(&k.as_str()))
                        && v.get("units").is_none_or(Value::is_string)
                        && v.get("response").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                        && v.get("value").and_then(Value::as_f64).is_some_and(f64::is_finite)
                        && v.get("operating_point").and_then(Value::as_i64).is_some_and(|i| i >= 0)
                });
                if !valid || !seen.insert((value["response"].as_str().unwrap_or_default(), value["operating_point"].as_i64())) {
                    return contract("resume diagnostic responses are malformed");
                }
            }
        }
        let index_i = i64::try_from(index).unwrap_or(i64::MAX);
        if !is_int(row.get("i"))
            || !is_int(row.get("iteration"))
            || row["i"].as_i64() != Some(index_i)
            || row["iteration"].as_i64() != Some(index_i)
            || !is_int(row.get("stage_index"))
            || !is_int(row.get("stage_iteration"))
        {
            return contract("resume history iteration sequence is malformed");
        }
        let (state, expected_si, expected_li, _) = expected_cursor.clone();
        if !state
            || row["stage_index"].as_i64() != i64::try_from(expected_si).ok()
            || row["stage_iteration"].as_i64() != Some(expected_li)
        {
            return contract("resume history schedule sequence is malformed");
        }
        let stage = &stages[expected_si];
        if let std::collections::btree_map::Entry::Vacant(e) = programs.entry(expected_si) {
            e.insert(StageResponseProgram::new(
                &stage.stage_responses(&responses)?,
                &stage.operating_points,
                &stage.robust_mode,
                stage.robust_beta,
            )?);
        }
        let program = &programs[&expected_si];
        let expected_active = active_coordinates(r.blocks, Some(&stage.released_blocks), coords)?;
        if row.get("stage").and_then(Value::as_str) != Some(stage.id.as_str())
            || row.get("provider").and_then(Value::as_str) != Some(stage.provider.as_str())
            || row.get("fidelity").and_then(Value::as_str) != Some(stage.fidelity.as_str())
            || row.get("transition").and_then(Value::as_str) != Some(stage.transition.as_str())
            || row.get("released_blocks") != Some(&strings(&stage.released_blocks))
            || row.get("active_coordinates") != Some(&strings(&expected_active))
            || row.get("operating_points") != Some(&ints(&stage.operating_points))
            || row.get("robust_mode").and_then(Value::as_str) != Some(stage.robust_mode.as_str())
        {
            return contract("resume history stage identity drifted");
        }
        let step_fraction = num(row.get("step_fraction"));
        let objective_ok = row.get("accepted").is_some_and(Value::is_boolean)
            && row.get("continuable").is_some_and(Value::is_boolean)
            && row.get("bound_feasible").is_some_and(Value::is_boolean)
            && HISTORY_FINITE_FIELDS.iter().all(|k| finite_number(row.get(*k)))
            && num(row.get("gradient_norm")) >= 0.0
            && (0.0..=1.0).contains(&step_fraction)
            && num(row.get("L")) == num(row.get("objective"))
            && num(row.get("L_first")) == first
            && is_int(row.get("trial_evaluations"))
            && row["trial_evaluations"].as_i64().is_some_and(|v| v >= 0)
            && is_int(row.get("armijo_rejections"))
            && row["armijo_rejections"].as_i64().is_some_and(|v| v >= 0)
            && row.get("multiplier_updates").is_some_and(Value::is_array)
            && row.get("stationarity").is_some_and(Value::is_object)
            && row.get("terms").is_some_and(Value::is_array)
            && row.get("diagnostics").is_some_and(Value::is_object);
        if !objective_ok {
            return contract("resume history objective record is malformed");
        }
        let stationarity = row["stationarity"].as_object().cloned().unwrap_or_default();
        let keys: BTreeSet<&str> = stationarity.keys().map(String::as_str).collect();
        let expected_keys: BTreeSet<&str> =
            ["projected_gradient_max_norm", "reference", "relative", "tolerance", "satisfied"]
                .into_iter()
                .collect();
        if keys != expected_keys
            || num(stationarity.get("projected_gradient_max_norm")) != num(row.get("gradient_norm"))
            || num(stationarity.get("tolerance")) != settings.stationarity_tolerance.as_f64()
            || !stationarity.get("satisfied").is_some_and(Value::is_boolean)
        {
            return contract("resume history stationarity record is malformed");
        }
        let row_state = ProjectedSearch::state_from_wire(
            row.get("search_state").unwrap_or(&Value::Null),
            &program.expanded,
            settings,
        )
        .map_err(|e| {
            CaeError::contract(format!("resume history search state is malformed: {}", e.message()))
        })?;
        if row.get("step_policy").is_some_and(|v| v.as_str() != Some(row_state.step_policy.as_str()))
            || (row["search_state"]["schema"].as_str() == Some(implexity_optim::optimizer::SEARCH_STATE_SCHEMA) && row.get("step_policy").is_none()) {
            return contract("resume history step policy disagrees with checkpoint");
        }
        if num(row.get("trust_step")) != row_state.trust_step {
            return contract("resume history trust step disagrees with its search state");
        }
        let expected_terms: Vec<(String, i64)> =
            program.points.iter().flat_map(|p| program.names.iter().map(move |n| (n.clone(), *p))).collect();
        let mut observed_terms = Vec::new();
        for term in row["terms"].as_array().cloned().unwrap_or_default() {
            let ok = term.as_object().is_some_and(|t| {
                t.len() == 4
                    && t.get("response").is_some_and(Value::is_string)
                    && is_int(t.get("operating_point"))
                    && finite_number(t.get("value"))
                    && finite_number(t.get("objective_contribution"))
                    && t.contains_key("value")
                    && t.contains_key("objective_contribution")
            });
            if !ok {
                return contract("resume history response term is malformed");
            }
            observed_terms.push((
                term["response"].as_str().unwrap_or("").to_string(),
                term["operating_point"].as_i64().unwrap_or(-1),
            ));
        }
        if observed_terms != expected_terms {
            return contract("resume history response term sequence drifted");
        }
        let expected_bounds: Vec<(String, i64, String)> = program
            .points
            .iter()
            .flat_map(|p| {
                program
                    .responses
                    .iter()
                    .filter(|s| BOUND_SENSES.contains(&s.sense.as_str()))
                    .map(move |s| (s.name.clone(), *p, s.sense.clone()))
            })
            .collect();
        let bound_rows = match row.get("bound_multipliers") {
            Some(Value::Array(rows)) => rows.clone(),
            _ => return contract("resume history bound multiplier record is malformed"),
        };
        let observed_bounds: Vec<(String, i64, String)> = bound_rows
            .iter()
            .filter(|b| b.is_object())
            .map(|b| {
                (
                    b.get("response").and_then(Value::as_str).unwrap_or("").to_string(),
                    b.get("operating_point").and_then(Value::as_i64).unwrap_or(-1),
                    b.get("sense").and_then(Value::as_str).unwrap_or("").to_string(),
                )
            })
            .collect();
        let bound_field_set: BTreeSet<&str> = BOUND_ROW_FIELDS.iter().copied().collect();
        let rows_ok = bound_rows.iter().all(|b| {
            b.as_object().is_some_and(|m| {
                m.keys().map(String::as_str).collect::<BTreeSet<_>>() == bound_field_set
                    && BOUND_ROW_FIELDS
                        .iter()
                        .filter(|k| !matches!(**k, "response" | "operating_point" | "sense"))
                        .all(|k| finite_number(m.get(*k)))
            })
        });
        if observed_bounds != expected_bounds || !rows_ok {
            return contract("resume history bound multiplier record is malformed");
        }
        let bound_state = &row_state.bound_state;
        let multipliers: Vec<f64> = bound_rows.iter().map(|b| num(b.get("multiplier"))).collect();
        let penalties: Vec<f64> = bound_rows.iter().map(|b| num(b.get("penalty"))).collect();
        let max_violation = bound_rows
            .iter()
            .map(|b| num(b.get("scaled_violation")))
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
            .unwrap_or(0.0);
        let max_measure = bound_rows
            .iter()
            .map(|b| num(b.get("measure")))
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
            .unwrap_or(0.0);
        if multipliers != bound_state.multipliers
            || penalties != bound_state.penalties
            || num(row.get("max_scaled_bound_violation")) != max_violation
            || num(row.get("bound_measure")) != max_measure
            || row["bound_feasible"].as_bool()
                != Some(num(row.get("max_scaled_bound_violation")) <= bound_tolerance)
        {
            return contract("resume history bound record disagrees with its search state");
        }
        let reason = row.get("reason").filter(|v| !v.is_null());
        if let Some(r) = reason
            && r.as_str().is_none_or(str::is_empty)
        {
            return contract("resume history termination reason is malformed");
        }
        let accepted = row["accepted"].as_bool() == Some(true);
        if (!accepted && step_fraction != 0.0) || (accepted && step_fraction <= 0.0) {
            return contract("resume history acceptance record is malformed");
        }
        for (key, expected) in r.identities {
            if row.get(key) != Some(expected) {
                return contract("resume history execution identity drifted");
            }
        }
        if !is_int(row.get("best_history_index"))
            || !row.get("design_state_id").is_some_and(Value::is_string)
            || !row.get("best_design_state_id").is_some_and(Value::is_string)
            || !row.get("coordinate_design_state_ids").is_some_and(Value::is_object)
        {
            return contract("resume history design identity is malformed");
        }
        let expected_name = format!("live_{index:06}.npz");
        if !push_ok(row.get("push"), Some(&expected_name)) {
            return contract("resume history live snapshot reference is malformed");
        }
        let live_path = out_dir.join(&expected_name);
        let (Ok(meta), Ok(live_sha)) = (std::fs::metadata(&live_path), sha256_file(&live_path)) else {
            return contract(format!("resume live snapshot {} is missing", repr(&expected_name)));
        };
        if Some(meta.len()) != row["push"]["bytes"].as_u64()
            || Some(live_sha.as_str()) != row["push"]["sha256"].as_str()
        {
            return contract(format!("resume live snapshot {} content drifted", repr(&expected_name)));
        }
        let live = load_design_snapshot(&live_path, coords, &shapes, r.bounds, r.solve_id)?;
        let live_id = design_identity(&live)?;
        if row["design_state_id"].as_str() != Some(live_id.as_str())
            || row.get("coordinate_design_state_ids") != Some(&Value::Object(coordinate_identities(&live)?))
        {
            return contract("resume history live design identity drifted");
        }
        let raw_admission = row.get("candidate_admission");
        let raw_attempts = row.get("candidate_admission_attempts");
        if let Some(a) = raw_attempts
            && !(is_int(Some(a)) && a.as_i64().is_some_and(|v| v >= 1))
        {
            return contract("resume history candidate-admission attempt count is malformed");
        }
        if raw_admission.is_some() != raw_attempts.is_some() {
            return contract("resume history candidate-admission record is incomplete");
        }
        if let Some(raw) = raw_admission {
            let previous_id = design_identity(&previous)?;
            let admission = CandidateAdmission::from_any(
                &AdmissionReply::Record(raw.clone()),
                Some(&previous_id),
                if accepted { Some(live_id.as_str()) } else { None },
                true,
            )?;
            if accepted && !admission.allow {
                return contract("resume history accepted a provider-rejected candidate");
            }
        }
        let unchanged = eq_arrays(&live, &previous, coords);
        if !accepted && !unchanged {
            return contract("resume history committed a rejected candidate");
        }
        if accepted && unchanged {
            return contract("resume history accepted an unchanged candidate");
        }
        let row_best = history_best_index(&history[..=index])?;
        if row["best_history_index"].as_u64() != u64::try_from(row_best).ok()
            || num(row.get("L_best")) != num(history[row_best].get("L"))
            || row.get("best_design_state_id") != history[row_best].get("design_state_id")
        {
            return contract("resume history best-design sequence drifted");
        }
        let Some(updates) = row
            .get("coordinate_updates")
            .and_then(Value::as_object)
            .filter(|u| u.len() == coords.len() && coords.iter().all(|c| u.contains_key(c)))
        else {
            return contract("resume history coordinate update record is malformed");
        };
        let update_fields: BTreeSet<&str> = COORDINATE_UPDATE_FIELDS.iter().copied().collect();
        let mut projected_max = 0.0_f64;
        for name in coords {
            let update = &updates[name];
            let ok = update.as_object().is_some_and(|u| {
                u.keys().map(String::as_str).collect::<BTreeSet<_>>() == update_fields
                    && is_int(u.get("free_entries"))
                    && COORDINATE_UPDATE_FIELDS
                        .iter()
                        .filter(|k| **k != "free_entries")
                        .all(|k| finite_number(u.get(*k)) && num(u.get(*k)) >= 0.0)
            });
            if !ok {
                return contract("resume history coordinate update record is malformed");
            }
            let row_step_scale = coordinate_step_scale(update.get("step_scale"), name)?;
            if r.step_scales.get(name).is_some_and(|s| *s != row_step_scale) {
                return contract("resume history coordinate step scale drifted");
            }
            let (Some(l), Some(p)) = (live.get(name), previous.get(name)) else {
                return contract("resume history coordinate update record is malformed");
            };
            let delta = l - p;
            let expected_free =
                r.coordinate_masks.get(name).map_or(l.len(), |m| m.iter().filter(|v| **v).count());
            let inactive = !expected_active.contains(name);
            if num(update.get("max_abs")) != max_abs(&delta)
                || !norm_matches(num(update.get("l2")), l2(&delta))
                || update["free_entries"].as_u64() != u64::try_from(expected_free).ok()
                || (inactive
                    && (num(update.get("projected_gradient_max_abs")) != 0.0
                        || num(update.get("update_denominator")) != 0.0))
            {
                return contract("resume history coordinate update evidence drifted");
            }
            projected_max = projected_max.max(num(update.get("projected_gradient_max_abs")));
        }
        if projected_max != num(row.get("gradient_norm")) {
            return contract("resume history gradient norm evidence drifted");
        }
        let adaptive = row.get("adaptive");
        if let Some(policy) = stage.adaptive.as_ref() {
            let mut rows: Vec<Value> = history[..index]
                .iter()
                .filter(|h| h.get("stage").and_then(Value::as_str) == Some(stage.id.as_str()))
                .cloned()
                .collect();
            rows.push(row_value.clone());
            let topology: Vec<f64> =
                live.get(r.primary).map(|a| a.iter().copied().collect()).unwrap_or_default();
            let mut expected_adaptive = assess(
                &rows,
                &row["diagnostics"],
                &topology,
                &AdaptivePolicy::from_dict(Some(&Value::Object(policy.clone())))?,
                r.problem_doc,
                &implexity_runtime::multiphysics_monitor::assess_multiphysics,
            )?;
            if expected_adaptive.get("ready") == Some(&Value::Bool(true))
                && expected_si + 1 < stages.len()
                && let Value::Object(m) = &mut expected_adaptive
            {
                m.insert("next_stage".into(), Value::String(stages[expected_si + 1].id.clone()));
            }
            if adaptive != Some(&expected_adaptive) {
                return contract("resume history adaptive evidence drifted");
            }
        } else if adaptive.is_some() {
            return contract("resume history contains undeclared adaptive evidence");
        }
        let adaptive_transition = adaptive.and_then(|a| a.get("authorized")) == Some(&Value::Bool(true))
            && expected_si + 1 < stages.len();
        let reason_text = reason.and_then(Value::as_str);
        let updates_nonempty = row["multiplier_updates"].as_array().is_some_and(|u| !u.is_empty());
        if (adaptive_transition && reason_text != Some("adaptive_transition"))
            || (!adaptive_transition && accepted && reason.is_some())
            || (!adaptive_transition
                && !accepted
                && !reason_text.is_some_and(|s| TERMINAL_REASONS.contains(&s))
                && !(reason.is_none() && updates_nonempty))
        {
            return contract("resume history termination reason is malformed");
        }
        expected_cursor = next_cursor(row, stages);
        if row["continuable"].as_bool() != Some(expected_cursor.0) {
            return contract("resume history continuation state is malformed");
        }
        expected_termination = reason_text.map_or_else(|| "iteration_limit".to_string(), str::to_string);
        live_values.push(live.clone());
        previous = live;
    }
    let stored_cursor: Cursor =
        (continuation == "continue", start_stage_u, start_local, stored["stage_id"].clone());
    if expected_cursor != stored_cursor {
        return contract("resume checkpoint cursor disagrees with committed history");
    }
    if stored["termination_reason"] != expected_termination {
        return contract("resume checkpoint termination reason drifted");
    }
    let running_best = history_best_index(&history)?;
    let best_index_u = usize::try_from(best_index).unwrap_or(usize::MAX);
    if best != num(history[running_best].get("L"))
        || best_index_u != running_best
        || Some(stored["best_design_state_id"].as_str())
            != history[best_index_u].get("design_state_id").and_then(Value::as_str)
    {
        return contract("resume checkpoint best-design identity drifted");
    }
    let stored_search_state: Value = serde_json::from_str(&stored["search_state"])
        .map_err(|_| CaeError::contract("resume checkpoint search state is malformed"))?;
    let last = history.last().cloned().unwrap_or(Value::Null);
    if canonical_text(&stored_search_state) != stored["search_state"]
        || Some(&stored_search_state) != last.get("search_state")
    {
        return contract("resume checkpoint search state disagrees with history");
    }
    let final_live = live_values.last().cloned().unwrap_or_default();
    if Some(stored["design_state_id"].as_str()) != last.get("design_state_id").and_then(Value::as_str)
        || !eq_arrays(&designs, &final_live, coords)
    {
        return contract("resume checkpoint disagrees with final live design");
    }
    let best_path = out_dir.join("best.npz");
    if !best_path.is_file() {
        return contract("resume best design snapshot is missing");
    }
    let best_snapshot = load_design_snapshot(&best_path, coords, &shapes, r.bounds, r.solve_id)?;
    if design_identity(&best_snapshot)? != stored["best_design_state_id"]
        || !eq_arrays(&best_snapshot, &live_values[best_index_u], coords)
    {
        return contract("resume best design snapshot identity drifted");
    }
    let consumed = parse_canonical(&stored["matching_time_guess_consumed"], "matching-time evidence")
        .map_err(|e| {
            if e.message().ends_with("is not canonical") {
                CaeError::contract("resume checkpoint matching-time evidence is not canonical")
            } else {
                CaeError::contract("resume checkpoint matching-time evidence is malformed")
            }
        })?;
    let consumed = if consumed.is_null() {
        None
    } else {
        Some(Value::Object(public_descriptor(&consumed).map_err(|e| CaeError::contract(e.to_string()))?))
    };

    if continuation == "terminal" && !admit_terminal {
        return contract("terminal hierarchical checkpoint cannot be resumed");
    }
    Ok(Resumed {
        designs,
        stage_index: start_stage_u,
        local_next: start_local,
        termination_reason: stored["termination_reason"].clone(),
        history,
        first,
        best,
        best_design_state_id: stored["best_design_state_id"].clone(),
        best_history_index: best_index_u,
        matching_time_consumed: consumed,
        responses,
        normalization_record,
        coupling_initialization,
    })
}

struct ContinuationRequest {
    source_dir: PathBuf,
    epoch: Option<usize>,
    reproduce: bool,
}

fn continuation_request(raw: &Value) -> CaeResult<ContinuationRequest> {
    let Some(m) = raw.as_object() else {
        return contract("continuation must be an object");
    };
    if m.keys().any(|k| !matches!(k.as_str(), "schema" | "source_dir" | "epoch" | "mode"))
        || m.get("schema").and_then(Value::as_str) != Some(CONTINUATION_SCHEMA)
    {
        return contract(format!(
            "continuation must be {{schema: {CONTINUATION_SCHEMA}, source_dir, epoch, mode}}"
        ));
    }
    let reproduce = match m.get("mode") {
        None | Some(Value::Null) => false,
        Some(Value::String(mode)) if mode == "continue" => false,
        Some(Value::String(mode)) if mode == "reproduce" => true,
        Some(_) => return contract("continuation mode must be 'continue' or 'reproduce'"),
    };
    let source_dir = m
        .get("source_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| CaeError::contract("continuation source_dir must be an absolute path"))?;
    let epoch = match m.get("epoch") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|e| usize::try_from(e).ok())
                .ok_or_else(|| CaeError::contract("continuation epoch must be a nonnegative integer"))?,
        ),
    };
    Ok(ContinuationRequest { source_dir, epoch, reproduce })
}

fn row_identities(row: &Map<String, Value>) -> Map<String, Value> {
    IDENTITY_KEYS.iter().map(|k| ((*k).to_string(), row.get(*k).cloned().unwrap_or(Value::Null))).collect()
}



#[allow(clippy::too_many_lines)]
fn seed_continuation(
    raw: &Value,
    out_dir: &Path,
    r: &ResumeInputs<'_>,
    initial_design_state_id: &str,
    provenance: &Value,
) -> JobResult<Value> {
    let request = continuation_request(raw)?;
    let src = &request.source_dir;
    if !src.is_dir() {
        return Err(JobError::contract("continuation source directory does not exist"));
    }
    let payload: Value = std::fs::read_to_string(src.join("history.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| JobError::contract("continuation source history is missing or unreadable"))?;
    if payload.get("schema").and_then(Value::as_str) != Some(HISTORY_SCHEMA) {
        return Err(JobError::contract("continuation source history schema is unsupported"));
    }
    let source_rows: Vec<Value> =
        payload.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
    if source_rows.is_empty() {
        return Err(JobError::contract("continuation source history is empty"));
    }

    let continues = |row: &Value| row.as_object().is_some_and(|m| next_cursor(m, r.stages).0);
    let epoch = match request.epoch {
        Some(e) => e,
        None if request.reproduce => source_rows.len() - 1,
        None => source_rows.iter().rposition(continues).ok_or_else(|| {
            JobError::contract("continuation source has no row from which its search continues")
        })?,
    };
    if epoch >= source_rows.len() {
        return Err(JobError::contract(format!(
            "continuation epoch {epoch} is outside the source history (0..{})",
            source_rows.len() - 1
        )));
    }
    let source_ckpt = implexity_io::npz::load_file(&src.join("ckpt.npz"))
        .map_err(|_| CaeError::contract("continuation source checkpoint is unreadable"))?;
    let source_solve_id = strict_text(&source_ckpt, "solve_id")?;
    let normalization_text = strict_text(&source_ckpt, "response_normalization")?;
    let coupling_text = strict_text(&source_ckpt, "coupling_initialization")?;
    let shapes: BTreeMap<String, Vec<usize>> =
        r.run_initial.iter().map(|(k, v)| (k.to_string(), v.shape().to_vec())).collect();
    let source_initial =
        load_design_snapshot(&src.join("initial.npz"), r.coords, &shapes, r.bounds, &source_solve_id)?;
    if !eq_arrays(&source_initial, r.run_initial, r.coords) {
        return Err(JobError::contract("continuation source starts from another initial design"));
    }
    let mut rows: Vec<Value> = Vec::with_capacity(epoch + 1);
    let mut designs = NamedArrays::new();
    let mut lives: Vec<NamedArrays> = Vec::with_capacity(epoch + 1);
    for (index, source_row) in source_rows.iter().take(epoch + 1).enumerate() {
        let Some(row) = source_row.as_object() else {
            return Err(JobError::contract("continuation source history row is malformed"));
        };
        if row.get("i").and_then(Value::as_u64) != u64::try_from(index).ok() {
            return Err(JobError::contract("continuation source history sequence is malformed"));
        }
        let name = format!("live_{index:06}.npz");
        if !push_ok(row.get("push"), Some(&name)) {
            return Err(JobError::contract("continuation source live snapshot reference is malformed"));
        }
        let source_live = src.join(&name);
        let digest = sha256_file(&source_live)
            .map_err(|_| CaeError::contract(format!("continuation source snapshot {name} is missing")))?;
        if row["push"]["sha256"].as_str() != Some(digest.as_str()) {
            return Err(JobError::contract(format!("continuation source snapshot {name} content drifted")));
        }
        let live = load_design_snapshot(&source_live, r.coords, &shapes, r.bounds, &source_solve_id)?;
        if row.get("design_state_id").and_then(Value::as_str) != Some(design_identity(&live)?.as_str()) {
            return Err(JobError::contract(format!("continuation source snapshot {name} identity drifted")));
        }
        let target = out_dir.join(&name);
        write_design(&target, &live, r.solve_id)?;
        let mut push = Map::new();
        push.insert("file".into(), Value::String(name));
        push.insert("bytes".into(), Value::from(std::fs::metadata(&target)?.len()));
        push.insert("sha256".into(), Value::String(sha256_file(&target)?));
        let mut copied = row.clone();
        let mut source = Map::new();
        source.insert("schema".into(), Value::String(CONTINUATION_SCHEMA.into()));
        source.insert("source_row".into(), Value::from(index));
        source.insert("identities".into(), Value::Object(row_identities(row)));
        copied.insert("continuation_source".into(), Value::Object(source));
        copied.insert("push".into(), Value::Object(push));

        copied.insert("continuable".into(), Value::Bool(next_cursor(row, r.stages).0));
        for (k, v) in r.identities {
            copied.insert(k.clone(), v.clone());
        }
        designs.clone_from(&live);
        lives.push(live);
        rows.push(Value::Object(copied));
    }
    let last = rows[epoch].as_object().cloned().unwrap_or_default();
    let (next_continue, _, next_local, next_stage_id) = next_cursor(&last, r.stages);
    if !next_continue && !request.reproduce {
        return Err(JobError::contract(format!(
            "continuation epoch {epoch} ended its search ({}); choose an earlier epoch",
            last.get("reason").map_or_else(|| "terminal".to_string(), py_str)
        )));
    }

    write_design(&out_dir.join("initial.npz"), r.run_initial, r.solve_id)?;
    let mut sidecar = Map::new();
    sidecar.insert("schema".into(), Value::String("implexity-hierarchical-initial-design/1".into()));
    sidecar.insert("design_state_id".into(), Value::String(initial_design_state_id.into()));
    sidecar
        .insert("coordinate_design_state_ids".into(), Value::Object(coordinate_identities(r.run_initial)?));
    for (k, v) in r.identities {
        sidecar.insert(k.clone(), v.clone());
    }
    atomic_json(&out_dir.join("initial_design.json"), &Value::Object(sidecar))?;
    let best_index = history_best_index(&rows)?;
    write_design(&out_dir.join("best.npz"), &lives[best_index], r.solve_id)?;
    let design_id = design_identity(&designs)?;

    let seeded_checkpoint = |k: usize| -> JobResult<Vec<(String, NpyArray)>> {
        let prefix = &rows[..=k];
        let row = prefix[k].as_object().cloned().unwrap_or_default();
        let (cont, stage, local, stage_id) = next_cursor(&row, r.stages);
        let best_k = history_best_index(prefix)?;
        let design_k = &lives[k];
        let reason = row.get("reason").and_then(Value::as_str).unwrap_or("iteration_limit").to_string();
        let mut members: Vec<(String, NpyArray)> =
            design_k.iter().map(|(n, v)| (format!("p_{}", slot(n)), NpyArray::from_f64(v))).collect();
        members.push(("coordinate_order".into(), NpyArray::strings(&design_k.names())));
        let text = |n: &str, v: &str| (n.to_string(), NpyArray::scalar_str(v));
        let int = |n: &str, v: i64| (n.to_string(), NpyArray::scalar_i64(v));
        members.push(text("checkpoint_schema", CHECKPOINT_SCHEMA));
        members.push(int("stage_index", i64::try_from(stage).unwrap_or(i64::MAX)));
        members.push(text("stage_id", &stage_id));
        members.push(int("local_next", local));
        members.push(int("global_next", i64::try_from(prefix.len()).unwrap_or(i64::MAX)));
        members.push(text("continuation_state", if cont { "continue" } else { "terminal" }));
        members.push(text("termination_reason", &reason));
        members.push(("L_first".into(), NpyArray::scalar_f64(num(prefix[0].get("L_first")))));
        members.push(("L_best".into(), NpyArray::scalar_f64(num(prefix[best_k].get("L")))));
        for (n, v) in r.identities {
            members.push(text(n, v.as_str().unwrap_or("")));
        }
        members.push(text("initial_design_state_id", initial_design_state_id));
        members.push(text("design_state_id", &design_identity(design_k)?));
        members.push(text(
            "initial_coordinate_design_state_ids",
            &canonical_text(&Value::Object(coordinate_identities(r.run_initial)?)),
        ));
        members.push(text(
            "coordinate_design_state_ids",
            &canonical_text(&Value::Object(coordinate_identities(design_k)?)),
        ));
        members.push(text(
            "best_design_state_id",
            prefix[best_k].get("design_state_id").and_then(Value::as_str).unwrap_or(""),
        ));
        members.push(int("best_history_index", i64::try_from(best_k).unwrap_or(i64::MAX)));
        members.push(int("history_length", i64::try_from(prefix.len()).unwrap_or(i64::MAX)));
        members.push(text("history_digest", &history_digest(prefix)));
        members.push(text("matching_time_guess_consumed", &canonical_text(&Value::Null)));
        members.push(text("response_normalization", &normalization_text));
        members.push(text("coupling_initialization", &coupling_text));
        members.push(text("search_state", &canonical_text(row.get("search_state").unwrap_or(&Value::Null))));
        Ok(members)
    };

    let mut warm: Option<MatchingTimeNewtonGuess> = None;
    let mut warm_source = "cold";
    for k in 0..=epoch {
        let source_state = crate::epoch_state::read_epoch_state(src, k)?;
        if let Some(state) = &source_state
            && canonical_text(&state.history_row) != canonical_text(&source_rows[k])
        {
            return Err(JobError::contract(format!(
                "continuation source epoch state {k} belongs to another history row"
            )));
        }
        let design_k = design_identity(&lives[k])?;
        let (warm_k, source_k) = match source_state.as_ref() {
            Some(state) => (state.warm_start.clone(), "epoch_state"),
            None if k + 1 == source_rows.len() => {
                let latest = read_resume_warm_start(src, &design_k)?;
                let label = if latest.is_some() { "resume_warm_start" } else { "cold" };
                (latest, label)
            }
            None => (None, "cold"),
        };
        let source_k =
            if warm_k.is_none() && source_k == "epoch_state" { "cold (epoch state)" } else { source_k };
        let seeded_from = serde_json::json!({
            "schema": CONTINUATION_SCHEMA,
            "source_dir": src.to_string_lossy(),
            "source_epoch": k,
            "source_epoch_state": source_state.as_ref().map(|s| s.manifest["archive"].clone()),
            "warm_start": source_k,
        });
        crate::epoch_state::write_epoch_state(
            out_dir,
            &crate::epoch_state::EpochRecord {
                epoch: k,
                checkpoint: &seeded_checkpoint(k)?,
                warm_start: warm_k.as_ref(),
                history_row: &rows[k],
                history_payload: &history_payload(&rows[..=k]),
                provenance,
                seeded_from: Some(&seeded_from),
            },
        )?;
        if k == epoch {
            warm = warm_k;
            warm_source = source_k;
        }
    }
    write_resume_warm_start(out_dir, warm.as_ref(), &design_id)?;
    atomic_json(&out_dir.join("history.json"), &history_payload(&rows))?;
    atomic_npz_members(&out_dir.join("ckpt.npz"), &seeded_checkpoint(epoch)?)?;
    let source_ids = row_identities(&last);
    let changed: Vec<Value> = IDENTITY_KEYS
        .iter()
        .filter(|k| source_ids.get(**k) != r.identities.get(**k))
        .map(|k| Value::String((*k).to_string()))
        .collect();
    let mut record = Map::new();
    record.insert("schema".into(), Value::String(CONTINUATION_SCHEMA.into()));
    record.insert("source_dir".into(), Value::String(src.to_string_lossy().into_owned()));
    record.insert("source_epoch".into(), Value::from(epoch));
    record.insert("source_history_length".into(), Value::from(source_rows.len()));
    record.insert("source_identities".into(), Value::Object(source_ids));
    record.insert("changed_identities".into(), Value::Array(changed));
    record.insert("seed_design_state_id".into(), Value::String(design_id));
    record.insert("seed_objective".into(), fv(num(last.get("L"))));
    record
        .insert("warm_start".into(), Value::String(if warm.is_some() { "persisted" } else { "cold" }.into()));
    record.insert("warm_start_source".into(), Value::String(warm_source.into()));
    record.insert(
        "mode".into(),
        Value::String(if request.reproduce { "reproduce" } else { "continue" }.into()),
    );
    record.insert("next_stage".into(), Value::String(next_stage_id));
    record.insert("next_stage_iteration".into(), Value::from(next_local));
    let record = Value::Object(record);
    atomic_json(&out_dir.join("continuation.json"), &record)?;
    Ok(record)
}

pub const REPRODUCTION_SCHEMA: &str = "implexity-epoch-reproduction/1";

#[allow(clippy::needless_pass_by_value)]
fn reproduction_result(
    out_dir: &Path,
    seed: &Value,
    recorded: &Value,
    replayed_total: f64,
    replayed_terms: &Value,
    tolerance: f64,
    warm_evidence: &Value,
    checked: CaeResult<Value>,
) -> JobResult<Map<String, Value>> {
    let recorded_terms = recorded.get("terms").cloned().unwrap_or(Value::Null);
    let objective_identical = recorded.get("L").and_then(Value::as_f64).is_some_and(|l| l == replayed_total);
    let terms_identical = canonical_text(&recorded_terms) == canonical_text(replayed_terms);
    let (reproduced, evidence, error) = match &checked {
        Ok(evidence) => (true, evidence.clone(), Value::Null),
        Err(e) => (false, Value::Null, Value::String(e.message().to_string())),
    };
    let mut record = Map::new();
    record.insert("schema".into(), Value::String(REPRODUCTION_SCHEMA.into()));
    record.insert("status".into(), Value::String(if reproduced { "reproduced" } else { "differs" }.into()));
    record.insert("epoch".into(), seed.get("source_epoch").cloned().unwrap_or(Value::Null));
    record.insert("source_dir".into(), seed.get("source_dir").cloned().unwrap_or(Value::Null));
    record.insert("design_state_id".into(), recorded.get("design_state_id").cloned().unwrap_or(Value::Null));
    record.insert("warm_start".into(), seed.get("warm_start_source").cloned().unwrap_or(Value::Null));
    record.insert("warm_start_installation".into(), warm_evidence.clone());
    record.insert("relative_tolerance".into(), fv(tolerance));
    record.insert("bitwise".into(), Value::Bool(objective_identical && terms_identical));
    record.insert("objective_identical".into(), Value::Bool(objective_identical));
    record.insert("terms_identical".into(), Value::Bool(terms_identical));
    record.insert("recorded".into(), serde_json::json!({"L": recorded.get("L"), "terms": recorded_terms}));
    record.insert("replayed".into(), serde_json::json!({"L": fv(replayed_total), "terms": replayed_terms}));
    record.insert("replay_evidence".into(), evidence);
    record.insert("error".into(), error);
    atomic_json(&out_dir.join("reproduction.json"), &Value::Object(record.clone()))?;
    emit_line("REPRODUCED ", &Value::Object(record.clone()));
    match checked {
        Ok(_) => Ok(record),
        Err(e) => Err(e.into()),
    }
}

fn matching_time_config(spec: &Map<String, Value>) -> CaeResult<Map<String, Value>> {
    match spec.get("matching_time_guess") {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) if m.keys().all(|k| k == "consume" || k == "produce") => Ok(m.clone()),
        Some(_) => contract("matching_time_guess job transport is malformed"),
    }
}

fn matching_time_public_config(config: &Map<String, Value>) -> CaeResult<Option<Value>> {
    if config.is_empty() {
        return Ok(None);
    }
    let mut out = Map::new();
    if let Some(consume) = config.get("consume").filter(|v| !v.is_null()) {
        let Some(c) = consume.as_object() else {
            return contract("matching-time guess job consumption is malformed");
        };
        let mut row = Map::new();
        row.insert("capsule_id".into(), c.get("capsule_id").cloned().unwrap_or(Value::Null));
        row.insert("required".into(), c.get("required").cloned().unwrap_or(Value::Bool(true)));
        out.insert("consume".into(), Value::Object(row));
    }
    match config.get("produce") {
        None | Some(Value::Null | Value::Bool(false)) => {}
        Some(Value::Object(p)) => {
            let mut row = Map::new();
            row.insert(
                "require_accepted".into(),
                p.get("require_accepted").cloned().unwrap_or(Value::Bool(true)),
            );
            out.insert("produce".into(), Value::Object(row));
        }
        Some(_) => return contract("matching-time guess job production is malformed"),
    }
    Ok(if out.is_empty() { None } else { Some(Value::Object(out)) })
}

fn matching_time_fingerprint_spec(spec: &Map<String, Value>) -> CaeResult<Value> {
    let mut out = spec.clone();
    out.insert("update_metric".into(), Value::Object(normalise_update_metric(spec.get("update_metric"))?));
    if let Some(rows) = spec.get("design_coordinates").filter(|v| !v.is_null()) {
        let Some(rows) = rows.as_array().filter(|r| r.iter().all(Value::is_object)) else {
            return contract("design_coordinates must be a sequence of mappings");
        };
        let mut normalized = Vec::new();
        for row in rows {
            let mut item = row.as_object().cloned().unwrap_or_default();
            let name = item.get("coordinate").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
            let scale =
                coordinate_step_scale(Some(item.get("step_scale").unwrap_or(&Value::from(1.0))), &name)?;
            item.insert("step_scale".into(), fv(scale));
            normalized.push(Value::Object(item));
        }
        out.insert("design_coordinates".into(), Value::Array(normalized));
    }
    match matching_time_public_config(&matching_time_config(&out)?)? {
        None => {
            out.shift_remove("matching_time_guess");
        }
        Some(public) => {
            out.insert("matching_time_guess".into(), public);
        }
    }
    Ok(Value::Object(out))
}

fn nonempty_text(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| !s.is_empty())
}

fn validate_matching_time_lifecycle(
    config: &Map<String, Value>,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
) -> CaeResult<()> {
    if config.is_empty() {
        return Ok(());
    }
    let consume = config.get("consume").filter(|v| !v.is_null());
    if let Some(c) = consume {
        let ok = c.as_object().is_some_and(|m| {
            m.keys().all(|k| matches!(k.as_str(), "capsule_id" | "store_root" | "required"))
                && nonempty_text(m.get("capsule_id"))
                && nonempty_text(m.get("store_root"))
                && m.get("required").is_none_or(Value::is_boolean)
        }) && provides(provider, DesignOp::InstallMatchingTimeGuess);
        if !ok {
            return contract("selected provider has no valid matching-time guess install lifecycle");
        }
    }
    let produce = config.get("produce").filter(|v| !v.is_null() && **v != Value::Bool(false));
    if let Some(p) = produce {
        let ok = p.as_object().is_some_and(|m| {
            m.keys().all(|k| k == "store_root" || k == "require_accepted")
                && nonempty_text(m.get("store_root"))
                && m.get("require_accepted").is_none_or(|v| *v == Value::Bool(true))
        }) && provides(provider, DesignOp::ExportMatchingTimeGuess);
        if !ok {
            return contract(
                "selected provider has no valid accepted-state matching-time guess export lifecycle",
            );
        }
    }
    let require_accepted = produce
        .and_then(Value::as_object)
        .is_some_and(|m| m.get("require_accepted").is_none_or(|v| *v == Value::Bool(true)));
    if let Some(result) = provider_lifecycle_verifier(
        provider,
        problem,
        consume.is_some(),
        produce.is_some(),
        require_accepted,
    )? && result.get("mutation_performed") != Some(&Value::Bool(false))
    {
        return contract("matching-time lifecycle validation returned unsafe evidence");
    }
    Ok(())
}

fn guess_error(e: GuessError) -> JobError {
    JobError::from(e)
}

fn install_matching_time_guess(
    config: &Map<String, Value>,
    provider_name: &str,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    designs: &NamedArrays,
) -> JobResult<Option<Value>> {
    let Some(raw) = config.get("consume").filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(raw) = raw
        .as_object()
        .filter(|m| m.keys().all(|k| matches!(k.as_str(), "capsule_id" | "store_root" | "required")))
    else {
        return Err(JobError::contract("matching-time guess job consumption is malformed"));
    };
    let required = match raw.get("required") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(JobError::contract("matching-time guess required policy must be boolean")),
    };
    let capsule = raw.get("capsule_id").filter(|v| truthy(v));
    let root = raw.get("store_root").filter(|v| truthy(v));
    let (Some(capsule), Some(root)) = (capsule, root) else {
        if required {
            return Err(JobError::contract("required matching-time guess reference is missing"));
        }
        return Ok(None);
    };
    let owned = designs.owned_design()?;
    let execution =
        execution_identity(provider_name, &problem_json(provider, problem)?, &owned, None, None, None)?;
    let store = MatchingTimeGuessStore::new(py_str(root)).map_err(guess_error)?;
    let (guess, descriptor) = match store.load(&py_str(capsule), &execution) {
        Ok(v) => v,
        Err(GuessError::Missing(_)) if !required => return Ok(None),
        Err(e) => return Err(guess_error(e)),
    };
    let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::InstallMatchingTimeGuess))
    else {
        return Err(JobError::contract("selected provider has no matching-time guess lifecycle"));
    };
    let ack = ops.install_matching_time_guess(problem, &owned, &guess)?;
    if !install_acknowledgement_ok(&ack, &design_identity(designs)?) {
        return Err(JobError::contract(
            "matching-time guess installation acknowledgement is stale or unsafe",
        ));
    }
    let mut merged = descriptor;
    merged.insert("consumed".into(), Value::Bool(true));
    merged.insert("installation".into(), Value::Object(ack));
    Ok(Some(Value::Object(public_descriptor(&Value::Object(merged)).map_err(guess_error)?)))
}

fn export_matching_time_guess(
    config: &Map<String, Value>,
    provider_name: &str,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    designs: &NamedArrays,
) -> JobResult<Option<Value>> {
    let raw = match config.get("produce") {
        None | Some(Value::Null | Value::Bool(false)) => return Ok(None),
        Some(v) => v,
    };
    let Some(raw) =
        raw.as_object().filter(|m| m.keys().all(|k| k == "store_root" || k == "require_accepted"))
    else {
        return Err(JobError::contract("matching-time guess job production is malformed"));
    };
    let root = raw.get("store_root").filter(|v| truthy(v));
    let required = raw.get("require_accepted").cloned().unwrap_or(Value::Bool(true));
    let Some(root) = root.filter(|_| required == Value::Bool(true)) else {
        return Err(JobError::contract("terminal job guess export requires accepted-state policy"));
    };
    let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::ExportMatchingTimeGuess))
    else {
        return Err(JobError::contract("selected provider has no matching-time guess lifecycle"));
    };
    let owned = designs.owned_design()?;
    let guess = ops.export_matching_time_guess(problem, &owned, true)?;
    let execution =
        execution_identity(provider_name, &problem_json(provider, problem)?, &owned, None, None, None)?;
    let store = MatchingTimeGuessStore::new(py_str(root)).map_err(guess_error)?;
    let descriptor = store.create(&guess, &execution).map_err(guess_error)?;
    Ok(Some(Value::Object(public_descriptor(&Value::Object(descriptor)).map_err(guess_error)?)))
}

pub(crate) const RESUME_WARM_START_SLOTS: [&str; 2] = ["resume_warm_start.npz", "resume_warm_start.alt.npz"];
pub(crate) const RESUME_WARM_START_SIDECAR: &str = "resume_warm_start.json";
pub(crate) const RESUME_WARM_START_FILES: [&str; 3] =
    ["resume_warm_start.npz", "resume_warm_start.alt.npz", "resume_warm_start.json"];
pub(crate) const RESUME_WARM_START_TEMPORARIES: [&str; 3] =
    ["resume_warm_start.npz.tmp.npz", "resume_warm_start.alt.npz.tmp.npz", "resume_warm_start.json.tmp"];
const RESUME_WARM_START_SCHEMA: &str = "implexity-resume-warm-start/1";
const LEGACY_RESUME_WARM_START_SCHEMA: &str = "implexity-rust-resume-warm-start/1";

fn known_warm_start_schema(tag: Option<&str>) -> bool {
    tag.is_some_and(|t| t == RESUME_WARM_START_SCHEMA || t == LEGACY_RESUME_WARM_START_SCHEMA)
}



pub(crate) fn accepted_warm_start(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    designs: &NamedArrays,
    points: &[i64],
) -> Option<MatchingTimeNewtonGuess> {
    if points != [0] {
        return None;
    }
    let ops = design_operations(provider).filter(|o| {
        o.provides(DesignOp::ExportMatchingTimeGuess) && o.provides(DesignOp::InstallMatchingTimeGuess)
    })?;
    let owned = designs.owned_design().ok()?;
    ops.export_matching_time_guess(problem, &owned, true).ok()
}

fn warm_start_generations(out_dir: &Path) -> CaeResult<Vec<Value>> {
    let sidecar = out_dir.join(RESUME_WARM_START_SIDECAR);
    if !sidecar.is_file() {
        return Ok(Vec::new());
    }
    let record: Value = std::fs::read_to_string(&sidecar)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(|| CaeError::contract("resume warm-start sidecar is malformed"))?;
    match record.get("schema").and_then(Value::as_str) {
        Some(RESUME_WARM_START_SCHEMA) => match record.get("generations") {
            Some(Value::Array(rows)) if rows.len() <= 2 && rows.iter().all(Value::is_object) => {
                Ok(rows.clone())
            }
            _ => contract("resume warm-start sidecar generations are malformed"),
        },
        Some(LEGACY_RESUME_WARM_START_SCHEMA) => Ok(vec![record]),
        _ => contract("resume warm-start sidecar schema is unsupported"),
    }
}

pub(crate) fn write_resume_warm_start(
    out_dir: &Path,
    guess: Option<&MatchingTimeNewtonGuess>,
    committed_design_state_id: &str,
) -> JobResult<()> {
    let previous =
        warm_start_generations(out_dir).ok().and_then(|rows| rows.into_iter().next()).filter(|g| {
            g.get("committed_design_state_id").and_then(Value::as_str) != Some(committed_design_state_id)
        });
    let taken = previous.as_ref().and_then(|g| g.get("archive")).and_then(Value::as_str);
    let slot = RESUME_WARM_START_SLOTS
        .iter()
        .copied()
        .find(|s| Some(*s) != taken)
        .unwrap_or(RESUME_WARM_START_SLOTS[0]);
    let mut generation = Map::new();
    generation.insert("committed_design_state_id".into(), Value::String(committed_design_state_id.into()));
    match guess {
        None => {
            generation.insert("archive".into(), Value::Null);
            generation.insert("states".into(), Value::from(0));
        }
        Some(guess) => {
            let archive = out_dir.join(slot);
            atomic_npz_members(&archive, &warm_start_members(guess)?)?;
            generation.insert("archive".into(), Value::String(slot.into()));
            generation.insert("archive_sha256".into(), Value::String(sha256_file(&archive)?));
            generation.insert("states".into(), Value::from(guess.states().len()));
        }
    }
    let generations: Vec<Value> = std::iter::once(Value::Object(generation))
        .chain(previous.map(|mut g| {

            if let Some(m) = g.as_object_mut() {
                m.remove("schema");
            }
            g
        }))
        .collect();
    let referenced: Vec<String> = generations
        .iter()
        .filter_map(|g| g.get("archive").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut record = Map::new();
    record.insert("schema".into(), Value::String(RESUME_WARM_START_SCHEMA.into()));
    record.insert("generations".into(), Value::Array(generations));
    atomic_json(&out_dir.join(RESUME_WARM_START_SIDECAR), &Value::Object(record))?;
    for name in RESUME_WARM_START_SLOTS {
        let path = out_dir.join(name);
        if !referenced.iter().any(|r| r == name) && path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}


pub(crate) fn read_resume_warm_start(
    out_dir: &Path,
    committed_design_state_id: &str,
) -> CaeResult<Option<MatchingTimeNewtonGuess>> {
    let Some(generation) = warm_start_generations(out_dir)?.into_iter().find(|g| {
        g.get("committed_design_state_id").and_then(Value::as_str) == Some(committed_design_state_id)
    }) else {
        return Ok(None);
    };
    let name = match generation.get("archive") {
        Some(Value::Null) => return Ok(None),
        Some(Value::String(n)) if RESUME_WARM_START_SLOTS.contains(&n.as_str()) => n.clone(),
        _ => return contract("resume warm-start generation names no archive slot"),
    };
    let archive = out_dir.join(&name);
    let digest =
        sha256_file(&archive).map_err(|_| CaeError::contract("resume warm-start archive is missing"))?;
    if generation.get("archive_sha256").and_then(Value::as_str) != Some(digest.as_str()) {
        return contract("resume warm-start archive does not match its sidecar");
    }
    let npz = implexity_io::npz::load_file(&archive)
        .map_err(|_| CaeError::contract("resume warm-start archive is unreadable"))?;
    let count = generation.get("states").and_then(Value::as_u64).unwrap_or(0);
    if npz.files().len() != usize::try_from(count).unwrap_or(usize::MAX).saturating_add(3) {
        return contract("resume warm-start archive contains undeclared members");
    }
    warm_start_from_members(&|key| npz.get(key), count).map(Some)
}


pub(crate) fn warm_start_members(guess: &MatchingTimeNewtonGuess) -> JobResult<Vec<(String, NpyArray)>> {
    let mut members: Vec<(String, NpyArray)> = vec![
        ("schema".into(), NpyArray::scalar_str(RESUME_WARM_START_SCHEMA)),
        (
            "provider_identity".into(),
            NpyArray::scalar_str(&canonical_text(&Value::Object(guess.provider_identity().clone()))),
        ),
        (
            "provenance".into(),
            NpyArray::scalar_str(&canonical_text(&Value::Object(guess.provenance().clone()))),
        ),
    ];
    for (n, state) in guess.states().iter().enumerate() {
        let values = ArrayD::from_shape_vec(IxDyn(&[state.len()]), state.to_vec())
            .map_err(|e| JobError::contract(format!("resume warm start state {n}: {e}")))?;
        members.push((format!("state_{n:04}"), NpyArray::from_f64(&values)));
    }
    Ok(members)
}


pub(crate) fn warm_start_from_members<'a>(
    get: &dyn Fn(&str) -> Option<&'a NpyArray>,
    count: u64,
) -> CaeResult<MatchingTimeNewtonGuess> {
    let text = |key: &str| -> CaeResult<String> {
        match get(key).map(|a| &a.data) {
            Some(NpyData::Unicode { values, .. }) if values.len() == 1 => Ok(values[0].clone()),
            _ => contract(format!("resume warm-start archive lacks {key}")),
        }
    };
    if !known_warm_start_schema(Some(text("schema")?.as_str())) {
        return contract("resume warm-start archive schema is unsupported");
    }
    let object = |key: &str| -> CaeResult<Map<String, Value>> {
        match parse_canonical(&text(key)?, key)? {
            Value::Object(m) => Ok(m),
            _ => contract(format!("resume warm-start {key} must be an object")),
        }
    };
    let (identity, provenance) = (object("provider_identity")?, object("provenance")?);
    let mut states = Vec::new();
    for n in 0..count {
        let values = get(&format!("state_{n:04}"))
            .and_then(NpyArray::as_f64)
            .ok_or_else(|| CaeError::contract("resume warm-start archive lacks a state"))?;
        states.push(values.iter().copied().collect::<Vec<f64>>());
    }
    MatchingTimeNewtonGuess::new(states, identity, provenance)
}

pub(crate) fn install_resume_warm_start(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    designs: &NamedArrays,
    guess: &MatchingTimeNewtonGuess,
) -> JobResult<Value> {
    let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::InstallMatchingTimeGuess))
    else {
        return Err(JobError::contract(
            "resume warm start requires the provider's matching-time guess lifecycle",
        ));
    };
    let owned = designs.owned_design()?;
    let ack = ops.install_matching_time_guess(problem, &owned, guess)?;
    if !install_acknowledgement_ok(&ack, &design_identity(designs)?) {
        return Err(JobError::contract("resume warm-start installation acknowledgement is stale or unsafe"));
    }
    let mut out = Map::new();
    out.insert("schema".into(), Value::String(RESUME_WARM_START_SCHEMA.into()));
    out.insert("installed".into(), Value::Bool(true));
    out.insert("states".into(), Value::from(guess.states().len()));
    out.insert("provenance".into(), Value::Object(guess.provenance().clone()));
    out.insert("installation".into(), Value::Object(ack));
    Ok(Value::Object(out))
}


pub fn coordinate_designable_masks(
    raw: Option<&Value>,
    designs: &NamedArrays,
) -> CaeResult<BTreeMap<String, ArrayD<bool>>> {
    let mut out = BTreeMap::new();
    let Some(raw) = raw.filter(|v| truthy(v)) else { return Ok(out) };
    let Some(map) = raw.as_object() else {
        return contract("coordinate masks must be a mapping");
    };
    for (coord, value) in map {
        let Some(target) = designs.get(coord) else {
            return contract(format!("coordinate mask references unknown design coordinate {}", repr(coord)));
        };
        let designable = match value {
            Value::Object(spec) => match spec.get("designable") {
                None => continue,
                Some(v) => v.clone(),
            },
            Value::Null => continue,
            other => other.clone(),
        };
        let invalid =
            || CaeError::contract(format!("{coord}: designable coordinate mask must be boolean/0-1"));
        let mask = crate::provider_worker::json_array(&designable).ok_or_else(invalid)?;
        let target_shape = target.shape().to_vec();
        let mask = if mask.ndim() == 0 {
            ArrayD::from_elem(IxDyn(&target_shape), mask.iter().next().copied().unwrap_or(0.0))
        } else if mask.shape() == target_shape.as_slice() {
            mask
        } else if mask.ndim() == 3
            && target_shape.len() > 3
            && mask.shape() == &target_shape[target_shape.len() - 3..]
        {
            mask.broadcast(IxDyn(&target_shape)).map(|b| b.to_owned()).ok_or_else(invalid)?
        } else {
            return contract(format!(
                "{coord}: designable coordinate mask shape {} does not match coordinate shape {}",
                implexity_optim::numeric::shape_repr(mask.shape())
                    .replace('(', "[")
                    .replace(",)", "]")
                    .replace(')', "]"),
                implexity_optim::numeric::shape_repr(&target_shape)
                    .replace('(', "[")
                    .replace(",)", "]")
                    .replace(')', "]")
            ));
        };
        if mask.iter().any(|v| !v.is_finite() || (*v != 0.0 && *v != 1.0)) {
            return Err(invalid());
        }
        out.insert(coord.clone(), mask.mapv(|v| v != 0.0));
    }
    Ok(out)
}

fn stage_coordinates(
    designs: &NamedArrays,
    bounds: &BTreeMap<String, Bounds>,
    masks: &BTreeMap<String, ArrayD<bool>>,
    active: &[String],
) -> Vec<StageCoordinate> {
    designs
        .iter()
        .map(|(name, value)| {
            let b = &bounds[name];
            let mut designable = ArrayD::from_elem(value.raw_dim(), active.iter().any(|a| a == name));
            if let Some(mask) = masks.get(name) {
                Zip::from(&mut designable).and(mask).for_each(|d, m| *d &= *m);
            }
            StageCoordinate {
                name: name.to_string(),
                lower: b.lower.clone(),
                upper: b.upper.clone(),
                designable,
            }
        })
        .collect()
}

pub(crate) fn diagnostic_response_rows(provider: &dyn CaeProvider, responses: &BTreeMap<String, f64>, point: i64) -> Value {
    let metadata = provider.capabilities().ok().map(|c| c.to_map());
    Value::Array(responses.iter().filter(|(_, value)| value.is_finite()).map(|(name, value)| {
        let mut row = Map::new();
        row.insert("response".into(), Value::String(name.clone()));
        row.insert("value".into(), fv(*value));
        row.insert("operating_point".into(), Value::from(point));
        if let Some(unit) = metadata.as_ref().and_then(|m| m.get("response_metadata")).and_then(|m| m.get(name)).and_then(|m| m.get("unit")).and_then(Value::as_str) {
            row.insert("units".into(), Value::String(unit.into()));
        }
        Value::Object(row)
    }).collect())
}

fn evaluation_values(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    names: &[String],
    point: usize,
) -> SearchResult<Option<(BTreeMap<String, f64>, Map<String, Value>)>> {
    let mut raw = if let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::EvaluateDesign))
    {
        check_operating_point(provider, DesignOp::EvaluateDesign, point)?;
        crate::solver_recovery::once("provider_evaluation", || ops.evaluate_design(problem, design, point))?
    } else {
        let caps = provider.capabilities()?;
        let ProviderCapabilities::Legacy(legacy) = &caps else { return Ok(None) };
        if design.names() != [legacy.topology_coordinate.clone()] || !provides(provider, DesignOp::Evaluate) {
            return Ok(None);
        }
        let array = design.get(&legacy.topology_coordinate).cloned().unwrap_or_default();
        crate::solver_recovery::once("provider_evaluation", || legacy_evaluate(provider, problem, &array, point))?
    };
    raw.diagnostics.insert("diagnostic_responses".into(), diagnostic_response_rows(provider, &raw.responses, i64::try_from(point).unwrap_or(i64::MAX)));
    evaluation_response_values(&raw, names, "provider evaluation").map(Some)
}

fn robust_diagnostics(
    program: &StageResponseProgram,
    per_point: &[Map<String, Value>],
) -> Map<String, Value> {
    if program.mode == "nominal" {
        return per_point.first().cloned().unwrap_or_default();
    }
    let mut m = Map::new();
    m.insert("diagnostic_responses".into(), Value::Array(per_point.iter().flat_map(|p| p.get("diagnostic_responses").and_then(Value::as_array).cloned().unwrap_or_default()).collect()));
    m.insert("robust_mode".into(), Value::String(program.mode.clone()));
    m.insert("operating_points".into(), ints(&program.points));
    m.insert("points".into(), Value::Array(per_point.iter().cloned().map(Value::Object).collect()));
    m
}

fn join_strings(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().map(py_str).collect::<Vec<_>>().join("; "))
        .unwrap_or_default()
}


#[allow(clippy::too_many_lines)]
pub fn regime_ok(
    diagnostics: &mut Map<String, Value>,
    problem: Option<&Value>,
    limits: Option<&Map<String, Value>>,
    observe_engineering: bool,
) -> CaeResult<(bool, String)> {
    let d = diagnostics.clone();
    let waived = || contract("opaque provider validity refusal cannot be waived by engineering observation");
    for key in ["regimeValid", "regime_valid"] {
        if let Some(v) = d.get(key)
            && !truthy(v)
        {
            if observe_engineering {
                return waived();
            }
            return Ok((false, d.get("regimeMessage").filter(|m| truthy(m)).map(py_str).unwrap_or_default()));
        }
    }
    for key in ["regime", "validity"] {
        if let Some(row) = d.get(key).and_then(Value::as_object)
            && let Some(ok) = row.get("ok")
            && !truthy(ok)
        {
            if observe_engineering {
                return waived();
            }
            return Ok((false, row.get("message").filter(|m| truthy(m)).map(py_str).unwrap_or_default()));
        }
    }
    let robust =
        matches!(d.get("robust_mode").and_then(Value::as_str), Some("expected" | "smooth_worst_case"));
    if d.contains_key("points") || robust {
        let points = d.get("points").and_then(Value::as_array);
        let ids = d.get("operating_points").and_then(Value::as_array);
        let valid = match (points, ids) {
            (Some(points), Some(ids)) => {
                let unique: BTreeSet<i64> = ids.iter().filter_map(Value::as_i64).collect();
                !ids.is_empty()
                    && points.len() == ids.len()
                    && ids.iter().all(|i| is_int(Some(i)) && i.as_i64().is_some_and(|v| v >= 0))
                    && unique.len() == ids.len()
                    && points.iter().all(Value::is_object)
            }
            _ => false,
        };
        if !valid {
            return contract("robust regime diagnostics require exact operating-point coverage");
        }
        let (points, ids) = (points.cloned().unwrap_or_default(), ids.cloned().unwrap_or_default());
        let mut reports = Vec::new();
        let mut decisions = Vec::new();
        let mut messages = Vec::new();
        for (point_id, point_diagnostics) in ids.iter().zip(points) {
            let mut point = point_diagnostics.as_object().cloned().unwrap_or_default();
            let (ok, message) = regime_ok(&mut point, problem, limits, observe_engineering)?;
            decisions.push(ok);
            if !message.is_empty() {
                messages.push(format!("operating point {}: {message}", py_str(point_id)));
            }
            let assessment = point.get("engineering_regime_assessment").cloned().unwrap_or_else(|| {
                serde_json::json!({
                    "assessment_schema": "implexity-regime-monitor-assessment/1",
                    "ok": ok, "computable": ok,
                    "contract_issues": if ok { Vec::<String>::new() } else {
                        vec![if message.is_empty() { "provider validity refused".to_string() } else { message.clone() }]
                    },
                    "engineering_satisfied": true, "engineering_violations": [], "values": {}, "units": {},
                })
            });
            reports.push(serde_json::json!({"operating_point": point_id, "assessment": assessment}));
        }
        let all = |key: &str| {
            reports.iter().all(|r| r.get("assessment").and_then(|a| a.get(key)).is_some_and(truthy))
        };
        let assessment = serde_json::json!({
            "assessment_schema": "implexity-regime-monitor-assessment/2",
            "operating_points": ids,
            "points": reports,
            "computable": all("computable"),
            "engineering_satisfied": all("engineering_satisfied"),
            "ok": all("ok"),
            "policy": if observe_engineering { "observe" } else { "enforce" },
            "final_acceptance_performed": false,
            "model_authority_promoted": false,
        });
        diagnostics.insert("engineering_regime_assessment".into(), assessment);
        return Ok((decisions.iter().all(|d| *d), messages.join("; ")));
    }
    if let (Some(limits), Some(problem)) =
        (limits.filter(|l| !l.is_empty()), problem.filter(|p| p.is_object()))
    {
        let check = implexity_core::registries::global().extensions.evaluate_regime_monitors(
            problem,
            Some(&Value::Object(d.clone())),
            Some(limits),
        );
        let chk = check.as_object().cloned().unwrap_or_default();
        let record = |policy: &str| {
            let mut m = chk.clone();
            m.insert("policy".into(), Value::String(policy.into()));
            m.insert("final_acceptance_performed".into(), Value::Bool(false));
            m.insert("model_authority_promoted".into(), Value::Bool(false));
            Value::Object(m)
        };
        diagnostics.insert(
            "engineering_regime_assessment".into(),
            record(if observe_engineering { "observe" } else { "enforce" }),
        );
        let v1 = chk.get("assessment_schema").and_then(Value::as_str)
            == Some("implexity-regime-monitor-assessment/1");
        let computable = chk.get("computable") == Some(&Value::Bool(true));
        let failure = || {
            contract(format!(
                "regime monitor contract/computability failure: {}",
                join_strings(chk.get("issues"))
            ))
        };
        if v1 && !computable {
            return failure();
        }
        if observe_engineering {
            if !v1 || !computable {
                return failure();
            }
            diagnostics.insert("engineering_regime_assessment".into(), record("observe"));
            return Ok((true, join_strings(chk.get("engineering_violations"))));
        }
        if !chk.get("ok").is_some_and(truthy) {
            return Ok((false, join_strings(chk.get("issues"))));
        }
    }
    Ok((true, String::new()))
}


pub fn primal_sensitivity_consistency(
    declaration: Option<&Value>,
    primal: f64,
    sensitivity: f64,
) -> CaeResult<(f64, f64)> {
    let (mut rtol, mut atol) = (1.0e-9, 1.0e-11);
    if let Some(raw) = declaration {
        let Some(m) = raw.as_object().filter(|m| {
            m.len() == 2 && m.contains_key("relative_tolerance") && m.contains_key("absolute_tolerance")
        }) else {
            return contract("provider primal/sensitivity consistency declaration is malformed");
        };
        let (r, a) = (&m["relative_tolerance"], &m["absolute_tolerance"]);
        if !r.is_number() || !a.is_number() {
            return contract("provider primal/sensitivity consistency tolerances must be numeric");
        }
        rtol = num(Some(r));
        atol = num(Some(a));
        if !rtol.is_finite()
            || !atol.is_finite()
            || rtol < 0.0
            || atol < 0.0
            || rtol > 1.0e-4
            || atol > 1.0e-8
        {
            return contract(
                "provider primal/sensitivity consistency tolerance exceeds the bounded numerical contract",
            );
        }
    }
    let (p, s) = (primal, sensitivity);
    if !p.is_finite() || !s.is_finite() || (s - p).abs() > atol + rtol * p.abs() {
        let scale = p.abs().max(s.abs()).max(f64::MIN_POSITIVE);
        return contract(format!(
            "primal and sensitivity paths disagree at the same candidate design: primal={}, sensitivity={}, relative_delta={}, rtol={}, atol={}",
            implexity_optim::numeric::format_g(p, 17),
            implexity_optim::numeric::format_g(s, 17),
            implexity_optim::numeric::format_g((p - s).abs() / scale, 6),
            implexity_optim::numeric::format_g(rtol, 6),
            implexity_optim::numeric::format_g(atol, 6),
        ));
    }
    Ok((rtol, atol))
}

fn response_path_consistency(
    program: &StageResponseProgram,
    declaration: Option<&Value>,
    primal: &BTreeMap<String, f64>,
    sensitivity: &BTreeMap<String, f64>,
) -> CaeResult<()> {
    let mut scales: BTreeMap<&str, f64> = BTreeMap::new();
    for spec in &program.responses {
        let s = spec.scale.as_f64();
        let entry = scales.entry(spec.name.as_str()).or_insert(s);
        *entry = entry.min(s);
    }
    let keys: BTreeSet<&String> = program.location.keys().collect();
    if primal.keys().collect::<BTreeSet<_>>() != keys || sensitivity.keys().collect::<BTreeSet<_>>() != keys {
        return contract("response consistency operating-point coverage differs");
    }
    for (key, (name, point)) in &program.location {
        let scale = scales.get(name.as_str()).copied().unwrap_or(1.0);
        primal_sensitivity_consistency(declaration, primal[key] / scale, sensitivity[key] / scale).map_err(
            |e| {
                CaeError::contract(format!(
                    "response {} at operating_point={point}: {}",
                    repr(name),
                    e.message()
                ))
            },
        )?;
    }
    Ok(())
}


pub fn accept_design(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    designs: &NamedArrays,
    points: &[i64],
) -> CaeResult<()> {
    let require_identity = if provides(provider, DesignOp::OptimizerLifecycle) {
        let config = lifecycle(provider, None, None, Some(problem))?;
        if designs.names().iter().any(|n| !config.design_coordinates.contains(n)) {
            return contract("commit design coordinates disagree with optimizer lifecycle");
        }
        match config.acceptance_operation.as_deref() {
            None => return Ok(()),
            Some(op) if DesignOp::from_name(op) == Some(DesignOp::AcceptDesign) => {}
            Some(op) => {
                return contract(format!(
                    "provider acceptance operation {} is not an accepted-design commit",
                    repr(op)
                ));
            }
        }
        config.require_identity_evidence
    } else {
        false
    };
    let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::AcceptDesign)) else {
        return Ok(());
    };
    let expected = design_identity(designs)?;
    for point in points {
        let point = usize::try_from(*point).unwrap_or(usize::MAX);
        check_operating_point(provider, DesignOp::AcceptDesign, point)?;
        if point != 0 {

            return contract("provider accept_design has no operating-point dispatch in this runtime");
        }
        let owned = designs.owned_design()?;
        let result = ops.accept_design(problem, &owned)?;
        if require_identity
            && result.as_ref().and_then(|r| r.get("design_state_id")).and_then(Value::as_str)
                != Some(expected.as_str())
        {
            return contract("accepted-design commit omitted matching design_state_id acknowledgement");
        }
        if let Some(r) = result {
            let returned = r.get("design_state_id").or_else(|| r.get("designStateId"));
            if let Some(v) = returned.filter(|v| !v.is_null())
                && v.as_str() != Some(expected.as_str())
            {
                return contract("accepted-design commit returned a stale or mismatched design_state_id");
            }
        }
    }
    Ok(())
}


pub fn admit_candidate_design(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    current: &NamedArrays,
    candidate: &NamedArrays,
    operation: Option<DesignOp>,
    require_identity_evidence: bool,
) -> CaeResult<CandidateAdmission> {
    let Some(op) = operation else {
        return Ok(CandidateAdmission {
            current_design_state_id: Some(design_identity(current)?),
            candidate_design_state_id: Some(design_identity(candidate)?),
            ..CandidateAdmission::default()
        });
    };
    provider_candidate_admission(
        provider,
        problem,
        &CandidateDesign::Named(current.clone()),
        &CandidateDesign::Named(candidate.clone()),
        op,
        true,
        require_identity_evidence,
    )
}

#[derive(Debug, Clone)]
enum Projector {
    Scalar { lo: f64, hi: f64 },
    Array { lo: Bound, hi: Bound },
}

impl Projector {
    fn project(
        &self,
        x: &ArrayD<f64>,
        masks: &BoxMasks,
        initial: &ArrayD<f64>,
        provider: &dyn CaeProvider,
        problem: &ProviderProblem,
    ) -> CaeResult<ArrayD<f64>> {
        match self {
            Self::Scalar { lo, hi } => project_with_masks(x, *lo, *hi, masks, initial, provider, problem),
            Self::Array { lo, hi } => project_array_box(x, lo, hi, masks, initial, provider, problem),
        }
    }
}

struct StageExact {
    provider: Arc<dyn CaeProvider>,
    problem: ProviderProblem,
    problem_doc: Option<Value>,
    program: StageResponseProgram,
    stage: ScheduleStage,
    observe: bool,
    primal: Option<(String, BTreeMap<String, f64>)>,
    exact_values: Vec<BTreeMap<String, f64>>,
    stashed_error: Option<SearchError>,
}

impl StageExact {
    fn screen(
        &self,
        mut diagnostics: Map<String, Value>,
        candidate: bool,
    ) -> SearchResult<Map<String, Value>> {
        let (ok, message) = regime_ok(
            &mut diagnostics,
            self.problem_doc.as_ref(),
            self.stage.validity_limits.as_ref(),
            self.observe,
        )?;
        if candidate && !ok {
            if self.stage.regime_action == "refuse" {
                return Err(SearchError::Fatal(CaeError::contract(format!(
                    "{}: validity regime left: {message}",
                    self.stage.id
                ))));
            }
            if self.stage.regime_action == "reject_step" {
                return Err(SearchError::Trial(TrialFailure::Rejection {
                    message: if message.is_empty() { "validity regime left".into() } else { message },
                    kind: "regime_rejected".into(),
                    decision: "regime_rejected".into(),
                }));
            }
        }
        Ok(diagnostics)
    }

    fn sensitivities(&mut self, design: &NamedArrays, trial: bool) -> SearchResult<ExactPoint> {
        let owned = design.owned_design()?;
        let mut values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        let mut per_point = Vec::new();
        for point in self.program.points.clone() {
            let raw = crate::solver_recovery::once("provider_sensitivity", || design_sensitivities(
                self.provider.as_ref(),
                &self.problem,
                &owned,
                &self.program.names,
                trial,
                usize::try_from(point).unwrap_or(usize::MAX),
            ))?;
            let mut point_diagnostics = raw.diagnostics.clone();
            let cached_point = usize::try_from(point).map_err(|_| CaeError::contract("operating point must be nonnegative and platform-representable"))?;
            let cached = design_operations(self.provider.as_ref()).and_then(|ops| ops.cached_evaluation_design(&self.problem, &owned, cached_point));
            let computed = match cached {
                Some(Ok(implexity_optim::provider_ops::CachedEvaluation::Available(e))) => e.responses,
                _ => raw.responses.clone(),
            };
            point_diagnostics.insert("diagnostic_responses".into(), diagnostic_response_rows(self.provider.as_ref(), &computed, point));
            per_point.push(point_diagnostics);
            for name in &self.program.names {
                let key = point_key(point, name);
                let value =
                    raw.responses.get(name).copied().ok_or_else(|| {
                        CaeError::contract(format!("provider omitted response {}", repr(name)))
                    })?;
                values.insert(key.clone(), value);
                let rows = raw.gradients.get(name).ok_or_else(|| {
                    CaeError::contract(format!("provider omitted gradients of {}", repr(name)))
                })?;
                let mut g = NamedArrays::new();
                for (coordinate, _) in owned.iter() {
                    let row = rows.get(coordinate).ok_or_else(|| {
                        CaeError::contract(format!("provider omitted gradient {coordinate}"))
                    })?;
                    g.insert(coordinate, row.clone());
                }
                gradients.insert(key, g);
            }
        }
        let diagnostics = self.screen(robust_diagnostics(&self.program, &per_point), trial)?;
        let identity = design_identity(&owned)?;
        if trial
            && let Some((primal_id, primal_values)) = &self.primal
            && *primal_id == identity
        {
            let declaration = crate::provider_hooks::with_hooks(self.provider.as_ref(), |h| {
                h.primal_sensitivity_consistency(&self.problem)
            })
            .transpose()?;
            response_path_consistency(&self.program, declaration.as_ref(), primal_values, &values)?;
        }
        self.primal = None;
        Ok(ExactPoint { design: owned, values, gradients, diagnostics })
    }

    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues> {
        let owned = design.owned_design()?;
        let mut values = BTreeMap::new();
        let mut per_point = Vec::new();
        for point in self.program.points.clone() {
            let evaluated = evaluation_values(
                self.provider.as_ref(),
                &self.problem,
                &owned,
                &self.program.names,
                usize::try_from(point).unwrap_or(usize::MAX),
            )?;
            let Some((point_values, diagnostics)) = evaluated else {
                let exact = self.sensitivities(&owned, true)?;
                return Ok((exact.values.clone(), exact.diagnostics.clone(), Some(exact)));
            };
            per_point.push(diagnostics);
            for name in &self.program.names {
                values.insert(point_key(point, name), point_values.get(name).copied().unwrap_or(f64::NAN));
            }
        }
        let diagnostics = self.screen(robust_diagnostics(&self.program, &per_point), true)?;
        self.primal = Some((design_identity(&owned)?, values.clone()));
        Ok((values, diagnostics, None))
    }
}

type StageRanked = (f64, NamedArrays, Option<ApproximateEvaluation>);

struct StageHooks {
    exact: Rc<RefCell<StageExact>>,
    masks: BoxMasks,
    coordinate_masks: BTreeMap<String, ArrayD<bool>>,
    run_initial: NamedArrays,
    stage_entry: NamedArrays,
    active: Vec<String>,
    primary: String,
    projector: Projector,
    admission_operation: Option<DesignOp>,
    admission_identity: bool,
    requested_preview: Option<ComputationEffortPolicy>,
    preview_cadence: i64,
    requested_updates: i64,
    stage_iteration: i64,
    preview_record: Option<Map<String, Value>>,
    preview_lane: Option<ApproximationLane>,
    ranked: Vec<StageRanked>,
    ranked_count: usize,
}

fn flatten(names: &[String], values: &NamedArrays) -> ArrayD<f64> {
    let data: Vec<f64> = names.iter().filter_map(|n| values.get(n)).flat_map(|a| a.iter().copied()).collect();
    let n = data.len();
    ArrayD::from_shape_vec(IxDyn(&[n]), data).unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])))
}

fn unflatten(names: &[String], shapes: &[Vec<usize>], flat: &ArrayD<f64>) -> CaeResult<NamedArrays> {
    let data: Vec<f64> = flat.iter().copied().collect();
    let mut out = NamedArrays::new();
    let mut cursor = 0;
    for (name, shape) in names.iter().zip(shapes) {
        let size: usize = shape.iter().product();
        let chunk = data
            .get(cursor..cursor + size)
            .ok_or_else(|| CaeError::contract("flat design is shorter than its layout"))?;
        out.insert(
            name.clone(),
            ArrayD::from_shape_vec(IxDyn(shape), chunk.to_vec())
                .map_err(|_| CaeError::contract("flat design layout drifted"))?,
        );
        cursor += size;
    }
    Ok(out)
}

impl StageHooks {
    fn begin_iteration(&mut self, stage_iteration: i64) {
        self.stage_iteration = stage_iteration;
        self.preview_record = None;
    }

    fn provider(&self) -> (Arc<dyn CaeProvider>, ProviderProblem) {
        let e = self.exact.borrow();
        (Arc::clone(&e.provider), e.problem.clone())
    }
}

impl StageSearchHooks for StageHooks {
    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues> {
        self.exact.borrow_mut().values(design)
    }

    fn sensitivities(&mut self, design: &NamedArrays) -> SearchResult<ExactPoint> {
        self.exact.borrow_mut().sensitivities(design, true)
    }

    fn project(&mut self, _current: &NamedArrays, unprojected: NamedArrays) -> CaeResult<NamedArrays> {
        let mut trial = freeze_inactive(&unprojected, &self.stage_entry, &self.active)?;
        for (name, mask) in &self.coordinate_masks {
            let (Some(t), Some(init)) = (trial.get_mut(name), self.run_initial.get(name)) else { continue };
            Zip::from(t).and(mask).and(init).for_each(|t, m, i| {
                if !*m {
                    *t = *i;
                }
            });
        }
        if self.active.contains(&self.primary) {
            let (provider, problem) = self.provider();
            let x = trial.get(&self.primary).cloned().unwrap_or_default();
            let initial = self.run_initial.get(&self.primary).cloned().unwrap_or_default();
            let y = self.projector.project(&x, &self.masks, &initial, provider.as_ref(), &problem)?;
            trial.insert(self.primary.clone(), y);
        }
        for (name, mask) in &self.coordinate_masks {
            let (Some(t), Some(init)) = (trial.get(name), self.run_initial.get(name)) else { continue };
            #[allow(clippy::float_cmp)]
            if t.iter().zip(init.iter()).zip(mask.iter()).any(|((t, i), m)| !*m && t != i) {
                return contract(format!("projection altered locked entries of {name}"));
            }
        }
        Ok(trial)
    }

    fn admit(&mut self, current: &NamedArrays, trial: &NamedArrays) -> CaeResult<Option<CandidateAdmission>> {
        let (provider, problem) = self.provider();
        let admission = admit_candidate_design(
            provider.as_ref(),
            &problem,
            current,
            trial,
            self.admission_operation,
            self.admission_identity,
        )?;
        if admission.relinearize_after_accept {
            return contract(
                "hierarchical candidate admission requested unsupported post-admission relinearization",
            );
        }
        Ok(Some(admission))
    }

    fn accept(
        &mut self,
        _previous: &NamedArrays,
        design: &NamedArrays,
        _admission: Option<&CandidateAdmission>,
        point: ExactPoint,
    ) -> CaeResult<(Option<Value>, ExactPoint)> {
        let (provider, problem) = self.provider();
        let points = self.exact.borrow().stage.operating_points.clone();
        accept_design(provider.as_ref(), &problem, design, &points)?;
        Ok((None, point))
    }

    #[allow(clippy::too_many_lines)]
    fn candidates(
        &mut self,
        search: &StageSearch,
        direction: &NamedArrays,
        step: f64,
    ) -> CaeResult<Option<Vec<(f64, NamedArrays, usize)>>> {
        let Some(requested) = self.requested_preview.clone() else { return Ok(None) };
        let settings = &search.settings;
        let current = &search.current.point.design;
        let names = current.names();
        let shapes: Vec<Vec<usize>> =
            names.iter().map(|n| current.get(n).map(|a| a.shape().to_vec()).unwrap_or_default()).collect();
        let interactive = requested.mode == ComputationMode::InteractivePreview;
        let move_limit = settings.move_limit.as_f64();
        let mut raw: Vec<(f64, NamedArrays)> = Vec::new();
        let mut update_count = 1_i64;
        if interactive {
            update_count = self
                .preview_cadence
                .min(self.requested_updates - self.stage_iteration * self.preview_cadence)
                .max(1);
            #[allow(clippy::cast_precision_loss)]
            let update_step = step.min(move_limit / update_count as f64);
            let mut proposal = current.clone();
            for update in 0..update_count {
                let trial = search.trial_from(self, &proposal, direction, update_step)?;
                if eq_arrays(&trial, &proposal, &names) {
                    break;
                }
                #[allow(clippy::cast_precision_loss)]
                raw.push((((update + 1) as f64 * update_step).min(move_limit), trial.clone()));
                proposal = trial;
            }
        } else {
            let mut trial_step = step;
            while trial_step >= settings.minimum_step_fraction.as_f64() {
                let trial = search.trial_from(self, current, direction, trial_step)?;
                if !eq_arrays(&trial, current, &names) {
                    raw.push((trial_step, trial));
                }
                trial_step *= settings.backtracking.as_f64();
            }
        }
        let exact = Rc::clone(&self.exact);
        let (closure_names, closure_shapes) = (names.clone(), shapes.clone());
        let program = search.program.clone();
        let bound_state = search.bound_state.clone();
        let exact_fn: ExactEvaluateFn = Box::new(move |target: &ArrayD<f64>| {
            let design = unflatten(&closure_names, &closure_shapes, target)?;
            let evaluated = exact.borrow_mut().values(&design);
            let values = match evaluated {
                Ok((values, _, _)) => values,
                Err(e) => {
                    let error = CaeError::from(e.clone());
                    exact.borrow_mut().stashed_error = Some(e);
                    return Err(error);
                }
            };
            exact.borrow_mut().exact_values.push(values.clone());
            let total = program.aggregate(&values, &bound_state)?.total;
            Ok(ArrayD::from_elem(IxDyn(&[1]), total))
        });
        let spans: NamedArrays =
            NamedArrays::from_pairs(search.coordinates.iter().map(|c| (c.name.clone(), c.span())));
        let flat_candidates: Vec<(f64, ArrayD<f64>)> =
            raw.iter().map(|(s, t)| (*s, flatten(&names, t))).collect();
        let (ranked, ranked_count, lane) = preview_ranked_candidates(
            &flatten(&names, current),
            &flatten(&names, &search.current.gradients),
            search.current.total,
            flat_candidates,
            &requested,
            exact_fn,
            &flatten(&names, &spans),
        )?;
        let anchor = design_identity(current)?;
        let unflat = |t: &ArrayD<f64>| unflatten(&names, &shapes, t);
        let budget = self.exact.borrow().stage.iterations;
        let record = preview_record(
            &requested,
            &anchor,
            &ranked,
            ranked_count,
            &unflat,
            self.stage_iteration * self.preview_cadence,
            update_count,
            self.stage_iteration,
            budget,
        )?;
        self.preview_record = Some(record);
        self.preview_lane = Some(lane);
        self.ranked_count = ranked_count;
        let ordered: Vec<_> = if interactive { ranked.into_iter().rev().collect() } else { ranked };
        let mut out = Vec::new();
        self.ranked.clear();
        for (index, (s, flat, preview)) in ordered.into_iter().enumerate() {
            let design = unflatten(&names, &shapes, &flat)?;
            self.ranked.push((s, design.clone(), preview));
            out.push((s, design, index));
        }
        Ok(Some(out))
    }

    fn evaluate_candidate(
        &mut self,
        _search: &StageSearch,
        step: f64,
        design: &NamedArrays,
        meta: usize,
    ) -> SearchResult<TrialValues> {
        let Some((_, _, Some(preview))) = self.ranked.get(meta).cloned() else {
            return self.values(design);
        };
        {
            let mut e = self.exact.borrow_mut();
            e.exact_values.clear();
            e.stashed_error = None;
        }
        let lane =
            self.preview_lane.as_mut().ok_or_else(|| CaeError::contract("preview lane is unavailable"))?;
        let corrected = match lane.correct(&preview, false) {
            Ok(c) => c,
            Err(e) => {
                let error = self
                    .exact
                    .borrow_mut()
                    .stashed_error
                    .take()
                    .unwrap_or_else(|| SearchError::from(CaeError::from(e)));
                if matches!(error, SearchError::Trial(_))
                    && let Some(Value::Array(corrections)) =
                        self.preview_record.as_mut().and_then(|r| r.get_mut("exact_corrections"))
                {
                    corrections.push(serde_json::json!({
                        "candidate_rank": meta + 1,
                        "candidate_design_state_id": design_identity(design)?,
                        "status": "failed_convergence",
                    }));
                }
                return Err(error);
            }
        };
        let values = self
            .exact
            .borrow_mut()
            .exact_values
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
            meta + 1,
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

    fn trial_decision(
        &mut self,
        iteration: i64,
        attempt: i64,
        decision: &str,
        current: &NamedArrays,
        candidate: &NamedArrays,
        step: f64,
        info: &TrialInfo,
    ) -> CaeResult<()> {
        trial_progress(iteration, attempt, decision, current, Some(candidate), step, info)
    }

    fn recover(&mut self, search: &StageSearch) -> CaeResult<Option<(ExactPoint, Value)>> {
        let released = implexity_solve::numerical_state::reset_all();
        if released.iter().all(|(_, n)| *n == 0) {
            return Ok(None);
        }
        let design = search.current.point.design.clone();
        let started = Instant::now();
        let evaluated = self.exact.borrow_mut().sensitivities(&design, false);
        let mut record = Map::new();
        record.insert(
            "released".into(),
            Value::Object(released.iter().map(|(n, c)| (n.clone(), Value::from(*c))).collect()),
        );
        record.insert("reevaluated_design_state_id".into(), Value::String(design_identity(&design)?));
        record.insert("reevaluation_wall_s".into(), fv(started.elapsed().as_secs_f64()));
        let point = match evaluated {
            Ok(point) => point,
            Err(SearchError::Fatal(e)) => return Err(e),
            Err(SearchError::Trial(t)) => {
                let message: String = t.message().chars().take(500).collect();
                emit_line(
                    "RECOVERY ",
                    &serde_json::json!({"status": "reevaluation_failed", "message": message}),
                );
                return Ok(None);
            }
        };
        let (provider, problem) = self.provider();
        let points = self.exact.borrow().stage.operating_points.clone();
        accept_design(provider.as_ref(), &problem, &design, &points)?;
        let previous = search.current.total;
        let replay = search.combine(point.clone())?.total;
        record.insert("objective_before".into(), fv(previous));
        record.insert("objective_reevaluated".into(), fv(replay));
        emit_line("RECOVERY ", &Value::Object(record.clone()));
        Ok(Some((point, Value::Object(record))))
    }
}

#[derive(Clone)]
struct StageContext {
    provider: Arc<dyn CaeProvider>,
    active: Vec<String>,
    admission_operation: Option<DesignOp>,
    admission_identity: bool,
}

fn has_staged_initializer(provider: &dyn CaeProvider) -> bool {
    provider.as_any().downcast_ref::<IntentOrchestratedProvider>().is_some()
}

struct Run {
    problem: ProviderProblem,
    problem_doc: Option<Value>,
    stages: Vec<ScheduleStage>,
    blocks: Vec<DesignBlock>,
    coords: Vec<String>,
    primary: String,
    search_settings: CoordinateOptimizationSettings,
    masks: BoxMasks,
    coordinate_masks: BTreeMap<String, ArrayD<bool>>,
    run_initial: NamedArrays,
    bounds: BTreeMap<String, Bounds>,
    projector: Projector,
    step_scales: BTreeMap<String, f64>,
    update_metric: Map<String, Value>,
    responses: Vec<ResponseSpec>,
    response_normalization: Option<Value>,
    normalization_active: bool,
    normalization_record: Option<Value>,
    requested_preview: Option<ComputationEffortPolicy>,
    preview_cadence: i64,
    requested_stage_updates: BTreeMap<String, i64>,
    provider_descriptor_sha256: String,
    contexts: BTreeMap<usize, StageContext>,
}

impl Run {
    fn stage_context(&mut self, si: usize) -> CaeResult<StageContext> {
        if let Some(c) = self.contexts.get(&si) {
            return Ok(c.clone());
        }
        let stage = &self.stages[si];
        let provider = implexity_core::registries::global().providers.get(&stage.provider)?;
        let (caps, capcoords, _responses, sensitivities) = provider_capabilities(provider.as_ref())?;
        let declared = design_operations(provider.as_ref())
            .is_some_and(|o| o.provides(DesignOp::OptimizerLifecycle) && !o.lifecycle_is_problem_specific());
        let (admission_operation, admission_identity) = if declared {
            let config = lifecycle(provider.as_ref(), None, Some(&capcoords), None)?;
            let op = match config.candidate_admission_operation.as_deref() {
                None => None,
                Some(name) => Some(DesignOp::from_name(name).ok_or_else(|| {
                    CaeError::contract(format!(
                        "provider declares unknown admission operation {}",
                        repr(name)
                    ))
                })?),
            };
            (op, config.require_identity_evidence)
        } else {
            (
                provides(provider.as_ref(), DesignOp::CandidateDesignAdmission)
                    .then_some(DesignOp::CandidateDesignAdmission),
                false,
            )
        };
        require_provider_child_execution(&caps, &stage.id)?;
        if implexity_core::wire::fingerprint_value(&Value::Object(caps.to_map()))
            != self.provider_descriptor_sha256
        {
            return contract(format!("{}: registered provider descriptor drifted within the run", stage.id));
        }
        if !sensitivities {
            return contract(format!("{}: provider has no declared sensitivities", stage.id));
        }
        let active = active_coordinates(&self.blocks, Some(&stage.released_blocks), &self.coords)?;
        if active.is_empty() {
            return contract(format!("{}: no released coordinate", stage.id));
        }
        let missing: Vec<&String> = active.iter().filter(|c| !capcoords.contains(c)).collect();
        if !missing.is_empty() {
            return contract(format!(
                "{}: provider has no derivative for {}",
                stage.id,
                implexity_core::pyobj::list_repr(&missing)
            ));
        }
        let context = StageContext { provider, active, admission_operation, admission_identity };
        self.contexts.insert(si, context.clone());
        Ok(context)
    }

    fn program(&self, si: usize) -> CaeResult<StageResponseProgram> {
        let stage = &self.stages[si];
        StageResponseProgram::new(
            &stage.stage_responses(&self.responses)?,
            &stage.operating_points,
            &stage.robust_mode,
            stage.robust_beta,
        )
    }

    fn stage_search(
        &mut self,
        si: usize,
        entry: &NamedArrays,
        point_designs: &NamedArrays,
        state: Option<SearchState>,
        normalize: bool,
    ) -> CaeResult<(StageSearch, StageHooks)> {
        let context = self.stage_context(si)?;
        let stage = self.stages[si].clone();
        let program = self.program(si)?;
        let exact = Rc::new(RefCell::new(StageExact {
            provider: Arc::clone(&context.provider),
            problem: self.problem.clone(),
            problem_doc: self.problem_doc.clone(),
            program,
            observe: stage.regime_action == "warn",
            stage: stage.clone(),
            primal: None,
            exact_values: Vec::new(),
            stashed_error: None,
        }));
        let hooks = StageHooks {
            exact: Rc::clone(&exact),
            masks: self.masks.clone(),
            coordinate_masks: self.coordinate_masks.clone(),
            run_initial: self.run_initial.clone(),
            stage_entry: entry.clone(),
            active: context.active.clone(),
            primary: self.primary.clone(),
            projector: self.projector.clone(),
            admission_operation: context.admission_operation,
            admission_identity: context.admission_identity,
            requested_preview: self.requested_preview.clone(),
            preview_cadence: self.preview_cadence,
            requested_updates: self
                .requested_stage_updates
                .get(&stage.id)
                .copied()
                .unwrap_or(stage.iterations),
            stage_iteration: 0,
            preview_record: None,
            preview_lane: None,
            ranked: Vec::new(),
            ranked_count: 0,
        };
        let point = exact.borrow_mut().sensitivities(point_designs, false).map_err(CaeError::from)?;
        if normalize && self.normalization_record.is_none() && self.normalization_active {
            let program = exact.borrow().program.clone();
            let mut reference = BTreeMap::new();
            for name in &program.names {
                let peak = program
                    .points
                    .iter()
                    .map(|p| point.values.get(&point_key(*p, name)).copied().unwrap_or(f64::NAN).abs())
                    .fold(f64::NEG_INFINITY, f64::max);
                reference.insert(name.clone(), peak);
            }
            let (responses, record) = resolve_initial_response_normalization(
                &self.responses,
                &reference,
                self.response_normalization.as_ref(),
            )?;
            self.responses = responses;
            self.normalization_record = record;
            exact.borrow_mut().program = self.program(si)?;
        }
        let program = exact.borrow().program.clone();
        let state = state.unwrap_or_else(|| StageSearch::fresh_state(&program, &self.search_settings));
        let search = StageSearch::new(
            program,
            self.step_scales.clone(),
            self.update_metric.clone(),
            self.search_settings.clone(),
            stage_coordinates(point_designs, &self.bounds, &self.coordinate_masks, &context.active),
            point,
            state,
        )?;
        Ok((search, hooks))
    }

    fn carried_state(
        &self,
        program: &StageResponseProgram,
        previous: Option<&(StageResponseProgram, implexity_optim::bounds::BoundMultiplierState)>,
    ) -> CaeResult<Option<SearchState>> {
        let Some((previous_program, bound_state)) = previous else { return Ok(None) };
        let mut state = StageSearch::fresh_state(program, &self.search_settings);
        state.bound_state = program.carry(previous_program, bound_state)?;
        Ok(Some(state))
    }
}

fn load_coordinate(path: &Path, key: Option<&str>) -> CaeResult<ArrayD<f64>> {
    let archive = implexity_io::npz::load_file(path)
        .map_err(|e| CaeError::contract(format!("{}: {e}", path.display())))?;
    let files = archive.files();
    let key = match key {
        Some(k) => k.to_string(),
        None if files.contains(&"topology") => "topology".into(),
        None => files.first().map(|s| (*s).to_string()).unwrap_or_default(),
    };
    let raw = archive
        .get(&key)
        .ok_or_else(|| CaeError::contract(format!("{}: missing array {}", path.display(), repr(&key))))?;
    if is_complex(raw) {
        return contract(format!("{}: complex design data", path.display()));
    }
    raw.to_f64().ok_or_else(|| CaeError::contract(format!("{}: design data is not numeric", path.display())))
}

fn parse_topology_mask(
    masks: &Map<String, Value>,
    key: &str,
    shape: &[usize],
) -> CaeResult<Option<ArrayD<f64>>> {
    match masks.get(key) {
        None => Ok(None),
        Some(raw) => match crate::provider_worker::json_array(raw) {
            Some(a) if a.shape() == shape && a.iter().all(|v| v.is_finite()) => Ok(Some(a)),
            _ => contract(format!("invalid topology {key} mask")),
        },
    }
}


#[allow(clippy::too_many_lines)]
pub fn run(
    spec: &Map<String, Value>,
    out_dir: &Path,
    resume: bool,
    settings: &LegacySingleArrayOptimizationSettings,
    execution_started: Instant,
) -> JobResult<Map<String, Value>> {
    let mut resume = resume;
    implexity_core::contracts::reject_removed_constraint_keys(
        &Value::Object(spec.clone()),
        "hierarchical job specification",
    )?;
    let provider_name =
        spec.get("provider").filter(|v| truthy(v)).map(|v| py_str(v).trim().to_string()).unwrap_or_default();
    if provider_name.is_empty() {
        return Err(JobError::contract("hierarchical job requires a provider"));
    }
    let raw_schedule = spec.get("schedule");
    require_single_provider_schedule(&Value::String(provider_name.clone()), raw_schedule)?;
    if !resume {
        refuse_fresh_outputs(out_dir, spec)?;
    }
    let base = implexity_core::registries::global().providers.get(&provider_name)?;
    let (base_capabilities, supported_coordinates, _supported_responses, _sensitivities) =
        provider_capabilities(base.as_ref())?;
    require_provider_child_execution(&base_capabilities, &provider_name)?;
    let empty = Value::Object(Map::new());
    let problem = base.normalise_problem(spec.get("problem").filter(|v| truthy(v)).unwrap_or(&empty))?;
    let problem_doc = problem_json(base.as_ref(), &problem).ok().filter(Value::is_object);
    let (supported_coordinates, primary) =
        authoring_coordinates(base.as_ref(), &problem, &base_capabilities, &supported_coordinates)?;
    let mut responses: Vec<ResponseSpec> = spec
        .get("responses")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(ResponseSpec::from_dict).collect::<CaeResult<Vec<_>>>())
        .transpose()?
        .unwrap_or_default();
    if responses.is_empty() {
        return Err(JobError::contract("hierarchical job requires responses"));
    }
    let response_normalization = spec.get("response_normalization").filter(|v| !v.is_null()).cloned();
    let normalization_active =
        normalise_response_normalization(response_normalization.as_ref(), &responses)?.is_some();
    if let Some(check) = design_operations(base.as_ref()).and_then(|o| {
        o.validate_response_selection(&problem, &responses.iter().map(|r| r.name.clone()).collect::<Vec<_>>())
    }) {
        check?;
    }
    let coordinate_settings = settings.as_coordinate_settings();
    let mut designs = NamedArrays::new();
    let mut bounds: BTreeMap<String, Bounds> = BTreeMap::new();
    let mut step_scales: BTreeMap<String, f64> = BTreeMap::new();
    for row in spec.get("design_coordinates").and_then(Value::as_array).cloned().unwrap_or_default() {
        let Some(row) = row.as_object() else {
            return Err(JobError::contract("design coordinate declarations must be mappings"));
        };
        let name = row.get("coordinate").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        if name.is_empty() || designs.contains(&name) {
            return Err(JobError::contract("design coordinates must be unique"));
        }
        let file = match row.get("file").filter(|v| truthy(v)) {
            Some(f) => py_str(f),
            None if name == primary => "topology_initial.npz".into(),
            None => String::new(),
        };
        if file.is_empty() {
            return Err(JobError::contract(format!("{name}: design file required")));
        }
        let key = row.get("key").filter(|v| truthy(v)).map(py_str);
        let array = load_coordinate(&out_dir.join(&file), key.as_deref())?;
        let (lo, hi) = coordinate_bounds(
            &array,
            row.get("lower").unwrap_or(&Value::from(0)),
            row.get("upper").unwrap_or(&Value::from(1)),
            &name,
        )?;
        let b =
            Bounds { lower: lo.broadcast(array.shape()), upper: hi.broadcast(array.shape()), raw: (lo, hi) };
        if array.ndim() == 0 || !array.iter().all(|v| v.is_finite()) || !within(&array, &b, 1e-12) {
            return Err(JobError::contract(format!("{name}: invalid design coordinate")));
        }
        designs.insert(name.clone(), array);
        bounds.insert(name.clone(), b);
        step_scales.insert(
            name.clone(),
            coordinate_step_scale(Some(row.get("step_scale").unwrap_or(&Value::from(1.0))), &name)?,
        );
    }
    let projector = match bounds.get(&primary) {
        Some(b) if array_bounds(&b.raw.0, &b.raw.1) => {
            Projector::Array { lo: b.raw.0.clone(), hi: b.raw.1.clone() }
        }
        _ => Projector::Scalar {
            lo: coordinate_settings.coordinate_lower.as_f64(),
            hi: coordinate_settings.coordinate_upper.as_f64(),
        },
    };
    let coords = designs.names();
    if coords.first() != Some(&primary) {
        return Err(JobError::contract("the provider-declared primary design coordinate must be first"));
    }
    if coords.iter().any(|c| !supported_coordinates.contains(c)) {
        return Err(JobError::contract("job design coordinate is not declared by provider capabilities"));
    }
    validate_design_request(base.as_ref(), &coords, &responses, &problem)?;
    let raw_blocks = spec.get("design_blocks").and_then(Value::as_array);
    let blocks = validate_blocks(raw_blocks.map(Vec::as_slice), &coords)?;
    let op_count = problem_doc
        .as_ref()
        .and_then(|p| p.get("mission"))
        .and_then(Value::as_object)
        .and_then(|m| m.get("operatingPoints").filter(|v| truthy(v)).or_else(|| m.get("operating_points")))
        .and_then(Value::as_array)
        .map(Vec::len);
    let default_schedule;
    let schedule_rows: &[Value] = if let Some(rows) =
        raw_schedule.filter(|v| truthy(v)).and_then(Value::as_array)
    {
        rows
    } else {
        let released: Vec<String> =
            blocks.iter().filter(|b| FREE_ROLES.contains(&b.role.as_str())).map(|b| b.id.clone()).collect();
        default_schedule = vec![serde_json::json!({
            "id": "simultaneous", "provider": provider_name,
            "iterations": coordinate_settings.iterations, "released_blocks": released,
        })];
        &default_schedule
    };
    let mut stages = validate_schedule(Some(schedule_rows), &provider_name, &blocks, &coords, op_count)?;
    require_single_provider_schedule(
        &Value::String(provider_name.clone()),
        Some(&Value::Array(stages.iter().map(ScheduleStage::to_value).collect())),
    )?;

    for stage in &stages {
        stage.stage_responses(&responses)?;
    }
    let raw_solve_id = spec.get("solve_id").cloned().unwrap_or(Value::String("provider-job".into()));
    let solve_id = match raw_solve_id.as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err(JobError::contract("hierarchical job solve_id must be non-empty text")),
    };
    let update_metric = normalise_update_metric(spec.get("update_metric"))?;
    let mut coordinate_masks = coordinate_designable_masks(spec.get("coordinate_masks"), &designs)?;
    let matching = matching_time_config(spec)?;
    if !matching.is_empty() {
        let providers: BTreeSet<&str> = stages.iter().map(|s| s.provider.as_str()).collect();
        let points: BTreeSet<Vec<i64>> = stages.iter().map(|s| s.operating_points.clone()).collect();
        if providers != [provider_name.as_str()].into_iter().collect() {
            return Err(JobError::contract(
                "matching-time guess transport requires one provider lifecycle owner across every optimization stage",
            ));
        }
        if points != [vec![0]].into_iter().collect() {
            return Err(JobError::contract(
                "matching-time guess transport currently requires one nominal operating-point state owner",
            ));
        }
        validate_matching_time_lifecycle(&matching, base.as_ref(), &problem)?;
    }

    let topo_shape = designs.get(&primary).map(|a| a.shape().to_vec()).unwrap_or_default();
    let raw_masks = spec.get("masks").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut topo_free = ArrayD::from_elem(IxDyn(&topo_shape), true);
    let mut parsed_masks: BTreeMap<&str, ArrayD<f64>> = BTreeMap::new();
    for key in ["fixed_solid", "fixed_void", "preserve", "designable"] {
        if let Some(mask) = parse_topology_mask(&raw_masks, key, &topo_shape)? {
            if key == "designable" {
                Zip::from(&mut topo_free).and(&mask).for_each(|f, m| *f &= *m > 0.5);
            } else {
                Zip::from(&mut topo_free).and(&mask).for_each(|f, m| *f &= *m <= 0.5);
            }
            parsed_masks.insert(key, mask);
        }
    }
    let mut masks = BoxMasks::from_value(&raw_masks)?;
    if let Some(lock) = coordinate_masks.get(&primary).cloned() {
        let keep = lock.mapv(|v| !v);
        let primary_bounds = &bounds[&primary];
        let value = &designs.get(&primary).cloned().unwrap_or_default();
        for (key, target) in [("fixed_solid", &primary_bounds.upper), ("fixed_void", &primary_bounds.lower)] {
            if let Some(mask) = parsed_masks.get(key) {
                #[allow(clippy::float_cmp)]
                let conflict = keep
                    .iter()
                    .zip(mask.iter())
                    .zip(value.iter().zip(target.iter()))
                    .any(|((k, m), (v, t))| *k && *m > 0.5 && v != t);
                if conflict {
                    return Err(JobError::contract("coordinate lock conflicts with a fixed topology role"));
                }
            }
        }
        let prior =
            parsed_masks.get("preserve").cloned().unwrap_or_else(|| ArrayD::zeros(IxDyn(&topo_shape)));
        let mut preserve = ArrayD::zeros(IxDyn(&topo_shape));
        Zip::from(&mut preserve)
            .and(&prior)
            .and(&keep)
            .for_each(|p, a, k| *p = if *a > 0.5 || *k { 1.0 } else { 0.0 });
        masks.preserve = Some(preserve);
        Zip::from(&mut topo_free).and(&lock).for_each(|f, l| *f &= *l);
    }
    coordinate_masks.insert(primary.clone(), topo_free);
    let initial_primary = designs.get(&primary).cloned().unwrap_or_default();
    let projected = projector.project(&initial_primary, &masks, &initial_primary, base.as_ref(), &problem)?;
    designs.insert(primary.clone(), projected);
    let run_initial = designs.clone();

    let current_physics = physics_snapshot()?;
    let effort_binding = if let Some(b) = spec.get("computation_effort").filter(|v| !v.is_null()) {
        b.clone()
    } else {
        let mut scope = Map::new();
        scope.insert("provider".into(), Value::String(provider_name.clone()));
        scope.insert("solve_id".into(), Value::String(solve_id.clone()));
        let mut context = Map::new();
        context.insert("operation".into(), Value::String("provider_optimization".into()));
        context.insert(
            "scope_digest".into(),
            Value::String(implexity_core::wire::fingerprint_value(&Value::Object(scope))),
        );
        Value::Object(make_server_effort_binding(
            None,
            &provider_name,
            &base_capabilities,
            &current_physics,
            Some(&context),
            None,
        )?)
    };
    let facts = ProviderFacts {
        provider_name: &provider_name,
        capabilities: &base_capabilities,
        physics: &current_physics,
        candidates: None,
    };
    let (selection, effort_binding) = validate_effort_binding(&effort_binding, Some(&facts))?;
    let digest = |k: &str| effort_binding.get(k).map(py_str).unwrap_or_default();
    let requested_policy_digest = digest("requested_policy_digest");
    let effective_effort_digest = digest("effective_effort_digest");
    let operation_context_digest = digest("operation_context_digest");
    let coupling_policy = effort_binding.get("coupling_approximation").cloned().unwrap_or(Value::Null);
    let requested_preview = preview_policy(&selection);
    let (runtime_source, runtime_source_sha256) =
        runtime_source_binding(&implexity_solve::matching_time_guess::runtime_source_identity())?;
    let package_identity = stable_package_identity(&current_physics);
    let package_identity_sha256 =
        implexity_core::wire::fingerprint_value(&implexity_core::wire::to_wire(&package_identity)?);
    let descriptor_value = Value::Object(base_capabilities.to_map());
    let provider_descriptor_sha256 = implexity_core::wire::fingerprint_value(&descriptor_value);
    let provider_profile = effort_binding.get("provider_profile").cloned().unwrap_or(Value::Null);
    let provider_profile_sha256 =
        implexity_core::wire::fingerprint_value(&implexity_core::wire::to_wire(&provider_profile)?);
    let document_content_id = match spec.get("document_content_id").or_else(|| spec.get("content_id")) {
        None => "undeclared-document".to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => {
            return Err(JobError::contract(
                "hierarchical job document content identity must be non-empty text",
            ));
        }
    };
    let initial_design_state_id = design_identity(&run_initial)?;
    let mut fingerprint_input = Map::new();
    fingerprint_input.insert("schema".into(), Value::String(RUN_IDENTITY_SCHEMA.into()));
    fingerprint_input
        .insert("spec".into(), implexity_core::wire::to_wire(&matching_time_fingerprint_spec(spec)?)?);
    fingerprint_input.insert("settings".into(), crate::provider_job::settings_identity(spec, settings));
    fingerprint_input
        .insert("feasible_initial_design".into(), Value::String(initial_design_state_id.clone()));
    fingerprint_input.insert(
        "initial_coordinate_design_state_ids".into(),
        Value::Object(coordinate_identities(&run_initial)?),
    );
    fingerprint_input.insert(
        "normalized_schedule".into(),
        Value::Array(stages.iter().map(ScheduleStage::to_value).collect()),
    );
    fingerprint_input.insert("provider_descriptor".into(), descriptor_value);
    fingerprint_input.insert("package_identity".into(), package_identity);
    fingerprint_input.insert("runtime_source_identity".into(), runtime_source);
    fingerprint_input.insert("provider_profile".into(), provider_profile);
    fingerprint_input.insert("solve_id".into(), Value::String(solve_id.clone()));
    fingerprint_input.insert("document_content_id".into(), Value::String(document_content_id.clone()));
    fingerprint_input
        .insert("requested_policy_digest".into(), Value::String(requested_policy_digest.clone()));
    fingerprint_input
        .insert("effective_effort_digest".into(), Value::String(effective_effort_digest.clone()));
    fingerprint_input
        .insert("operation_context_digest".into(), Value::String(operation_context_digest.clone()));
    let run_fingerprint = implexity_core::wire::fingerprint_value(&Value::Object(fingerprint_input));
    let requested_stage_updates: BTreeMap<String, i64> =
        stages.iter().map(|s| (s.id.clone(), s.iterations)).collect();
    let requested_updates_total: i64 = stages.iter().map(|s| s.iterations).sum();
    let preview_cadence = match &requested_preview {
        Some(r) if r.mode == ComputationMode::InteractivePreview => r.exact_correction_cadence,
        _ => 1,
    };
    if preview_cadence > 1 {
        for stage in &mut stages {
            stage.iterations = (stage.iterations + preview_cadence - 1) / preview_cadence;
        }
    }
    let identity_values = [
        &run_fingerprint,
        &runtime_source_sha256,
        &package_identity_sha256,
        &provider_descriptor_sha256,
        &provider_profile_sha256,
        &requested_policy_digest,
        &effective_effort_digest,
        &operation_context_digest,
        &solve_id,
        &document_content_id,
    ];
    let mut identities = Map::new();
    for (k, v) in IDENTITY_KEYS.iter().zip(identity_values) {
        identities.insert((*k).into(), Value::String(v.clone()));
    }
    let epoch_provenance = crate::epoch_state::run_provenance(spec, &provider_name, &identities);

    let mut run = Run {
        problem: problem.clone(),
        problem_doc: problem_doc.clone(),
        stages: stages.clone(),
        blocks: blocks.clone(),
        coords: coords.clone(),
        primary: primary.clone(),
        search_settings: coordinate_settings.clone(),
        masks: masks.clone(),
        coordinate_masks: coordinate_masks.clone(),
        run_initial: run_initial.clone(),
        bounds: bounds.clone(),
        projector: projector.clone(),
        step_scales: step_scales.clone(),
        update_metric: update_metric.clone(),
        responses: responses.clone(),
        response_normalization: response_normalization.clone(),
        normalization_active,
        normalization_record: None,
        requested_preview: requested_preview.clone(),
        preview_cadence,
        requested_stage_updates: requested_stage_updates.clone(),
        provider_descriptor_sha256: provider_descriptor_sha256.clone(),
        contexts: BTreeMap::new(),
    };

    let mut history: Vec<Value> = Vec::new();
    let mut best = f64::INFINITY;
    let mut first: Option<f64> = None;
    let mut start_stage = 0_usize;
    let mut start_local = 0_i64;
    let mut best_design_state_id = initial_design_state_id.clone();
    let matching_time_consumed: Option<Value>;
    let ckpt = out_dir.join("ckpt.npz");
    let mut coupling_initializer_attempted = false;
    let mut coupling_initialization: Option<Value> = None;

    let mut continuation_seed: Option<Value> = None;
    if !resume && let Some(raw) = spec.get("continuation").filter(|v| !v.is_null()) {
        let record = seed_continuation(
            raw,
            out_dir,
            &ResumeInputs {
                coords: &coords,
                run_initial: &run_initial,
                bounds: &bounds,
                coordinate_masks: &coordinate_masks,
                blocks: &blocks,
                stages: &stages,
                responses: &responses,
                response_normalization: response_normalization.as_ref(),
                problem_doc: problem_doc.as_ref(),
                primary: &primary,
                solve_id: &solve_id,
                identities: &identities,
                settings: &coordinate_settings,
                step_scales: &step_scales,
            },
            &initial_design_state_id,
            &epoch_provenance,
        )?;
        emit_line("CONTINUATION ", &record);
        continuation_seed = Some(record);
        resume = true;
    }
    if resume {
        if let Some(recovery) = crate::epoch_state::restore_committed_generation(out_dir, &identities, &solve_id)? { emit_line("GENERATION_RECOVERY ", &recovery); }
        if !ckpt.is_file() {
            return Err(JobError::contract("resume requested without checkpoint"));
        }
        let resumed = load_resume_generation(
            &ckpt,
            &out_dir.join("history.json"),
            out_dir,
            &ResumeInputs {
                coords: &coords,
                run_initial: &run_initial,
                bounds: &bounds,
                coordinate_masks: &coordinate_masks,
                blocks: &blocks,
                stages: &stages,
                responses: &responses,
                response_normalization: response_normalization.as_ref(),
                problem_doc: problem_doc.as_ref(),
                primary: &primary,
                solve_id: &solve_id,
                identities: &identities,
                settings: &coordinate_settings,
                step_scales: &step_scales,
            },
            continuation_seed.as_ref().is_some_and(|c| c["mode"] == "reproduce"),
        )?;
        designs = resumed.designs;

        let moved = crate::epoch_state::set_aside_uncommitted(out_dir, resumed.history.len())?;
        if !moved.is_empty() {
            emit_line("EPOCH_STATE_SET_ASIDE ", &serde_json::json!({"files": moved}));
        }
        start_stage = resumed.stage_index;
        start_local = resumed.local_next;
        let _ = resumed.termination_reason;
        history = resumed.history;
        first = Some(resumed.first);
        best = resumed.best;
        best_design_state_id = resumed.best_design_state_id;
        let _ = resumed.best_history_index;
        matching_time_consumed = resumed.matching_time_consumed;
        responses = resumed.responses;
        run.responses.clone_from(&responses);
        run.normalization_record = resumed.normalization_record;
        coupling_initialization = resumed.coupling_initialization;
        let current_primary = designs.get(&primary).cloned().unwrap_or_default();
        let reprojected = projector.project(
            &current_primary,
            &masks,
            &initial_primary_of(&run_initial, &primary),
            base.as_ref(),
            &problem,
        )?;
        if reprojected != current_primary {
            return Err(JobError::contract("resume checkpoint design requires projection repair"));
        }
        let consume = matching.get("consume").and_then(Value::as_object);
        if consume.is_some_and(|c| c.get("required").is_none_or(truthy)) && matching_time_consumed.is_none() {
            return Err(JobError::contract(
                "resume checkpoint lost required matching-time consumption evidence",
            ));
        }
        let evidence_path = out_dir.join("matching_time_guess_consumed.json");
        if evidence_path.is_file() {
            let sidecar = std::fs::read_to_string(&evidence_path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| public_descriptor(&v).ok())
                .ok_or_else(|| JobError::contract("resume matching-time evidence sidecar is malformed"))?;
            if Some(&Value::Object(sidecar)) != matching_time_consumed.as_ref() {
                return Err(JobError::contract(
                    "resume checkpoint matching-time evidence is stale or mismatched",
                ));
            }
        } else if matching_time_consumed.is_some() {
            return Err(JobError::contract("resume matching-time evidence sidecar is missing"));
        }
    } else {
        matching_time_consumed =
            install_matching_time_guess(&matching, &provider_name, base.as_ref(), &problem, &designs)?;
        if let Some(consumed) = &matching_time_consumed {
            atomic_json(&out_dir.join("matching_time_guess_consumed.json"), consumed)?;
        }
        write_design(&out_dir.join("initial.npz"), &run_initial, &solve_id)?;
        let mut sidecar = Map::new();
        sidecar.insert("schema".into(), Value::String("implexity-hierarchical-initial-design/1".into()));
        sidecar.insert("design_state_id".into(), Value::String(initial_design_state_id.clone()));
        sidecar.insert(
            "coordinate_design_state_ids".into(),
            Value::Object(coordinate_identities(&run_initial)?),
        );
        for (k, v) in &identities {
            sidecar.insert(k.clone(), v.clone());
        }
        atomic_json(&out_dir.join("initial_design.json"), &Value::Object(sidecar))?;
    }
    let mut gi = i64::try_from(history.len()).unwrap_or(i64::MAX);
    let bound_tolerance = coordinate_settings.bound_tolerance.as_f64();
    let mut epoch_writer = match spec.get("epoch_fields").filter(|v| !v.is_null()) {
        Some(selection) => match crate::epoch_capture::normalise_selection(Some(selection))? {
            Some(config) => Some(
                EpochFieldArtifactAdapter::for_selection(&out_dir.join("epoch_field_artifacts"), &config)
                    .map_err(JobError::from)?,
            ),
            None => None,
        },
        None => None,
    };

    let mut current: Option<(StageSearch, StageHooks, usize)> = None;
    let mut carried: Option<(StageResponseProgram, implexity_optim::bounds::BoundMultiplierState)> = None;

    let mut committed_warm: Option<MatchingTimeNewtonGuess> = None;
    if resume {
        let last = history.last().cloned().unwrap_or(Value::Null);
        let committed_index =
            usize::try_from(last.get("stage_index").and_then(Value::as_i64).unwrap_or(0)).unwrap_or(0);
        let committed_program = run.program(committed_index)?;
        let committed_state = ProjectedSearch::state_from_wire(
            last.get("search_state").unwrap_or(&Value::Null),
            &committed_program.expanded,
            &coordinate_settings,
        )?;
        carried = Some((committed_program, committed_state.bound_state.clone()));
        committed_warm = read_resume_warm_start(out_dir, &design_identity(&designs)?)?;

        let same_runtime = continuation_seed
            .as_ref()
            .is_none_or(|c| c["changed_identities"].as_array().is_some_and(Vec::is_empty));
        let mut exact_replay = same_runtime && (continuation_seed.is_none() || committed_warm.is_some());
        let warm_evidence = match &committed_warm {
            Some(guess) => {
                let replay_provider = run.stage_context(committed_index)?.provider;
                match install_resume_warm_start(replay_provider.as_ref(), &problem, &designs, guess) {
                    Ok(evidence) => evidence,
                    Err(e) if continuation_seed.is_some() => {
                        exact_replay = false;
                        committed_warm = None;
                        serde_json::json!({"schema": RESUME_WARM_START_SCHEMA, "installed": false,
                                           "reason": e.message()})
                    }
                    Err(e) => return Err(e),
                }
            }
            None => Value::Null,
        };
        if continuation_seed.is_some() {

            for row in &history {
                emit_line("ITER ", row);
            }
        }
        let (mut replay_search, replay_hooks) =
            run.stage_search(committed_index, &designs, &designs, Some(committed_state), false)?;
        let tolerance = if exact_replay {
            implexity_runtime::replay_validation::REPLAY_RELATIVE_TOLERANCE
        } else {
            CONTINUATION_REPLAY_RELATIVE_TOLERANCE
        };
        let replayed_terms = Value::Array(replay_search.current.terms.clone());
        let checked = implexity_runtime::replay_validation::validate_objective_replay_within(
            last.get("L").unwrap_or(&Value::Null),
            last.get("terms").unwrap_or(&Value::Null),
            &fv(replay_search.current.total),
            &replayed_terms,
            tolerance,
        );
        if let Some(seed) = continuation_seed.as_ref().filter(|c| c["mode"] == "reproduce") {
            return reproduction_result(
                out_dir,
                seed,
                &last,
                replay_search.current.total,
                &replayed_terms,
                tolerance,
                &warm_evidence,
                checked,
            );
        }
        let evidence = checked?;
        emit_line("RESUME_REPLAY ", &evidence);
        replay_search.current.point.diagnostics.insert("optimizer_resume_replay".into(), evidence);
        replay_search.current.point.diagnostics.insert("resume_warm_start".into(), warm_evidence);
        if committed_index == start_stage {
            current = Some((replay_search, replay_hooks, committed_index));
        }
    }
    let stage_count = stages.len();
    for si in start_stage..stage_count {
        let stage = stages[si].clone();
        let context = run.stage_context(si)?;
        let provider = Arc::clone(&context.provider);
        let local0 = if si == start_stage { start_local } else { 0 };
        let preflight =
            match design_operations(provider.as_ref()).filter(|o| o.provides(DesignOp::PreflightDesign)) {
                Some(ops) => ops.preflight_design(&problem, &designs.owned_design()?)?,
                None => provider.preflight(&problem, designs.get(&primary))?,
            };
        if !preflight.get("ok").is_none_or(truthy) {
            return Err(JobError::contract(format!("{}: preflight refused design", stage.id)));
        }
        let coupling = implexity_core::coupling_graph::validate_provider_couplings(
            provider.as_ref(),
            Some(&problem),
            &implexity_core::registries::global().extensions,
            true,
        );
        if !coupling.get("ok").is_some_and(truthy) {
            return Err(JobError::contract(format!(
                "{}: coupling preflight refused optimization: {}",
                stage.id,
                py_str(coupling.get("errors").unwrap_or(&Value::Null))
            )));
        }
        if current.as_ref().is_none_or(|(_, _, s)| *s != si) {
            let stage_entry = designs.clone();
            let program = run.program(si)?;
            let state = run.carried_state(&program, carried.as_ref())?;
            let initializer_requested = !resume
                && !coupling_initializer_attempted
                && requested_preview.is_some()
                && coupling_policy.get("preset").and_then(Value::as_str) != Some("exact");
            if initializer_requested {
                coupling_initializer_attempted = true;
                let requested = requested_preview.clone().unwrap_or_default();
                let policy = serde_json::json!({
                    "mode": requested.mode.as_str(),
                    "ood_policy": requested.ood_policy.as_str(),
                    "coupling_approximation": coupling_policy,
                });
                let nominal = stage.operating_points == [0];
                let scope = if nominal {
                    let owned = designs.owned_design()?;
                    design_operations(provider.as_ref())
                        .and_then(|o| o.staged_initial_guess_scope(&problem, &owned, &policy))
                        .transpose()?
                } else {
                    None
                };
                let mut ci = match (&scope, nominal) {
                    (Some(scope), _) => match &scope.value {
                        None => unavailable_coupling_initialization(
                            &coupling_policy,
                            "provider_initializer_declined",
                            &stage.operating_points,
                        ),
                        Some(trace) => {
                            let mut t = implexity_core::wire::to_wire(trace)?;
                            if let Value::Object(m) = &mut t {
                                m.insert("exact_correction_completed".into(), Value::Bool(false));
                            }
                            t
                        }
                    },
                    (None, true) => unavailable_coupling_initialization(
                        &coupling_policy,
                        "provider_initializer_unavailable",
                        &stage.operating_points,
                    ),
                    (None, false) => unavailable_coupling_initialization(
                        &coupling_policy,
                        if has_staged_initializer(provider.as_ref()) {
                            "unsupported_operating_points"
                        } else {
                            "provider_initializer_unavailable"
                        },
                        &stage.operating_points,
                    ),
                };
                emit_line("PREVIEW ", &ci);
                let (search, hooks) = run.stage_search(si, &stage_entry, &designs, state, true)?;
                drop(scope);
                current = Some((search, hooks, si));
                if let Value::Object(m) = &mut ci {
                    let initially: Value =
                        m.get("lagged_coupling_ids").cloned().unwrap_or(Value::Array(Vec::new()));
                    m.insert("initially_lagged_coupling_ids".into(), initially);
                    m.insert("lagged_coupling_ids".into(), Value::Array(Vec::new()));
                    m.insert("inactive_coupling_ids".into(), Value::Array(Vec::new()));
                    m.insert("exact_correction_completed".into(), Value::Bool(true));
                    m.insert("result_truth_status".into(), Value::String("exact".into()));
                    m.insert("active_coupling_state".into(), Value::String("all_active".into()));
                    if let Some(Value::Object(last)) = m
                        .get_mut("restoration_schedule")
                        .and_then(Value::as_array_mut)
                        .and_then(|s| s.last_mut())
                    {
                        last.insert("completed".into(), Value::Bool(true));
                    }
                }
                coupling_initialization = completed_coupling_initialization(&ci)?;
                if let Some(c) = &coupling_initialization {
                    emit_line("PREVIEW ", c);
                }
            } else {
                let (search, hooks) = run.stage_search(si, &stage_entry, &designs, state, true)?;
                current = Some((search, hooks, si));
            }
        }
        accept_design(provider.as_ref(), &problem, &designs, &stage.operating_points)?;
        let Some((search, hooks, _)) = current.as_mut() else {
            return Err(JobError::contract("hierarchical stage search is unavailable"));
        };
        if first.is_none() {
            first = Some(search.current.total);
        }
        let first_value = first.unwrap_or(f64::NAN);
        for li in local0..stage.iterations {
            let iteration_entry = designs.clone();
            hooks.begin_iteration(li);
            let (projected, _norm) = search.projected_gradient();
            search.last_denominators = BTreeMap::new();
            let warm_before =
                accepted_warm_start(provider.as_ref(), &problem, &designs, &stage.operating_points);
            let engine = search.iterate(hooks, gi)?;
            let accepted = engine.get("accepted") == Some(&Value::Bool(true));
            if accepted {
                committed_warm = warm_before;
            }
            let mut reason: Option<String> = engine.get("reason").and_then(Value::as_str).map(str::to_string);
            designs = search.current.point.design.clone();
            let l = search.current.total;
            let current_design_state_id = design_identity(&designs)?;
            let measures = search.measures()?;
            let max_violation = measures.max_scaled_violation;
            let live = format!("live_{gi:06}.npz");
            let live_path = out_dir.join(&live);
            write_design(&live_path, &designs, &solve_id)?;
            let mut row = Map::new();
            row.insert("i".into(), Value::from(gi));
            row.insert("iteration".into(), Value::from(gi));
            row.insert("stage".into(), Value::String(stage.id.clone()));
            row.insert("stage_index".into(), Value::from(si));
            row.insert("stage_iteration".into(), Value::from(li));
            row.insert("provider".into(), Value::String(stage.provider.clone()));
            row.insert("fidelity".into(), Value::String(stage.fidelity.clone()));
            row.insert("transition".into(), Value::String(stage.transition.clone()));
            row.insert("released_blocks".into(), strings(&stage.released_blocks));
            row.insert("active_coordinates".into(), strings(&context.active));
            row.insert("operating_points".into(), ints(&stage.operating_points));
            row.insert("robust_mode".into(), Value::String(stage.robust_mode.clone()));
            row.insert("L".into(), fv(l));
            row.insert("objective".into(), fv(l));
            row.insert("L_first".into(), fv(first_value));
            row.insert("base_objective".into(), fv(search.current.base_objective));
            row.insert("gradient_norm".into(), fv(num(engine.get("gradient_norm"))));
            row.insert("stationarity".into(), engine.get("stationarity").cloned().unwrap_or(Value::Null));
            row.insert("accepted".into(), Value::Bool(accepted));
            row.insert("step_fraction".into(), fv(num(engine.get("step_fraction"))));
            row.insert("trust_step".into(), fv(num(engine.get("trust_step"))));
            row.insert("terms".into(), Value::Array(search.current.terms.clone()));
            row.insert("diagnostic_responses".into(), search.current.point.diagnostics.get("diagnostic_responses").cloned().unwrap_or(Value::Array(Vec::new())));
            row.insert("max_scaled_bound_violation".into(), fv(max_violation));
            row.insert("bound_measure".into(), fv(measures.max_measure));
            row.insert("bound_feasible".into(), Value::Bool(max_violation <= bound_tolerance));
            row.insert(
                "bound_multipliers".into(),
                Value::Array(measures.bounds.iter().map(bound_row).collect()),
            );
            row.insert(
                "multiplier_updates".into(),
                engine.get("multiplier_updates").cloned().unwrap_or(Value::Array(Vec::new())),
            );
            row.insert(
                "trial_evaluations".into(),
                Value::from(engine.get("trial_evaluations").and_then(Value::as_i64).unwrap_or(0)),
            );
            row.insert(
                "armijo_rejections".into(),
                Value::from(engine.get("armijo_rejections").and_then(Value::as_i64).unwrap_or(0)),
            );
            row.insert("search_state".into(), search.state_wire());
            let bytes = std::fs::metadata(&live_path)?.len();
            let mut push = Map::new();
            push.insert("file".into(), Value::String(live.clone()));
            push.insert("bytes".into(), Value::from(bytes));
            push.insert("sha256".into(), Value::String(sha256_file(&live_path)?));
            row.insert("push".into(), Value::Object(push));
            if let Some(r) = &reason {
                row.insert("reason".into(), Value::String(r.clone()));
            }
            for key in ENGINE_OPTIONAL_FIELDS {
                if let Some(v) = engine.get(key) {
                    row.insert(key.into(), v.clone());
                }
            }
            if let Some(a) = engine.get("candidate_admission") {
                row.insert("candidate_admission".into(), a.clone());
                row.insert(
                    "candidate_admission_attempts".into(),
                    Value::from(
                        engine.get("candidate_admission_attempts").and_then(Value::as_i64).unwrap_or(0),
                    ),
                );
            }
            if let Some(mut record) = hooks.preview_record.take() {
                record.insert("final_result_authority".into(), Value::String("exact".into()));
                record.insert("accepted_after_exact_correction".into(), Value::Bool(accepted));
                emit_line("PREVIEW ", &Value::Object(record));
            }
            row.insert("design_state_id".into(), Value::String(current_design_state_id.clone()));
            row.insert("coordinate_design_state_ids".into(), Value::Object(coordinate_identities(&designs)?));
            for (k, v) in &identities {
                row.insert(k.clone(), v.clone());
            }
            let mut coordinate_updates = Map::new();
            for (name, value) in designs.iter() {
                let entry = iteration_entry.get(name).cloned().unwrap_or_else(|| value.clone());
                let delta = value - &entry;
                let free =
                    coordinate_masks.get(name).map_or(value.len(), |m| m.iter().filter(|v| **v).count());
                let pg = projected.get(name);
                let mut update = Map::new();
                update.insert("max_abs".into(), fv(max_abs(&delta)));
                update.insert("l2".into(), fv(l2(&delta)));
                update.insert("free_entries".into(), Value::from(free));
                update.insert(
                    "projected_gradient_max_abs".into(),
                    fv(pg.filter(|p| !p.is_empty()).map_or(0.0, max_abs)),
                );
                update.insert(
                    "update_denominator".into(),
                    fv(search.last_denominators.get(name).copied().unwrap_or(0.0)),
                );
                update.insert("step_scale".into(), fv(step_scales.get(name).copied().unwrap_or(1.0)));
                coordinate_updates.insert(name.into(), Value::Object(update));
            }
            row.insert("coordinate_updates".into(), Value::Object(coordinate_updates));
            let mut diagnostics = search.current.point.diagnostics.clone();
            if search.program.mode != "nominal" {
                diagnostics
                    .insert("weights".into(), Value::Array(search.weights()?.into_iter().map(fv).collect()));
            }
            if spec.get("epoch_fields").is_some_and(|v| !v.is_null()) {
                let capture = match (accepted, epoch_writer.as_mut()) {
                    (true, Some(adapter)) => {
                        let mut source = Map::new();
                        source.insert("run_fingerprint".into(), Value::String(run_fingerprint.clone()));
                        source.insert("solve_id".into(), Value::String(solve_id.clone()));
                        source.insert("epoch".into(), Value::from(gi));
                        source.insert("checkpoint_sha256".into(), row["push"]["sha256"].clone());
                        let mut writer =
                            |fields: &BTreeMap<String, ArrayD<f64>>,
                             ident: &Map<String, Value>,
                             meta: &Map<String, Value>| {
                                adapter.write(fields, ident, meta).map_err(|e| e.to_string())
                            };
                        let mut capture = capture_cached_epoch(
                            provider.as_ref(),
                            &problem,
                            &designs,
                            &to_usize_points(&stage.operating_points),
                            gi,
                            &source,
                            spec.get("epoch_fields"),
                            &mut writer,
                        )?;
                        retain_epoch_sensitivities(
                            &mut capture,
                            provider.as_ref(),
                            &problem,
                            &search.current.point,
                            &search.current.gradients,
                            &search.program.points,
                            &search.program.names,
                            &source,
                            spec.get("epoch_fields"),
                            &mut writer,
                        );
                        capture
                    }
                    _ => serde_json::json!({
                        "status": "not_recorded",
                        "reason": if accepted { "writer_unavailable" } else { "no_design_update" },
                        "additional_physics_evaluations": 0,
                        "engineering_acceptance": false,
                    }),
                };
                diagnostics.insert("epoch_field_capture".into(), capture);
            }
            row.insert("diagnostics".into(), Value::Object(diagnostics));
            if let Some(policy) = stage.adaptive.as_ref() {
                let mut rows: Vec<Value> = history
                    .iter()
                    .filter(|h| h.get("stage").and_then(Value::as_str) == Some(stage.id.as_str()))
                    .cloned()
                    .collect();
                rows.push(Value::Object(row.clone()));
                let topology: Vec<f64> =
                    designs.get(&primary).map(|a| a.iter().copied().collect()).unwrap_or_default();
                let mut recommendation = assess(
                    &rows,
                    &row["diagnostics"],
                    &topology,
                    &AdaptivePolicy::from_dict(Some(&Value::Object(policy.clone())))?,
                    problem_doc.as_ref(),
                    &implexity_runtime::multiphysics_monitor::assess_multiphysics,
                )?;
                let ready = recommendation.get("ready") == Some(&Value::Bool(true));
                if ready
                    && si + 1 < stage_count
                    && let Value::Object(m) = &mut recommendation
                {
                    m.insert("next_stage".into(), Value::String(stages[si + 1].id.clone()));
                }
                row.insert("adaptive".into(), recommendation.clone());
                if ready && si + 1 < stage_count {
                    let mut fidelity = Map::new();
                    fidelity.insert("stage".into(), Value::String(stage.id.clone()));
                    fidelity.insert("next_stage".into(), Value::String(stages[si + 1].id.clone()));
                    if let Value::Object(m) = &recommendation {
                        for (k, v) in m {
                            fidelity.insert(k.clone(), v.clone());
                        }
                    }
                    emit_line("FIDELITY ", &Value::Object(fidelity));
                    if recommendation.get("authorized") == Some(&Value::Bool(true)) {
                        reason = Some("adaptive_transition".into());
                        row.insert("reason".into(), Value::String("adaptive_transition".into()));
                    }
                }
            }
            let mut candidates = history.clone();
            candidates.push(Value::Object(row.clone()));
            let best_history_index = history_best_index(&candidates)?;
            best = num(candidates[best_history_index].get("L"));
            best_design_state_id =
                candidates[best_history_index]["design_state_id"].as_str().unwrap_or("").to_string();
            row.insert("L_best".into(), fv(best));
            row.insert("best_history_index".into(), Value::from(best_history_index));
            row.insert("best_design_state_id".into(), Value::String(best_design_state_id.clone()));
            if best_history_index == history.len() {
                write_design(&out_dir.join("best.npz"), &designs, &solve_id)?;
            }
            let (next_continue, next_stage, next_local, next_stage_id) = next_cursor(&row, &stages);
            row.insert("continuable".into(), Value::Bool(next_continue));
            let termination_reason = reason.clone().unwrap_or_else(|| "iteration_limit".into());
            history.push(Value::Object(row.clone()));
            let digest = history_digest(&history);
            let mut members: Vec<(String, NpyArray)> =
                designs.iter().map(|(k, v)| (format!("p_{}", slot(k)), NpyArray::from_f64(v))).collect();
            members.push(("coordinate_order".into(), NpyArray::strings(&designs.names())));
            let text = |k: &str, v: &str| (k.to_string(), NpyArray::scalar_str(v));
            let int = |k: &str, v: i64| (k.to_string(), NpyArray::scalar_i64(v));
            members.push(text("checkpoint_schema", CHECKPOINT_SCHEMA));
            members.push(int("stage_index", i64::try_from(next_stage).unwrap_or(i64::MAX)));
            members.push(text("stage_id", &next_stage_id));
            members.push(int("local_next", next_local));
            members.push(int("global_next", i64::try_from(history.len()).unwrap_or(i64::MAX)));
            members.push(text("continuation_state", if next_continue { "continue" } else { "terminal" }));
            members.push(text("termination_reason", &termination_reason));
            members.push(("L_first".into(), NpyArray::scalar_f64(first_value)));
            members.push(("L_best".into(), NpyArray::scalar_f64(best)));
            for (k, v) in &identities {
                members.push(text(k, v.as_str().unwrap_or("")));
            }
            members.push(text("initial_design_state_id", &initial_design_state_id));
            members.push(text("design_state_id", &current_design_state_id));
            members.push(text(
                "initial_coordinate_design_state_ids",
                &canonical_text(&Value::Object(coordinate_identities(&run_initial)?)),
            ));
            members.push(text(
                "coordinate_design_state_ids",
                &canonical_text(&Value::Object(coordinate_identities(&designs)?)),
            ));
            members.push(text("best_design_state_id", &best_design_state_id));
            members.push(int("best_history_index", i64::try_from(best_history_index).unwrap_or(i64::MAX)));
            members.push(int("history_length", i64::try_from(history.len()).unwrap_or(i64::MAX)));
            members.push(text("history_digest", &digest));
            members.push(text(
                "matching_time_guess_consumed",
                &canonical_text(matching_time_consumed.as_ref().unwrap_or(&Value::Null)),
            ));
            members.push(text(
                "response_normalization",
                &canonical_text(run.normalization_record.as_ref().unwrap_or(&Value::Null)),
            ));
            members.push(text(
                "coupling_initialization",
                &canonical_text(coupling_initialization.as_ref().unwrap_or(&Value::Null)),
            ));
            members.push(text("search_state", &canonical_text(&row["search_state"])));
            if accepted || gi == 0 {
                write_resume_warm_start(out_dir, committed_warm.as_ref(), &current_design_state_id)?;
            }

            crate::epoch_state::write_epoch_state(
                out_dir,
                &crate::epoch_state::EpochRecord {
                    epoch: history.len() - 1,
                    checkpoint: &members,
                    warm_start: committed_warm.as_ref(),
                    history_row: history.last().unwrap_or(&Value::Null),
                    history_payload: &history_payload(&history),
                    provenance: &epoch_provenance,
                    seeded_from: None,
                },
            )?;
            atomic_json(&out_dir.join("history.json"), &history_payload(&history))?;
            atomic_npz_members(&ckpt, &members)?;
            emit_line("ITER ", &Value::Object(row.clone()));
            gi += 1;
            let op = control(out_dir);
            if let Some(op) = op.filter(|_| next_continue) {
                let mut halted = Map::new();
                halted.insert("op".into(), Value::String(op.clone()));
                halted.insert("iterations".into(), Value::from(history.len()));
                halted.insert("requested_optimization_updates".into(), Value::from(requested_updates_total));
                halted.insert("exact_corrections".into(), Value::from(history.len()));
                halted.insert("update_metric".into(), Value::Object(update_metric.clone()));
                halted.insert("coordinate_step_scales".into(), step_scales_value(&step_scales, &coords));
                halted
                    .insert("requested_policy_digest".into(), Value::String(requested_policy_digest.clone()));
                halted
                    .insert("effective_effort_digest".into(), Value::String(effective_effort_digest.clone()));
                halted.insert(
                    "operation_context_digest".into(),
                    Value::String(operation_context_digest.clone()),
                );
                halted.insert("run_fingerprint".into(), Value::String(run_fingerprint.clone()));
                let mut result = Map::new();
                result.insert("status".into(), Value::String(op));
                result.insert("history".into(), Value::Array(history.clone()));
                result.insert("update_metric".into(), Value::Object(update_metric.clone()));
                result.insert("coordinate_step_scales".into(), step_scales_value(&step_scales, &coords));
                result.insert("run_fingerprint".into(), Value::String(run_fingerprint.clone()));
                result
                    .insert("requested_policy_digest".into(), Value::String(requested_policy_digest.clone()));
                result
                    .insert("effective_effort_digest".into(), Value::String(effective_effort_digest.clone()));
                result.insert(
                    "operation_context_digest".into(),
                    Value::String(operation_context_digest.clone()),
                );
                result.insert("search_state".into(), row["search_state"].clone());
                if let Some(c) = &matching_time_consumed {
                    halted.insert("matching_time_guess_consumed".into(), c.clone());
                    result.insert("matching_time_guess_consumed".into(), c.clone());
                }
                if let Some(n) = &run.normalization_record {
                    halted.insert("response_normalization".into(), n.clone());
                    result.insert("response_normalization".into(), n.clone());
                }
                if let Some(c) = &coupling_initialization {
                    halted.insert("coupling_initialization".into(), c.clone());
                    result.insert("coupling_initialization".into(), c.clone());
                }
                let halted = attach_exact_effort_evidence(
                    &halted,
                    &Value::Object(effort_binding.clone()),
                    execution_started,
                    u64::try_from(history.len()).unwrap_or(u64::MAX),
                )?;
                result.insert(
                    "halted_computation_evidence".into(),
                    halted.get("computation_evidence").cloned().unwrap_or(Value::Null),
                );
                emit_line("HALTED ", &Value::Object(halted));
                return Ok(result);
            }
            if !next_continue || next_stage != si {
                break;
            }
        }
        carried = Some((search.program.clone(), search.bound_state.clone()));
    }
    let Some((search, _hooks, _)) = current.as_ref() else {
        return Err(JobError::contract("hierarchical job completed without a stage search"));
    };
    let outcome = search.outcome(&history)?;
    let termination_reason = outcome.get("termination_reason").cloned().unwrap_or(Value::Null);
    let mut summary = Map::new();
    summary.insert("status".into(), Value::String("completed".into()));
    summary.insert("provider".into(), Value::String(provider_name.clone()));
    summary.insert("topology_coordinate".into(), Value::String(primary.clone()));
    summary.insert("design_coordinates".into(), strings(&coords));
    summary.insert("update_metric".into(), Value::Object(update_metric.clone()));
    summary.insert("coordinate_step_scales".into(), step_scales_value(&step_scales, &coords));
    summary
        .insert("stages".into(), Value::Array(stages.iter().map(|s| Value::String(s.id.clone())).collect()));
    summary.insert("iterations".into(), Value::from(history.len()));
    summary.insert("requested_optimization_updates".into(), Value::from(requested_updates_total));
    summary.insert("exact_corrections".into(), Value::from(history.len()));
    summary.insert("L_first".into(), fv(first.unwrap_or(f64::NAN)));
    summary.insert("L_best".into(), fv(best));
    summary.insert("best_history_index".into(), Value::from(best_history_index_of(&history)?));
    summary.insert("L_last".into(), fv(num(history.last().and_then(|r| r.get("L")))));
    summary.insert("history".into(), Value::Array(history.clone()));
    summary.insert("history_digest".into(), Value::String(history_digest(&history)));
    let mut response_values = Map::new();
    let mut grouped: Map<String, Value> = Map::new();
    for term in &search.current.terms {
        let name = term.get("response").map(py_str).unwrap_or_default();
        let value = fv(num(term.get("value")));
        response_values.insert(name.clone(), value.clone());
        let point = term.get("operating_point").map_or_else(|| "0".to_string(), py_str);
        if let Value::Object(group) = grouped.entry(point).or_insert_with(|| Value::Object(Map::new())) {
            group.insert(name, value);
        }
    }
    summary.insert("responses".into(), Value::Object(response_values));
    if grouped.len() > 1 {
        summary.insert("responses_by_operating_point".into(), Value::Object(grouped));
        summary.insert(
            "responses_summary_scope".into(),
            Value::String("last_operating_point_compatibility_projection".into()),
        );
    }
    summary.insert("search_outcome".into(), outcome.clone());
    summary.insert("search_state".into(), search.state_wire());
    summary.insert("termination_reason".into(), termination_reason);
    summary.insert(
        "optimization_converged".into(),
        outcome.get("optimization_converged").cloned().unwrap_or(Value::Bool(false)),
    );
    summary.insert("initial_design_state_id".into(), Value::String(design_identity(&run_initial)?));
    summary.insert("final_design_state_id".into(), Value::String(design_identity(&designs)?));
    summary.insert("best_design_state_id".into(), Value::String(best_design_state_id));
    summary.insert("solve_id".into(), Value::String(solve_id.clone()));
    summary.insert("document_content_id".into(), Value::String(document_content_id));
    summary.insert("run_fingerprint".into(), Value::String(run_fingerprint));
    summary.insert("runtime_source_sha256".into(), Value::String(runtime_source_sha256));
    summary.insert("package_identity_sha256".into(), Value::String(package_identity_sha256));
    summary.insert("provider_descriptor_sha256".into(), Value::String(provider_descriptor_sha256));
    summary.insert("provider_profile_sha256".into(), Value::String(provider_profile_sha256));
    summary.insert("requested_policy_digest".into(), Value::String(requested_policy_digest));
    summary.insert("effective_effort_digest".into(), Value::String(effective_effort_digest));
    summary.insert("operation_context_digest".into(), Value::String(operation_context_digest));
    if let Some(n) = &run.normalization_record {
        summary.insert("response_normalization".into(), n.clone());
    }
    if let Some(c) = &matching_time_consumed {
        summary.insert("matching_time_guess_consumed".into(), c.clone());
    }
    if let Some(c) = &coupling_initialization {
        summary.insert("coupling_initialization".into(), c.clone());
    }
    if let Some(produced) =
        export_matching_time_guess(&matching, &provider_name, base.as_ref(), &problem, &designs)?
    {
        summary.insert("matching_time_guess".into(), produced);
    }
    write_design(&out_dir.join("final.npz"), &designs, &solve_id)?;
    let summary = attach_exact_effort_evidence(
        &summary,
        &Value::Object(effort_binding),
        execution_started,
        u64::try_from(history.len()).unwrap_or(u64::MAX),
    )?;
    atomic_json(&out_dir.join("summary.json"), &Value::Object(summary.clone()))?;
    let mut done = summary.clone();
    done.shift_remove("history");
    emit_line("DONE ", &Value::Object(done));
    Ok(summary)
}

fn initial_primary_of(run_initial: &NamedArrays, primary: &str) -> ArrayD<f64> {
    run_initial.get(primary).cloned().unwrap_or_default()
}

fn best_history_index_of(history: &[Value]) -> CaeResult<usize> {
    history_best_index(history)
}

fn step_scales_value(scales: &BTreeMap<String, f64>, coords: &[String]) -> Value {
    let mut m = Map::new();
    for c in coords {
        m.insert(c.clone(), fv(scales.get(c).copied().unwrap_or(1.0)));
    }
    Value::Object(m)
}

fn value_err(m: &str) -> JobError {
    JobError::value(m)
}

fn npz_to_f64(a: &NpyArray) -> Option<ArrayD<f64>> {
    a.to_f64()
}


#[allow(clippy::too_many_lines)]
pub fn validate_provider_terminal_summary_relations(
    summary: &Value,
    history: &Value,
    primary: &str,
    final_design_id: &str,
    identities: &Map<String, Value>,
) -> JobResult<()> {
    let (Some(summary), Some(history)) = (summary.as_object(), history.as_array()) else {
        return Err(value_err("managed provider terminal summary is malformed"));
    };
    let Some(last) = history.last().and_then(Value::as_object) else {
        return Err(value_err("managed provider terminal summary is malformed"));
    };
    let Some(terms) = last.get("terms").and_then(Value::as_array) else {
        return Err(value_err("managed provider terminal responses are malformed"));
    };
    let malformed = || value_err("managed provider terminal responses are malformed");
    let mut responses = Map::new();
    for term in terms {
        let (Some(r), Some(v)) = (term.get("response"), term.get("value")) else { return Err(malformed()) };
        let v = implexity_optim::pyval::py_float(v).map_err(|_| malformed())?;
        responses.insert(py_str(r), float_value(v));
    }
    if responses.iter().any(|(k, v)| k.is_empty() || !v.as_f64().is_some_and(f64::is_finite)) {
        return Err(malformed());
    }
    let mut grouped: Map<String, Value> = Map::new();
    for term in terms {
        let op = term.get("operating_point").cloned().unwrap_or(Value::from(0));
        if !implexity_optim::pyval::is_int(&op) || op.as_i64().is_some_and(|v| v < 0) {
            return Err(value_err("managed provider terminal operating point is malformed"));
        }
        let entry = grouped.entry(py_str(&op)).or_insert_with(|| Value::Object(Map::new()));
        let Some(values) = entry.as_object_mut() else { return Err(malformed()) };
        let name = py_str(&term["response"]);
        if values.contains_key(&name) {
            return Err(value_err("managed provider terminal response/point pair is duplicated"));
        }
        let v = implexity_optim::pyval::py_float(&term["value"]).map_err(|_| malformed())?;
        values.insert(name, float_value(v));
    }
    let mut expected_points: Vec<Value> = last
        .get("operating_points")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![Value::from(0)]);
    if last.get("robust_mode").map_or("nominal".to_string(), py_str) == "nominal" {
        expected_points.truncate(1);
    }
    let names: Vec<String> = grouped.keys().cloned().collect();
    let expected_names: Vec<String> = expected_points.iter().map(py_str).collect();
    let response_keys: BTreeSet<&String> = responses.keys().collect();
    if names != expected_names
        || grouped
            .values()
            .any(|v| v.as_object().is_none_or(|m| m.keys().collect::<BTreeSet<_>>() != response_keys))
    {
        return Err(value_err("managed provider terminal operating-point coverage drifted"));
    }
    if (grouped.len() > 1 || summary.contains_key("responses_by_operating_point"))
        && (summary.get("responses_by_operating_point") != Some(&Value::Object(grouped.clone()))
            || summary.get("responses_summary_scope").and_then(Value::as_str)
                != Some("last_operating_point_compatibility_projection"))
    {
        return Err(value_err("managed provider terminal grouped responses drifted"));
    }
    let checked = implexity_optim::constraint_admission::terminal_search_outcome(summary, last)?;
    let identity_keys = ["package_identity_sha256", "provider_descriptor_sha256", "provider_profile_sha256"];
    if summary.get("topology_coordinate").and_then(Value::as_str) != Some(primary)
        || summary.get("final_design_state_id").and_then(Value::as_str) != Some(final_design_id)
        || last.get("design_state_id").and_then(Value::as_str) != Some(final_design_id)
        || summary.get("optimization_converged") != Some(&Value::Bool(checked))
        || summary.get("responses") != Some(&Value::Object(responses))
        || identity_keys.iter().any(|k| summary.get(*k) != identities.get(*k))
    {
        return Err(value_err("managed provider completed summary identity drifted"));
    }
    Ok(())
}


#[allow(clippy::too_many_lines)]
pub fn validate_managed_generation(partial: &Path, completed: bool) -> JobResult<()> {
    use crate::managed_io::{JSON_LIMIT, NPZ_LIMIT, artifact_fingerprint, read_json, read_npz};
    let spec_v = read_json(&partial.join("spec.json"), JSON_LIMIT)?;
    let spec = spec_v.as_object().cloned().unwrap_or_default();
    let rows = match spec.get("design_coordinates") {
        Some(Value::Array(a)) if !a.is_empty() => a.clone(),
        _ => return Err(value_err("managed array-provider job omitted named design coordinates")),
    };
    let provider_name =
        spec.get("provider").filter(|v| truthy(v)).map(|v| py_str(v).trim().to_string()).unwrap_or_default();
    require_single_provider_schedule(&Value::String(provider_name.clone()), spec.get("schedule"))?;
    let base = implexity_core::registries::global().providers.get(&provider_name)?;
    let (base_capabilities, supported, _, _) = provider_capabilities(base.as_ref())?;
    require_provider_child_execution(&base_capabilities, &provider_name)?;
    let empty = Value::Object(Map::new());
    let problem = base.normalise_problem(spec.get("problem").filter(|v| truthy(v)).unwrap_or(&empty))?;
    let problem_doc = problem_json(base.as_ref(), &problem).ok().filter(Value::is_object);
    let (supported, primary) =
        authoring_coordinates(base.as_ref(), &problem, &base_capabilities, &supported)?;
    let responses: Vec<ResponseSpec> = spec
        .get("responses")
        .and_then(Value::as_array)
        .map(|r| r.iter().map(ResponseSpec::from_dict).collect::<CaeResult<Vec<_>>>())
        .transpose()?
        .unwrap_or_default();
    if responses.is_empty() {
        return Err(value_err("managed provider generation omitted responses"));
    }
    let response_normalization = spec.get("response_normalization").filter(|v| !v.is_null()).cloned();
    normalise_response_normalization(response_normalization.as_ref(), &responses)?;
    if let Some(check) = design_operations(base.as_ref()).and_then(|o| {
        o.validate_response_selection(&problem, &responses.iter().map(|r| r.name.clone()).collect::<Vec<_>>())
    }) {
        check?;
    }
    let settings = crate::provider_job::settings_for_spec(&spec, true, partial)?;
    let coordinate_settings = settings.as_coordinate_settings();
    let mut designs = NamedArrays::new();
    let mut bounds: BTreeMap<String, Bounds> = BTreeMap::new();
    let mut step_scales: BTreeMap<String, f64> = BTreeMap::new();
    let explicit_step_scales =
        rows.iter().any(|r| r.as_object().is_some_and(|m| m.contains_key("step_scale")));
    for row in &rows {
        let Some(row) = row.as_object() else {
            return Err(value_err("managed design-coordinate declaration is malformed"));
        };
        let name = row.get("coordinate").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        let filename = row.get("file").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        if name.is_empty()
            || designs.contains(&name)
            || filename.is_empty()
            || Path::new(&filename).file_name().map(|n| n.to_string_lossy().into_owned())
                != Some(filename.clone())
        {
            return Err(value_err("managed design-coordinate declaration drifted"));
        }
        let (archive, _) = read_npz(&partial.join(&filename), NPZ_LIMIT, None)?;
        let key = match row.get("key").filter(|v| truthy(v)) {
            Some(k) => py_str(k),
            None => {
                if archive.iter().any(|(k, _)| k == "topology") {
                    "topology".into()
                } else {
                    archive.first().map(|(k, _)| k.clone()).unwrap_or_default()
                }
            }
        };
        let Some((_, raw)) = archive.iter().find(|(k, _)| *k == key) else {
            return Err(value_err("managed design-coordinate archive omitted its declared key"));
        };
        let value = npz_to_f64(raw).ok_or_else(|| value_err("managed design-coordinate input is invalid"))?;
        let (lo, hi) = coordinate_bounds(
            &value,
            row.get("lower").unwrap_or(&Value::from(0.0)),
            row.get("upper").unwrap_or(&Value::from(1.0)),
            &name,
        )?;
        let scale = coordinate_step_scale(Some(row.get("step_scale").unwrap_or(&Value::from(1.0))), &name)
            .map_err(|e| JobError::value(e.message().to_string()))?;
        let b =
            Bounds { lower: lo.broadcast(value.shape()), upper: hi.broadcast(value.shape()), raw: (lo, hi) };
        if value.ndim() == 0 || !value.iter().all(|v| v.is_finite()) || !within(&value, &b, 1e-12) {
            return Err(value_err("managed design-coordinate input is invalid"));
        }
        designs.insert(name.clone(), value);
        bounds.insert(name.clone(), b);
        step_scales.insert(name, scale);
    }
    let coords = designs.names();
    if coords.first() != Some(&primary) || coords.iter().any(|c| !supported.contains(c)) {
        return Err(value_err("managed provider coordinate capability drifted"));
    }
    let blocks =
        validate_blocks(spec.get("design_blocks").and_then(Value::as_array).map(Vec::as_slice), &coords)?;
    let op_count = problem_doc
        .as_ref()
        .and_then(|p| p.get("mission"))
        .and_then(Value::as_object)
        .and_then(|m| m.get("operatingPoints").filter(|v| truthy(v)).or_else(|| m.get("operating_points")))
        .and_then(Value::as_array)
        .map(Vec::len);
    let default_schedule;
    let schedule_rows: &[Value] = if let Some(rows) =
        spec.get("schedule").filter(|v| truthy(v)).and_then(Value::as_array)
    {
        rows
    } else {
        let released: Vec<String> =
            blocks.iter().filter(|b| FREE_ROLES.contains(&b.role.as_str())).map(|b| b.id.clone()).collect();
        default_schedule = vec![serde_json::json!({
            "id": "simultaneous", "provider": provider_name,
            "iterations": coordinate_settings.iterations, "released_blocks": released,
        })];
        &default_schedule
    };
    let stages = validate_schedule(Some(schedule_rows), &provider_name, &blocks, &coords, op_count)?;
    require_single_provider_schedule(
        &Value::String(provider_name.clone()),
        Some(&Value::Array(stages.iter().map(ScheduleStage::to_value).collect())),
    )?;
    let raw_masks = spec.get("masks").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut coordinate_masks = coordinate_designable_masks(spec.get("coordinate_masks"), &designs)?;
    let matching = matching_time_config(&spec)?;
    if !matching.is_empty() {
        let providers: BTreeSet<&str> = stages.iter().map(|s| s.provider.as_str()).collect();
        let points: BTreeSet<Vec<i64>> = stages.iter().map(|s| s.operating_points.clone()).collect();
        if providers != [provider_name.as_str()].into_iter().collect() {
            return Err(value_err("managed matching-time lifecycle changed provider owner"));
        }
        if points != [vec![0]].into_iter().collect() {
            return Err(value_err("managed matching-time lifecycle changed operating points"));
        }
        validate_matching_time_lifecycle(&matching, base.as_ref(), &problem)?;
    }
    let topo_shape = designs.get(&primary).map(|a| a.shape().to_vec()).unwrap_or_default();
    let mut topo_free = ArrayD::from_elem(IxDyn(&topo_shape), true);
    let mut parsed: BTreeMap<&str, ArrayD<f64>> = BTreeMap::new();
    for key in ["fixed_solid", "fixed_void", "preserve", "designable"] {
        if raw_masks.contains_key(key) {
            let mask = crate::optimize::spec::json_array(&raw_masks[key])
                .filter(|m| m.shape() == topo_shape.as_slice() && m.iter().all(|v| v.is_finite()))
                .ok_or_else(|| value_err("managed provider topology mask drifted"))?;
            if key == "designable" {
                Zip::from(&mut topo_free).and(&mask).for_each(|f, m| *f &= *m > 0.5);
            } else {
                Zip::from(&mut topo_free).and(&mask).for_each(|f, m| *f &= *m <= 0.5);
            }
            parsed.insert(key, mask);
        }
    }
    let mut masks = BoxMasks::from_value(&raw_masks)?;
    if let Some(lock) = coordinate_masks.get(&primary).cloned() {
        let keep = lock.mapv(|v| !v);
        let pb = &bounds[&primary];
        let value = designs.get(&primary).cloned().unwrap_or_default();
        for (key, target) in [("fixed_solid", &pb.upper), ("fixed_void", &pb.lower)] {
            if let Some(mask) = parsed.get(key) {
                #[allow(clippy::float_cmp)]
                let conflict = keep
                    .iter()
                    .zip(mask.iter())
                    .zip(value.iter().zip(target.iter()))
                    .any(|((k, m), (v, t))| *k && *m > 0.5 && v != t);
                if conflict {
                    return Err(value_err("managed coordinate lock conflicts with fixed topology role"));
                }
            }
        }
        let prior = parsed.get("preserve").cloned().unwrap_or_else(|| ArrayD::zeros(IxDyn(&topo_shape)));
        let mut preserve = ArrayD::zeros(IxDyn(&topo_shape));
        Zip::from(&mut preserve)
            .and(&prior)
            .and(&keep)
            .for_each(|p, a, k| *p = if *a > 0.5 || *k { 1.0 } else { 0.0 });
        masks.preserve = Some(preserve);
        Zip::from(&mut topo_free).and(&lock).for_each(|f, l| *f &= *l);
    }
    coordinate_masks.insert(primary.clone(), topo_free);
    let projector = match bounds.get(&primary) {
        Some(b) if array_bounds(&b.raw.0, &b.raw.1) => {
            Projector::Array { lo: b.raw.0.clone(), hi: b.raw.1.clone() }
        }
        _ => Projector::Scalar {
            lo: coordinate_settings.coordinate_lower.as_f64(),
            hi: coordinate_settings.coordinate_upper.as_f64(),
        },
    };
    let initial_primary = designs.get(&primary).cloned().unwrap_or_default();
    let projected = projector.project(&initial_primary, &masks, &initial_primary, base.as_ref(), &problem)?;
    designs.insert(primary.clone(), projected);
    let run_initial = designs.clone();
    let solve_id = match spec.get("solve_id") {
        None => "provider-job".to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Err(value_err("managed provider solve identity is malformed")),
    };
    let current_physics = physics_snapshot()?;
    let effort_binding = if let Some(b) = spec.get("computation_effort").filter(|v| !v.is_null()) {
        b.clone()
    } else {
        let mut scope = Map::new();
        scope.insert("provider".into(), Value::String(provider_name.clone()));
        scope.insert("solve_id".into(), Value::String(solve_id.clone()));
        let mut context = Map::new();
        context.insert("operation".into(), Value::String("provider_optimization".into()));
        context.insert(
            "scope_digest".into(),
            Value::String(implexity_core::wire::fingerprint_value(&Value::Object(scope))),
        );
        Value::Object(make_server_effort_binding(
            None,
            &provider_name,
            &base_capabilities,
            &current_physics,
            Some(&context),
            None,
        )?)
    };
    let facts = ProviderFacts {
        provider_name: &provider_name,
        capabilities: &base_capabilities,
        physics: &current_physics,
        candidates: None,
    };
    let (_selection, effort_binding) = validate_effort_binding(&effort_binding, Some(&facts))?;
    let digest = |k: &str| effort_binding.get(k).map(py_str).unwrap_or_default();
    let (runtime_source, runtime_source_sha256) =
        runtime_source_binding(&implexity_solve::matching_time_guess::runtime_source_identity())?;
    let package_identity = stable_package_identity(&current_physics);
    let package_identity_sha256 =
        implexity_core::wire::fingerprint_value(&implexity_core::wire::to_wire(&package_identity)?);
    let descriptor_value = Value::Object(base_capabilities.to_map());
    let provider_descriptor_sha256 = implexity_core::wire::fingerprint_value(&descriptor_value);
    let provider_profile = effort_binding.get("provider_profile").cloned().unwrap_or(Value::Null);
    let provider_profile_sha256 =
        implexity_core::wire::fingerprint_value(&implexity_core::wire::to_wire(&provider_profile)?);
    let document_content_id = match spec.get("document_content_id").or_else(|| spec.get("content_id")) {
        None => "undeclared-document".to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Err(value_err("managed provider document identity is malformed")),
    };
    let initial_design_id = design_identity(&run_initial)?;
    let mut fi = Map::new();
    fi.insert("schema".into(), Value::String(RUN_IDENTITY_SCHEMA.into()));
    fi.insert("spec".into(), implexity_core::wire::to_wire(&matching_time_fingerprint_spec(&spec)?)?);
    fi.insert("settings".into(), crate::provider_job::settings_identity(&spec, &settings));
    fi.insert("feasible_initial_design".into(), Value::String(initial_design_id.clone()));
    fi.insert(
        "initial_coordinate_design_state_ids".into(),
        Value::Object(coordinate_identities(&run_initial)?),
    );
    fi.insert(
        "normalized_schedule".into(),
        Value::Array(stages.iter().map(ScheduleStage::to_value).collect()),
    );
    fi.insert("provider_descriptor".into(), descriptor_value);
    fi.insert("package_identity".into(), package_identity);
    fi.insert("runtime_source_identity".into(), runtime_source);
    fi.insert("provider_profile".into(), provider_profile);
    fi.insert("solve_id".into(), Value::String(solve_id.clone()));
    fi.insert("document_content_id".into(), Value::String(document_content_id.clone()));
    for k in ["requested_policy_digest", "effective_effort_digest", "operation_context_digest"] {
        fi.insert(k.into(), Value::String(digest(k)));
    }
    let run_fingerprint = implexity_core::wire::fingerprint_value(&Value::Object(fi));
    let identity_values = [
        run_fingerprint.clone(),
        runtime_source_sha256.clone(),
        package_identity_sha256,
        provider_descriptor_sha256,
        provider_profile_sha256,
        digest("requested_policy_digest"),
        digest("effective_effort_digest"),
        digest("operation_context_digest"),
        solve_id.clone(),
        document_content_id.clone(),
    ];
    let mut identities = Map::new();
    for (k, v) in IDENTITY_KEYS.iter().zip(identity_values) {
        identities.insert((*k).into(), Value::String(v));
    }
    let mut required: BTreeSet<&str> =
        ["spec.json", "initial.npz", "initial_design.json", "best.npz", "ckpt.npz", "history.json"]
            .into_iter()
            .collect();
    if completed {
        required.insert("summary.json");
        required.insert("final.npz");
    }
    for name in &required {
        if name.ends_with(".npz") {
            read_npz(&partial.join(name), NPZ_LIMIT, None)?;
        } else {
            artifact_fingerprint(&partial.join(name), JSON_LIMIT)?;
        }
    }
    let sidecar = partial.join("matching_time_guess_consumed.json");
    if sidecar.exists() || sidecar.is_symlink() {
        artifact_fingerprint(&sidecar, JSON_LIMIT)?;
    }
    let warm_sidecar = partial.join(RESUME_WARM_START_SIDECAR);
    if warm_sidecar.exists() || warm_sidecar.is_symlink() {
        artifact_fingerprint(&warm_sidecar, JSON_LIMIT)?;
    }
    for slot in RESUME_WARM_START_SLOTS {
        let archive = partial.join(slot);
        if archive.exists() || archive.is_symlink() {
            read_npz(&archive, NPZ_LIMIT, None)?;
        }
    }
    for entry in std::fs::read_dir(partial)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("live_") {
            let ok = name.len() == "live_000000.npz".len()
                && name.ends_with(".npz")
                && name["live_".len().."live_".len() + 6].bytes().all(|b| b.is_ascii_digit());
            if !ok {
                return Err(value_err("managed provider live generation contains an unapproved name"));
            }
            read_npz(&entry.path(), NPZ_LIMIT, None)?;
        }
    }
    let resumed = load_resume_generation(
        &partial.join("ckpt.npz"),
        &partial.join("history.json"),
        partial,
        &ResumeInputs {
            coords: &coords,
            run_initial: &run_initial,
            bounds: &bounds,
            coordinate_masks: &coordinate_masks,
            blocks: &blocks,
            stages: &stages,
            responses: &responses,
            response_normalization: response_normalization.as_ref(),
            problem_doc: problem_doc.as_ref(),
            primary: &primary,
            solve_id: &solve_id,
            identities: &identities,
            settings: &coordinate_settings,
            step_scales: &step_scales,
        },
        false,
    );
    let resumed = match resumed {
        Ok(r) => Some(r),
        Err(e)
            if completed
                && e.python_class() == "CAEContractError"
                && e.message() == "terminal hierarchical checkpoint cannot be resumed" =>
        {
            None
        }
        Err(e) => return Err(e.into()),
    };
    if !completed {
        return Ok(());
    }
    if resumed.is_some() {
        return Err(value_err("completed managed generation retained a continuable checkpoint"));
    }
    let summary = read_json(&partial.join("summary.json"), JSON_LIMIT)?;
    let history_payload = read_json(&partial.join("history.json"), JSON_LIMIT)?;
    let history = history_payload.get("history").cloned().unwrap_or(Value::Null);
    let hist = history.as_array().cloned().unwrap_or_default();
    let shapes: BTreeMap<String, Vec<usize>> =
        run_initial.iter().map(|(k, v)| (k.to_string(), v.shape().to_vec())).collect();
    let final_design =
        load_design_snapshot(&partial.join("final.npz"), &coords, &shapes, &bounds, &solve_id)?;
    let last = hist.last().cloned().unwrap_or(Value::Null);
    let malformed = || value_err("managed provider terminal responses are malformed");
    let mut expected_responses = Map::new();
    for term in last.get("terms").and_then(Value::as_array).ok_or_else(malformed)? {
        let (Some(r), Some(v)) = (term.get("response"), term.get("value")) else { return Err(malformed()) };
        expected_responses
            .insert(py_str(r), float_value(implexity_optim::pyval::py_float(v).map_err(|_| malformed())?));
    }
    let final_id = design_identity(&final_design)?;
    validate_provider_terminal_summary_relations(&summary, &history, &primary, &final_id, &identities)?;
    let s = summary.as_object().cloned().unwrap_or_default();
    let last_o = last.as_object().cloned().unwrap_or_default();
    let best_idx =
        last_o.get("best_history_index").and_then(|v| implexity_optim::pyval::py_int(v).ok()).unwrap_or(-1);
    let best_row = usize::try_from(best_idx).ok().and_then(|i| hist.get(i));
    let f = |v: Option<&Value>| v.and_then(Value::as_f64).unwrap_or(f64::NAN);
    #[allow(clippy::float_cmp)]
    let drift = s.get("provider").and_then(Value::as_str) != Some(provider_name.as_str())
        || s.get("design_coordinates") != Some(&serde_json::json!(coords))
        || ((explicit_step_scales || s.contains_key("coordinate_step_scales"))
            && s.get("coordinate_step_scales")
                != Some(&Value::Object(
                    step_scales.iter().map(|(k, v)| (k.clone(), float_value(*v))).collect(),
                )))
        || s.get("stages")
            != Some(&serde_json::json!(stages.iter().map(|st| st.id.clone()).collect::<Vec<_>>()))
        || s.get("iterations") != Some(&Value::from(hist.len()))
        || s.get("history_digest").and_then(Value::as_str) != Some(history_digest(&hist).as_str())
        || s.get("initial_design_state_id").and_then(Value::as_str) != Some(initial_design_id.as_str())
        || s.get("final_design_state_id").and_then(Value::as_str) != Some(final_id.as_str())
        || last_o.get("design_state_id").and_then(Value::as_str) != Some(final_id.as_str())
        || best_row.is_none()
        || s.get("best_design_state_id") != best_row.and_then(|r| r.get("design_state_id"))
        || s.get("run_fingerprint").and_then(Value::as_str) != Some(run_fingerprint.as_str())
        || s.get("solve_id").and_then(Value::as_str) != Some(solve_id.as_str())
        || s.get("document_content_id").and_then(Value::as_str) != Some(document_content_id.as_str())
        || s.get("runtime_source_sha256").and_then(Value::as_str) != Some(runtime_source_sha256.as_str())
        || ["package_identity_sha256", "provider_descriptor_sha256", "provider_profile_sha256"]
            .iter()
            .any(|k| s.get(*k) != identities.get(*k))
        || ["requested_policy_digest", "effective_effort_digest", "operation_context_digest"]
            .iter()
            .any(|k| s.get(*k).and_then(Value::as_str) != Some(digest(k).as_str()))
        || s.get("optimization_converged")
            != Some(&Value::Bool(implexity_optim::constraint_admission::terminal_search_outcome(
                &s, &last_o,
            )?))
        || s.get("responses") != Some(&Value::Object(expected_responses))
        || f(s.get("L_first")) != f(hist.first().and_then(|r| r.get("L_first")))
        || s.get("best_history_index") != last_o.get("best_history_index")
        || f(s.get("L_best")) != f(best_row.and_then(|r| r.get("L")))
        || f(s.get("L_last")) != f(last_o.get("L"))
        || last_o.get("continuable") != Some(&Value::Bool(false));
    if drift {
        return Err(value_err("managed provider completed summary identity drifted"));
    }
    Ok(())
}

