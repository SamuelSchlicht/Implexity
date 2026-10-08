// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use implexity_core::pyobj::{py_str, truthy};
use implexity_geometry::NdArray;
use implexity_optim::numeric::float_value;
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use super::job::{ModelOptJob, num_array};
use super::run::StartArgs;
use super::terminal::numerical_attention_decision_name;
use super::{JobEntry, LiveJob, ModelOptimizeManager, is_terminal, live_apply_hz, lock};
use crate::error::{JobError, JobResult};
use crate::managed_io::{
    COPY_LIMIT, JSON_LIMIT, NPZ_LIMIT, artifact_fingerprint, copy_regular, read_json, read_npz,
};
use crate::private::epoch_seconds;

fn opt1<T>(m: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(m))
}

fn verr(m: impl Into<String>) -> JobError {
    JobError::value(m)
}

fn to_nd(v: &BTreeMap<String, ArrayD<f64>>) -> JobResult<BTreeMap<String, NdArray>> {
    v.iter()
        .map(|(k, a)| {
            NdArray::from_f64(a.shape().to_vec(), a.iter().copied().collect())
                .map(|n| (k.clone(), n))
                .ok_or_else(|| JobError::value("design value shape mismatch"))
        })
        .collect()
}

fn history(
    me: &ModelOptimizeManager,
    kind: &str,
    label: &str,
    details: &Map<String, Value>,
) -> Option<Value> {
    me.inner
        .authoring
        .history
        .append(kind, label, Some(details), None, None)
        .ok()
}

fn job_url(id: &str) -> String {
    format!("/v1/implicit/optimize/jobs/{id}")
}

