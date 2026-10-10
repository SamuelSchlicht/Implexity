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



use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::canonical::{canonical_sha256, is_digest, obj, s};
use crate::computation_effort::{
    EffectiveComputationEffort, ObservedComputationEffort, ResultWithTruth, TruthEnvelope,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityGateError {
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Binding(String),
    #[error("{0}")]
    Scope(String),
    #[error("{0}")]
    Replay(String),
}

type GateResult<T> = Result<T, AuthorityGateError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateAction {
    Accept,
    Export,
    AuthoritativeCache,
    McpOptimizationCommit,
}

impl CandidateAction {

    pub fn parse(value: &str) -> GateResult<Self> {
        match value {
            "accept" => Ok(Self::Accept),
            "export" => Ok(Self::Export),
            "authoritative_cache" => Ok(Self::AuthoritativeCache),
            "mcp_optimization_commit" => Ok(Self::McpOptimizationCommit),
            _ => Err(AuthorityGateError::Contract("unsupported candidate action".into())),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Export => "export",
            Self::AuthoritativeCache => "authoritative_cache",
            Self::McpOptimizationCommit => "mcp_optimization_commit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorizedAction {
    Candidate(CandidateAction),
    Qualification,
}

fn digest(name: &str, value: &str) -> GateResult<String> {
    if is_digest(value) {
        Ok(value.to_string())
    } else {
        Err(AuthorityGateError::Contract(format!("{name} must be a lowercase SHA-256 digest")))
    }
}

#[must_use]
pub fn provider_registry_binding_digest(truth: &TruthEnvelope) -> String {
    canonical_sha256(&obj([
        ("provider_registry_generation", Value::from(truth.provider_registry_generation)),
        ("provider_registry_fingerprint", s(&truth.provider_registry_fingerprint)),
        ("provider_profile_id", s(&truth.provider_profile_id)),
        ("normalized_profile_digest", s(&truth.normalized_profile_digest)),
    ]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    pub candidate_digest: String,
    pub design_state_digest: String,
    pub run_digest: String,
    pub source_manifest_digest: String,
    pub provider_registry_digest: String,
    pub guard_set_digest: String,
}

impl AuthorityContext {

    pub fn new(
        candidate_digest: &str,
        design_state_digest: &str,
        run_digest: &str,
        source_manifest_digest: &str,
        provider_registry_digest: &str,
        guard_set_digest: &str,
    ) -> GateResult<Self> {
        Ok(Self {
            candidate_digest: digest("candidate_digest", candidate_digest)?,
            design_state_digest: digest("design_state_digest", design_state_digest)?,
            run_digest: digest("run_digest", run_digest)?,
            source_manifest_digest: digest("source_manifest_digest", source_manifest_digest)?,
            provider_registry_digest: digest("provider_registry_digest", provider_registry_digest)?,
            guard_set_digest: digest("guard_set_digest", guard_set_digest)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedSinkEvidence {
    pub action: AuthorizedAction,
    pub context: AuthorityContext,
    pub result_digest: String,
    pub truth_digest: String,
    pub admission_proof_digest: String,
    pub qualification_proof_digest: Option<String>,
}

#[derive(Debug)]
pub struct AdmissionHandle {
    brand: u64,
    token: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Candidate,
    Qualification,
}

#[derive(Debug)]
struct Record {
    kind: Kind,
    scopes: Vec<AuthorizedAction>,
    context: AuthorityContext,
    effective: EffectiveComputationEffort,
    observed: ObservedComputationEffort,
    result_digest: String,
    truth_digest: String,
    admission_proof_digest: String,
    qualification_proof_digest: Option<String>,
}

#[derive(Debug)]
struct Ledger {
    brand: u64,
    next: AtomicU64,
    records: Mutex<HashMap<u64, Record>>,
}

static BRANDS: AtomicU64 = AtomicU64::new(1);

fn checked_exact_result(
    result: &ResultWithTruth,
    effective: &EffectiveComputationEffort,
    observed: &ObservedComputationEffort,
) -> GateResult<()> {
    result.truth().validate_against(effective, observed).map_err(|e| AuthorityGateError::Contract(e.0))?;
    if !result.truth().exact_truth() {
        return Err(AuthorityGateError::Contract("authoritative admission requires exact truth".into()));
    }
    let actual = result.payload_digest();
    if Some(&actual) != result.truth().result_digest.as_ref() {
        return Err(AuthorityGateError::Binding("canonical payload bytes do not match exact truth".into()));
    }
    Ok(())
}

impl Ledger {
    fn store(&self, record: Record) -> AdmissionHandle {
        let token = self.next.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut records) = self.records.lock() {
            records.insert(token, record);
        }
        AdmissionHandle { brand: self.brand, token }
    }
}

#[derive(Debug, Clone)]
pub struct CandidateAdmissionIssuer {
    ledger: Arc<Ledger>,
}

impl CandidateAdmissionIssuer {

    pub fn admit_candidate(
        &self,
        result: &ResultWithTruth,
        effective: &EffectiveComputationEffort,
        observed: &ObservedComputationEffort,
        context: &AuthorityContext,
        scopes: &[CandidateAction],
        acceptance_proof_digest: &str,
    ) -> GateResult<AdmissionHandle> {
        checked_exact_result(result, effective, observed)?;
        if context.provider_registry_digest != provider_registry_binding_digest(result.truth()) {
            return Err(AuthorityGateError::Binding(
                "provider registry binding does not match exact truth".into(),
            ));
        }
        let mut actions: Vec<AuthorizedAction> = Vec::new();
        for a in scopes {
            let a = AuthorizedAction::Candidate(*a);
            if !actions.contains(&a) {
                actions.push(a);
            }
        }
        if actions.is_empty() {
            return Err(AuthorityGateError::Scope("candidate admission requires at least one action".into()));
        }
        let proof = digest("acceptance_proof_digest", acceptance_proof_digest)?;
        Ok(self.ledger.store(Record {
            kind: Kind::Candidate,
            scopes: actions,
            context: context.clone(),
            effective: effective.clone(),
            observed: observed.clone(),
            result_digest: result.payload_digest(),
            truth_digest: result.truth().sha256(),
            admission_proof_digest: proof,
            qualification_proof_digest: None,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct QualificationAdmissionIssuer {
    ledger: Arc<Ledger>,
}

impl QualificationAdmissionIssuer {

    pub fn admit_qualification(
        &self,
        result: &ResultWithTruth,
        effective: &EffectiveComputationEffort,
        observed: &ObservedComputationEffort,
        context: &AuthorityContext,
        acceptance_proof_digest: &str,
        qualification_proof_digest: &str,
    ) -> GateResult<AdmissionHandle> {
        checked_exact_result(result, effective, observed)?;
        if context.provider_registry_digest != provider_registry_binding_digest(result.truth()) {
            return Err(AuthorityGateError::Binding(
                "provider registry binding does not match exact truth".into(),
            ));
        }
        let admission = digest("acceptance_proof_digest", acceptance_proof_digest)?;
        let qualification = digest("qualification_proof_digest", qualification_proof_digest)?;
        if qualification == admission {
            return Err(AuthorityGateError::Contract(
                "qualification proof must be distinct from candidate-acceptance proof".into(),
            ));
        }
        Ok(self.ledger.store(Record {
            kind: Kind::Qualification,
            scopes: vec![AuthorizedAction::Qualification],
            context: context.clone(),
            effective: effective.clone(),
            observed: observed.clone(),
            result_digest: result.payload_digest(),
            truth_digest: result.truth().sha256(),
            admission_proof_digest: admission,
            qualification_proof_digest: Some(qualification),
        }))
    }
}

#[derive(Debug, Clone)]
pub struct SinkAuthorityGate {
    ledger: Arc<Ledger>,
}

impl SinkAuthorityGate {
    #[allow(clippy::needless_pass_by_value, reason = "admissions are move-only: the handle is consumed")]
    fn consume<T>(
        &self,
        handle: AdmissionHandle,
        action: AuthorizedAction,
        result: &ResultWithTruth,
        expected_context: &AuthorityContext,
        sink: impl FnOnce(&[u8], &AuthorizedSinkEvidence) -> T,
        expected_kind: Kind,
    ) -> GateResult<T> {
        if handle.brand != self.ledger.brand {
            return Err(AuthorityGateError::Replay(
                "admission handle belongs to another authority boundary".into(),
            ));
        }
        let evidence = {
            let mut records = self
                .ledger
                .records
                .lock()
                .map_err(|_| AuthorityGateError::Replay("admission ledger is unavailable".into()))?;
            let Some(record) = records.get(&handle.token) else {
                return Err(AuthorityGateError::Replay("admission is unknown or already consumed".into()));
            };
            if record.kind != expected_kind {
                return Err(AuthorityGateError::Scope("admission kind does not authorize this sink".into()));
            }
            if !record.scopes.contains(&action) {
                return Err(AuthorityGateError::Scope("action is outside the admitted scope".into()));
            }
            if *expected_context != record.context {
                return Err(AuthorityGateError::Binding(
                    "current sink context differs from admission".into(),
                ));
            }
            checked_exact_result(result, &record.effective, &record.observed)?;
            if result.truth().sha256() != record.truth_digest {
                return Err(AuthorityGateError::Binding("truth evidence differs from admission".into()));
            }
            if result.payload_digest() != record.result_digest {
                return Err(AuthorityGateError::Binding("payload differs from admitted result".into()));
            }
            if provider_registry_binding_digest(result.truth()) != record.context.provider_registry_digest {
                return Err(AuthorityGateError::Binding("provider registry binding drifted".into()));
            }
            let evidence = AuthorizedSinkEvidence {
                action,
                context: record.context.clone(),
                result_digest: record.result_digest.clone(),
                truth_digest: record.truth_digest.clone(),
                admission_proof_digest: record.admission_proof_digest.clone(),
                qualification_proof_digest: record.qualification_proof_digest.clone(),
            };
            records.remove(&handle.token);
            evidence
        };
        Ok(sink(result.payload_bytes(), &evidence))
    }


    pub fn execute_candidate<T>(
        &self,
        handle: AdmissionHandle,
        action: CandidateAction,
        result: &ResultWithTruth,
        expected_context: &AuthorityContext,
        sink: impl FnOnce(&[u8], &AuthorizedSinkEvidence) -> T,
    ) -> GateResult<T> {
        self.consume(
            handle,
            AuthorizedAction::Candidate(action),
            result,
            expected_context,
            sink,
            Kind::Candidate,
        )
    }


    pub fn execute_qualification<T>(
        &self,
        handle: AdmissionHandle,
        result: &ResultWithTruth,
        expected_context: &AuthorityContext,
        sink: impl FnOnce(&[u8], &AuthorizedSinkEvidence) -> T,
    ) -> GateResult<T> {
        self.consume(
            handle,
            AuthorizedAction::Qualification,
            result,
            expected_context,
            sink,
            Kind::Qualification,
        )
    }
}

#[must_use]
pub fn create_private_authority_boundary()
-> (CandidateAdmissionIssuer, QualificationAdmissionIssuer, SinkAuthorityGate) {
    let ledger = Arc::new(Ledger {
        brand: BRANDS.fetch_add(1, Ordering::Relaxed),
        next: AtomicU64::new(1),
        records: Mutex::new(HashMap::new()),
    });
    (
        CandidateAdmissionIssuer { ledger: Arc::clone(&ledger) },
        QualificationAdmissionIssuer { ledger: Arc::clone(&ledger) },
        SinkAuthorityGate { ledger },
    )
}
