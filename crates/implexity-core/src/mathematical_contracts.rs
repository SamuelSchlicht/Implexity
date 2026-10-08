// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use crate::contracts::{CaeProvider, ProviderProblem, require_contract_bool};
use crate::error::CaeResult;
use crate::pyobj::{list_repr, py_str, truthy};

pub const ADVANCED_STRUCTURES: [&str; 9] = [
    "generalized_eigenproblem",
    "complementarity_or_contact",
    "hyperbolic_conservation",
    "field_network_coupling",
    "mesh_transfer_or_remeshing",
    "nonhermitian_eigenproblem",
    "dae_or_saddle_point",
    "nonmatching_interface",
    "event_partition",
];

const POLICIES: [(&str, &str); 9] = [
    ("eigen_multiplicity_policy", "eigenMultiplicityPolicy"),
    ("nonsmooth_policy", "nonsmoothPolicy"),
    ("discontinuity_policy", "discontinuityPolicy"),
    ("transfer_policy", "transferPolicy"),
    ("field_network_policy", "fieldNetworkPolicy"),
    ("exceptional_point_policy", "exceptionalPointPolicy"),
    ("dae_nullspace_policy", "daeNullspacePolicy"),
    ("nonmatching_interface_policy", "nonmatchingInterfacePolicy"),
    ("event_partition_policy", "eventPartitionPolicy"),
];

#[derive(Debug, Clone, PartialEq)]
pub struct MathematicalStructureDeclaration {
    pub provider: String,
    pub structures: Vec<String>,
    pub policies: Vec<Option<Value>>,
    pub exact_design_derivatives: bool,
    pub exact_state_transpose: bool,
}

impl MathematicalStructureDeclaration {

    pub fn from_value(value: &Value) -> CaeResult<Self> {
        let Some(m) = value.as_object() else {
            return Err(crate::error::CaeError::contract("mathematical declaration must be a mapping"));
        };
        let policies = POLICIES
            .iter()
            .map(|(snake, camel)| match m.get(*snake) {
                Some(v) => Some(v.clone()),
                None => m.get(*camel).cloned(),
            })
            .map(|v| v.filter(|v| !v.is_null()))
            .collect();
        let claim = |snake: &str, camel: &str, label: &str| -> CaeResult<bool> {
            let v = m.get(snake).or_else(|| m.get(camel)).cloned().unwrap_or(Value::Bool(true));
            Ok(require_contract_bool(&v, label, false)?.unwrap_or(true))
        };
        Ok(Self {
            provider: m.get("provider").filter(|v| truthy(v)).map(py_str).unwrap_or_default(),
            structures: m
                .get("structures")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(py_str).collect())
                .unwrap_or_default(),
            policies,
            exact_design_derivatives: claim(
                "exact_design_derivatives",
                "exactDesignDerivatives",
                "mathematical exact_design_derivatives",
            )?,
            exact_state_transpose: claim(
                "exact_state_transpose",
                "exactStateTranspose",
                "mathematical exact_state_transpose",
            )?,
        })
    }

    fn policy(&self, name: &str) -> Option<&str> {
        POLICIES
            .iter()
            .position(|(s, _)| *s == name)
            .and_then(|i| self.policies[i].as_ref())
            .and_then(Value::as_str)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("provider".into(), json!(self.provider));
        m.insert("structures".into(), json!(self.structures));
        for ((name, _), v) in POLICIES.iter().zip(&self.policies) {
            m.insert((*name).into(), v.clone().unwrap_or(Value::Null));
        }
        m.insert("exact_design_derivatives".into(), json!(self.exact_design_derivatives));
        m.insert("exact_state_transpose".into(), json!(self.exact_state_transpose));
        Value::Object(m)
    }
}