impl ModelOptimizeManager {
    pub(crate) fn apply_job_design(
        &self,
        job: &ModelOptJob,
        values: &BTreeMap<String, ArrayD<f64>>,
        persist: bool,
        source: &str,
    ) -> JobResult<Value> {
        let derived = self.provider_derived_values(job, values)?;
        let overlap: Vec<&String> = values.keys().filter(|k| derived.contains_key(*k)).collect();
        if !overlap.is_empty() {
            return Err(verr(format!(
                "provider-derived outputs overlap design coordinates {}",
                implexity_core::pyobj::list_repr(&overlap)
            )));
        }
        let mut combined = values.clone();
        combined.extend(derived.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mut plan = job.plan.clone();
        plan.extend(job.provider_derived_plan.iter().cloned());
        let mut report =
            self.inner
                .models
                .apply_values(&plan, &to_nd(&combined)?, persist, source)?;
        if let Some(r) = report.as_object_mut() {
            r.insert(
                "provider_derived_model_updates".into(),
                json!(derived.keys().collect::<Vec<_>>()),
            );
        }
        Ok(report)
    }

    pub(crate) fn apply_live(&self, job: &LiveJob, row: &mut Value) -> JobResult<()> {
        let (id, status, requested, last) = {
            let j = lock(job);
            (
                j.id.clone(),
                j.status.clone(),
                j.managed_requested_control.clone(),
                j.last_apply_t,
            )
        };
        let Some(push) = row.get("push").filter(|p| !p.is_null()).cloned() else {
            self.event_iter(&id, row, false);
            return Ok(());
        };
        if status != "running" || matches!(requested.as_deref(), Some("stop" | "supersede")) {
            self.event_iter(&id, row, false);
            return Ok(());
        }
        let now = crate::private::perf_counter();
        let forced = row.get("steer_applied_before").is_some_and(truthy);
        if !forced && last.is_some_and(|l| (now - l) < 1.0 / live_apply_hz()) {
            lock(job).apply_skipped += 1;
            self.event_iter(&id, row, false);
            return Ok(());
        }
        lock(job).last_apply_t = Some(now);
        let t0 = std::time::Instant::now();
        let values = self.managed_load_live_design(job, row, false)?;
        let snapshot = lock(job).clone();
        let rep = self.apply_job_design(
            &snapshot,
            &values,
            false,
            &format!(
                "implicit_optimize:{} i{}",
                id,
                row.get("i").map_or_else(|| "None".to_string(), py_str)
            ),
        )?;
        let push = row.get("push").cloned().unwrap_or(push);
        {
            let mut j = lock(job);
            j.live = Some(
                values
                    .iter()
                    .map(|(k, v)| (k.clone(), num_array(v)))
                    .collect(),
            );
            j.model_content_id_owned = rep.get("content_id").cloned().unwrap_or(Value::Null);
            let mut stat = Map::new();
            stat.insert("i".into(), row.get("i").cloned().unwrap_or(Value::Null));
            stat.insert(
                "bytes".into(),
                json!(push.get("bytes").and_then(Value::as_i64).unwrap_or(0)),
            );
            stat.insert(
                "sha256".into(),
                push.get("verified_sha256").cloned().unwrap_or(Value::Null),
            );
            stat.insert(
                "design_state_id".into(),
                push.get("verified_design_state_id")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            stat.insert(
                "save_ms".into(),
                push.get("save_ms").cloned().unwrap_or(Value::Null),
            );
            stat.insert(
                "apply_ms".into(),
                float_value(implexity_mesh::numeric::py_round_digits(
                    t0.elapsed().as_secs_f64() * 1e3,
                    2,
                )),
            );
            j.apply_stats.push(stat);
        }
        if let Some(r) = row.as_object_mut() {
            r.insert(
                "document_content_id".into(),
                rep.get("content_id").cloned().unwrap_or(Value::Null),
            );
            r.insert(
                "document_moved".into(),
                json!(
                    rep.get("moved")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len)
                ),
            );
            r.insert(
                "document_provider_derived_updates".into(),
                rep.get("provider_derived_model_updates")
                    .cloned()
                    .unwrap_or(json!([])),
            );
        }
        let applied = row.get("document_content_id").is_some();
        self.event_iter(&id, row, applied);
        Ok(())
    }

    pub(crate) fn clear_live(&self, job: &LiveJob) {
        let (id, attention_open) = {
            let j = lock(job);
            let open = j.status == "attention"
                && j.numerical_attention
                    .as_ref()
                    .and_then(|a| a.get("state"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| s == "open" || s == "branch_pending");
            (j.id.clone(), open)
        };
        let live = self.inner.models.live_optimisation();
        let owner = live
            .as_ref()
            .and_then(|l| l.get("job_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if attention_open {
            if live.as_ref().is_none_or(|l| !l.is_object()) || owner.as_deref() == Some(id.as_str())
            {
                self.hold_numerical_attention_live(job);
            }
            return;
        }
        if owner.as_deref() == Some(id.as_str()) {
            self.inner.models.set_live_optimisation(None);
        }
    }

    pub(crate) fn hold_numerical_attention_live(&self, job: &LiveJob) {
        let (id, free) = {
            let j = lock(job);
            (
                j.id.clone(),
                j.plan
                    .iter()
                    .filter_map(|e| e.get("ref").cloned())
                    .collect::<Vec<_>>(),
            )
        };
        let current = self.inner.models.live_optimisation();
        let since = current
            .as_ref()
            .filter(|c| c.get("job_id").and_then(Value::as_str) == Some(id.as_str()))
            .and_then(|c| c.get("since").cloned())
            .filter(truthy)
            .unwrap_or_else(|| float_value(epoch_seconds()));
        self.inner.models.set_live_optimisation(Some(json!({
            "job_id": id, "since": since, "job": job_url(&id), "free": free,
            "note": "an accepted checkpoint is awaiting a numerical solver decision; retry with more solver effort, \
                     continue as exploratory, or stop and restore before editing the model",
        })));
    }

    pub(crate) fn revert(&self, job: &LiveJob, why: &str) -> bool {
        let snapshot = {
            let mut j = lock(job);
            if j.accepted.as_ref().is_some_and(truthy) || j.model_rollback_resolved {
                return true;
            }
            if j.before_values.is_empty() || j.live.is_none() {
                j.model_rollback_resolved = true;
                return true;
            }
            j.clone()
        };
        let result = (|| -> JobResult<Value> {
            let mut values = snapshot.before_values.clone();
            values.extend(
                snapshot
                    .provider_derived_before_values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            let mut plan = snapshot.plan.clone();
            plan.extend(snapshot.provider_derived_plan.iter().cloned());
            Ok(self.inner.models.apply_values(
                &plan,
                &to_nd(&values)?,
                false,
                &format!("implicit_optimize:{} reverted", snapshot.id),
            )?)
        })();
        let mut j = lock(job);
        match result {
            Ok(rep) => {
                let cid = rep.get("content_id").cloned().unwrap_or(Value::Null);
                let j = &mut *j;
                j.live = None;
                j.model_content_id_owned = cid.clone();
                j.model_rollback_resolved = true;
                let note = format!(
                    " | the model document's {} design variable(s) were put back to their pre-run values ({why}); content_id {}",
                    j.plan.len(),
                    py_str(&cid)
                );
                j.message.push_str(&note);
                true
            }
            Err(e) => {
                let id = j.id.clone();
                let note = format!(
                    " | REVERT FAILED ({}); the document may still hold the iterate -- GET /v1/implicit/optimize/jobs/{id}/before \
                     has the document as it was, ready to PUT back",
                    e.describe()
                );
                j.message.push_str(&note);
                false
            }
        }
    }

    pub(crate) fn settle_intervention_branch(
        &self,
        child: &LiveJob,
        succeeded: bool,
        detail: Option<&str>,
    ) {
        let (child_id, parent_id, token) = {
            let c = lock(child);
            (
                c.id.clone(),
                c.parent_job_id.clone(),
                c.intervention_branch_token.clone(),
            )
        };
        let (Some(parent_id), Some(token)) = (parent_id, token) else {
            return;
        };
        let Some(parent) = self.entry(&parent_id).and_then(|e| e.live()) else {
            return;
        };
        if lock(&parent)
            .intervention
            .as_ref()
            .is_none_or(|i| !truthy(i))
        {
            return;
        }
        {
            let _g = self.lifecycle_lock(&parent_id).hold();
            let mut state = self.state();
            let mut p = lock(&parent);
            let Some(iv) = p.intervention.as_mut().and_then(Value::as_object_mut) else {
                return;
            };
            let owned = iv.get("state").and_then(Value::as_str) == Some("branch_pending")
                && iv.get("pending_token").and_then(Value::as_str) == Some(token.as_str())
                && iv
                    .get("pending_child_job_id")
                    .is_none_or(|v| v.is_null() || v.as_str() == Some(child_id.as_str()));
            if !owned {
                return;
            }
            let now = float_value(epoch_seconds());
            iv.shift_remove("pending_child_job_id");
            iv.shift_remove("pending_at");
            iv.shift_remove("pending_token");
            if succeeded {
                iv.insert("state".into(), json!("branched"));
                iv.insert("closed_at".into(), now);
                iv.insert("child_job_id".into(), json!(child_id));
                p.status = "branched".into();
                p.mark("manual_intervention_branched");
            } else {
                iv.insert("state".into(), json!("open"));
                iv.insert("last_failed_child_job_id".into(), json!(child_id));
                iv.insert("last_failure_at".into(), now);
                if state.model_authority_job_id.as_deref() == Some(child_id.as_str()) {
                    state.model_authority_job_id = Some(parent_id.clone());
                }
                if let Some(d) = detail.filter(|d| !d.is_empty()) {
                    iv.insert(
                        "last_failure".into(),
                        json!(d.chars().take(500).collect::<String>()),
                    );
                }
                p.mark("manual_intervention_branch_failed");
            }
        }
        if succeeded {
            let mut details = Map::new();
            details.insert("parent_job_id".into(), json!(parent_id));
            details.insert("child_job_id".into(), json!(child_id));
            history(
                self,
                "optimization_branch",
                "Optimization resumed from manually modified iterate",
                &details,
            );
        }
        self.event(&parent_id);
    }

    pub(crate) fn settle_numerical_attention_branch(
        &self,
        child: &LiveJob,
        succeeded: bool,
        detail: Option<&str>,
    ) {
        let (child_id, parent_id, token) = {
            let c = lock(child);
            (
                c.id.clone(),
                c.parent_job_id.clone(),
                c.numerical_attention_branch_token.clone(),
            )
        };
        let (Some(parent_id), Some(token)) = (parent_id, token) else {
            return;
        };
        let Some(parent) = self.entry(&parent_id).and_then(|e| e.live()) else {
            return;
        };
        if lock(&parent)
            .numerical_attention
            .as_ref()
            .is_none_or(|a| !a.is_object())
        {
            return;
        }
        let mut hold = false;
        {
            let _g = self.lifecycle_lock(&parent_id).hold();
            let mut state = self.state();
            let mut p = lock(&parent);
            let status = p.status.clone();
            let Some(att) = p
                .numerical_attention
                .as_mut()
                .and_then(Value::as_object_mut)
            else {
                return;
            };
            let owned = status == "attention"
                && att.get("state").and_then(Value::as_str) == Some("branch_pending")
                && att.get("pending_token").and_then(Value::as_str) == Some(token.as_str())
                && att
                    .get("pending_child_job_id")
                    .is_none_or(|v| v.is_null() || v.as_str() == Some(child_id.as_str()));
            if !owned {
                return;
            }
            att.shift_remove("pending_token");
            att.shift_remove("pending_child_job_id");
            if succeeded {
                att.insert("state".into(), json!("branched"));
                att.insert("outcome".into(), json!("exploratory_branch_started"));
                att.insert("child_job_id".into(), json!(child_id));
                p.status = "branched".into();
                p.mark("numerical_attention_branched");
            } else {
                att.insert("state".into(), json!("open"));
                att.insert("action".into(), Value::Null);
                att.insert("outcome".into(), json!("pending"));
                att.insert("result_authority".into(), Value::Null);
                att.shift_remove("continuation_iteration_budget");
                att.insert("last_failed_child_job_id".into(), json!(child_id));
                if let Some(d) = detail.filter(|d| !d.is_empty()) {
                    att.insert(
                        "last_failure".into(),
                        json!(d.chars().take(500).collect::<String>()),
                    );
                }
                if state.model_authority_job_id.as_deref() == Some(child_id.as_str()) {
                    state.model_authority_job_id = Some(parent_id.clone());
                }
                let live = self.inner.models.live_optimisation();
                let owner = live
                    .as_ref()
                    .and_then(|l| l.get("job_id"))
                    .and_then(Value::as_str);
                hold = live.as_ref().is_none_or(|l| !l.is_object())
                    || owner == Some(child_id.as_str())
                    || owner == Some(parent_id.as_str());
                p.mark("numerical_attention_branch_failed");
            }
        }
        if hold {
            self.hold_numerical_attention_live(&parent);
        }
        self.event(&parent_id);
    }

    pub(crate) fn assert_model_mutation_authority(
        &self,
        job: &LiveJob,
        operation: &str,
        allow_intervention_edits: bool,
    ) -> JobResult<()> {
        let state = self.state();
        let (id, status, owned) = {
            let j = lock(job);
            (
                j.id.clone(),
                j.status.clone(),
                j.model_content_id_owned.clone(),
            )
        };
        if let Some(active) = state
            .active
            .as_ref()
            .filter(|a| **a != id)
            .and_then(|a| state.jobs.get(a))
            && !is_terminal(&active.status())
        {
            return opt1(format!(
                "{operation}: job {} currently owns the live model",
                active.id()
            ));
        }
        if state.model_authority_job_id.as_deref() != Some(id.as_str()) {
            return opt1(format!(
                "{operation}: this result no longer owns the live model; a newer job or document revision is authoritative"
            ));
        }
        if let Some(live) = self
            .inner
            .models
            .live_optimisation()
            .filter(|l| !l.is_null())
            && live.get("job_id").and_then(Value::as_str) != Some(id.as_str())
        {
            return opt1(format!(
                "{operation}: another optimization owns the live model"
            ));
        }
        if !(allow_intervention_edits && status == "intervening") {
            let current = self
                .inner
                .models
                .status()?
                .get("content_id")
                .cloned()
                .unwrap_or(Value::Null);
            if current != owned {
                return opt1(format!(
                    "{operation}: the model changed after this job's last authoritative revision; stale results cannot overwrite it"
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn control(&self, job: &LiveJob, op: &str) -> JobResult<()> {
        let (before_birth, from_pause, control) = {
            let _state = self.state();
            let mut j = lock(job);
            let previous = j.managed_requested_control.clone();
            match op {
                "pause" => {
                    if matches!(previous.as_deref(), Some("stop" | "supersede")) {
                        return opt1(format!(
                            "pause: terminal control {} is already committed",
                            previous.unwrap_or_default()
                        ));
                    }
                    if previous.as_deref() == Some("pause") {
                        return opt1("pause: a pause is already committed for this job");
                    }
                    j.managed_requested_control = Some("pause".into());
                }
                "stop" | "supersede" => {
                    if let Some(p) = previous
                        .as_deref()
                        .filter(|p| *p == "stop" || *p == "supersede")
                    {
                        return opt1(format!(
                            "{op}: terminal control {p} is already committed; the first terminal request is authoritative"
                        ));
                    }
                    j.managed_requested_control = Some(op.into());
                }
                _ => return opt1("unsupported managed child control"),
            }
            let control = j.managed_control.clone();
            let terminal = if op == "stop" {
                "stopped"
            } else {
                "superseded"
            };
            if (op == "stop" || op == "supersede") && j.status == "paused" {
                j.status = terminal.into();
                j.mark(&format!("{terminal}_from_pause"));
                j.managed_supervisor_state = "cancelled".into();
                j.managed_supervisor_reason = Some(format!("{terminal}_from_pause"));
                j.message = format!("{terminal} from validated pause");
                j.t_end = Some(epoch_seconds());
                (false, true, control)
            } else if control.is_none() {
                if op == "stop" || op == "supersede" {
                    j.status = terminal.into();
                    j.mark(&format!("{terminal}_before_spawn"));
                    j.managed_supervisor_state = "cancelled".into();
                    j.managed_supervisor_reason = Some(format!("{terminal}_before_spawn"));
                    j.message = format!("{terminal} atomically before child birth");
                    j.t_end = Some(epoch_seconds());
                    (true, false, None)
                } else {
                    if j.managed_requested_control.as_deref() == Some("pause") {
                        j.managed_requested_control = None;
                    }
                    return opt1(format!(
                        "{op}: managed child control is not established yet"
                    ));
                }
            } else {
                if op == "stop" || op == "supersede" {
                    j.status = if op == "stop" {
                        "stopping"
                    } else {
                        "superseding"
                    }
                    .into();
                    j.mark(&format!("{op}_requested"));
                }
                (false, false, control)
            }
        };
        if before_birth || from_pause {
            let reason = lock(job)
                .managed_supervisor_reason
                .clone()
                .unwrap_or_default();
            let rollback_ok = if before_birth {
                lock(job).model_rollback_resolved = true;
                true
            } else {
                self.revert(job, &reason)
            };
            let (id, parent) = {
                let j = lock(job);
                (j.id.clone(), j.parent_job_id.clone())
            };
            if rollback_ok {
                self.clear_live(job);
            }
            if rollback_ok && parent.is_some() {
                self.settle_intervention_branch(job, false, Some(&reason));
                self.settle_numerical_attention_branch(job, false, Some(&reason));
            }
            if rollback_ok {
                let mut state = self.state();
                if state.active.as_deref() == Some(id.as_str()) {
                    state.active = None;
                }
                if state.model_authority_job_id.as_deref() == Some(id.as_str()) {
                    state.model_authority_job_id = None;
                }
            }
            self.event(&id);
            lock(job).managed_terminal_event.set();
            return Ok(());
        }
        let control =
            control.ok_or_else(|| JobError::runtime("managed child control disappeared"))?;
        if op == "stop" || op == "supersede" {
            self.inner.supervisor.request_cancel(&control)?;
            return Ok(());
        }
        let written = (|| -> JobResult<()> {
            let directory = self.inner.supervisor.partial_directory(&control)?;
            let temporary = directory.join(format!(
                "control.json.{}.tmp",
                crate::private::token_hex(16)?
            ));
            let mut f = crate::private::create_exclusive(&temporary, 0o644)?;
            crate::private::write_all_sync(&mut f, br#"{"op": "pause"}"#)?;
            drop(f);
            std::fs::rename(&temporary, directory.join("control.json"))?;
            Ok(())
        })();
        if let Err(e) = written {
            let _state = self.state();
            let mut j = lock(job);
            if j.managed_requested_control.as_deref() == Some("pause") {
                j.managed_requested_control = None;
            }
            return opt1(format!(
                "pause: managed checkpoint request failed: {}",
                e.message()
            ));
        }
        Ok(())
    }

    pub fn op(&self, job_id: &str, op: &str) -> JobResult<Value> {
        let entry = self.require_entry(job_id)?;
        let lifecycle = self.lifecycle_lock(job_id);
        let Some(_guard) = lifecycle.try_hold() else {
            return opt1(format!(
                "{op}: another lifecycle transition for job {job_id} is in progress; poll the job and retry from its resulting state"
            ));
        };
        let rollback_resolved = entry
            .live()
            .is_some_and(|j| lock(&j).model_rollback_resolved);
        let manipulation = Arc::clone(&self.inner.authoring.manipulation);
        let reservation = if op == "resume"
            || op == "accept"
            || (op == "discard" && !rollback_resolved)
        {
            Some(manipulation.reserve_idle(&format!("{op} optimization result"), Some(job_id))?)
        } else {
            None
        };
        let result = if op == "resume" || op == "accept" {
            let mut out = None;
            self.inner
                .host
                .physics_runtime_guard(&Value::String(job_id.into()), &mut || {
                    out = Some(self.op_serialized(&entry, op)?);
                    Ok(())
                })
                .map(|()| out.unwrap_or(Value::Null))
        } else {
            self.op_serialized(&entry, op)
        };
        if let Some(r) = reservation {
            let _ = manipulation.release_reservation(&r);
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn op_serialized(&self, entry: &JobEntry, op: &str) -> JobResult<Value> {
        let job = match entry {
            JobEntry::Recovered(r) => {
                if op != "stop" {
                    return opt1(format!(
                        "{op}: a restart-recovered job is inspect/stop-only"
                    ));
                }
                let (id, control) = {
                    let mut j = lock(r);
                    if j.status != "running" && j.status != "finalizing" {
                        return opt1(format!("stop: job is {}", j.status));
                    }
                    if j.managed_requested_control.is_some() {
                        return opt1("stop: the recovered job already has a terminal control");
                    }
                    j.managed_requested_control = Some("stop".into());
                    j.status = "stopping".into();
                    j.message = "supervised stop requested after service restart".into();
                    j.mark("stop_requested");
                    (j.id.clone(), j.managed_control.clone())
                };
                self.inner.supervisor.request_cancel(&control)?;
                self.event(&id);
                return Ok(lock(r).as_dict(false));
            }
            JobEntry::Live(j) => Arc::clone(j),
        };
        let (id, working, provider, job_dir) = {
            let j = lock(&job);
            (
                j.id.clone(),
                j.working_design.as_ref().is_some_and(truthy),
                j.physics_provider.clone(),
                PathBuf::from(j.job_dir.clone().unwrap_or_default()),
            )
        };
        if working && ["resume", "retry_exact", "accept", "discard", "intervene"].contains(&op) {
            return opt1(
                "This result has already been saved as a working design. Edit the current model or start a fresh evaluation; \
                 its original run remains available for inspection.",
            );
        }
        if ["resume", "retry_exact", "accept"].contains(&op) && !provider.is_empty() {
            let spec_path = job_dir.join("spec.json");
            if spec_path.is_file() {
                let stored =
                    implexity_core::json::read_file(&spec_path).map_err(JobError::runtime)?;
                if let Some(snap) = stored.get("physics_snapshot").filter(|s| truthy(s)) {
                    let current = crate::effort::physics_snapshot()?;
                    if snap.get("registry_fingerprint") != current.get("registry_fingerprint") {
                        return opt1(
                            "STALE_PHYSICS_PLAN: loaded add-ins differ from the job snapshot",
                        );
                    }
                }
            }
        }
        match op {
            "intervene" => return self.intervene_serialized(&job),
            "pause" => {
                let (status, requested) = {
                    let j = lock(&job);
                    (j.status.clone(), j.managed_requested_control.clone())
                };
                if status != "running" {
                    return opt1(format!("pause: job is {status}"));
                }
                if matches!(requested.as_deref(), Some("stop" | "supersede")) {
                    return opt1("pause: a terminal control is already committed for this job");
                }
                self.control(&job, "pause")?;
                lock(&job).message = "pause requested (honoured between iterations)".into();
            }
            "stop" => {
                let status = lock(&job).status.clone();
                if ["queued", "resuming", "running", "paused"].contains(&status.as_str()) {
                    self.control(&job, "stop")?;
                    let mut j = lock(&job);
                    if !is_terminal(&j.status) {
                        j.message = "immediate supervised stop requested".into();
                    }
                } else {
                    return opt1(format!("stop: job is {status}"));
                }
            }
            "resume" | "retry_exact" => self.resume_serialized(&job, op)?,
            "accept" => {
                self.assert_model_mutation_authority(&job, "accept", false)?;
                self.accept(&job)?;
                {
                    let mut state = self.state();
                    if state.model_authority_job_id.as_deref() == Some(id.as_str()) {
                        state.model_authority_job_id = None;
                    }
                }
                let discard = lock(&job)
                    .numerical_attention
                    .as_ref()
                    .and_then(|a| a.get("action"))
                    .and_then(Value::as_str)
                    == Some("discard");
                if !discard {
                    let mut details = Map::new();
                    details.insert("job_id".into(), json!(id));
                    details.insert("provider".into(), json!(provider));
                    history(
                        self,
                        "optimization_accepted",
                        "Optimized design accepted",
                        &details,
                    );
                }
            }
            "discard" => {
                let (status, open_intervention, resolved, parent) = {
                    let j = lock(&job);
                    (
                        j.status.clone(),
                        j.intervention
                            .as_ref()
                            .and_then(|i| i.get("state"))
                            .and_then(Value::as_str)
                            == Some("open"),
                        j.model_rollback_resolved,
                        j.parent_job_id.clone(),
                    )
                };
                if ![
                    "completed",
                    "stopped",
                    "paused",
                    "intervening",
                    "attention",
                    "error",
                ]
                .contains(&status.as_str())
                {
                    return opt1(format!("discard: job is {status}"));
                }
                if status == "intervening" && !open_intervention {
                    return opt1(
                        "discard: an intervention branch is reserved or already committed; the historical parent cannot mutate the model",
                    );
                }
                if !resolved {
                    self.assert_model_mutation_authority(&job, "discard", true)?;
                    if !self.revert(&job, "discarded") {
                        return opt1(
                            "discard: rollback failed; the job retains model authority so the operation can be retried safely",
                        );
                    }
                }
                if parent.is_some() {
                    self.settle_intervention_branch(&job, false, Some("discarded after rollback"));
                    self.settle_numerical_attention_branch(
                        &job,
                        false,
                        Some("discarded after rollback"),
                    );
                }
                {
                    let mut j = lock(&job);
                    j.status = "discarded".into();
                    j.mark("discarded");
                }
                self.clear_live(&job);
                {
                    let mut state = self.state();
                    if state.model_authority_job_id.as_deref() == Some(id.as_str()) {
                        state.model_authority_job_id = None;
                    }
                }
                let mut details = Map::new();
                details.insert("job_id".into(), json!(id));
                history(
                    self,
                    "optimization_discarded",
                    "Optimization result discarded",
                    &details,
                );
            }
            other => {
                return opt1(format!(
                    "unknown op {}; ops are pause, intervene, resume, stop, accept, discard",
                    implexity_core::py_repr::repr_str(other)
                ));
            }
        }
        self.event(&id);
        Ok(lock(&job).as_dict(false))
    }

    #[allow(clippy::too_many_lines)]
    fn resume_serialized(&self, job: &LiveJob, op: &str) -> JobResult<()> {
        let expected_status = if op == "resume" {
            "paused"
        } else {
            "attention"
        };
        let expected_terminal = if op == "resume" {
            "paused"
        } else {
            "numerical_attention"
        };
        let (status, job_dir, manifest, attention, id) = {
            let j = lock(job);
            (
                j.status.clone(),
                PathBuf::from(j.job_dir.clone().unwrap_or_default()),
                j.managed_terminal_manifest.clone(),
                j.numerical_attention.clone(),
                j.id.clone(),
            )
        };
        if status != expected_status {
            return opt1(format!(
                "{op}: job is {status}; required checkpoint state is {expected_status}"
            ));
        }
        if !job_dir.join("ckpt.npz").is_file() {
            return opt1("resume: no checkpoint recorded yet");
        }
        let sealed = (|| -> JobResult<()> {
            if manifest
                .as_ref()
                .and_then(|m| m.get("terminal_kind"))
                .and_then(Value::as_str)
                != Some(expected_terminal)
            {
                return Err(verr(format!(
                    "validated {expected_terminal} seal is unavailable"
                )));
            }
            if op == "retry_exact" {
                let token = attention
                    .as_ref()
                    .and_then(|a| a.get("event_token"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = numerical_attention_decision_name(&token)?;
                let policy = read_json(&job_dir.join(name), 1024 * 1024)?;
                let policy =
                    crate::solver_telemetry::validate_numerical_deviation_policy(Some(&policy))?
                        .ok_or_else(|| verr("exact retry decision identity drifted"))?;
                if policy.get("action").and_then(Value::as_str) != Some("retry_exact")
                    || policy.get("event_token") != Some(&token)
                {
                    return Err(verr("exact retry decision identity drifted"));
                }
            }
            self.verify_managed_terminal_seal(manifest.as_ref(), &job_dir)
        })();
        if let Err(e) = sealed {
            return opt1(format!(
                "resume: paused generation failed its terminal seal: {}",
                e.message()
            ));
        }
        self.assert_model_mutation_authority(job, "resume", false)?;
        let ctl = job_dir.join("control.json");
        let control_payload = if ctl.is_file() {
            Some(std::fs::read(&ctl)?)
        } else {
            None
        };
        let previous_job = lock(job).clone();
        let previous_live = self.inner.models.live_optimisation();
        let previous_active = {
            let mut state = self.state();
            if let Some(act) = state
                .active
                .as_ref()
                .filter(|a| **a != id)
                .and_then(|a| state.jobs.get(a))
                && !is_terminal(&act.status())
            {
                return opt1(format!(
                    "resume: job {} is {}; one at a time",
                    act.id(),
                    act.status()
                ));
            }
            let prev = state.active.clone();
            state.active = Some(id.clone());
            prev
        };
        let started = (|| -> JobResult<()> {
            if control_payload.is_some() {
                std::fs::remove_file(&ctl)?;
            }
            {
                let mut j = lock(job);
                j.summary = None;
                j.error = None;
                j.managed_requested_control = None;
                j.managed_control = None;
                j.managed_operation_id = None;
                j.managed_active_dir = None;
                j.managed_terminal_event.clear();
                j.managed_supervisor_state = "not_started".into();
                j.managed_supervisor_reason = Some("resume_queued".into());
                j.status = "resuming".into();
                j.mark(if op == "resume" {
                    "resume_requested"
                } else {
                    "numerical_exact_retry_requested"
                });
                if op == "retry_exact"
                    && let Some(a) = j
                        .numerical_attention
                        .as_mut()
                        .and_then(Value::as_object_mut)
                {
                    a.insert("outcome".into(), json!("exact_retry_running"));
                }
                let free: Vec<Value> = j
                    .plan
                    .iter()
                    .filter_map(|e| e.get("ref").cloned())
                    .collect();
                self.inner.models.set_live_optimisation(Some(json!({
                    "job_id": id, "since": float_value(epoch_seconds()), "job": job_url(&id), "free": free,
                    "note": "resumed: the design variables below are this job's latest iterate, in memory",
                })));
                j.message = "resuming from the checkpoint (L0 and the term references are NOT re-measured: they belong \
                             to the start model)"
                    .into();
            }
            let me = self.clone();
            let handle = Arc::clone(job);
            std::thread::Builder::new()
                .name(format!("implexity-opt-{id}"))
                .spawn(move || me.run(&handle, true))?;
            Ok(())
        })();
        if let Err(e) = started {
            {
                let mut j = lock(job);
                j.status.clone_from(&previous_job.status);
                j.summary.clone_from(&previous_job.summary);
                j.error.clone_from(&previous_job.error);
                j.managed_requested_control
                    .clone_from(&previous_job.managed_requested_control);
                j.managed_control.clone_from(&previous_job.managed_control);
                j.managed_operation_id
                    .clone_from(&previous_job.managed_operation_id);
                j.managed_active_dir
                    .clone_from(&previous_job.managed_active_dir);
                j.managed_supervisor_state
                    .clone_from(&previous_job.managed_supervisor_state);
                j.managed_supervisor_reason
                    .clone_from(&previous_job.managed_supervisor_reason);
                j.message.clone_from(&previous_job.message);
                j.timeline.clone_from(&previous_job.timeline);
                if previous_job.managed_terminal_event.is_set() {
                    j.managed_terminal_event.set();
                } else {
                    j.managed_terminal_event.clear();
                }
            }
            self.inner.models.set_live_optimisation(previous_live);
            self.state().active = previous_active;
            if let Some(payload) = control_payload
                && !ctl.exists()
            {
                let temporary = job_dir.join(format!(
                    "control.json.resume-restore.{}",
                    crate::private::token_hex(16)?
                ));
                let mut f = crate::private::create_exclusive(&temporary, 0o644)?;
                crate::private::write_all_sync(&mut f, &payload)?;
                drop(f);
                std::fs::rename(&temporary, &ctl)?;
            }
            self.event(&id);
            return opt1(format!(
                "{op}: optimization worker could not start; the exact validated generation was restored: {}",
                e.describe()
            ));
        }
        Ok(())
    }

    fn accept_moved_report(
        job: &ModelOptJob,
        values: &BTreeMap<String, ArrayD<f64>>,
    ) -> JobResult<Vec<Value>> {
        let mut moved = Vec::new();
        for entry in &job.plan {
            let r = entry.get("ref").map(py_str).unwrap_or_default();
            let Some(v) = values.get(&r) else { continue };
            let was = entry.get("start").cloned().unwrap_or(Value::Null);
            let now = num_array(v);
            let mut row = Map::new();
            for (k, src) in [
                ("ref", "ref"),
                ("kind", "kind"),
                ("parameter", "parameter"),
                ("node", "node"),
                ("param", "param"),
            ] {
                row.insert(k.into(), entry.get(src).cloned().unwrap_or(Value::Null));
            }
            row.insert(
                "units".into(),
                entry.get("node_units").cloned().unwrap_or(Value::Null),
            );
            row.insert(
                "document_units".into(),
                entry.get("units").cloned().unwrap_or(Value::Null),
            );
            row.insert("was".into(), was.clone());
            row.insert("now".into(), now.clone());
            if entry.get("kind").and_then(Value::as_str) == Some("parameter")
                && entry.get("units") != entry.get("node_units")
            {
                row.insert(
                    "document_was".into(),
                    entry.get("start_document").cloned().unwrap_or(Value::Null),
                );
                let dn = match now.as_f64() {
                    Some(n) if now.is_f64() => {
                        let from = entry.get("node_units").map(py_str).unwrap_or_default();
                        let to = entry.get("units").map(py_str).unwrap_or_default();
                        float_value(implexity_geometry::document::convert(n, &from, &to)?)
                    }
                    _ => Value::Null,
                };
                row.insert("document_now".into(), dn);
            }
            if was.is_f64() && now.is_f64() {
                let (w, n) = (
                    was.as_f64().unwrap_or(f64::NAN),
                    now.as_f64().unwrap_or(f64::NAN),
                );
                row.insert("delta".into(), float_value(n - w));
                let span = match (
                    entry.get("lo").and_then(Value::as_f64),
                    entry.get("hi").and_then(Value::as_f64),
                ) {
                    (Some(lo), Some(hi)) => Some(hi - lo),
                    _ => None,
                };
                if let Some(s) = span.filter(|s| *s != 0.0) {
                    row.insert("delta_fraction_of_range".into(), float_value((n - w) / s));
                }
                row.insert("moved".into(), json!((n - w).abs() > 1e-12));
            } else if let (Some(wa), Some(na)) = (was.as_array(), now.as_array()) {
                let wv: Vec<f64> = wa.iter().filter_map(Value::as_f64).collect();
                let nv: Vec<f64> = na.iter().filter_map(Value::as_f64).collect();
                let delta: Vec<f64> = nv.iter().zip(&wv).map(|(a, b)| (a - b).abs()).collect();
                let mean = |v: &[f64]| implexity_optim::numeric::np_sum(v) / v.len() as f64;
                let mn = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
                let mx = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                row.insert("size".into(), json!(na.len()));
                row.insert("moved".into(), json!(delta.iter().any(|d| *d > 1e-12)));
                row.insert("delta_absmean".into(), float_value(mean(&delta)));
                row.insert("delta_absmax".into(), float_value(mx(&delta)));
                row.insert(
                    "entries_moved".into(),
                    json!(delta.iter().filter(|d| **d > 1e-12).count()),
                );
                row.insert("was".into(), json!({"mean": float_value(mean(&wv)), "min": float_value(mn(&wv)), "max": float_value(mx(&wv))}));
                row.insert("now".into(), json!({"mean": float_value(mean(&nv)), "min": float_value(mn(&nv)), "max": float_value(mx(&nv))}));
            }
            moved.push(Value::Object(row));
        }
        Ok(moved)
    }

    #[allow(clippy::too_many_lines)]
    fn accept(&self, job: &LiveJob) -> JobResult<()> {
        let snapshot = lock(job).clone();
        if snapshot.result_authority != "authoritative" {
            return opt1(
                "accept: exploratory_non_authoritative results cannot be accepted or restored as canonical model authority",
            );
        }
        if snapshot.status != "completed" {
            return opt1(format!(
                "accept: job is {}; only a fully validated completed job may be accepted",
                snapshot.status
            ));
        }
        let job_dir = PathBuf::from(snapshot.job_dir.clone().unwrap_or_default());
        let validated =
            (|| -> JobResult<(PathBuf, Value, Option<Value>, BTreeMap<String, ArrayD<f64>>, Vec<Value>)> {
                let manifest = snapshot.managed_terminal_manifest.clone();
                if manifest.as_ref().and_then(|m| m.get("terminal_kind")).and_then(Value::as_str)
                    != Some("completed")
                {
                    return Err(verr("completed terminal seal is unavailable"));
                }
                let control = snapshot
                    .managed_control
                    .clone()
                    .ok_or_else(|| verr("managed optimization control is unavailable"))?;
                let committed = self.inner.supervisor.committed_directory(&control)?;
                self.verify_managed_terminal_seal(manifest.as_ref(), &committed)?;
                self.verify_managed_terminal_seal(manifest.as_ref(), &job_dir)?;
                let files = manifest.as_ref().and_then(|m| m.get("files")).cloned().unwrap_or(Value::Null);
                let mut best = committed.join("best.npz");
                let mut expected_best = files.get("best.npz").cloned().unwrap_or(Value::Null);
                let declared: Vec<implexity_core::contracts::ResponseSpec> = snapshot
                    .provider_responses
                    .iter()
                    .map(implexity_core::contracts::ResponseSpec::from_dict)
                    .collect::<Result<_, _>>()?;
                let sealed_history = read_json(&committed.join("history.json"), JSON_LIMIT)?;
                let sealed_spec = read_json(&committed.join("spec.json"), JSON_LIMIT)?;
                let rows =
                    sealed_history.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
                let admission = implexity_optim::constraint_admission::select_screened_history_candidate(
                    &declared,
                    Some(rows.as_slice()),
                    sealed_spec.get("schedule").and_then(Value::as_array).map(Vec::as_slice),
                )?;
                if let Some(a) = &admission {
                    let file = a.get("file").map(py_str).unwrap_or_default();
                    best = committed.join(&file);
                    expected_best = files.get(&file).cloned().unwrap_or(Value::Null);
                }
                let (arrays, _) = read_npz(&best, NPZ_LIMIT, Some(&expected_best))?;
                let values = Self::managed_design_values(&snapshot, &arrays)?;
                if let Some(a) = &admission {
                    let id = implexity_optim::design_identity(&implexity_optim::NamedArrays::from_pairs(
                        values.clone(),
                    ))?;
                    if a.get("design_state_id").and_then(Value::as_str) != Some(id.as_str()) {
                        return Err(verr(
                            "engineering screening admission and selected design identity differ",
                        ));
                    }
                }
                Ok((best, expected_best, admission, values, rows))
            })();
        let (best, expected_best, admission, values, sealed_rows) = match validated {
            Ok(v) => v,
            Err(e) => {
                return opt1(format!(
                    "accept: sealed managed terminal validation failed: {}",
                    e.message()
                ));
            }
        };
        let moved = Self::accept_moved_report(&snapshot, &values)?;
        let best_row: Option<Value> = if let Some(a) = &admission {
            a.get("epoch")
                .and_then(Value::as_u64)
                .and_then(|e| sealed_rows.get(usize::try_from(e).unwrap_or(usize::MAX)))
                .cloned()
        } else if let Some(idx) = sealed_rows
            .last()
            .and_then(|r| r.get("best_history_index"))
            .and_then(Value::as_u64)
        {
            sealed_rows
                .get(usize::try_from(idx).unwrap_or(usize::MAX))
                .cloned()
        } else if sealed_rows.is_empty() {
            None
        } else {
            let mut best_i = 0;
            for (i, r) in sealed_rows.iter().enumerate() {
                let empty = Map::new();
                let merit = |v: &Value| {
                    implexity_optim::constraint_admission::committed_merit(
                        v.as_object().unwrap_or(&empty),
                    )
                };
                if merit(r)? < merit(&sealed_rows[best_i])? {
                    best_i = i;
                }
            }
            Some(sealed_rows[best_i].clone())
        };
        let best_note = best_row.as_ref().map_or_else(String::new, |r| {
            format!(
                "; best L = {} at iteration {}",
                implexity_geometry::pyfmt::fmt_f(
                    r.get("L").and_then(Value::as_f64).unwrap_or(f64::NAN),
                    6
                ),
                r.get("i").map(py_str).unwrap_or_default()
            )
        });
        let models_dir = self.inner.models.dir().to_path_buf();
        let npz = models_dir.join(format!("model.optimised.{}.npz", snapshot.id));
        let prepared = (|| -> JobResult<Value> {
            let bundle = self.prepare_model_before_bundle(&snapshot)?;
            let fp = match artifact_fingerprint(&npz, COPY_LIMIT) {
                Ok(f) => f,
                Err(_) if !npz.exists() => {
                    let _ = copy_regular(&best, &npz, COPY_LIMIT, false);
                    artifact_fingerprint(&npz, COPY_LIMIT)?
                }
                Err(e) => return Err(e),
            };
            if fp != expected_best {
                return Err(verr("accepted best-design copy drifted"));
            }
            Ok(bundle)
        })();
        let bundle = match prepared {
            Ok(b) => b,
            Err(e) => {
                return opt1(format!(
                    "accept: immutable acceptance artifact preparation failed: {}",
                    e.message()
                ));
            }
        };
        let rep = self.apply_job_design(
            &snapshot,
            &values,
            true,
            &format!("implicit_optimize:{} accepted", snapshot.id),
        )?;
        let g = |k: &str| rep.get(k).cloned().unwrap_or(Value::Null);
        let moved_count = rep
            .get("moved")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let mut accepted = Map::new();
        for (k, v) in [
            ("path", json!(self.inner.models.path().to_string_lossy())),
            ("values_npz", json!(npz.to_string_lossy())),
            ("document_content_id", g("content_id")),
            ("document_content_id_before", g("content_id_before")),
            ("document_sha256", g("sha256")),
            ("structure_id", g("structure_id")),
            ("recompiled", g("recompiled")),
            ("moved", Value::Array(moved.clone())),
            ("node_parameters_moved", g("moved")),
            (
                "provider_derived_model_updates",
                g("provider_derived_model_updates"),
            ),
            (
                "moved_note",
                json!(
                    "'moved' is per DESIGN VARIABLE and reads from the PRE-RUN value -- what a user means by what moved.  \
                       'node_parameters_moved' is per NODE parameter and reads from the value that was in the document when \
                       accept wrote (the last live iterate), which is what the document edit itself did."
                ),
            ),
            (
                "before",
                json!({
                    "url": format!("/v1/implicit/optimize/jobs/{}/before", snapshot.id),
                    "file": bundle["path"], "sha256": snapshot.before_sha256, "sidecars": bundle["sidecars"],
                    "undo": "PUT /v1/implicit/model with the document that URL returns; it is the model exactly as it was \
                             before this run started",
                }),
            ),
        ] {
            accepted.insert(k.into(), v);
        }
        if let Some(r) = &best_row {
            accepted.insert(
                "objective".into(),
                float_value(r.get("L").and_then(Value::as_f64).unwrap_or(f64::NAN)),
            );
            accepted.insert(
                "selected_epoch".into(),
                json!(r.get("i").and_then(Value::as_i64).unwrap_or(0)),
            );
            accepted.insert(
                "selected_source_file".into(),
                json!(best.file_name().map(|n| n.to_string_lossy().into_owned())),
            );
        }
        if let Some(a) = &admission {
            accepted.insert("engineering_admission".into(), a.clone());
            accepted.insert(
                "selected_epoch".into(),
                a.get("epoch").cloned().unwrap_or(Value::Null),
            );
            accepted.insert(
                "selected_source_file".into(),
                a.get("file").cloned().unwrap_or(Value::Null),
            );
        }
        {
            let mut j = lock(job);
            j.model_content_id_owned = g("content_id");
            j.accepted = Some(Value::Object(accepted));
            j.status = "accepted".into();
            j.message = format!(
                "accepted: {} design variable(s) written into the model document ({moved_count} node parameter(s) moved), \
                 content_id {} -> {}{best_note}.  The pre-run document is at /v1/implicit/optimize/jobs/{}/before.",
                moved.len(),
                py_str(&g("content_id_before")),
                py_str(&g("content_id")),
                snapshot.id
            );
            j.mark("accepted");
        }
        let recorded = (|| -> JobResult<Value> {
            let mut rec = self.record(&snapshot.id, true)?;
            let rec_path = models_dir.join(format!("model.optimised.{}.record.json", snapshot.id));
            let mut urls = Map::new();
            urls.insert(
                "npz".into(),
                json!(format!("/v1/implicit/optimize/jobs/{}/record", snapshot.id)),
            );
            implexity_io::provenance::embed::stamp(
                &mut rec,
                &[("npz".into(), Some(npz.to_string_lossy().into_owned()))],
                Some(&rec_path),
                &urls,
                true,
            )
            .map_err(|e| JobError::runtime(e.to_string()))?;
            std::fs::copy(&rec_path, job_dir.join("record.json"))?;
            Ok(json!({"rec": rec, "path": rec_path.to_string_lossy()}))
        })();
        {
            let mut j = lock(job);
            let mut rec_value = None;
            if let Some(Value::Object(acc)) = j.accepted.as_mut() {
                match &recorded {
                    Ok(r) => {
                        let rec = &r["rec"];
                        acc.insert("record".into(), r["path"].clone());
                        acc.insert(
                            "record_id".into(),
                            rec.get("record_id").cloned().unwrap_or(Value::Null),
                        );
                        acc.insert(
                            "record_embedded".into(),
                            json!(
                                rec.get("stamped")
                                    .and_then(|s| s.get("npz"))
                                    .and_then(|n| n.get("embedded"))
                                    .is_some_and(truthy)
                            ),
                        );
                        rec_value = Some(rec.clone());
                    }
                    Err(e) => {
                        acc.insert("record".into(), Value::Null);
                        acc.insert(
                            "record_error".into(),
                            json!(format!(
                                "{}({})",
                                e.python_class(),
                                implexity_core::py_repr::repr_str(&e.message())
                            )),
                        );
                    }
                }
            }
            if rec_value.is_some() {
                j.record = rec_value;
            }
        }
        self.clear_live(job);
        let mut state = self.state();
        if state.active.as_deref() == Some(snapshot.id.as_str()) {
            state.active = None;
        }
        Ok(())
    }

    pub fn intervene(&self, job_id: &str) -> JobResult<Value> {
        let job = self.require_entry(job_id)?.live().ok_or_else(|| {
            JobError::optimize1("intervene: a restart-recovered job is inspect/stop-only")
        })?;
        let lifecycle = self.lifecycle_lock(job_id);
        let Some(_g) = lifecycle.try_hold() else {
            return opt1(format!(
                "intervene: another lifecycle transition for job {job_id} is in progress; poll and retry"
            ));
        };
        self.intervene_serialized(&job)
    }

    fn intervene_serialized(&self, job: &LiveJob) -> JobResult<Value> {
        let (status, id, provider, job_dir) = {
            let j = lock(job);
            (
                j.status.clone(),
                j.id.clone(),
                j.physics_provider.clone(),
                PathBuf::from(j.job_dir.clone().unwrap_or_default()),
            )
        };
        if status != "paused" {
            return opt1(format!(
                "manual intervention requires a paused job so no gradient process can race the user's edit; job is {status}"
            ));
        }
        self.assert_model_mutation_authority(job, "intervene", false)?;
        let snapshot = self.inner.models.snapshot()?;
        std::fs::write(
            job_dir.join("model_intervention_base.json"),
            implexity_geometry::document::dumps(&snapshot),
        )?;
        let content_id = self
            .inner
            .models
            .status()?
            .get("content_id")
            .cloned()
            .unwrap_or(Value::Null);
        {
            let mut j = lock(job);
            j.status = "intervening".into();
            j.mark("manual_intervention_opened");
            j.intervention = Some(json!({
                "state": "open", "opened_at": float_value(epoch_seconds()), "content_id": content_id,
                "base_document": "model_intervention_base.json",
                "note": "the paused iterate is now the authoritative editable model; old primal/adjoint/checkpoint evidence is invalid for further design updates",
            }));
            j.model_content_id_owned = content_id;
        }
        self.clear_live(job);
        {
            let mut state = self.state();
            if state.active.as_deref() == Some(id.as_str()) {
                state.active = None;
            }
        }
        let mut details = Map::new();
        details.insert("job_id".into(), json!(id));
        details.insert("provider".into(), json!(provider));
        if let Some(entry) = history(
            self,
            "optimization_intervention",
            "Optimization paused for manual intervention",
            &details,
        ) && let Some(hid) = entry.get("id").cloned()
            && let Some(Value::Object(iv)) = lock(job).intervention.as_mut()
        {
            iv.insert("history_id".into(), hid);
        }
        self.event(&id);
        Ok(lock(job).as_dict(false))
    }

    #[allow(clippy::too_many_lines)]
    pub fn branch_after_intervention(
        &self,
        job_id: &str,
        request: Option<&Map<String, Value>>,
    ) -> JobResult<Value> {
        let parent = self.require_entry(job_id)?.live().ok_or_else(|| {
            JobError::optimize1("this job has no open manual-intervention checkpoint")
        })?;
        let lifecycle = self.lifecycle_lock(job_id);
        let manipulation = Arc::clone(&self.inner.authoring.manipulation);
        let token = crate::private::token_hex(16)?;
        let reservation = {
            let Some(_g) = lifecycle.try_hold() else {
                return opt1(format!(
                    "branch: another lifecycle transition for job {job_id} is in progress; poll and retry"
                ));
            };
            let reservation =
                manipulation.reserve_idle("resume optimization after manual intervention", None)?;
            let setup = (|| -> JobResult<()> {
                let open = |p: &ModelOptJob| {
                    p.status == "intervening"
                        && p.intervention.as_ref().is_some_and(truthy)
                        && p.intervention
                            .as_ref()
                            .and_then(|i| i.get("state"))
                            .and_then(Value::as_str)
                            == Some("open")
                };
                if !open(&lock(&parent)) {
                    return opt1("this job has no open manual-intervention checkpoint");
                }
                self.assert_model_mutation_authority(&parent, "branch", true)?;
                let _state = self.state();
                let mut p = lock(&parent);
                if !open(&p) {
                    return opt1("this job has no open manual-intervention checkpoint");
                }
                if let Some(Value::Object(iv)) = p.intervention.as_mut() {
                    iv.insert("state".into(), json!("branch_pending"));
                    iv.insert("pending_at".into(), float_value(epoch_seconds()));
                    iv.insert("pending_token".into(), json!(token));
                    iv.insert("pending_child_job_id".into(), Value::Null);
                }
                Ok(())
            })();
            if let Err(e) = setup {
                let _ = manipulation.release_reservation(&reservation);
                return Err(e);
            }
            reservation
        };
        self.event(job_id);
        let explicit = request.is_some_and(|r| !r.is_empty());
        let mut req = match request.filter(|r| !r.is_empty()) {
            Some(r) => r.clone(),
            None => lock(&parent).request.clone(),
        };
        if req.is_empty() {
            self.restore_intervention_branch(&parent, &token);
            let _ = manipulation.release_reservation(&reservation);
            return opt1("the intervention branch has no optimization declaration to rebuild");
        }
        if !explicit {
            strip_consumed_handoff(&mut req);
        }
        let started = (|| -> JobResult<Value> {
            let job_dir = self.job_dir(&parent);
            let baseline =
                implexity_core::json::read_file(&job_dir.join("model_intervention_base.json"))
                    .map_err(JobError::runtime)?;
            let previous = Value::Object(req.clone());
            let (rebound, count) = implexity_authoring::geometry_freeze::rebind_matching_maps(
                &previous,
                &baseline,
                &self.inner.models.snapshot()?,
            )?;
            let mut next = rebound;
            if count > 0 {
                next = implexity_runtime::provider_problem_document::prepare_provider_revisions(
                    &previous, &next,
                )?
                .0;
            }
            let next = next.as_object().cloned().unwrap_or_default();
            self.preflight(&next)?;
            self.start(
                &next,
                &StartArgs {
                    parent_job_id: Some(job_id.to_string()),
                    parent_branch_token: Some(token.clone()),
                    manipulation_reservation: Some(reservation.clone()),
                    ..StartArgs::default()
                },
            )
        })();
        let _ = manipulation.release_reservation(&reservation);
        let child = match started {
            Ok(c) => c,
            Err(e) => {
                self.restore_intervention_branch(&parent, &token);
                return Err(e);
            }
        };
        let child_id = child.get("job_id").cloned().unwrap_or(Value::Null);
        {
            let _g = lifecycle.hold();
            let _state = self.state();
            let mut p = lock(&parent);
            if let Some(Value::Object(iv)) = p.intervention.as_mut()
                && iv.get("state").and_then(Value::as_str) == Some("branch_pending")
                && iv.get("pending_token").and_then(Value::as_str) == Some(token.as_str())
            {
                iv.insert("pending_child_job_id".into(), child_id);
            }
        }
        self.event(job_id);
        Ok(child)
    }

    fn restore_intervention_branch(&self, parent: &LiveJob, token: &str) {
        let id = lock(parent).id.clone();
        {
            let _g = self.lifecycle_lock(&id).hold();
            let _state = self.state();
            let mut p = lock(parent);
            if let Some(Value::Object(iv)) = p.intervention.as_mut()
                && iv.get("state").and_then(Value::as_str) == Some("branch_pending")
                && iv.get("pending_token").and_then(Value::as_str) == Some(token)
            {
                iv.insert("state".into(), json!("open"));
                iv.shift_remove("pending_at");
                iv.shift_remove("pending_token");
                iv.shift_remove("pending_child_job_id");
            }
        }
        self.event(&id);
    }

    pub fn resolve_numerical_attention(
        &self,
        job_id: &str,
        event_token: &str,
        action: &str,
    ) -> JobResult<Value> {
        let mut out = None;
        self.inner
            .host
            .physics_runtime_guard(&Value::String(job_id.into()), &mut || {
                out =
                    Some(self.resolve_numerical_attention_guarded(job_id, event_token, action)?);
                Ok(())
            })?;
        out.ok_or_else(|| JobError::runtime("numerical attention produced no reply"))
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_numerical_attention_guarded(
        &self,
        job_id: &str,
        event_token: &str,
        action: &str,
    ) -> JobResult<Value> {
        if action != "retry_exact" && action != "discard" {
            return opt1("numerical attention action must be retry_exact or discard");
        }
        let entry = self.require_entry(job_id)?;
        let Some(job) = entry.live() else {
            return opt1("restart-recovered jobs cannot resolve numerical attention");
        };
        let lifecycle = self.lifecycle_lock(job_id);
        let Some(_g) = lifecycle.try_hold() else {
            return opt1(
                "numerical attention: another lifecycle transition is in progress; poll and retry",
            );
        };
        let manipulation = Arc::clone(&self.inner.authoring.manipulation);
        let mut reservation: Option<String> = None;
        let result = (|| -> JobResult<Value> {
            if let Some(a) = lock(&job).numerical_attention.clone()
                && a.get("event_token").and_then(Value::as_str) == Some(event_token)
                && a.get("action").is_some_and(|v| !v.is_null())
            {
                if a.get("action").and_then(Value::as_str) != Some(action) {
                    return opt1("numerical attention was already resolved by a different action");
                }
                return Ok(lock(&job).as_dict(false));
            }
            let attention = require_open_numerical_attention(&lock(&job), event_token)?;
            if action == "retry_exact"
                && attention.get("exact_retry_available") != Some(&Value::Bool(true))
            {
                return opt1(
                    "exact retry is exhausted for this numerical deviation; choose exploratory continuation or discard",
                );
            }
            reservation = Some(
                manipulation
                    .reserve_idle(&format!("{action} numerical attention"), Some(job_id))?,
            );
            self.assert_model_mutation_authority(&job, action, false)?;
            {
                let mut j = lock(&job);
                if let Some(Value::Object(a)) = j.numerical_attention.as_mut() {
                    a.insert("action".into(), json!(action));
                    a.insert("result_authority".into(), json!("authoritative"));
                    a.insert(
                        "outcome".into(),
                        json!(if action == "retry_exact" {
                            "exact_retry_queued"
                        } else {
                            "discard_queued"
                        }),
                    );
                    if action == "discard" {
                        a.insert("state".into(), json!("resolved"));
                    }
                }
            }
            if action == "discard" {
                let result = self.op_serialized(&entry, "discard")?;
                if let Some(Value::Object(a)) = lock(&job).numerical_attention.as_mut() {
                    a.insert("outcome".into(), json!("discarded"));
                }
                return Ok(result);
            }
            let attention = lock(&job)
                .numerical_attention
                .clone()
                .unwrap_or(Value::Null);
            let policy = numerical_attention_policy(&attention, "retry_exact")?;
            let raw = crate::private::canonical_text(&Value::Object(policy));
            let name = numerical_attention_decision_name(&Value::String(event_token.into()))?;
            let path = self.job_dir(&job).join(&name);
            Self::managed_publish_bytes_once(&path, raw.as_bytes(), 1024 * 1024)?;
            let fp = artifact_fingerprint(&path, 1024 * 1024)?;
            {
                let mut j = lock(&job);
                let ok = j
                    .managed_terminal_manifest
                    .as_ref()
                    .and_then(|m| m.get("terminal_kind"))
                    .and_then(Value::as_str)
                    == Some("numerical_attention");
                if !ok {
                    return opt1("validated numerical-attention seal is unavailable");
                }
                if let Some(Value::Object(files)) = j
                    .managed_terminal_manifest
                    .as_mut()
                    .and_then(|m| m.get_mut("files"))
                {
                    files.insert(name, fp);
                }
                if let Some(Value::Object(a)) = j.numerical_attention.as_mut() {
                    a.insert("state".into(), json!("resolved_retry"));
                }
            }
            self.op_serialized(&entry, "retry_exact")
        })();
        if result.is_err() {
            let mut j = lock(&job);
            let attention_status = j.status.clone();
            if let Some(Value::Object(a)) = j.numerical_attention.as_mut()
                && a.get("event_token").and_then(Value::as_str) == Some(event_token)
                && attention_status == "attention"
            {
                a.insert("state".into(), json!("open"));
                a.insert("action".into(), Value::Null);
                a.insert("outcome".into(), json!("pending"));
                a.insert("result_authority".into(), Value::Null);
            }
        }
        if let Some(r) = reservation {
            let _ = manipulation.release_reservation(&r);
        }
        result
    }

    fn restore_numerical_attention_branch(&self, parent: &LiveJob, token: &str) {
        let id = lock(parent).id.clone();
        let mut hold = false;
        {
            let _g = self.lifecycle_lock(&id).hold();
            let _state = self.state();
            let mut p = lock(parent);
            if let Some(Value::Object(a)) = p.numerical_attention.as_mut()
                && a.get("state").and_then(Value::as_str) == Some("branch_pending")
                && a.get("pending_token").and_then(Value::as_str) == Some(token)
            {
                a.insert("state".into(), json!("open"));
                a.insert("action".into(), Value::Null);
                a.insert("outcome".into(), json!("pending"));
                a.insert("result_authority".into(), Value::Null);
                a.shift_remove("pending_token");
                a.shift_remove("pending_child_job_id");
                a.shift_remove("continuation_iteration_budget");
                hold = true;
            }
        }
        if hold {
            self.hold_numerical_attention_live(parent);
        }
        self.event(&id);
    }

    #[allow(clippy::too_many_lines)]
    pub fn branch_after_numerical_attention(
        &self,
        job_id: &str,
        event_token: &str,
    ) -> JobResult<Value> {
        let parent = self.require_entry(job_id)?.live().ok_or_else(|| {
            JobError::optimize1("this job has no open numerical certification deviation")
        })?;
        let lifecycle = self.lifecycle_lock(job_id);
        let manipulation = Arc::clone(&self.inner.authoring.manipulation);
        let token = crate::private::token_hex(16)?;
        let (req, policy, reservation) = {
            let Some(_g) = lifecycle.try_hold() else {
                return opt1("numerical attention branch transition is already in progress");
            };
            let replay = lock(&parent).numerical_attention.clone();
            if let Some(a) = replay.as_ref().filter(|a| {
                a.get("event_token").and_then(Value::as_str) == Some(event_token)
                    && a.get("action").and_then(Value::as_str) == Some("continue_exploratory")
            }) {
                let (req, budget) = numerical_attention_continuation_request(&lock(&parent))?;
                bind_continuation_budget(&mut lock(&parent), &budget)?;
                let req = branch_transport_request(&req);
                let child_id = a
                    .get("child_job_id")
                    .filter(|v| truthy(v))
                    .or_else(|| a.get("pending_child_job_id").filter(|v| !v.is_null()))
                    .map(py_str);
                let Some(child_id) = child_id else {
                    return opt1("numerical attention branch transition is already in progress");
                };
                return match self.entry(&child_id).and_then(|e| e.live()) {
                    Some(child) => {
                        assert_attention_child_request(&lock(&child), &req, &budget)?;
                        Ok(lock(&child).as_dict(false))
                    }
                    None => Ok(lock(&parent)
                        .numerical_attention
                        .clone()
                        .unwrap_or(Value::Null)),
                };
            }
            let attention = require_open_numerical_attention(&lock(&parent), event_token)?;
            if attention.get("exploratory_continuation_available") != Some(&Value::Bool(true)) {
                return opt1(
                    "exploratory continuation is unavailable for this numerical deviation",
                );
            }
            let (req, budget) = numerical_attention_continuation_request(&lock(&parent))?;
            let policy = numerical_attention_policy(&attention, "continue_exploratory")?;
            let reservation =
                manipulation.reserve_idle("continue after numerical attention", Some(job_id))?;
            let setup = (|| -> JobResult<()> {
                self.assert_model_mutation_authority(&parent, "continue exploratory", false)?;
                let mut p = lock(&parent);
                bind_continuation_budget(&mut p, &budget)?;
                if let Some(Value::Object(a)) = p.numerical_attention.as_mut() {
                    a.insert("state".into(), json!("branch_pending"));
                    a.insert("action".into(), json!("continue_exploratory"));
                    a.insert("outcome".into(), json!("exploratory_branch_queued"));
                    a.insert(
                        "result_authority".into(),
                        json!("exploratory_non_authoritative"),
                    );
                    a.insert("pending_token".into(), json!(token));
                    a.insert("pending_child_job_id".into(), Value::Null);
                }
                Ok(())
            })();
            if let Err(e) = setup {
                let _ = manipulation.release_reservation(&reservation);
                return Err(e);
            }
            (req, policy, reservation)
        };
        self.event(job_id);
        let req = branch_transport_request(&req);
        let started = (|| -> JobResult<Value> {
            self.preflight(&req)?;
            self.start(
                &req,
                &StartArgs {
                    parent_job_id: Some(job_id.to_string()),
                    parent_numerical_attention_token: Some(token.clone()),
                    numerical_deviation_policy: Some(Value::Object(policy.clone())),
                    result_authority: Some("exploratory_non_authoritative".into()),
                    manipulation_reservation: Some(reservation.clone()),
                    ..StartArgs::default()
                },
            )
        })();
        let _ = manipulation.release_reservation(&reservation);
        let child = match started {
            Ok(c) => c,
            Err(e) => {
                self.restore_numerical_attention_branch(&parent, &token);
                return Err(e);
            }
        };
        {
            let _g = lifecycle.hold();
            let _state = self.state();
            let mut p = lock(&parent);
            if let Some(Value::Object(a)) = p.numerical_attention.as_mut()
                && a.get("state").and_then(Value::as_str) == Some("branch_pending")
                && a.get("pending_token").and_then(Value::as_str) == Some(token.as_str())
            {
                a.insert(
                    "pending_child_job_id".into(),
                    child.get("job_id").cloned().unwrap_or(Value::Null),
                );
            }
        }
        self.event(job_id);
        Ok(child)
    }

    pub fn steer(&self, job_id: &str, req: &Map<String, Value>) -> JobResult<Value> {
        let job = self.require_entry(job_id)?.live().ok_or_else(|| {
            JobError::optimize1("steer: a restart-recovered job is inspect/stop-only")
        })?;
        let lifecycle = self.lifecycle_lock(job_id);
        let Some(_g) = lifecycle.try_hold() else {
            return opt1(format!(
                "steer: another lifecycle transition for job {job_id} is in progress; poll and retry"
            ));
        };
        self.steer_serialized(&job, req)
    }

    #[allow(clippy::too_many_lines)]
    fn steer_serialized(&self, job: &LiveJob, req: &Map<String, Value>) -> JobResult<Value> {
        let snapshot = lock(job).clone();
        if !snapshot.steerable && !snapshot.provider_execution.is_empty() {
            return opt1(
                "native provider jobs require pause, intervene and branch_after_intervention for problem changes; setting \
                 steerable:true cannot enable an unsupported path",
            );
        }
        if !snapshot.steerable {
            return opt1(format!(
                "job {} was started with steerable: false, so the model's fixed parameters are constants closed over by the \
                 compiled gradient and changing one costs a re-trace (measured: tens of seconds against 0.01 s for a step).  \
                 Start with steerable: true.",
                snapshot.id
            ));
        }
        if snapshot.status == "resuming" {
            return opt1(
                "steer: resume is establishing a new private child; poll until it is running, then retry so the target \
                 generation is unambiguous",
            );
        }
        if snapshot.status != "running" && snapshot.status != "paused" {
            return opt1(format!(
                "steer: job is {}; a steer applies at an iteration boundary, so the job has to be running (or paused, in \
                 which case it lands on resume)",
                snapshot.status
            ));
        }
        if matches!(
            snapshot.managed_requested_control.as_deref(),
            Some("stop" | "supersede")
        ) {
            return opt1("steer: a terminal control is already committed for this job");
        }
        let pending = if snapshot.status == "running" {
            let Some(control) = &snapshot.managed_control else {
                return opt1(
                    "steer: the private managed child is not established yet; poll and retry",
                );
            };
            self.inner
                .supervisor
                .partial_directory(control)?
                .join("steer.json")
        } else {
            PathBuf::from(snapshot.job_dir.clone().unwrap_or_default()).join("steer.json")
        };
        if pending.exists()
            || pending.is_symlink()
            || snapshot
                .steers
                .iter()
                .any(|q| q.get("status").and_then(Value::as_str) == Some("queued"))
        {
            return opt1(
                "steer: a steer is already queued for the next boundary.  One edit, one boundary -- wait for the ack (the \
                 opt_steer event with applied: true), then send the next.",
            );
        }
        let mut req = req.clone();
        let drive: super::super::optimize::spec::Drive = snapshot
            .drive
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        crate::optimize::spec::json_array(v).map(|a| (k.clone(), a))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if req.get("set").is_none_or(Value::is_null)
            && let Some(named) = req.get("parameters").and_then(Value::as_object).cloned()
        {
            let m = self.inner.models.require()?;
            let child = m.node(&snapshot.node).ok();
            let free_refs: BTreeSet<String> = snapshot
                .free
                .iter()
                .filter_map(|f| f.get("ref").map(py_str))
                .collect();
            let mut sets = Map::new();
            let mut problems = Vec::new();
            let mut names: Vec<&String> = named.keys().collect();
            names.sort();
            for nm in names {
                let hits: Vec<(String, String, String)> = named_refs(&m, child.as_ref(), nm)
                    .into_iter()
                    .filter(|h| drive.contains_key(&h.0) || free_refs.contains(&h.0))
                    .collect();
                if hits.len() != 1 {
                    problems.push(format!(
                        "steer: {} drives {} parameter(s) of the model being optimised{}; steer the node parameter directly with \
                         {{\"set\": {{\"model/<path>:<name>\": value}}}}",
                        implexity_core::py_repr::repr_str(nm),
                        hits.len(),
                        if hits.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", hits.iter().map(|h| h.0.clone()).collect::<Vec<_>>().join(", "))
                        }
                    ));
                    continue;
                }
                let (r, pu, nu) = hits[0].clone();
                let v = crate::optimize::spec::py_float(&named[nm])?;
                sets.insert(
                    r,
                    float_value(implexity_geometry::document::convert(v, &pu, &nu)?),
                );
            }
            if !problems.is_empty() {
                return Err(JobError::optimize(problems));
            }
            req.insert("set".into(), Value::Object(sets));
        }
        let spec = snapshot
            .spec
            .clone()
            .ok_or_else(|| JobError::optimize1("steer: this job carries no model declaration"))?;
        let d =
            crate::optimize::spec::classify_model_steer(&spec, &drive, &Value::Object(req.clone()));
        if let Some(r) = &d.refused {
            return opt1(format!("steer: {r}"));
        }
        let mut payload = req.clone();
        let mom = req
            .get("momentum")
            .map_or_else(|| py_str(&snapshot.momentum), py_str)
            .to_lowercase();
        let momentum = if mom == "keep" || mom == "reset" {
            json!(mom)
        } else {
            snapshot.momentum.clone()
        };
        payload.insert("momentum".into(), momentum.clone());
        let request_id = crate::private::token_hex(16)?;
        payload.insert("request_id".into(), json!(request_id));
        let mut ack = Map::new();
        for (k, v) in [
            ("kind", json!("implicit_steer")),
            ("job_id", json!(snapshot.id)),
            ("status", json!("queued")),
            ("applied", json!(false)),
            ("queued_at_iteration", json!(snapshot.rows.len())),
            ("request_id", json!(request_id)),
            ("hot", json!(d.hot)),
            ("changes", json!(d.changes)),
            ("keys", json!(d.sets.keys().collect::<Vec<_>>())),
            ("momentum", momentum),
            (
                "expected_cost",
                json!(if d.hot {
                    "one iteration: with steerable=true the model's fixed parameters are a TRACED ARGUMENT of the compiled \
                     gradient, so nothing is re-declared or recompiled (measured: 1.4 ms against 49 s for a re-trace)"
                } else {
                    "a re-trace and an XLA recompilation"
                }),
            ),
            (
                "calibration",
                json!(
                    "L0 and the term references were measured on the START model and are KEPT: L after this steer is a \
                       value of a different function of the design variables (the fixed parameters are part of that \
                       function), so the two ends of the loss curve are not comparable"
                ),
            ),
            (
                "note",
                json!("applied at the next iteration boundary, never inside a gradient"),
            ),
            ("poll", json!(job_url(&snapshot.id))),
        ] {
            ack.insert(k.into(), v);
        }
        let mut queued = ack.clone();
        queued.insert(
            "_sets".into(),
            Value::Object(
                d.sets
                    .iter()
                    .map(|(k, v)| (k.clone(), num_array(v)))
                    .collect(),
            ),
        );
        lock(job).steers.push(queued.clone());
        let tmp = pending.with_file_name(format!("steer.json.{request_id}.tmp"));
        let written = (|| -> JobResult<Value> {
            let text = implexity_core::json::dumps(
                &Value::Object(payload.clone()),
                &implexity_core::json::DumpOptions::indented(1),
            );
            let mut f = crate::private::create_exclusive(&tmp, 0o644)?;
            crate::private::write_all_sync(&mut f, text.as_bytes())?;
            drop(f);
            let fp = artifact_fingerprint(&tmp, 1024 * 1024)?;
            std::fs::rename(&tmp, &pending)?;
            Ok(fp)
        })();
        let fp = match written {
            Ok(fp) => fp,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                let mut j = lock(job);
                j.steers
                    .retain(|q| q.get("request_id") != Some(&json!(request_id)));
                return Err(e);
            }
        };
        {
            let mut j = lock(job);
            if let Some(q) = j
                .steers
                .iter_mut()
                .find(|q| q.get("request_id") == Some(&json!(request_id)))
            {
                q.insert("request_fingerprint".into(), fp.clone());
            }
            j.message = format!("steer queued: {}", d.changes.join("; "));
        }
        ack.insert("request_fingerprint".into(), fp);
        self.inner.host.broadcast(&json!({
            "event": "opt_steer", "kind": "implicit_optimize", "job_id": snapshot.id, "steer": Value::Object(ack.clone()),
        }));
        self.event(&snapshot.id);
        Ok(Value::Object(ack))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn steer_event(&self, job: &LiveJob, ent: &Value) -> JobResult<()> {
        let id = lock(job).id.clone();
        let _g = self.lifecycle_lock(&id).hold();
        let Some(e) = ent.as_object() else {
            return Err(verr("managed steer acknowledgement must be an object"));
        };
        let request_id = e
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(request_id) = request_id else {
            return Err(verr(
                "managed steer acknowledgement requires an exact nonempty queued request identity",
            ));
        };
        let Some(applied) = e.get("applied").and_then(Value::as_bool) else {
            return Err(verr(
                "managed steer acknowledgement applied must be boolean",
            ));
        };
        for key in ["seq", "at_iteration"] {
            if !(e.get(key).is_some_and(implexity_optim::pyval::is_int)
                && e[key].as_i64().is_some_and(|v| v >= 0))
            {
                return Err(verr(format!(
                    "managed steer acknowledgement {key} must be a nonnegative integer"
                )));
            }
        }
        if !e.get("hot").is_some_and(Value::is_boolean) {
            return Err(verr("managed steer acknowledgement hot must be boolean"));
        }
        if !e
            .get("changes")
            .and_then(Value::as_array)
            .is_some_and(|c| c.iter().all(Value::is_string))
        {
            return Err(verr(
                "managed steer acknowledgement changes must be a string list",
            ));
        }
        for key in ["t_wall", "steer_ms"] {
            if let Some(v) = e.get(key)
                && !v.as_f64().is_some_and(|f| f.is_finite() && f >= 0.0)
            {
                return Err(verr(format!(
                    "managed steer acknowledgement {key} must be finite and nonnegative"
                )));
            }
        }
        if e.get("replayed").is_some_and(|v| !v.is_boolean()) {
            return Err(verr(
                "managed steer acknowledgement replayed must be boolean",
            ));
        }
        for key in ["keys", "rebuilt"] {
            if let Some(v) = e.get(key)
                && !v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
            {
                return Err(verr(format!(
                    "managed steer acknowledgement {key} must be a string list"
                )));
            }
        }
        if applied {
            if !(e.get("regime").is_some_and(implexity_optim::pyval::is_int)
                && e["regime"].as_i64().is_some_and(|v| v >= 0))
            {
                return Err(verr(
                    "applied managed steer acknowledgement requires a nonnegative integer regime",
                ));
            }
            if e.get("refused")
                .is_some_and(|r| !(r.is_null() || r.as_str() == Some("")))
            {
                return Err(verr(
                    "applied managed steer acknowledgement cannot be refused",
                ));
            }
        } else if e
            .get("refused")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(verr(
                "refused managed steer acknowledgement requires a reason",
            ));
        }
        let rec = {
            let mut j = lock(job);
            let matches: Vec<usize> = j
                .steers
                .iter()
                .enumerate()
                .filter(|(_, q)| {
                    q.get("status").and_then(Value::as_str) == Some("queued")
                        && q.get("request_id").and_then(Value::as_str) == Some(request_id.as_str())
                })
                .map(|(i, _)| i)
                .collect();
            if matches.len() != 1 {
                return Err(verr(
                    "managed steer acknowledgement has no matching queued request identity (a unique match is required)",
                ));
            }
            let idx = matches[0];
            let mut converted: Vec<(String, ArrayD<f64>)> = Vec::new();
            if applied {
                for (k, v) in j.steers[idx]
                    .get("_sets")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default()
                {
                    let a = crate::optimize::spec::json_array(&v)
                        .filter(|a| a.iter().all(|x| x.is_finite()));
                    let Some(a) = a else {
                        return Err(verr(
                            "managed steer acknowledgement resolved a nonfinite queued value",
                        ));
                    };
                    converted.push((k, a));
                }
            }
            if applied && let Some(d) = j.drive.as_object_mut() {
                for (k, a) in &converted {
                    d.insert(k.clone(), crate::optimize::spec::safe(a));
                }
            }
            let q = &mut j.steers[idx];
            q.shift_remove("_sets");
            for (k, v) in e {
                q.insert(k.clone(), v.clone());
            }
            q.insert(
                "status".into(),
                json!(if applied { "applied" } else { "refused" }),
            );
            let rec = Value::Object(q.clone());
            if applied {
                j.regimes = e.get("regime").and_then(Value::as_i64).unwrap_or(j.regimes) + 1;
                j.mark(&format!("steer#{}", py_str(&e["seq"])));
                j.message = format!(
                    "steer #{} applied before iteration {} [{}]: {}",
                    py_str(&e["seq"]),
                    py_str(&e["at_iteration"]),
                    if e.get("hot").is_some_and(truthy) {
                        "hot -- no re-trace"
                    } else {
                        "cold"
                    },
                    e.get("changes")
                        .and_then(Value::as_array)
                        .map(|c| c.iter().map(py_str).collect::<Vec<_>>().join("; "))
                        .unwrap_or_default()
                );
            } else {
                j.message = format!(
                    "steer refused: {}",
                    e.get("refused").map(py_str).unwrap_or_default()
                );
            }
            rec
        };
        self.inner.host.broadcast(
            &json!({"event": "opt_steer", "kind": "implicit_optimize", "job_id": id, "steer": rec}),
        );
        self.event(&id);
        Ok(())
    }

    pub fn job_info(&self, job_id: &str) -> JobResult<Value> {
        if let Some(entry) = self.entry(job_id) {
            Ok(entry.as_dict(true))
        } else {
            self.stored_job_info(job_id, true)
        }
    }

    #[must_use]
    pub fn jobs_list(&self) -> Value {
        let (jobs, active) = {
            let state = self.state();
            let jobs: Vec<JobEntry> = state.jobs.values().cloned().collect();
            let active = state
                .active
                .as_ref()
                .and_then(|a| state.jobs.get(a))
                .filter(|e| !is_terminal(&e.status()))
                .map(JobEntry::id);
            (jobs, active)
        };
        let mut rows: Vec<(f64, Value)> = jobs
            .iter()
            .map(|j| (j.t_submit(), j.as_dict(false)))
            .collect();
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        let known: BTreeSet<String> = rows
            .iter()
            .filter_map(|(_, v)| v["job_id"].as_str().map(str::to_string))
            .collect();
        let mut diagnostics = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.inner.dir) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                if id.len() != 12
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || known.contains(&id)
                {
                    continue;
                }
                match self.stored_job_info(&id, false) {
                    Ok(info) => rows.push((info["t_submit"].as_f64().unwrap_or(0.0), info)),
                    Err(error) => {
                        diagnostics.push(json!({"job_id":id,"message":error.to_string()}))
                    }
                }
            }
        }
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        json!({"kind": "implicit_optimize_jobs", "jobs": rows.into_iter().map(|(_, v)| v).collect::<Vec<_>>(), "active": active, "stored_job_diagnostics":diagnostics})
    }

    pub fn before(&self, job_id: &str) -> JobResult<Value> {
        let entry = self.require_entry(job_id)?;
        let Some(job) = entry.live() else {
            return Err(JobError::of(
                "AttributeError",
                "'_RecoveredModelOptJob' object has no attribute 'before_sha256'",
            ));
        };
        let j = lock(&job);
        let values: Map<String, Value> = j
            .plan
            .iter()
            .map(|e| {
                (
                    e.get("ref").map(py_str).unwrap_or_default(),
                    e.get("start").cloned().unwrap_or(Value::Null),
                )
            })
            .collect();
        Ok(json!({
            "kind": "implicit_model_before", "job_id": j.id, "status": j.status, "sha256": j.before_sha256,
            "document": j.before_doc,
            "file": Path::new(j.job_dir.as_deref().unwrap_or("")).join("model_before.json").to_string_lossy(),
            "values": values,
            "undo": "PUT /v1/implicit/model with the value of 'document' -- it is this model exactly as it was before the \
                     run, and restoring it undoes an accept",
            "note": "model_before.json and every sidecar it references are published together in the job directory before \
                     the job becomes visible. The endpoint document remains directly PUT-able because its immutable \
                     content-addressed source sidecars are retained in model storage.",
        }))
    }

    fn read_job_json(job_dir: &Path, name: &str) -> Option<Value> {
        let p = job_dir.join(name);
        if !p.is_file() {
            return None;
        }
        implexity_core::json::read_file(&p).ok()
    }

    #[allow(clippy::too_many_lines)]
    pub fn record(&self, job_id: &str, refresh: bool) -> JobResult<Value> {
        let entry = self.require_entry(job_id)?;
        let Some(job) = entry.live() else {
            return Err(JobError::of(
                "AttributeError",
                "'_RecoveredModelOptJob' object has no attribute 'record'",
            ));
        };
        let j = lock(&job).clone();
        if let Some(r) = j.record.as_ref().filter(|_| !refresh) {
            return Ok(r.clone());
        }
        if j.rows.is_empty() && (j.status == "queued" || j.status == "running") {
            return opt1(format!(
                "job {job_id} is {} and has completed no iteration: there is nothing to make a record of yet",
                j.status
            ));
        }
        let job_dir = PathBuf::from(j.job_dir.clone().unwrap_or_default());
        let mut summary = j
            .summary
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                Self::read_job_json(&job_dir, "summary.json")
                    .and_then(|v| v.as_object().cloned())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default();
        let hist = Self::read_job_json(&job_dir, "history.json").unwrap_or_else(|| json!({}));
        let mut timeline: Vec<Value> = summary
            .get("model_timeline")
            .filter(|v| truthy(v))
            .or_else(|| hist.get("model_timeline").filter(|v| truthy(v)))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for q in &j.steers {
            if q.get("status").and_then(Value::as_str) == Some("queued") {
                let mut e: Map<String, Value> = q
                    .iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                e.insert("applied".into(), json!(false));
                e.insert("seq".into(), json!(timeline.len() + 1));
                e.insert(
                    "at_iteration".into(),
                    q.get("queued_at_iteration").cloned().unwrap_or(Value::Null),
                );
                e.insert(
                    "refused".into(),
                    json!("queued, but the run ended before the next iteration boundary, so it was never applied and moved nothing"),
                );
                timeline.push(Value::Object(e));
            }
        }
        if !summary.contains_key("L0") {
            let l0 = summary.get("l0").cloned().unwrap_or(Value::Null);
            summary.insert("L0".into(), l0);
        }
        let reg = &implexity_core::registries::global().contributions;
        let binding = implexity_authoring::physics_binding::for_provider(reg, &j.physics_provider);
        let mut case = j.case_norm.clone();
        if !case.is_object() {
            case = json!({});
        }
        if let Some(b) = &binding {
            case = b.record_case(&case, &j.objective_block);
        } else if truthy(&j.objective_block)
            && let Some(c) = case.as_object_mut()
        {
            c.insert("objective".into(), j.objective_block.clone());
        }
        let last_row = j.rows.last().cloned().unwrap_or(Value::Null);
        let summary_v = Value::Object(summary.clone());
        let objective = binding
            .as_ref()
            .and_then(|b| {
                b.record_objective(
                    &case,
                    summary.get("objective_meta").unwrap_or(&Value::Null),
                    &last_row,
                    summary.get("references").unwrap_or(&Value::Null),
                    summary.get("l0").and_then(Value::as_f64),
                )
            })
            .unwrap_or(Value::Null);
        let fidelity_items = binding
            .as_ref()
            .map(|b| b.record_fidelity(&summary_v, &case))
            .unwrap_or_default();
        let service_version = self.inner.host.service_version();
        let backend = self.inner.host.backend_name();
        let links = json!({
            "job": job_url(&j.id), "record": format!("/v1/implicit/optimize/jobs/{}/record", j.id),
            "before": format!("/v1/implicit/optimize/jobs/{}/before", j.id),
            "steer": if j.steerable { json!(format!("/v1/implicit/optimize/jobs/{}/steer", j.id)) } else { Value::Null },
            "model": "/v1/implicit/model", "job_dir_is_host_specific": true,
        });
        let sv = service_version.as_str().map(str::to_string);
        let mut rec = implexity_io::provenance::records::optimisation_record(
            &implexity_io::provenance::records::OptimisationInputs {
                job_info: j.as_dict(false),
                case_start: case.clone(),
                case_final: case.clone(),
                summary: summary_v.clone(),
                timeline: Value::Array(timeline),
                rows: Value::Array(j.rows.clone()),
                service_version: sv.as_deref(),
                backend: Some(backend.as_str()),
                links,
                accepted: j.accepted.clone().unwrap_or(Value::Null),
                artefacts: self.run_artefacts(&j),
                objective,
                fidelity_items,
                ..implexity_io::provenance::records::OptimisationInputs::default()
            },
        )
        .map_err(|e| JobError::runtime(e.to_string()))?;
        let requested = j
            .request
            .get("exact_state_handoff")
            .cloned()
            .filter(|v| !v.is_null());
        let consumed = summary
            .get("matching_time_guess_consumed")
            .filter(|v| truthy(v))
            .cloned()
            .or_else(|| j.matching_time_guess_consumed.clone());
        let produced = summary
            .get("matching_time_guess")
            .filter(|v| !v.is_null())
            .cloned();
        if requested.is_some() || consumed.is_some() || produced.is_some() {
            let mut request_evidence = Map::new();
            request_evidence.insert("schema".into(), json!("implexity-exact-state-handoff/1"));
            if let Some(r) = requested.as_ref().and_then(Value::as_object) {
                if let Some(c) = r.get("consume").and_then(Value::as_object) {
                    request_evidence.insert(
                        "consume".into(),
                        json!({"capsule_id": c.get("capsule_id"), "required": c.get("required").cloned().unwrap_or(json!(true))}),
                    );
                }
                request_evidence.insert(
                    "produce".into(),
                    r.get("produce").cloned().unwrap_or(json!(false)),
                );
            }
            let descriptor = |v: &Option<Value>| -> JobResult<Value> {
                match v {
                    Some(v) => Ok(Value::Object(
                        implexity_solve::matching_time_guess::public_descriptor(v)?,
                    )),
                    None => Ok(Value::Null),
                }
            };
            if let Some(r) = rec.as_object_mut() {
                r.insert(
                    "exact_state_handoff".into(),
                    json!({
                        "schema": "implexity-exact-state-handoff-evidence/1",
                        "method": "matching_time_newton_initial_guess",
                        "truth_status": "exact_canonical_rerun_required",
                        "canonical_cache_admission": false,
                        "request": request_evidence,
                        "consumed": descriptor(&consumed)?,
                        "produced": descriptor(&produced)?,
                        "note": "A capsule can initialize Newton only. Every receiving process reruns the canonical residual, \
                                 Jacobian, condition checks, physical guards, and requested adjoint.",
                    }),
                );
            }
        }
        let refs = summary
            .get("references")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(r) = rec.as_object_mut() {
            let mut obj = r
                .get("objective")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(l0) = summary.get("l0").filter(|v| !v.is_null()) {
                obj.insert(
                    "normalisation_L0".into(),
                    json!({
                        "value": l0,
                        "source": "the run's own summary (read back from ckpt.npz every iteration)",
                        "meaning": "the weighted sum on the START model; L is that sum divided by this, which is why L = 1 at iteration 0",
                    }),
                );
            }
            if let Some(Value::Array(terms)) = obj.get_mut("terms") {
                for t in terms.iter_mut() {
                    let key = t.get("reference_key").filter(|v| truthy(v)).map(py_str);
                    if let (Some(key), Some(tm)) = (key, t.as_object_mut())
                        && let Some(v) = refs.get(&key)
                    {
                        tm.insert("calibrated".into(), json!(true));
                        tm.insert("reference_value".into(), v.clone());
                        tm.insert(
                            "calibration_note".into(),
                            json!("normalised on the start MODEL; this is the reference the run measured"),
                        );
                    }
                }
            }
            obj.insert(
                "references".into(),
                if refs.is_empty() {
                    Value::Null
                } else {
                    Value::Object(refs.clone())
                },
            );

            if r.get("objective").is_some_and(Value::is_object) {
                r.insert("objective".into(), Value::Object(obj));
            }
            r.insert("model".into(), self.model_block(&j, &summary));
            let lr_means =
                if j.settings.get("scaling").and_then(Value::as_str) == Some("unit_range") {
                    format!(
                        "{} of EACH design variable's declared range per step",
                        implexity_geometry::pyfmt::g(j.lr)
                    )
                } else {
                    format!(
                        "{} in each parameter's RAW units per step",
                        implexity_geometry::pyfmt::g(j.lr)
                    )
                };
            if let Some(Value::Object(o)) = r.get_mut("optimiser") {
                let s = |k: &str| j.settings.get(k).cloned().unwrap_or(Value::Null);
                for (k, v) in [
                    ("driver", json!("python3 -m implexity.implicit.optimize")),
                    (
                        "node",
                        json!(
                            "the Optimize NODE: the design variables are the model's own parameters"
                        ),
                    ),
                    ("iters_per_stage", json!(j.iters)),
                    ("stages", json!(1)),
                    ("total_iters", json!(j.iters)),
                    ("scaling", s("scaling")),
                    ("band_h", s("band_h")),
                    ("eval_mode", s("eval_mode")),
                    ("smooth_r", s("smooth_r")),
                    ("model_units", s("model_units")),
                    ("lattice_volfrac", s("lattice_volfrac")),
                    ("solve_id", json!(j.solve_id)),
                    ("lr_means", json!(lr_means)),
                    (
                        "replay",
                        json!(
                            "python3 -m implexity.implicit.optimize --spec-file <spec.json> --job-dir <dir> --replay <this record's run.steers>"
                        ),
                    ),
                    (
                        "replay_note",
                        json!(
                            "the spec.json in the job directory carries the model document, the free set, the objective and the case: it is the whole input"
                        ),
                    ),
                ] {
                    o.insert(k.into(), v);
                }
            }
            let mut fidelity: Vec<Value> = r
                .get("fidelity")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for it in &mut fidelity {
                if it.get("name").and_then(Value::as_str) == Some("volume constraint")
                    && let Some(o) = it.as_object_mut()
                {
                    let weights: Vec<Value> = summary
                        .get("spec")
                        .and_then(|s| s.get("constraints"))
                        .and_then(Value::as_array)
                        .map(|c| {
                            c.iter()
                                .map(|x| x.get("weight").cloned().unwrap_or(Value::Null))
                                .collect()
                        })
                        .unwrap_or_default();
                    o.insert("status".into(), json!("approximated"));
                    o.insert(
                        "detail".into(),
                        json!("the volume budget on an Optimize node is a two-sided quadratic PENALTY, not a projection: an \
                               arbitrary implicit graph has no global level-set offset to project along.  The volume DRIFTS, and \
                               the drift is the measurement below."),
                    );
                    o.insert(
                        "measured".into(),
                        json!({
                            "target": summary.get("probe").and_then(|p| p.get("V_model")),
                            "V_first": j.rows.first().and_then(|r| r.get("V")),
                            "V_last": j.rows.last().and_then(|r| r.get("V")),
                            "drift_fraction": drift(&j.rows),
                            "penalty_weight": weights,
                        }),
                    );
                    o.insert("source".into(), json!("implicit.optimize.VolumeFraction"));
                }
            }
            fidelity.push(json!({
                "name": "the design variables", "status": "measured",
                "detail": "what moved, in the parameters' own units, from the PRE-RUN values.  This is the claim the run makes.",
                "measured": {"free": j.free, "moved": j.accepted.as_ref().and_then(|a| a.get("moved"))},
                "source": "implicit.optimize.Design",
            }));
            fidelity.push(json!({
                "name": "the frozen geometry map", "status": "declared",
                "detail": "the CASE LATTICE's control channels are not design variables of this node: they are projected once \
                           onto the case's volume fraction and held.  The material the physics sees is rho * dm.",
                "measured": summary.get("lattice"),
                "source": "implicit.optimize.Problem",
            }));
            r.insert("fidelity".into(), Value::Array(fidelity));
            if let Some(Value::Array(steers)) = r.get_mut("run").and_then(|v| v.get_mut("steers")) {
                for e in steers.iter_mut() {
                    if let Some(o) = e.as_object_mut() {
                        o.insert("kind".into(), json!("model_parameter"));
                        o.insert(
                            "note_kind".into(),
                            json!("a MODEL steer moves a FIXED parameter of the graph, not a boundary condition; the case is unchanged across it"),
                        );
                    }
                }
            }
        }
        implexity_io::provenance::records::hoist_warnings(&mut rec);
        implexity_io::provenance::finalise(&mut rec);
        if is_terminal(&j.status) {
            lock(&job).record = Some(rec.clone());
        }
        Ok(rec)
    }

    fn model_block(&self, j: &ModelOptJob, summary: &Map<String, Value>) -> Value {
        let after = self.inner.models.model().and_then(|m| {
            let doc = m.to_doc().ok()?;
            Some(json!({
                "sha256": crate::private::sha256_hex(&implexity_geometry::document::canonical_bytes(&doc)),
                "content_id": m.content_id(), "structure_id": m.structure_id(),
            }))
        });
        json!({
            "schema": j.before_doc.get("schema"), "name": j.before_doc.get("name"),
            "node": j.node, "kind": j.model_kind, "structure_id": j.structure_id,
            "content_id_start": j.content_id_start, "document_before": j.before_doc,
            "document_before_sha256": j.before_sha256, "document_after": after,
            "free": j.free, "plan": j.plan,
            "values_start": summary.get("free_start"), "values_final": summary.get("free_final"),
            "accepted": j.accepted,
            "note": "the whole document is in here because a model document IS the geometry -- the same argument case_block \
                     makes for the case.  document_before is what went in; the values that came out are values_final, and \
                     what accept wrote is accepted.moved.",
        })
    }

    fn run_artefacts(&self, j: &ModelOptJob) -> Value {
        let job_dir = PathBuf::from(j.job_dir.clone().unwrap_or_default());
        let mut out = Map::new();
        for (key, name) in [
            ("best_values_npz", "best.npz"),
            ("final_values_npz", "final_model.npz"),
            ("checkpoint_npz", "ckpt.npz"),
            ("summary_json", "summary.json"),
            ("history_json", "history.json"),
            ("job_spec_json", "spec.json"),
            ("model_before_json", "model_before.json"),
            ("section_png", "section_zmid.png"),
        ] {
            let p = job_dir.join(name);
            if p.is_file() {
                out.insert(
                    key.into(),
                    json!({"file": name, "bytes": std::fs::metadata(&p).map_or(0, |m| m.len()),
                           "sha256": crate::private::sha256_file(&p).unwrap_or_default()}),
                );
            }
        }
        if let Some(npz) = j
            .accepted
            .as_ref()
            .and_then(|a| a.get("values_npz"))
            .and_then(Value::as_str)
            .map(PathBuf::from)
            && npz.is_file()
        {
            out.insert(
                "npz".into(),
                json!({
                    "format": "npz", "role": "the accepted design variables",
                    "file": npz.file_name().map(|n| n.to_string_lossy().into_owned()),
                    "bytes_as_written": std::fs::metadata(&npz).map_or(0, |m| m.len()),
                    "sha256_as_written": crate::private::sha256_file(&npz).unwrap_or_default(),
                    "geometry": null,
                    "geometry_note": "this .npz holds the MODEL's parameter values under their ParamRef names, not a surface \
                                      and not a control field: the geometry is the document, and the document is in this record",
                }),
            );
        }
        Value::Object(out)
    }

    #[allow(clippy::too_many_lines)]
    pub fn export_epoch_document(&self, job_id: &str, epoch: i64) -> JobResult<Value> {
        let mut out = None;
        self.inner
            .host
            .physics_runtime_guard(&Value::String(job_id.into()), &mut || {
                out = Some(self.export_epoch_document_guarded(job_id, epoch)?);
                Ok(())
            })?;
        out.ok_or_else(|| JobError::runtime("epoch export produced no document"))
    }

    #[allow(clippy::too_many_lines)]
    fn export_epoch_document_guarded(&self, job_id: &str, epoch: i64) -> JobResult<Value> {
        if job_id.len() != 12
            || !job_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(verr(
                "optimization epoch export requires a 12-character lowercase hexadecimal job_id",
            ));
        }
        if epoch < 0 {
            return Err(verr(
                "optimization epoch export requires a nonnegative integer epoch",
            ));
        }
        let (snapshot, stored_directory, live_job) = if let Some(entry) = self.entry(job_id) {
            let job = entry.live().ok_or_else(|| {
                verr("recovered inspect/stop-only jobs have no checkpoint export authority")
            })?;
            let snapshot = lock(&job).clone();
            (snapshot, None, Some(job))
        } else {
            let (snapshot, directory) = self.stored_observation(job_id)?;
            self.stored_provider_guard(&directory)?;
            (snapshot, Some(directory), None)
        };
        if snapshot.provider_execution != "array" {
            return Err(verr(
                "optimization epoch export requires immutable per-epoch provider checkpoints",
            ));
        }
        let matches: Vec<Value> = snapshot
            .rows
            .iter()
            .filter(|r| {
                r.get("iteration")
                    .is_some_and(implexity_optim::pyval::is_int)
                    && r["iteration"].as_i64() == Some(epoch)
            })
            .cloned()
            .collect();
        if matches.len() != 1 {
            return Err(JobError::of(
                "KeyError",
                implexity_core::py_repr::repr_str(&format!(
                    "job {job_id} has no unique published epoch {epoch}"
                )),
            ));
        }
        let mut row = matches[0].clone();
        if row.get("i").and_then(Value::as_i64) != Some(epoch)
            || row
                .get("push")
                .and_then(|p| p.get("file"))
                .and_then(Value::as_str)
                != Some(format!("live_{epoch:06}.npz").as_str())
        {
            return Err(verr("optimization epoch row/checkpoint identity drifted"));
        }
        let (terminal_state, values) = if let Some(directory) = &stored_directory {
            (
                "succeeded".to_string(),
                self.stored_epoch_values(&snapshot, directory, &mut row)?,
            )
        } else {
            let control = snapshot
                .managed_control
                .clone()
                .ok_or_else(|| verr("managed optimization control is unavailable"))?;
            let terminal = self.inner.supervisor.status(control.operation_id())?;
            let failed = ["failed", "cancelled", "timed_out", "discarded", "abandoned"]
                .contains(&terminal.state.as_str());
            (
                terminal.state,
                self.managed_load_live_design(live_job.as_ref().unwrap(), &mut row, failed)?,
            )
        };
        let failed = ["failed", "cancelled", "timed_out", "discarded", "abandoned"]
            .contains(&terminal_state.as_str());
        let derived = self.provider_derived_values(&snapshot, &values)?;
        let overlap: Vec<&String> = values.keys().filter(|k| derived.contains_key(*k)).collect();
        if !overlap.is_empty() {
            return Err(verr(format!(
                "provider-derived outputs overlap design coordinates {}",
                implexity_core::pyobj::list_repr(&overlap)
            )));
        }
        let mut combined = values.clone();
        combined.extend(derived.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mut plan = snapshot.plan.clone();
        plan.extend(snapshot.provider_derived_plan.iter().cloned());
        let before = snapshot.before_doc.clone();
        let before_sha256 =
            crate::private::sha256_hex(&implexity_geometry::document::canonical_bytes(&before));
        if before_sha256 != snapshot.before_sha256 {
            return Err(verr("pre-run model document identity drifted"));
        }
        let job_dir = PathBuf::from(snapshot.job_dir.clone().unwrap_or_default());
        let stored = read_json(&job_dir.join("model_before.json"), 64 * 1024 * 1024)?;
        if crate::private::sha256_hex(&implexity_geometry::document::canonical_bytes(&stored))
            != before_sha256
        {
            return Err(verr("published pre-run model document identity drifted"));
        }
        let design_state_id = implexity_optim::design_identity(
            &implexity_optim::NamedArrays::from_pairs(values.clone()),
        )?;
        if Some(design_state_id.as_str()) != row.get("design_state_id").and_then(Value::as_str) {
            return Err(verr("optimization epoch design identity drifted"));
        }
        let push = row["push"].clone();
        let checkpoint_sha256 = push.get("verified_sha256").cloned().unwrap_or(Value::Null);
        if Some(&checkpoint_sha256) != push.get("sha256") {
            return Err(verr("optimization epoch checkpoint digest drifted"));
        }
        let (unbound, unbound_report) =
            implexity_authoring::model_manager::ModelManager::detached_document_with_values(
                &before,
                &plan,
                &to_nd(&combined)?,
                Some(&job_dir),
            )?;
        let mut render_before = unbound;
        let Some(doc) = render_before.as_object_mut() else {
            return Err(verr("optimization epoch model metadata is malformed"));
        };
        let meta = doc.entry("meta").or_insert_with(|| json!({}));
        let Some(meta) = meta.as_object_mut() else {
            return Err(verr("optimization epoch model metadata is malformed"));
        };
        let mut namespace = match meta.get("implexity") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(n)) => n.clone(),
            Some(_) => {
                return Err(verr(
                    "optimization epoch Implexity metadata namespace is malformed",
                ));
            }
        };
        namespace.insert(
            "render_identity".into(),
            json!({
                "schema": "implexity-optimization-render-identity/1", "job_id": job_id, "epoch": epoch,
                "solve_id": snapshot.solve_id, "design_state_id": design_state_id,
                "checkpoint_sha256": checkpoint_sha256, "model_content_id": unbound_report.get("content_id"),
            }),
        );
        meta.insert("implexity".into(), Value::Object(namespace));
        let (document, report) =
            implexity_authoring::model_manager::ModelManager::detached_document_with_values(
                &render_before,
                &[],
                &BTreeMap::new(),
                None,
            )?;
        if report.get("content_id") != unbound_report.get("content_id") {
            return Err(verr(
                "optimization render metadata changed model parameter identity",
            ));
        }
        let mut derived_names: Vec<&String> = derived.keys().collect();
        derived_names.sort();
        Ok(json!({
            "schema": "implexity-optimization-epoch-document/1",
            "kind": "implicit_optimization_epoch_document",
            "job_id": job_id, "epoch": epoch, "iteration": epoch,
            "solve_id": snapshot.solve_id, "physics_provider": snapshot.physics_provider,
            "provider_execution": "array", "result_authority": snapshot.result_authority,
            "canonical_eligible": snapshot.result_authority == "authoritative" && !failed,
            "observational_only": snapshot.result_authority != "authoritative" || failed,
            "job_terminal_state": if failed { json!(terminal_state) } else { Value::Null },
            "completed_job_terminal": terminal_state == "succeeded",
            "final_acceptance_performed": false,
            "design_state_id": design_state_id,
            "checkpoint": {"sha256": checkpoint_sha256, "bytes": push.get("bytes").and_then(Value::as_i64).unwrap_or(0)},
            "base_model_sha256": before_sha256,
            "structure_id": report.get("structure_id"), "content_id": report.get("content_id"),
            "document_sha256": report.get("sha256"), "document_bytes": report.get("document_bytes"),
            "provider_derived_model_updates": derived_names,
            "retained_physics_fields": row.get("diagnostics").and_then(|d| d.get("epoch_field_capture")),
            "document": document,
            "read_only": true, "live_model_mutated": false, "solve_started": false,
        }))
    }

    pub fn read_epoch_field(
        &self,
        job_id: &str,
        epoch: i64,
        field: &str,
        operating_point: i64,
        maximum_bytes: u64,
    ) -> JobResult<Value> {
        self.read_epoch_field_mode(
            job_id,
            epoch,
            field,
            operating_point,
            maximum_bytes,
            "payload",
        )
    }

    pub fn read_epoch_field_mode(
        &self,
        job_id: &str,
        epoch: i64,
        field: &str,
        operating_point: i64,
        maximum_bytes: u64,
        mode: &str,
    ) -> JobResult<Value> {
        let exported = self.export_epoch_document(job_id, epoch)?;
        let directory = if self.entry(job_id).is_none() {
            self.stored_observation(job_id)?.1
        } else {
            let job = self
                .require_entry(job_id)?
                .live()
                .ok_or_else(|| verr("recovered job has no retained field authority"))?;
            let control = lock(&job)
                .managed_control
                .clone()
                .ok_or_else(|| verr("managed optimization control is unavailable"))?;
            if exported
                .get("job_terminal_state")
                .is_some_and(|v| !v.is_null())
            {
                self.inner
                    .supervisor
                    .terminal_observation_directory(&control)?
            } else {
                self.managed_output_directory(Some(&control))?
            }
        };
        let mut result = crate::epoch_fields::read_recorded_field_mode(
            &directory.join("epoch_field_artifacts"),
            exported
                .get("retained_physics_fields")
                .unwrap_or(&Value::Null),
            &json!(epoch),
            &exported["design_state_id"],
            &exported["checkpoint"]["sha256"],
            field,
            operating_point,
            i64::try_from(maximum_bytes).unwrap_or(i64::MAX),
            mode,
        )?;
        if let Some(o) = result.as_object_mut() {
            o.insert("job_id".into(), json!(job_id));
        }
        Ok(result)
    }

    pub fn use_working_design(&self, payload: &Value) -> JobResult<Value> {
        let request = crate::working_result::validate_request(payload).map_err(JobError::value)?;
        let job_id = request.get("job_id").map(py_str).unwrap_or_default();
        self.require_entry(&job_id)?;
        let lifecycle = self.lifecycle_lock(&job_id);
        let Some(_g) = lifecycle.try_hold() else {
            return Err(verr(
                "Another transition is updating this run. Refresh its state and retry.",
            ));
        };
        let manipulation = Arc::clone(&self.inner.authoring.manipulation);
        let reservation =
            manipulation.reserve_idle("use result as working design", Some(&job_id))?;
        let mut out = None;
        let result =
            self.inner
                .host
                .physics_runtime_guard(&Value::Object(request.clone()), &mut || {
                    out = Some(self.adopt_working_design(&request)?);
                    Ok(())
                });
        let _ = manipulation.release_reservation(&reservation);
        result?;
        out.ok_or_else(|| JobError::runtime("working design produced no receipt"))
    }

    #[allow(clippy::too_many_lines)]
    fn adopt_working_design(&self, payload: &Map<String, Value>) -> JobResult<Value> {
        let job_id = payload.get("job_id").map(py_str).unwrap_or_default();
        let entry = self.require_entry(&job_id)?;
        let Some(job) = entry.live() else {
            return Err(verr(
                "This recovered run has no reconstructable model checkpoint in the current service.",
            ));
        };
        let snapshot = lock(&job).clone();
        let terminal_results = [
            "completed",
            "stopped",
            "error",
            "discarded",
            "accepted",
            "superseded",
        ];
        if !terminal_results.contains(&snapshot.status.as_str()) {
            return Err(verr(
                "Pause and use manual intervention, or stop the run before replacing the working model.",
            ));
        }
        if snapshot.provider_execution != "array" {
            return Err(verr(
                "This legacy run has no immutable per-epoch model snapshot. Its recorded files remain available for inspection.",
            ));
        }
        {
            let state = self.state();
            if let Some(active) = state
                .active
                .as_ref()
                .filter(|a| **a != job_id)
                .and_then(|a| state.jobs.get(a))
                && !terminal_results.contains(&active.status().as_str())
            {
                return Err(verr(
                    "Another running job owns the model. Pause or stop it before using a saved result.",
                ));
            }
        }
        let job_dir = PathBuf::from(snapshot.job_dir.clone().unwrap_or_default());
        let (rows, schedule) = match snapshot
            .managed_terminal_manifest
            .as_ref()
            .filter(|m| m.get("files").is_some_and(Value::is_object))
        {
            Some(manifest) => {
                self.verify_managed_terminal_seal(Some(manifest), &job_dir)?;
                let raw = read_json(&job_dir.join("history.json"), JSON_LIMIT)?;
                let spec = read_json(&job_dir.join("spec.json"), JSON_LIMIT)?;
                (
                    raw.get("history").cloned().unwrap_or(Value::Null),
                    spec.get("schedule").cloned().unwrap_or(Value::Null),
                )
            }
            None => (
                Value::Array(snapshot.rows.clone()),
                snapshot
                    .request
                    .get("schedule")
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
        };
        let Some(rows) = rows
            .as_array()
            .filter(|r| r.iter().all(Value::is_object))
            .cloned()
        else {
            return Err(verr(
                "The saved checkpoint history cannot be read reliably.",
            ));
        };
        let available: Vec<&Value> = rows
            .iter()
            .filter(|r| {
                r.get("i").is_some_and(implexity_optim::pyval::is_int)
                    && r.get("push").is_some_and(Value::is_object)
            })
            .collect();
        if available.is_empty() {
            return Err(verr(
                "No saved design checkpoint exists for this run. The original model and available diagnostics remain accessible.",
            ));
        }
        let epoch = match payload.get("epoch") {
            Some(e) => e.as_i64().unwrap_or(-1),
            None => available
                .iter()
                .filter_map(|r| r["i"].as_i64())
                .max()
                .unwrap_or(0),
        };
        let matches: Vec<&&Value> = available
            .iter()
            .filter(|r| r["i"].as_i64() == Some(epoch))
            .collect();
        if matches.len() != 1 {
            return Err(verr(
                "The requested epoch has no unique saved design checkpoint.",
            ));
        }
        let mut row = (*matches[0]).clone();
        if row["push"].get("file").and_then(Value::as_str)
            != Some(format!("live_{epoch:06}.npz").as_str())
        {
            return Err(verr(
                "The checkpoint file does not match the selected epoch.",
            ));
        }
        let control = snapshot
            .managed_control
            .clone()
            .ok_or_else(|| verr("managed optimization control is unavailable"))?;
        let supervisor = self.inner.supervisor.status(control.operation_id())?;
        let failed = ["failed", "cancelled", "timed_out", "discarded", "abandoned"]
            .contains(&supervisor.state.as_str());
        let values = self.managed_load_live_design(&job, &mut row, failed)?;
        let identity = implexity_optim::design_identity(
            &implexity_optim::NamedArrays::from_pairs(values.clone()),
        )?;
        let derived = self.provider_derived_values(&snapshot, &values)?;
        if values.keys().any(|k| derived.contains_key(k)) {
            return Err(verr("A derived model output overlaps a design coordinate."));
        }
        if crate::private::sha256_hex(&implexity_geometry::document::canonical_bytes(
            &snapshot.before_doc,
        )) != snapshot.before_sha256
        {
            return Err(verr("The source model identity has changed."));
        }
        let mut plan = snapshot.plan.clone();
        plan.extend(snapshot.provider_derived_plan.iter().cloned());
        let mut combined = values.clone();
        combined.extend(derived);
        let (mut document, _) =
            implexity_authoring::model_manager::ModelManager::detached_document_with_values(
                &snapshot.before_doc,
                &plan,
                &to_nd(&combined)?,
                Some(&job_dir),
            )?;
        let row_map = row.as_object().cloned().unwrap_or_default();
        let schedule_rows = schedule.as_array().cloned();
        let mut advisory = crate::working_result::assessment(
            &snapshot.provider_responses,
            &row_map,
            schedule_rows.as_deref(),
            &snapshot.status,
            &snapshot.result_authority,
        );
        let source_outcome = snapshot
            .summary
            .as_ref()
            .and_then(|s| s.get("search_outcome"))
            .cloned()
            .unwrap_or(Value::Null);
        if source_outcome.get("optimization_converged") == Some(&Value::Bool(false))
            && let Some(Value::Array(w)) = advisory.get_mut("warnings")
        {
            w.insert(
                0,
                json!("Optimization convergence was not established. You can still develop this design."),
            );
        }
        let mut provenance = Map::new();
        for (k, v) in [
            ("schema", json!("implexity-working-design-origin/1")),
            ("job_id", json!(job_id)),
            ("epoch", json!(epoch)),
            ("solve_id", json!(snapshot.solve_id)),
            ("design_state_id", json!(identity)),
            (
                "checkpoint_sha256",
                row["push"]
                    .get("verified_sha256")
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            ("source_status", json!(snapshot.status)),
            ("source_result_authority", json!(snapshot.result_authority)),
            ("assessment", advisory),
            ("source_search_outcome", source_outcome),
            ("final_acceptance_performed", json!(false)),
            ("physical_state_reused", json!(false)),
            ("adjoint_reused", json!(false)),
        ] {
            provenance.insert(k.into(), v);
        }
        let set_namespace = |doc: &mut Value, prov: &Map<String, Value>| {
            if let Some(d) = doc.as_object_mut() {
                let meta = d.entry("meta").or_insert_with(|| json!({}));
                if let Some(m) = meta.as_object_mut() {
                    let ns = m.entry("implexity").or_insert_with(|| json!({}));
                    if let Some(n) = ns.as_object_mut() {
                        n.insert("working_design".into(), Value::Object(prov.clone()));
                        n.shift_remove("render_identity");
                    }
                }
            }
        };
        set_namespace(&mut document, &provenance);
        let (result, receipt) = {
            let _live = self.inner.models.live_lock();
            let current = self.inner.models.status()?;
            if current.get("content_id") != payload.get("expected_content_id")
                || current.get("sha256") != payload.get("expected_document_sha256")
            {
                return Err(verr(
                    "The working model changed. Refresh it before replacing it with this result.",
                ));
            }
            let history_before = self.inner.host.history_bundle()?;
            let previous = self.inner.models.snapshot()?;
            let models_dir = self.inner.models.dir().to_path_buf();
            let (backup, _) =
                implexity_authoring::model_manager::ModelManager::detached_document_with_values(
                    &previous,
                    &[],
                    &BTreeMap::new(),
                    Some(&models_dir),
                )?;
            let blob = implexity_geometry::document::dumps(&backup);
            let backup_name = format!(
                "working-before-{}.json",
                crate::private::sha256_hex(blob.as_bytes())
            );
            Self::managed_publish_bytes_once(
                &models_dir.join(&backup_name),
                blob.as_bytes(),
                64 * 1024 * 1024,
            )?;
            provenance.insert("previous_model_file".into(), json!(backup_name));
            set_namespace(&mut document, &provenance);
            let result = self.inner.models.put(&document)?;
            let mut receipt = provenance.clone();
            receipt.insert(
                "document_content_id".into(),
                result.get("content_id").cloned().unwrap_or(Value::Null),
            );
            receipt.insert(
                "document_sha256".into(),
                result.get("sha256").cloned().unwrap_or(Value::Null),
            );
            receipt.insert(
                "path".into(),
                json!(self.inner.models.path().to_string_lossy()),
            );
            receipt.insert(
                "previous_content_id".into(),
                current.get("content_id").cloned().unwrap_or(Value::Null),
            );
            receipt.insert("requires_fresh_evaluation".into(), json!(true));
            {
                let mut j = lock(&job);
                j.working_design = Some(Value::Object(receipt.clone()));
                j.model_content_id_owned = result.get("content_id").cloned().unwrap_or(Value::Null);
                j.model_rollback_resolved = true;
            }
            self.clear_live(&job);
            {
                let mut state = self.state();
                if state.active.as_deref() == Some(job_id.as_str()) {
                    state.active = None;
                }
                state.model_authority_job_id = None;
            }
            match self.inner.host.record_external_history(
                &history_before,
                "Use optimization result as working design",
                "optimization_result",
            ) {
                Ok(h) => {
                    receipt.insert("history".into(), h);
                }
                Err(e) => {
                    receipt.insert("history_warning".into(), json!(e));
                }
            }

            lock(&job).working_design = Some(Value::Object(receipt.clone()));
            (result, receipt)
        };
        {
            let mut j = lock(&job);
            j.message = "Working design saved. You can edit it and optimize again. Engineering assessment remains advisory.".into();
            j.mark("working_design_saved");
        }
        self.event(&job_id);
        Ok(json!({
            "schema": "implexity-working-design-adoption/1", "saved": true,
            "working_design": receipt, "model": result, "job": lock(&job).as_dict(false),
            "final_acceptance_performed": false,
        }))
    }
}

fn strip_consumed_handoff(req: &mut Map<String, Value>) {
    if let Some(Value::Object(h)) = req.get("exact_state_handoff").cloned() {
        let mut h = h;
        h.shift_remove("consume");
        if h.get("produce") == Some(&Value::Bool(true)) {
            req.insert("exact_state_handoff".into(), Value::Object(h));
        } else {
            req.shift_remove("exact_state_handoff");
        }
    }
}

fn branch_transport_request(req: &Map<String, Value>) -> Map<String, Value> {
    let mut r = req.clone();
    strip_consumed_handoff(&mut r);
    r
}

fn require_open_numerical_attention(
    job: &ModelOptJob,
    event_token: &str,
) -> JobResult<Map<String, Value>> {
    let attention = job.numerical_attention.as_ref().and_then(Value::as_object);
    let Some(a) = attention.filter(|a| {
        job.status == "attention" && a.get("state").and_then(Value::as_str) == Some("open")
    }) else {
        return opt1("this job has no open numerical certification deviation");
    };
    if a.get("event_token").and_then(Value::as_str) != Some(event_token) {
        return opt1("numerical attention event token is stale or invalid");
    }
    Ok(a.clone())
}

fn numerical_attention_policy(
    attention: &impl AsAttention,
    action: &str,
) -> JobResult<Map<String, Value>> {
    let a = attention.attention();
    if a.get("schema").and_then(Value::as_str) != Some("implexity-numerical-attention/1")
        || !(action == "retry_exact" || action == "continue_exploratory")
    {
        return Err(verr("numerical attention policy source is invalid"));
    }
    let mut policy = Map::new();
    policy.insert(
        "schema".into(),
        json!("implexity-numerical-deviation-policy/1"),
    );
    policy.insert("action".into(), json!(action));
    for k in ["event_token", "solver_prefix", "bounded_ceiling"] {
        policy.insert(
            k.into(),
            a.get(k)
                .cloned()
                .ok_or_else(|| JobError::of("KeyError", format!("'{k}'")))?,
        );
    }
    policy.insert(
        "provenance".into(),
        a.get("provenance").cloned().unwrap_or(Value::Null),
    );
    if action == "retry_exact" {
        let budget = a.get("exact_retry_budget").cloned().unwrap_or(Value::Null);
        policy.insert(
            "retry_max_iterations_per_attempt".into(),
            budget
                .get("max_iterations_per_attempt")
                .cloned()
                .unwrap_or(Value::Null),
        );
        policy.insert(
            "retry_attempt_limit".into(),
            budget.get("attempt_limit").cloned().unwrap_or(Value::Null),
        );
    }
    crate::solver_telemetry::validate_numerical_deviation_policy(Some(&Value::Object(policy)))?
        .ok_or_else(|| verr("numerical attention policy source is invalid"))
}

trait AsAttention {
    fn attention(&self) -> Map<String, Value>;
}

impl AsAttention for Map<String, Value> {
    fn attention(&self) -> Map<String, Value> {
        self.clone()
    }
}

impl AsAttention for Value {
    fn attention(&self) -> Map<String, Value> {
        self.as_object().cloned().unwrap_or_default()
    }
}

fn numerical_attention_continuation_request(
    parent: &ModelOptJob,
) -> JobResult<(Map<String, Value>, Value)> {
    if parent.request.is_empty() {
        return opt1("numerical attention continuation state is incomplete");
    }
    let request_iters = parent.request.get("iters");
    let settings_iters = parent.settings.get("iters");
    let positive = |v: Option<&Value>| {
        v.is_some_and(implexity_optim::pyval::is_int)
            && v.and_then(Value::as_i64).is_some_and(|x| x >= 1)
    };
    if !positive(request_iters) || !positive(settings_iters) || parent.iters < 1 {
        return opt1(
            "numerical attention continuation requires one positive integer iteration cap in the request and effective settings",
        );
    }
    let ri = request_iters.and_then(Value::as_i64).unwrap_or(0);
    if !(ri == settings_iters.and_then(Value::as_i64).unwrap_or(-1) && ri == parent.iters) {
        return opt1("numerical attention continuation iteration caps disagree");
    }
    let accepted = i64::try_from(parent.rows.len()).unwrap_or(i64::MAX);
    if accepted >= ri {
        return opt1(
            "numerical attention continuation has no positive unconsumed iteration horizon",
        );
    }
    let remaining = ri - accepted;
    let mut child = parent.request.clone();
    child.insert("iters".into(), json!(remaining));
    let mut po = parent.request.clone();
    po.shift_remove("iters");
    let mut co = child.clone();
    co.shift_remove("iters");
    if super::managed_restart_request_sha256(&Value::Object(po))?
        != super::managed_restart_request_sha256(&Value::Object(co))?
    {
        return opt1(
            "numerical attention continuation changed its parent declaration outside the iteration horizon",
        );
    }
    let budget = json!({
        "schema": "implexity-numerical-attention-continuation-budget/1",
        "parent_iteration_cap": ri, "parent_accepted_iterations": accepted, "child_iteration_cap": remaining,
    });
    Ok((child, budget))
}

fn bind_continuation_budget(parent: &mut ModelOptJob, budget: &Value) -> JobResult<()> {
    let Some(Value::Object(a)) = parent.numerical_attention.as_mut() else {
        return Ok(());
    };
    if let Some(existing) = a
        .get("continuation_iteration_budget")
        .filter(|v| !v.is_null())
        && existing != budget
    {
        return opt1(
            "numerical attention continuation budget changed after it was bound to this event token",
        );
    }
    a.insert("continuation_iteration_budget".into(), budget.clone());
    Ok(())
}

fn assert_attention_child_request(
    child: &ModelOptJob,
    expected: &Map<String, Value>,
    budget: &Value,
) -> JobResult<()> {
    let cap = budget.get("child_iteration_cap").and_then(Value::as_i64);
    if Some(child.iters) != cap
        || child.settings.get("iters").and_then(Value::as_i64) != cap
        || super::managed_restart_request_sha256(&Value::Object(child.request.clone()))?
            != super::managed_restart_request_sha256(&Value::Object(expected.clone()))?
    {
        return opt1(
            "numerical attention continuation child does not match its bound server-derived request",
        );
    }
    Ok(())
}

fn named_refs(
    m: &implexity_geometry::document::Model,
    child: Option<&implexity_geometry::NodeRef>,
    name: &str,
) -> Vec<(String, String, String)> {
    let Some(child) = child else {
        return Vec::new();
    };
    let mut paths: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for (path, n) in child.walk() {
        if let Some(nid) = m.id_of(&n) {
            let mut p = vec!["model".to_string()];
            p.extend(path);
            paths.entry(nid).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    for (nid, binds) in m.bindings() {
        for (p, b) in binds {
            if let implexity_geometry::document::Binding::Bind { name: n, .. } = b
                && n == name
                && paths.get(nid).is_some_and(|v| v.len() == 1)
            {
                let units = m
                    .node(nid)
                    .ok()
                    .and_then(|node| node.info().param(p).map(|s| s.units.clone()))
                    .unwrap_or_default();
                out.push((
                    implexity_geometry::ParamRef::new(paths[nid][0].clone(), p.clone()).as_str(),
                    m.param_units()
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| "-".into()),
                    units,
                ));
            }
        }
    }
    out.sort();
    out
}

fn drift(rows: &[Value]) -> Value {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return Value::Null;
    };
    let v0 = first.get("V").and_then(Value::as_f64);
    let v1 = last.get("V").and_then(Value::as_f64);
    match (v0, v1) {
        (Some(v0), Some(v1)) if v0 != 0.0 => float_value(v1 / v0 - 1.0),
        _ => Value::Null,
    }
}
