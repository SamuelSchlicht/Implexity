// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::contracts::{
    CaeProvider, CoordinateOptimizationSettings, LegacySingleArrayOptimizationSettings, ProviderCapabilities,
    ProviderProblem, ResponseSpec, TOPOLOGY_COORDINATE,
};
use implexity_core::numeric_contract::real_scalar_f64;
use implexity_core::py_repr::{repr_float, repr_str};
use implexity_core::{CaeError, CaeResult};
use ndarray::{ArrayD, Zip};
use serde_json::{Map, Value};

pub use crate::bounds::{
    BOUND_SENSES, BOUND_STATE_SCHEMA, BoundMeasures, BoundMultiplierState, NormalizationPolicy,
    RESPONSE_NORMALIZATION_RESULT_SCHEMA, RESPONSE_NORMALIZATION_SCHEMA, augmented_objective, bound_measures,
    bound_program_identity, normalise_response_normalization, resolve_initial_response_normalization,
    restore_response_normalization, update_bound_multipliers,
};
pub use crate::search::{
    BoxCoordinate, Candidate, ExactPoint, MAX_CONSECUTIVE_STALLS, ProjectedSearch, SEARCH_METHOD,
    SEARCH_OUTCOME_SCHEMA, SEARCH_STATE_SCHEMA, SearchError, SearchHooks, SearchPoint, SearchResult,
    SearchState, TERMINAL_REASONS, TrialFailure, TrialInfo, TrialValues, box_coordinates,
};

use crate::bounds::lookup;
use crate::candidate_events::{CandidateAdmission, provider_candidate_admission};
use crate::design::{DesignLayout, NamedArrays, design_identity, require_finite};
use crate::design_state::{BoundInput, DesignState, NormaliseOptions, normalise_design};
use crate::numeric::{array_mean, array_to_value, float_value, shape_repr, str_list_repr};
use crate::provider_ops::{
    CandidateDesign, DesignOp, DesignSensitivities, LifecycleDeclaration, check_operating_point,
    design_operations, legacy_evaluate, legacy_sensitivity, provides,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizerLifecycleConfig {
    pub design_coordinates: Vec<String>,
    pub sensitivity_operation: String,
    pub evaluation_operation: String,
    pub candidate_admission_operation: Option<String>,
    pub acceptance_operation: Option<String>,
    pub require_identity_evidence: bool,
    pub compatibility_mode: bool,
}

impl OptimizerLifecycleConfig {

    pub fn new(
        design_coordinates: Vec<String>,
        sensitivity_operation: &str,
        evaluation_operation: &str,
        candidate_admission_operation: Option<&str>,
        acceptance_operation: Option<&str>,
        require_identity_evidence: bool,
        compatibility_mode: bool,
    ) -> CaeResult<Self> {
        if design_coordinates.is_empty() || design_coordinates.iter().any(String::is_empty) {
            return Err(CaeError::contract("optimizer lifecycle requires nonempty design-coordinate names"));
        }
        let mut unique = design_coordinates.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != design_coordinates.len() {
            return Err(CaeError::contract("optimizer lifecycle design coordinates must be unique"));
        }
        for (label, value) in
            [("sensitivity_operation", sensitivity_operation), ("evaluation_operation", evaluation_operation)]
        {
            if value.is_empty() {
                return Err(CaeError::contract(format!("optimizer lifecycle {label} must be nonempty text")));
            }
        }
        for (label, value) in [
            ("candidate_admission_operation", candidate_admission_operation),
            ("acceptance_operation", acceptance_operation),
        ] {
            if value == Some("") {
                return Err(CaeError::contract(format!(
                    "optimizer lifecycle {label} must be nonempty text or null"
                )));
            }
        }
        Ok(Self {
            design_coordinates,
            sensitivity_operation: sensitivity_operation.into(),
            evaluation_operation: evaluation_operation.into(),
            candidate_admission_operation: candidate_admission_operation.map(str::to_string),
            acceptance_operation: acceptance_operation.map(str::to_string),
            require_identity_evidence,
            compatibility_mode,
        })
    }


    pub fn from_mapping(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract(
                "optimizer lifecycle capabilities must be a mapping or typed config",
            ));
        };
        let required = [
            "design_coordinates",
            "sensitivity_operation",
            "evaluation_operation",
            "candidate_admission_operation",
            "acceptance_operation",
            "require_identity_evidence",
        ];
        let mut missing: Vec<&str> = required.iter().copied().filter(|k| !map.contains_key(*k)).collect();
        missing.sort_unstable();
        let mut extra: Vec<&String> = map
            .keys()
            .filter(|k| !required.contains(&k.as_str()) && k.as_str() != "compatibility_mode")
            .collect();
        extra.sort();
        if !missing.is_empty() || !extra.is_empty() {
            return Err(CaeError::contract(format!(
                "optimizer lifecycle capability mismatch; missing={}, extra={}",
                str_list_repr(&missing),
                str_list_repr(&extra)
            )));
        }
        let coordinates = match &map["design_coordinates"] {
            Value::Array(items) => {
                items.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<Vec<_>>>().ok_or_else(
                    || CaeError::contract("optimizer lifecycle requires nonempty design-coordinate names"),
                )?
            }
            _ => return Err(CaeError::contract("optimizer lifecycle design_coordinates must be a sequence")),
        };
        let text = |key: &str| -> CaeResult<String> {
            match &map[key] {
                Value::String(s) if !s.is_empty() => Ok(s.clone()),
                _ => Err(CaeError::contract(format!("optimizer lifecycle {key} must be nonempty text"))),
            }
        };
        let optional = |key: &str| -> CaeResult<Option<String>> {
            match &map[key] {
                Value::Null => Ok(None),
                Value::String(s) if !s.is_empty() => Ok(Some(s.clone())),
                _ => Err(CaeError::contract(format!(
                    "optimizer lifecycle {key} must be nonempty text or null"
                ))),
            }
        };
        let flag = |key: &str, default: Option<bool>| -> CaeResult<bool> {
            match (map.get(key), default) {
                (None, Some(d)) => Ok(d),
                (Some(Value::Bool(b)), _) => Ok(*b),
                _ => Err(CaeError::contract(format!("optimizer lifecycle {key} must be a boolean"))),
            }
        };
        let sensitivity = text("sensitivity_operation")?;
        let evaluation = text("evaluation_operation")?;
        let candidate = optional("candidate_admission_operation")?;
        let acceptance = optional("acceptance_operation")?;
        let require = flag("require_identity_evidence", None)?;
        let compat = flag("compatibility_mode", Some(false))?;
        Self::new(
            coordinates,
            &sensitivity,
            &evaluation,
            candidate.as_deref(),
            acceptance.as_deref(),
            require,
            compat,
        )
    }


    pub fn compatibility(provider: &dyn CaeProvider, coordinate_names: Option<&[String]>) -> CaeResult<Self> {
        let caps = provider.capabilities()?;
        let coordinates = match &caps {
            ProviderCapabilities::Descriptor(d) => d.design_coordinates.clone(),
            ProviderCapabilities::Legacy(l) => l.base.design_coordinates.clone(),
            ProviderCapabilities::Mapping(m) => match m.get("design_coordinates") {
                None | Some(Value::Null) => {
                    return Err(CaeError::contract("provider capabilities omitted design_coordinates"));
                }
                Some(Value::Array(items)) => items.iter().map(implexity_core::pyobj::py_str).collect(),
                Some(_) => {
                    return Err(CaeError::contract(
                        "provider design_coordinates capability must be a sequence",
                    ));
                }
            },
        };
        if let Some(names) = coordinate_names
            && names != coordinates.as_slice()
        {
            return Err(CaeError::contract(
                "explicit optimizer coordinate declaration disagrees with provider capabilities",
            ));
        }
        let sensitivities = match &caps {
            ProviderCapabilities::Descriptor(d) => Some(d.sensitivities),
            ProviderCapabilities::Legacy(l) => Some(l.base.sensitivities),
            ProviderCapabilities::Mapping(m) => m.get("sensitivities").and_then(Value::as_bool),
        };
        if sensitivities != Some(true) {
            return Err(CaeError::contract(
                "provider must explicitly declare sensitivities=true for optimization",
            ));
        }
        let has = |op| provides(provider, op);
        let sensitivity = if has(DesignOp::SensitivityDesign) { "sensitivity_design" } else { "sensitivity" };
        let evaluation = if has(DesignOp::EvaluateDesign) {
            "evaluate_design"
        } else if has(DesignOp::Evaluate) {
            "evaluate"
        } else if has(DesignOp::SensitivityDesign) {
            "sensitivity_design"
        } else {
            "sensitivity"
        };
        let candidate = if has(DesignOp::CandidateDesignAdmission) {
            Some("candidate_design_admission")
        } else if coordinates.len() == 1 && has(DesignOp::CandidateAdmission) {
            Some("candidate_admission")
        } else {
            None
        };
        let acceptance = has(DesignOp::AcceptDesign).then_some("accept_design");
        Self::new(coordinates, sensitivity, evaluation, candidate, acceptance, false, true)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            "design_coordinates".into(),
            Value::Array(self.design_coordinates.iter().cloned().map(Value::String).collect()),
        );
        m.insert("sensitivity_operation".into(), Value::String(self.sensitivity_operation.clone()));
        m.insert("evaluation_operation".into(), Value::String(self.evaluation_operation.clone()));
        m.insert(
            "candidate_admission_operation".into(),
            self.candidate_admission_operation.clone().map_or(Value::Null, Value::String),
        );
        m.insert(
            "acceptance_operation".into(),
            self.acceptance_operation.clone().map_or(Value::Null, Value::String),
        );
        m.insert("require_identity_evidence".into(), Value::Bool(self.require_identity_evidence));
        m.insert("compatibility_mode".into(), Value::Bool(self.compatibility_mode));
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleInput {
    Typed(OptimizerLifecycleConfig),
    Mapping(Value),
}

