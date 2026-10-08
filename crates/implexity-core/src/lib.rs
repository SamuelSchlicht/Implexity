// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

pub mod backends;
pub mod capability_status;
pub mod component_manifests;
pub mod contracts;
pub mod contributions;
pub mod coupling_graph;
pub mod coupling_inventory;
pub mod distributions;
pub mod error;
pub mod extensions;
pub mod field_source_catalog;
pub mod graph;
pub mod history_field_sources;
pub mod ids;
pub mod json;
pub mod mathematical_contracts;
pub mod numeric_contract;
pub mod numerical_requirements;
pub mod objective_terms;
pub mod orchestration;
pub mod package_catalog;
pub mod package_session;
pub mod package_state;
pub mod packages;
pub mod parity;
pub mod plugins;
pub mod providers;
pub mod py_repr;
pub mod pyobj;
pub mod registries;
pub mod rng;
pub mod route_tables;
pub mod runtime_environment;
pub mod schema_ids;
pub mod schemas;
pub mod semantic_physics;
pub mod sufficiency;
pub mod sync;
pub mod wire;

pub use error::{CaeError, CaeResult, CaseError};

pub const CRATE: &str = "implexity-core";

pub const LAYER: &str = "foundation";

pub const PYTHON_COMPATIBILITY_VERSION: &str = "24.0.0rc12.dev9";

pub const RUST_VERSION: &str = env!("CARGO_PKG_VERSION");
