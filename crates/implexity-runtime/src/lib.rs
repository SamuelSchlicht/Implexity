// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

pub mod addin;
pub mod algebraic_runtime;
pub mod api;
pub mod authority_gate;
pub mod cae_runtime;
pub mod canonical;
pub mod computation_effort;
pub mod coupling_control;
pub mod discretisation_convergence;
pub mod discretization_gate;
pub mod dynamic_frames;
pub mod exact_acceleration_production_authority;
pub mod exact_acceleration_qualification;
pub mod exact_execution_authority;
pub mod exact_provider_session;
pub mod execution_readiness;
pub mod intent_orchestrated;
pub mod multiphysics_monitor;
pub mod numerical_progress;
pub mod orchestration_coupling;
pub mod orchestration_runtime;
pub mod physics_quality;
pub mod problem;
pub mod provider_job_authority;
pub mod provider_problem_document;
pub mod pyval;
pub mod qualification_benchmark_authority;
pub mod qualification_operation_identity;
pub mod replay_validation;
pub mod residual_runtime;
pub mod resource_telemetry;
pub mod results;
pub mod worker_preimport_bootstrap;
pub mod worker_runtime_profile;
pub mod workspace_capabilities;

pub use implexity_core::orchestration::compat;
pub use implexity_solve::{operation_context, preconditioner_lease, trace};

pub const CRATE: &str = "implexity-runtime";

pub const LAYER: &str = "kernel";

pub mod provider_import;
