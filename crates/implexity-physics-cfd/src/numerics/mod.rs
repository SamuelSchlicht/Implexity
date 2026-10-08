// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



pub mod adjoint;
pub mod config;
pub mod preconditioner;
pub mod reference;
pub mod solver;
pub mod system;
pub mod validation;

pub use adjoint::{DiscreteAdjointResult, solve_discrete_adjoint};
pub use config::solver_config_from_problem;
pub use preconditioner::{
    FactorizationStrategy, SaddlePointLduPreconditioner, SchurApproximation, SparseApproximateInverse,
    build_diagonal_velocity_schur,
};
pub use reference::{GridFluxReferenceModel, build_grid_flux_reference_model};
pub use solver::{KrylovMethod, SaddlePointKrylovSolver, SaddlePointSolveResult, SaddlePointSolverConfig};
pub use system::SaddlePointSystem;
