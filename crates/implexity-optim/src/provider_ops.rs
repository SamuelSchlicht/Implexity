// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;

use implexity_core::contracts::{
    CaeProvider, Evaluation, MatchingTimeNewtonGuess, ProviderProblem, Sensitivity,
};
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use crate::candidate_events::CandidateAdmission;
use crate::design::NamedArrays;

pub const DESIGN_OPERATIONS: &str = "implexity.cae.design_operations";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DesignOp {
    Evaluate,
    Sensitivity,
    EvaluateWithoutDesign,
    EvaluateDesign,
    EvaluateResultsDesign,
    PreflightDesign,
    SensitivityDesign,
    SensitivitiesDesign,
    SensitivityMany,
    AcceptDesign,
    CandidateDesignAdmission,
    CandidateAdmission,
    OnTopologyEventAccepted,
    ProjectTopology,
    OptimizerLifecycle,
    InstallMatchingTimeGuess,
    ExportMatchingTimeGuess,
    PhysicalCertification,
}

impl DesignOp {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Evaluate | Self::EvaluateWithoutDesign => "evaluate",
            Self::Sensitivity => "sensitivity",
            Self::EvaluateDesign => "evaluate_design",
            Self::EvaluateResultsDesign => "evaluate_results_design",
            Self::PreflightDesign => "preflight_design",
            Self::SensitivityDesign => "sensitivity_design",
            Self::SensitivitiesDesign => "sensitivities_design",
            Self::SensitivityMany => "sensitivity_many",
            Self::AcceptDesign => "accept_design",
            Self::CandidateDesignAdmission => "candidate_design_admission",
            Self::CandidateAdmission => "candidate_admission",
            Self::OnTopologyEventAccepted => "on_topology_event_accepted",
            Self::ProjectTopology => "project_topology",
            Self::OptimizerLifecycle => "optimizer_lifecycle_capabilities",
            Self::InstallMatchingTimeGuess => "install_matching_time_guess",
            Self::ExportMatchingTimeGuess => "export_matching_time_guess",
            Self::PhysicalCertification => "physical_certification",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "evaluate" => Self::Evaluate,
            "sensitivity" => Self::Sensitivity,
            "evaluate_design" => Self::EvaluateDesign,
            "evaluate_results_design" => Self::EvaluateResultsDesign,
            "preflight_design" => Self::PreflightDesign,
            "sensitivity_design" => Self::SensitivityDesign,
            "sensitivities_design" => Self::SensitivitiesDesign,
            "sensitivity_many" => Self::SensitivityMany,
            "accept_design" => Self::AcceptDesign,
            "candidate_design_admission" => Self::CandidateDesignAdmission,
            "candidate_admission" => Self::CandidateAdmission,
            "on_topology_event_accepted" => Self::OnTopologyEventAccepted,
            "project_topology" => Self::ProjectTopology,
            "optimizer_lifecycle_capabilities" => Self::OptimizerLifecycle,
            "install_matching_time_guess" => Self::InstallMatchingTimeGuess,
            "export_matching_time_guess" => Self::ExportMatchingTimeGuess,
            "physical_certification" => Self::PhysicalCertification,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesignSensitivity {
    pub value: f64,
    pub gradients: NamedArrays,
    pub diagnostics: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DesignSensitivities {
    pub responses: BTreeMap<String, f64>,
    pub gradients: BTreeMap<String, NamedArrays>,
    pub diagnostics: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CandidateDesign {
    Array(ArrayD<f64>),
    Named(NamedArrays),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdmissionReply {
    Unspecified,
    Allow(bool),
    Record(Value),
    Typed(CandidateAdmission),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleDeclaration {
    Typed(crate::optimizer::OptimizerLifecycleConfig),
    Mapping(Value),
}

fn unavailable(op: DesignOp) -> CaeError {
    CaeError::contract(format!("provider operation {:?} is unavailable", op.name()))
}


#[allow(unused_variables)]
pub trait DesignOperations: Send + Sync {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(op, DesignOp::Evaluate | DesignOp::Sensitivity)
    }

    fn dispatches_operating_points(&self, op: DesignOp) -> bool {
        false
    }


    fn evaluate_at(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        operating_point: usize,
    ) -> CaeResult<Evaluation> {
        Err(unavailable(DesignOp::Evaluate))
    }


    fn sensitivity_at(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
        operating_point: usize,
    ) -> CaeResult<Sensitivity> {
        Err(unavailable(DesignOp::Sensitivity))
    }


    fn evaluate_without_design(&self, problem: &ProviderProblem) -> CaeResult<Evaluation> {
        Err(unavailable(DesignOp::EvaluateWithoutDesign))
    }


    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        operating_point: usize,
    ) -> CaeResult<Evaluation> {
        Err(unavailable(DesignOp::EvaluateDesign))
    }


    fn evaluate_results_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Evaluation> {
        Err(unavailable(DesignOp::EvaluateResultsDesign))
    }


    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        Err(unavailable(DesignOp::PreflightDesign))
    }


    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        operating_point: usize,
    ) -> CaeResult<DesignSensitivity> {
        Err(unavailable(DesignOp::SensitivityDesign))
    }


    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        operating_point: usize,
    ) -> CaeResult<DesignSensitivities> {
        Err(unavailable(DesignOp::SensitivitiesDesign))
    }


