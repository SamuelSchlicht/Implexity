// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use implexity_core::json::{DumpOptions, dumps};
use serde_json::{Value, json};

use crate::error::{AgentError, AgentResult};
use crate::host::{ManagedCapsule, ManagedControl, ManagedStatus, ManagedSupervisor};
use crate::pyval::is_hex;
use crate::runtime::{AgentManager, lock};

const PUBLIC_ENVELOPE_SCHEMA: &str = "implexity-managed-evaluation-inspection/1";
const AUTHORITATIVE_STATUS_SCHEMA: &str = "implexity-managed-authoritative-status/1";
const COMMIT_RECORD_SCHEMA: &str = "implexity-managed-authoritative-commit/1";
const PRIVATE_ROOT: &str = ".implexity-private-managed-evaluations";



pub fn operation_id(raw: Option<&Value>) -> AgentResult<String> {
    raw.and_then(Value::as_str)
        .filter(|s| is_hex(s, 48))
        .map(str::to_owned)
        .ok_or_else(|| {
            AgentError::contract(
                "managed evaluation requires a 48-character lowercase hexadecimal operation_id",
            )
        })
}

struct Record {
    operation_id: String,
    operation_kind: String,
    owner_instance_digest: String,
    frozen_case_digest: String,
    frozen_case_revision: String,
    frozen_model_digest: String,
    frozen_package_digest: String,
    frozen_package_snapshot: Value,
    control: Arc<dyn ManagedControl>,
    capsule: Arc<dyn ManagedCapsule>,
    commit: Mutex<Commit>,
}

#[derive(Default)]
struct Commit {
    finalize_started: bool,
    cancellation_requested: bool,
    state: String,
    reason: Option<String>,
    terminal_result: Option<Value>,
}

#[derive(Default)]
pub struct ManagedState {
    records: Mutex<BTreeMap<String, Arc<Record>>>,
    starting: Mutex<bool>,
    supervisor: Mutex<Option<Arc<dyn ManagedSupervisor>>>,
}

fn canonical_digest(payload: &Value) -> String {
    implexity_io::digest::sha256_hex(dumps(payload, &DumpOptions::canonical()).as_bytes())
}

fn public_envelope(status: &Value, result: Option<Value>) -> Value {
    let mut out = json!({"schema": PUBLIC_ENVELOPE_SCHEMA, "status": status});
    if let Some(r) = result {
        out["exact_result"] = r;
    }
    out
}

impl AgentManager {
    fn supervisor(&self) -> AgentResult<Arc<dyn ManagedSupervisor>> {
        let mut slot = lock(&self.managed.supervisor);
        if let Some(s) = slot.as_ref() {
            return Ok(Arc::clone(s));
        }
        let root = std::path::absolute(&self.state_dir)
            .unwrap_or_else(|_| self.state_dir.clone())
            .join(PRIVATE_ROOT);
        let s = self.host.managed_supervisor(&root)?;
        *slot = Some(Arc::clone(&s));
        Ok(s)
    }



    pub fn initialize_managed_supervision(&self) -> AgentResult<()> {
        self.supervisor().map(|_| ())
    }

    fn service_owner_digest(&self) -> String {
        let root =
            std::fs::canonicalize(&self.state_dir).unwrap_or_else(|_| self.state_dir.clone());
        canonical_digest(&json!({"state_root": root.to_string_lossy()}))
    }

    fn case_owner_snapshot(&self) -> AgentResult<(String, String)> {
        let (document, revision) = match self.host.case_snapshot() {
            None => (Value::Null, None),
            Some((doc, rev)) => {
                let rev = rev
                    .or_else(|| doc.get("revision").cloned())
                    .filter(|v| !v.is_null());
                (doc, rev)
            }
        };
        let digest = canonical_digest(&json!({"case": document}));
        let token = match revision {
            None => format!("content:{digest}"),
            Some(Value::String(s)) => s,
            Some(v) if crate::pyval::is_int(&v) => v.to_string(),
            Some(_) => {
                return Err(AgentError::refused(
                    "the authoritative case revision is not canonical",
                ));
            }
        };
        Ok((digest, token))
    }

    fn model_owner_digest(&self) -> String {
        let identity = self
            .host
            .model()
            .and_then(|m| {
                let _live = m.live_lock();
                let s = m.status().ok()?;
                Some(json!({"structure_id": s.structure_id, "content_id": s.content_id, "sha256": m.document_sha256()}))
            })
            .unwrap_or_else(|| json!({"structure_id": null, "content_id": null, "sha256": null}));
        canonical_digest(&identity)
    }

