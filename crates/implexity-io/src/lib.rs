// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

pub const CRATE: &str = "implexity-io";

pub const LAYER: &str = "foundation";

pub mod atomic;
pub mod bundle_resources;
pub mod digest;
pub mod fsguard;
pub mod heavy_lease;
pub mod locate;
pub mod npy;
pub mod npz;
pub mod png_io;
pub mod provenance;
pub mod source_inventory;
pub mod zip;

pub mod storage_budget;
