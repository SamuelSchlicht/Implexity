// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::contracts::{Evaluation, ProviderCapabilities, ProviderDescriptor, ProviderProblem};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::design_identity;
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::{AdmissionReply, CandidateDesign, DesignOp, LifecycleDeclaration};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

pub use implexity_optim::design::NamedArrays as Design;
pub use implexity_optim::provider_ops::{DesignOperations, DesignSensitivities, DesignSensitivity};

pub trait LbmOperations: Send + Sync {

    fn preflight_value(&self, problem: &Value, design: &Design) -> CaeResult<Map<String, Value>>;


    fn evaluate_value(&self, problem: &Value, design: &Design) -> CaeResult<Evaluation>;


    fn sensitivities_value(
        &self,
        problem: &Value,
        design: &Design,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities>;


    fn admission_value(&self, problem: &Value, candidate: &Design) -> CaeResult<Map<String, Value>>;
}


pub fn single_sensitivity<P: LbmOperations + ?Sized>(
    provider: &P,
    problem: &Value,
    design: &Design,
    response: &str,
) -> CaeResult<DesignSensitivity> {
    let mut out = provider.sensitivities_value(problem, design, &[response.to_string()])?;
    let value = out.responses.get(response).copied().unwrap_or(f64::NAN);
    let gradients = out.gradients.remove(response).unwrap_or_default();
    Ok(DesignSensitivity { value, gradients, diagnostics: out.diagnostics })
}

#[macro_export]
macro_rules! lbm_design_operations {
    ($ty:ty, $coordinate:expr) => {
        impl $crate::design_ops::DesignOperations for $ty {
            fn provides(&self, op: implexity_optim::provider_ops::DesignOp) -> bool {
                $crate::design_ops::provides(op)
            }

            fn evaluate_design(
                &self,
                problem: &implexity_core::contracts::ProviderProblem,
                design: &$crate::design_ops::Design,
                operating_point: usize,
            ) -> implexity_core::CaeResult<implexity_core::contracts::Evaluation> {
                $crate::design_ops::nominal(operating_point)?;
                <Self as $crate::design_ops::LbmOperations>::evaluate_value(
                    self,
                    $crate::design_ops::problem_value(problem)?,
                    design,
                )
            }

            fn preflight_design(
                &self,
                problem: &implexity_core::contracts::ProviderProblem,
                design: &$crate::design_ops::Design,
            ) -> implexity_core::CaeResult<serde_json::Map<String, serde_json::Value>> {
                <Self as $crate::design_ops::LbmOperations>::preflight_value(
                    self,
                    $crate::design_ops::problem_value(problem)?,
                    design,
                )
            }

            fn sensitivity_design(
                &self,
                problem: &implexity_core::contracts::ProviderProblem,
                design: &$crate::design_ops::Design,
                response: &str,
                operating_point: usize,
            ) -> implexity_core::CaeResult<$crate::design_ops::DesignSensitivity> {
                $crate::design_ops::nominal(operating_point)?;
                $crate::design_ops::single_sensitivity(
                    self,
                    $crate::design_ops::problem_value(problem)?,
                    design,
                    response,
                )
            }

            fn sensitivities_design(
                &self,
                problem: &implexity_core::contracts::ProviderProblem,
                design: &$crate::design_ops::Design,
                responses: &[String],
                operating_point: usize,
            ) -> implexity_core::CaeResult<$crate::design_ops::DesignSensitivities> {
                $crate::design_ops::nominal(operating_point)?;
                <Self as $crate::design_ops::LbmOperations>::sensitivities_value(
                    self,
                    $crate::design_ops::problem_value(problem)?,
                    design,
                    responses,
                )
            }

            fn candidate_admission(
                &self,
                op: implexity_optim::provider_ops::DesignOp,
                problem: &implexity_core::contracts::ProviderProblem,
                current: &implexity_optim::provider_ops::CandidateDesign,
                trial: &implexity_optim::provider_ops::CandidateDesign,
            ) -> implexity_core::CaeResult<implexity_optim::provider_ops::AdmissionReply> {
                if op != implexity_optim::provider_ops::DesignOp::CandidateDesignAdmission {
                    return Err(implexity_core::CaeError::contract(format!(
                        "provider operation {:?} is unavailable",
                        op.name()
                    )));
                }
                let problem = $crate::design_ops::problem_value(problem)?;
                $crate::design_ops::candidate_admission(
                    current,
                    trial,
                    |candidate| {
                        <Self as $crate::design_ops::LbmOperations>::admission_value(self, problem, candidate)
                    },
                    "Fixed LBM trajectory admitted",
                )
            }

            fn optimizer_lifecycle(
                &self,
                _problem: Option<&implexity_core::contracts::ProviderProblem>,
            ) -> implexity_core::CaeResult<implexity_optim::provider_ops::LifecycleDeclaration> {
                $crate::design_ops::lbm_lifecycle($coordinate)
            }
        }
    };
}

pub const PROVIDED: [DesignOp; 6] = [
    DesignOp::PreflightDesign,
    DesignOp::EvaluateDesign,
    DesignOp::SensitivityDesign,
    DesignOp::SensitivitiesDesign,
    DesignOp::CandidateDesignAdmission,
    DesignOp::OptimizerLifecycle,
];

#[must_use]
pub fn provides(op: DesignOp) -> bool {
    PROVIDED.contains(&op)
}


pub fn lbm_lifecycle(coordinate: &str) -> CaeResult<LifecycleDeclaration> {
    Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
        vec![coordinate.to_string()],
        "sensitivity_design",
        "evaluate_design",
        Some("candidate_design_admission"),
        None,
        false,
        false,
    )?))
}


