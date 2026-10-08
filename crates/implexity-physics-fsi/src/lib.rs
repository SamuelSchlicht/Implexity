// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

pub mod addins;
pub mod carrier;
pub mod contact;
pub mod editor;
pub mod frames;
pub mod interface;
mod json;
pub mod model;
pub mod moving_contact;
pub mod restart;
pub mod modifier;
pub mod options;
pub mod problem;
pub mod provider;
pub mod regime;
pub mod run;
pub mod templates;

pub const CRATE: &str = "implexity-physics-fsi";

pub const LAYER: &str = "physics";
