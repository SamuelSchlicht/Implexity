// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![forbid(unsafe_code)]


#![allow(
    clippy::float_cmp,
    clippy::needless_range_loop,
    clippy::nonminimal_bool,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

pub mod addins;
pub mod ageing;
pub mod compressible;
pub mod d3q19;
pub mod design;
pub mod design_ops;
pub mod geometry;
pub mod history_block;
pub mod history_partials;
pub mod linear_structure;
#[cfg(feature = "moving")]
pub mod moving;
pub mod nparray;
pub mod porous;
pub mod ports;
pub mod provider;
pub mod radiation;
pub mod rv;
pub mod solver;
pub mod structure_provider;
pub mod thermal;
pub mod thermal_links;
pub mod thermal_provider;
pub mod wall_exchange;

pub const CRATE: &str = "implexity-physics-lbm";

pub const LAYER: &str = "physics";

pub mod scaling;

pub mod schema_ids;
