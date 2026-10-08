// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::contracts::{
    Evaluation, FieldValue, MatchingTimeNewtonGuess, ProviderProblem, Sensitivity,
};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::numeric::{array_to_value, float_value};
use implexity_optim::optimizer::DirectGradientRun;
use implexity_optim::provider_ops::{CachedEvaluation, DesignSensitivities, DesignSensitivity};
use serde_json::{Map, Value};

#[must_use]
pub fn field_value(value: &FieldValue) -> Value {
    match value {
        FieldValue::Array(a) => array_to_value(a),
        FieldValue::Json(v) => v.clone(),
    }
}

#[must_use]
pub fn fields_value(fields: &BTreeMap<String, FieldValue>) -> Value {
    Value::Object(fields.iter().map(|(k, v)| (k.clone(), field_value(v))).collect())
}

#[must_use]
pub fn responses_value(responses: &BTreeMap<String, f64>) -> Value {
    Value::Object(responses.iter().map(|(k, v)| (k.clone(), float_value(*v))).collect())
}

#[must_use]
pub fn evaluation_value(schema: Option<&str>, e: &Evaluation) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(s) = schema {
        out.insert("schema".into(), Value::String(s.into()));
    }
    out.insert("provider".into(), Value::String(e.provider.clone()));
    out.insert("responses".into(), responses_value(&e.responses));
    out.insert("diagnostics".into(), Value::Object(e.diagnostics.clone()));
    out.insert("fields".into(), fields_value(&e.fields));
    out
}

#[must_use]
pub fn design_sensitivity_value(s: &DesignSensitivity) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("value".into(), float_value(s.value));
    out.insert("gradients".into(), s.gradients.to_wire());
    out.insert("diagnostics".into(), Value::Object(s.diagnostics.clone()));
    out
}

#[must_use]
pub fn design_sensitivities_value(s: &DesignSensitivities) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("responses".into(), responses_value(&s.responses));
    out.insert(
        "gradients".into(),
        Value::Object(s.gradients.iter().map(|(k, v)| (k.clone(), v.to_wire())).collect()),
    );
    out.insert("diagnostics".into(), Value::Object(s.diagnostics.clone()));
    out
}

#[derive(Debug, Clone)]
pub enum ExecutionOutput {
    Evaluation(Evaluation),
    Sensitivity(Sensitivity),
    DesignSensitivity(DesignSensitivity),
    Sensitivities(DesignSensitivities),
    Run(DirectGradientRun),
    Guess(MatchingTimeNewtonGuess),
    Cached(CachedEvaluation),
    Json(Map<String, Value>),
}

impl ExecutionOutput {
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Evaluation(e) | Self::Cached(CachedEvaluation::Available(e)) => {
                Value::Object(evaluation_value(None, e))
            }
            Self::Sensitivity(s) => {
                let mut out = Map::new();
                out.insert("provider".into(), Value::String(s.provider.clone()));
                out.insert("response".into(), Value::String(s.response.clone()));
                out.insert("value".into(), float_value(s.value));
                out.insert("gradient".into(), array_to_value(&s.gradient));
                out.insert("diagnostics".into(), Value::Object(s.diagnostics.clone()));
                Value::Object(out)
            }
            Self::DesignSensitivity(s) => Value::Object(design_sensitivity_value(s)),
            Self::Sensitivities(s) => Value::Object(design_sensitivities_value(s)),
            Self::Run(r) => Value::Object(r.record.clone()),
            Self::Guess(g) => {
                let mut out = Map::new();
                out.insert("provider_identity".into(), Value::Object(g.provider_identity().clone()));
                out.insert("provenance".into(), Value::Object(g.provenance().clone()));
                out.insert(
                    "states".into(),
                    Value::Array(
                        g.states().iter().map(|s| implexity_optim::numeric::float_list(s)).collect(),
                    ),
                );
                Value::Object(out)
            }
            Self::Cached(CachedEvaluation::Unavailable(m)) | Self::Json(m) => Value::Object(m.clone()),
        }
    }


    pub fn into_json(self, label: &str) -> CaeResult<Map<String, Value>> {
        match self {
            Self::Json(m) => Ok(m),
            other => {
                Err(CaeError::contract(format!("{label}: expected a mapping result, got {}", other.kind())))
            }
        }
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Evaluation(_) | Self::Cached(_) => "Evaluation",
            Self::Sensitivity(_) => "Sensitivity",
            Self::DesignSensitivity(_) | Self::Sensitivities(_) | Self::Run(_) | Self::Json(_) => "dict",
            Self::Guess(_) => "MatchingTimeNewtonGuess",
        }
    }
}

#[must_use]
pub fn json_problem(value: Value) -> ProviderProblem {
    Arc::new(value)
}

#[must_use]
pub fn problem_json(problem: &ProviderProblem) -> Option<&Value> {
    problem.downcast_ref::<Value>()
}

#[must_use]
pub fn external_port_wire(value: &implexity_core::orchestration::ExternalPortValue) -> Value {
    let mut out = Map::new();
    out.insert("port".into(), value.port.to_value());
    out.insert("value".into(), value.value.clone());
    out.insert("source_id".into(), Value::String(value.source_id.clone()));
    Value::Object(out)
}
