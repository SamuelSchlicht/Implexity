// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]


#![allow(
    clippy::float_cmp,
    clippy::float_cmp_const,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::items_after_statements,

    clippy::nonminimal_bool,

    clippy::match_same_arms
)]

pub mod boxes;
pub mod contour_from_sdf;
pub mod derivatives;
pub mod differentiable_drag;
pub mod differentiable_reachability;
pub mod direct_occupancy;
pub mod document;
pub mod domain_sdf;
pub mod engineering;
pub mod engineering_history;
pub mod error;
pub mod errors;
pub mod eval;
pub mod examples;
pub mod field_registration;
pub mod field_views;
pub mod fieldclass;
pub mod glsl;
pub mod glsl_text;
pub mod kinds;
pub mod lattice;
pub mod linalg3;
pub mod node;
pub mod numpy;
pub mod occupancy;
pub mod ops;
pub mod phase_connectivity;
pub mod phase_partition;
pub mod preview;
pub mod primitives;
pub mod profiles;
pub mod pyfmt;
pub mod result_fields;
pub mod sampled;
pub mod scalar;
pub mod semantic_regions;
pub mod topology_monitor;
pub mod tpms;
pub mod value;

pub use error::{GResult, GeometryError};
pub use fieldclass::{ClassKind, FieldClass};
pub use node::{Attr, Node, NodeRef, ParamRef, Registry};
pub use value::{DType, NdArray, ParamValue};

pub const CRATE: &str = "implexity-geometry";

pub const LAYER: &str = "kernel";

#[must_use]
pub fn kernel_registry() -> Registry {
    let mut r = Registry::empty();
    let all = primitives::entries()
        .into_iter()
        .chain(ops::entries())
        .chain(profiles::entries())
        .chain(tpms::entries())
        .chain(sampled::entries())
        .chain(lattice::entries());
    for e in all {

        let _ = r.register(e);
    }
    r
}

pub mod field_processing;