    fn package_owner_snapshot() -> AgentResult<(Value, String)> {
        let registries = implexity_core::registries::global();
        let status = implexity_core::packages::global().status()?;
        let providers = registries.providers.snapshot().token;
        let addins = registries.addins.snapshot().token;
        let rules = registries.sufficiency.snapshot().token;
        let extensions = registries.extensions.snapshot().token;
        let snapshot = json!({
            "schema": "implexity-managed-package-snapshot/1",
            "generation": status["generation"],
            "loaded": status["loaded"],
            "load_order_fingerprint": status["load_order_fingerprint"],
            "loaded_manifests": status["loaded_manifests"],
            "registries": {
                "providers": {"generation": providers.generation, "fingerprint": providers.fingerprint},
                "addins": {"generation": addins.generation, "fingerprint": addins.fingerprint},
                "sufficiency": {"generation": rules.generation, "fingerprint": rules.fingerprint},
                "extensions": {"generation": extensions.generation, "fingerprint": extensions.fingerprint},
            },
        });
        let digest = canonical_digest(&snapshot);
        Ok((snapshot, digest))
    }

    fn owned_record(&self, id: &str) -> AgentResult<Arc<Record>> {
        let id = operation_id(Some(&json!(id)))?;
        let record = lock(&self.managed.records).get(&id).cloned();
        let Some(record) = record else {
            return Err(AgentError::refused(
                "managed evaluation is unknown or no longer owned",
            ));
        };
        if self.service_owner_digest() != record.owner_instance_digest {
            return Err(AgentError::permission(
                "managed evaluation belongs to a different service instance",
            ));
        }
        Ok(record)
    }

    fn authoritative_public_status(record: &Record, child: &ManagedStatus) -> AgentResult<Value> {
        let c = lock(&record.commit);
        let elapsed = child
            .wire
            .get("elapsed_s")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        match c.state.as_str() {
            "" | "pending" => Ok(child.wire.clone()),
            "finalizing" => Ok(
                json!({"schema": AUTHORITATIVE_STATUS_SCHEMA, "operation_id": record.operation_id,
                                      "state": "finalizing", "phase": "finalizing", "terminal": false,
                                      "cancelable": false, "terminal_reason": null, "elapsed_s": elapsed}),
            ),
            state @ ("succeeded" | "discarded_stale" | "commit_failed" | "cancelled") => {
                let reason = if state == "succeeded" {
                    child
                        .wire
                        .get("terminal_reason")
                        .filter(|v| crate::pyval::truthy(v))
                        .cloned()
                        .unwrap_or_else(|| json!("completed"))
                } else {
                    json!(c.reason)
                };
                Ok(
                    json!({"schema": AUTHORITATIVE_STATUS_SCHEMA, "operation_id": record.operation_id,
                          "state": state, "phase": "terminal", "terminal": true, "cancelable": false,
                          "terminal_reason": reason, "elapsed_s": elapsed}),
                )
            }
            _ => Err(AgentError::refused(
                "managed authoritative state is invalid",
            )),
        }
    }

    fn persist_disposition(
        &self,
        record: &Record,
        state: &str,
        reason: &str,
    ) -> Result<(), String> {
        let root = std::path::absolute(&self.state_dir)
            .unwrap_or_else(|_| self.state_dir.clone())
            .join(PRIVATE_ROOT)
            .join("authoritative");
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let target = root.join(format!("{}.json", record.operation_id));
        let payload = json!({"schema": COMMIT_RECORD_SCHEMA, "operation_id": record.operation_id,
                             "operation_kind": record.operation_kind, "state": state,
                             "terminal_reason": reason,
                             "frozen_case_revision": record.frozen_case_revision,
                             "frozen_case_digest": record.frozen_case_digest,
                             "frozen_model_digest": record.frozen_model_digest,
                             "frozen_package_digest": record.frozen_package_digest});
        let encoded = dumps(&payload, &DumpOptions::canonical()) + "\n";
        if target.exists() {
            let existing = std::fs::read(&target).map_err(|e| e.to_string())?;
            if existing != encoded.as_bytes() {
                return Err("managed authoritative disposition identity collision".into());
            }
            return Ok(());
        }
        implexity_io::atomic::write_atomic(&target, encoded.as_bytes()).map_err(|e| e.to_string())
    }

    fn terminalize(&self, record: &Record, state: &str, reason: &str) {
        let (state, reason) = match self.persist_disposition(record, state, reason) {
            Ok(()) => (state, reason),
            Err(_) => ("commit_failed", "authoritative_disposition_failed"),
        };
        let mut c = lock(&record.commit);
        state.clone_into(&mut c.state);
        c.reason = Some(reason.to_owned());
    }



