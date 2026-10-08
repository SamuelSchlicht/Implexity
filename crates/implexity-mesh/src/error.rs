// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("{0}")]
    Invalid(String),
    #[error("case rejected:\n  {}", .0.join("\n  "))]
    Case(Vec<String>),
    #[error("mesh rejected:\n  {}", .0.join("\n  "))]
    Rejected(Vec<String>),
    #[error("{}", implexity_core::py_repr::repr_str(.0))]
    Key(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Stale(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl MeshError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io { context: context.into(), source }
    }
}
