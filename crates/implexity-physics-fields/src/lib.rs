// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![forbid(unsafe_code)]


#![allow(
    clippy::redundant_closure_for_method_calls,
    clippy::assign_op_pattern,
    clippy::float_cmp,
    clippy::nonminimal_bool,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::cast_precision_loss,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::needless_range_loop
)]

pub const CRATE: &str = "implexity-physics-fields";

pub const LAYER: &str = "physics";

pub mod adapter;
pub mod addins;
pub mod common;
pub mod electromagnetic_loads;
pub mod electrothermal;
pub mod host;
pub mod neutral;
pub mod prescribed_joule;
pub mod prescribed_volumetric_heating;
