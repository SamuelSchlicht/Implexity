// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LinalgError {
    #[error("shape error: {0}")]
    Shape(String),
    #[error("singular matrix: {0}")]
    Singular(String),
    #[error("matrix is not positive definite: {0}")]
    NotPositiveDefinite(String),
    #[error("non-finite values: {0}")]
    NonFinite(String),
    #[error("no convergence: {0}")]
    NoConvergence(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("operator failed: {0}")]
    Operator(String),
    #[error("out of memory: {0}")]
    OutOfMemory(String),
}

impl From<faer::sparse::FaerError> for LinalgError {
    fn from(e: faer::sparse::FaerError) -> Self {
        match e {
            faer::sparse::FaerError::OutOfMemory => Self::OutOfMemory("faer allocation failed".into()),
            faer::sparse::FaerError::IndexOverflow => Self::Shape("sparse index overflow".into()),
            other => Self::Shape(format!("faer: {other:?}")),
        }
    }
}
