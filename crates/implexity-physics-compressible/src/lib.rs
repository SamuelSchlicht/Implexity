// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

#![forbid(unsafe_code)]
pub const CRATE: &str = "implexity-physics-compressible";
pub const LAYER: &str = "physics";
pub mod errors;
pub mod roots;
pub mod array;
pub mod pyval;
pub mod equilibrium_chemistry;
pub mod isentropic_expansion;
pub mod plug_nozzle_expansion;
pub mod injector_mixing;
pub mod turbomachine;
pub mod feed_system_screening;
pub mod injection_screening;
pub mod radiation_screening;
pub mod thermoacoustic_screening;
pub mod euler3d;
pub mod quasi1d_euler;
pub mod euler3d_ad;
pub mod euler3d_muscl;
pub mod screening;
pub mod chemistry;
pub mod quasi1d_nozzle_flow;
pub mod euler3d_structure;

pub mod providers;
pub mod package;
pub mod response_derivatives;
pub mod conjugate_closures;
pub mod perfect_gas;

pub mod schema_ids;
