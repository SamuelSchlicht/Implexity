// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

pub mod ageing_hooks;
pub mod api;
pub mod diagnostics;
pub mod error;
pub mod integration;
pub mod numerics;
pub mod optimizer_bridge;
pub mod preflight;
pub mod pyfmt;
pub mod resolved_stokes;
pub mod workspace_contract;
pub mod schemas;

pub use api::{CATALOG_SCHEMA, CfdWorkspaceController, catalog, route_table};
pub use error::{CfdError, CfdResult};
pub use optimizer_bridge::{declare as declare_optimization, run_with_universal_optimizer};
pub use preflight::{Issue, PreflightReport, run_preflight};
pub use resolved_stokes::{ResolvedStokesBrinkmanBackend, assemble_stokes_brinkman};
pub use workspace_contract::{
    BoundaryCondition, Brinkman, CfdProblem, Domain, Fluid, Objective, PressureGauge, SolverSettings,
    ThermalSettings, from_mapping,
};

pub const CRATE: &str = "implexity-physics-cfd";

pub const LAYER: &str = "physics";

pub mod schema_ids;
