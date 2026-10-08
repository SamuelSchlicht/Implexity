// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeSet;
use std::path::Path;

use implexity_core::pyobj::{py_str, truthy};
use implexity_optim::numeric::float_value;
use implexity_optim::provider_ops::{DesignOp, design_operations};
use serde_json::{Map, Value, json};

use super::declare::Declaration;
use super::job::JobMeta;
use super::{ModelOptimizeManager, child_env};
use crate::error::{JobError, JobResult};

fn opt1<T>(m: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(m))
}

fn env_seconds(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn dumps(v: &Value) -> String {
    implexity_core::json::dumps(v, &implexity_core::json::DumpOptions::default())
}

fn declaration_echo(rep: &mut Map<String, Value>, js: &Map<String, Value>, meta: &JobMeta) {
    rep.insert("node".into(), js.get("node").cloned().unwrap_or(Value::Null));
    rep.insert("declared_by".into(), meta.get("declared_by").clone());
    rep.insert("case_source".into(), meta.get("case_source").clone());
    rep.insert("free_plan".into(), meta.get("plan").clone());
    rep.insert("warnings".into(), meta.get("warnings").clone());
    rep.insert("ignored".into(), meta.get("ignored").clone());
}

fn filtered(req: &Map<String, Value>, drop: &[&str]) -> Value {
    Value::Object(
        req.iter()
            .filter(|(k, _)| !drop.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

fn triplet(v: Option<&Value>) -> [i64; 3] {
    let vals: Vec<i64> = v
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| crate::optimize::spec::py_int(x).ok()).collect())
        .unwrap_or_default();
    <[i64; 3]>::try_from(vals.as_slice()).unwrap_or([32, 32, 32])
}

impl ModelOptimizeManager {
    #[allow(clippy::too_many_arguments)]
    fn legacy_analysis(
        &self,
        js: &Map<String, Value>,
        subdir: &str,
        flag: Option<(&str, &Value)>,
        prefix: &str,
        busy: &str,
        timeout_env: (&str, f64),
        noun: &str,
    ) -> JobResult<(Map<String, Value>, f64)> {
        let lease = self.inner.host.eval_lock()?;
        if !lease.acquire(false, None).map_err(|e| JobError::runtime(e.0))? {
            return opt1(format!("a heavy process is already running{}; {busy}", self.heavy_busy_suffix()));
        }
        let run = (|| -> JobResult<(Option<crate::worker_cli::ChildOutput>, f64)> {
            let tmp = self.inner.dir.join(subdir);
            std::fs::create_dir_all(&tmp)?;
            let spec_file = tmp.join("spec.json");
            std::fs::write(&spec_file, dumps(&Value::Object(js.clone())))?;
            let mut command = self.inner.host.worker_command();
            command.extend([
                crate::worker_cli::OPTIMIZE.to_string(),
                "--spec-file".into(),
                spec_file.to_string_lossy().into_owned(),
            ]);
            match flag {
                Some((f, request)) => {
                    let request_file = tmp.join("request.json");
                    std::fs::write(&request_file, dumps(request))?;
                    command.extend([f.to_string(), request_file.to_string_lossy().into_owned()]);
                }
                None => command.push("--preflight".into()),
            }
            let t0 = crate::private::epoch_seconds();
            let timeout = env_seconds(timeout_env.0, timeout_env.1);
            let out = crate::worker_cli::run_captured(
                &command,
                &self.inner.host.worker_cwd(),
                &child_env()?,
                crate::worker_cli::duration_from_secs(timeout),
            )?;
            Ok((out, t0))
        })();
        let _ = lease.release();
        let (out, t0) = run?;
        let Some(out) = out else {
            return Err(JobError::of(
                "TimeoutExpired",
                format!(
                    "the {noun} subprocess timed out after {} seconds",
                    env_seconds(timeout_env.0, timeout_env.1)
                ),
            ));
        };
        let line_prefix = format!("{prefix} ");
        let mut rep: Option<Map<String, Value>> = None;
        let mut err: Option<Value> = None;
        for line in out.stdout.lines() {
            if let Some(body) = line.strip_prefix(&line_prefix) {
                rep = Some(
                    serde_json::from_str(body).map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?,
                );
            } else if let Some(body) = line.strip_prefix("ERROR ") {
                err = Some(
                    serde_json::from_str(body).unwrap_or_else(|_| json!({"error": body, "problems": [body]})),
                );
            }
        }
        let Some(rep) = rep else {
            let mut problems: Vec<String> = err
                .as_ref()
                .and_then(|e| e.get("problems"))
                .and_then(Value::as_array)
                .filter(|p| !p.is_empty())
                .map_or_else(
                    || {
                        vec![format!(
                            "the {noun} subprocess exited {} and printed no {prefix} line",
                            out.returncode
                        )]
                    },
                    |p| p.iter().map(py_str).collect(),
                );
            let tail: String =
                out.stderr.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
            if !tail.is_empty() && err.is_none() {
                problems.push(format!("stderr tail: {tail}"));
            }
            return Err(JobError::optimize(problems));
        };
        Ok((rep, t0))
    }


    #[allow(clippy::too_many_lines)]
    pub fn preflight(&self, req: &Map<String, Value>) -> JobResult<Value> {
        let Declaration { js, meta } = self.declare(&filtered(req, &["exact_state_handoff"]))?;
        if meta.get("provider_execution").as_str() == Some("array") {
            self.matching_time_handoff(req.get("exact_state_handoff"), true)?;
            let provider = implexity_core::registries::global().providers.get(&meta.s("physics_provider"))?;
            let caps = provider.capabilities()?;
            let ops = design_operations(provider.as_ref());
            let profile_aware = caps
                .get("traits")
                .and_then(|t| t.get("computation_effort").cloned())
                .is_some_and(|v| !v.is_null())
                || ops.is_some_and(implexity_optim::DesignOperations::has_computation_effort_scope);
            let mut rep: Map<String, Value> = if profile_aware {
                self.run_provider_worker(&meta, "preflight", None, None, None, None)?
            } else {
                let started = crate::private::perf_counter_ns();
                let topology = self.provider_current_topology(&meta)?;
                let inner = (|| -> JobResult<(Map<String, Value>, Value, Value)> {
                    let problem = provider.normalise_problem(meta.get("provider_problem"))?;
                    let projected = match ops.filter(|o| o.provides(DesignOp::ProjectTopology)) {
                        Some(o) => o.project_topology(&problem, &topology, &topology)?,
                        None => topology.clone(),
                    };
                    let mut rep = match ops.filter(|o| o.provides(DesignOp::PreflightDesign)) {
                        Some(o) => {
                            let design = self.provider_current_design(&meta)?;
                            let named = implexity_optim::NamedArrays::from_pairs(design);
                            o.preflight_design(&problem, &named)?
                        }
                        None => provider.preflight(&problem, Some(&projected))?,
                    };
                    let coupling = implexity_core::coupling_graph::validate_provider_couplings(
                        provider.as_ref(),
                        Some(&problem),
                        &implexity_core::registries::global().extensions,
                        true,
                    );
                    if !coupling.get("ok").is_some_and(truthy) {
                        let msgs: Vec<String> = coupling
                            .get("errors")
                            .and_then(Value::as_array)
                            .map(|e| {
                                e.iter()
                                    .map(|i| {
                                        i.get("message")
                                            .filter(|v| truthy(v))
                                            .map_or_else(|| py_str(i), py_str)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        return Err(JobError::optimize(if msgs.is_empty() {
                            vec!["automatic multiphysics coupling preflight refused optimization".into()]
                        } else {
                            msgs
                        }));
                    }
                    rep.insert("couplingReport".into(), coupling.clone());
                    let problem_doc = crate::provider_worker::problem_json(provider.as_ref(), &problem)?;
                    let effects = match ops.and_then(|o| o.preflight_effects(&problem_doc)) {
                        Some(r) => Some(r?),
                        None => None,
                    };
                    Ok((rep, coupling, effects.unwrap_or(Value::Null)))
                })();
                let (mut rep, _coupling, effects) = match inner {
                    Ok(v) => v,
                    Err(e) if e.python_class() == "OptimizeError" => return Err(e),
                    Err(e) => {
                        return opt1(format!(
                            "provider {} preflight: {}",
                            implexity_core::py_repr::repr_str(&meta.s("physics_provider")),
                            e.message()
                        ));
                    }
                };
                let checked = crate::provider_worker::checked_preflight_effects(if effects.is_null() {
                    None
                } else {
                    Some(&effects)
                })?;
                rep.insert(
                    "provider_admission".into(),
                    crate::provider_worker::provider_admission_report(
                        crate::private::perf_counter_ns() - started,
                        Some(&checked),
                        false,
                    )?,
                );
                rep
            };
            for (k, v) in [
                ("node", meta.get("node").clone()),
                ("declared_by", meta.get("declared_by").clone()),
                ("case_source", meta.get("case_source").clone()),
                ("free_plan", meta.get("plan").clone()),
                ("warnings", meta.get("warnings").clone()),
                ("ignored", meta.get("ignored").clone()),
                ("start", json!("POST /v1/implicit/optimize with the same body")),
                ("driver", json!("mature implicit runtime + modular CAE provider preflight")),
                ("physics_provider", meta.get("physics_provider").clone()),
                ("design_coordinate_selection", meta.get("design_coordinate_selection").clone()),
                ("topology_coordinate", json!("model:control")),
                ("native_runtime_contract", meta.get("native_runtime_contract").clone()),
                ("constraint_semantics", meta.get("constraint_semantics").clone()),
            ] {
                rep.insert(k.into(), v);
            }
            rep.insert(
                "computation_effort".into(),
                Value::Object(implexity_runtime::provider_job_authority::public_effort_view(
                    meta.get("computation_effort"),
                )?),
            );
            return Ok(Value::Object(rep));
        }
        let (mut rep, t0) = self.legacy_analysis(
            &js,
            "preflight",
            None,
            "PREFLIGHT",
            "a preflight builds the case's geometry map and samples the model (about 380 MB and 3 s, measured) and \
             this service runs one at a time by construction, on an 8 GB cgroup.  Poll the job, or wait.",
            ("IMPLEXITY_IMPLICIT_PREFLIGHT_TIMEOUT", 900.0),
            "preflight",
        )?;
        rep.insert(
            "seconds".into(),
            float_value(implexity_mesh::numeric::py_round_digits(crate::private::epoch_seconds() - t0, 2)),
        );
        declaration_echo(&mut rep, &js, &meta);
        rep.insert("start".into(), json!("POST /v1/implicit/optimize with the same body"));
        rep.insert("driver".into(), json!("python3 -m implexity.implicit.optimize --preflight"));
        Ok(Value::Object(rep))
    }

    fn provider_current_topology(&self, meta: &JobMeta) -> JobResult<ndarray::ArrayD<f64>> {
        let m = self.inner.models.require()?;
        let child = m.node(&meta.s("node"))?;
        let (arr, _, _) = super::declare::provider_ref_value(&child, &meta.s("topology_source_ref"))?;
        if arr.ndim() != 3 || !arr.iter().all(|v| v.is_finite()) {
            return opt1("the live model:control field is no longer a finite 3-D array");
        }
        Ok(arr)
    }


    #[allow(clippy::too_many_lines)]
    pub fn sensitivity(&self, req: &Map<String, Value>) -> JobResult<Value> {
        let transport = [
            "stream",
            "stream_fields",
            "stream_response",
            "sensitivity_responses",
            "previous_field_ids",
            "tile_shape",
            "max_preview_voxels",
            "exact_state_handoff",
        ];
        let Declaration { js, meta } = self.declare(&filtered(req, &transport))?;
        let requested_batch = req.get("sensitivity_responses").filter(|v| !v.is_null());
        let is_array = meta.get("provider_execution").as_str() == Some("array");
        if let Some(batch) = requested_batch {
            let ok = batch.as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.iter().all(|n| n.as_str().is_some_and(|s| !s.trim().is_empty()))
                    && a.iter().map(py_str).collect::<BTreeSet<_>>().len() == a.len()
            });
            if !ok {
                return opt1("sensitivity_responses must be a nonempty array of unique nonempty strings");
            }
            if req.get("response").is_some_and(|v| !v.is_null())
                || req.get("stream_response").is_some_and(|v| !v.is_null())
            {
                return opt1("a sensitivity batch cannot also select response or stream_response");
            }
            if !is_array {
                return opt1(
                    "sensitivity_responses requires an array provider with a named-design batch contract",
                );
            }
        }
        let stream_enabled = req.get("stream").is_none_or(truthy);
        if is_array {
            let handoff = self.matching_time_handoff(req.get("exact_state_handoff"), false)?;
            let response_names: Vec<String> = if let Some(batch) = requested_batch {
                batch.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default()
            } else {
                let first = meta
                    .get("provider_responses")
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|r| r.get("name"))
                    .cloned();
                let response = req
                    .get("stream_response")
                    .filter(|v| truthy(v))
                    .or_else(|| req.get("response").filter(|v| truthy(v)))
                    .cloned()
                    .or(first)
                    .filter(truthy);
                let Some(response) = response else {
                    return opt1("provider sensitivity needs a response name");
                };
                vec![py_str(&response)]
            };
            let declared: Vec<String> = meta
                .get("provider_responses")
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|r| {
                            r.get("name")
                                .filter(|v| truthy(v))
                                .or_else(|| r.get("response").filter(|v| truthy(v)))
                                .map(py_str)
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let unknown: Vec<&String> = response_names.iter().filter(|n| !declared.contains(n)).collect();
            if !unknown.is_empty() {
                return opt1(format!(
                    "provider sensitivity responses are not declared by the analysis: {}",
                    implexity_core::pyobj::list_repr(&unknown)
                ));
            }
            let artifact_root =
                stream_enabled.then(|| self.inner.dir.join("sensitivity_artifacts_v24_provider"));
            if requested_batch.is_none() {
                let mut rep = self.run_provider_worker(
                    &meta,
                    "sensitivity",
                    Some(&response_names[0]),
                    None,
                    artifact_root.as_deref(),
                    handoff.as_ref(),
                )?;
                if let Some(root) = &artifact_root {
                    self.publish_provider_sensitivity(&mut rep, &meta, req, &response_names[0], root)?;
                }
                return Ok(Value::Object(rep));
            }
            let mut rep = self.run_provider_worker(
                &meta,
                "sensitivity",
                None,
                Some(&response_names),
                artifact_root.as_deref(),
                handoff.as_ref(),
            )?;
            let members: BTreeSet<String> = rep
                .get("sensitivities")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            if rep.get("schema").and_then(Value::as_str) != Some("implexity-provider-sensitivities/1")
                || rep.get("response_order") != Some(&json!(response_names))
                || members != response_names.iter().cloned().collect::<BTreeSet<_>>()
            {
                return opt1("provider sensitivity batch returned a mismatched response contract");
            }
            if let Some(root) = &artifact_root {
                self.publish_provider_sensitivity_batch(&mut rep, &meta, req, &response_names, root)?;
            }
            return Ok(Value::Object(rep));
        }
        let artifact_root = self.inner.dir.join("sensitivity_artifacts_v22");
        let sreq = json!({"artifact_root": if stream_enabled { json!(artifact_root.to_string_lossy()) } else { Value::Null }});
        let (mut rep, t0) = self.legacy_analysis(
            &js,
            "sensitivity",
            Some(("--sensitivity-file", &sreq)),
            "SENSITIVITY",
            "a sensitivity analysis builds and differentiates the coupled CAE problem and this service admits one \
             heavy process at a time",
            ("IMPLEXITY_IMPLICIT_SENSITIVITY_TIMEOUT", 1200.0),
            "sensitivity",
        )?;
        rep.insert(
            "seconds".into(),
            float_value(implexity_mesh::numeric::py_round_digits(crate::private::epoch_seconds() - t0, 2)),
        );
        declaration_echo(&mut rep, &js, &meta);
        if stream_enabled && rep.get("artifact").is_some_and(truthy) {
            self.publish_legacy_streams(&mut rep, &meta, req, &artifact_root, true)?;
        }
        Ok(Value::Object(rep))
    }


    pub fn results(&self, req: &Map<String, Value>) -> JobResult<Value> {
        let mut out: Option<Value> = None;
        self.inner.host.physics_runtime_guard(&Value::Object(req.clone()), &mut || {
            out = Some(self.results_guarded(req)?);
            Ok(())
        })?;
        out.ok_or_else(|| JobError::runtime("results produced no report"))
    }

    fn results_guarded(&self, req: &Map<String, Value>) -> JobResult<Value> {
        let transport = [
            "fields",
            "include_values",
            "max_inline_values",
            "stream",
            "stream_fields",
            "previous_field_ids",
            "tile_shape",
            "max_preview_voxels",
            "exact_state_handoff",
        ];
        let Declaration { js, meta } = self.declare(&filtered(req, &transport))?;
        let stream_enabled = req.get("stream").is_none_or(truthy);
        if meta.get("provider_execution").as_str() == Some("array") {
            let handoff = self.matching_time_handoff(req.get("exact_state_handoff"), false)?;
            let artifact_root = stream_enabled.then(|| self.inner.dir.join("result_artifacts_v24_provider"));
            let mut rep = self.run_provider_worker(
                &meta,
                "evaluate",
                None,
                None,
                artifact_root.as_deref(),
                handoff.as_ref(),
            )?;
            if let Some(root) = &artifact_root {
                self.publish_provider_results(&mut rep, &meta, req, root)?;
            }
            return Ok(Value::Object(rep));
        }
        let artifact_root = self.inner.dir.join("result_artifacts_v22");
        let max_inline = match req.get("max_inline_values") {
            None => 4096,
            Some(v) => crate::optimize::spec::py_int(v)?,
        };
        let rreq = json!({
            "fields": req.get("fields").filter(|v| truthy(v)).cloned().unwrap_or(json!([])),
            "include_values": req.get("include_values").is_some_and(truthy),
            "max_inline_values": max_inline,
            "artifact_root": if stream_enabled { json!(artifact_root.to_string_lossy()) } else { Value::Null },
        });
        let (mut rep, t0) = self.legacy_analysis(
            &js,
            "results",
            Some(("--results-file", &rreq)),
            "RESULTS",
            "result-field evaluation executes the coupled CAE chain and this service admits one heavy process at a time",
            ("IMPLEXITY_IMPLICIT_SENSITIVITY_TIMEOUT", 1200.0),
            "result-field",
        )?;
        rep.insert(
            "seconds".into(),
            float_value(implexity_mesh::numeric::py_round_digits(crate::private::epoch_seconds() - t0, 2)),
        );
        declaration_echo(&mut rep, &js, &meta);
        if stream_enabled && rep.get("artifact").is_some_and(truthy) {
            self.publish_legacy_streams(&mut rep, &meta, req, &artifact_root, false)?;
        }
        Ok(Value::Object(rep))
    }


    pub fn derivative(&self, req: &Map<String, Value>) -> JobResult<Value> {
        let norm = implexity_geometry::derivatives::normalise(&Value::Object(req.clone())).map_err(|e| {
            JobError::Problems {
                class: "DerivativeError".into(),
                message: format!("derivative request rejected:\n  {}", e.problems.join("\n  ")),
                problems: e.problems,
            }
        })?;
        let Declaration { js, meta } = self.declare(&filtered(req, &["operator", "tangent", "cotangent"]))?;
        let (mut rep, t0) = self.legacy_analysis(
            &js,
            "derivative",
            Some(("--derivative-file", &norm)),
            "DERIVATIVE",
            "derivative operators execute the coupled CAE chain and this service admits one heavy process at a time",
            ("IMPLEXITY_IMPLICIT_SENSITIVITY_TIMEOUT", 1200.0),
            "derivative",
        )?;
        rep.insert(
            "seconds".into(),
            float_value(implexity_mesh::numeric::py_round_digits(crate::private::epoch_seconds() - t0, 2)),
        );
        declaration_echo(&mut rep, &js, &meta);
        Ok(Value::Object(rep))
    }

    #[allow(clippy::too_many_lines)]
    fn publish_legacy_streams(
        &self,
        rep: &mut Map<String, Value>,
        meta: &JobMeta,
        req: &Map<String, Value>,
        artifact_root: &Path,
        sensitivity: bool,
    ) -> JobResult<()> {
        use implexity_authoring::progressive_fields::{FieldIdentity, Reducer};
        let store = crate::artifacts::ResultArtifactStore::new(artifact_root)?;
        let artifact = rep.get("artifact").and_then(Value::as_object).cloned().unwrap_or_default();
        let artifact_id = artifact.get("artifact_id").map(py_str).unwrap_or_default();
        let arrays = store.read_arrays_bounded(&artifact_id, None, &crate::artifacts::Limits::default())?;
        let meta_block = artifact.get("metadata").cloned().unwrap_or(Value::Null);
        let registration = rep
            .get("field_registration")
            .filter(|v| truthy(v))
            .cloned()
            .or_else(|| meta_block.get("field_registration").cloned())
            .unwrap_or(Value::Null);
        let field_meta_all = if sensitivity {
            meta_block.get("fields").and_then(Value::as_object).cloned().unwrap_or_default()
        } else {
            rep.get("fields").and_then(Value::as_object).cloned().unwrap_or_default()
        };
        let requested: Option<BTreeSet<String>> = req
            .get("stream_fields")
            .filter(|v| !v.is_null())
            .map(|v| v.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default());
        let response_filter = req.get("stream_response").filter(|v| truthy(v)).map(py_str);
        let previous = req.get("previous_field_ids").and_then(Value::as_object).cloned().unwrap_or_default();
        let tile = triplet(req.get("tile_shape"));
        let voxels = match req.get("max_preview_voxels") {
            None => 64_000,
            Some(v) => crate::optimize::spec::py_int(v)?,
        };
        let field_store = self.field_store()?;
        let reg_shape: Vec<u64> = registration
            .get("shape")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let mut streams = Map::new();
        let mut omitted = Map::new();
        for (name, array) in arrays {
            let fm = field_meta_all.get(&name).and_then(Value::as_object).cloned().unwrap_or_default();
            if let Some(r) = &requested
                && !r.contains(&name)
            {
                continue;
            }
            let fa = super::evaluation::field_array(&array);
            let identity = |response: String| FieldIdentity {
                model_id: rep
                    .get("model")
                    .and_then(|m| m.get("content_id"))
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
                problem_id: meta.s("solve_id"),
                field_name: name.clone(),
                registration_id: registration
                    .get("registration_id")
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
                response_id: response,
                optimisation_iteration: None,
            };
            if sensitivity {
                if let Some(rf) = &response_filter
                    && fm.get("response").map(py_str).as_deref() != Some(rf.as_str())
                {
                    continue;
                }
                let shape: Vec<u64> = array.shape.iter().map(|v| *v as u64).collect();
                if array.shape.len() != 3 || registration.is_null() || shape != reg_shape {
                    continue;
                }
                let response = fm
                    .get("response")
                    .filter(|v| truthy(v))
                    .map_or_else(|| "weighted_total".to_string(), py_str);
                let Some(nd) = super::evaluation::to_ndarray(&array, false) else { continue };
                let publication = field_store.publish_revision(
                    &nd,
                    &identity(response.clone()),
                    &registration,
                    previous.get(&name).and_then(Value::as_str),
                    Reducer::MaxAbsSigned,
                    voxels,
                    true,
                    tile,
                )?;
                let view_spec = implexity_geometry::field_views::field_views(
                    &fa,
                    "scalar",
                    None,
                    true,
                    super::evaluation::FIELD_VIEW_SAMPLE_LIMIT,
                )?;
                let default_view = view_spec
                    .get("views")
                    .and_then(Value::as_array)
                    .and_then(|v| v.first())
                    .cloned()
                    .unwrap_or(Value::Null);
                let manifest = publication.get("manifest").cloned().unwrap_or(Value::Null);
                let endpoints = manifest.get("endpoints").cloned().unwrap_or(Value::Null);
                streams.insert(
                    name.clone(),
                    json!({
                        "schema": "implexity-field-stream-ref/1",
                        "field_id": publication.get("field_id"),
                        "field_name": name,
                        "response_id": response,
                        "parameter": fm.get("parameter"),
                        "rank": "scalar",
                        "components": 1,
                        "units": fm.get("units").cloned().unwrap_or(json!("1")),
                        "signed": true,
                        "views": view_spec.get("views"),
                        "default_view": view_spec.get("default_view"),
                        "component": default_view.get("component"),
                        "parent_field_id": manifest.get("parent_field_id"),
                        "manifest": endpoints.get("manifest"),
                        "delta": endpoints.get("delta"),
                        "tile": endpoints.get("tile"),
                        "visual_range": default_view.get("range"),
                        "registration": registration,
                        "exact_delta": manifest.get("exact_delta"),
                    }),
                );
                continue;
            }
            let spatial: Vec<u64> = array.shape.iter().take(3).map(|v| *v as u64).collect();
            if registration.is_null() || spatial != reg_shape {
                omitted.insert(name, json!("the field shape does not match the exact analysis registration"));
                continue;
            }
            let (sshape, sdata) = match implexity_geometry::field_views::prepare_stream_array(&fa) {
                Ok(v) => v,
                Err(e) => {
                    omitted.insert(name, json!(e.to_string()));
                    continue;
                }
            };
            let stream_fa = implexity_geometry::field_views::FieldArray {
                shape: sshape.clone(),
                data: sdata.clone(),
                complex: false,
            };
            let rank = fm.get("rank").map_or_else(|| "scalar".to_string(), py_str);
            let labels =
                implexity_geometry::field_views::component_labels_from_metadata(Some(&fm), Some(&fa));
            let view_spec = match implexity_geometry::field_views::field_views(
                &stream_fa,
                &rank,
                labels.as_deref(),
                fm.get("signed").is_some_and(truthy),
                super::evaluation::FIELD_VIEW_SAMPLE_LIMIT,
            ) {
                Ok(v) => v,
                Err(e) => {
                    omitted.insert(name, json!(e.to_string()));
                    continue;
                }
            };
            let default_id = view_spec.get("default_view").cloned().unwrap_or(Value::Null);
            let default_view = view_spec
                .get("views")
                .and_then(Value::as_array)
                .and_then(|v| v.iter().find(|x| x.get("id") == Some(&default_id)))
                .cloned()
                .unwrap_or(Value::Null);
            let nd = implexity_geometry::NdArray::from_f64(sshape, sdata)
                .ok_or_else(|| JobError::value("stream array shape mismatch"))?;
            let publication = field_store.publish_revision(
                &nd,
                &identity(name.clone()),
                &registration,
                previous.get(&name).and_then(Value::as_str),
                Reducer::Mean,
                voxels,
                false,
                tile,
            )?;
            let complex = fa.complex;
            let manifest = publication.get("manifest").cloned().unwrap_or(Value::Null);
            let endpoints = manifest.get("endpoints").cloned().unwrap_or(Value::Null);
            streams.insert(
                name.clone(),
                json!({
                    "schema": "implexity-field-stream-ref/1",
                    "field_id": publication.get("field_id"),
                    "field_name": name,
                    "rank": rank,
                    "value_representation": if complex { "interleaved_real_imaginary" } else { "real" },
                    "components": view_spec.get("components"),
                    "views": view_spec.get("views"),
                    "default_view": default_id,
                    "component": default_view.get("component"),
                    "units": fm.get("units").cloned().unwrap_or(json!("1")),
                    "parent_field_id": manifest.get("parent_field_id"),
                    "manifest": endpoints.get("manifest"),
                    "delta": endpoints.get("delta"),
                    "tile": endpoints.get("tile"),
                    "visual_range": default_view.get("range"),
                    "registration": registration,
                    "exact_delta": manifest.get("exact_delta"),
                }),
            );
        }
        if sensitivity {
            rep.insert("sensitivity_stream_count".into(), json!(streams.len()));
            rep.insert("sensitivity_streams".into(), Value::Object(streams));

            super::evaluation::reorder_after(rep, "sensitivity_streams", "sensitivity_stream_count");
        } else {
            rep.insert("field_stream_count".into(), json!(streams.len()));
            rep.insert("field_streams".into(), Value::Object(streams));
            super::evaluation::reorder_after(rep, "field_streams", "field_stream_count");
            if !omitted.is_empty() {
                rep.insert("field_streams_omitted".into(), Value::Object(omitted));
            }
        }
        Ok(())
    }
}
