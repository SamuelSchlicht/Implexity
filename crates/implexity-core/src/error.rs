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


use std::fmt;
use serde_json::Value;


#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaeError {
    #[error("{0}")]
    Contract(String),
    #[error("{message}")]
    Recovery { cause: Box<Self>, report: serde_json::Value, message: String },
    #[error("{0}")]
    Convergence(String),
    #[error("{0}")]
    NewtonConvergence(String),
}

impl CaeError {
    pub fn solver_recovery(&self) -> Option<&serde_json::Value> {
        match self { Self::Recovery { report, .. } => Some(report), _ => None }
    }

    pub fn contract(message: impl Into<String>) -> Self {
        Self::Contract(message.into())
    }

    pub fn convergence(message: impl Into<String>) -> Self {
        Self::Convergence(message.into())
    }

    pub fn newton(message: impl Into<String>) -> Self {
        Self::NewtonConvergence(message.into())
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Contract(m) | Self::Convergence(m) | Self::NewtonConvergence(m) => m,
            Self::Recovery { message, .. } => message,
        }
    }

    #[must_use]
    pub fn is_convergence(&self) -> bool {
        match self { Self::Recovery { cause, .. } => cause.is_convergence(), _ => matches!(self, Self::Convergence(_) | Self::NewtonConvergence(_)) }
    }

    #[must_use]
    pub fn is_newton_convergence(&self) -> bool {
        match self { Self::Recovery { cause, .. } => cause.is_newton_convergence(), _ => matches!(self, Self::NewtonConvergence(_)) }
    }

    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Recovery { cause, .. } => cause.python_class(),
            Self::Contract(_) => "CAEContractError",
            Self::Convergence(_) => "CAEConvergenceError",
            Self::NewtonConvergence(_) => "CAENewtonConvergenceError",
        }
    }

    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Recovery { cause, .. } => cause.http_status(),
            Self::Contract(_) => 400,
            Self::Convergence(_) | Self::NewtonConvergence(_) => 500,
        }
    }

    #[must_use]
    pub fn context(self, context: &str) -> Self {
        match self {
            Self::Recovery { cause, report, message } => Self::Recovery { cause, report, message: format!("{context}: {message}") },
            Self::Contract(m) => Self::Contract(format!("{context}: {m}")),
            Self::Convergence(m) => Self::Convergence(format!("{context}: {m}")),
            Self::NewtonConvergence(m) => Self::NewtonConvergence(format!("{context}: {m}")),
        }
    }
}

pub type CaeResult<T> = Result<T, CaeError>;


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseError {
    pub problems: Vec<String>,
}

impl CaseError {
    pub fn new<I, S>(problems: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { problems: problems.into_iter().map(Into::into).collect() }
    }

    #[must_use]
    pub fn http_status(&self) -> u16 {
        422
    }

    #[must_use]
    pub fn envelope(&self) -> serde_json::Value {
        serde_json::json!({"error": "case rejected", "problems": self.problems})
    }
}

impl fmt::Display for CaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {

        write!(f, "case rejected:\n  {}", self.problems.join("\n  "))
    }
}

impl std::error::Error for CaseError {}

#[must_use]
pub fn error_envelope(message: &str, detail: Option<&str>) -> serde_json::Value {
    serde_json::json!({"error": message, "detail": detail})
}


pub fn validate_solver_recovery(raw: &Value) -> CaeResult<Value> {
    let fail = || CaeError::contract("invalid solver recovery report");
    let keys = ["schema", "operation", "status", "attempt_count", "retry_count", "max_retries", "settings_changed", "message", "first_failure", "final_failure"];
    let m = raw.as_object().filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k))).ok_or_else(fail)?;
    let text = |v: &Value, limit| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= limit && s.chars().all(|c| !c.is_control()));
    if m["schema"] != "implexity-solver-recovery/1" || m["attempt_count"].as_u64() != Some(2) || m["retry_count"].as_u64() != Some(1) || m["max_retries"].as_u64() != Some(1) || m["settings_changed"] != false || !text(&m["operation"],128) || !text(&m["message"],512) { return Err(fail()); }
    let status = m["status"].as_str().ok_or_else(fail)?;
    if !["retrying", "recovered", "needs_attention"].contains(&status) { return Err(fail()); }
    let valid_detail = |v: &Value| v.as_object().is_some_and(|d| d.len() == 2 && d.get("error_type").is_some_and(|v| text(v,128)) && d.get("reason").is_some_and(|v| text(v,1536)));
    if !valid_detail(&m["first_failure"]) || if status == "needs_attention" { !valid_detail(&m["final_failure"]) } else { !m["final_failure"].is_null() } { return Err(fail()); }
    Ok(raw.clone())
}
