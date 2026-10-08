// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::path::PathBuf;
use std::sync::Arc;

use implexity_mesh::model_view::ModelView;
use serde_json::Value;

use crate::error::{AgentError, AgentResult};


#[derive(Debug, Clone, PartialEq)]
pub enum HostOp {

    ImportModel(Value),
    EditGraph(Value),
    SetParameters(Value),
    InspectParameters,
    InitializeLatticeSeed(Value),
    ValidateModel(Value),

    SeedCatalogue,
    SeedPreview(Value),
    SeedCommit(Value),
    SeedPreviewCurrent(Value),
    SeedBakeCurrent(Value),

    GuidedSetupInspect,
    GuidedSetupReview(Value),
    GuidedSetupApply(Value),
    GuidedSetupValidateApply(Value),
    OptimizationSetupInspect,
    OptimizationSetupSave(Value),
    OptimizationSetupValidate(Value),
    ReviseProblem(Value),
    ReviseProblemValidate(Value),

    InteractionCapabilities,
    Interaction(String, Value),
    PromoteCage(Value),
    RebindRegion(Value, Value),
    Manipulation(String, Value),

    CurrentProblem,
    SetEngineeringProblem {
        payload: Value,
        provider_envelope: bool,
    },

    HistoryList(usize),
    HistorySnapshot(String, Value),
    HistoryRestore(String),
    HistoryAppend(String, Value),

    JobsList,
    JobInfo(String),
    ReadEpochField(Value),
    ExportEpoch(String, i64),
    Results(Value),
    Sensitivity(Value),
    Preflight(Value),
    CheckGradients(Value),
    Start(Value),
    Steer(String, Value),
    UseWorkingDesign(Value),
    ValidateWorkingResult(Value),
    JobOperation(String, String),
    BranchAfterIntervention(String, Value),
    ResolveNumericalAttention(String, String, String),
    BranchAfterNumericalAttention(String, String),

    InspectStudy(String),
    CreateStudy(Value),
    RunStudyVariant(Value),

    ReadResultArray(Value),
    AuthoredEvaluationArtifact {
        result: Value,
        problem: Value,
        design: Value,
    },
}

