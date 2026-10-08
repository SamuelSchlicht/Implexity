// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::{CaeError, CaeResult};
use implexity_optim::NamedArrays;
use implexity_optim::design_identity;
use implexity_optim::search::TrialInfo;
use implexity_runtime::computation_effort::{
    ComputationEffortPolicy, ComputationMode, EffectiveComputationEffort,
};
use implexity_solve::approximation::{
    ApproximateEvaluation, ApproximationError, ApproximationLane, ApproximationPolicy, EffortMode,
    Evaluation, ExactEvaluateFn, ExactEvaluation, ExactTruth, FailurePolicy, TaylorJvpProvider,
};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value};

#[must_use]
pub fn preview_policy(selection: &EffectiveComputationEffort) -> Option<ComputationEffortPolicy> {
    matches!(selection.requested.mode, ComputationMode::VerifiedPreview | ComputationMode::InteractivePreview)
        .then(|| selection.requested.clone())
}

#[must_use]
pub fn interactive_correction_plan(
    requested: Option<&ComputationEffortPolicy>,
    total_updates: i64,
) -> (i64, i64) {
    match requested {
        Some(r) if r.mode == ComputationMode::InteractivePreview => {
            let cadence = r.exact_correction_cadence.max(1);
            (cadence, (total_updates + cadence - 1) / cadence)
        }
        _ => (1, total_updates),
    }
}

pub type Ranked = (f64, ArrayD<f64>, Option<ApproximateEvaluation>);

fn usize_cadence(value: i64) -> usize {
    usize::try_from(value.max(1)).unwrap_or(1)
}