fn op_available(provider: &dyn CaeProvider, name: &str) -> bool {
    DesignOp::from_name(name).is_some_and(|op| provides(provider, op))
}


pub fn lifecycle(
    provider: &dyn CaeProvider,
    explicit: Option<&LifecycleInput>,
    coordinate_names: Option<&[String]>,
    problem: Option<&ProviderProblem>,
) -> CaeResult<OptimizerLifecycleConfig> {
    let config = match explicit {
        Some(LifecycleInput::Typed(c)) => c.clone(),
        Some(LifecycleInput::Mapping(v)) => OptimizerLifecycleConfig::from_mapping(v)?,
        None => match design_operations(provider).filter(|o| o.provides(DesignOp::OptimizerLifecycle)) {
            Some(ops) => {
                let raw = if ops.lifecycle_is_problem_specific() {
                    ops.optimizer_lifecycle(problem)?
                } else {
                    ops.optimizer_lifecycle(None)?
                };
                match raw {
                    LifecycleDeclaration::Typed(c) => c,
                    LifecycleDeclaration::Mapping(v) => OptimizerLifecycleConfig::from_mapping(&v)?,
                }
            }
            None => OptimizerLifecycleConfig::compatibility(provider, coordinate_names)?,
        },
    };
    if let Some(names) = coordinate_names
        && names != config.design_coordinates.as_slice()
    {
        return Err(CaeError::contract(
            "optimizer coordinate declaration disagrees with lifecycle capabilities",
        ));
    }
    for op in [&config.sensitivity_operation, &config.evaluation_operation] {
        if !op_available(provider, op) {
            return Err(CaeError::contract(format!(
                "provider declares optimizer operation {}, but it is unavailable",
                repr_str(op)
            )));
        }
    }
    for op in [&config.candidate_admission_operation, &config.acceptance_operation].into_iter().flatten() {
        if !op_available(provider, op) {
            return Err(CaeError::contract(format!(
                "provider declares optimizer operation {}, but it is unavailable",
                repr_str(op)
            )));
        }
    }
    Ok(config)
}


