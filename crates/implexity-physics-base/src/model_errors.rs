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


use serde_json::{Map, Value, json};

use implexity_core::CaeError;

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsModelIssue {
    pub code: String,
    pub message: String,
    pub path: String,
    pub severity: String,
    pub details: Option<Map<String, Value>>,
}

impl PhysicsModelIssue {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        let mut out = Map::new();
        out.insert("code".into(), json!(self.code));
        out.insert("message".into(), json!(self.message));
        out.insert("path".into(), json!(self.path));
        out.insert("severity".into(), json!(self.severity));
        if let Some(details) = self.details.as_ref().filter(|d| !d.is_empty()) {
            out.insert("details".into(), Value::Object(details.clone()));
        }
        Value::Object(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsModelErrorKind {
    Model,
    Validation,
    ChemistryBalance,
    BackendUnavailable,
    BackendQualification,
    AdjointQualification,
    ConservationQualification,
}

impl PhysicsModelErrorKind {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Model => "PHYSICS_MODEL_ERROR",
            Self::Validation => "PHYSICS_VALIDATION_FAILED",
            Self::ChemistryBalance => "PHYSICS_CHEMISTRY_UNBALANCED",
            Self::BackendUnavailable => "PHYSICS_BACKEND_MISSING",
            Self::BackendQualification => "PHYSICS_BACKEND_UNQUALIFIED",
            Self::AdjointQualification => "PHYSICS_ADJOINT_UNQUALIFIED",
            Self::ConservationQualification => "PHYSICS_CONSERVATION_UNQUALIFIED",
        }
    }

    #[must_use]
    pub fn class_name(self) -> &'static str {
        match self {
            Self::Model => "PhysicsModelError",
            Self::Validation => "PhysicsValidationError",
            Self::ChemistryBalance => "ChemistryBalanceError",
            Self::BackendUnavailable => "BackendUnavailableError",
            Self::BackendQualification => "BackendQualificationError",
            Self::AdjointQualification => "AdjointQualificationError",
            Self::ConservationQualification => "ConservationQualificationError",
        }
    }

    #[must_use]
    pub fn is_contract(self) -> bool {
        matches!(self, Self::Validation | Self::ChemistryBalance)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PhysicsError {
    #[error("{0}")]
    Cae(#[from] CaeError),
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Key(String),
    #[error("{0}")]
    Type(String),
    #[error("{message}")]
    Model {
        kind: PhysicsModelErrorKind,
        message: String,
        path: String,
        details: Map<String, Value>,
    },
}

pub type PhysicsResult<T> = Result<T, PhysicsError>;

impl PhysicsError {
    pub fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }

    pub fn contract(message: impl Into<String>) -> Self {
        Self::Cae(CaeError::contract(message))
    }

    pub fn convergence(message: impl Into<String>) -> Self {
        Self::Cae(CaeError::convergence(message))
    }

    pub fn model(kind: PhysicsModelErrorKind, message: impl Into<String>) -> Self {
        Self::Model { kind, message: message.into(), path: String::new(), details: Map::new() }
    }

    pub fn validation(message: impl Into<String>, path: impl Into<String>) -> Self {
        Self::Model {
            kind: PhysicsModelErrorKind::Validation,
            message: message.into(),
            path: path.into(),
            details: Map::new(),
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        self.to_string()
    }

    #[must_use]
    pub fn is_contract(&self) -> bool {
        match self {
            Self::Cae(e) => matches!(e, CaeError::Contract(_)),
            Self::Model { kind, .. } => kind.is_contract(),
            _ => false,
        }
    }

    #[must_use]
    pub fn is_convergence(&self) -> bool {
        matches!(self, Self::Cae(e) if e.is_convergence())
    }

    #[must_use]
    pub fn as_issue(&self) -> Option<PhysicsModelIssue> {
        match self {
            Self::Model { kind, message, path, details } => Some(PhysicsModelIssue {
                code: kind.code().into(),
                message: message.clone(),
                path: path.clone(),
                severity: "error".into(),
                details: Some(details.clone()),
            }),
            _ => None,
        }
    }

    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Cae(e) => e.python_class(),
            Self::Value(_) => "ValueError",
            Self::Key(_) => "KeyError",
            Self::Type(_) => "TypeError",
            Self::Model { kind, .. } => kind.class_name(),
        }
    }
}

impl From<PhysicsError> for CaeError {
    fn from(e: PhysicsError) -> Self {
        match e {
            PhysicsError::Cae(inner) => inner,
            other => CaeError::contract(other.to_string()),
        }
    }
}
