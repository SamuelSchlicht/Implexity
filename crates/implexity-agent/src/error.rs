// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
