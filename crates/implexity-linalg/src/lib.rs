// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

pub mod dense;
pub mod error;
pub mod graph;
pub mod ilu;
pub mod krylov;
pub mod lu;
pub mod multifrontal;
pub mod onenormest;
pub mod operator;
pub mod ordering;
pub mod retention;
pub mod sparse;
pub mod spatial;
pub mod spectral;

pub use error::LinalgError;
pub use faer::c64;
pub use operator::{FnOperator, Identity, LinearOperator};
pub use sparse::{AssemblyPlan, CscMatrix, CsrMatrix, Format};

pub const CRATE: &str = "implexity-linalg";

pub const LAYER: &str = "foundation";

pub mod nnls;
