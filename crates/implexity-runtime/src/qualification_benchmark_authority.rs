// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

