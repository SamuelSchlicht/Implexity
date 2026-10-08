// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

pub mod dual;
pub mod error;
pub mod forward;
pub mod gradcheck;
pub mod hyperdual;
pub mod implicit;
pub mod jet3;
pub mod revolve;
pub mod scalar;
pub mod scan;
pub mod small;
pub mod tape;

pub use dual::Dual;
pub use error::AdError;
pub use hyperdual::HyperDual;
pub use jet3::Jet3;
pub use scalar::Scalar;
pub use tape::{Grads, Tape, Var};

pub const CRATE: &str = "implexity-ad";

pub const LAYER: &str = "foundation";