pub fn commit_accepted_design(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    values: &NamedArrays,
    config: &OptimizerLifecycleConfig,
) -> CaeResult<Option<Value>> {
    if config.acceptance_operation.is_none() {
        return Ok(None);
    }
    let owned = values.owned_design()?;
    let identity = design_identity(&owned)?;
    let ops =
        design_operations(provider).filter(|o| o.provides(DesignOp::AcceptDesign)).ok_or_else(|| {
            CaeError::contract("provider declares optimizer operation 'accept_design', but it is unavailable")
        })?;
    let result = ops.accept_design(problem, &owned)?;
    let Some(result) = result else {
        if config.require_identity_evidence {
            return Err(CaeError::contract(
                "accepted-design commit omitted required design_state_id acknowledgement",
            ));
        }
        let mut m = Map::new();
        m.insert("design_state_id".into(), Value::String(identity));
        m.insert("compatibility_acknowledgement".into(), Value::Bool(true));
        return Ok(Some(Value::Object(m)));
    };
    if result.contains_key("design_state_id") && result.contains_key("designStateId") {
        return Err(CaeError::contract(
            "accepted-design commit contains ambiguous design-state identity aliases",
        ));
    }
    let returned = result.get("design_state_id").or_else(|| result.get("designStateId"));
    if returned.and_then(Value::as_str) != Some(identity.as_str()) {
        return Err(CaeError::contract(
            "accepted-design commit returned a stale or mismatched design_state_id",
        ));
    }
    let mut out = Map::new();
    out.insert("design_state_id".into(), Value::String(identity));
    for (k, v) in result {
        out.insert(k, v);
    }
    Ok(Some(Value::Object(out)))
}


pub fn trial_value(value: f64, label: &str) -> SearchResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(SearchError::Trial(TrialFailure::NonFinite(format!(
            "{label} is not finite ({})",
            repr_float(value)
        ))))
    }
}

fn exact_or_trial(value: f64, label: &str, trial: bool) -> SearchResult<f64> {
    if trial { trial_value(value, label) } else { Ok(real_scalar_f64(value, label)?) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseDatum {
    pub value: f64,
    pub gradient: ArrayD<f64>,
    pub diagnostics: Map<String, Value>,
}


pub fn validate_request(provider: &dyn CaeProvider, responses: &[ResponseSpec]) -> CaeResult<()> {
    if responses.is_empty() {
        return Err(CaeError::contract("optimization requires a nonempty list of ResponseSpec values"));
    }
    let caps = provider.capabilities()?;
    let declared: Vec<String> = match &caps {
        ProviderCapabilities::Descriptor(d) => d.responses.clone(),
        ProviderCapabilities::Legacy(l) => l.base.responses.clone(),
        ProviderCapabilities::Mapping(m) => match m.get("responses") {
            None | Some(Value::Null) => {
                return Err(CaeError::contract("provider capabilities omitted responses"));
            }
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    CaeError::contract("provider responses capability must be a sequence of nonempty ids")
                })?,
            Some(_) => {
                return Err(CaeError::contract(
                    "provider responses capability must be a sequence of nonempty ids",
                ));
            }
        },
    };
    if declared.iter().any(String::is_empty) {
        return Err(CaeError::contract("provider responses capability must be a sequence of nonempty ids"));
    }
    let mut unique = declared.clone();
    unique.sort();
    unique.dedup();
    if unique.len() != declared.len() {
        return Err(CaeError::contract("provider responses capability contains duplicate ids"));
    }
    let missing: Vec<&String> = responses.iter().map(|s| &s.name).filter(|n| !declared.contains(n)).collect();
    if !missing.is_empty() {
        return Err(CaeError::contract(format!(
            "optimization requested undeclared provider responses {}",
            str_list_repr(&missing)
        )));
    }
    Ok(())
}


pub fn array_response_data(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    topology: &ArrayD<f64>,
    names: &[String],
    operating_point: usize,
    trial: bool,
) -> SearchResult<BTreeMap<String, ResponseDatum>> {
    let many = design_operations(provider).filter(|o| o.provides(DesignOp::SensitivityMany));
    let sensitivities = match many {
        Some(ops) if names.len() > 1 => {
            check_operating_point(provider, DesignOp::SensitivityMany, operating_point)?;
            ops.sensitivity_many(problem, topology, names, operating_point)?
        }
        _ => {
            if !provides(provider, DesignOp::Sensitivity) {
                return Err(
                    CaeError::contract("provider has no declared legacy sensitivity operation").into()
                );
            }
            let mut out = BTreeMap::new();
            for name in names {
                out.insert(
                    name.clone(),
                    legacy_sensitivity(provider, problem, topology, name, operating_point)?,
                );
            }
            out
        }
    };
    if sensitivities.len() != names.len() || !names.iter().all(|n| sensitivities.contains_key(n)) {
        return Err(
            CaeError::contract("provider sensitivities do not exactly match requested responses").into()
        );
    }
    let mut data = BTreeMap::new();
    for name in names {
        let s = &sensitivities[name];
        let label = format!("provider response {}", repr_str(name));
        let value = exact_or_trial(s.value, &label, trial)?;
        if trial && s.gradient.iter().any(|v| !v.is_finite()) {
            return Err(SearchError::Trial(TrialFailure::NonFinite(format!(
                "provider gradient for {} is not finite",
                repr_str(name)
            ))));
        }
        require_finite(&s.gradient, &format!("provider gradient for {}", repr_str(name)))?;
        if s.gradient.shape() != topology.shape() {
            return Err(CaeError::contract(format!(
                "provider returned gradient shape {} for {}, expected {}",
                shape_repr(s.gradient.shape()),
                repr_str(name),
                shape_repr(topology.shape())
            ))
            .into());
        }
        data.insert(
            name.clone(),
            ResponseDatum { value, gradient: s.gradient.clone(), diagnostics: s.diagnostics.clone() },
        );
    }
    Ok(data)
}


