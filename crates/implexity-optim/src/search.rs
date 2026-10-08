// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::contracts::{CoordinateOptimizationSettings, ResponseSpec};
use implexity_core::{CaeError, CaeResult};
use ndarray::{ArrayD, Zip};
use serde_json::{Map, Value};

use crate::bounds::{
    Augmented, BoundMeasures, BoundMultiplierState, augmented_objective, bound_measures,
    bound_program_identity, lookup, update_bound_multipliers,
};
use crate::candidate_events::CandidateAdmission;
use crate::design::{NamedArrays, design_identity};
use crate::design_state::DesignState;
use crate::numeric::{array_sum, float_value, max_abs};

pub const SEARCH_STATE_SCHEMA: &str = "implexity-projected-search-state/2";
pub const SEARCH_OUTCOME_SCHEMA: &str = "implexity-augmented-lagrangian-search-outcome/1";
pub const SEARCH_METHOD: &str = "projected_gradient_augmented_lagrangian_response_bounds";
pub const TERMINAL_REASONS: [&str; 4] = ["converged", "line_search", "penalty_limit", "unchanged"];
pub const MAX_CONSECUTIVE_STALLS: i64 = 2;
const MAX_RECORDED_REJECTIONS: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum TrialFailure {
    Convergence(CaeError),
    NonFinite(String),
    Rejection {
        message: String,
        kind: String,
        decision: String,
    },
}

impl TrialFailure {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Convergence(e) => e.message(),
            Self::NonFinite(m) | Self::Rejection { message: m, .. } => m,
        }
    }

    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Convergence(e) => e.python_class(),
            Self::NonFinite(_) => "NonFiniteTrialResponse",
            Self::Rejection { .. } => "CandidateRejection",
        }
    }


    pub fn rejection(message: &str, kind: &str, decision: &str) -> CaeResult<Self> {
        if kind.is_empty() || decision.is_empty() {
            return Err(CaeError::contract("candidate rejection requires a kind and a decision"));
        }
        Ok(Self::Rejection { message: message.into(), kind: kind.into(), decision: decision.into() })
    }

    #[must_use]
    pub fn into_cae(self) -> CaeError {
        match self {
            Self::Convergence(e) => e,
            Self::NonFinite(m) | Self::Rejection { message: m, .. } => CaeError::convergence(m),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchError {
    Trial(TrialFailure),
    Fatal(CaeError),
}

impl From<CaeError> for SearchError {
    fn from(e: CaeError) -> Self {
        if e.solver_recovery().is_some() { return Self::Fatal(e); }
        if e.is_convergence() { Self::Trial(TrialFailure::Convergence(e)) } else { Self::Fatal(e) }
    }
}

impl From<SearchError> for CaeError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::Trial(t) => t.into_cae(),
            SearchError::Fatal(e) => e,
        }
    }
}

pub type SearchResult<T> = Result<T, SearchError>;