    pub(crate) fn start_managed_evaluation(
        &self,
        kind: &str,
        request: Value,
    ) -> AgentResult<Value> {
        let owner = self.service_owner_digest();
        {
            let mut starting = lock(&self.managed.starting);
            if *starting {
                return Err(
                    AgentError::refused("another managed evaluation is being prepared")
                        .managed_not_started(),
                );
            }
            *starting = true;
        }
        let mut start_attempted = false;
        let mut control: Option<Arc<dyn ManagedControl>> = None;
        let mut supervisor: Option<Arc<dyn ManagedSupervisor>> = None;
        let mut registered: Option<String> = None;
        let outcome = (|| -> AgentResult<Value> {
            let record = {
                let _packages = implexity_core::packages::global().hold();
                self.host.packages_status()?;
                let (frozen_packages, frozen_package_digest) = Self::package_owner_snapshot()?;
                let (frozen_case_digest, frozen_case_revision) = {
                    let _state = self.host.state_lock();
                    self.case_owner_snapshot()?
                };
                let frozen_model_digest = self.model_owner_digest();
                let capsule = self.host.prepare_managed_evaluation(kind, request)?;
                if capsule.operation_kind() != kind {
                    return Err(AgentError::refused(
                        "exact manager returned a mismatched managed capsule",
                    ));
                }
                let sup = self.supervisor()?;
                supervisor = Some(Arc::clone(&sup));
                start_attempted = true;
                let started = capsule.start(&sup)?;
                control = Some(Arc::clone(&started));
                let (current_packages, current_package_digest) = Self::package_owner_snapshot()?;
                let (case_digest, case_revision) = {
                    let _state = self.host.state_lock();
                    self.case_owner_snapshot()?
                };
                let model_digest = self.model_owner_digest();
                if self.service_owner_digest() != owner
                    || case_digest != frozen_case_digest
                    || case_revision != frozen_case_revision
                    || model_digest != frozen_model_digest
                    || current_package_digest != frozen_package_digest
                    || current_packages != frozen_packages
                {
                    let _ = sup.request_cancel(started.as_ref());
                    return Err(AgentError::permission(
                        "authoritative state changed while managed evaluation started",
                    ));
                }
                let record = Arc::new(Record {
                    operation_id: started.operation_id(),
                    operation_kind: kind.to_owned(),
                    owner_instance_digest: owner.clone(),
                    frozen_case_digest,
                    frozen_case_revision,
                    frozen_model_digest,
                    frozen_package_digest,
                    frozen_package_snapshot: frozen_packages,
                    control: started,
                    capsule,
                    commit: Mutex::new(Commit {
                        state: "pending".into(),
                        ..Commit::default()
                    }),
                });
                let mut records = lock(&self.managed.records);
                if records.contains_key(&record.operation_id) {
                    return Err(AgentError::refused("managed operation identity collision"));
                }
                records.insert(record.operation_id.clone(), Arc::clone(&record));
                registered = Some(record.operation_id.clone());
                record
            };
            let sup = supervisor
                .clone()
                .ok_or_else(|| AgentError::refused("managed supervisor unavailable"))?;
            let status = sup.status(&record.operation_id)?;
            if status.state == "succeeded" {
                let result = self.finalize_success(&record);
                let public = Self::authoritative_public_status(&record, &status)?;
                return Ok(public_envelope(&public, result));
            }
            Ok(public_envelope(&status.wire, None))
        })();
        let failed = outcome.is_err();
        if failed {
            if let (Some(c), Some(s)) = (&control, &supervisor) {
                let _ = s.request_cancel(c.as_ref());
            }
            if let Some(id) = &registered {
                lock(&self.managed.records).remove(id);
            }
        }
        *lock(&self.managed.starting) = false;
        let outcome = match outcome {
            Err(e @ (AgentError::Refused(_) | AgentError::Permission(_))) => Err(e),
            Err(_) => Err(AgentError::refused(
                "managed evaluation could not be started safely",
            )),
            ok => ok,
        };
        if start_attempted {
            outcome
        } else {
            outcome.map_err(AgentError::managed_not_started)
        }
    }

