// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END






#![forbid(unsafe_code)]

pub mod api;
pub mod campaign;
pub mod capture;
pub mod contracts;
#[cfg(feature = "dynamic-results")]
pub mod dynamic;
pub mod error;
pub mod guidance;
pub mod host;
pub mod managed;
pub mod manual;
pub mod pyval;
mod render;
mod public_geometry;
mod public_scene;
pub mod runtime;
pub mod views;

pub use error::{AgentError, AgentResult};
pub use host::{AgentHost, AgentManagers, AgentModel, HostOp};
pub use runtime::AgentManager;

pub const CRATE: &str = "implexity-agent";

pub const LAYER: &str = "kernel";
