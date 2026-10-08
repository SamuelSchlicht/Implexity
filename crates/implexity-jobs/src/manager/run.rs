// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::{Path, PathBuf};
use std::sync::Arc;

use implexity_core::pyobj::{py_str, truthy};
use serde_json::{Map, Value, json};

use super::declare::Declaration;
use super::job::ModelOptJob;
use super::{
    JobEntry, LiveJob, ModelOptimizeManager, child_env, is_terminal, lock, managed_exact_parent_resources,
};
use crate::checkpoint_scratch::{CheckpointScratch, remove_checkpoint_scratch};
use crate::error::{JobError, JobResult};
use crate::managed_evaluation::{ManagedEvaluationControl, ManagedEvaluationPolicy, StartOptions};
use crate::managed_io::JSON_LIMIT;
use crate::private::epoch_seconds;

fn opt1<T>(m: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(m))
}

fn verr(m: impl Into<String>) -> JobError {
    JobError::value(m)
}

#[derive(Debug, Clone, Default)]
pub struct StartArgs {
    pub parent_job_id: Option<String>,
    pub parent_branch_token: Option<String>,
    pub parent_numerical_attention_token: Option<String>,
    pub numerical_deviation_policy: Option<Value>,
    pub result_authority: Option<String>,
    pub manipulation_reservation: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Protocol {
    pub rows: Vec<Value>,
    pub previews: Vec<Value>,
    pub steers: Vec<Value>,
    pub halted: Option<Value>,
    pub done: Option<Value>,
    pub attention: Option<Value>,
    pub errors: Vec<Value>,
    pub terminal_frame: Option<String>,
    pub numerical_progress: Option<Value>,
}

fn is_int(v: Option<&Value>) -> bool {
    v.is_some_and(implexity_optim::pyval::is_int)
}

impl ModelOptimizeManager {

    pub(crate) fn await_superseded_active(&self, channel: &str, seq: i64) -> JobResult<()> {
        loop {
            let active = {
                let mut state = self.state();
                let Some(id) = state.active.clone() else { return Ok(()) };
                let Some(entry) = state.jobs.get(&id).cloned() else {
                    state.active = None;
                    return Ok(());
                };
                let status = entry.status();
                if is_terminal(&status) {
                    state.active = None;
                    return Ok(());
                }
                let JobEntry::Live(job) = entry else {
                    return opt1(format!(
                        "restart-recovered job {id} is still active; inspect or stop that exact job before starting another"
                    ));
                };
                let (ch, s) = {
                    let j = lock(&job);
                    (j.channel.clone(), j.seq)
                };
                if py_str(&ch) != channel || s >= seq {
                    return opt1(format!(
                        "a model optimisation is already {status} (job {id}, channel {}); stop it or supersede it on \
                         the same channel with a higher seq",
                        implexity_core::pyobj::repr(&ch)
                    ));
                }
                (id, job)
            };
            let (id, job) = active;
            let lifecycle = self.lifecycle_lock(&id);
            let terminal_event = {
                let _g = lifecycle.hold();
                {
                    let state = self.state();
                    let j = lock(&job);
                    if state.active.as_deref() != Some(id.as_str()) || is_terminal(&j.status) {
                        continue;
                    }
                    if py_str(&j.channel) != channel || j.seq >= seq {
                        return opt1("optimization admission changed while supersession was being reserved");
                    }
                }
                self.control(&job, "supersede")?;
                {
                    let mut j = lock(&job);
                    if !is_terminal(&j.status) {
                        j.status = "superseding".into();
                        j.mark("supersede_requested");
                        j.message = "supersession requested; awaiting managed descendant quiescence and document rollback".into();
                    }
                }
                let ev = Arc::clone(&lock(&job).managed_terminal_event);
                self.event(&id);
                ev
            };
            terminal_event.wait(None);
            let status = lock(&job).status.clone();
            if status != "superseded" {
                return opt1(format!("superseded job did not reach a clean managed terminal: {status}"));
            }
        }
    }


    pub fn start(&self, req: &Map<String, Value>, args: &StartArgs) -> JobResult<Value> {
        let channel = req.get("channel").map_or_else(|| "optimize".to_string(), py_str);
        let seq = match req.get("seq") {
            None => 0,
            Some(v) => crate::optimize::spec::py_int(v)?,
        };
        let manipulation = Arc::clone(&self.inner.authoring.manipulation);
        self.await_superseded_active(&channel, seq)?;
        let owned = args.manipulation_reservation.is_none();
        let reservation = match &args.manipulation_reservation {
            None => manipulation.reserve_idle("start optimization", None)?,
            Some(r) => {
                manipulation.assert_reservation(r)?;
                r.clone()
            }
        };
        let mut out: Option<Value> = None;
        let result = self.inner.host.physics_runtime_guard(&Value::Object(req.clone()), &mut || {
            out = Some(self.start_guarded(req, args)?);
            Ok(())
        });
        if owned {
            let _ = manipulation.release_reservation(&reservation);
        }
        result?;
        out.ok_or_else(|| JobError::runtime("start produced no reply"))
    }

