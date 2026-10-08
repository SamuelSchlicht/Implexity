// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]
#![recursion_limit = "512"]



#![allow(
    clippy::result_large_err,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::nonminimal_bool,
    clippy::type_complexity,
    clippy::redundant_closure_for_method_calls,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

pub mod addins;
pub mod channel_transport;
pub mod conjugate_energy_contract;
pub mod conjugate_history;
pub mod conjugate_interface;
pub mod conjugate_preload;
pub mod conservative_interfaces;
pub mod elastic_preload;
pub mod equation_of_state;
pub mod fluid_admission;
pub mod incompressible_transport;
pub mod liquid_admissibility;
pub mod liquid_pressure_margin;
pub mod local_group;
pub mod mac_energy_contract;
pub mod nodal_cell_exchange;
pub mod nodal_transport;
pub mod peng_robinson;
pub mod qualification_identity;
pub mod real_fluid_screening;
pub mod rv;
pub mod stokes_brinkman;
pub mod thermal_exchange;
pub mod thermal_stress_wall;
pub mod unified_history;
pub mod unified_history_preconditioner;
pub mod wall_closure;
pub mod water_saturation;

pub use addins::link;

pub const CRATE: &str = "implexity-physics-thermofluid";

pub const LAYER: &str = "physics";

pub mod porous_reference;

pub mod schema_ids;