    fn sensitivity_many(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        responses: &[String],
        operating_point: usize,
    ) -> CaeResult<BTreeMap<String, Sensitivity>> {
        Err(unavailable(DesignOp::SensitivityMany))
    }


    fn accept_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Option<Map<String, Value>>> {
        Err(unavailable(DesignOp::AcceptDesign))
    }


    fn candidate_admission(
        &self,
        op: DesignOp,
        problem: &ProviderProblem,
        current: &CandidateDesign,
        trial: &CandidateDesign,
    ) -> CaeResult<AdmissionReply> {
        Err(unavailable(op))
    }


    fn on_topology_event_accepted(
        &self,
        problem: &ProviderProblem,
        previous: &ArrayD<f64>,
        design: &ArrayD<f64>,
        admission: &Value,
    ) -> CaeResult<()> {
        Err(unavailable(DesignOp::OnTopologyEventAccepted))
    }


    fn project_topology(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        reference: &ArrayD<f64>,
    ) -> CaeResult<ArrayD<f64>> {
        Err(unavailable(DesignOp::ProjectTopology))
    }


    fn optimizer_lifecycle(&self, problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Err(unavailable(DesignOp::OptimizerLifecycle))
    }

    fn lifecycle_is_problem_specific(&self) -> bool {
        false
    }


    fn install_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        guess: &MatchingTimeNewtonGuess,
    ) -> CaeResult<Map<String, Value>> {
        Err(unavailable(DesignOp::InstallMatchingTimeGuess))
    }


    fn export_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        require_accepted: bool,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        Err(unavailable(DesignOp::ExportMatchingTimeGuess))
    }


    fn physical_certification(&self, problem: &ProviderProblem) -> CaeResult<Map<String, Value>> {
        Err(unavailable(DesignOp::PhysicalCertification))
    }

    fn authoring_design_coordinates(&self, problem: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        None
    }

    fn default_design_bindings(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        None
    }

    fn validate_response_selection(
        &self,
        problem: &ProviderProblem,
        names: &[String],
    ) -> Option<CaeResult<()>> {
        None
    }

    fn problem_objectives(&self, problem: &ProviderProblem) -> Vec<Value> {
        Vec::new()
    }

    fn problem_responses(&self, problem: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        None
    }

    fn coupling_control(&self, problem: Option<&Value>) -> Option<CaeResult<Value>> {
        None
    }

    fn preflight_effects(&self, problem: &Value) -> Option<CaeResult<Value>> {
        None
    }

    fn cached_evaluation_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        operating_point: usize,
    ) -> Option<CaeResult<CachedEvaluation>> {
        None
    }

    fn replan_problem_revision(&self, previous: &Value, revised: &Value) -> Option<CaeResult<Value>> {
        None
    }

    fn has_computation_effort_scope(&self) -> bool {
        false
    }

    fn computation_effort_scope(&self, binding: &Value) -> Option<CaeResult<ProviderScope>> {
        None
    }

    fn staged_initial_guess_scope(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        policy: &Value,
    ) -> Option<CaeResult<ProviderScope>> {
        None
    }

    fn problem_document(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        problem.downcast_ref::<Value>().cloned().map(Ok)
    }

    fn workspace_declaration(&self, name: &str, problem: Option<&Value>) -> Option<CaeResult<Value>> {
        None
    }

    fn exact_computation_effort_candidate_profiles(&self) -> Option<CaeResult<Value>> {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CachedEvaluation {
    Available(Evaluation),
    Unavailable(Map<String, Value>),
}

pub struct ProviderScope {
    pub value: Option<Value>,
    pub guard: Option<Box<dyn Any + Send>>,
}

impl std::fmt::Debug for ProviderScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderScope").field("value", &self.value).finish_non_exhaustive()
    }
}

