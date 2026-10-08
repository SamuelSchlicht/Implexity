// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::contracts::{CoordinateOptimizationSettings, ResponseSpec};
use implexity_core::{CaeError, CaeResult};
use crate::bounds::{
    BOUND_SENSES, BoundMeasures, BoundMultiplierState, augmented_objective, bound_measures,
    bound_program_identity, lookup, update_bound_multipliers,
};
use crate::candidate_events::CandidateAdmission;
use crate::numeric::float_value;
use crate::response_program::measurement_names;
use crate::search::{
    ExactPoint, MAX_CONSECUTIVE_STALLS, SEARCH_METHOD, SEARCH_OUTCOME_SCHEMA, SEARCH_STATE_SCHEMA,
    SearchError, SearchPoint, SearchResult, SearchState, TrialFailure, TrialInfo, TrialValues,
};
use crate::{NamedArrays, design_identity};
use ndarray::{ArrayD, Zip};
use serde_json::{Map, Value};

const MAX_RECORDED_REJECTIONS: usize = 16;

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn f(value: implexity_core::pyobj::PyNum) -> f64 {
    value.as_f64()
}

#[must_use]
pub fn point_key(point: i64, name: &str) -> String {
    format!("op{point}:{name}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub total: f64,
    pub base: f64,
    pub weights: Vec<f64>,
    pub coefficients: Vec<f64>,
    pub terms: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageResponseProgram {
    pub responses: Vec<ResponseSpec>,
    pub names: Vec<String>,
    pub points: Vec<i64>,
    pub mode: String,
    pub beta: f64,
    pub per_point: Vec<Vec<ResponseSpec>>,
    pub expanded: Vec<ResponseSpec>,
    pub bound_count: usize,
    pub identity: String,
    pub location: BTreeMap<String, (String, i64)>,
}

impl StageResponseProgram {

    pub fn new(
        responses: &[ResponseSpec],
        operating_points: &[i64],
        robust_mode: &str,
        robust_beta: f64,
    ) -> CaeResult<Self> {
        let unique: std::collections::BTreeSet<&i64> = operating_points.iter().collect();
        if operating_points.is_empty()
            || operating_points.iter().any(|p| *p < 0)
            || unique.len() != operating_points.len()
        {
            return contract("response program requires distinct nonnegative operating points");
        }
        let (points, mode) = if operating_points.len() == 1 || robust_mode == "nominal" {
            (operating_points[..1].to_vec(), "nominal".to_string())
        } else if robust_mode == "expected" || robust_mode == "smooth_worst_case" {
            (operating_points.to_vec(), robust_mode.to_string())
        } else {
            return contract(format!(
                "unsupported robust mode {}",
                implexity_core::py_repr::repr_str(robust_mode)
            ));
        };
        if mode == "smooth_worst_case" && (!robust_beta.is_finite() || robust_beta <= 0.0) {
            return contract("smooth worst-case aggregation requires a positive finite beta");
        }
        let names = measurement_names(responses)?;
        let per_point: Vec<Vec<ResponseSpec>> = points
            .iter()
            .map(|p| {
                responses
                    .iter()
                    .map(|spec| {
                        let mut s = spec.clone();
                        s.name = point_key(*p, &spec.name);
                        s
                    })
                    .collect()
            })
            .collect();
        let expanded: Vec<ResponseSpec> = per_point.iter().flatten().cloned().collect();
        let bound_count = responses.iter().filter(|s| BOUND_SENSES.contains(&s.sense.as_str())).count();
        let identity = bound_program_identity(&expanded);
        let mut location = BTreeMap::new();
        for p in &points {
            for n in &names {
                location.insert(point_key(*p, n), (n.clone(), *p));
            }
        }
        Ok(Self {
            responses: responses.to_vec(),
            names,
            points,
            mode,
            beta: robust_beta,
            per_point,
            expanded,
            bound_count,
            identity,
            location,
        })
    }

    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.points.iter().flat_map(|p| self.names.iter().map(move |n| point_key(*p, n))).collect()
    }


    pub fn aggregate(
        &self,
        values: &BTreeMap<String, f64>,
        state: &BoundMultiplierState,
    ) -> CaeResult<Aggregate> {
        if state.program != self.identity {
            return contract("response-bound multiplier state belongs to another response program");
        }
        let count = self.bound_count;
        let mut totals = Vec::new();
        let mut bases = Vec::new();
        let mut coefficients: Vec<Vec<f64>> = Vec::new();
        let mut terms = Vec::new();
        for (index, (point, specs)) in self.points.iter().zip(&self.per_point).enumerate() {
            let window = index * count..(index + 1) * count;
            let local = BoundMultiplierState {
                program: bound_program_identity(specs),
                multipliers: state.multipliers[window.clone()].to_vec(),
                penalties: state.penalties[window.clone()].to_vec(),
                initial_penalties: state.initial_penalties[window].to_vec(),
                updates: state.updates,
                reference_measure: state.reference_measure,
            };
            let a = augmented_objective(specs, &lookup(values), &local, None)?;
            totals.push(a.total);
            bases.push(a.base);
            coefficients.push(a.coefficients);
            let prefix = point_key(*point, "").len();
            for term in a.terms {
                let response = term.get("response").and_then(Value::as_str).unwrap_or("");
                let mut row = Map::new();
                row.insert(
                    "response".into(),
                    Value::String(response.get(prefix..).unwrap_or("").to_string()),
                );
                row.insert("value".into(), term.get("value").cloned().unwrap_or(Value::Null));
                row.insert(
                    "objective_contribution".into(),
                    term.get("objective_contribution").cloned().unwrap_or(Value::Null),
                );
                row.insert("operating_point".into(), Value::from(*point));
                terms.push(Value::Object(row));
            }
        }
        let n = totals.len();
        let (weights, total, base) = match self.mode.as_str() {
            "nominal" => (vec![1.0], totals[0], bases[0]),
            "expected" => {
                #[allow(clippy::cast_precision_loss)]
                let w = 1.0 / n as f64;
                let weights = vec![w; n];
                let total = totals.iter().fold(0.0, |acc, t| acc + w * t);
                let base = bases.iter().fold(0.0, |acc, b| acc + w * b);
                (weights, total, base)
            }
            _ => {
                let peak = totals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let scaled: Vec<f64> = totals.iter().map(|t| (self.beta * (t - peak)).exp()).collect();
                let sum: f64 = scaled.iter().sum();
                let weights: Vec<f64> = scaled.iter().map(|s| s / sum).collect();
                let total = peak + sum.ln() / self.beta;
                let base_peak = bases.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let base_sum: f64 = bases.iter().map(|b| (self.beta * (b - base_peak)).exp()).sum();
                let base = base_peak + base_sum.ln() / self.beta;
                (weights, total, base)
            }
        };
        if !total.is_finite() || !base.is_finite() {
            return contract("nonfinite aggregate response objective");
        }
        let flat: Vec<f64> = weights
            .iter()
            .zip(&coefficients)
            .flat_map(|(w, group)| group.iter().map(move |c| w * c))
            .collect();
        Ok(Aggregate { total, base, weights, coefficients: flat, terms })
    }


    pub fn carry(&self, previous: &Self, state: &BoundMultiplierState) -> CaeResult<BoundMultiplierState> {

        let same_roles = previous.responses.len() == self.responses.len()
            && previous.responses.iter().zip(&self.responses).all(|(a, b)| {
                a.name == b.name
                    && a.sense == b.sense
                    && a.scale == b.scale
                    && a.weight == b.weight
                    && a.target.is_some() == b.target.is_some()
            });
        if state.program != previous.identity || !same_roles {
            return contract("carried multiplier state belongs to another response program");
        }
        if state.program == self.identity {
            return Ok(state.clone());
        }
        let count = self.bound_count;
        let index: BTreeMap<i64, usize> = previous.points.iter().enumerate().map(|(i, p)| (*p, i)).collect();
        let mut multipliers = Vec::new();
        let mut penalties = Vec::new();
        for point in &self.points {
            let source = index.get(point).copied().unwrap_or(0) * count;
            multipliers.extend_from_slice(&state.multipliers[source..source + count]);
            penalties.extend_from_slice(&state.penalties[source..source + count]);
        }
        let initial = BoundMultiplierState::initial(&self.expanded);
        Ok(BoundMultiplierState {
            multipliers,
            penalties,
            updates: state.updates,
            reference_measure: None,
            ..initial
        })
    }

    #[must_use]
    pub fn point_measures(&self, measures: &BoundMeasures) -> BoundMeasures {
        let bounds = measures
            .bounds
            .iter()
            .map(|row| {
                let mut row = row.as_object().cloned().unwrap_or_default();
                let key = row.get("response").and_then(Value::as_str).unwrap_or("").to_string();
                if let Some((name, point)) = self.location.get(&key) {
                    row.insert("response".into(), Value::String(name.clone()));
                    row.insert("operating_point".into(), Value::from(*point));
                }
                Value::Object(row)
            })
            .collect();
        BoundMeasures { bounds, ..measures.clone() }
    }
}


pub fn normalise_update_metric(raw: Option<&Value>) -> CaeResult<Map<String, Value>> {
    let (value, alpha_raw) = match raw {
        Some(Value::Object(m)) => {
            let mut unknown: Vec<&String> = m.keys().filter(|k| *k != "mode" && *k != "alpha").collect();
            if !unknown.is_empty() {
                unknown.sort();
                return contract(format!(
                    "update_metric contains unknown fields {}",
                    implexity_core::pyobj::list_repr(&unknown)
                ));
            }
            let mode = m
                .get("mode")
                .filter(|v| implexity_core::pyobj::truthy(v))
                .map_or_else(|| "gradient_adaptive".to_string(), implexity_core::pyobj::py_str);
            (mode.trim().to_lowercase(), m.get("alpha").cloned().unwrap_or(Value::from(0.5)))
        }
        None | Some(Value::Null) => ("gradient_adaptive".to_string(), Value::from(0.5)),
        Some(other) => (implexity_core::pyobj::py_str(other).trim().to_lowercase(), Value::from(0.5)),
    };
    let mode = match value.as_str() {
        "global" | "global_max" => "global_max",
        "family" | "block_balanced" | "per_coordinate" | "family_balanced" => "family_balanced",
        "adaptive" | "gradient_adaptive" => "gradient_adaptive",
        _ => return contract("update_metric mode must be global_max, gradient_adaptive, or family_balanced"),
    };
    let Some(alpha) = alpha_raw.as_f64().filter(|a| alpha_raw.is_number() && a.is_finite()) else {
        return contract("update_metric alpha must be finite");
    };
    if !(0.0..=1.0).contains(&alpha) {
        return contract("update_metric alpha must be between 0 and 1");
    }
    let alpha = match mode {
        "global_max" => 0.0,
        "family_balanced" => 1.0,
        _ => alpha,
    };
    let mut out = Map::new();
    out.insert("mode".into(), Value::String(mode.into()));
    out.insert("alpha".into(), Value::from(alpha));
    Ok(out)
}


pub fn coordinate_update_denominators(
    effective: &BTreeMap<String, ArrayD<f64>>,
    bounded: &BTreeMap<String, ArrayD<f64>>,
    active: &[String],
    metric: &Map<String, Value>,
    masks: &BTreeMap<String, ArrayD<bool>>,
) -> CaeResult<BTreeMap<String, f64>> {
    let metric = normalise_update_metric(Some(&Value::Object(metric.clone())))?;
    if active.is_empty() {
        return Ok(BTreeMap::new());
    }
    let finite = |m: &BTreeMap<String, ArrayD<f64>>| {
        active.iter().all(|n| m.get(n).is_some_and(|a| a.iter().all(|v| v.is_finite())))
    };
    if !finite(effective) || !finite(bounded) {
        return contract("optimizer update metric received a nonfinite gradient");
    }
    if metric["mode"] == "global_max" {
        let scale = active
            .iter()
            .filter_map(|n| effective.get(n))
            .map(|a| a.iter().fold(0.0_f64, |m, v| m.max(v.abs())))
            .fold(0.0_f64, f64::max);
        return Ok(active.iter().map(|n| (n.clone(), scale)).collect());
    }
    let mut rms = BTreeMap::new();
    for name in active {
        let Some(value) = bounded.get(name) else { continue };
        let free: Vec<f64> = match masks.get(name) {
            None => value.iter().copied().collect(),
            Some(mask) => value.iter().zip(mask.iter()).filter(|(_, m)| **m).map(|(v, _)| *v).collect(),
        };
        let r = if free.is_empty() {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let n = free.len() as f64;
            implexity_linalg_norm(&free) / n.sqrt()
        };
        rms.insert(name.clone(), r);
    }
    let strongest = rms.values().copied().fold(0.0_f64, f64::max);
    let floor = 0.05 * strongest;
    let alpha = metric["alpha"].as_f64().unwrap_or(0.5);
    if strongest <= 0.0 {
        return Ok(active.iter().map(|n| (n.clone(), 0.0)).collect());
    }
    Ok(active
        .iter()
        .map(|n| {
            let r = rms.get(n).copied().unwrap_or(0.0);
            (n.clone(), strongest.powf(1.0 - alpha) * r.max(floor).powf(alpha))
        })
        .collect())
}

fn implexity_linalg_norm(values: &[f64]) -> f64 {
    let s: f64 = values.iter().map(|v| v * v).sum();
    s.sqrt()
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageCoordinate {
    pub name: String,
    pub lower: ArrayD<f64>,
    pub upper: ArrayD<f64>,
    pub designable: ArrayD<bool>,
}

impl StageCoordinate {
    #[must_use]
    pub fn span(&self) -> ArrayD<f64> {
        &self.upper - &self.lower
    }
}

pub trait StageSearchHooks {

    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues>;

    fn sensitivities(&mut self, design: &NamedArrays) -> SearchResult<ExactPoint>;

    fn project(&mut self, current: &NamedArrays, unprojected: NamedArrays) -> CaeResult<NamedArrays>;

    fn admit(&mut self, current: &NamedArrays, trial: &NamedArrays) -> CaeResult<Option<CandidateAdmission>>;

    fn accept(
        &mut self,
        previous: &NamedArrays,
        design: &NamedArrays,
        admission: Option<&CandidateAdmission>,
        point: ExactPoint,
    ) -> CaeResult<(Option<Value>, ExactPoint)>;

    fn candidates(
        &mut self,
        search: &StageSearch,
        direction: &NamedArrays,
        step: f64,
    ) -> CaeResult<Option<Vec<(f64, NamedArrays, usize)>>>;

    fn evaluate_candidate(
        &mut self,
        search: &StageSearch,
        step: f64,
        design: &NamedArrays,
        meta: usize,
    ) -> SearchResult<TrialValues>;

    #[allow(clippy::too_many_arguments)]
    fn trial_decision(
        &mut self,
        iteration: i64,
        attempt: i64,
        decision: &str,
        current: &NamedArrays,
        candidate: &NamedArrays,
        step: f64,
        info: &TrialInfo,
    ) -> CaeResult<()>;

    fn recover(&mut self, search: &StageSearch) -> CaeResult<Option<(ExactPoint, Value)>> {
        let _ = search;
        Ok(None)
    }
}

pub const MAX_CONSECUTIVE_NUMERICAL_REJECTIONS: usize = 6;

#[derive(Default)]
struct Log {
    admissions: Vec<CandidateAdmission>,
    rejections: Vec<Value>,
    evaluations: i64,
    armijo_rejections: i64,
    not_descent: i64,
    attempts: i64,
    consecutive_numerical: usize,
    recovery: Option<Value>,
}

struct Accepted {
    point: ExactPoint,
    trial_objective: f64,
    armijo_bound: f64,
    directional: f64,
    delta: f64,
}

#[derive(Debug, Clone)]
pub struct StageSearch {
    pub program: StageResponseProgram,
    pub step_scales: BTreeMap<String, f64>,
    pub update_metric: Map<String, Value>,
    pub last_denominators: BTreeMap<String, f64>,
    pub settings: CoordinateOptimizationSettings,
    pub coordinates: Vec<StageCoordinate>,
    pub bound_state: BoundMultiplierState,
    pub trust_step: f64,
    pub stationarity_reference: Option<f64>,
    pub consecutive_stalls: i64,
    pub accepted_since_update: i64,
    pub current: SearchPoint,
    iteration: i64,
}

impl StageSearch {

    pub fn new(
        program: StageResponseProgram,
        step_scales: BTreeMap<String, f64>,
        update_metric: Map<String, Value>,
        settings: CoordinateOptimizationSettings,
        coordinates: Vec<StageCoordinate>,
        initial_point: ExactPoint,
        state: SearchState,
    ) -> CaeResult<Self> {
        if state.step_policy != settings.step_policy { return contract("checkpoint step policy differs from optimization settings"); }
        if state.bound_state.program != bound_program_identity(&program.expanded) {
            return contract("response-bound multiplier state belongs to another response program");
        }
        let placeholder = SearchPoint {
            point: initial_point.clone(),
            total: 0.0,
            gradients: NamedArrays::new(),
            terms: Vec::new(),
            base_objective: 0.0,
        };
        let mut search = Self {
            program,
            step_scales,
            update_metric,
            last_denominators: BTreeMap::new(),
            settings,
            coordinates,
            bound_state: state.bound_state,
            trust_step: state.trust_step,
            stationarity_reference: state.stationarity_reference,
            consecutive_stalls: state.consecutive_stalls,
            accepted_since_update: state.accepted_since_update,
            current: placeholder,
            iteration: 0,
        };
        search.current = search.combine(initial_point)?;
        Ok(search)
    }

    #[must_use]
    pub fn fresh_state(
        program: &StageResponseProgram,
        settings: &CoordinateOptimizationSettings,
    ) -> SearchState {
        SearchState {
            step_policy: settings.step_policy.clone(),
            trust_step: f(settings.step_fraction).min(f(settings.move_limit)),
            stationarity_reference: None,
            consecutive_stalls: 0,
            accepted_since_update: 0,
            bound_state: BoundMultiplierState::initial(&program.expanded),
        }
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


    pub fn combine(&self, point: ExactPoint) -> CaeResult<SearchPoint> {
        let combined = self.program.aggregate(&point.values, &self.bound_state)?;
        let mut grads = NamedArrays::new();
        for c in &self.coordinates {
            grads.insert(c.name.clone(), ArrayD::zeros(c.lower.raw_dim()));
        }
        for (spec, coefficient) in self.program.expanded.iter().zip(&combined.coefficients) {
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
            return contract("nonfinite aggregate response derivative");
        }
        Ok(SearchPoint {
            point,
            total: combined.total,
            gradients: grads,
            terms: combined.terms,
            base_objective: combined.base,
        })
    }


    pub fn trial_objective(&self, values: &BTreeMap<String, f64>) -> CaeResult<f64> {
        Ok(self.program.aggregate(values, &self.bound_state)?.total)
    }


    pub fn weights(&self) -> CaeResult<Vec<f64>> {
        Ok(self.program.aggregate(&self.current.point.values, &self.bound_state)?.weights)
    }


    pub fn measures(&self) -> CaeResult<BoundMeasures> {
        let raw =
            bound_measures(&self.program.expanded, &lookup(&self.current.point.values), &self.bound_state)?;
        Ok(self.program.point_measures(&raw))
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
            let (Some(grad), Some(value)) =
                (self.current.gradients.get(&c.name), self.current.point.design.get(&c.name))
            else {
                continue;
            };
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
                norm = norm.max(pg.iter().fold(0.0_f64, |m, v| m.max(v.abs())));
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
            &self.program.expanded,
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


    pub fn direction(&mut self, projected: &NamedArrays) -> CaeResult<NamedArrays> {
        let active: Vec<String> = projected
            .iter()
            .filter(|(name, _)| {
                self.coordinates.iter().any(|c| c.name == *name && c.designable.iter().any(|d| *d))
            })
            .map(|(n, _)| n.to_string())
            .collect();
        let scale = |n: &str| self.step_scales.get(n).copied().unwrap_or(1.0);
        let effective: BTreeMap<String, ArrayD<f64>> =
            active.iter().filter_map(|n| projected.get(n).map(|p| (n.clone(), p * scale(n)))).collect();
        let bounded: BTreeMap<String, ArrayD<f64>> =
            active.iter().filter_map(|n| projected.get(n).map(|p| (n.clone(), p.clone()))).collect();
        let masks: BTreeMap<String, ArrayD<bool>> = active
            .iter()
            .filter_map(|n| {
                self.coordinates.iter().find(|c| &c.name == n).map(|c| (n.clone(), c.designable.clone()))
            })
            .collect();
        let denominators =
            coordinate_update_denominators(&effective, &bounded, &active, &self.update_metric, &masks)?;
        self.last_denominators = denominators.clone();
        let mut out = NamedArrays::new();
        for (name, value) in projected.iter() {
            let denominator = denominators.get(name).copied().unwrap_or(0.0);
            let dir = match effective.get(name) {
                Some(e) if denominator > 0.0 => e.mapv(|v| -v / denominator),
                _ => ArrayD::zeros(value.raw_dim()),
            };
            out.insert(name.to_string(), dir);
        }
        Ok(out)
    }


    pub fn trial_from(
        &self,
        hooks: &mut dyn StageSearchHooks,
        source: &NamedArrays,
        direction: &NamedArrays,
        step: f64,
    ) -> CaeResult<NamedArrays> {
        let current = &self.current.point.design;
        let mut raw = NamedArrays::new();
        for c in &self.coordinates {
            let (Some(src), Some(cur), Some(dir)) =
                (source.get(&c.name), current.get(&c.name), direction.get(&c.name))
            else {
                return contract(format!("trial design omits coordinate {}", c.name));
            };
            let span = c.span();
            let move_limit =
                self.step_scales.get(&c.name).copied().unwrap_or(1.0) * f(self.settings.move_limit);
            let data: Vec<f64> = src
                .iter()
                .zip(cur.iter())
                .zip(dir.iter().zip(span.iter()))
                .zip(c.lower.iter().zip(c.upper.iter()).zip(c.designable.iter()))
                .map(|(((s, cur), (d, sp)), ((lo, hi), des))| {
                    if !*des {
                        return *cur;
                    }
                    let mv = move_limit * sp;
                    let x = s + step * d * sp;
                    x.max(cur - mv).min(cur + mv).max(*lo).min(*hi)
                })
                .collect();
            let moved = ArrayD::from_shape_vec(src.raw_dim(), data)
                .map_err(|_| CaeError::contract(format!("trial design shape of {} drifted", c.name)))?;
            raw.insert(c.name.clone(), moved);
        }
        hooks.project(current, raw)
    }

    fn decision(
        &self,
        hooks: &mut dyn StageSearchHooks,
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
        hooks: &mut dyn StageSearchHooks,
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
        log.consecutive_numerical += 1;
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
        hooks: &mut dyn StageSearchHooks,
        step: f64,
        trial: &NamedArrays,
        meta: Option<usize>,
        log: &mut Log,
    ) -> CaeResult<Option<Accepted>> {
        let design = &self.current.point.design;
        let mut directional = 0.0_f64;
        let mut first = true;
        for (k, d) in design.iter() {
            let (Some(g), Some(t)) = (self.current.gradients.get(k), trial.get(k)) else {
                return contract(format!("trial design omits coordinate {k}"));
            };
            let prod = g * &(t - d);
            let s = crate::numeric::array_sum(&prod);
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
        log.consecutive_numerical = 0;
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
                Some(m) if d <= m => m,
                _ => d,
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
        hooks: &mut dyn StageSearchHooks,
        direction: &NamedArrays,
        log: &mut Log,
    ) -> CaeResult<(f64, Option<(NamedArrays, Option<CandidateAdmission>, Accepted)>)> {
        if self.settings.step_policy == "fixed_step" {
            let step = f(self.settings.step_fraction).min(f(self.settings.move_limit));
            let trial = self.trial_from(hooks, &self.current.point.design, direction, step)?;
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
                if let Some(result) = self.try_candidate(hooks, candidate_step, &trial, Some(meta), log)? {
                    return Ok((candidate_step, Some((trial, admission, result))));
                }
                if log.consecutive_numerical >= MAX_CONSECUTIVE_NUMERICAL_REJECTIONS {
                    return Ok((candidate_step, None));
                }
            }
            step = smallest * backtracking;
        }
        let minimum = f(self.settings.minimum_step_fraction);
        while step >= minimum {
            let trial = self.trial_from(hooks, &self.current.point.design, direction, step)?;
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
            if log.consecutive_numerical >= MAX_CONSECUTIVE_NUMERICAL_REJECTIONS {
                return Ok((step, None));
            }
            step *= backtracking;
        }
        Ok((step, None))
    }


    #[allow(clippy::too_many_lines)]
    pub fn iterate(&mut self, hooks: &mut dyn StageSearchHooks, iteration: i64) -> CaeResult<Value> {
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
        let pg = loop {
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
                break pg;
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
        let direction = self.direction(&pg)?;
        let (mut step, mut found) = self.line_search(hooks, &direction, &mut log)?;
        if self.settings.step_policy == "backtracking" && found.is_none() && !log.rejections.is_empty() {

            if let Some((point, record)) = hooks.recover(self)? {
                self.current = self.combine(point)?;
                self.trust_step = f64::min(f(self.settings.step_fraction), f(self.settings.move_limit));
                let (pg, _) = self.projected_gradient();
                let direction = self.direction(&pg)?;
                log.consecutive_numerical = 0;
                let rejected_before = log.rejections.len();
                (step, found) = self.line_search(hooks, &direction, &mut log)?;
                let mut record = match record {
                    Value::Object(m) => m,
                    other => {
                        let mut m = Map::new();
                        m.insert("provider".into(), other);
                        m
                    }
                };
                record.insert("rejections_before_recovery".into(), Value::from(rejected_before));
                record.insert("accepted_after_recovery".into(), Value::Bool(found.is_some()));
                log.recovery = Some(Value::Object(record));
            }
        }
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
        if let Some(recovery) = &log.recovery {
            row.insert("numerical_recovery".into(), recovery.clone());
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
            return contract("search recorded convergence that its final state does not support");
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