pub fn design_sensitivities(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    names: &[String],
    trial: bool,
    operating_point: usize,
) -> SearchResult<DesignSensitivities> {
    let names = crate::native_design::unique_names(names);
    let ops = design_operations(provider);
    let batched = ops.filter(|o| o.provides(DesignOp::SensitivitiesDesign));
    let single = ops.filter(|o| o.provides(DesignOp::SensitivityDesign));
    if batched.is_none() && single.is_none() {
        if design.len() != 1 {
            return Err(CaeError::contract(format!(
                "provider {} has no sensitivity_design() for a multi-coordinate design",
                repr_str(provider.name())
            ))
            .into());
        }
        let Some((coordinate, array)) = design.first() else {
            return Err(CaeError::contract("named design must be a nonempty mapping").into());
        };
        let data = array_response_data(provider, problem, array, &names, operating_point, trial)?;
        let mut out = DesignSensitivities::default();
        let mut per = Map::new();
        for (name, row) in data {
            out.responses.insert(name.clone(), row.value);
            out.gradients.insert(name.clone(), NamedArrays::single(coordinate, row.gradient));
            per.insert(name, Value::Object(row.diagnostics));
        }
        out.diagnostics.insert("responses".into(), Value::Object(per));
        out.diagnostics.insert("shared_state".into(), Value::Bool(false));
        return Ok(out);
    }
    let layout = DesignLayout::from_values(design)?;
    let raw = if let Some(ops) = batched {
        check_operating_point(provider, DesignOp::SensitivitiesDesign, operating_point)?;
        ops.sensitivities_design(problem, design, &names, operating_point)?
    } else {
        let ops =
            single.ok_or_else(|| CaeError::contract("provider has no named design sensitivity contract"))?;
        check_operating_point(provider, DesignOp::SensitivityDesign, operating_point)?;
        let mut out = DesignSensitivities::default();
        let mut per = Map::new();
        for n in &names {
            let row = ops.sensitivity_design(problem, design, n, operating_point)?;
            out.responses.insert(n.clone(), row.value);
            out.gradients.insert(n.clone(), row.gradients);
            per.insert(n.clone(), Value::Object(row.diagnostics));
        }
        out.diagnostics.insert("responses".into(), Value::Object(per));
        out.diagnostics.insert("shared_state".into(), Value::Bool(false));
        out
    };
    let same = |keys: Vec<&String>| keys.len() == names.len() && names.iter().all(|n| keys.contains(&n));
    if !same(raw.responses.keys().collect()) {
        return Err(CaeError::contract("batched responses do not match the requested response names").into());
    }
    if !same(raw.gradients.keys().collect()) {
        return Err(CaeError::contract("batched gradients do not match the requested response names").into());
    }
    let mut out =
        DesignSensitivities { diagnostics: raw.diagnostics.clone(), ..DesignSensitivities::default() };
    for name in &names {
        let value =
            exact_or_trial(raw.responses[name], &format!("provider response {}", repr_str(name)), trial)?;
        out.responses.insert(name.clone(), value);
        let rows = &raw.gradients[name];
        if trial && rows.iter().any(|(_, a)| a.iter().any(|v| !v.is_finite())) {
            return Err(SearchError::Trial(TrialFailure::NonFinite(format!(
                "provider gradient for {} is not finite",
                repr_str(name)
            ))));
        }
        out.gradients.insert(name.clone(), layout.repack(rows, &format!("provider derivative {name}"))?);
    }
    Ok(out)
}


pub fn evaluation_response_values(
    evaluation: &implexity_core::contracts::Evaluation,
    names: &[String],
    label: &str,
) -> SearchResult<(BTreeMap<String, f64>, Map<String, Value>)> {
    let mut out = BTreeMap::new();
    for name in names {
        let Some(v) = evaluation.responses.get(name) else {
            return Err(CaeError::contract(format!("{label} omitted response {}", repr_str(name))).into());
        };
        out.insert(name.clone(), trial_value(*v, &format!("{label} response {}", repr_str(name)))?);
    }
    Ok((out, evaluation.diagnostics.clone()))
}

pub struct DesignHooks<'a> {
    provider: &'a dyn CaeProvider,
    problem: &'a ProviderProblem,
    config: OptimizerLifecycleConfig,
    names: Vec<String>,
    values_only: bool,
}

impl<'a> DesignHooks<'a> {

    pub fn new(
        provider: &'a dyn CaeProvider,
        problem: &'a ProviderProblem,
        responses: &[ResponseSpec],
        config: OptimizerLifecycleConfig,
    ) -> CaeResult<Self> {
        if !["sensitivity_design", "sensitivity"].contains(&config.sensitivity_operation.as_str()) {
            return Err(CaeError::contract(format!(
                "unsupported optimizer sensitivity operation {}",
                repr_str(&config.sensitivity_operation)
            )));
        }
        if !["evaluate_design", "evaluate", "sensitivity_design", "sensitivity"]
            .contains(&config.evaluation_operation.as_str())
        {
            return Err(CaeError::contract(format!(
                "unsupported optimizer evaluation operation {}",
                repr_str(&config.evaluation_operation)
            )));
        }
        let values_only = ["evaluate_design", "evaluate"].contains(&config.evaluation_operation.as_str());
        Ok(Self { provider, problem, config, names: crate::bounds::response_names(responses), values_only })
    }