#[allow(clippy::too_many_arguments)]
pub fn preview_ranked_candidates(
    x: &ArrayD<f64>,
    anchor_gradient: &ArrayD<f64>,
    anchor_total: f64,
    candidates: Vec<(f64, ArrayD<f64>)>,
    requested: &ComputationEffortPolicy,
    exact_evaluate: ExactEvaluateFn,
    design_span: &ArrayD<f64>,
) -> CaeResult<(Vec<Ranked>, usize, ApproximationLane)> {
    let span = design_span.mapv(|v| v.max(f64::EPSILON));
    #[allow(clippy::cast_precision_loss)]
    let root = (x.len() as f64).sqrt();
    let design_scale = span.mapv(|v| v * root);
    let gradient = anchor_gradient.clone();
    let provider = TaylorJvpProvider::new(
        Box::new(move |_anchor: &ArrayD<f64>, direction: &ArrayD<f64>| {
            let dot = implexity_optim::numeric::array_sum(&(&gradient * direction));
            Ok(ArrayD::from_elem(IxDyn(&[1]), dot))
        }),
        None,
    );
    let mut lane = ApproximationLane::new(exact_evaluate, Some(Box::new(provider)));
    lane.use_exact_anchor(
        &ExactEvaluation {
            design: x.clone(),
            response: ArrayD::from_elem(IxDyn(&[1]), anchor_total),
            truth: ExactTruth {
                elapsed_seconds: 0.0,
                corrected_from_anchor_id: None,
                observed_normalized_error: None,
                fallback_reason: None,
            },
        },
        Some(&design_scale),
    )
    .map_err(CaeError::from)?;
    let verified = requested.mode == ComputationMode::VerifiedPreview;
    let max_preview_seconds = requested.target_update_rate_hz.map(|hz| 1.0 / hz);
    let cadence = usize_cadence(requested.exact_correction_cadence);
    let policy = ApproximationPolicy {
        mode: if verified { EffortMode::VerifiedPreview } else { EffortMode::InteractivePreview },
        trust_radius: requested.trust_radius,
        max_normalized_error: verified.then_some(requested.max_response_error),
        max_preview_seconds,
        exact_correction_cadence: cadence,
        on_failure: FailurePolicy::Refuse,
    };
    if !verified && !candidates.is_empty() {
        let path_policy = ApproximationPolicy {
            mode: EffortMode::InteractivePreview,
            trust_radius: requested.trust_radius,
            max_normalized_error: None,
            max_preview_seconds,
            exact_correction_cadence: cadence,
            on_failure: FailurePolicy::Refuse,
        };
        let designs: Vec<ArrayD<f64>> = candidates.iter().map(|(_, t)| t.clone()).collect();
        let path = match lane.preview_path(&designs, &path_policy) {
            Ok(p) => p,
            Err(ApproximationError::PreviewUnavailable(_)) => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        if !path.is_empty() {
            let used = path.len();
            let ranked = path
                .into_iter()
                .enumerate()
                .map(|(i, preview)| (candidates[i].0, candidates[i].1.clone(), Some(preview)))
                .collect();
            return Ok((ranked, used, lane));
        }
        return Ok((Vec::new(), 0, lane));
    }
    let mut ranked: Vec<Ranked> = Vec::new();
    let mut fallback: Vec<Ranked> = Vec::new();
    for (step, trial) in candidates {
        match lane.evaluate(&trial, &policy) {
            Ok(Evaluation::Approximate(preview)) => ranked.push((step, trial, Some(preview))),
            Ok(Evaluation::Exact(_)) | Err(ApproximationError::PreviewUnavailable(_)) => {
                fallback.push((step, trial, None));
            }
            Err(e) => return Err(e.into()),
        }
    }
    ranked.sort_by(|a, b| {
        let ka = a.2.as_ref().map_or(f64::NAN, |p| p.response[0]);
        let kb = b.2.as_ref().map_or(f64::NAN, |p| p.response[0]);
        ka.partial_cmp(&kb)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((-a.0).partial_cmp(&-b.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let count = ranked.len();
    ranked.extend(fallback);
    Ok((ranked, count, lane))
}

#[must_use]
pub fn preview_wire(
    requested: &ComputationEffortPolicy,
    preview: &ApproximateEvaluation,
    exact: &ExactEvaluation,
    step: f64,
    candidate_rank: usize,
    candidate_count: usize,
    candidate_design_state_id: &str,
) -> Value {
    let observed = exact.truth.observed_normalized_error;
    let limit = requested.max_response_error;
    let within = observed.map(|o| o <= limit);
    let mut correction = match exact.truth.to_wire() {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    correction.insert("objective".into(), Value::from(exact.response[0]));
    correction.insert("requested_response_error_limit".into(), Value::from(limit));
    correction.insert("within_requested_response_error".into(), within.map_or(Value::Null, Value::Bool));
    let mut m = Map::new();
    m.insert("schema".into(), Value::String("implexity-optimization-preview/1".into()));
    m.insert("requested_mode".into(), Value::String(requested.mode.as_str().into()));
    m.insert("authoritative".into(), Value::Bool(false));
    m.insert("anchor_id".into(), Value::String(preview.truth.anchor_id.clone()));
    m.insert("candidate_design_state_id".into(), Value::String(candidate_design_state_id.into()));
    m.insert("candidate_rank".into(), Value::from(candidate_rank));
    m.insert("candidate_count".into(), Value::from(candidate_count));
    m.insert("step_fraction".into(), Value::from(step));
    m.insert("predicted_objective".into(), Value::from(preview.response[0]));
    m.insert("truth".into(), preview.truth.to_wire());
    m.insert("exact_correction".into(), Value::Object(correction));
    m.insert("commit_rule".into(), Value::String("exact_correction_only".into()));
    Value::Object(m)
}


#[allow(clippy::too_many_arguments)]
pub fn preview_record(
    requested: &ComputationEffortPolicy,
    anchor_design_state_id: &str,
    ranked: &[Ranked],
    ranked_count: usize,
    unflatten: &dyn Fn(&ArrayD<f64>) -> CaeResult<NamedArrays>,
    update_start: i64,
    updates_planned: i64,
    correction_index: i64,
    correction_budget: i64,
) -> CaeResult<Map<String, Value>> {
    let last = ranked.last().and_then(|r| r.2.as_ref()).map_or(0, |p| p.truth.updates_since_exact);
    let mut proposals = Vec::new();
    for (rank, (step, trial, preview)) in ranked.iter().enumerate() {
        let Some(preview) = preview else { continue };
        let mut p = Map::new();
        p.insert("rank".into(), Value::from(rank + 1));
        p.insert("update_index".into(), Value::from(preview.truth.updates_since_exact));
        p.insert("candidate_design_state_id".into(), Value::String(design_identity(&unflatten(trial)?)?));
        p.insert("step_fraction".into(), Value::from(*step));
        p.insert("predicted_objective".into(), Value::from(preview.response[0]));
        p.insert("truth".into(), preview.truth.to_wire());
        proposals.push(Value::Object(p));
    }
    let mut m = Map::new();
    m.insert("schema".into(), Value::String("implexity-optimization-preview-trace/1".into()));
    m.insert("requested_mode".into(), Value::String(requested.mode.as_str().into()));
    m.insert("authoritative".into(), Value::Bool(false));
    m.insert("exact_anchor_design_state_id".into(), Value::String(anchor_design_state_id.into()));
    m.insert("ranked_candidate_count".into(), Value::from(ranked_count));
    m.insert("configured_cadence_updates".into(), Value::from(requested.exact_correction_cadence));
    m.insert("optimization_update_start".into(), Value::from(update_start));
    m.insert("optimization_updates_planned".into(), Value::from(updates_planned));
    m.insert("exact_correction_index".into(), Value::from(correction_index));
    m.insert("exact_correction_budget".into(), Value::from(correction_budget));
    m.insert("updates_before_exact_correction".into(), Value::from(last));
    m.insert("ranked_proposals".into(), Value::Array(proposals));
    m.insert("exact_corrections".into(), Value::Array(Vec::new()));
    m.insert("commit_rule".into(), Value::String("exact_correction_only".into()));
    Ok(m)
}

fn bounded(value: &str, limit: usize) -> String {
    let cleaned: String =
        value.chars().map(|c| if implexity_core::py_repr::is_printable(c) { c } else { ' ' }).collect();
    let mut end = cleaned.len().min(limit);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    let text = &cleaned[..end];
    if text.is_empty() { "unspecified".into() } else { text.to_string() }
}


#[allow(clippy::too_many_arguments)]
pub fn trial_progress(
    iteration: i64,
    attempt: i64,
    decision: &str,
    current: &NamedArrays,
    candidate: Option<&NamedArrays>,
    step: f64,
    info: &TrialInfo,
) -> CaeResult<()> {
    let mut report = Map::new();
    report.insert("schema".into(), Value::String("implexity-optimization-trial-decision/1".into()));
    report.insert("iteration".into(), Value::from(iteration));
    report.insert("attempt".into(), Value::from(attempt));
    report.insert("decision".into(), Value::String(decision.into()));
    report.insert("current_design_state_id".into(), Value::String(design_identity(current)?));
    report.insert("step_fraction".into(), Value::from(step));
    if let Some(c) = candidate {
        report.insert("candidate_design_state_id".into(), Value::String(design_identity(c)?));
    }
    let mut unavailable = Vec::new();
    for (name, value) in [
        ("objective", info.objective),
        ("armijo_bound", info.armijo_bound),
        ("directional", info.directional),
    ] {
        if let Some(v) = value {
            if v.is_finite() {
                report.insert(name.into(), Value::from(v));
            } else {
                unavailable.push(Value::String(name.into()));
            }
        }
    }
    if !unavailable.is_empty() {
        report.insert("unavailable_fields".into(), Value::Array(unavailable));
    }
    if let Some(reason) = &info.reason {
        report.insert("reason".into(), Value::String(bounded(reason, 1536)));
    }
    if let Some(error) = &info.error {
        report.insert("error_type".into(), Value::String(bounded(error.python_class(), 128)));
        report.insert("error".into(), Value::String(bounded(error.message(), 1536)));
    }
    implexity_solve::trace::point("optimization_trial_decision", || {
        let mut fields = Map::new();
        fields.insert("trial_decision".into(), Value::Object(report));
        fields
    })
}