pub fn nominal(operating_point: usize) -> CaeResult<()> {
    if operating_point == 0 {
        Ok(())
    } else {
        Err(CaeError::contract(
            "provider does not implement operating_point dispatch; refusing to repeat the nominal state under another label",
        ))
    }
}

fn named(design: &CandidateDesign) -> CaeResult<&Design> {
    match design {
        CandidateDesign::Named(d) => Ok(d),
        CandidateDesign::Array(_) => {
            Err(CaeError::contract("candidate_design_admission requires named design mappings"))
        }
    }
}


pub fn candidate_admission(
    current: &CandidateDesign,
    candidate: &CandidateDesign,
    parts: impl FnOnce(&Design) -> CaeResult<Map<String, Value>>,
    reason: &str,
) -> CaeResult<AdmissionReply> {
    let (current, candidate) = (named(current)?, named(candidate)?);
    let mut out = Map::new();
    out.insert("current_design_state_id".into(), json!(design_identity(current)?));
    out.insert("candidate_design_state_id".into(), json!(design_identity(candidate)?));
    match parts(candidate) {
        Err(error) => {
            out.insert("allow".into(), json!(false));
            out.insert("reason".into(), json!(error.message()));
        }
        Ok(diagnostics) => {
            out.insert("allow".into(), json!(true));
            out.insert("reason".into(), json!(reason));
            out.insert("diagnostics".into(), Value::Object(diagnostics));
        }
    }
    Ok(AdmissionReply::Record(Value::Object(out)))
}


pub fn presentation(
    descriptor: &ProviderDescriptor,
    editor: Value,
    execution: &str,
) -> CaeResult<ProviderCapabilities> {
    Ok(ProviderCapabilities::Descriptor(Box::new(descriptor.clone().with_presentation(editor, execution)?)))
}


pub fn check_responses(names: &[String], known: &[&str], message: &str) -> CaeResult<()> {
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    if names.is_empty() || unique.len() != names.len() || names.iter().any(|n| !known.contains(&n.as_str())) {
        return Err(CaeError::contract(message));
    }
    Ok(())
}


pub fn single_coordinate<'a>(
    design: &'a Design,
    coordinate: &str,
    message: &str,
) -> CaeResult<&'a ArrayD<f64>> {
    match design.get(coordinate) {
        Some(a) if design.len() == 1 => Ok(a),
        _ => Err(CaeError::contract(message)),
    }
}


pub fn problem_value(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("LBM providers require their own normalised problem mapping"))
}


pub fn array(values: Vec<f64>, shape: &[usize]) -> CaeResult<ArrayD<f64>> {
    ArrayD::from_shape_vec(ndarray::IxDyn(shape), values)
        .map_err(|e| CaeError::contract(format!("internal array shape error: {e}")))
}