    pub fn exact(&self, design: &NamedArrays, trial: bool) -> SearchResult<ExactPoint> {
        let owned = design.owned_design()?;
        let raw = design_sensitivities(self.provider, self.problem, &owned, &self.names, trial, 0)?;
        let mut values = BTreeMap::new();
        for name in &self.names {
            values.insert(
                name.clone(),
                exact_or_trial(raw.responses[name], &format!("provider response {}", repr_str(name)), trial)?,
            );
        }
        let mut gradients = BTreeMap::new();
        for name in &self.names {
            let rows = &raw.gradients[name];
            let missing: Vec<String> = owned.names().into_iter().filter(|c| !rows.contains(c)).collect();
            let extra: Vec<String> = rows.names().into_iter().filter(|c| !owned.contains(c)).collect();
            if !missing.is_empty() || !extra.is_empty() {
                return Err(CaeError::contract(format!(
                    "provider sensitivity for {} has coordinate mismatch; missing={}, extra={}",
                    repr_str(name),
                    str_list_repr(&missing),
                    str_list_repr(&extra)
                ))
                .into());
            }
            let mut checked = NamedArrays::new();
            for (coordinate, reference) in owned.iter() {
                let array = rows.get(coordinate).cloned().unwrap_or_default();
                if trial && array.shape() == reference.shape() && array.iter().any(|v| !v.is_finite()) {
                    return Err(SearchError::Trial(TrialFailure::NonFinite(format!(
                        "provider gradient {}/{} is not finite",
                        repr_str(name),
                        repr_str(coordinate)
                    ))));
                }
                require_finite(
                    &array,
                    &format!("provider gradient {}/{}", repr_str(name), repr_str(coordinate)),
                )?;
                if array.shape() != reference.shape() {
                    return Err(CaeError::contract(format!(
                        "provider returned invalid response/gradient for {}/{coordinate}",
                        repr_str(name)
                    ))
                    .into());
                }
                checked.insert(coordinate, array);
            }
            gradients.insert(name.clone(), checked);
        }
        Ok(ExactPoint { design: owned, values, gradients, diagnostics: raw.diagnostics })
    }
}

impl SearchHooks for DesignHooks<'_> {
    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues> {
        if !self.values_only {
            let point = self.exact(design, true)?;
            return Ok((point.values.clone(), point.diagnostics.clone(), Some(point)));
        }
        let owned = design.owned_design()?;
        let evaluation = if self.config.evaluation_operation == "evaluate_design" {
            let ops = design_operations(self.provider)
                .filter(|o| o.provides(DesignOp::EvaluateDesign))
                .ok_or_else(|| {
                    CaeError::contract(
                        "provider declares optimizer operation 'evaluate_design', but it is unavailable",
                    )
                })?;
            ops.evaluate_design(self.problem, &owned, 0)?
        } else {
            if owned.len() != 1 {
                return Err(CaeError::contract(
                    "single-array evaluation requires exactly one design coordinate",
                )
                .into());
            }
            let array = owned.first().map(|(_, a)| a.clone()).unwrap_or_default();
            legacy_evaluate(self.provider, self.problem, &array, 0)?
        };
        let (values, diagnostics) =
            evaluation_response_values(&evaluation, &self.names, "provider evaluation")?;
        Ok((values, diagnostics, None))
    }

    fn sensitivities(&mut self, design: &NamedArrays) -> SearchResult<ExactPoint> {
        self.exact(design, true)
    }

    fn admit(&mut self, current: &NamedArrays, trial: &NamedArrays) -> CaeResult<Option<CandidateAdmission>> {
        design_candidate_admission_values(self.provider, self.problem, current, trial, &self.config)
    }

    fn accept(
        &mut self,
        _previous: &NamedArrays,
        design: &NamedArrays,
        _admission: Option<&CandidateAdmission>,
        point: ExactPoint,
    ) -> CaeResult<(Option<Value>, ExactPoint)> {
        Ok((commit_accepted_design(self.provider, self.problem, design, &self.config)?, point))
    }
}


pub fn design_candidate_admission_values(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    current: &NamedArrays,
    candidate: &NamedArrays,
    lifecycle: &OptimizerLifecycleConfig,
) -> CaeResult<Option<CandidateAdmission>> {
    let Some(operation) = &lifecycle.candidate_admission_operation else { return Ok(None) };
    let current = current.owned_design()?;
    let candidate = candidate.owned_design()?;
    let op = DesignOp::from_name(operation).ok_or_else(|| {
        CaeError::contract(format!("provider declares required {operation} operation but it is unavailable"))
    })?;
    let (cur, cand) =
        if lifecycle.compatibility_mode && op == DesignOp::CandidateAdmission && current.len() == 1 {
            let name = current.names().remove(0);
            (
                CandidateDesign::Array(current.get(&name).cloned().unwrap_or_default()),
                CandidateDesign::Array(candidate.get(&name).cloned().unwrap_or_default()),
            )
        } else {
            (CandidateDesign::Named(current), CandidateDesign::Named(candidate))
        };
    provider_candidate_admission(
        provider,
        problem,
        &cur,
        &cand,
        op,
        true,
        lifecycle.require_identity_evidence,
    )
    .map(Some)
}

pub struct ArrayHooks<'a> {
    inner: DesignHooks<'a>,
    coordinate: String,
}

impl<'a> ArrayHooks<'a> {

    pub fn new(
        provider: &'a dyn CaeProvider,
        problem: &'a ProviderProblem,
        responses: &[ResponseSpec],
        config: OptimizerLifecycleConfig,
    ) -> CaeResult<Self> {
        let coordinate = config.design_coordinates[0].clone();
        Ok(Self { inner: DesignHooks::new(provider, problem, responses, config)?, coordinate })
    }

