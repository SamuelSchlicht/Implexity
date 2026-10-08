// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::time::Instant;

use implexity_core::error::CaeError;
use ndarray::ArrayD;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApproximationError {
    #[error("{0}")]
    General(String),
    #[error("{0}")]
    PreviewUnavailable(String),
    #[error("{0}")]
    ApproximateCommit(String),
    #[error("{0}")]
    ProviderContract(String),
    #[error("{0}")]
    Exact(CaeError),
}

impl From<ApproximationError> for CaeError {
    fn from(e: ApproximationError) -> Self {
        match e {
            ApproximationError::Exact(inner) => inner,
            other => CaeError::contract(other.to_string()),
        }
    }
}

pub type ApproxResult<T> = Result<T, ApproximationError>;

fn general(message: &str) -> ApproximationError {
    ApproximationError::General(message.into())
}

fn contract(message: impl Into<String>) -> ApproximationError {
    ApproximationError::ProviderContract(message.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffortMode {
    Exact,
    VerifiedPreview,
    InteractivePreview,
}

impl EffortMode {
    #[must_use]
    pub fn value(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::VerifiedPreview => "verified_preview",
            Self::InteractivePreview => "interactive_preview",
        }
    }

    fn parse(s: &str) -> ApproxResult<Self> {
        match s {
            "exact" => Ok(Self::Exact),
            "verified_preview" => Ok(Self::VerifiedPreview),
            "interactive_preview" => Ok(Self::InteractivePreview),
            _ => Err(general("invalid approximation policy mode")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailurePolicy {
    Exact,
    Refuse,
}

impl FailurePolicy {
    #[must_use]
    pub fn value(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Refuse => "refuse",
        }
    }
}

fn positive(name: &str, v: &Value, optional: bool) -> ApproxResult<Option<f64>> {
    if v.is_null() && optional {
        return Ok(None);
    }
    v.as_f64()
        .filter(|x| v.is_number() && x.is_finite() && *x > 0.0)
        .map(Some)
        .ok_or_else(|| general(&format!("{name} must be a positive finite number")))
}

fn nonnegative(name: &str, v: &Value) -> ApproxResult<Option<f64>> {
    if v.is_null() {
        return Ok(None);
    }
    v.as_f64()
        .filter(|x| v.is_number() && x.is_finite() && *x >= 0.0)
        .map(Some)
        .ok_or_else(|| general(&format!("{name} must be a non-negative finite number")))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApproximationPolicy {
    pub mode: EffortMode,
    pub trust_radius: f64,
    pub max_normalized_error: Option<f64>,
    pub max_preview_seconds: Option<f64>,
    pub exact_correction_cadence: usize,
    pub on_failure: FailurePolicy,
}

impl Default for ApproximationPolicy {
    fn default() -> Self {
        Self {
            mode: EffortMode::Exact,
            trust_radius: 0.25,
            max_normalized_error: None,
            max_preview_seconds: Some(0.1),
            exact_correction_cadence: 1,
            on_failure: FailurePolicy::Exact,
        }
    }
}

impl ApproximationPolicy {


    pub fn from_request(value: Option<&Value>) -> ApproxResult<Self> {
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return Ok(Self::default());
        };
        if let Some(mode) = value.as_str() {
            return Self::build(&json!({"mode": mode}));
        }
        let map =
            value.as_object().ok_or_else(|| general("approximation policy must be a mapping or mode"))?;
        let allowed = [
            "mode",
            "trust_radius",
            "max_normalized_error",
            "max_preview_seconds",
            "exact_correction_cadence",
            "on_failure",
        ];
        let mut unknown: Vec<&String> = map.keys().filter(|k| !allowed.contains(&k.as_str())).collect();
        if !unknown.is_empty() {
            unknown.sort();
            let names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
            return Err(general(&format!("unknown approximation policy fields: {}", names.join(", "))));
        }
        Self::build(value)
    }

    fn build(value: &Value) -> ApproxResult<Self> {
        let d = Self::default();
        let get = |k: &str| value.get(k);
        let mode = match get("mode") {
            None => d.mode,
            Some(v) => {
                EffortMode::parse(v.as_str().ok_or_else(|| general("invalid approximation policy mode"))?)?
            }
        };
        let on_failure = match get("on_failure").map(|v| v.as_str()) {
            None => d.on_failure,
            Some(Some("exact")) => FailurePolicy::Exact,
            Some(Some("refuse")) => FailurePolicy::Refuse,
            Some(_) => return Err(general("invalid approximation policy mode")),
        };
        let trust_radius = match get("trust_radius") {
            None => d.trust_radius,
            Some(v) => positive("trust_radius", v, false)?.unwrap_or(d.trust_radius),
        };
        let max_normalized_error = match get("max_normalized_error") {
            None => None,
            Some(v) => nonnegative("max_normalized_error", v)?,
        };
        let max_preview_seconds = match get("max_preview_seconds") {
            None => d.max_preview_seconds,
            Some(v) => positive("max_preview_seconds", v, true)?,
        };
        let exact_correction_cadence = match get("exact_correction_cadence") {
            None => 1,
            Some(v) => v
                .as_u64()
                .filter(|n| v.is_u64() && *n >= 1)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| general("exact_correction_cadence must be a positive integer"))?,
        };
        let policy = Self {
            mode,
            trust_radius,
            max_normalized_error,
            max_preview_seconds,
            exact_correction_cadence,
            on_failure,
        };
        policy.validate()?;
        Ok(policy)
    }



    pub fn validate(&self) -> ApproxResult<()> {
        if !self.trust_radius.is_finite() || self.trust_radius <= 0.0 {
            return Err(general("trust_radius must be a positive finite number"));
        }
        if self.max_normalized_error.is_some_and(|e| !e.is_finite() || e < 0.0) {
            return Err(general("max_normalized_error must be a non-negative finite number"));
        }
        if self.max_preview_seconds.is_some_and(|s| !s.is_finite() || s <= 0.0) {
            return Err(general("max_preview_seconds must be a positive finite number"));
        }
        if self.exact_correction_cadence < 1 {
            return Err(general("exact_correction_cadence must be a positive integer"));
        }
        if self.mode == EffortMode::VerifiedPreview && self.max_normalized_error.is_none() {
            return Err(general("verified_preview requires max_normalized_error"));
        }
        Ok(())
    }

    #[must_use]
    pub fn to_request(&self) -> Value {
        json!({"mode": self.mode.value(), "trust_radius": self.trust_radius,
            "max_normalized_error": self.max_normalized_error, "max_preview_seconds": self.max_preview_seconds,
            "exact_correction_cadence": self.exact_correction_cadence, "on_failure": self.on_failure.value()})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactAnchor {
    pub anchor_id: String,
    pub design: ArrayD<f64>,
    pub response: ArrayD<f64>,
    pub design_scale: ArrayD<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderApproximation {
    pub response: ArrayD<f64>,
    pub normalized_distance: f64,
    pub normalized_error_bound: Option<f64>,
    pub method: String,
}

pub trait ApproximationProvider {


    fn approximate(
        &self,
        anchor: &ExactAnchor,
        target: &ArrayD<f64>,
        policy: &ApproximationPolicy,
    ) -> ApproxResult<ProviderApproximation>;
}

fn finite(name: &str, a: &ArrayD<f64>, shape: Option<&[usize]>) -> ApproxResult<ArrayD<f64>> {
    if a.is_empty() || !a.iter().all(|v| v.is_finite()) {
        return Err(contract(format!("{name} must be a non-empty finite numerical array")));
    }
    if let Some(s) = shape
        && a.shape() != s
    {
        return Err(contract(format!("{name} shape {} does not match {}", tuple(a.shape()), tuple(s))));
    }
    Ok(a.clone())
}

fn tuple(s: &[usize]) -> String {
    let parts: Vec<String> = s.iter().map(ToString::to_string).collect();
    if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
}

fn norm(a: impl Iterator<Item = f64>) -> f64 {
    a.map(|v| v * v).sum::<f64>().sqrt()
}

pub type JvpFn = Box<dyn Fn(&ArrayD<f64>, &ArrayD<f64>) -> ApproxResult<ArrayD<f64>>>;
pub type ErrorEstimatorFn = Box<dyn Fn(&ArrayD<f64>, &ArrayD<f64>) -> ApproxResult<f64>>;

pub struct TaylorJvpProvider {
    jvp: JvpFn,
    error_estimator: Option<ErrorEstimatorFn>,
}

impl TaylorJvpProvider {
    #[must_use]
    pub fn new(jvp: JvpFn, error_estimator: Option<ErrorEstimatorFn>) -> Self {
        Self { jvp, error_estimator }
    }
}

impl ApproximationProvider for TaylorJvpProvider {
    fn approximate(
        &self,
        anchor: &ExactAnchor,
        target: &ArrayD<f64>,
        _policy: &ApproximationPolicy,
    ) -> ApproxResult<ProviderApproximation> {
        let target = finite("target_design", target, Some(anchor.design.shape()))?;
        let direction = &target - &anchor.design;
        let distance = norm(direction.iter().zip(anchor.design_scale.iter()).map(|(d, s)| d / s));
        let increment =
            finite("jvp response", &(self.jvp)(&anchor.design, &direction)?, Some(anchor.response.shape()))?;
        let response =
            finite("Taylor response", &(&anchor.response + &increment), Some(anchor.response.shape()))?;
        let error = match &self.error_estimator {
            None => None,
            Some(f) => {
                let e = f(&anchor.design, &direction)?;
                if !e.is_finite() || e < 0.0 {
                    return Err(contract("Taylor error estimator must return a non-negative finite number"));
                }
                Some(e)
            }
        };
        Ok(ProviderApproximation {
            response,
            normalized_distance: distance,
            normalized_error_bound: error,
            method: "exact_anchor_taylor_jvp".into(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApproximateTruth {
    pub anchor_id: String,
    pub method: String,
    pub normalized_distance: f64,
    pub normalized_error_bound: Option<f64>,
    pub elapsed_seconds: f64,
    pub updates_since_exact: usize,
    pub exact_correction_due: bool,
}

impl ApproximateTruth {
    #[must_use]
    pub fn to_wire(&self) -> Value {
        json!({"status": "approximate", "anchor_id": self.anchor_id, "method": self.method,
            "normalized_distance": self.normalized_distance, "normalized_error_bound": self.normalized_error_bound,
            "elapsed_seconds": self.elapsed_seconds, "updates_since_exact": self.updates_since_exact,
            "exact_correction_due": self.exact_correction_due, "correction_required": true})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactTruth {
    pub elapsed_seconds: f64,
    pub corrected_from_anchor_id: Option<String>,
    pub observed_normalized_error: Option<f64>,
    pub fallback_reason: Option<String>,
}

impl ExactTruth {
    #[must_use]
    pub fn to_wire(&self) -> Value {
        json!({"status": "exact", "elapsed_seconds": self.elapsed_seconds,
            "corrected_from_anchor_id": self.corrected_from_anchor_id,
            "observed_normalized_error": self.observed_normalized_error,
            "fallback_reason": self.fallback_reason, "correction_required": false})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApproximateEvaluation {
    pub design: ArrayD<f64>,
    pub response: ArrayD<f64>,
    pub truth: ApproximateTruth,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactEvaluation {
    pub design: ArrayD<f64>,
    pub response: ArrayD<f64>,
    pub truth: ExactTruth,
}

fn flat(a: &ArrayD<f64>) -> Value {
    fn rec(shape: &[usize], data: &[f64]) -> Value {
        if shape.is_empty() {
            return json!(data.first().copied().unwrap_or(0.0));
        }
        let stride: usize = shape[1..].iter().product();
        Value::Array((0..shape[0]).map(|i| rec(&shape[1..], &data[i * stride..(i + 1) * stride])).collect())
    }
    let data: Vec<f64> = a.iter().copied().collect();
    rec(a.shape(), &data)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Evaluation {
    Approximate(ApproximateEvaluation),
    Exact(ExactEvaluation),
}

impl Evaluation {
    #[must_use]
    pub fn authoritative(&self) -> bool {
        matches!(self, Self::Exact(_))
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let (design, response, truth, auth) = match self {
            Self::Approximate(a) => (&a.design, &a.response, a.truth.to_wire(), false),
            Self::Exact(e) => (&e.design, &e.response, e.truth.to_wire(), true),
        };
        json!({"schema": "implexity-approximation-evaluation/1", "design": flat(design), "response": flat(response),
            "authoritative": auth, "truth": truth})
    }
}

pub type ExactEvaluateFn = Box<dyn FnMut(&ArrayD<f64>) -> Result<ArrayD<f64>, CaeError>>;

pub struct ApproximationLane {
    exact_evaluate: ExactEvaluateFn,
    provider: Option<Box<dyn ApproximationProvider>>,
    anchor: Option<ExactAnchor>,
}

fn anchor_id() -> String {
    let mut b = implexity_io::atomic::os_random_bytes(16).unwrap_or_else(|_| {
        let n =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        n.to_le_bytes().to_vec()
    });
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    hex::encode(b)
}

impl ApproximationLane {
    #[must_use]
    pub fn new(exact_evaluate: ExactEvaluateFn, provider: Option<Box<dyn ApproximationProvider>>) -> Self {
        Self { exact_evaluate, provider, anchor: None }
    }

    #[must_use]
    pub fn anchor(&self) -> Option<&ExactAnchor> {
        self.anchor.as_ref()
    }



    pub fn set_anchor(
        &mut self,
        design: &ArrayD<f64>,
        design_scale: Option<&ArrayD<f64>>,
    ) -> ApproxResult<ExactAnchor> {
        let exact = self.evaluate_exact(design, None, None)?;
        self.install_anchor(&exact, design_scale)
    }



    pub fn use_exact_anchor(
        &mut self,
        exact: &ExactEvaluation,
        design_scale: Option<&ArrayD<f64>>,
    ) -> ApproxResult<ExactAnchor> {
        let checked = ExactEvaluation {
            design: finite("exact design", &exact.design, None)?,
            response: finite("exact response", &exact.response, None)?,
            truth: exact.truth.clone(),
        };
        self.install_anchor(&checked, design_scale)
    }



    pub fn evaluate(
        &mut self,
        design: &ArrayD<f64>,
        policy: &ApproximationPolicy,
    ) -> ApproxResult<Evaluation> {
        let target = finite("design", design, None)?;
        if policy.mode == EffortMode::Exact {
            let exact = self.evaluate_exact(&target, None, None)?;
            self.install_anchor(&exact, None)?;
            return Ok(Evaluation::Exact(exact));
        }
        let Some(anchor) = self.anchor.clone() else {
            return self.failure(&target, policy, "no exact anchor is available");
        };
        if self.provider.is_none() {
            return self.failure(&target, policy, "no approximation provider is available");
        }
        if target.shape() != anchor.design.shape() {
            return self.failure(&target, policy, "target design shape differs from anchor");
        }
        let started = Instant::now();
        let proposed = match self.provider.as_ref().map(|p| p.approximate(&anchor, &target, policy)) {
            Some(Ok(p)) => p,
            Some(Err(ApproximationError::PreviewUnavailable(reason))) => {
                return self.failure(&target, policy, &reason);
            }
            Some(Err(e)) => return Err(e),
            None => return self.failure(&target, policy, "no approximation provider is available"),
        };
        let elapsed = started.elapsed().as_secs_f64();
        let proposed = validate_provider_result(&proposed, anchor.response.shape())?;
        if policy.max_preview_seconds.is_some_and(|m| elapsed > m) {
            return self.failure(&target, policy, "preview time budget was exceeded");
        }
        if proposed.normalized_distance > policy.trust_radius {
            return self.failure(&target, policy, "target is outside the trust radius");
        }
        if let Some(max) = policy.max_normalized_error {
            match proposed.normalized_error_bound {
                None => return self.failure(&target, policy, "provider supplied no error bound"),
                Some(b) if b > max => {
                    return self.failure(&target, policy, "preview error bound is too large");
                }
                Some(_) => {}
            }
        }
        Ok(Evaluation::Approximate(ApproximateEvaluation {
            design: target,
            response: proposed.response,
            truth: ApproximateTruth {
                anchor_id: anchor.anchor_id,
                method: proposed.method,
                normalized_distance: proposed.normalized_distance,
                normalized_error_bound: proposed.normalized_error_bound,
                elapsed_seconds: elapsed,
                updates_since_exact: 1,
                exact_correction_due: policy.exact_correction_cadence == 1,
            },
        }))
    }



    pub fn preview_path(
        &mut self,
        designs: &[ArrayD<f64>],
        policy: &ApproximationPolicy,
    ) -> ApproxResult<Vec<ApproximateEvaluation>> {
        if policy.mode == EffortMode::Exact {
            return Err(general("preview_path requires a preview mode"));
        }
        if designs.is_empty() {
            return Err(general("preview_path requires at least one design"));
        }
        if designs.len() > policy.exact_correction_cadence {
            return Err(general("preview_path exceeds exact_correction_cadence"));
        }
        let preview_only = ApproximationPolicy { on_failure: FailurePolicy::Refuse, ..policy.clone() };
        let mut updates: Vec<ApproximateEvaluation> = Vec::new();
        for (index, target) in designs.iter().enumerate() {
            match self.evaluate(target, &preview_only) {
                Ok(Evaluation::Approximate(mut a)) => {
                    a.truth.updates_since_exact = index + 1;
                    a.truth.exact_correction_due = false;
                    updates.push(a);
                }
                Ok(Evaluation::Exact(_)) => {
                    return Err(ApproximationError::ApproximateCommit(
                        "preview_path received an authoritative result before correction".into(),
                    ));
                }
                Err(ApproximationError::PreviewUnavailable(reason)) => {
                    if updates.is_empty() {
                        return Err(ApproximationError::PreviewUnavailable(reason));
                    }
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        match updates.last_mut() {
            Some(last) => last.truth.exact_correction_due = true,
            None => {
                return Err(ApproximationError::PreviewUnavailable(
                    "no bounded preview update is available".into(),
                ));
            }
        }
        Ok(updates)
    }



    pub fn correct(
        &mut self,
        preview: &ApproximateEvaluation,
        install_anchor: bool,
    ) -> ApproxResult<ExactEvaluation> {
        let Some(anchor) = self.anchor.clone().filter(|a| a.anchor_id == preview.truth.anchor_id) else {
            return Err(ApproximationError::ApproximateCommit(
                "preview is stale or does not belong to the current exact anchor".into(),
            ));
        };
        let exact = self.evaluate_exact(&preview.design, Some(preview.truth.anchor_id.clone()), None)?;
        let denominator = norm(exact.response.iter().copied()).max(1.0);
        let observed =
            norm(exact.response.iter().zip(preview.response.iter()).map(|(a, b)| a - b)) / denominator;
        let corrected = ExactEvaluation {
            design: exact.design.clone(),
            response: exact.response.clone(),
            truth: ExactTruth {
                elapsed_seconds: exact.truth.elapsed_seconds,
                corrected_from_anchor_id: Some(preview.truth.anchor_id.clone()),
                observed_normalized_error: Some(observed),
                fallback_reason: None,
            },
        };
        let prior = (anchor.design.shape() == exact.design.shape()).then_some(anchor.design_scale);
        if install_anchor {
            self.install_anchor(&corrected, prior.as_ref())?;
        }
        Ok(corrected)
    }

    pub fn commit<T>(&self, exact: &ExactEvaluation, sink: impl FnOnce(&ExactEvaluation) -> T) -> T {
        sink(exact)
    }

    fn failure(
        &mut self,
        target: &ArrayD<f64>,
        policy: &ApproximationPolicy,
        reason: &str,
    ) -> ApproxResult<Evaluation> {
        if policy.on_failure == FailurePolicy::Refuse {
            return Err(ApproximationError::PreviewUnavailable(reason.into()));
        }
        let exact = self.evaluate_exact(target, None, Some(reason.into()))?;
        self.install_anchor(&exact, None)?;
        Ok(Evaluation::Exact(exact))
    }

    fn evaluate_exact(
        &mut self,
        design: &ArrayD<f64>,
        corrected_from: Option<String>,
        fallback: Option<String>,
    ) -> ApproxResult<ExactEvaluation> {
        let target = finite("design", design, None)?;
        let started = Instant::now();
        let raw = (self.exact_evaluate)(&target).map_err(ApproximationError::Exact)?;
        let response = finite("exact response", &raw, None)?;
        Ok(ExactEvaluation {
            design: target,
            response,
            truth: ExactTruth {
                elapsed_seconds: started.elapsed().as_secs_f64(),
                corrected_from_anchor_id: corrected_from,
                observed_normalized_error: None,
                fallback_reason: fallback,
            },
        })
    }

    fn install_anchor(
        &mut self,
        exact: &ExactEvaluation,
        design_scale: Option<&ArrayD<f64>>,
    ) -> ApproxResult<ExactAnchor> {
        let scale = match design_scale {
            None => ArrayD::from_elem(exact.design.raw_dim(), 1.0),
            Some(s) => {
                let s = finite("design_scale", s, Some(exact.design.shape()))?;
                if s.iter().any(|v| *v <= 0.0) {
                    return Err(general("design_scale must be strictly positive"));
                }
                s
            }
        };
        let anchor = ExactAnchor {
            anchor_id: anchor_id(),
            design: exact.design.clone(),
            response: exact.response.clone(),
            design_scale: scale,
        };
        self.anchor = Some(anchor.clone());
        Ok(anchor)
    }
}

fn validate_provider_result(
    value: &ProviderApproximation,
    expected: &[usize],
) -> ApproxResult<ProviderApproximation> {
    let response = finite("provider response", &value.response, Some(expected))?;
    if !value.normalized_distance.is_finite() || value.normalized_distance < 0.0 {
        return Err(contract("provider distance must be non-negative finite"));
    }
    if value.normalized_error_bound.is_some_and(|b| !b.is_finite() || b < 0.0) {
        return Err(contract("provider error bound must be non-negative finite"));
    }
    if value.method.trim().is_empty() {
        return Err(contract("provider method must be a non-empty string"));
    }
    Ok(ProviderApproximation {
        response,
        normalized_distance: value.normalized_distance,
        normalized_error_bound: value.normalized_error_bound,
        method: value.method.trim().to_string(),
    })
}

