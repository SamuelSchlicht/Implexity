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



use implexity_core::contracts::ProviderCapabilities;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use crate::canonical::canonical_sha256;

pub const SOURCE_MANIFEST_SCHEMA: &str = "implexity-source-manifest/2";
pub const PREFREEZE_CLOSURE_SCHEMA: &str = "implexity-stage57-exact-baseline-prefreeze-closure/1";
pub const QUALIFICATION_POLICY_SCHEMA: &str = "implexity-exact-acceleration-qualification-benchmark-policy/1";
pub const QUALIFICATION_PARENT_SCOPE_SCHEMA: &str = "implexity-private-qualification-parent-scope/1";
pub const QUALIFICATION_CHILD_MESSAGE_SCHEMA: &str = "implexity-private-qualification-child-authority/1";
pub const QUALIFICATION_OPERATION_SCHEMA: &str = "implexity-private-qualification-operation/1";
pub const EXACT_DIRECT_POLICY: &str = "exact_direct_default";
pub const QUALIFICATION_ACCELERATED_POLICY: &str = "exact_krylov_bounded_sparse_ilu_v1";
pub const SOLVER_WORKSPACE_LIMIT_BYTES: i64 = 512 * 1024 * 1024;
pub const EXECUTABLE_RESIDENCY_LIMIT_BYTES: i64 = 12 * 1024 * 1024 * 1024;
pub const SPARSE_STRUCTURE_LIMIT_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const TRACE_AGGREGATE_CAP_BYTES: i64 = 256 * 1024 * 1024;
pub const ACCELERATED_WALL_LIMIT_SECONDS: f64 = 10_800.0;
pub const DIRECT_DIAGNOSTIC_WALL_LIMIT_SECONDS: f64 = 28_800.0;

pub const ROLE_POLICY: [(&str, &str, bool); 4] = [
    ("exact_direct_primal", "evaluate", false),
    ("exact_direct_sensitivity", "sensitivity", false),
    ("accelerated_exact_primal", "evaluate", true),
    ("accelerated_exact_sensitivity", "sensitivity", true),
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QualificationBenchmarkError {
    #[error("{0}")]
    Authority(String),
    #[error("{0}")]
    Replay(String),
    #[error("{0}")]
    Drift(String),
}

impl From<QualificationBenchmarkError> for CaeError {
    fn from(e: QualificationBenchmarkError) -> Self {
        CaeError::contract(e.to_string())
    }
}

#[must_use]
pub fn qualification_policy() -> Value {
    json!({
        "schema": QUALIFICATION_POLICY_SCHEMA,
        "status": "qualification_only_unpromoted",
        "canonical_promotion_authorized": false,
        "production_allowlist_modified": false,
        "accelerated_solver_policy": QUALIFICATION_ACCELERATED_POLICY,
        "features": [
            "bounded_worker_threads",
            "executable_residency",
            "nondefault_exact_profile",
            "parallel_logical_batch",
            "provider_owned_exact_preconditioner",
            "sparse_structure_reuse",
            "transactional_krylov_recycle",
        ],
        "resources": {
            "solver_workspace_limit_bytes": SOLVER_WORKSPACE_LIMIT_BYTES,
            "executable_residency_memory_limit_bytes": EXECUTABLE_RESIDENCY_LIMIT_BYTES,
            "sparse_structure_memory_limit_bytes": SPARSE_STRUCTURE_LIMIT_BYTES,
            "trace_aggregate_cap_bytes": TRACE_AGGREGATE_CAP_BYTES,
            "accelerated_wall_limit_milliseconds": 10_800_000,
            "direct_diagnostic_wall_limit_milliseconds": 28_800_000,
        },
        "cold_record": {
            "cross_record_compilation_reuse": false,
            "cross_record_state_reuse": false,
            "cross_record_residency_reuse": false,
            "within_operation_residency_required_when_accelerated": true,
            "within_operation_sparse_reuse_required_when_accelerated": true,
        },
    })
}

#[must_use]
pub fn qualification_policy_provenance() -> Map<String, Value> {
    let policy = qualification_policy();
    let mut out = policy.as_object().cloned().unwrap_or_default();
    out.insert("policy_sha256".into(), Value::String(canonical_sha256(&policy)));
    out
}


pub fn build_exact_acceleration_qualification_scope(record_role: &str) -> CaeResult<()> {
    if !ROLE_POLICY.iter().any(|(r, _, _)| *r == record_role) {
        return Err(
            QualificationBenchmarkError::Authority("qualification record role is not closed".into()).into()
        );
    }
    match implexity_io::bundle_resources::implementation_source_root() {
        Ok(root) => Err(QualificationBenchmarkError::Authority(format!(
            "source manifest at {} does not attest the running executable",
            root.display()
        ))
        .into()),
        Err(message) => Err(QualificationBenchmarkError::Authority(message).into()),
    }
}

#[must_use]
pub fn qualification_profile_visible(_candidate_policy: &str) -> bool {
    false
}

#[must_use]
pub fn parent_qualification_active() -> bool {
    false
}

#[must_use]
pub fn select_private_qualification_profile(
    _profiles: &Map<String, Value>,
    _default_policy: &str,
) -> Option<Value> {
    None
}

#[must_use]
pub fn prepare_qualification_child_launch() -> Option<Value> {
    None
}


pub fn build_qualification_child_execution() -> CaeResult<()> {
    Err(QualificationBenchmarkError::Authority(
        "qualification child execution lacks a verified parent scope".into(),
    )
    .into())
}


pub fn require_private_qualification_profile(_profile: &Value) -> CaeResult<()> {
    Err(QualificationBenchmarkError::Authority(
        "nondefault qualification profile lacks child authority".into(),
    )
    .into())
}


pub fn private_qualification_provider_effort_validation(
    _provider_name: &str,
    _capabilities: &ProviderCapabilities,
    _physics: &Value,
    _effort_binding: &Value,
    _candidates: Option<&Value>,
) -> CaeResult<Option<(Map<String, Value>, Value)>> {
    Ok(None)
}

