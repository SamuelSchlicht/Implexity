// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::*;
use crate::managed_io::{NPZ_LIMIT, artifact_fingerprint, read_json, read_npz};
use implexity_geometry::document::Binding;

impl ModelOptimizeManager {
    pub(crate) fn stored_observation(&self, id: &str) -> JobResult<(ModelOptJob, PathBuf)> {
        let (descriptor, job_dir) = self.read_managed_restart_descriptor(id)?;
        let operations = self.inner.dir.join("managed_children/operations");
        let mut matches = Vec::new();
        for entry in std::fs::read_dir(&operations)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let operation_id = entry.file_name().to_string_lossy().to_string();
            if operation_id.len() != 48
                || !operation_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                continue;
            }
            let journal = match read_json(&entry.path().join("journal.json"), 16 * 1024 * 1024) {
                Ok(journal) => journal,
                Err(_) => continue,
            };
            if journal["schema"] != "implexity-private-managed-evaluation-journal/2"
                || journal["operation_id"] != operation_id
            {
                continue;
            }
            if journal["recovery_owner"]["kind"] == "implicit_optimize"
                && journal["recovery_owner"]["id"] == id
                && journal["state"] == "succeeded"
            {
                let directory = entry.path().join("committed");
                if std::fs::symlink_metadata(&directory)?.file_type().is_dir() {
                    matches.push(directory);
                }
            }
        }
        if matches.len() != 1 {
            return Err(JobError::value(
                "saved optimization has no unique successful committed output",
            ));
        }
        let directory = matches.remove(0);
        let spec = read_json(&directory.join("spec.json"), 64 * 1024 * 1024)?;
        if artifact_fingerprint(&directory.join("spec.json"), 64 * 1024 * 1024)?
            != descriptor["spec_fingerprint"]
        {
            return Err(JobError::value(
                "saved optimization specification identity drifted",
            ));
        }
        if spec["schema"] != "implexity-provider-job/1"
            || spec
                .get("derived_model_outputs")
                .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(JobError::value(
                "saved optimization requires an unsupported reconstruction contract",
            ));
        }
        let summary = read_json(&directory.join("summary.json"), 64 * 1024 * 1024)?;
        let history = read_json(&directory.join("history.json"), 64 * 1024 * 1024)?;
        let rows = history["history"]
            .as_array()
            .cloned()
            .ok_or_else(|| JobError::value("saved optimization history is malformed"))?;
        if summary["solve_id"] != spec["solve_id"]
            || summary["history_digest"].as_str()
                != Some(crate::hierarchical_job::history_digest_of(&rows).as_str())
        {
            return Err(JobError::value(
                "saved optimization history identity drifted",
            ));
        }
        let before = read_json(&job_dir.join("model_before.json"), 64 * 1024 * 1024)?;
        let model = implexity_geometry::document::build(&before, Some(&job_dir), None)?;
        let node_name = descriptor["snapshot"]["node"]
            .as_str()
            .ok_or_else(|| JobError::value("saved optimization geometry node is absent"))?;
        let root = model.node(node_name)?;
        if spec["document_content_id"].as_str().is_none()
            || Some(root.content_id().as_str()) != spec["document_content_id"].as_str()
        {
            return Err(JobError::value(
                "saved optimization initial model identity drifted",
            ));
        }
        let mut meta = JobMeta::default();
        meta.fields = descriptor["snapshot"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        meta.fields.insert("before_doc".into(), before.clone());
        meta.fields.insert(
            "before_sha256".into(),
            json!(sha256_hex(&implexity_geometry::document::canonical_bytes(
                &before
            ))),
        );
        meta.fields
            .insert("provider_execution".into(), json!("array"));
        meta.fields
            .insert("physics_provider".into(), spec["provider"].clone());
        meta.fields.insert(
            "result_authority".into(),
            json!("exploratory_non_authoritative"),
        );
        let coordinate_rows = spec["design_coordinates"]
            .as_array()
            .ok_or_else(|| JobError::value("saved optimization coordinates are absent"))?;
        let requested = descriptor["request"]["design_coordinates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut plan = Vec::new();
        for coordinate in coordinate_rows {
            let name = coordinate["coordinate"]
                .as_str()
                .ok_or_else(|| JobError::value("saved coordinate name is malformed"))?;
            let selected = requested.iter().find(|r| r["coordinate"] == name);
            let source = selected
                .and_then(|r| r["ref"].as_str())
                .or_else(|| {
                    if name == "model:control" {
                        descriptor["request"]["topology"]["ref"].as_str()
                    } else {
                        None
                    }
                })
                .ok_or_else(|| {
                    JobError::value("saved coordinate has no explicit geometry binding")
                })?;
            let (value, node, reference) = declare::provider_ref_value(&root, source)?;
            let node_id = model
                .id_of(&node)
                .ok_or_else(|| JobError::value("saved coordinate node is absent"))?;
            let binding = model
                .bindings()
                .get(&node_id)
                .and_then(|b| b.get(&reference.name));
            let key = match binding {
                Some(Binding::Array { key }) => Some(key.clone()),
                None => None,
                _ => return Err(JobError::value("saved coordinate binding is unsupported")),
            };
            plan.push(json!({"kind":if key.is_some(){"spatial_array"}else{"node_param"},"ref":name,"source_ref":source,"node":node_id,"param":reference.name,"node_units":"-","units":"-","array_key":key,"array_file":Value::Null,"start":job::num_array(&value),"lo":coordinate["lower"],"hi":coordinate["upper"]}));
            meta.before_values.insert(name.into(), value);
        }
        meta.fields.insert("plan".into(), json!(plan));
        let mut observation = ModelOptJob::new(json!("optimize"), 0, &meta)?;
        observation.id = id.into();
        observation.status = "completed".into();
        observation.message = "Saved run".into();
        observation.progress = 1.0;
        observation.rows = rows;
        observation.summary = summary.as_object().cloned();
        observation.t_submit = descriptor["t_submit"].as_f64().unwrap_or(0.0);
        observation.job_dir = Some(job_dir.display().to_string());
        observation.settings = spec["settings"].as_object().cloned().unwrap_or_default();
        observation.request = descriptor["request"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        Ok((observation, directory))
    }

    pub(crate) fn stored_job_info(&self, id: &str, rows: bool) -> JobResult<Value> {
        let (job, _) = self.stored_observation(id)?;
        let mut result = job.as_dict(rows);
        result["read_only"] = json!(true);
        result["stored_observation"] = json!(true);
        Ok(result)
    }

    pub(crate) fn stored_provider_guard(&self, directory: &Path) -> JobResult<()> {
        let spec = read_json(&directory.join("spec.json"), 64 * 1024 * 1024)?;
        let provider_name = spec["provider"]
            .as_str()
            .ok_or_else(|| JobError::value("saved provider identity is malformed"))?;
        let provider = implexity_core::registries::global()
            .providers
            .get(provider_name)?;
        let problem = provider.normalise_problem(&spec["problem"])?;
        let derived_enabled = crate::provider_hooks::with_hooks(provider.as_ref(), |h| {
            Some(h.derived_model_hooks_enabled(&problem))
        })
        .unwrap_or(true);
        let has_derived = crate::provider_hooks::with_hooks(provider.as_ref(), |h| {
            Some(h.has_derived_model_output_refs() || h.has_derive_model_updates())
        })
        .unwrap_or(false);
        if derived_enabled && has_derived {
            return Err(JobError::value(
                "saved optimization derived outputs require a persisted reconstruction contract",
            ));
        }
        Ok(())
    }

    pub(crate) fn stored_epoch_values(
        &self,
        job: &ModelOptJob,
        directory: &Path,
        row: &mut Value,
    ) -> JobResult<BTreeMap<String, ArrayD<f64>>> {
        let epoch = row["i"]
            .as_i64()
            .filter(|i| *i >= 0)
            .ok_or_else(|| JobError::value("saved epoch index is malformed"))?;
        let name = format!("live_{epoch:06}.npz");
        if row["iteration"].as_i64() != Some(epoch) || row["push"]["file"].as_str() != Some(&name) {
            return Err(JobError::value("saved epoch checkpoint identity drifted"));
        }
        let (arrays, fingerprint) = read_npz(&directory.join(name), NPZ_LIMIT, None)?;
        if fingerprint["sha256"] != row["push"]["sha256"]
            || fingerprint["bytes"] != row["push"]["bytes"]
        {
            return Err(JobError::value("saved epoch checkpoint payload drifted"));
        }
        let values = Self::managed_design_values(job, &arrays)?;
        let state = implexity_optim::design_identity(&implexity_optim::NamedArrays::from_pairs(
            values.clone(),
        ))?;
        if row["design_state_id"].as_str() != Some(&state) {
            return Err(JobError::value("saved epoch design identity drifted"));
        }
        row["push"]["verified_sha256"] = fingerprint["sha256"].clone();
        row["push"]["verified_design_state_id"] = json!(state);
        Ok(values)
    }
}
