// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]


#![allow(
    clippy::float_cmp,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::items_after_statements,
    clippy::nonminimal_bool,
    clippy::match_same_arms,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    clippy::struct_excessive_bools
)]

pub mod error;
pub mod py;
pub mod spatial_values;

pub use error::{AResult, AuthoringError};

pub const CRATE: &str = "implexity-authoring";

pub const LAYER: &str = "kernel";
pub mod api;
pub mod editing;
pub mod engineering_glyphs;
pub mod entities;
pub mod field_brush;
pub mod field_interaction;
pub mod geometry_freeze;
pub mod geometry_holds;
pub mod geometry_sculpt;
pub mod geometry_selection;
pub mod guided_setup;
pub mod incremental_fields;
pub mod interaction_runtime;
pub mod interaction_transactions;
pub mod interactive_runtime;
pub mod kinds;
pub mod manipulation;
pub mod migrate_problem_backend;
pub mod model_manager;
pub mod model_view;
pub mod optimization_setup;
pub mod physics_binding;
pub mod problem;
pub mod progressive_fields;
pub mod rigid_translation;
pub mod seeds;
pub mod sensitivity_authoring;
pub mod services;
pub mod setup_transaction;
pub mod spatial_design;
pub mod spatial_selection;
pub mod spatial_transactions;
pub mod surface_authoring;
pub mod surface_regions;
pub mod sync;
pub mod zlib_deflate;
