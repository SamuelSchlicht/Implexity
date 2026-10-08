// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AdError {
    #[error("shape error: {0}")]
    Shape(String),
    #[error("singular matrix: {0}")]
    Singular(String),
    #[error("non-finite value: {0}")]
    NonFinite(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("callback failed: {0}")]
    Callback(String),
}
