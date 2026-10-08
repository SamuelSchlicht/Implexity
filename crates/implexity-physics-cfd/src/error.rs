// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::CaeError;
use implexity_linalg::LinalgError;

pub type CfdResult<T> = Result<T, CfdError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CfdError {
    #[error("{0}")]
    Input(String),
    #[error("{0}")]
    Type(String),
    #[error("{0}")]
    Key(String),
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Convergence(String),
    #[error("{0}")]
    Qualification(String),
    #[error("{0}")]
    Optimization(String),
    #[error("{0}")]
    Integration(String),
    #[error("{0}")]
    Runtime(String),
}

impl CfdError {
    #[must_use]
    pub fn python_name(&self) -> &'static str {
        match self {
            Self::Input(_) => "CFDInputError",
            Self::Type(_) => "TypeError",
            Self::Key(_) => "KeyError",
            Self::Contract(_) => "SaddlePointContractError",
            Self::Convergence(_) => "SaddlePointConvergenceError",
            Self::Qualification(_) => "ResolvedCFDQualificationError",
            Self::Optimization(_) => "CFDOptimizationError",
            Self::Integration(_) => "CFDIntegrationError",
            Self::Runtime(_) => "RuntimeError",
        }
    }

    #[must_use]
    pub fn is_value_error(&self) -> bool {
        matches!(self, Self::Input(_) | Self::Contract(_) | Self::Type(_) | Self::Key(_))
    }
}

impl From<LinalgError> for CfdError {
    fn from(e: LinalgError) -> Self {
        Self::Contract(e.to_string())
    }
}

impl From<CfdError> for CaeError {
    fn from(e: CfdError) -> Self {
        match e {
            CfdError::Convergence(m) => Self::convergence(m),
            other => Self::contract(other.to_string()),
        }
    }
}
