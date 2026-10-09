// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END






#![forbid(unsafe_code)]
#![recursion_limit = "512"]

#![allow(
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::nonminimal_bool,
    clippy::type_complexity,
    clippy::items_after_statements,
    clippy::result_large_err
)]

pub const CRATE: &str = "implexity-physics-solid";

pub const LAYER: &str = "physics";

pub mod addins;
pub mod components;
pub mod coupling;
pub mod coupling_inventory;
pub mod creep_life;
pub mod fatigue;
pub mod fatigue_life;
pub mod history;
pub mod hyperelastic;
pub mod implicit_hyperelastic_support;
pub mod inelastic;
pub mod mandel;
pub mod material;
pub mod moving_interface;
pub mod pchip;
pub mod phase_stress_transfer;
pub mod polymer;
pub mod soft;
pub mod soft_fsi;
pub mod solid_elements;
pub mod solid_energy_contract;
pub mod solid_exchange;
pub mod solid_face_trace;
pub mod solid_history;
pub mod solid_nodal_balance;

pub mod structural_dynamics;
pub mod structural_inertia;
pub mod thermoelastic_energy;
pub mod util;

pub mod schema_ids;

pub mod contact_compliance;

pub mod three_stage_creep;