    fn assert_intervention_admission(&self, args: &StartArgs) -> JobResult<()> {
        let jobs: Vec<LiveJob> = self.state().jobs.values().filter_map(JobEntry::live).collect();
        for job in &jobs {
            let j = lock(job);
            let Some(intervention) = j.intervention.as_ref().and_then(Value::as_object) else { continue };
            let st = intervention.get("state").and_then(Value::as_str).unwrap_or("");
            if j.status != "intervening" || !(st == "open" || st == "branch_pending") {
                continue;
            }
            let owned = args.parent_job_id.as_deref() == Some(j.id.as_str())
                && st == "branch_pending"
                && intervention.get("pending_token").and_then(Value::as_str)
                    == args.parent_branch_token.as_deref();
            if !owned {
                return opt1(
                    "an open manual intervention owns the model; resume it through that job's provenance branch or discard it",
                );
            }
        }
        for job in &jobs {
            let j = lock(job);
            let Some(attention) = j.numerical_attention.as_ref().and_then(Value::as_object) else { continue };
            let st = attention.get("state").and_then(Value::as_str).unwrap_or("");
            if j.status != "attention" || !(st == "open" || st == "branch_pending") {
                continue;
            }
            let owned = args.parent_job_id.as_deref() == Some(j.id.as_str())
                && st == "branch_pending"
                && attention.get("pending_token").and_then(Value::as_str)
                    == args.parent_numerical_attention_token.as_deref();
            if !owned {
                return opt1(
                    "an unresolved numerical certification deviation owns the accepted checkpoint; retry, continue \
                     exploratory, or discard it through that job",
                );
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn start_guarded(&self, req: &Map<String, Value>, args: &StartArgs) -> JobResult<Value> {
        if ["numerical_deviation_policy", "result_authority", "parent_numerical_attention_token"]
            .iter()
            .any(|k| req.contains_key(*k))
        {
            return opt1(
                "numerical deviation authority is server-issued and cannot be supplied in an optimization request",
            );
        }
        let channel = req.get("channel").map_or_else(|| Value::from("optimize"), |v| Value::from(py_str(v)));
        let seq = match req.get("seq") {
            None => 0,
            Some(v) => crate::optimize::spec::py_int(v)?,
        };
        self.assert_intervention_admission(args)?;
        let declaration: Map<String, Value> = req
            .iter()
            .filter(|(k, _)| k.as_str() != "exact_state_handoff" && k.as_str() != "continuation")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let Declaration { mut js, mut meta } = self.declare(&Value::Object(declaration))?;

        if let Some(raw) = req.get("continuation").filter(|v| !v.is_null()) {
            let source = raw.get("source_dir").and_then(Value::as_str).map(Path::new);
            let well_formed = raw.as_object().is_some_and(|m| {
                m.keys().all(|k| matches!(k.as_str(), "schema" | "source_dir" | "epoch"))
                    && m.get("schema").and_then(Value::as_str)
                        == Some(crate::hierarchical_job::CONTINUATION_SCHEMA)
                    && m.get("epoch").is_none_or(|e| e.is_null() || e.as_u64().is_some())
            });
            if !well_formed || source.is_none_or(|p| !p.is_absolute()) {
                return opt1(format!(
                    "continuation must be {{schema: {}, source_dir: <absolute provider-job directory>, epoch: <index or null>}}",
                    crate::hierarchical_job::CONTINUATION_SCHEMA
                ));
            }
            if source.is_some_and(|p| !p.join("history.json").is_file() || !p.join("ckpt.npz").is_file()) {
                return opt1("continuation source holds no recorded provider-job generation");
            }
            if meta.get("provider_execution").as_str() != Some("array")
                || !js.contains_key("design_coordinates")
            {
                return opt1("continuation requires a provider job with named design coordinates");
            }
            js.insert("continuation".into(), raw.clone());
        }
        if meta.get("provider_execution").as_str() == Some("array")
            && let Some(handoff) = self.matching_time_handoff(req.get("exact_state_handoff"), true)?
        {
            js.insert(
                "matching_time_guess".into(),
                super::declare::matching_time_job_declaration(Some(&handoff)).unwrap_or(Value::Null),
            );
        }
        let result_authority = args.result_authority.clone().unwrap_or_else(|| "authoritative".into());
        meta.fields.insert("request".into(), Value::Object(req.clone()));
        meta.fields.insert("parent_job_id".into(), json!(args.parent_job_id));
        meta.fields.insert("intervention_branch_token".into(), json!(args.parent_branch_token));
        meta.fields
            .insert("numerical_attention_branch_token".into(), json!(args.parent_numerical_attention_token));
        meta.fields.insert("result_authority".into(), json!(result_authority));
        if let Some(policy) = &args.numerical_deviation_policy {
            let policy = crate::solver_telemetry::validate_numerical_deviation_policy(Some(policy))?
                .ok_or_else(|| JobError::value("numerical deviation policy is invalid"))?;
            if policy.get("action").and_then(Value::as_str) != Some("continue_exploratory") {
                return opt1("fresh numerical-deviation branch must be exploratory");
            }
            if result_authority != "exploratory_non_authoritative" {
                return opt1("exploratory numerical branch must be non-authoritative");
            }
            js.insert("numerical_deviation_policy".into(), Value::Object(policy));
        }
        self.assert_intervention_admission(args)?;
        let (job, lifecycle) = {
            let mut state = self.state();
            if let Some(act) = state.active.as_ref().and_then(|id| state.jobs.get(id))
                && !is_terminal(&act.status())
            {
                return opt1(format!(
                    "optimization admission changed while its declaration was being compiled; retry after job {} reaches terminal",
                    act.id()
                ));
            }
            let mut job = ModelOptJob::new(channel, seq, &meta)?;
            let (wall, memory) = managed_exact_parent_resources("optimize", job.computation_effort.as_ref())?;
            job.managed_budget_wall_s = wall;
            job.managed_budget_memory_bytes = Some(memory);
            job.managed_budget_initialized = true;
            let job_dir = self.inner.dir.join(&job.id);
            implexity_io::fsguard::create_dir_owner_only(&job_dir)?;
            {
                let md = implexity_io::fsguard::stat_nofollow(&job_dir)?;
                if md.is_symlink() || !md.is_dir() || !md.owned {
                    return Err(JobError::runtime("new managed optimization job directory is unsafe"));
                }
            }
            job.job_dir = Some(job_dir.to_string_lossy().into_owned());
            let spec_file = job_dir.join("spec.json");
            job.spec_file = Some(spec_file.to_string_lossy().into_owned());
            if job.provider_execution == "array" {
                let topo = meta.topology_initial.clone().unwrap_or_default();
                write_npz(&job_dir.join("topology_initial.npz"), &[("topology", &topo)])?;
                js.insert("topology_file".into(), json!("topology_initial.npz"));
                let files = meta.get("coordinate_files").as_object().cloned().unwrap_or_default();
                for (coord, value) in &meta.coordinate_initial {
                    if coord == "model:control" {
                        continue;
                    }
                    if let Some(f) = files.get(coord).filter(|v| truthy(v)) {
                        write_npz(&job_dir.join(py_str(f)), &[("value", value)])?;
                    }
                }
            }
            let text = implexity_core::json::dumps(
                &Value::Object(js.clone()),
                &implexity_core::json::DumpOptions::indented(1),
            );
            let mut file = crate::private::create_exclusive(&spec_file, 0o644)?;
            crate::private::write_all_sync(&mut file, text.as_bytes())?;
            drop(file);
            job.managed_spec_fingerprint =
                Some(crate::managed_io::artifact_fingerprint(&spec_file, JSON_LIMIT)?);
            self.prepare_model_before_bundle(&job)?;
            let mut prune: Vec<String> = Vec::new();
            if state.jobs.len() >= 30 {
                let mut done: Vec<(f64, String)> = state
                    .jobs
                    .values()
                    .filter_map(|e| {
                        let status = e.status();
                        if !is_terminal(&status) {
                            return None;
                        }
                        if let JobEntry::Live(j) = e {
                            let j = lock(j);
                            let pending = status == "intervening"
                                && j.intervention
                                    .as_ref()
                                    .and_then(|i| i.get("state"))
                                    .and_then(Value::as_str)
                                    .is_some_and(|s| s == "open" || s == "branch_pending");
                            if pending {
                                return None;
                            }
                        }
                        Some((e.t_submit(), e.id()))
                    })
                    .collect();
                done.sort_by(|a, b| a.0.total_cmp(&b.0));
                let keep_from = done.len().saturating_sub(10);
                prune = done[..keep_from].iter().map(|(_, id)| id.clone()).collect();
            }
            let lifecycle = super::job::Gate::new();
            lifecycle.acquire(true);
            let id = job.id.clone();
            let free: Vec<Value> = job.plan.iter().filter_map(|e| e.get("ref").cloned()).collect();
            self.inner.models.set_live_optimisation(Some(json!({
                "job_id": id,
                "since": implexity_optim::numeric::float_value(job.t_submit),
                "job": format!("/v1/implicit/optimize/jobs/{id}"),
                "free": free,
                "note": "the design variables below are this job's latest iterate, in memory.  The stored file still \
                         holds the pre-run values; accept writes them, anything else puts them back.",
            })));
            state.lifecycle_locks.insert(id.clone(), Arc::clone(&lifecycle));
            let handle: LiveJob = Arc::new(std::sync::Mutex::new(job));
            state.jobs.insert(id.clone(), JobEntry::Live(Arc::clone(&handle)));
            state.active = Some(id.clone());
            state.model_authority_job_id = Some(id);
            for old in prune {
                state.jobs.shift_remove(&old);
            }
            (handle, lifecycle)
        };
        let (id, provider, parent) = {
            let j = lock(&job);
            (j.id.clone(), j.physics_provider.clone(), j.parent_job_id.clone())
        };
        if args.parent_numerical_attention_token.is_none() {
            let mut details = Map::new();
            details.insert("job_id".into(), json!(id));
            details.insert("provider".into(), json!(provider));
            details.insert("parent_job_id".into(), json!(parent));
            let _ = self.inner.authoring.history.append(
                "optimization_started",
                "Direct-gradient optimization started",
                Some(&details),
                None,
                None,
            );
        }
        let response = (|| -> JobResult<Value> {
            let j = lock(&job);
            let settings: Map<String, Value> = [
                "iters",
                "lr",
                "band_h",
                "eval_mode",
                "smooth_r",
                "scaling",
                "steerable",
                "live_every",
                "momentum",
                "model_units",
            ]
            .iter()
            .map(|k| {
                j.settings
                    .get(*k)
                    .cloned()
                    .map(|v| ((*k).to_string(), v))
                    .ok_or_else(|| JobError::of("KeyError", implexity_core::py_repr::repr_str(k)))
            })
            .collect::<JobResult<_>>()?;
            let mut r = Map::new();
            for (k, v) in [
                ("kind", json!("implicit_optimize")),
                ("job_id", json!(j.id)),
                ("status", json!(j.status)),
                ("node", json!(j.node)),
                ("model_kind", json!(j.model_kind)),
                ("declared_by", json!(j.declared_by)),
                ("case", j.case_name.clone()),
                ("case_source", meta.get("case_source").clone()),
                ("grid", j.grid.clone()),
                ("solve_id", json!(j.solve_id)),
                ("physics_provider", json!(j.physics_provider)),
                ("provider_execution", json!(j.provider_execution)),
                ("result_authority", json!(j.result_authority)),
                ("canonical_eligible", json!(j.result_authority == "authoritative")),
                ("settings", Value::Object(settings)),
                ("free", Value::Array(j.design_variables())),
                ("objective", j.objective_terms.clone()),
                ("warnings", Value::Array(j.warnings.clone())),
                ("ignored", Value::Object(j.ignored.clone())),
                ("steerable", json!(j.steerable)),
                ("poll", json!(format!("/v1/implicit/optimize/jobs/{}", j.id))),
                ("ops", json!(format!("/v1/implicit/optimize/jobs/{}", j.id))),
                (
                    "steer",
                    if j.steerable {
                        json!(format!("/v1/implicit/optimize/jobs/{}/steer", j.id))
                    } else {
                        Value::Null
                    },
                ),
                ("before", json!(format!("/v1/implicit/optimize/jobs/{}/before", j.id))),
                (
                    "document",
                    json!({
                        "written_now": false,
                        "what": "while this job runs, the document's free parameters ARE the optimiser's latest iterate \
                                 (in memory, so GET /v1/implicit/parameters shows them move).  The stored file keeps the \
                                 pre-run values until you accept; stop, discard, error or supersession put them back.",
                        "before": format!("/v1/implicit/optimize/jobs/{}/before", j.id),
                        "sha256_before": j.before_sha256,
                    }),
                ),
            ] {
                r.insert(k.into(), v);
            }
            if let Some(effort) = &j.computation_effort {
                r.insert(
                    "computation_effort".into(),
                    Value::Object(implexity_runtime::provider_job_authority::public_effort_view(effort)?),
                );
            }
            r.insert("supervisor".into(), j.as_dict(false).get("supervisor").cloned().unwrap_or(Value::Null));
            Ok(Value::Object(r))
        })();
        let spawned = response.as_ref().ok().map(|_| {
            let me = self.clone();
            let handle = Arc::clone(&job);
            std::thread::Builder::new()
                .name(format!("implexity-opt-{id}"))
                .spawn(move || me.run(&handle, false))
        });
        let failure = match (&response, spawned) {
            (Ok(_), Some(Ok(_))) => None,
            (Err(e), _) => Some(e.describe()),
            (Ok(_), Some(Err(e))) => Some(format!("OSError: {e}")),
            (Ok(_), None) => Some("RuntimeError: worker not spawned".into()),
        };
        if let Some(detail) = failure {
            {
                let mut j = lock(&job);
                j.status = "error".into();
                j.mark("worker_thread_start_failed");
                j.managed_supervisor_state = "failed".into();
                j.managed_supervisor_reason = Some("worker_thread_start_failed".into());
                j.error = Some(json!(detail));
                j.message = "optimization worker could not start".into();
                j.t_end = Some(epoch_seconds());
                j.model_rollback_resolved = true;
            }
            self.clear_live(&job);
            if parent.is_some() {
                self.settle_intervention_branch(&job, false, Some("worker_thread_start_failed"));
                self.settle_numerical_attention_branch(&job, false, Some("worker_thread_start_failed"));
            }
            {
                let mut state = self.state();
                if state.active.as_deref() == Some(id.as_str()) {
                    state.active = None;
                }
                if state.model_authority_job_id.as_deref() == Some(id.as_str()) {
                    state.model_authority_job_id = None;
                }
            }
            self.event(&id);
            lock(&job).managed_terminal_event.set();
            lifecycle.release();
            return opt1(format!("optimization worker could not start: {detail}"));
        }
        lifecycle.release();
        response
    }

    pub(crate) fn managed_output_directory(
        &self,
        control: Option<&ManagedEvaluationControl>,
    ) -> JobResult<PathBuf> {
        let control = control.ok_or_else(|| verr("managed optimization control is unavailable"))?;
        let status = self.inner.supervisor.status(control.operation_id())?;
        if status.state == "succeeded" {
            return Ok(self.inner.supervisor.committed_directory(control)?);
        }
        Ok(self.inner.supervisor.partial_directory(control)?)
    }

    fn managed_child_command(&self, provider_execution: &str, resume: bool) -> Vec<String> {
        let mut command = self.inner.host.worker_command();
        command.push(
            if provider_execution == "array" {
                crate::worker_cli::PROVIDER_JOB
            } else {
                crate::worker_cli::OPTIMIZE
            }
            .to_string(),
        );
        let partial = crate::managed_evaluation::MANAGED_PARTIAL_DIRECTORY_ARGUMENT;
        command.extend([
            "--spec-file".into(),
            format!("{partial}/spec.json"),
            "--job-dir".into(),
            partial.to_string(),
        ]);
        if resume {
            command.push("--resume".into());
        }
        command
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn ingest_managed_protocol_line(
        &self,
        job: &LiveJob,
        line: &str,
        protocol: &mut Protocol,
    ) -> JobResult<()> {
        if line.len() > 1024 * 1024 {
            return Err(verr("managed optimization protocol line exceeds its bound"));
        }
        let id = lock(job).id.clone();
        if let Some(body) = line.strip_prefix("NUMERICAL ") {
            if protocol.terminal_frame.is_some() {
                return Err(verr("numerical progress followed a terminal frame"));
            }
            let raw: Value =
                serde_json::from_str(body).map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?;
            let payload = implexity_runtime::numerical_progress::validate_numerical_progress(&raw)?;
            if let Some(prev) = &protocol.numerical_progress {
                let seq = payload.get("sequence").and_then(Value::as_i64).unwrap_or(-1);
                let pseq = prev.get("sequence").and_then(Value::as_i64).unwrap_or(-1);
                let el = payload.get("elapsed_s").and_then(Value::as_f64).unwrap_or(f64::NAN);
                let pel = prev.get("elapsed_s").and_then(Value::as_f64).unwrap_or(f64::NAN);
                if seq != pseq + 1 || el < pel || prev.get("capped").is_some_and(truthy) {
                    return Err(verr("numerical progress sequence/time drifted"));
                }
            } else if payload.get("sequence").and_then(Value::as_i64) != Some(1) {
                return Err(verr("numerical progress must start at sequence one"));
            }
            protocol.numerical_progress = Some(payload.clone());
            {
                let mut j = lock(job);
                j.message = format!(
                    "Numerical solve: {}",
                    payload.get("event").map(py_str).unwrap_or_default().replace('_', " ")
                );
                if let Some(report) = payload.get("solver_recovery") {
                    j.solver_recovery = Some(crate::solver_recovery::validate(report)?);
                    j.message = report.get("message").map(py_str).unwrap_or_default();
                }
                j.numerical_progress = Some(payload);
            }
            self.event(&id);
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix("PROGRESS ") {
            if protocol.terminal_frame.is_some() {
                return Err(verr("managed optimization protocol continued after its terminal frame"));
            }
            let Some((frac, message)) = rest.split_once(' ') else {
                return Err(verr("managed optimization PROGRESS line is malformed"));
            };
            let fraction: f64 = frac.trim().parse().map_err(|_| {
                verr(format!(
                    "could not convert string to float: {}",
                    implexity_core::py_repr::repr_str(frac)
                ))
            })?;
            if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                return Err(verr("managed optimization progress is invalid"));
            }
            {
                let mut j = lock(job);
                j.progress = fraction;
                j.message = message.to_string();
            }
            self.event(&id);
            return Ok(());
        }
        let prefixes = [
            ("ITER ", "rows"),
            ("PREVIEW ", "previews"),
            ("STEER ", "steers"),
            ("HALTED ", "halted"),
            ("DONE ", "done"),
            ("ERROR ", "errors"),
            ("ATTENTION ", "attention"),
        ];
        let Some((prefix, kind)) = prefixes.iter().find(|(p, _)| line.starts_with(p)) else { return Ok(()) };
        if protocol.terminal_frame.is_some() {
            return Err(verr("managed optimization protocol continued after its terminal frame"));
        }
        let payload: Value = serde_json::from_str(&line[prefix.len()..])
            .map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?;
        if !payload.is_object() {
            return Err(verr("managed optimization protocol payload is not an object"));
        }
        match *kind {
            "halted" | "done" | "attention" => {
                let slot = match *kind {
                    "halted" => &mut protocol.halted,
                    "done" => &mut protocol.done,
                    _ => &mut protocol.attention,
                };
                if slot.is_some() {
                    return Err(verr("managed optimization terminal protocol is duplicated"));
                }
                *slot = Some(payload.clone());
            }
            "rows" => protocol.rows.push(payload.clone()),
            "previews" => protocol.previews.push(payload.clone()),
            "steers" => protocol.steers.push(payload.clone()),
            _ => protocol.errors.push(payload.clone()),
        }
        if matches!(*kind, "halted" | "done" | "errors" | "attention") {
            protocol.terminal_frame = Some((*kind).to_string());
        }
        match *kind {
            "rows" => {
                if !is_int(payload.get("i")) {
                    return Err(verr("managed optimization iteration omitted its index"));
                }
                let effort = lock(job).computation_effort.clone();
                if let Some(expected) = effort {
                    let drift =
                        ["requested_policy_digest", "effective_effort_digest", "operation_context_digest"]
                            .iter()
                            .any(|k| payload.get(*k) != expected.get(*k));
                    if drift {
                        return Err(verr("managed optimization iteration effort identity drifted"));
                    }
                }
                let mut live_row = payload.clone();
                lock(job).rows.push(live_row.clone());
                self.settle_intervention_branch(job, true, None);
                self.settle_numerical_attention_branch(job, true, None);
                let control = lock(job).managed_control.clone();
                let dir = self.managed_output_directory(control.as_ref())?;
                lock(job).managed_active_dir = Some(dir.to_string_lossy().into_owned());
                self.apply_live(job, &mut live_row)?;

                let mut j = lock(job);
                if let Some(last) = j.rows.last_mut() {
                    *last = live_row;
                }
            }
            "previews" => {
                if payload.get("schema").and_then(Value::as_str)
                    != Some("implexity-optimization-preview-trace/1")
                    || payload.get("authoritative") != Some(&Value::Bool(false))
                {
                    return Err(verr("managed optimization preview frame is malformed"));
                }
                lock(job).previews.push(payload);
            }
            "steers" => self.steer_event(job, &payload)?,
            "errors" => {
                let evidence = managed_child_error_evidence(protocol);
                let mut j = lock(job);
                if let Some(report) = evidence.as_ref().and_then(|e| e.get("solver_recovery")) { j.solver_recovery = Some(report.clone()); }
                j.error = Some(evidence.unwrap_or_else(|| json!("managed child reported an invalid ERROR payload")));
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn drain_managed_output(
        &self,
        job: &LiveJob,
        protocol: &mut Protocol,
        terminal: bool,
    ) -> JobResult<()> {
        let control = lock(job)
            .managed_control
            .clone()
            .ok_or_else(|| verr("managed optimization control is unavailable"))?;
        loop {
            let offset = lock(job).managed_stdout_offset;
            let (payload, next, eof) =
                self.inner.supervisor.read_output(&control, "stdout", offset, 64 * 1024)?;
            let lines = {
                let mut j = lock(job);
                j.managed_stdout_offset = next;
                let mut pending = std::mem::take(&mut j.managed_stdout_pending);
                pending.extend_from_slice(&payload);
                if pending.len() > 1024 * 1024 && !pending.contains(&b'\n') {
                    return Err(verr("managed optimization output line exceeds its bound"));
                }
                let mut parts: Vec<Vec<u8>> = pending.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
                j.managed_stdout_pending = parts.pop().unwrap_or_default();
                parts
            };
            for encoded in lines {
                let text = String::from_utf8(encoded)
                    .map_err(|e| JobError::of("UnicodeDecodeError", e.to_string()))?;
                self.ingest_managed_protocol_line(job, text.trim_end_matches('\r'), protocol)?;
            }
            let err_offset = lock(job).managed_stderr_offset;
            let (stderr, err_next, _) =
                self.inner.supervisor.read_output(&control, "stderr", err_offset, 64 * 1024)?;
            {
                let mut j = lock(job);
                j.managed_stderr_offset = err_next;
                if !stderr.is_empty() {
                    let mut tail = j.stderr_tail.clone().unwrap_or_default();
                    tail.push_str(&String::from_utf8_lossy(&stderr));
                    let chars: Vec<char> = tail.chars().collect();
                    let start = chars.len().saturating_sub(2000);
                    j.stderr_tail = Some(chars[start..].iter().collect());
                }
            }
            if eof || payload.is_empty() {
                break;
            }
        }
        if terminal && !lock(job).managed_stdout_pending.is_empty() {
            return Err(verr("managed optimization protocol ended mid-line"));
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    pub(crate) fn run(&self, job: &LiveJob, resume: bool) {
        let mut protocol = Protocol::default();
        let lease = match self.inner.host.eval_lock() {
            Ok(l) => l,
            Err(e) => {
                self.fail_before_start(job, &format!("OSError: {e}"));
                return;
            }
        };
        if let Err(e) = lease.acquire(true, None) {
            self.fail_before_start(job, &format!("RuntimeError: {}", e.0));
            return;
        }
        let id = lock(job).id.clone();
        let (return_without_start, mut settle_unstarted_branch) = {
            let _state = self.state();
            let mut j = lock(job);
            if !["queued", "resuming", "stopping", "superseding"].contains(&j.status.as_str()) {
                (true, j.parent_job_id.is_some() && j.rows.is_empty() && j.summary.is_none())
            } else if matches!(j.managed_requested_control.as_deref(), Some("stop" | "supersede")) {
                let terminal = if j.managed_requested_control.as_deref() == Some("stop") {
                    "stopped"
                } else {
                    "superseded"
                };
                j.status = terminal.into();
                j.mark(terminal);
                j.managed_supervisor_state = "cancelled".into();
                j.managed_supervisor_reason = Some(format!("{terminal}_before_spawn"));
                j.message = format!("{} before child birth", terminal.replace('_', " "));
                j.t_end = Some(epoch_seconds());
                (true, j.parent_job_id.is_some())
            } else {
                j.status = "running".into();
                if j.t_start.is_none() {
                    j.t_start = Some(epoch_seconds());
                }
                j.mark(if resume { "resumed" } else { "running" });
                j.message = "starting a source-bounded supervised optimization child".into();
                j.managed_stdout_offset = 0;
                j.managed_stderr_offset = 0;
                j.managed_stdout_pending = Vec::new();
                (false, false)
            }
        };
        if return_without_start {
            let status = lock(job).status.clone();
            if is_terminal(&status) {
                let _ = remove_checkpoint_scratch(&self.job_dir(job));
                let mut rollback_ok = true;
                if matches!(status.as_str(), "stopped" | "superseded" | "error") {
                    let reason = lock(job)
                        .managed_supervisor_reason
                        .clone()
                        .unwrap_or_else(|| "terminal_before_worker_start".into());
                    rollback_ok = self.revert(job, &reason);
                }
                if rollback_ok {
                    self.clear_live(job);
                }
                if rollback_ok && settle_unstarted_branch {
                    self.settle_intervention_branch(job, false, Some(&status));
                    self.settle_numerical_attention_branch(job, false, Some(&status));
                    settle_unstarted_branch = false;
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
            }
            if settle_unstarted_branch && !rollback_blocks_authority_release(&lock(job)) {
                let status = lock(job).status.clone();
                self.settle_intervention_branch(job, false, Some(&status));
                self.settle_numerical_attention_branch(job, false, Some(&status));
            }
            let _ = lease.release();
            return;
        }
        self.event(&id);
        let mut resume_prefix: Vec<Value> = Vec::new();
        let mut protocol_failure: Option<JobError> = None;
        let mut terminal_guard: Option<super::job::GateGuard> = None;
        let mut checkpoint_scratch: Option<CheckpointScratch> = None;
        let outcome = (|| -> JobResult<()> {
            let mut startup_failure: Option<JobError> = None;
            if resume {
                resume_prefix = self.managed_resume_history_prefix(job)?;
            }
            {
                let mut j = lock(job);
                if !j.managed_budget_initialized {
                    let (wall, memory) =
                        managed_exact_parent_resources("optimize", j.computation_effort.as_ref())?;
                    j.managed_budget_wall_s = wall;
                    j.managed_budget_memory_bytes = Some(memory);
                    j.managed_budget_initialized = true;
                }
            }
            let (remaining, provider_execution, request, memory) = {
                let j = lock(job);
                (
                    j.managed_budget_wall_s.map(|w| (w - j.managed_consumed_wall_s).max(0.0)),
                    j.provider_execution.clone(),
                    j.request.clone(),
                    j.managed_budget_memory_bytes,
                )
            };
            if remaining.is_some_and(|r| r <= 0.0) {
                return Err(verr("managed optimization original wall-time budget is exhausted"));
            }
            let mut env = child_env()?;
            if provider_execution == "array" && implexity_runtime::exact_acceleration_production_authority::production_admission_inputs_present() {
                return Err(JobError::runtime(
                    "production acceleration authority is not available in this build (see docs/HANDOFF.md)",
                ));
            }

            let scratch = CheckpointScratch::prepare(&self.job_dir(job))?;
            env.extend(scratch.environment());
            checkpoint_scratch = Some(scratch);

            if implexity_runtime::dynamic_frames::availability::active() {
                env.extend(crate::dynamic_capture::prepare(&self.job_dir(job))?.environment());
            }
            let command = self.managed_child_command(&provider_execution, resume);
            if provider_execution == "array"
                && request.get("exact_state_handoff").is_some_and(|v| !v.is_null())
            {
                env.insert(
                    "IMPLEXITY_MATCHING_TIME_GUESS_ROOT".into(),
                    self.inner.dir.join("matching_time_newton_guesses").to_string_lossy().into_owned(),
                );
            }
            {
                let snapshot = lock(job).clone();
                self.write_managed_restart_descriptor(&snapshot, resume)?;
            }
            let control = {
                let _state = self.state();
                if is_terminal(&lock(job).status) {
                    return Ok(());
                }
                let mut policy = ManagedEvaluationPolicy::with_timeout(remaining)?;
                policy.cooperative_grace_s = 0.1;
                policy.term_grace_s = 1.0;
                policy.kill_grace_s = 2.0;
                policy.poll_interval_s = 0.05;
                policy.telemetry_interval_s = 0.25;
                policy.memory_limit_bytes = memory;
                let policy = policy.checked()?;
                let me = self.clone();
                let validated = Arc::clone(job);
                let validator: crate::managed_evaluation::TerminalValidator =
                    Arc::new(move |partial: &Path| {
                        me.validate_managed_optimization_terminal(&validated, partial)
                            .map_err(|e| e.message())
                    });
                let mut options = StartOptions::new(policy, validator);
                options.cwd = Some(self.inner.host.worker_cwd());
                options.env = env;
                options.replace_env = true;
                let prep_me = self.clone();
                let prep_job = Arc::clone(job);
                options.prepare_partial = Some(Box::new(move |partial: &Path| {
                    prep_me
                        .prepare_managed_optimization_partial(&prep_job, resume, partial)
                        .map_err(|e| e.message())
                }));
                options.recovery_owner = Some(("implicit_optimize".into(), id.clone()));
                let started = self.inner.supervisor.start(&command, options);
                let control = match started {
                    Ok(c) => c,
                    Err(e) => {
                        if resume {
                            let _ = settle_resume_steer_transfer(&self.job_dir(job), false);
                        }
                        return Err(e.into());
                    }
                };
                {
                    let mut j = lock(job);
                    j.managed_operation_id = Some(control.operation_id().to_string());
                    j.managed_control = Some(control.clone());
                    j.managed_supervisor_state = "running".into();
                }
                if resume && let Err(e) = settle_resume_steer_transfer(&self.job_dir(job), true) {
                    let _ = self.inner.supervisor.request_cancel(&control);
                    startup_failure = Some(e);
                }
                control
            };
            if matches!(lock(job).managed_requested_control.as_deref(), Some("stop" | "supersede")) {
                let _ = self.inner.supervisor.request_cancel(&control);
            }
            let dir = self.managed_output_directory(Some(&control)).ok();
            lock(job).managed_active_dir = dir.map(|d| d.to_string_lossy().into_owned());
            protocol_failure = startup_failure;
            let status = loop {
                if protocol_failure.is_none()
                    && let Err(e) = self.drain_managed_output(job, &mut protocol, false)
                {
                    protocol_failure = Some(e);
                    let _ = self.inner.supervisor.request_cancel(&control);
                }
                let status = self.inner.supervisor.wait(control.operation_id(), Some(0.1))?;
                let observation = self.inner.supervisor.observation(&control)?;
                {
                    let mut j = lock(job);
                    j.managed_supervisor_state.clone_from(&status.state);
                    j.managed_supervisor_reason.clone_from(&status.terminal_reason);
                    let mut observed = observation.to_wire().get("observed").cloned().unwrap_or(Value::Null);
                    let rss = observed.get("peak_owned_rss_bytes").and_then(Value::as_i64).unwrap_or(0);
                    let mem = observed.get("peak_owned_memory_bytes").and_then(Value::as_i64).unwrap_or(0);
                    j.managed_peak_owned_rss_bytes = j.managed_peak_owned_rss_bytes.max(rss);
                    j.managed_peak_owned_memory_bytes = j.managed_peak_owned_memory_bytes.max(mem);
                    if let Some(o) = observed.as_object_mut() {
                        o.insert("peak_owned_rss_bytes".into(), json!(j.managed_peak_owned_rss_bytes));
                        o.insert("peak_owned_memory_bytes".into(), json!(j.managed_peak_owned_memory_bytes));
                    }
                    j.managed_observation = Some(observed);
                }
                self.event(&id);
                if status.terminal {
                    if protocol_failure.is_none()
                        && let Err(e) = self.drain_managed_output(job, &mut protocol, true)
                    {
                        protocol_failure = Some(e);
                    }
                    break status;
                }
            };
            {
                let mut j = lock(job);
                let consumed = j.managed_consumed_wall_s + status.elapsed_s;
                j.managed_consumed_wall_s = j.managed_budget_wall_s.map_or(consumed, |b| b.min(consumed));
                j.managed_active_dir = None;
            }
            terminal_guard = Some(self.lifecycle_lock(&id).hold());
            let forced = lock(job).managed_requested_control.clone();
            if matches!(forced.as_deref(), Some("stop" | "supersede")) {
                {
                    let mut j = lock(job);
                    j.status =
                        if forced.as_deref() == Some("stop") { "stopped" } else { "superseded" }.into();
                    let s = j.status.clone();
                    j.mark(&s);
                    j.message = format!(
                        "{s}; managed child tree is absent ({}) and partial work was not published",
                        j.managed_supervisor_reason.clone().unwrap_or_else(|| "None".into())
                    );
                }
                if self.revert(job, "forced terminal after descendant quiescence") {
                    self.clear_live(job);
                }
            } else if let Some(e) = protocol_failure.take() {
                return Err(e);
            } else if status.state == "succeeded" {
                let committed = self.inner.supervisor.committed_directory(&control)?;
                self.finish_managed_success(job, &protocol, &committed, &resume_prefix)?;
            } else {
                return Err(verr(format!(
                    "managed optimization terminal {}: {}",
                    status.state,
                    status.terminal_reason.clone().unwrap_or_else(|| "None".into())
                )));
            }
            Ok(())
        })();
        if let Err(exc) = outcome {
            if terminal_guard.is_none() {
                terminal_guard = Some(self.lifecycle_lock(&id).hold());
            }
            let is_superseded = lock(job).status == "superseded";
            if !is_superseded {
                {
                    let mut j = lock(job);
                    j.status = "error".into();
                    j.mark("error");
                    let child_error = if protocol_failure.is_none()
                        && j.managed_supervisor_reason.as_deref() == Some("child_failed")
                    {
                        managed_child_error_evidence(&protocol)
                    } else {
                        None
                    };
                    if let Some(report) = child_error.as_ref().and_then(|e| e.get("solver_recovery")) { j.solver_recovery = Some(report.clone()); }
                    j.error = Some(child_error.unwrap_or_else(|| json!(exc.describe())));
                    if j.managed_supervisor_reason.is_none() {
                        j.managed_supervisor_reason = Some("supervision_failed".into());
                    }
                }
                self.revert(job, "forced or invalid terminal");
            }
        }

        match checkpoint_scratch.take() {
            Some(scratch) => {
                let _ = scratch.remove();
            }
            None => {
                let _ = remove_checkpoint_scratch(&self.job_dir(job));
            }
        }
        if terminal_guard.is_none() {
            terminal_guard = Some(self.lifecycle_lock(&id).hold());
        }
        {
            let (parent, blocks, rows_empty, has_summary, detail) = {
                let j = lock(job);
                (
                    j.parent_job_id.clone(),
                    rollback_blocks_authority_release(&j),
                    j.rows.is_empty(),
                    j.summary.is_some(),
                    j.error.as_ref().map_or_else(|| j.status.clone(), py_str),
                )
            };
            if parent.is_some() && !blocks && rows_empty && !has_summary {
                self.settle_intervention_branch(job, false, Some(&detail));
                self.settle_numerical_attention_branch(job, false, Some(&detail));
            } else if parent.is_some() && !blocks && has_summary {
                self.settle_intervention_branch(job, true, None);
                self.settle_numerical_attention_branch(job, true, None);
            }
            let clear = {
                let mut j = lock(job);
                j.t_end = Some(epoch_seconds());
                is_terminal(&j.status) && !rollback_blocks_authority_release(&j)
            };
            if clear {
                self.clear_live(job);
            }
            self.event(&id);
            lock(job).managed_terminal_event.set();
        }
        drop(terminal_guard);
        let _ = lease.release();
    }

    fn fail_before_start(&self, job: &LiveJob, detail: &str) {
        let id = {
            let mut j = lock(job);
            j.status = "error".into();
            j.mark("error");
            j.error = Some(json!(detail));
            j.managed_supervisor_reason = Some("supervision_failed".into());
            j.t_end = Some(epoch_seconds());
            j.id.clone()
        };
        if self.revert(job, "forced or invalid terminal") {
            self.clear_live(job);
        }
        self.event(&id);
        lock(job).managed_terminal_event.set();
    }

    pub(crate) fn job_dir(&self, job: &LiveJob) -> PathBuf {
        PathBuf::from(lock(job).job_dir.clone().unwrap_or_default())
    }
}

pub(crate) fn rollback_blocks_authority_release(job: &ModelOptJob) -> bool {
    matches!(job.status.as_str(), "stopped" | "superseded" | "error")
        && !job.accepted.as_ref().is_some_and(truthy)
        && !job.model_rollback_resolved
}


pub(crate) fn settle_resume_steer_transfer(job_dir: &Path, child_started: bool) -> JobResult<()> {
    let pending = job_dir.join("steer.json");
    let transfer = job_dir.join("steer.resume-transfer.json");
    if !(transfer.exists() || transfer.is_symlink()) {
        return Ok(());
    }
    if child_started {
        std::fs::remove_file(&transfer)?;
        return Ok(());
    }
    if pending.exists() || pending.is_symlink() {
        return Err(verr("managed optimization steer transfer restoration is ambiguous"));
    }
    std::fs::rename(&transfer, &pending)?;
    Ok(())
}

fn write_npz(path: &Path, arrays: &[(&str, &ndarray::ArrayD<f64>)]) -> JobResult<()> {
    let members: Vec<(String, implexity_io::npy::NpyArray)> =
        arrays.iter().map(|(k, a)| ((*k).to_string(), implexity_io::npy::NpyArray::from_f64(a))).collect();
    let refs: Vec<(&str, &implexity_io::npy::NpyArray)> =
        members.iter().map(|(k, a)| (k.as_str(), a)).collect();
    let bytes = implexity_io::npz::save(&refs).map_err(|e| JobError::runtime(e.to_string()))?;
    std::fs::write(path, bytes)?;
    Ok(())
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub(crate) fn managed_child_error_evidence(protocol: &Protocol) -> Option<Value> {
    if protocol.errors.len() != 1 {
        return None;
    }
    let payload = protocol.errors[0].as_object()?;
    let raw_error = match payload.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return None,
    };
    let raw_problems: Vec<Value> = match payload.get("problems") {
        None | Some(Value::Null) => {
            raw_error.iter().filter(|e| !e.is_empty()).map(|e| Value::String(e.clone())).collect()
        }
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return None,
    };
    if raw_problems.len() > 16 || raw_problems.iter().any(|v| !v.is_string()) {
        return None;
    }
    let mut truncated = false;
    let mut bounded = |value: &str| -> String {
        let text = value.trim();
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > 4096 {
            truncated = true;
            chars[..4096].iter().collect()
        } else {
            text.to_string()
        }
    };
    let error = raw_error.as_deref().filter(|e| !e.is_empty()).map(&mut bounded);
    let mut problems: Vec<String> = raw_problems
        .iter()
        .filter_map(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(&mut bounded)
        .collect();
    if problems.is_empty()
        && let Some(e) = &error
    {
        problems = vec![e.clone()];
    }
    if problems.is_empty() {
        return None;
    }
    let mut numerical = payload.get("numerical_solver_failure").filter(|v| !v.is_null()).cloned();
    if let Some(nf) = &numerical {
        let expected = [
            "schema",
            "classification",
            "solver_record",
            "observed_relative_residual",
            "nominal_relative_residual_limit",
            "bounded_ceiling",
        ];
        let nfo = nf.as_object()?;
        let classification = nfo.get("classification").and_then(Value::as_str).unwrap_or("");
        if nfo.len() != expected.len()
            || !expected.iter().all(|k| nfo.contains_key(*k))
            || nfo.get("schema").and_then(Value::as_str) != Some("implexity-numerical-solver-failure/1")
            || !["nonfinite_residual", "outside_bounded_envelope", "attention_scope_unavailable"]
                .contains(&classification)
        {
            return None;
        }
        let record = crate::solver_telemetry::validate_numerical_solver_record(&nfo["solver_record"]).ok()?;
        let nominal = implexity_optim::pyval::py_float(&nfo["nominal_relative_residual_limit"]).ok()?;
        let ceiling = implexity_optim::pyval::py_float(&nfo["bounded_ceiling"]).ok()?;
        let cert = record.get("certification")?;
        if !nominal.is_finite()
            || nominal <= 0.0
            || !ceiling.is_finite()
            || ceiling <= nominal
            || cert.get("passed") != Some(&Value::Bool(false))
            || cert.get("relative_residual_limit").and_then(Value::as_f64) != Some(nominal)
        {
            return None;
        }
        let observed = nfo.get("observed_relative_residual").cloned().unwrap_or(Value::Null);
        let final_residual = record.get("final_relative_residual").cloned().unwrap_or(Value::Null);
        if classification == "nonfinite_residual" {
            if !observed.is_null() || !final_residual.is_null() {
                return None;
            }
        } else {
            let o = observed.as_f64().filter(|v| v.is_finite())?;
            if final_residual.as_f64() != Some(o) {
                return None;
            }
            if classification == "outside_bounded_envelope" && !(o > ceiling) {
                return None;
            }
            if classification == "attention_scope_unavailable" && !(nominal < o && o <= ceiling) {
                return None;
            }
        }
        numerical = Some(nf.clone());
    }
    let recovery = payload.get("solver_recovery").map(crate::solver_recovery::validate).transpose().ok()?;
    let encoded = crate::private::canonical_text(&protocol.errors[0]);
    let mut result = Map::new();
    result.insert("schema".into(), json!("implexity-managed-child-error/1"));
    result.insert("kind".into(), json!("managed_child_error"));
    result.insert("error".into(), json!(error.unwrap_or_else(|| problems[0].clone())));
    result.insert("problems".into(), json!(problems));
    result.insert("source_payload_sha256".into(), json!(crate::private::sha256_hex(encoded.as_bytes())));
    result.insert("text_truncated".into(), json!(truncated));
    if let Some(report) = recovery { result.insert("solver_recovery".into(), report); }
    if let Some(nf) = numerical {
        result.insert("numerical_solver_failure".into(), nf);
    }
    Some(Value::Object(result))
}