impl HostOp {
    #[must_use]
    pub fn python(&self) -> &'static str {
        match self {
            Self::ImportModel(_) => "implicit.api.ModelManager.put",
            Self::EditGraph(_) => "implicit.api.ModelManager.edit",
            Self::SetParameters(_) => "implicit.api.ModelManager.set_parameters",
            Self::InspectParameters => "implicit.api.ModelManager.parameter_table",
            Self::InitializeLatticeSeed(_) => "implicit.lattice.initialization.initialize_solid_fraction",
            Self::ValidateModel(_) => "implicit.document.problems_of",
            Self::SeedCatalogue
            | Self::SeedPreview(_)
            | Self::SeedCommit(_)
            | Self::SeedPreviewCurrent(_)
            | Self::SeedBakeCurrent(_) => "implicit.seeds",
            Self::GuidedSetupInspect
            | Self::GuidedSetupReview(_)
            | Self::GuidedSetupApply(_)
            | Self::GuidedSetupValidateApply(_) => "implicit.guided_setup",
            Self::OptimizationSetupInspect
            | Self::OptimizationSetupSave(_)
            | Self::OptimizationSetupValidate(_)
            | Self::ReviseProblem(_)
            | Self::ReviseProblemValidate(_) => "implicit.optimization_setup",
            Self::InteractionCapabilities | Self::Interaction(..) | Self::PromoteCage(_) => {
                "implicit.interaction_http.runtime_for"
            }
            Self::RebindRegion(..) => "implicit.semantic_regions.rebind_semantic_region",
            Self::Manipulation(..) => "implicit.api.manipulation_manager",
            Self::CurrentProblem | Self::SetEngineeringProblem { .. } => "implicit.api.problem_store",
            Self::HistoryList(_)
            | Self::HistorySnapshot(..)
            | Self::HistoryRestore(_)
            | Self::HistoryAppend(..) => "implicit.api.engineering_history_manager",
            Self::JobsList
            | Self::JobInfo(_)
            | Self::ReadEpochField(_)
            | Self::ExportEpoch(..)
            | Self::Results(_)
            | Self::Sensitivity(_)
            | Self::Preflight(_)
            | Self::CheckGradients(_)
            | Self::Start(_)
            | Self::Steer(..)
            | Self::UseWorkingDesign(_)
            | Self::JobOperation(..)
            | Self::BranchAfterIntervention(..)
            | Self::ResolveNumericalAttention(..)
            | Self::BranchAfterNumericalAttention(..) => "implicit.api.ModelOptimizeManager",
            Self::ValidateWorkingResult(_) => "implicit.working_result.validate_request",
            Self::InspectStudy(_) | Self::CreateStudy(_) | Self::RunStudyVariant(_) => "implicit.studies",
            Self::ReadResultArray(_) | Self::AuthoredEvaluationArtifact { .. } => {
                "implicit.result_arrays_http"
            }
        }
    }

    #[must_use]
    pub fn owner(&self) -> &'static str {
        match self {
            Self::HistoryList(_)
            | Self::HistorySnapshot(..)
            | Self::HistoryRestore(_)
            | Self::HistoryAppend(..)
            | Self::JobsList
            | Self::JobInfo(_)
            | Self::ReadEpochField(_)
            | Self::ExportEpoch(..)
            | Self::Results(_)
            | Self::Sensitivity(_)
            | Self::Preflight(_)
            | Self::CheckGradients(_)
            | Self::Start(_)
            | Self::Steer(..)
            | Self::UseWorkingDesign(_)
            | Self::ValidateWorkingResult(_)
            | Self::JobOperation(..)
            | Self::BranchAfterIntervention(..)
            | Self::ResolveNumericalAttention(..)
            | Self::BranchAfterNumericalAttention(..)
            | Self::InspectStudy(_)
            | Self::CreateStudy(_)
            | Self::RunStudyVariant(_)
            | Self::ReadResultArray(_)
            | Self::AuthoredEvaluationArtifact { .. } => "WP-11",
            _ => "WP-12",
        }
    }

    #[must_use]
    pub fn unavailable(&self) -> AgentError {
        AgentError::refused(format!(
            "{} is not available in this service: its manager ({}) is not installed",
            self.python(),
            self.owner()
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManagedStatus {
    pub state: String,
    pub wire: Value,
}

pub trait ManagedControl: Send + Sync {
    fn operation_id(&self) -> String;
}

pub trait ManagedSupervisor: Send + Sync {


    fn status(&self, operation_id: &str) -> AgentResult<ManagedStatus>;


    fn request_cancel(&self, control: &dyn ManagedControl) -> AgentResult<ManagedStatus>;


    fn committed_directory(&self, control: &dyn ManagedControl) -> AgentResult<PathBuf>;

    fn solver_recovery(&self, _control: &dyn ManagedControl) -> AgentResult<Option<Value>> {
        Ok(None)
    }
}

pub trait ManagedCapsule: Send + Sync {
    fn operation_kind(&self) -> String;


    fn start(&self, supervisor: &Arc<dyn ManagedSupervisor>) -> AgentResult<Arc<dyn ManagedControl>>;


    fn finalize(&self, committed_directory: &std::path::Path) -> AgentResult<Value>;
}

pub type HostGuard = Box<dyn std::any::Any>;

pub trait AgentModel: ModelView + Send + Sync {


    fn status_value(&self) -> AgentResult<Value>;
    fn document_sha256(&self) -> Option<String>;
    fn geometry_model(&self) -> Option<Arc<implexity_geometry::document::model::Model>>;
}

pub trait AgentHost: Send + Sync {
    fn state_dir(&self) -> PathBuf;
    fn dynamic_results_available(&self) -> bool {
        implexity_runtime::dynamic_frames::availability::active()
    }
    fn dynamic_catalogue(&self) -> implexity_runtime::dynamic_frames::catalogue::Catalogue {
        implexity_runtime::dynamic_frames::catalogue::Catalogue {
            service_root: Some(
                self.state_dir().join(implexity_runtime::dynamic_frames::catalogue::SERVICE_ROOT),
            ),
            jobs_root: None,
        }
    }


    fn call(&self, op: HostOp) -> AgentResult<Value>;
    fn model(&self) -> Option<Arc<dyn AgentModel>>;


    fn detached_model(&self, document: &Value) -> AgentResult<Arc<dyn AgentModel>>;


    fn result_store(
        &self,
        artifact_id: &str,
    ) -> AgentResult<Arc<dyn implexity_render::artifact::ResultArtifactStore + Send + Sync>>;


    fn packages_status(&self) -> AgentResult<Value>;


    fn packages_change(&self, package: &str, operation: &str, expected: Option<&Value>)
    -> AgentResult<Value>;
    fn imported_providers(&self, _request: Option<&Value>, _check: bool) -> AgentResult<Value> { Err(AgentError::failed("Provider import is unavailable in this host")) }
    fn case_snapshot(&self) -> Option<(Value, Option<Value>)>;


    fn eval_lock(&self) -> AgentResult<HostGuard>;
    fn state_lock(&self) -> HostGuard;


    fn managed_supervisor(&self, private_root: &std::path::Path) -> AgentResult<Arc<dyn ManagedSupervisor>>;


    fn prepare_managed_evaluation(&self, kind: &str, request: Value) -> AgentResult<Arc<dyn ManagedCapsule>>;
    fn viewer_capture_origin(&self) -> Option<String>;
    fn viewer_asset(&self, name: &str) -> Option<Vec<u8>>;
}

pub trait AgentManagers: Send + Sync {


    fn call(&self, op: HostOp) -> AgentResult<Value>;
    fn model(&self) -> Option<Arc<dyn AgentModel>>;


    fn detached_model(&self, document: &Value) -> AgentResult<Arc<dyn AgentModel>>;


    fn result_store(
        &self,
        artifact_id: &str,
    ) -> AgentResult<Arc<dyn implexity_render::artifact::ResultArtifactStore + Send + Sync>>;


    fn managed_supervisor(&self, private_root: &std::path::Path) -> AgentResult<Arc<dyn ManagedSupervisor>>;


    fn prepare_managed_evaluation(&self, kind: &str, request: Value) -> AgentResult<Arc<dyn ManagedCapsule>>;
}