const POLICY_CHECKS: [(&str, &str, &[&str], &str, &str); 9] = [
    (
        "generalized_eigenproblem",
        "eigen_multiplicity_policy",
        &["cluster", "tracked_simple_modes", "refuse"],
        "EIGEN_MULTIPLICITY_POLICY_MISSING",
        "eigenproblems require an explicit mode-multiplicity/crossing policy",
    ),
    (
        "complementarity_or_contact",
        "nonsmooth_policy",
        &["smooth_regularization", "generalized_derivative", "event_partition", "refuse"],
        "NONSMOOTH_POLICY_MISSING",
        "non-smooth physics requires an explicit derivative/event policy",
    ),
    (
        "hyperbolic_conservation",
        "discontinuity_policy",
        &["differentiable_flux", "shock_tracking", "frozen_shock", "refuse"],
        "DISCONTINUITY_POLICY_MISSING",
        "hyperbolic conservation laws require a discontinuity derivative policy",
    ),
    (
        "mesh_transfer_or_remeshing",
        "transfer_policy",
        &["conservative_adjoint", "exact_remesh_adjoint", "refuse"],
        "TRANSFER_ADJOINT_POLICY_MISSING",
        "mesh transfer/remeshing requires an exact conservative adjoint policy",
    ),
    (
        "field_network_coupling",
        "field_network_policy",
        &["monolithic", "iterative_conservative", "refuse"],
        "FIELD_NETWORK_POLICY_MISSING",
        "field-network coupling requires a conservative solve policy",
    ),
    (
        "nonhermitian_eigenproblem",
        "exceptional_point_policy",
        &["biorthogonal_simple", "cluster_subspace", "refuse"],
        "EXCEPTIONAL_POINT_POLICY_MISSING",
        "non-Hermitian eigenproblems require an exceptional-point policy",
    ),
    (
        "dae_or_saddle_point",
        "dae_nullspace_policy",
        &["explicit_gauge", "nullspace_projection", "refuse"],
        "DAE_NULLSPACE_POLICY_MISSING",
        "DAE/saddle-point systems require a gauge/nullspace policy",
    ),
    (
        "nonmatching_interface",
        "nonmatching_interface_policy",
        &["conservative_mortar", "conservative_projection", "refuse"],
        "NONMATCHING_INTERFACE_POLICY_MISSING",
        "nonmatching interfaces require a conservative transfer policy",
    ),
    (
        "event_partition",
        "event_partition_policy",
        &["relinearize", "generalized_derivative", "refuse"],
        "EVENT_PARTITION_POLICY_MISSING",
        "event-driven physics requires an event partition derivative policy",
    ),
];

#[must_use]
pub fn validate_mathematical_structure(
    d: &MathematicalStructureDeclaration,
    for_optimization: bool,
    active_design_coordinates: &[String],
) -> Value {
    let mut errors: Vec<Value> = Vec::new();
    let issue =
        |errors: &mut Vec<Value>, code: &str, msg: String| errors.push(json!({"code": code, "message": msg}));
    if d.provider.is_empty() {
        issue(&mut errors, "MATH_PROVIDER_MISSING", "provider id is required".into());
    }
    let unique: std::collections::BTreeSet<&String> = active_design_coordinates.iter().collect();
    if active_design_coordinates.iter().any(String::is_empty)
        || unique.len() != active_design_coordinates.len()
    {
        issue(
            &mut errors,
            "DESIGN_COORDINATES_INVALID",
            "active design-coordinate ids must be non-empty and unique".into(),
        );
    }
    let mut unknown: Vec<&String> =
        d.structures.iter().filter(|s| !ADVANCED_STRUCTURES.contains(&s.as_str())).collect();
    unknown.sort();
    unknown.dedup();
    if !unknown.is_empty() {
        issue(
            &mut errors,
            "UNKNOWN_MATHEMATICAL_STRUCTURE",
            format!("unknown structures: {}", list_repr(&unknown)),
        );
    }
    let has = |s: &str| d.structures.iter().any(|x| x == s);
    for (structure, policy, allowed, code, message) in POLICY_CHECKS {
        if has(structure) && !d.policy(policy).is_some_and(|p| allowed.contains(&p)) {
            issue(&mut errors, code, message.into());
        }
    }
    if for_optimization {
        if POLICIES.iter().any(|(name, _)| d.policy(name) == Some("refuse")) {
            issue(
                &mut errors,
                "ADVANCED_STRUCTURE_NOT_OPTIMIZABLE",
                "at least one active advanced structure explicitly refuses direct-gradient optimization"
                    .into(),
            );
        }
        if !d.exact_design_derivatives {
            issue(
                &mut errors,
                "EXACT_DESIGN_DERIVATIVE_MISSING",
                "direct-gradient optimization requires exact design derivatives".into(),
            );
        }
        if !d.exact_state_transpose {
            issue(
                &mut errors,
                "EXACT_TRANSPOSE_MISSING",
                "implicit direct-gradient optimization requires an exact state-Jacobian transpose path"
                    .into(),
            );
        }
    }
    json!({
        "schema": "implexity-mathematical-structure-report/2",
        "provider": d.provider,
        "structures": d.structures,
        "active_design_coordinates": active_design_coordinates,
        "ok": errors.is_empty(),
        "errors": errors,
        "warnings": [],
        "declaration": d.to_value(),
    })
}


pub fn validate_provider_mathematics(
    provider: &dyn CaeProvider,
    problem: Option<&ProviderProblem>,
    for_optimization: bool,
) -> CaeResult<Option<Value>> {
    let Some(raw) = provider.mathematical_declaration(problem) else { return Ok(None) };
    let coordinates = provider.capabilities()?.design_coordinates();
    let d = MathematicalStructureDeclaration::from_value(&raw)?;
    Ok(Some(validate_mathematical_structure(&d, for_optimization, &coordinates)))
}
