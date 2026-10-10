// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
pub mod nonlinear_graph;
pub mod positive_cone;
pub mod cone_residual;
pub mod constrained_graph;
pub mod block_constrained_graph;
pub mod partial_constrained_graph;
pub mod diagonal_graph;
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
pub mod persistent_history;
pub mod sparse_low_rank;
pub mod sparse_spd_low_rank;
pub mod step_stability;
pub mod time_functional;
pub mod time_stepper;

pub mod local_condensation;

pub mod rejected_state;

pub mod fixed_coordinate_graph;