    fn finalize_success(&self, record: &Record) -> Option<Value> {
        enum Outcome {
            Stale,
            Failed,
            Done(Value),
        }
        {
            let mut c = lock(&record.commit);
            match c.state.as_str() {
                "succeeded" => return c.terminal_result.clone(),
                "discarded_stale" | "commit_failed" | "cancelled" => return None,
                _ => {}
            }
            if c.finalize_started {
                return None;
            }
            c.finalize_started = true;
            c.state = "finalizing".into();
        }
        let outcome = (|| -> Outcome {
            let Ok(sup) = self.supervisor() else {
                return Outcome::Failed;
            };
            let Ok(directory) = sup.committed_directory(record.control.as_ref()) else {
                return Outcome::Failed;
            };
            let _packages = implexity_core::packages::global().hold();
            let Ok(_eval) = self.host.eval_lock() else {
                return Outcome::Failed;
            };
            let _state = self.host.state_lock();
            let model = self.host.model();
            let _live = model.as_ref().map(|m| m.live_lock());
            let Ok((case_digest, case_revision)) = self.case_owner_snapshot() else {
                return Outcome::Failed;
            };
            let Ok((packages, package_digest)) = Self::package_owner_snapshot() else {
                return Outcome::Failed;
            };
            let model_digest = self.model_owner_digest();
            if case_digest != record.frozen_case_digest
                || case_revision != record.frozen_case_revision
                || model_digest != record.frozen_model_digest
                || package_digest != record.frozen_package_digest
                || packages != record.frozen_package_snapshot
            {
                return Outcome::Stale;
            }
            match record.capsule.finalize(&directory) {
                Ok(v) if v.is_object() => Outcome::Done(v),
                _ => Outcome::Failed,
            }
        })();
        match outcome {
            Outcome::Stale => {
                self.terminalize(record, "discarded_stale", "authoritative_state_changed");
                None
            }
            Outcome::Failed => {
                self.terminalize(record, "commit_failed", "authoritative_commit_failed");
                None
            }
            Outcome::Done(v) => {
                let mut c = lock(&record.commit);
                c.terminal_result = Some(v.clone());
                c.state = "succeeded".into();
                c.reason = Some("completed".into());
                Some(v)
            }
        }
    }

    fn claim_precommit_cancellation(&self, record: &Record, child: &ManagedStatus) {
        if !matches!(
            child.state.as_str(),
            "succeeded" | "cancelled" | "discarded"
        ) {
            return;
        }
        let claimed = {
            let mut c = lock(&record.commit);
            if c.cancellation_requested && c.state == "pending" && !c.finalize_started {
                c.finalize_started = true;
                c.state = "finalizing".into();
                true
            } else {
                false
            }
        };
        if claimed {
            self.terminalize(record, "cancelled", "cancelled_before_commit");
        }
    }



    pub(crate) fn inspect_managed_evaluation(&self, id: &str) -> AgentResult<Value> {
        let record = self.owned_record(id)?;
        let sup = self.supervisor()?;
        let status = sup.status(&record.operation_id)?;
        self.claim_precommit_cancellation(&record, &status);
        if status.state == "succeeded" {
            let cancelled = lock(&record.commit).state == "cancelled";
            let result = if cancelled {
                None
            } else {
                self.finalize_success(&record)
            };
            return Ok(public_envelope(
                &Self::authoritative_public_status(&record, &status)?,
                result,
            ));
        }
        if lock(&record.commit).state != "pending" {
            return Ok(public_envelope(
                &Self::authoritative_public_status(&record, &status)?,
                None,
            ));
        }
        let mut envelope = public_envelope(&status.wire, None);
        if status.state == "failed" {
            if let Some(report) = sup.solver_recovery(record.control.as_ref())? {
                envelope["solver_recovery"] = implexity_core::error::validate_solver_recovery(&report)?;
            }
        }
        Ok(envelope)
    }



    pub(crate) fn cancel_managed_evaluation(&self, id: &str) -> AgentResult<Value> {
        let record = self.owned_record(id)?;
        let sup = self.supervisor()?;
        {
            let mut c = lock(&record.commit);
            if c.state == "pending" && !c.finalize_started {
                c.cancellation_requested = true;
            }
        }
        let status = sup.request_cancel(record.control.as_ref())?;
        self.claim_precommit_cancellation(&record, &status);
        if status.state != "succeeded" {
            if lock(&record.commit).state != "pending" {
                return Ok(public_envelope(
                    &Self::authoritative_public_status(&record, &status)?,
                    None,
                ));
            }
            return Ok(public_envelope(&status.wire, None));
        }
        let result = {
            let c = lock(&record.commit);
            if c.state == "succeeded" {
                c.terminal_result.clone()
            } else {
                None
            }
        };
        Ok(public_envelope(
            &Self::authoritative_public_status(&record, &status)?,
            result,
        ))
    }
}

#[must_use]
pub fn private_root(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join(PRIVATE_ROOT)
}
