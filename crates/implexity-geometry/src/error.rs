// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GeometryError {
    #[error("{0}")]
    Model(String),
    #[error("model document rejected:\n  {}", .0.join("\n  "))]
    ModelDoc(Vec<String>),
    #[error("{0}")]
    Expr(String),
    #[error("{0}")]
    BoundViolation(String),
    #[error("{message}")]
    Transpile {
        message: String,
        refusals: Vec<serde_json::Value>,
    },
    #[error("{0}")]
    Value(String),
    #[error("{}", crate::pyfmt::str_repr(.0))]
    Key(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(String),
}

impl GeometryError {
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        match self {
            Self::ModelDoc(p) => p.clone(),
            other => vec![other.to_string()],
        }
    }

    #[must_use]
    pub fn is_model_error(&self) -> bool {
        matches!(self, Self::Model(_) | Self::Transpile { .. })
    }
}

pub type GResult<T> = Result<T, GeometryError>;

pub(crate) fn model_err<T>(msg: impl Into<String>) -> GResult<T> {
    Err(GeometryError::Model(msg.into()))
}

pub(crate) fn value_err<T>(msg: impl Into<String>) -> GResult<T> {
    Err(GeometryError::Value(msg.into()))
}
