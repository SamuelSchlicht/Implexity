// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

pub mod bounds;
pub mod candidate_events;
pub mod constraint_admission;
pub mod coordinate_bounds;
pub mod design;
pub mod design_freedom;
pub mod design_state;
pub mod mma;
pub mod native_design;
pub mod numeric;
pub mod optimizer;
pub mod optjob;
pub mod provider_ops;
pub mod pyval;
pub mod regime;
pub mod response_program;
pub mod search;
pub mod stage_search;

pub use design::{DesignLayout, NamedArrays, design_identity};
pub use provider_ops::{
    DESIGN_OPERATIONS, DesignOp, DesignOperations, admitted_responses, design_interface, design_operations,
};

pub const CRATE: &str = "implexity-optim";

pub const LAYER: &str = "kernel";
