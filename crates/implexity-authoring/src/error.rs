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



use implexity_geometry::GeometryError;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum AuthoringError {
    #[error(transparent)]
    Cae(implexity_core::CaeError),
    #[error("{message}")]
    Value {
        class: &'static str,
        message: String,
    },
    #[error("{}", problems_message(class, problems))]
    Problems {
        class: &'static str,
        problems: Vec<String>,
    },
    #[error("{message}")]
    Runtime {
        class: &'static str,
        message: String,
    },
    #[error("{0}")]
    Key(String),
    #[error("{0}")]
    Type(String),
    #[error("{0}")]
    Io(String),
    #[error(transparent)]
    Geometry(#[from] GeometryError),
}

fn problems_message(class: &str, problems: &[String]) -> String {
    let head = match class {
        "ModelDocError" => "model document rejected:\n  ",
        "SeedError" => "geometry seed rejected:\n  ",
        "ProblemError" => "engineering problem rejected:\n  ",
        "StudyError" => "study rejected:\n  ",
        "DerivativeError" => "derivative request rejected:\n  ",
        _ => "",
    };
    if head.is_empty() { problems.join("; ") } else { format!("{head}{}", problems.join("\n  ")) }
}

impl AuthoringError {
    #[must_use]
    pub fn value(class: &'static str, message: impl Into<String>) -> Self {
        Self::Value { class, message: message.into() }
    }

    #[must_use]
    pub fn problems(class: &'static str, problems: Vec<String>) -> Self {
        Self::Problems { class, problems }
    }

    #[must_use]
    pub fn model_doc(problems: Vec<String>) -> Self {
        Self::Geometry(GeometryError::ModelDoc(problems))
    }

    #[must_use]
    pub fn runtime(class: &'static str, message: impl Into<String>) -> Self {
        Self::Runtime { class, message: message.into() }
    }

    #[must_use]
    pub fn class(&self) -> &str {
        match self {
            Self::Cae(e) => e.python_class(),
            Self::Value { class, .. } | Self::Problems { class, .. } | Self::Runtime { class, .. } => class,
            Self::Key(_) => "KeyError",
            Self::Type(_) => "TypeError",
            Self::Io(_) => "OSError",
            Self::Geometry(g) => match g {
                GeometryError::Model(_) | GeometryError::Transpile { .. } => "ModelError",
                GeometryError::ModelDoc(_) => "ModelDocError",
                GeometryError::Expr(_) => "ExprError",
                GeometryError::BoundViolation(_) => "BoundViolation",
                GeometryError::Value(_) => "ValueError",
                GeometryError::Key(_) => "KeyError",
                GeometryError::Cancelled => "Cancelled",
                GeometryError::Io(_) => "OSError",
            },
        }
    }

    #[must_use]
    pub fn problem_list(&self) -> Vec<String> {
        match self {
            Self::Problems { problems, .. } => problems.clone(),
            Self::Geometry(GeometryError::ModelDoc(p)) => p.clone(),
            other => vec![other.to_string()],
        }
    }

    #[must_use]
    pub fn is_model_doc(&self) -> bool {
        matches!(self, Self::Geometry(GeometryError::ModelDoc(_)))
    }

    #[must_use]
    pub fn is_value_error(&self) -> bool {
        match self {
            Self::Cae(e) => !e.is_convergence(),
            Self::Value { .. } | Self::Problems { .. } => true,
            Self::Geometry(g) => !matches!(g, GeometryError::Io(_) | GeometryError::Cancelled),
            _ => false,
        }
    }
}

pub type AResult<T> = Result<T, AuthoringError>;

#[must_use]
pub fn cae(e: implexity_core::error::CaeError) -> AuthoringError {
    use implexity_core::error::CaeError;
    match e {
        e @ CaeError::Recovery { .. } => AuthoringError::Cae(e),
        CaeError::Contract(m) => AuthoringError::value("CAEContractError", m),
        CaeError::Convergence(m) => AuthoringError::runtime("CAEConvergenceError", m),
        CaeError::NewtonConvergence(m) => AuthoringError::runtime("CAENewtonConvergenceError", m),
    }
}
