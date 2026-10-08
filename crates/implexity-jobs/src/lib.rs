// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]
#![allow(

    clippy::float_cmp,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::nonminimal_bool,

    clippy::case_sensitive_file_extension_comparisons,

    clippy::unused_self,
    clippy::items_after_statements,
    clippy::type_complexity,
    clippy::too_many_arguments,

    clippy::struct_excessive_bools,
    clippy::large_enum_variant,
    clippy::struct_field_names
)]

pub mod api;
pub mod artifacts;
pub mod checkpoint_scratch;
pub mod dynamic_capture;
pub mod effort;
pub mod epoch_capture;
pub mod epoch_fields;
pub mod epoch_state;
pub mod error;
pub mod heavy_runtime;
pub mod hierarchical_job;
pub mod managed_evaluation;
pub mod managed_io;
pub mod managed_workers;
pub mod manager;
pub mod optimize;
pub mod preview;
pub mod private;
pub mod provider_hooks;
pub mod provider_job;
pub mod provider_worker;
pub mod result_arrays;
pub mod solver_telemetry;
pub mod solver_recovery;
pub mod stage_search;
pub mod studies;
pub mod worker_cli;
pub mod working_result;

pub const CRATE: &str = "implexity-jobs";

pub const LAYER: &str = "kernel";
