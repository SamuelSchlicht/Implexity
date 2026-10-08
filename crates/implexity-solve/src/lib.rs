// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





#![forbid(unsafe_code)]

pub const CRATE: &str = "implexity-solve";

pub const LAYER: &str = "kernel";

pub mod certificate;
pub mod convergence;
pub mod diagnostics;
pub mod exact_matrix;
pub mod factorization;
pub mod implicit_block;
pub mod linear_workspace;
pub mod local_assembly;
pub mod matrix;
pub mod native_history;
pub mod newton_krylov;
pub mod nonsmooth;
pub mod numerical_state;
pub mod operation_context;
pub mod preconditioner_lease;
pub mod pyfmt;
pub mod trace;

pub use convergence::{
    ConvergenceCriterion, ConvergenceReport, Criterion, PerFieldCriterion, ScalarL2Criterion,
};
pub use exact_matrix::{AdmittedExactMatrix, ExactMatrixIdentity, admit_exact_matrix};
pub use factorization::Factorization;
pub use implicit_block::{
    BatchedAdjointResult, BlockCallbacks, BlockOptions, ImplicitBlockSystem, ImplicitSolveResult,
};
pub use local_assembly::{AssemblyOptions, Incidence, Kind, LocalResidual, LocalResidualAssembly};
pub use matrix::{FnAction, Jacobian, MatrixAction};
pub use native_history::{
    HistoryAdjoint, HistoryOptions, HistoryProblem, HistorySolution, HistorySolveOptions, NativeHistorySystem,
};
pub mod affine_history;
pub mod ale_geometry;
pub mod ale_transport;
pub mod block_preconditioning;
pub mod boundary_control;
pub mod complex_spectral;
pub mod composite_field_solver;
pub mod conservative_transfer;
pub mod coupled_history;
pub mod dae;
pub mod differentiable;
pub mod geometry_design_map;
pub mod hybrid_linearization;
pub mod hyperbolic;
pub mod implicit_material_surface;
pub mod interface_variational;
pub mod matching_time_guess;
pub mod material_cut_ale;
pub mod nonmatching_interface;
pub mod recycling_linearization;
pub mod sparse_block_residual;
pub mod sparse_field_solver;
pub mod spectral;
pub mod unstructured_transfer;
pub mod sparse_variational_interface {
    pub use crate::interface_variational::{SparseInterfaceSystem, sparse_mortar, sparse_symmetric_nitsche};
}
pub mod adaptive_scheduler;
pub mod approximation;
pub mod checkpointed_history;
pub mod crack_topology;
pub mod dynamic_program;
pub mod event_partition;
pub mod field_graph;
pub mod field_network;
pub mod fracture_events;
pub mod geometric_remesh;
pub mod gradient_fields;
pub mod history_observers;
pub mod interface_quasi_newton;
pub mod local_refinement;
pub mod moving_discontinuity;
pub mod multidim_front;
pub mod multirate_coupling;
pub mod periodic;
pub mod state_store;
pub mod step_stability;
pub mod time_functional;
pub mod time_stepper;

pub mod local_condensation;

pub mod rejected_state;
