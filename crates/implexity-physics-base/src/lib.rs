// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

pub mod addins;
pub mod array;
pub mod boundary_regions;
pub mod bridge;
pub mod contracts;
pub mod core_bridge;
pub mod core_sufficiency;
pub mod coupling_policy;
pub mod cyclic_plastic_strain;
pub mod density_interpolation;
pub mod engineering_library;
pub mod grad_sanitize;
pub mod history_numerical_extension;
pub mod material_domains;
pub mod materials;
pub mod model_errors;
pub mod models;
pub mod occupancy_grayness;
pub mod planner;
pub mod provenance;
pub mod reference_field_solvers;
pub mod region_temperature_extrema;
pub mod registry;
pub mod runtime;
pub mod sufficiency;
pub mod temperature_extrema;
pub mod temperature_projection;
pub mod thermal_exchange_law;
pub mod wall_film_temperature;

pub use model_errors::{PhysicsError, PhysicsResult};

pub const CRATE: &str = "implexity-physics-base";

pub const LAYER: &str = "physics";

pub mod schema_ids;
