// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
