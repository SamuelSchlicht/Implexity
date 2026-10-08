// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::Path;

use serde_json::Value;

use implexity_runtime::qualification_operation_identity::{
    QualificationOperationIdentityError, SourceBoundInputs,
    derive_source_bound_qualification_operation_identity as derive,
};

use crate::unified_history_preconditioner::{
    DIRECT_POLICY, SPARSE_ILU_KRYLOV_POLICY, UnifiedHistoryExactProfile,
};

#[derive(Debug, Clone, Copy)]
pub struct QualificationInputs<'a> {
    pub record_role: &'a str,
    pub run_id: &'a str,
    pub release_candidate_id: &'a str,
    pub source_manifest_path: &'a Path,
    pub source_manifest_file_sha256: &'a str,
    pub source_manifest_aggregate_sha256: &'a str,
    pub scientific_scope_sha256: &'a str,
    pub frozen_input_root: &'a Path,
}


pub fn role_profile(record_role: &str) -> Result<Value, QualificationOperationIdentityError> {
    if !matches!(
        record_role,
        "exact_direct_primal"
            | "exact_direct_sensitivity"
            | "accelerated_exact_primal"
            | "accelerated_exact_sensitivity"
    ) {
        return Err(QualificationOperationIdentityError("qualification record role is not closed".into()));
    }
    let policy =
        if record_role.starts_with("accelerated_") { SPARSE_ILU_KRYLOV_POLICY } else { DIRECT_POLICY };
    Ok(UnifiedHistoryExactProfile { solver_policy: policy.into(), ..UnifiedHistoryExactProfile::default() }
        .to_wire())
}


pub fn derive_source_bound_qualification_operation_identity(
    inputs: &QualificationInputs<'_>,
) -> Result<Value, QualificationOperationIdentityError> {
    let profile = role_profile(inputs.record_role)?;
    derive(&SourceBoundInputs {
        record_role: inputs.record_role,
        run_id: inputs.run_id,
        release_candidate_id: inputs.release_candidate_id,
        source_manifest_path: inputs.source_manifest_path,
        source_manifest_file_sha256: inputs.source_manifest_file_sha256,
        source_manifest_aggregate_sha256: inputs.source_manifest_aggregate_sha256,
        scientific_scope_sha256: inputs.scientific_scope_sha256,
        frozen_input_root: inputs.frozen_input_root,
        provider_id: super::unified_history::NAME,
        exact_solver_profile: &profile,
    })
}

