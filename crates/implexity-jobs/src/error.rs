// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::CaeError;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum JobError {
    #[error("{}", .0.message())]
    Cae(CaeError),
    #[error("{message}")]
    Recovery { cause: Box<Self>, report: serde_json::Value, message: String },
    #[error("{message}")]
    Other {
        class: String,
        message: String,
    },
    #[error("{message}")]
    Problems {
        class: String,
        problems: Vec<String>,
        message: String,
    },
}

impl JobError {
    #[must_use]
    pub fn solver_recovery(&self) -> Option<&serde_json::Value> {
        match self { Self::Recovery { report, .. } => Some(report), Self::Cae(e) => e.solver_recovery(), _ => None }
    }

    pub fn value(message: impl Into<String>) -> Self {
        Self::Other { class: "ValueError".into(), message: message.into() }
    }

    #[must_use]
    pub fn runtime(message: impl Into<String>) -> Self {
        Self::Other { class: "RuntimeError".into(), message: message.into() }
    }

    #[must_use]
    pub fn of(class: &str, message: impl Into<String>) -> Self {
        Self::Other { class: class.into(), message: message.into() }
    }

    #[must_use]
    pub fn contract(message: impl Into<String>) -> Self {
        Self::Cae(CaeError::contract(message))
    }

    #[must_use]
    pub fn optimize(problems: Vec<String>) -> Self {
        let message = problems.join("; ");
        Self::Problems { class: "OptimizeError".into(), problems, message }
    }

    #[must_use]
    pub fn optimize1(problem: impl Into<String>) -> Self {
        Self::optimize(vec![problem.into()])
    }

    #[must_use]
    pub fn model_doc(problems: Vec<String>) -> Self {
        let message = format!("model document rejected:\n  {}", problems.join("\n  "));
        Self::Problems { class: "ModelDocError".into(), problems, message }
    }

    #[must_use]
    pub fn python_class(&self) -> &str {
        match self {
            Self::Recovery { cause, .. } => cause.python_class(),
            Self::Cae(e) => e.python_class(),
            Self::Other { class, .. } | Self::Problems { class, .. } => class,
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Recovery { message, .. } => message.clone(),
            Self::Cae(e) => e.message().to_string(),
            Self::Other { message, .. } | Self::Problems { message, .. } => message.clone(),
        }
    }

    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        match self {
            Self::Problems { problems, .. } => problems.clone(),
            other => vec![other.message()],
        }
    }

    #[must_use]
    pub fn is_value_error(&self) -> bool {
        match self {
            Self::Recovery { cause, .. } => cause.is_value_error(),
            Self::Cae(e) => !e.is_convergence(),
            Self::Problems { .. } => true,
            Self::Other { class, .. } => matches!(
                class.as_str(),
                "ValueError"
                    | "ArtifactError"
                    | "StudyError"
                    | "WorkingResultError"
                    | "MatchingTimeGuessError"
                    | "MissingMatchingTimeGuessError"
                    | "ManagedEvaluationContractError"
                    | "HeavyRuntimeContractError"
                    | "BindingError"
                    | "ModelError"

                    | "ExprError"
                    | "BoundViolation"
                    | "JSONDecodeError"
                    | "DragError"
                    | "EngineeringGlyphError"
                    | "EntitySpecError"
                    | "FieldInteractionError"
                    | "SculptError"
                    | "SpatialAuthoringError"
                    | "SpatialSelectionError"
                    | "SpatialValueError"
                    | "SurfacePatchError"
            ),
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        format!("{}: {}", self.python_class(), self.message())
    }
}

impl From<CaeError> for JobError {
    fn from(e: CaeError) -> Self {
        Self::Cae(e)
    }
}

impl From<crate::artifacts::ArtifactError> for JobError {
    fn from(e: crate::artifacts::ArtifactError) -> Self {
        Self::of(e.python_class(), e.to_string())
    }
}

impl From<crate::epoch_fields::EpochFieldError> for JobError {
    fn from(e: crate::epoch_fields::EpochFieldError) -> Self {
        match e {
            crate::epoch_fields::EpochFieldError::Artifact(a) => a.into(),
            crate::epoch_fields::EpochFieldError::MissingField(f) => {
                Self::of("KeyError", implexity_core::py_repr::repr_str(&f))
            }
        }
    }
}

impl From<crate::heavy_runtime::HeavyRuntimeContractError> for JobError {
    fn from(e: crate::heavy_runtime::HeavyRuntimeContractError) -> Self {
        Self::of("HeavyRuntimeContractError", e.0)
    }
}

impl From<crate::managed_evaluation::ManagedEvaluationError> for JobError {
    fn from(e: crate::managed_evaluation::ManagedEvaluationError) -> Self {
        Self::of(e.python_class(), e.to_string())
    }
}

impl From<implexity_solve::matching_time_guess::GuessError> for JobError {
    fn from(e: implexity_solve::matching_time_guess::GuessError) -> Self {
        match e {
            implexity_solve::matching_time_guess::GuessError::Invalid(m) => {
                Self::of("MatchingTimeGuessError", m)
            }
            implexity_solve::matching_time_guess::GuessError::Missing(m) => {
                Self::of("MissingMatchingTimeGuessError", m)
            }
        }
    }
}

impl From<implexity_geometry::GeometryError> for JobError {
    fn from(e: implexity_geometry::GeometryError) -> Self {
        use implexity_geometry::GeometryError as G;
        match e {
            G::ModelDoc(problems) => Self::model_doc(problems),
            G::Model(m) | G::Transpile { message: m, .. } => Self::of("ModelError", m),
            G::Expr(m) => Self::of("ExprError", m),
            G::BoundViolation(m) => Self::of("BoundViolation", m),
            G::Value(m) => Self::value(m),
            G::Key(k) => Self::of("KeyError", implexity_geometry::pyfmt::str_repr(&k)),
            G::Cancelled => Self::of("Cancelled", "cancelled"),
            G::Io(m) => Self::of("OSError", m),
        }
    }
}

impl From<implexity_authoring::error::AuthoringError> for JobError {
    fn from(e: implexity_authoring::error::AuthoringError) -> Self {
        use implexity_authoring::error::AuthoringError as A;
        match e {
            A::Cae(e) => Self::Cae(e),
            A::Geometry(g) => Self::from(g),
            A::Problems { class, problems } => {
                let message = A::Problems { class, problems: problems.clone() }.to_string();
                Self::Problems { class: class.into(), problems, message }
            }
            other => Self::of(other.class(), other.to_string()),
        }
    }
}

impl From<std::io::Error> for JobError {
    fn from(e: std::io::Error) -> Self {
        let class = match e.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            std::io::ErrorKind::AlreadyExists => "FileExistsError",
            _ => "OSError",
        };
        Self::of(class, e.to_string())
    }
}

pub type JobResult<T> = Result<T, JobError>;