#[derive(Debug, Clone, PartialEq)]
pub struct ExactPoint {
    pub design: NamedArrays,
    pub values: BTreeMap<String, f64>,
    pub gradients: BTreeMap<String, NamedArrays>,
    pub diagnostics: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPoint {
    pub point: ExactPoint,
    pub total: f64,
    pub gradients: NamedArrays,
    pub terms: Vec<Value>,
    pub base_objective: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoxCoordinate {
    pub name: String,
    pub lower: ArrayD<f64>,
    pub upper: ArrayD<f64>,
    pub designable: ArrayD<bool>,
}

impl BoxCoordinate {
    #[must_use]
    pub fn span(&self) -> ArrayD<f64> {
        &self.upper - &self.lower
    }
}

#[must_use]
pub fn box_coordinates(state: &DesignState) -> Vec<BoxCoordinate> {
    state
        .coordinates
        .iter()
        .map(|c| BoxCoordinate {
            name: c.name.clone(),
            lower: c.lower.clone(),
            upper: c.upper.clone(),
            designable: c.designable.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrialInfo {
    pub objective: Option<f64>,
    pub armijo_bound: Option<f64>,
    pub directional: Option<f64>,
    pub error: Option<TrialFailure>,
    pub reason: Option<String>,
}

pub type Candidate = (f64, NamedArrays, Value);

pub type TrialValues = (BTreeMap<String, f64>, Map<String, Value>, Option<ExactPoint>);

pub trait SearchHooks {

    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues>;


    fn sensitivities(&mut self, design: &NamedArrays) -> SearchResult<ExactPoint>;


    fn project(&mut self, _current: &NamedArrays, unprojected: NamedArrays) -> CaeResult<NamedArrays> {
        Ok(unprojected)
    }


    fn admit(
        &mut self,
        _current: &NamedArrays,
        _trial: &NamedArrays,
    ) -> CaeResult<Option<CandidateAdmission>> {
        Ok(None)
    }


    fn accept(
        &mut self,
        _previous: &NamedArrays,
        _design: &NamedArrays,
        _admission: Option<&CandidateAdmission>,
        point: ExactPoint,
    ) -> CaeResult<(Option<Value>, ExactPoint)> {
        Ok((None, point))
    }


    fn candidates(
        &mut self,
        _search: &ProjectedSearch,
        _direction: &NamedArrays,
        _step: f64,
    ) -> CaeResult<Option<Vec<Candidate>>> {
        Ok(None)
    }


    fn evaluate_candidate(
        &mut self,
        _search: &ProjectedSearch,
        _step: f64,
        design: &NamedArrays,
        _meta: &Value,
    ) -> SearchResult<TrialValues> {
        self.values(design)
    }


    #[allow(clippy::too_many_arguments)]
    fn trial_decision(
        &mut self,
        _iteration: i64,
        _attempt: i64,
        _decision: &str,
        _current: &NamedArrays,
        _candidate: &NamedArrays,
        _step: f64,
        _info: &TrialInfo,
    ) -> CaeResult<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchState {
    pub step_policy: String,
    pub trust_step: f64,
    pub stationarity_reference: Option<f64>,
    pub consecutive_stalls: i64,
    pub accepted_since_update: i64,
    pub bound_state: BoundMultiplierState,
}

#[derive(Default)]
struct Log {
    admissions: Vec<CandidateAdmission>,
    rejections: Vec<Value>,
    evaluations: i64,
    armijo_rejections: i64,
    not_descent: i64,
    attempts: i64,
}

struct Accepted {
    point: ExactPoint,
    trial_objective: f64,
    armijo_bound: f64,
    directional: f64,
    delta: f64,
}

#[derive(Debug, Clone)]
pub struct ProjectedSearch {
    pub responses: Vec<ResponseSpec>,
    pub settings: CoordinateOptimizationSettings,
    pub coordinates: Vec<BoxCoordinate>,
    pub bound_state: BoundMultiplierState,
    pub trust_step: f64,
    pub stationarity_reference: Option<f64>,
    pub consecutive_stalls: i64,
    pub accepted_since_update: i64,
    pub current: SearchPoint,
    iteration: i64,
}

fn f(settings_value: implexity_core::pyobj::PyNum) -> f64 {
    settings_value.as_f64()
}

impl ProjectedSearch {

    pub fn new(
        responses: Vec<ResponseSpec>,
        settings: CoordinateOptimizationSettings,
        coordinates: Vec<BoxCoordinate>,
        initial_point: ExactPoint,
        state: Option<SearchState>,
    ) -> CaeResult<Self> {
        let (bound_state, trust_step, reference, stalls, accepted) = match state {
            None => (
                BoundMultiplierState::initial(&responses),
                f64::min(f(settings.step_fraction), f(settings.move_limit)),
                None,
                0,
                0,
            ),
            Some(s) if s.step_policy != settings.step_policy => return Err(CaeError::contract("checkpoint step policy differs from optimization settings")),
            Some(s) => (
                s.bound_state,
                s.trust_step,
                s.stationarity_reference,
                s.consecutive_stalls,
                s.accepted_since_update,
            ),
        };
        if bound_state.program != bound_program_identity(&responses) {
            return Err(CaeError::contract(
                "response-bound multiplier state belongs to another response program",
            ));
        }
        let placeholder = SearchPoint {
            point: initial_point.clone(),
            total: 0.0,
            gradients: NamedArrays::new(),
            terms: Vec::new(),
            base_objective: 0.0,
        };
        let mut search = Self {
            responses,
            settings,
            coordinates,
            bound_state,
            trust_step,
            stationarity_reference: reference,
            consecutive_stalls: stalls,
            accepted_since_update: accepted,
            current: placeholder,
            iteration: 0,
        };
        search.current = search.combine(initial_point)?;
        Ok(search)
    }

    #[must_use]
    pub fn state_wire(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(SEARCH_STATE_SCHEMA.into()));
        m.insert("step_policy".into(), Value::String(self.settings.step_policy.clone()));
        m.insert("trust_step".into(), float_value(self.trust_step));
        m.insert(
            "stationarity_reference".into(),
            self.stationarity_reference.map_or(Value::Null, float_value),
        );
        m.insert("consecutive_stalls".into(), Value::from(self.consecutive_stalls));
        m.insert("accepted_since_update".into(), Value::from(self.accepted_since_update));
        m.insert("bound_state".into(), self.bound_state.to_wire());
        Value::Object(m)
    }


    pub fn state_from_wire(
        raw: &Value,
        responses: &[ResponseSpec],
        settings: &CoordinateOptimizationSettings,
    ) -> CaeResult<SearchState> {
        let legacy = raw.get("schema").and_then(Value::as_str) == Some("implexity-projected-search-state/1");
        let policy = if legacy { "backtracking" } else { raw.get("step_policy").and_then(Value::as_str).unwrap_or("") };
        if policy != settings.step_policy { return Err(CaeError::contract("checkpoint step policy differs from optimization settings")); }
        let keys = [
            "schema",
            "trust_step",
            "stationarity_reference",
            "consecutive_stalls",
            "accepted_since_update",
            "bound_state",
        ];
        let Some(map) = raw.as_object().filter(|m| {
            m.len() == keys.len() + usize::from(!legacy)
                && keys.iter().all(|k| m.contains_key(*k))
                && m.get("schema").and_then(Value::as_str) == Some(if legacy { "implexity-projected-search-state/1" } else { SEARCH_STATE_SCHEMA })
        }) else {
            return Err(CaeError::contract("projected search state is malformed"));
        };
        let finite = |v: &Value, label: &str| -> CaeResult<f64> {
            match implexity_core::pyobj::PyNum::from_value(v).map(implexity_core::pyobj::PyNum::as_f64) {
                Some(x) if x.is_finite() => Ok(x),
                _ => Err(CaeError::contract(format!("{label} must be a finite number"))),
            }
        };
        let step = finite(&map["trust_step"], "projected search trust step")?;
        if (policy == "backtracking" && !(f(settings.minimum_step_fraction) <= step && step <= f(settings.move_limit)))
            || (policy == "fixed_step" && step != f(settings.step_fraction).min(f(settings.move_limit))) {
            return Err(CaeError::contract("projected search trust step lies outside its declared bounds"));
        }
        let reference = match &map["stationarity_reference"] {
            Value::Null => None,
            v => {
                let r = finite(v, "projected search stationarity reference")?;
                if r <= 0.0 {
                    return Err(CaeError::contract(
                        "projected search stationarity reference must be positive",
                    ));
                }
                Some(r)
            }
        };
        let stalls = Some(&map["consecutive_stalls"])
            .filter(|v| crate::pyval::is_int(v))
            .and_then(Value::as_i64)
            .filter(|s| (0..MAX_CONSECUTIVE_STALLS).contains(s))
            .ok_or_else(|| CaeError::contract("projected search stall count is malformed"))?;
        let accepted = Some(&map["accepted_since_update"])
            .filter(|v| crate::pyval::is_int(v))
            .and_then(Value::as_i64)
            .filter(|s| *s >= 0)
            .ok_or_else(|| CaeError::contract("projected search accepted-step count is malformed"))?;
        Ok(SearchState {
            step_policy: policy.into(),
            trust_step: step,
            stationarity_reference: reference,
            consecutive_stalls: stalls,
            accepted_since_update: accepted,
            bound_state: BoundMultiplierState::from_wire(&map["bound_state"], responses)?,
        })
    }

    fn augmented(&self, values: &BTreeMap<String, f64>) -> CaeResult<Augmented> {
        augmented_objective(&self.responses, &lookup(values), &self.bound_state, None)
    }


    pub fn combine(&self, point: ExactPoint) -> CaeResult<SearchPoint> {
        let a = self.augmented(&point.values)?;
        let mut grads = NamedArrays::new();
        for c in &self.coordinates {
            let shape = point.design.get(&c.name).map_or_else(|| c.lower.raw_dim(), ArrayD::raw_dim);
            grads.insert(c.name.clone(), ArrayD::zeros(shape));
        }
        for (spec, coefficient) in self.responses.iter().zip(&a.coefficients) {
            if *coefficient == 0.0 {
                continue;
            }
            let rows = point.gradients.get(&spec.name).ok_or_else(|| {
                CaeError::contract(format!(
                    "exact point omits gradients of {}",
                    implexity_core::py_repr::repr_str(&spec.name)
                ))
            })?;
            for (name, g) in grads.iter_mut() {
                let row = rows
                    .get(name)
                    .ok_or_else(|| CaeError::contract(format!("exact point omits gradient {name}")))?;
                Zip::from(g).and(row).for_each(|g, r| *g += coefficient * r);
            }
        }
        if grads.iter().any(|(_, g)| g.iter().any(|v| !v.is_finite())) {
            return Err(CaeError::contract("nonfinite aggregate response derivative"));
        }
        Ok(SearchPoint { point, total: a.total, gradients: grads, terms: a.terms, base_objective: a.base })
    }


    pub fn trial_objective(&self, values: &BTreeMap<String, f64>) -> CaeResult<f64> {
        Ok(self.augmented(values)?.total)
    }

    #[must_use]
    pub fn direction(&self, projected: &NamedArrays, norm: f64) -> NamedArrays {
        projected.iter().map(|(k, v)| (k.to_string(), v.mapv(|x| -x / norm))).collect()
    }


    pub fn measures(&self) -> CaeResult<BoundMeasures> {
        bound_measures(&self.responses, &lookup(&self.current.point.values), &self.bound_state)
    }

    fn feasible(&self) -> CaeResult<bool> {
        Ok(self.measures()?.max_measure <= f(self.settings.bound_tolerance))
    }

    #[must_use]
    pub fn projected_gradient(&self) -> (NamedArrays, f64) {
        let mut out = NamedArrays::new();
        let mut norm = 0.0_f64;
        for c in &self.coordinates {
            let span = c.span();
            let Some(grad) = self.current.gradients.get(&c.name) else { continue };
            let Some(value) = self.current.point.design.get(&c.name) else { continue };
            let g = grad * &span;
            let data: Vec<f64> = g
                .iter()
                .zip(value.iter())
                .zip(c.lower.iter().zip(c.upper.iter()))
                .zip(span.iter().zip(c.designable.iter()))
                .map(|(((g, v), (lo, hi)), (s, d))| {
                    let tol = 1e-12 * s;
                    let outward = (*v <= lo + tol && *g > 0.0) || (*v >= hi - tol && *g < 0.0);
                    if *d && !outward { *g } else { 0.0 }
                })
                .collect();
            let pg = ArrayD::from_shape_vec(g.raw_dim(), data).unwrap_or_else(|_| ArrayD::zeros(g.raw_dim()));
            if !pg.is_empty() {
                norm = norm.max(max_abs(&pg));
            }
            out.insert(c.name.clone(), pg);
        }
        (out, norm)
    }

    #[must_use]
    pub fn stationarity(&self) -> Value {
        let (_pg, norm) = self.projected_gradient();
        let reference = self.stationarity_reference;
        let tolerance = f(self.settings.stationarity_tolerance);
        let relative = if norm == 0.0 {
            0.0
        } else {
            match reference {
                Some(r) if r != 0.0 => norm / r,
                _ => 1.0,
            }
        };
        let satisfied = norm == 0.0 || reference.is_some_and(|r| norm <= tolerance * r);
        let mut m = Map::new();
        m.insert("projected_gradient_max_norm".into(), float_value(norm));
        m.insert("reference".into(), reference.map_or(Value::Null, float_value));
        m.insert("relative".into(), float_value(relative));
        m.insert("tolerance".into(), float_value(tolerance));
        m.insert("satisfied".into(), Value::Bool(satisfied));
        Value::Object(m)
    }

    fn update_multipliers(&mut self, trigger: &str) -> CaeResult<Value> {
        let (state, record) = update_bound_multipliers(
            &self.responses,
            &lookup(&self.current.point.values),
            &self.bound_state,
            &self.settings,
            trigger,
        )?;
        self.bound_state = state;
        self.accepted_since_update = 0;
        self.current = self.combine(self.current.point.clone())?;
        let mut record = record;
        if let Value::Object(m) = &mut record {
            m.insert("objective_after".into(), float_value(self.current.total));
        }
        Ok(record)
    }

    fn trial(
        &self,
        hooks: &mut dyn SearchHooks,
        direction: &NamedArrays,
        step: f64,
    ) -> CaeResult<NamedArrays> {
        let mut raw = NamedArrays::new();
        for c in &self.coordinates {
            let (Some(value), Some(dir)) = (self.current.point.design.get(&c.name), direction.get(&c.name))
            else {
                continue;
            };
            let span = c.span();
            let mut moved = value.clone();
            Zip::from(&mut moved)
                .and(dir)
                .and(&span)
                .and(&c.lower)
                .and(&c.upper)
                .and(&c.designable)
                .for_each(|m, d, s, lo, hi, des| {
                    if *des {
                        let x = *m + step * d * s;
                        *m = x.max(*lo).min(*hi);
                    }
                });
            raw.insert(c.name.clone(), moved);
        }
        hooks.project(&self.current.point.design, raw)
    }

    fn decision(
        &self,
        hooks: &mut dyn SearchHooks,
        log: &Log,
        decision: &str,
        trial: &NamedArrays,
        step: f64,
        info: &TrialInfo,
    ) -> CaeResult<()> {
        hooks.trial_decision(
            self.iteration,
            log.attempts,
            decision,
            &self.current.point.design,
            trial,
            step,
            info,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn reject(
        &self,
        hooks: &mut dyn SearchHooks,
        log: &mut Log,
        step: f64,
        trial: &NamedArrays,
        failure: TrialFailure,
        sensitivity: bool,
        mut info: TrialInfo,
    ) -> CaeResult<()> {
        let (kind, decision) = match &failure {
            TrialFailure::Rejection { kind, decision, .. } => (kind.clone(), decision.clone()),
            TrialFailure::NonFinite(_) => (
                (if sensitivity { "nonfinite_sensitivity" } else { "nonfinite_response" }).to_string(),
                "numerical_failure".into(),
            ),
            TrialFailure::Convergence(_) => (
                (if sensitivity { "sensitivity_rejected" } else { "numerical_convergence" }).to_string(),
                "numerical_failure".into(),
            ),
        };
        let message: String = failure.message().chars().take(1024).collect();
        let mut row = Map::new();
        row.insert("step_fraction".into(), float_value(step));
        row.insert("kind".into(), Value::String(kind));
        row.insert("message".into(), Value::String(message));
        log.rejections.push(Value::Object(row));
        info.error = Some(failure);
        info.reason = sensitivity.then(|| "candidate_sensitivity".to_string());
        self.decision(hooks, log, &decision, trial, step, &info)
    }

    #[allow(clippy::too_many_lines)]
    fn try_candidate(
        &self,
        hooks: &mut dyn SearchHooks,
        step: f64,
        trial: &NamedArrays,
        meta: Option<&Value>,
        log: &mut Log,
    ) -> CaeResult<Option<Accepted>> {
        let design = &self.current.point.design;
        let mut directional = 0.0_f64;
        let mut first = true;
        for (k, d) in design.iter() {
            let (Some(g), Some(t)) = (self.current.gradients.get(k), trial.get(k)) else {
                return Err(CaeError::contract(format!("trial design omits coordinate {k}")));
            };
            let prod = g * &(t - d);
            let s = array_sum(&prod);

            directional = if first { s } else { directional + s };
            first = false;
        }
        if self.settings.step_policy == "fixed_step" && !directional.is_finite() { return Err(CaeError::convergence("candidate directional derivative is not finite")); }
        if self.settings.step_policy == "backtracking" && (directional.is_nan() || directional >= 0.0) {
            log.not_descent += 1;
            return Ok(None);
        }
        log.attempts += 1;
        let bound = self.current.total + f(self.settings.armijo) * directional;
        self.decision(
            hooks,
            log,
            "started",
            trial,
            step,
            &TrialInfo { armijo_bound: (self.settings.step_policy == "backtracking").then_some(bound), directional: Some(directional), ..TrialInfo::default() },
        )?;
        let evaluated = if self.settings.step_policy == "fixed_step" {
            hooks.sensitivities(trial).map(|p| (p.values.clone(), p.diagnostics.clone(), Some(p)))
        } else { match meta {
            Some(meta) => hooks.evaluate_candidate(self, step, trial, meta),
            None => hooks.values(trial),
        }};
        let (values, _diagnostics, point) = match evaluated {
            Ok(v) => v,
            Err(SearchError::Fatal(e)) => return Err(e),
            Err(SearchError::Trial(t)) => {
                if self.settings.step_policy == "fixed_step" { return Err(t.into_cae()); }
                self.reject(
                    hooks,
                    log,
                    step,
                    trial,
                    t,
                    false,
                    TrialInfo { armijo_bound: (self.settings.step_policy == "backtracking").then_some(bound), ..TrialInfo::default() },
                )?;
                return Ok(None);
            }
        };
        log.evaluations += 1;
        let total = self.trial_objective(&values)?;
        if self.settings.step_policy == "fixed_step" && !total.is_finite() { return Err(CaeError::convergence("candidate objective is not finite")); }
        if self.settings.step_policy == "backtracking" && (total.is_nan() || total > bound) {
            log.armijo_rejections += 1;
            self.decision(
                hooks,
                log,
                "armijo_rejected",
                trial,
                step,
                &TrialInfo {
                    objective: Some(total),
                    armijo_bound: (self.settings.step_policy == "backtracking").then_some(bound),
                    directional: Some(directional),
                    ..TrialInfo::default()
                },
            )?;
            return Ok(None);
        }
        let point = match point {
            Some(p) => p,
            None => match hooks.sensitivities(trial) {
                Ok(p) => p,
                Err(SearchError::Fatal(e)) => return Err(e),
                Err(SearchError::Trial(t)) => {
                    if self.settings.step_policy == "fixed_step" { return Err(t.into_cae()); }
                    self.reject(
                        hooks,
                        log,
                        step,
                        trial,
                        t,
                        true,
                        TrialInfo {
                            objective: Some(total),
                            armijo_bound: (self.settings.step_policy == "backtracking").then_some(bound),
                            ..TrialInfo::default()
                        },
                    )?;
                    return Ok(None);
                }
            },
        };
        let mut delta: Option<f64> = None;
        for (name, v) in &values {
            let exact = point.values.get(name).copied().ok_or_else(|| {
                CaeError::contract(format!(
                    "exact point omits response {}",
                    implexity_core::py_repr::repr_str(name)
                ))
            })?;
            #[allow(clippy::float_cmp)]
            let d = if exact == *v { 0.0 } else { (exact - v).abs() / f64::max(v.abs(), f64::MIN_POSITIVE) };
            delta = Some(match delta {
                None => d,
                Some(m) if d > m => d,
                Some(m) => m,
            });
        }
        Ok(Some(Accepted {
            point,
            trial_objective: total,
            armijo_bound: bound,
            directional,
            delta: delta.unwrap_or(0.0),
        }))
    }

    fn unchanged(&self, trial: &NamedArrays) -> bool {
        trial
            .iter()
            .all(|(k, v)| self.current.point.design.get(k).is_some_and(|d| d.shape() == v.shape() && d == v))
    }

    #[allow(clippy::type_complexity)]
    fn line_search(
        &self,
        hooks: &mut dyn SearchHooks,
        direction: &NamedArrays,
        log: &mut Log,
    ) -> CaeResult<(f64, Option<(NamedArrays, Option<CandidateAdmission>, Accepted)>)> {
        if self.settings.step_policy == "fixed_step" {
            let step = f(self.settings.step_fraction).min(f(self.settings.move_limit));
            let trial = self.trial(hooks, direction, step)?;
            if self.unchanged(&trial) { return Ok((step, None)); }
            let admission = hooks.admit(&self.current.point.design, &trial)?;
            if let Some(a) = &admission {
                log.admissions.push(a.clone());
                if !a.allow || a.max_step_scale < 1.0 { return Err(CaeError::contract(format!("fixed-step candidate outside the physical domain: {}", a.reason))); }
            }
            let result = self.try_candidate(hooks, step, &trial, None, log)?;
            return Ok((step, result.map(|r| (trial, admission, r))));
        }
        let custom = hooks.candidates(self, direction, self.trust_step)?;
        let mut step = self.trust_step;
        let backtracking = f(self.settings.backtracking);
        if let Some(custom) = custom {
            let mut smallest = step;
            for (candidate_step, trial, meta) in custom {
                smallest = smallest.min(candidate_step);
                if self.unchanged(&trial) {
                    continue;
                }
                let admission = hooks.admit(&self.current.point.design, &trial)?;
                if let Some(a) = &admission {
                    log.admissions.push(a.clone());
                    if a.max_step_scale < 1.0 || !a.allow {
                        self.decision(
                            hooks,
                            log,
                            "admission_rejected",
                            &trial,
                            candidate_step,
                            &TrialInfo { reason: Some(a.reason.clone()), ..TrialInfo::default() },
                        )?;
                        continue;
                    }
                }
                if let Some(result) = self.try_candidate(hooks, candidate_step, &trial, Some(&meta), log)? {
                    return Ok((candidate_step, Some((trial, admission, result))));
                }
            }
            step = smallest * backtracking;
        }
        let minimum = f(self.settings.minimum_step_fraction);
        while step >= minimum {
            let trial = self.trial(hooks, direction, step)?;
            if self.unchanged(&trial) {
                step *= backtracking;
                continue;
            }
            let admission = hooks.admit(&self.current.point.design, &trial)?;
            if let Some(a) = &admission {
                log.admissions.push(a.clone());
                if a.max_step_scale < 1.0 {
                    self.decision(
                        hooks,
                        log,
                        "admission_rescaled",
                        &trial,
                        step,
                        &TrialInfo { reason: Some(a.reason.clone()), ..TrialInfo::default() },
                    )?;
                    step *= a.max_step_scale;
                    continue;
                }
                if !a.allow {
                    self.decision(
                        hooks,
                        log,
                        "admission_rejected",
                        &trial,
                        step,
                        &TrialInfo { reason: Some(a.reason.clone()), ..TrialInfo::default() },
                    )?;
                    step *= backtracking;
                    continue;
                }
            }
            if let Some(result) = self.try_candidate(hooks, step, &trial, None, log)? {
                return Ok((step, Some((trial, admission, result))));
            }
            step *= backtracking;
        }
        Ok((step, None))
    }


    #[allow(clippy::too_many_lines)]
    pub fn iterate(&mut self, hooks: &mut dyn SearchHooks, iteration: i64) -> CaeResult<Value> {
        self.iteration = iteration;
        let start_objective = self.current.total;
        let start_design_id = design_identity(&self.current.point.design)?;
        let mut updates: Vec<Value> = Vec::new();
        let mut log = Log::default();
        let mut stationary_updates = 0_i64;
        let ratio = f(self.settings.penalty_limit).ln() / f(self.settings.penalty_growth).ln();
        #[allow(clippy::cast_possible_truncation)]
        let max_stationary_updates = ratio.ceil() as i64 + 2;
        let mut start_stationarity: Option<Value> = None;
        let (pg, norm) = loop {
            let (pg, norm) = self.projected_gradient();
            if self.stationarity_reference.is_none() && norm > 0.0 {
                self.stationarity_reference = Some(norm);
            }
            let stationarity = self.stationarity();
            if start_stationarity.is_none() {
                start_stationarity = Some(stationarity.clone());
            }
            let start = start_stationarity.clone().unwrap_or(Value::Null);
            if stationarity.get("satisfied") != Some(&Value::Bool(true)) {
                break (pg, norm);
            }
            if self.feasible()? {
                return self.row(
                    iteration,
                    start_objective,
                    &start_design_id,
                    &start,
                    &updates,
                    &log,
                    RowKind::stopped("converged"),
                );
            }
            let at_limit = updates
                .last()
                .and_then(|u| u.get("violating_penalties_at_limit"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if stationary_updates >= max_stationary_updates || at_limit {
                return self.row(
                    iteration,
                    start_objective,
                    &start_design_id,
                    &start,
                    &updates,
                    &log,
                    RowKind::stopped("penalty_limit"),
                );
            }
            updates.push(self.update_multipliers("inner_stationarity")?);
            stationary_updates += 1;
        };
        let start = start_stationarity.unwrap_or(Value::Null);
        let direction = self.direction(&pg, norm);
        let (step, found) = self.line_search(hooks, &direction, &mut log)?;
        if self.settings.step_policy == "fixed_step" && found.is_none() {
            return self.row(iteration, start_objective, &start_design_id, &start, &updates, &log, RowKind::stopped("unchanged"));
        }
        if let Some((trial, admission, result)) = found {
            let previous = self.current.point.design.clone();
            let (commit, point) =
                hooks.accept(&previous, &trial, admission.as_ref(), result.point.clone())?;
            self.decision(
                hooks,
                &log,
                "accepted",
                &trial,
                step,
                &TrialInfo {
                    objective: Some(result.trial_objective),
                    armijo_bound: (self.settings.step_policy == "backtracking").then_some(result.armijo_bound),
                    directional: Some(result.directional),
                    ..TrialInfo::default()
                },
            )?;
            self.current = self.combine(point)?;
            self.consecutive_stalls = 0;
            self.accepted_since_update += 1;
            self.trust_step = if self.settings.step_policy == "fixed_step" { step } else { f64::min(f(self.settings.move_limit), step * f(self.settings.step_growth)) };
            if !self.bound_state.multipliers.is_empty()
                && !self.feasible()?
                && self.accepted_since_update >= self.settings.multiplier_update_interval
            {
                updates.push(self.update_multipliers("update_interval")?);
            }
            return self.row(
                iteration,
                start_objective,
                &start_design_id,
                &start,
                &updates,
                &log,
                RowKind::Accepted { step, admission, commit, result: Box::new(result) },
            );
        }
        self.trust_step =
            f64::min(f(self.settings.move_limit), f64::max(f(self.settings.minimum_step_fraction), step));
        let mut reason = None;
        if !self.bound_state.multipliers.is_empty()
            && !self.feasible()?
            && self.consecutive_stalls + 1 < MAX_CONSECUTIVE_STALLS
        {
            self.consecutive_stalls += 1;
            updates.push(self.update_multipliers("line_search_stall")?);
            self.trust_step = f64::min(f(self.settings.step_fraction), f(self.settings.move_limit));
        } else {
            reason = Some("line_search");
        }
        let admission = log.admissions.last().cloned();
        self.row(
            iteration,
            start_objective,
            &start_design_id,
            &start,
            &updates,
            &log,
            RowKind::Rejected { reason, admission },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        iteration: i64,
        start_objective: f64,
        start_design_id: &str,
        stationarity: &Value,
        updates: &[Value],
        log: &Log,
        kind: RowKind,
    ) -> CaeResult<Value> {
        let measures = self.measures()?;
        let (accepted, step, reason) = match &kind {
            RowKind::Accepted { step, .. } => (true, *step, None),
            RowKind::Rejected { reason, .. } => (false, 0.0, *reason),
        };
        let mut row = Map::new();
        row.insert("iteration".into(), Value::from(iteration));
        row.insert("objective".into(), float_value(start_objective));
        row.insert("objective_after".into(), float_value(self.current.total));
        row.insert("base_objective".into(), float_value(self.current.base_objective));
        row.insert(
            "gradient_norm".into(),
            stationarity.get("projected_gradient_max_norm").cloned().unwrap_or(Value::Null),
        );
        row.insert("stationarity".into(), stationarity.clone());
        row.insert("accepted".into(), Value::Bool(accepted));
        row.insert("step_fraction".into(), float_value(step));
        row.insert("trust_step".into(), float_value(self.trust_step));
        row.insert("terms".into(), Value::Array(self.current.terms.clone()));
        row.insert("max_scaled_bound_violation".into(), float_value(measures.max_scaled_violation));
        row.insert("bound_measure".into(), float_value(measures.max_measure));
        let multipliers: Vec<Value> = measures
            .bounds
            .iter()
            .map(|r| {
                let mut m = Map::new();
                for key in ["response", "sense", "multiplier", "penalty", "scaled_constraint"] {
                    m.insert(key.into(), r.get(key).cloned().unwrap_or(Value::Null));
                }
                Value::Object(m)
            })
            .collect();
        row.insert("bound_multipliers".into(), Value::Array(multipliers));
        row.insert("multiplier_updates".into(), Value::Array(updates.to_vec()));
        row.insert("trial_evaluations".into(), Value::from(log.evaluations));
        row.insert("step_policy".into(), Value::String(self.settings.step_policy.clone()));
        row.insert("armijo_rejections".into(), Value::from(log.armijo_rejections));
        row.insert("design_state_id".into(), Value::String(start_design_id.to_string()));
        row.insert("terminal".into(), Value::Bool(reason.is_some()));
        if let Some(r) = reason {
            row.insert("reason".into(), Value::String(r.into()));
        }
        if log.not_descent != 0 {
            row.insert("non_descent_trials".into(), Value::from(log.not_descent));
        }
        if let Some(last) = log.rejections.last() {
            row.insert("numerical_trial_rejections".into(), Value::from(log.rejections.len()));
            let start = log.rejections.len().saturating_sub(MAX_RECORDED_REJECTIONS);
            row.insert("numerical_rejections".into(), Value::Array(log.rejections[start..].to_vec()));
            row.insert(
                "last_numerical_trial_rejection".into(),
                last.get("message").cloned().unwrap_or(Value::Null),
            );
        }
        if !log.admissions.is_empty() {
            row.insert("candidate_admission_attempts".into(), Value::from(log.admissions.len()));
        }
        let admission = match &kind {
            RowKind::Accepted { admission, .. } | RowKind::Rejected { admission, .. } => admission,
        };
        if let Some(a) = admission {
            row.insert("candidate_admission".into(), a.to_value());
        }
        if let RowKind::Accepted { commit, result, .. } = kind {
            row.insert(
                "accepted_design_state_id".into(),
                Value::String(design_identity(&self.current.point.design)?),
            );
            let mut armijo = Map::new();
            armijo.insert("trial_objective".into(), float_value(result.trial_objective));
            armijo.insert("bound".into(), float_value(result.armijo_bound));
            armijo.insert("directional_derivative".into(), float_value(result.directional));
            if self.settings.step_policy == "backtracking" { row.insert("armijo".into(), Value::Object(armijo)); }
            row.insert("accepted_value_max_relative_difference".into(), float_value(result.delta));
            if let Some(c) = commit {
                row.insert("commit".into(), c);
            }
        }
        Ok(Value::Object(row))
    }


    pub fn outcome(&self, history: &[Value]) -> CaeResult<Value> {
        let stationarity = self.stationarity();
        let measures = self.measures()?;
        let tolerance = f(self.settings.bound_tolerance);
        let feasible = measures.max_measure <= tolerance;
        let stationary = stationarity.get("satisfied") == Some(&Value::Bool(true));
        let converged = stationary && feasible;
        let last_reason =
            history.last().and_then(|r| r.get("reason")).and_then(Value::as_str).filter(|s| !s.is_empty());
        let reason = last_reason.unwrap_or(if converged { "converged" } else { "iteration_limit" });
        if reason == "converged" && !converged {
            return Err(CaeError::contract(
                "search recorded convergence that its final state does not support",
            ));
        }
        let mut bounds = Map::new();
        bounds.insert("treatment".into(), Value::String("soft_augmented_lagrangian_response_bounds".into()));
        bounds.insert("tolerance".into(), float_value(tolerance));
        bounds.insert("satisfied_within_tolerance".into(), Value::Bool(feasible));
        bounds.insert("max_scaled_violation".into(), float_value(measures.max_scaled_violation));
        bounds.insert("max_measure".into(), float_value(measures.max_measure));
        bounds.insert("multiplier_updates".into(), Value::from(self.bound_state.updates));
        bounds.insert("bounds".into(), Value::Array(measures.bounds));
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(SEARCH_OUTCOME_SCHEMA.into()));
        m.insert("search_method".into(), Value::String(SEARCH_METHOD.into()));
        m.insert("step_policy".into(), Value::String(self.settings.step_policy.clone()));
        m.insert("termination_reason".into(), Value::String(reason.into()));
        m.insert("optimization_converged".into(), Value::Bool(converged));
        m.insert("search_stationary".into(), Value::Bool(stationary));
        m.insert("projected_stationarity".into(), stationarity);
        m.insert("response_bounds".into(), Value::Object(bounds));
        m.insert(
            "convergence_scope".into(),
            Value::String("projected_stationarity_and_response_bounds_within_tolerance".into()),
        );
        m.insert("final_acceptance_performed".into(), Value::Bool(false));
        Ok(Value::Object(m))
    }

    #[must_use]
    pub fn iteration(&self) -> i64 {
        self.iteration
    }
}

enum RowKind {
    Accepted {
        step: f64,
        admission: Option<CandidateAdmission>,
        commit: Option<Value>,
        result: Box<Accepted>,
    },
    Rejected {
        reason: Option<&'static str>,
        admission: Option<CandidateAdmission>,
    },
}

impl RowKind {
    fn stopped(reason: &'static str) -> Self {
        Self::Rejected { reason: Some(reason), admission: None }
    }
}
