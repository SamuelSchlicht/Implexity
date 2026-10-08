// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub mod assembly;
pub mod component_field;
pub mod config;
pub mod controls;
pub mod freeze;
pub mod initialization;
pub mod morphology;
pub mod node;
pub mod numerics;
pub mod synth;

pub use assembly::ControlledAssembly;
pub use controls::{CONTROL_SCHEMA, control_components, control_contract};
pub use node::ControlledLattice;

#[must_use]
pub fn entries() -> crate::node::KernelEntryList {
    vec![ControlledLattice::entry(), ControlledAssembly::entry()]
}
