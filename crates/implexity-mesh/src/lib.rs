// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

pub mod bodyapi;
pub mod bodyexport;
pub mod cast;
pub mod contours;
pub mod domain;
mod error;
pub mod exporters;
pub mod formats;
pub mod grid;
pub mod interop;
pub mod mc;
pub mod model_view;
pub mod numeric;
pub mod partition;
pub mod pyfmt;
pub mod raster;
pub mod step;
pub mod stl_export;
pub mod surface;
pub mod topology;
pub mod zip;

pub use error::MeshError;
pub use grid::{Field3, Grid3};

pub const CRATE: &str = "implexity-mesh";

pub const LAYER: &str = "kernel";