pub struct DesignInterface {
    cast: fn(&dyn Any) -> Option<&dyn DesignOperations>,
}

impl std::fmt::Debug for DesignInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DesignInterface")
    }
}

fn cast_to<P: DesignOperations + 'static>(any: &dyn Any) -> Option<&dyn DesignOperations> {
    any.downcast_ref::<P>().map(|p| p as &dyn DesignOperations)
}

impl DesignInterface {
    #[must_use]
    pub const fn of<P: DesignOperations + 'static>() -> Self {
        Self { cast: cast_to::<P> }
    }
}

struct InterfaceOf<P>(std::marker::PhantomData<P>);

impl<P: DesignOperations + 'static> InterfaceOf<P> {
    const INTERFACE: DesignInterface = DesignInterface::of::<P>();
}

#[must_use]
pub fn design_interface<P: DesignOperations + 'static>(
    name: &str,
) -> Option<&'static (dyn Any + Send + Sync)> {
    if name == DESIGN_OPERATIONS {
        let iface: &'static DesignInterface = &InterfaceOf::<P>::INTERFACE;
        Some(iface)
    } else {
        None
    }
}

#[must_use]
pub fn design_operations(provider: &dyn CaeProvider) -> Option<&dyn DesignOperations> {
    let iface = provider.interface(DESIGN_OPERATIONS)?.downcast_ref::<DesignInterface>()?;
    (iface.cast)(provider.as_any())
}


pub fn admitted_responses(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    declared: &[String],
) -> CaeResult<Vec<String>> {
    let mut out = declared.to_vec();
    if let Some(extra) = design_operations(provider).and_then(|o| o.problem_responses(problem)) {
        for name in extra? {
            if name.is_empty() {
                return Err(CaeError::contract("provider problem responses must be nonempty ids"));
            }
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    Ok(out)
}

#[must_use]
pub fn provides(provider: &dyn CaeProvider, op: DesignOp) -> bool {
    match design_operations(provider) {
        Some(ops) => ops.provides(op),
        None => matches!(op, DesignOp::Evaluate | DesignOp::Sensitivity),
    }
}


pub fn check_operating_point(
    provider: &dyn CaeProvider,
    op: DesignOp,
    operating_point: usize,
) -> CaeResult<()> {
    if operating_point == 0 {
        return Ok(());
    }
    let dispatches = design_operations(provider).is_some_and(|ops| ops.dispatches_operating_points(op));
    if dispatches {
        Ok(())
    } else {
        Err(CaeError::contract(
            "provider does not implement operating_point dispatch; refusing to repeat the nominal state under another label",
        ))
    }
}


pub fn require_op<'a>(
    provider: &'a dyn CaeProvider,
    op: DesignOp,
    message: &str,
) -> CaeResult<&'a dyn DesignOperations> {
    match design_operations(provider) {
        Some(ops) if ops.provides(op) => Ok(ops),
        _ => Err(CaeError::contract(message.to_string())),
    }
}


pub fn legacy_evaluate(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    topology: &ArrayD<f64>,
    operating_point: usize,
) -> CaeResult<Evaluation> {
    check_operating_point(provider, DesignOp::Evaluate, operating_point)?;
    match design_operations(provider) {
        Some(ops) if ops.dispatches_operating_points(DesignOp::Evaluate) => {
            ops.evaluate_at(problem, topology, operating_point)
        }
        _ => provider.evaluate(problem, topology),
    }
}


pub fn legacy_sensitivity(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    topology: &ArrayD<f64>,
    response: &str,
    operating_point: usize,
) -> CaeResult<Sensitivity> {
    check_operating_point(provider, DesignOp::Sensitivity, operating_point)?;
    match design_operations(provider) {
        Some(ops) if ops.dispatches_operating_points(DesignOp::Sensitivity) => {
            ops.sensitivity_at(problem, topology, response, operating_point)
        }
        _ => provider.sensitivity(problem, topology, response),
    }
}