    fn array(&self, design: &NamedArrays) -> CaeResult<ArrayD<f64>> {
        let a = design
            .get(&self.coordinate)
            .cloned()
            .ok_or_else(|| CaeError::contract(format!("design omits {}", repr_str(&self.coordinate))))?;
        require_finite(&a, "model:control")?;
        Ok(a)
    }


    pub fn exact(&self, design: &NamedArrays, trial: bool) -> SearchResult<ExactPoint> {
        let array = self.array(design)?;
        let data = array_response_data(
            self.inner.provider,
            self.inner.problem,
            &array,
            &self.inner.names,
            0,
            trial,
        )?;
        let mut values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        let mut diagnostics = Map::new();
        for (name, row) in data {
            values.insert(name.clone(), row.value);
            gradients.insert(name.clone(), NamedArrays::single(&self.coordinate, row.gradient));
            diagnostics.insert(name, Value::Object(row.diagnostics));
        }
        Ok(ExactPoint {
            design: NamedArrays::single(&self.coordinate, array),
            values,
            gradients,
            diagnostics,
        })
    }
}

impl SearchHooks for ArrayHooks<'_> {
    fn values(&mut self, design: &NamedArrays) -> SearchResult<TrialValues> {
        if !self.inner.values_only {
            let point = self.exact(design, true)?;
            return Ok((point.values.clone(), point.diagnostics.clone(), Some(point)));
        }
        let array = self.array(design)?;
        let evaluation = if self.inner.config.evaluation_operation == "evaluate_design" {
            let ops = design_operations(self.inner.provider)
                .filter(|o| o.provides(DesignOp::EvaluateDesign))
                .ok_or_else(|| {
                    CaeError::contract(
                        "provider declares optimizer operation 'evaluate_design', but it is unavailable",
                    )
                })?;
            ops.evaluate_design(self.inner.problem, &NamedArrays::single(&self.coordinate, array), 0)?
        } else {
            legacy_evaluate(self.inner.provider, self.inner.problem, &array, 0)?
        };
        let (values, diagnostics) =
            evaluation_response_values(&evaluation, &self.inner.names, "provider evaluation")?;
        Ok((values, diagnostics, None))
    }

    fn sensitivities(&mut self, design: &NamedArrays) -> SearchResult<ExactPoint> {
        self.exact(design, true)
    }

    fn admit(&mut self, current: &NamedArrays, trial: &NamedArrays) -> CaeResult<Option<CandidateAdmission>> {
        let config = &self.inner.config;
        let op_name =
            config.candidate_admission_operation.clone().unwrap_or_else(|| "candidate_admission".into());
        let op = DesignOp::from_name(&op_name).unwrap_or(DesignOp::CandidateAdmission);
        let declared = config.candidate_admission_operation.is_some();
        provider_candidate_admission(
            self.inner.provider,
            self.inner.problem,
            &CandidateDesign::Array(self.array(current)?),
            &CandidateDesign::Array(self.array(trial)?),
            op,
            declared,
            config.require_identity_evidence && declared,
        )
        .map(Some)
    }

    fn accept(
        &mut self,
        previous: &NamedArrays,
        design: &NamedArrays,
        admission: Option<&CandidateAdmission>,
        point: ExactPoint,
    ) -> CaeResult<(Option<Value>, ExactPoint)> {
        let mut point = point;
        if let Some(a) = admission.filter(|a| a.relinearize_after_accept)
            && let Some(ops) = design_operations(self.inner.provider)
                .filter(|o| o.provides(DesignOp::OnTopologyEventAccepted))
        {
            ops.on_topology_event_accepted(
                self.inner.problem,
                &self.array(previous)?,
                &self.array(design)?,
                &a.to_value(),
            )?;
            point = self.exact(design, false).map_err(CaeError::from)?;
        }
        let commit = commit_accepted_design(
            self.inner.provider,
            self.inner.problem,
            &NamedArrays::single(&self.coordinate, self.array(design)?),
            &self.inner.config,
        )?;
        Ok((commit, point))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveEvaluation {
    pub total: f64,
    pub gradient: ArrayD<f64>,
    pub terms: Vec<Value>,
    pub diagnostics: Map<String, Value>,
    pub resolved_responses: Vec<ResponseSpec>,
    pub response_normalization: Option<Value>,
    pub response_data: BTreeMap<String, ResponseDatum>,
    pub base_objective: f64,
}


pub fn objective(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    topology: &ArrayD<f64>,
    responses: &[ResponseSpec],
    response_normalization: Option<&Value>,
    operating_point: usize,
    bound_state: Option<&BoundMultiplierState>,
) -> CaeResult<ObjectiveEvaluation> {
    validate_request(provider, responses)?;
    require_finite(topology, "optimization design")?;
    if topology.is_empty() {
        return Err(CaeError::contract("optimization design must be finite and nonempty"));
    }
    let names = crate::bounds::response_names(responses);
    let data = array_response_data(provider, problem, topology, &names, operating_point, false)?;
    let values: BTreeMap<String, f64> = data.iter().map(|(k, r)| (k.clone(), r.value)).collect();
    let (resolved, normalization) =
        resolve_initial_response_normalization(responses, &values, response_normalization)?;
    let state = bound_state.cloned().unwrap_or_else(|| BoundMultiplierState::initial(&resolved));
    let a = augmented_objective(&resolved, &lookup(&values), &state, None)?;
    let mut grad = ArrayD::zeros(topology.raw_dim());
    let mut diagnostics = Map::new();
    for (spec, coefficient) in resolved.iter().zip(&a.coefficients) {
        Zip::from(&mut grad)
            .and(&data[&spec.name].gradient)
            .for_each(|g: &mut f64, r: &f64| *g += coefficient * r);
        diagnostics.insert(spec.name.clone(), Value::Object(data[&spec.name].diagnostics.clone()));
    }
    if grad.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("nonfinite aggregate response derivative"));
    }
    Ok(ObjectiveEvaluation {
        total: a.total,
        gradient: grad,
        terms: a.terms,
        diagnostics,
        resolved_responses: resolved,
        response_normalization: normalization,
        response_data: data,
        base_objective: a.base,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesignObjectiveEvaluation {
    pub total: f64,
    pub gradients: NamedArrays,
    pub terms: Vec<Value>,
    pub diagnostics: Map<String, Value>,
    pub base_objective: f64,
}


pub fn objective_design(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    state: &DesignState,
    responses: &[ResponseSpec],
    config: Option<&OptimizerLifecycleConfig>,
    bound_state: Option<&BoundMultiplierState>,
) -> CaeResult<DesignObjectiveEvaluation> {
    validate_request(provider, responses)?;
    let names = state.names();
    let config = match config {
        Some(c) => c.clone(),
        None => lifecycle(provider, None, Some(&names), Some(problem))?,
    };
    if config.design_coordinates != names {
        return Err(CaeError::contract("design state disagrees with optimizer lifecycle coordinates"));
    }
    let hooks = DesignHooks::new(provider, problem, responses, config)?;
    let point = hooks.exact(&state.values(), false)?;
    let bound_state = bound_state.cloned().unwrap_or_else(|| BoundMultiplierState::initial(responses));
    let a = augmented_objective(responses, &lookup(&point.values), &bound_state, None)?;
    let mut grads = NamedArrays::new();
    for c in &state.coordinates {
        grads.insert(c.name.clone(), ArrayD::zeros(c.value.raw_dim()));
    }
    for (spec, coefficient) in responses.iter().zip(&a.coefficients) {
        let rows = &point.gradients[&spec.name];
        for (name, g) in grads.iter_mut() {
            if let Some(r) = rows.get(name) {
                Zip::from(g).and(r).for_each(|g, r| *g += coefficient * r);
            }
        }
    }
    if grads.iter().any(|(_, g)| g.iter().any(|v| !v.is_finite())) {
        return Err(CaeError::contract("nonfinite aggregate response derivative"));
    }
    Ok(DesignObjectiveEvaluation {
        total: a.total,
        gradients: grads,
        terms: a.terms,
        diagnostics: point.diagnostics,
        base_objective: a.base,
    })
}


pub fn final_response_bound_assessment(responses: &[ResponseSpec], terms: &[Value]) -> CaeResult<Value> {
    let report = crate::constraint_admission::response_bound_report(
        responses,
        Some(&Value::Array(terms.to_vec())),
        Some(&Value::Array(vec![Value::from(0)])),
    )?;
    let mut m = Map::new();
    m.insert("schema".into(), Value::String("implexity-standalone-response-bound-assessment/1".into()));
    m.insert("report".into(), report);
    m.insert("final_acceptance_performed".into(), Value::Bool(false));
    m.insert("model_authority_promoted".into(), Value::Bool(false));
    m.insert("optimization_convergence_inferred".into(), Value::Bool(false));
    m.insert(
        "scope".into(),
        Value::String(
            "returned_design_response_bounds_only; canonical acceptance requires a separate managed transaction".into(),
        ),
    );
    Ok(Value::Object(m))
}

pub type ProgressCallback<'a> = &'a mut dyn FnMut(&Value);

#[derive(Debug, Clone, PartialEq)]
pub struct DirectGradientRun {
    pub design: NamedArrays,
    pub record: Map<String, Value>,
}

#[must_use]
pub fn legacy_settings_value(settings: &LegacySingleArrayOptimizationSettings) -> Value {
    let coordinate = settings.settings.to_value();
    let mut out = Map::new();
    if let Value::Object(m) = coordinate {
        for (k, v) in m {
            let key = match k.as_str() {
                "coordinate_lower" => "topology_lower".to_string(),
                "coordinate_upper" => "topology_upper".to_string(),
                _ => k,
            };
            out.insert(key, v);
        }
    }
    Value::Object(out)
}


pub fn optimise(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    topology: &ArrayD<f64>,
    responses: &[ResponseSpec],
    settings: &LegacySingleArrayOptimizationSettings,
    mut callback: Option<ProgressCallback<'_>>,
) -> CaeResult<DirectGradientRun> {
    validate_request(provider, responses)?;
    let config = lifecycle(provider, None, None, Some(problem))?;
    if config.design_coordinates != [TOPOLOGY_COORDINATE.to_string()] {
        return Err(CaeError::contract(
            "legacy array optimization requires exactly one declared model:control coordinate",
        ));
    }
    require_finite(topology, "model:control")?;
    if topology.ndim() != 3 {
        return Err(CaeError::contract("model:control must be a finite three-dimensional topology field"));
    }
    let lo = settings.settings.coordinate_lower.as_f64();
    let hi = settings.settings.coordinate_upper.as_f64();
    if topology.iter().any(|x| *x < lo || *x > hi) {
        return Err(CaeError::contract("initial model:control lies outside declared topology bounds"));
    }
    let mut item = Map::new();
    item.insert("value".into(), array_to_value(topology));
    item.insert("lower".into(), settings.settings.coordinate_lower.to_value());
    item.insert("upper".into(), settings.settings.coordinate_upper.to_value());
    let mut raw = Map::new();
    raw.insert(TOPOLOGY_COORDINATE.into(), Value::Object(item));
    let state =
        normalise_design(&Value::Object(raw), &NormaliseOptions::named(&[TOPOLOGY_COORDINATE.to_string()]))?;
    let mut hooks = ArrayHooks::new(provider, problem, responses, config.clone())?;
    let x = NamedArrays::single(TOPOLOGY_COORDINATE, topology.clone());
    let initial = hooks.exact(&x, false)?;
    let initial_commit = commit_accepted_design(provider, problem, &x, &config)?;
    let mut search = ProjectedSearch::new(
        responses.to_vec(),
        settings.as_coordinate_settings(),
        box_coordinates(&state),
        initial,
        None,
    )?;
    let mut history: Vec<Value> = Vec::new();
    for i in 0..settings.settings.iterations {
        let mean = search.current.point.design.get(TOPOLOGY_COORDINATE).map_or(f64::NAN, array_mean);
        let mut row = search.iterate(&mut hooks, i)?;
        if let Value::Object(m) = &mut row {
            m.insert("control_mean".into(), float_value(mean));
        }
        if let Some(cb) = callback.as_mut() {
            cb(&row);
        }
        let terminal = row.get("terminal") == Some(&Value::Bool(true));
        history.push(row);
        if terminal {
            break;
        }
    }
    let final_point = &search.current;
    let design = final_point.point.design.clone();
    let mut record = Map::new();
    record.insert("schema".into(), Value::String("implexity-direct-gradient-run/1".into()));
    record.insert("topology_coordinate".into(), Value::String(TOPOLOGY_COORDINATE.into()));
    record.insert("topology".into(), design.get(TOPOLOGY_COORDINATE).map_or(Value::Null, array_to_value));
    record.insert("objective".into(), float_value(final_point.total));
    record.insert("base_objective".into(), float_value(final_point.base_objective));
    record.insert("terms".into(), Value::Array(final_point.terms.clone()));
    record.insert("diagnostics".into(), Value::Object(final_point.point.diagnostics.clone()));
    record.insert(
        "final_response_bound_assessment".into(),
        final_response_bound_assessment(responses, &final_point.terms)?,
    );
    record.insert("search_outcome".into(), search.outcome(&history)?);
    record.insert("search_state".into(), search.state_wire());
    record.insert("history".into(), Value::Array(history));
    record.insert("settings".into(), legacy_settings_value(settings));
    let mut lc = Map::new();
    lc.insert("compatibility_mode".into(), Value::Bool(config.compatibility_mode));
    lc.insert("initial_commit".into(), initial_commit.unwrap_or(Value::Null));
    record.insert("optimizer_lifecycle".into(), Value::Object(lc));
    Ok(DirectGradientRun { design, record })
}


#[allow(clippy::too_many_arguments)]
pub fn optimise_design(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &Value,
    responses: &[ResponseSpec],
    settings: &CoordinateOptimizationSettings,
    coordinate_names: Option<&[String]>,
    explicit_lifecycle: Option<&LifecycleInput>,
    mut callback: Option<ProgressCallback<'_>>,
) -> CaeResult<DirectGradientRun> {
    validate_request(provider, responses)?;
    let config = lifecycle(provider, explicit_lifecycle, coordinate_names, Some(problem))?;
    let names = config.design_coordinates.clone();
    let state = normalise_design(
        design,
        &NormaliseOptions {
            coordinate_names: Some(names.clone()),
            coordinate_lower: Some(BoundInput::Json(settings.coordinate_lower.to_value())),
            coordinate_upper: Some(BoundInput::Json(settings.coordinate_upper.to_value())),
            ..NormaliseOptions::default()
        },
    )?;
    let mut hooks = DesignHooks::new(provider, problem, responses, config.clone())?;
    let values = state.values();
    let initial = hooks.exact(&values, false)?;
    let initial_commit = commit_accepted_design(provider, problem, &values, &config)?;
    let mut search =
        ProjectedSearch::new(responses.to_vec(), settings.clone(), box_coordinates(&state), initial, None)?;
    let mut history: Vec<Value> = Vec::new();
    for i in 0..settings.iterations {
        let means: Map<String, Value> = search
            .current
            .point
            .design
            .iter()
            .map(|(k, v)| (k.to_string(), float_value(array_mean(v))))
            .collect();
        let mut row = search.iterate(&mut hooks, i)?;
        if let Value::Object(m) = &mut row {
            m.insert("coordinate_means".into(), Value::Object(means));
        }
        if let Some(cb) = callback.as_mut() {
            cb(&row);
        }
        let terminal = row.get("terminal") == Some(&Value::Bool(true));
        history.push(row);
        if terminal {
            break;
        }
    }
    let final_point = &search.current;
    let design_out = final_point.point.design.clone();
    let mut record = Map::new();
    record.insert("schema".into(), Value::String("implexity-direct-gradient-run/2".into()));
    record.insert(
        "design_coordinates".into(),
        Value::Array(names.iter().cloned().map(Value::String).collect()),
    );
    record.insert("design".into(), design_out.to_wire());
    record.insert("objective".into(), float_value(final_point.total));
    record.insert("base_objective".into(), float_value(final_point.base_objective));
    record.insert("terms".into(), Value::Array(final_point.terms.clone()));
    record.insert("diagnostics".into(), Value::Object(final_point.point.diagnostics.clone()));
    record.insert(
        "final_response_bound_assessment".into(),
        final_response_bound_assessment(responses, &final_point.terms)?,
    );
    record.insert("search_outcome".into(), search.outcome(&history)?);
    record.insert("search_state".into(), search.state_wire());
    record.insert("history".into(), Value::Array(history));
    record.insert("settings".into(), settings.to_value());
    let mut lc = Map::new();
    lc.insert("compatibility_mode".into(), Value::Bool(config.compatibility_mode));
    lc.insert("initial_commit".into(), initial_commit.unwrap_or(Value::Null));
    record.insert("optimizer_lifecycle".into(), Value::Object(lc));
    if config.compatibility_mode
        && let Some(t) = design_out.get(TOPOLOGY_COORDINATE)
    {
        record.insert("topology_coordinate".into(), Value::String(TOPOLOGY_COORDINATE.into()));
        record.insert("topology".into(), array_to_value(t));
    }
    Ok(DirectGradientRun { design: design_out, record })
}
