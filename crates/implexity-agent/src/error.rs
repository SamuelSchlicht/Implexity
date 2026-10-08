// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Permission(String),
    #[error("{0}")]
    KeyError(String),
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
    #[error("{message}")]
    SolverRecovery { message: String, report: Value },
    #[error("{0}")]
    ManagedNotStarted(Box<Self>),
}

impl AgentError {
    pub fn managed_not_started(self) -> Self {
        Self::ManagedNotStarted(Box::new(self))
    }

    pub fn contract(message: impl Into<String>) -> Self {
        Self::Contract(message.into())
    }

    pub fn permission(message: impl Into<String>) -> Self {
        Self::Permission(message.into())
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::Refused(message.into())
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Contract(m)
            | Self::KeyError(m)
            | Self::Permission(m)
            | Self::Refused(m)
            | Self::Failed(m) => m,
            Self::SolverRecovery { message, .. } => message,
            Self::ManagedNotStarted(e) => e.message(),
        }
    }

    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Contract(_) => "AgentContractError",
            Self::KeyError(_) => "KeyError",
            Self::Permission(_) => "AgentPermissionError",
            Self::Refused(_) => "AgentError",
            Self::Failed(_) | Self::SolverRecovery { .. } => "RuntimeError",
            Self::ManagedNotStarted(e) => e.python_class(),
        }
    }

    #[must_use]
    pub fn reply(&self) -> (u16, Value) {
        if let Self::ManagedNotStarted(e) = self {
            let (status, mut body) = e.reply();
            body["managed_start_outcome"] = json!("not_started");
            return (status, body);
        }
        let problems = json!([self.message()]);
        match self {
            Self::Permission(_) => (
                403,
                json!({"ok": false, "error": "agent permission refused", "problems": problems}),
            ),
            Self::Contract(_) | Self::KeyError(_) | Self::Refused(_) => (
                422,
                json!({"ok": false, "error": "agent action rejected", "problems": problems}),
            ),
            Self::Failed(_) => (
                500,
                json!({"ok": false, "error": "agent action failed", "problems": problems}),
            ),
            Self::SolverRecovery { message, report } => (500, json!({"ok": false, "error": "Solver needs attention", "problems": [message], "solver_recovery": report})),
            Self::ManagedNotStarted(e) => e.reply(),
        }
    }
}

impl From<implexity_core::CaeError> for AgentError {
    fn from(e: implexity_core::CaeError) -> Self {
        if let Some(report) = e.solver_recovery() { return Self::SolverRecovery { message: e.message().to_string(), report: report.clone() }; }
        match e {
            implexity_core::CaeError::Contract(m) => Self::Contract(m),
            other => Self::Failed(other.message().to_string()),
        }
    }
}

pub type AgentResult<T> = Result<T, AgentError>;
