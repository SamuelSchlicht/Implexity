// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use implexity_core::pyobj::{py_str, truthy};
use implexity_io::npy::{NpyArray, NpyData};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use super::run::Protocol;
use super::{LiveJob, ModelOptimizeManager, lock};
use crate::error::{JobError, JobResult};
use crate::managed_io::{
    COPY_LIMIT, JSON_LIMIT, NPZ_LIMIT, artifact_fingerprint, copy_regular, read_json, read_npz,
};

fn verr(m: impl Into<String>) -> JobError {
    JobError::value(m)
}

fn is_int(v: Option<&Value>) -> bool {
    v.is_some_and(implexity_optim::pyval::is_int)
}

fn finite_number(v: Option<&Value>) -> bool {
    v.and_then(Value::as_f64).is_some_and(f64::is_finite)
}

fn is_slot(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_decision_name(name: &str) -> bool {
    name.strip_prefix("numerical_attention_decision.")
        .and_then(|r| r.strip_suffix(".json"))
        .is_some_and(crate::private::is_sha256)
}

fn is_live_epoch_name(name: &str) -> bool {
    name.len() == "live_000000.npz".len()
        && name.starts_with("live_")
        && name.ends_with(".npz")
        && name.as_bytes()[5..11].iter().all(u8::is_ascii_digit)
}

fn text_vector(arrays: &[(String, NpyArray)], name: &str, what: &str) -> JobResult<Vec<String>> {
    let a = arrays.iter().find(|(k, _)| k == name).map(|(_, a)| a);
    match a.map(|a| (&a.data, a.shape.len())) {
        Some((NpyData::Unicode { values, .. }, 1)) => Ok(values.clone()),
        Some(_) => Err(verr(format!("managed {what} {name} table is not text"))),
        None => Err(verr(format!("managed {what} {name} table is malformed"))),
    }
}

fn array_of(arrays: &[(String, NpyArray)], name: &str) -> Option<ArrayD<f64>> {
    arrays.iter().find(|(k, _)| k == name).and_then(|(_, a)| a.to_f64())
}

fn is_complex(arrays: &[(String, NpyArray)], name: &str) -> bool {
    arrays.iter().any(|(k, a)| k == name && matches!(a.data, NpyData::C64(_) | NpyData::C128(_)))
}

impl ModelOptimizeManager {

    pub(crate) fn verify_managed_terminal_seal(
        &self,
        manifest: Option<&Value>,
        directory: &Path,
    ) -> JobResult<()> {
        let Some(m) = manifest.and_then(Value::as_object).filter(|m| {
            m.get("schema").and_then(Value::as_str)
                == Some("implexity-private-optimization-terminal-manifest/1")
                && m.get("files").is_some_and(Value::is_object)
        }) else {
            return Err(verr("managed optimization terminal seal is unavailable"));
        };
        for (name, expected) in m["files"].as_object().into_iter().flatten() {
            let actual = artifact_fingerprint(&directory.join(name), COPY_LIMIT)?;
            if &actual != expected {
                return Err(verr(format!("managed optimization sealed artifact drifted: {name}")));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn prepare_managed_optimization_partial(
        &self,
        job: &LiveJob,
        resume: bool,
        partial: &Path,
    ) -> JobResult<()> {
        let (source, manifest, spec_file, expected_spec, attention, steers) = {
            let j = lock(job);
            (
                PathBuf::from(j.job_dir.clone().unwrap_or_default()),
                j.managed_terminal_manifest.clone(),
                PathBuf::from(j.spec_file.clone().unwrap_or_default()),
                j.managed_spec_fingerprint.clone(),
                j.numerical_attention.clone(),
                j.steers.clone(),
            )
        };
        let terminal_kind =
            manifest.as_ref().and_then(|m| m.get("terminal_kind")).and_then(Value::as_str).unwrap_or("");
        if resume {
            if terminal_kind != "paused" && terminal_kind != "numerical_attention" {
                return Err(verr(
                    "managed optimization resume lacks a validated pause or numerical-attention seal",
                ));
            }
            self.verify_managed_terminal_seal(manifest.as_ref(), &source)?;
        }
        let Some(expected_spec) = expected_spec.filter(Value::is_object) else {
            return Err(verr("managed optimization source specification drifted before child birth"));
        };
        if artifact_fingerprint(&spec_file, JSON_LIMIT)? != expected_spec {
            return Err(verr("managed optimization source specification drifted before child birth"));
        }
        copy_regular(&spec_file, &partial.join("spec.json"), JSON_LIMIT, true)?;
        if artifact_fingerprint(&partial.join("spec.json"), JSON_LIMIT)? != expected_spec {
            return Err(verr("managed optimization private specification copy drifted"));
        }
        let outputs: BTreeSet<&str> = [
            "best.npz",
            "ckpt.npz",
            "history.json",
            "initial.npz",
            "initial_design.json",
            "matching_time_guess_consumed.json",
            "resume_warm_start.npz",
            "resume_warm_start.alt.npz",
            "resume_warm_start.json",
            "continuation.json",
        ]
        .into_iter()
        .collect();
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(&source)?
            .filter_map(Result::ok)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        for (name, path) in names {
            let initial = name.ends_with(".npz")
                && name != "best.npz"
                && name != "ckpt.npz"
                && name != "resume_warm_start.npz"
                && name != "resume_warm_start.alt.npz"
                && !name.starts_with("live_");
            let checkpoint = resume
                && (outputs.contains(name.as_str())
                    || is_decision_name(&name)
                    || (name.starts_with("live_") && name.ends_with(".npz")));
            if initial || checkpoint {
                copy_regular(&path, &partial.join(&name), COPY_LIMIT, true)?;
            }
        }
        if resume {

            crate::epoch_state::copy_epoch_states(&source, partial, None)?;
        }
        if resume
            && terminal_kind == "numerical_attention"
            && attention.as_ref().and_then(|a| a.get("action")).and_then(Value::as_str) == Some("retry_exact")
        {
            let token = attention.as_ref().and_then(|a| a.get("event_token")).cloned().unwrap_or(Value::Null);
            let decision = numerical_attention_decision_name(&token)?;
            copy_regular(
                &source.join(&decision),
                &partial.join("numerical_attention_decision.json"),
                1024 * 1024,
                false,
            )?;
        }
        if resume {
            let pending = source.join("steer.json");
            let transfer = source.join("steer.resume-transfer.json");
            if transfer.exists() || transfer.is_symlink() {
                return Err(verr("managed optimization has an unresolved steer transfer"));
            }
            if pending.exists() || pending.is_symlink() {
                let queued: Vec<&Map<String, Value>> = steers
                    .iter()
                    .filter(|r| r.get("status").and_then(Value::as_str) == Some("queued"))
                    .collect();
                if queued.len() != 1 {
                    return Err(verr("paused steer file has no unique authoritative queue record"));
                }
                let expected = queued[0].get("request_fingerprint").filter(|v| v.is_object());
                if expected
                    .is_none_or(|e| artifact_fingerprint(&pending, 1024 * 1024).ok().as_ref() != Some(e))
                {
                    return Err(verr("paused steer file drifted from its queue record"));
                }
                let payload = read_json(&pending, 1024 * 1024)?;
                if !payload.is_object() || payload.get("request_id") != queued[0].get("request_id") {
                    return Err(verr("paused steer request identity drifted"));
                }
                std::fs::rename(&pending, &transfer)?;
                if let Err(e) = copy_regular(&transfer, &partial.join("steer.json"), 1024 * 1024, true) {
                    std::fs::rename(&transfer, &pending)?;
                    return Err(e);
                }
            }
            for required in ["ckpt.npz", "history.json"] {
                if !partial.join(required).is_file() {
                    return Err(verr(format!("managed optimization resume is missing {required}")));
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn validate_legacy_model_generation(
        &self,
        job: &LiveJob,
        partial: &Path,
        completed: bool,
        summary: Option<&Value>,
        history_payload: Option<Value>,
    ) -> JobResult<()> {
        let j = lock(job).clone();
        let expected_refs: Vec<String> =
            j.plan.iter().map(|e| e.get("ref").map(py_str).unwrap_or_default()).collect();
        if expected_refs.is_empty()
            || expected_refs.iter().collect::<BTreeSet<_>>().len() != expected_refs.len()
        {
            return Err(verr("managed model design plan is not closed"));
        }
        let by_ref: BTreeMap<String, &Value> =
            j.plan.iter().map(|e| (e.get("ref").map(py_str).unwrap_or_default(), e)).collect();
        let load_design = |name: &str| -> JobResult<(BTreeMap<String, ArrayD<f64>>, Vec<String>)> {
            let (arrays, _) = read_npz(&partial.join(name), NPZ_LIMIT, None)?;
            let meta = |t: &str| {
                text_vector(&arrays, t, "model design")
                    .map_err(|_| verr("managed model design metadata is malformed"))
            };
            let refs = meta("refs")?;
            let slots = meta("slots")?;
            let units = meta("units")?;
            if refs != expected_refs
                || slots.len() != refs.len()
                || slots.iter().collect::<BTreeSet<_>>().len() != slots.len()
                || units.len() != refs.len()
                || slots.iter().any(|s| !is_slot(s))
            {
                return Err(verr("managed model design coordinate table drifted"));
            }
            let mut expected: BTreeSet<String> =
                ["refs", "slots", "units", "solve_id"].iter().map(|s| (*s).to_string()).collect();
            expected.extend(slots.iter().map(|s| format!("p_{s}")));
            if arrays.iter().map(|(k, _)| k.clone()).collect::<BTreeSet<_>>() != expected {
                return Err(verr("managed model design archive layout drifted"));
            }
            let sid = arrays
                .iter()
                .find(|(k, _)| k == "solve_id")
                .and_then(|(_, a)| a.as_scalar_str())
                .filter(|s| !s.is_empty());
            let Some(sid) = sid else { return Err(verr("managed checkpoint solve_id is not text")) };
            if sid != j.solve_id {
                return Err(verr("managed model design solve identity drifted"));
            }
            let mut values = BTreeMap::new();
            for (r, slot) in refs.iter().zip(&slots) {
                let key = format!("p_{slot}");
                if is_complex(&arrays, &key) {
                    return Err(verr("managed model design contains complex data"));
                }
                let value =
                    array_of(&arrays, &key).ok_or_else(|| verr("managed model design is not numeric"))?;
                let entry = by_ref[r];
                let lo = entry.get("lo").and_then(Value::as_f64);
                let hi = entry.get("hi").and_then(Value::as_f64);
                let shape = j.before_values.get(r).map(|b| b.shape().to_vec()).unwrap_or_default();
                if value.shape() != shape.as_slice()
                    || !value.iter().all(|v| v.is_finite())
                    || lo.is_some_and(|l| value.iter().any(|v| *v < l))
                    || hi.is_some_and(|h| value.iter().any(|v| *v > h))
                {
                    return Err(verr("managed model design violates shape, bounds, or finiteness"));
                }
                values.insert(r.clone(), value);
            }
            Ok((values, slots))
        };
        let history_payload = match history_payload {
            Some(h) => h,
            None => read_json(&partial.join("history.json"), JSON_LIMIT)?,
        };
        let history = history_payload.get("history").and_then(Value::as_array).cloned();
        let expected_status = if completed { "completed" } else { "paused" };
        let Some(history) = history
            .filter(|_| history_payload.get("status").and_then(Value::as_str) == Some(expected_status))
        else {
            return Err(verr("managed model history generation is malformed"));
        };
        let refset: BTreeSet<&String> = expected_refs.iter().collect();
        for (index, row) in history.iter().enumerate() {
            let ok = row.is_object()
                && is_int(row.get("i"))
                && row.get("i").and_then(Value::as_u64) == Some(index as u64)
                && finite_number(row.get("L"));
            if !ok {
                return Err(verr("managed model history sequence is malformed"));
            }
            if let Some(free) = row.get("free").filter(|v| !v.is_null()) {
                let Some(f) = free.as_object().filter(|f| f.keys().collect::<BTreeSet<_>>() == refset) else {
                    return Err(verr("managed model history coordinate table drifted"));
                };
                for entry in f.values() {
                    let Some(e) = entry.as_object() else {
                        return Err(verr("managed model history coordinate evidence is malformed"));
                    };
                    if ["mean", "min", "max"].iter().any(|k| !finite_number(e.get(*k))) {
                        return Err(verr("managed model history coordinate evidence is nonfinite"));
                    }
                }
            }
        }
        let (checkpoint, _) = read_npz(&partial.join("ckpt.npz"), NPZ_LIMIT, None)?;
        let param_slots = text_vector(&checkpoint, "param_keys", "model checkpoint")
            .map_err(|_| verr("managed model checkpoint key table is malformed"))?;
        if param_slots.is_empty()
            || param_slots.iter().collect::<BTreeSet<_>>().len() != param_slots.len()
            || param_slots.iter().any(|s| !is_slot(s))
        {
            return Err(verr("managed model checkpoint key table drifted"));
        }
        let fixed: BTreeSet<String> = [
            "param_keys",
            "adam_count",
            "stage_idx",
            "it_next",
            "l0",
            "t_offset",
            "n_rows",
            "refs_json",
            "cont",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let coord: BTreeSet<String> = param_slots
            .iter()
            .flat_map(|s| ["p_", "adam_mu_", "adam_nu_"].iter().map(move |p| format!("{p}{s}")))
            .collect();
        let projection: BTreeSet<String> =
            ["proj_vol", "proj_dv", "proj_var"].iter().map(|s| (*s).to_string()).collect();
        let fields: BTreeSet<String> = checkpoint.iter().map(|(k, _)| k.clone()).collect();
        let unknown: Vec<&String> = fields
            .iter()
            .filter(|f| {
                !fixed.contains(*f)
                    && !coord.contains(*f)
                    && !projection.contains(*f)
                    && !f.starts_with("warm_")
            })
            .collect();
        let projection_present = fields.iter().any(|f| projection.contains(f));
        if !fixed.is_subset(&fields)
            || !coord.is_subset(&fields)
            || !unknown.is_empty()
            || (projection_present && !projection.is_subset(&fields))
        {
            return Err(verr("managed model checkpoint layout drifted"));
        }
        let scalar_int = |name: &str| -> JobResult<i64> {
            let a = checkpoint.iter().find(|(k, _)| k == name).map(|(_, a)| a);
            match a.filter(|a| a.shape.is_empty()).map(|a| &a.data) {
                Some(NpyData::I64(v)) if v.len() == 1 => Ok(v[0]),
                Some(NpyData::I32(v)) if v.len() == 1 => Ok(i64::from(v[0])),
                _ => Err(verr(format!("managed checkpoint {name} is not an exact integer"))),
            }
        };
        let scalar_num = |name: &str| -> JobResult<f64> {
            let a = checkpoint.iter().find(|(k, _)| k == name).map(|(_, a)| a).filter(|a| a.shape.is_empty());
            let v = a.and_then(NpyArray::to_f64).and_then(|x| x.iter().next().copied());
            v.filter(|v| v.is_finite())
                .ok_or_else(|| verr(format!("managed checkpoint {name} is not finite")))
        };
        let n = i64::try_from(history.len()).unwrap_or(i64::MAX);
        if scalar_int("stage_idx")? != 0
            || scalar_int("it_next")? != n
            || scalar_int("n_rows")? != n
            || scalar_int("adam_count")? < 0
            || scalar_num("l0")? <= 0.0
            || !scalar_num("t_offset")?.is_finite()
        {
            return Err(verr("managed model checkpoint cursor drifted"));
        }
        let cont = array_of(&checkpoint, "cont");
        if is_complex(&checkpoint, "cont")
            || cont.as_ref().is_none_or(|c| c.shape() != [4] || !c.iter().all(|v| v.is_finite()))
        {
            return Err(verr("managed model checkpoint continuation is malformed"));
        }
        let refs_text = checkpoint
            .iter()
            .find(|(k, _)| k == "refs_json")
            .and_then(|(_, a)| a.as_scalar_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| verr("managed checkpoint refs_json is not text"))?;
        let references: Value = serde_json::from_str(refs_text)
            .map_err(|_| verr("managed model checkpoint references are malformed"))?;
        if !references.is_object() {
            return Err(verr("managed model checkpoint references are not an object"));
        }
        for slot in &param_slots {
            let shape = array_of(&checkpoint, &format!("p_{slot}")).map(|a| a.shape().to_vec());
            for prefix in ["p_", "adam_mu_", "adam_nu_"] {
                let key = format!("{prefix}{slot}");
                let v = array_of(&checkpoint, &key);
                if is_complex(&checkpoint, &key)
                    || v.as_ref().map(|a| a.shape().to_vec()) != shape
                    || v.as_ref().is_none_or(|a| !a.iter().all(|x| x.is_finite()))
                {
                    return Err(verr("managed model checkpoint coordinate state is malformed"));
                }
            }
        }
        for (name, _) in &checkpoint {
            if name.starts_with("warm_")
                && (is_complex(&checkpoint, name)
                    || array_of(&checkpoint, name).is_none_or(|a| !a.iter().all(|x| x.is_finite())))
            {
                return Err(verr("managed model checkpoint warm state is malformed"));
            }
        }
        if !completed {
            return Ok(());
        }
        let Some(summary) = summary.and_then(Value::as_object).filter(|_| !history.is_empty()) else {
            return Err(verr("managed model completion generation is empty"));
        };
        let l = |r: &Value| r.get("L").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let min_l = history.iter().map(l).fold(f64::INFINITY, f64::min);
        #[allow(clippy::float_cmp)]
        let drift = summary.get("solve_id").and_then(Value::as_str) != Some(j.solve_id.as_str())
            || summary.get("iterations").and_then(Value::as_u64) != Some(history.len() as u64)
            || summary.get("L_first").and_then(Value::as_f64) != Some(l(&history[0]))
            || summary.get("L_last").and_then(Value::as_f64) != Some(l(&history[history.len() - 1]))
            || summary.get("L_best").and_then(Value::as_f64) != Some(min_l);
        if drift {
            return Err(verr("managed model completion objective identity drifted"));
        }
        let (best_values, slots) = load_design("best.npz")?;
        let (final_values, final_slots) = load_design("final_model.npz")?;
        if final_slots != slots {
            return Err(verr("managed model final coordinate slots drifted"));
        }
        let fs = summary.get("free_start").and_then(Value::as_object);
        let ff = summary.get("free_final").and_then(Value::as_object);
        let (Some(fs), Some(ff)) = (fs, ff) else {
            return Err(verr("managed model completion coordinate evidence drifted"));
        };
        if fs.keys().collect::<BTreeSet<_>>() != refset || ff.keys().collect::<BTreeSet<_>>() != refset {
            return Err(verr("managed model completion coordinate evidence drifted"));
        }
        for r in &expected_refs {
            let start = crate::optimize::spec::json_array(&fs[r]);
            let fin = crate::optimize::spec::json_array(&ff[r]);
            if start.as_ref() != j.before_values.get(r) || fin.as_ref() != best_values.get(r) {
                return Err(verr("managed model completion design identity drifted"));
            }
        }
        let best_index =
            (0..history.len()).fold(0, |b, i| if l(&history[i]) < l(&history[b]) { i } else { b });
        if let Some(best_free) = history[best_index].get("free").filter(|v| !v.is_null()) {
            for (r, value) in &best_values {
                let e = &best_free[r];
                let mean = implexity_optim::numeric::array_mean(value);
                let min = value.iter().copied().fold(f64::INFINITY, f64::min);
                let max = value.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                #[allow(clippy::float_cmp)]
                let bad = e["mean"].as_f64() != Some(mean)
                    || e["min"].as_f64() != Some(min)
                    || e["max"].as_f64() != Some(max)
                    || (value.ndim() == 0 && e["value"].as_f64() != value.iter().next().copied());
                if bad {
                    return Err(verr("managed model best design disagrees with its history row"));
                }
            }
        }
        if param_slots.iter().collect::<BTreeSet<_>>() != slots.iter().collect::<BTreeSet<_>>() {
            return Err(verr("managed model checkpoint coordinate slots drifted"));
        }
        let descriptors: BTreeMap<String, &Value> = j
            .free
            .iter()
            .filter(|f| f.is_object())
            .map(|f| (f.get("ref").map(py_str).unwrap_or_default(), f))
            .collect();
        let scaling = j.settings.get("scaling").map_or_else(|| "unit_range".to_string(), py_str);
        for (r, slot) in expected_refs.iter().zip(&slots) {
            let z = array_of(&checkpoint, &format!("p_{slot}")).unwrap_or_default();
            let expected_final = match scaling.as_str() {
                "raw" => z,
                "unit_range" => {
                    let span = descriptors.get(r).and_then(|d| d.get("span")).and_then(Value::as_f64);
                    let Some(span) = span.filter(|s| s.is_finite() && *s > 0.0) else {
                        return Err(verr("managed model coordinate scaling evidence is missing"));
                    };
                    let origin = by_ref[r].get("lo").and_then(Value::as_f64).unwrap_or(0.0);
                    z.mapv(|v| origin + v * span)
                }
                _ => return Err(verr("managed model coordinate scaling drifted")),
            };
            if Some(&expected_final) != final_values.get(r) {
                return Err(verr("managed model checkpoint disagrees with final design"));
            }
        }
        if j.live_every != 0 {
            let (live_values, live_slots) = load_design("live_model.npz")?;
            if live_slots != slots || expected_refs.iter().any(|r| live_values.get(r) != final_values.get(r))
            {
                return Err(verr("managed model live terminal disagrees with final design"));
            }
            let push = history[history.len() - 1].get("push");
            let size = std::fs::metadata(partial.join("live_model.npz"))?.len();
            if push.and_then(Value::as_object).is_none_or(|p| {
                p.get("file").and_then(Value::as_str) != Some("live_model.npz")
                    || p.get("bytes").and_then(Value::as_u64) != Some(size)
            }) {
                return Err(verr("managed model terminal live reference drifted"));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn validate_numerical_attention_sidecar(
        &self,
        job: &LiveJob,
        partial: &Path,
    ) -> JobResult<Value> {
        let (execution, provider) = {
            let j = lock(job);
            (j.provider_execution.clone(), j.physics_provider.clone())
        };
        if execution != "array" {
            return Err(verr("numerical attention requires a hierarchical provider job"));
        }
        let attention = read_json(&partial.join("numerical_attention.json"), 1024 * 1024)?;
        let keys = ["schema", "event", "deviation", "safe_checkpoint", "event_digest"];
        let Some(a) = attention.as_object().filter(|a| {
            a.len() == keys.len()
                && keys.iter().all(|k| a.contains_key(*k))
                && a["schema"] == "implexity-numerical-attention-checkpoint/1"
                && a["event"] == "numerical_certification_deviation"
        }) else {
            return Err(verr("managed numerical attention is malformed"));
        };
        let unsigned: Map<String, Value> = a
            .iter()
            .filter(|(k, _)| k.as_str() != "event_digest")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let digest =
            crate::private::sha256_hex(crate::private::canonical_text(&Value::Object(unsigned)).as_bytes());
        if a["event_digest"].as_str() != Some(digest.as_str()) {
            return Err(verr("managed numerical attention digest drifted"));
        }
        let expected_dev = [
            "schema",
            "solver_id",
            "solver_prefix",
            "solver_record",
            "observed_relative_residual",
            "nominal_relative_residual_limit",
            "bounded_ceiling",
            "attempt_count",
            "attempt_limit",
            "max_iterations_per_attempt",
            "retry_max_iterations_per_attempt",
            "retry_attempt_limit",
            "exact_retry_available",
            "exploratory_continuation_available",
        ];
        let d = a["deviation"].as_object();
        let ok = d.is_some_and(|d| {
            d.len() == expected_dev.len()
                && expected_dev.iter().all(|k| d.contains_key(*k))
                && d["schema"] == "implexity-numerical-certification-deviation/1"
                && d["solver_id"].as_str().is_some_and(|s| !s.is_empty())
                && d["solver_prefix"].as_str().is_some_and(|s| !s.is_empty())
                && d["solver_id"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with(d["solver_prefix"].as_str().unwrap_or("\u{0}"))
        });
        let Some(d) = d.filter(|_| ok) else {
            return Err(verr("managed numerical deviation evidence is malformed"));
        };
        for name in ["observed_relative_residual", "nominal_relative_residual_limit", "bounded_ceiling"] {
            if !d[name].as_f64().is_some_and(|v| v.is_finite() && v > 0.0) {
                return Err(verr("managed numerical deviation residual is malformed"));
            }
        }
        let f = |k: &str| d[k].as_f64().unwrap_or(f64::NAN);
        if !(f("nominal_relative_residual_limit") < f("observed_relative_residual")
            && f("observed_relative_residual") <= f("bounded_ceiling"))
        {
            return Err(verr("managed numerical deviation is outside its bounded envelope"));
        }
        let record = d["solver_record"].as_object();
        let cert = record.and_then(|r| r.get("certification")).and_then(Value::as_object);
        let rec_ok = record.is_some_and(|r| {
            r.get("schema").and_then(Value::as_str) == Some("implexity-numerical-solver-record/1")
                && r.get("solver_id") == Some(&d["solver_id"])
                && r.get("solve_scope").and_then(Value::as_str) == Some("primal_forward")
                && r.get("final_relative_residual") == Some(&d["observed_relative_residual"])
        }) && cert.is_some_and(|c| {
            c.get("passed") == Some(&Value::Bool(false))
                && c.get("relative_residual_limit") == Some(&d["nominal_relative_residual_limit"])
        });
        if !rec_ok {
            return Err(verr("managed numerical deviation solver record drifted"));
        }
        for name in [
            "attempt_count",
            "attempt_limit",
            "max_iterations_per_attempt",
            "retry_max_iterations_per_attempt",
            "retry_attempt_limit",
        ] {
            if !(is_int(d.get(name)) && d[name].as_i64().is_some_and(|v| v >= 1)) {
                return Err(verr("managed numerical deviation retry evidence is malformed"));
            }
        }
        let i = |k: &str| d[k].as_i64().unwrap_or(0);
        if i("attempt_count") > i("attempt_limit")
            || i("retry_max_iterations_per_attempt") < i("max_iterations_per_attempt")
            || i("retry_attempt_limit") < i("attempt_limit")
        {
            return Err(verr("managed numerical deviation retry budget is inconsistent"));
        }
        let Some(retry) = d["exact_retry_available"].as_bool() else {
            return Err(verr("managed numerical deviation retry availability is malformed"));
        };
        let expected_retry = i("retry_max_iterations_per_attempt") > i("max_iterations_per_attempt")
            || i("retry_attempt_limit") > i("attempt_limit");
        if retry != expected_retry {
            return Err(verr("managed numerical deviation retry availability drifted"));
        }
        if !d["exploratory_continuation_available"].is_boolean() {
            return Err(verr("managed numerical deviation exploratory availability is malformed"));
        }
        let identity = [
            "run_fingerprint",
            "runtime_source_sha256",
            "package_identity_sha256",
            "provider_descriptor_sha256",
            "provider_profile_sha256",
            "requested_policy_digest",
            "effective_effort_digest",
            "operation_context_digest",
            "solve_id",
            "document_content_id",
            "design_state_id",
        ];
        let extra = ["iteration", "history_length", "history_digest", "provider", "push"];
        let Some(cp) = a["safe_checkpoint"].as_object().filter(|c| {
            c.len() == identity.len() + extra.len()
                && identity.iter().chain(extra.iter()).all(|k| c.contains_key(*k))
        }) else {
            return Err(verr("managed numerical attention checkpoint is malformed"));
        };
        let history_payload = read_json(&partial.join("history.json"), JSON_LIMIT)?;
        let Some(history) =
            history_payload.get("history").and_then(Value::as_array).filter(|h| !h.is_empty())
        else {
            return Err(verr("managed numerical attention history is empty"));
        };
        let last = &history[history.len() - 1];
        let drift = cp["history_length"].as_u64() != Some(history.len() as u64)
            || cp["history_digest"].as_str()
                != Some(crate::hierarchical_job::history_digest_of(history).as_str())
            || Some(&cp["iteration"]) != last.get("iteration")
            || last.get("accepted") != Some(&Value::Bool(true))
            || last.get("continuable") != Some(&Value::Bool(true))
            || cp["provider"].as_str() != Some(provider.as_str())
            || cp["push"] != *last.get("push").unwrap_or(&Value::Null)
            || identity.iter().any(|k| cp[*k] != *last.get(*k).unwrap_or(&Value::Null));
        if drift {
            return Err(verr("managed numerical attention checkpoint identity drifted"));
        }
        Ok(attention)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn validate_managed_optimization_terminal(
        &self,
        job: &LiveJob,
        partial: &Path,
    ) -> JobResult<()> {
        let (fp, effort, execution, requested, plan) = {
            let j = lock(job);
            (
                j.managed_spec_fingerprint.clone(),
                j.computation_effort.clone(),
                j.provider_execution.clone(),
                j.managed_requested_control.clone(),
                j.plan.clone(),
            )
        };
        let Some(fp) = fp.filter(Value::is_object) else {
            return Err(verr("managed optimization private specification drifted"));
        };
        if artifact_fingerprint(&partial.join("spec.json"), JSON_LIMIT)? != fp {
            return Err(verr("managed optimization private specification drifted"));
        }
        let summary_path = partial.join("summary.json");
        if summary_path.exists() || summary_path.is_symlink() {
            let summary = read_json(&summary_path, JSON_LIMIT)?;
            let ok = summary.as_object().is_some_and(|s| {
                s.get("status").and_then(Value::as_str) == Some("completed")
                    && s.get("history").is_some_and(Value::is_array)
                    && is_int(s.get("iterations"))
                    && s["iterations"].as_u64() == s["history"].as_array().map(|h| h.len() as u64)
                    && s["iterations"].as_i64().is_some_and(|n| n >= 1)
            });
            if !ok {
                return Err(verr("managed optimization completion summary is malformed"));
            }
            for key in ["L_first", "L_best", "L_last"] {
                if !finite_number(summary.get(key)) {
                    return Err(verr("managed optimization completion summary is nonfinite"));
                }
            }
            if let Some(effort) = &effort {
                implexity_runtime::provider_job_authority::validate_exact_effort_evidence(&summary, effort)?;
            }
            let regular = |n: &str| {
                let p = partial.join(n);
                p.is_file() && !p.is_symlink()
            };
            if !regular("best.npz") || !regular("ckpt.npz") || !regular("history.json") {
                return Err(verr("managed optimization completion artifacts are incomplete"));
            }
            let sidecar = read_json(&partial.join("history.json"), JSON_LIMIT)?;
            if !sidecar.is_object() || sidecar.get("history") != summary.get("history") {
                return Err(verr("managed optimization history sidecar drifted"));
            }
            if execution == "array" {
                crate::hierarchical_job::validate_managed_generation(partial, true)?;
            } else {
                self.validate_legacy_model_generation(job, partial, true, Some(&summary), Some(sidecar))?;
            }
            check_checkpoint_file(&partial.join("ckpt.npz"), "managed optimization checkpoint")?;
            let values = crate::optimize::solve::load_model_npz(&partial.join("best.npz"))?;
            let names: BTreeSet<String> = values.into_iter().map(|(k, _)| k).collect();
            if plan.iter().any(|e| !names.contains(&e.get("ref").map(py_str).unwrap_or_default())) {
                return Err(verr("managed optimization best design is incomplete"));
            }
            return Ok(());
        }
        let attention_path = partial.join("numerical_attention.json");
        if attention_path.exists() || attention_path.is_symlink() {
            if requested.is_some() {
                return Err(verr("managed numerical attention conflicts with requested control"));
            }
            crate::hierarchical_job::validate_managed_generation(partial, false)?;
            self.validate_numerical_attention_sidecar(job, partial)?;
            return Ok(());
        }
        if requested.as_deref() != Some("pause") {
            return Err(verr("managed optimization produced no completed summary"));
        }
        let ck = partial.join("ckpt.npz");
        let hp = partial.join("history.json");
        if !ck.is_file() || ck.is_symlink() || !hp.is_file() || hp.is_symlink() {
            return Err(verr("managed optimization pause checkpoint is incomplete"));
        }
        let history = read_json(&hp, JSON_LIMIT)?;
        if execution == "array" {
            let ok = history.get("schema").and_then(Value::as_str)
                == Some(crate::hierarchical_job::HISTORY_SCHEMA)
                && history.get("history").is_some_and(Value::is_array);
            if !ok {
                return Err(verr("managed provider pause history is malformed"));
            }
            crate::hierarchical_job::validate_managed_generation(partial, false)?;
        } else {
            self.validate_legacy_model_generation(job, partial, false, None, Some(history))?;
        }
        check_checkpoint_file(&ck, "managed optimization pause checkpoint")
    }

    pub(crate) fn seal_managed_terminal(
        &self,
        job: &LiveJob,
        committed: &Path,
        terminal_kind: &str,
    ) -> JobResult<()> {
        let (execution, live_every) = {
            let j = lock(job);
            (j.provider_execution.clone(), j.live_every)
        };
        let allowed: BTreeSet<&str> = [
            "spec.json",
            "best.npz",
            "final_model.npz",
            "final.npz",
            "initial.npz",
            "initial_design.json",
            "ckpt.npz",
            "history.json",
            "summary.json",
            "section_zmid.png",
            "matching_time_guess_consumed.json",
            "resume_warm_start.npz",
            "resume_warm_start.alt.npz",
            "resume_warm_start.json",
            "continuation.json",
            "numerical_attention.json",
            "steer.json",
        ]
        .into_iter()
        .collect();
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(committed)?
            .filter_map(Result::ok)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        let mut files = Map::new();
        for (name, path) in names {
            if allowed.contains(name.as_str())
                || is_decision_name(&name)
                || is_live_epoch_name(&name)
                || name == "live_model.npz"
            {
                files.insert(name, artifact_fingerprint(&path, COPY_LIMIT)?);
            }
        }
        for relative in crate::epoch_state::epoch_state_files(committed) {
            let fingerprint = artifact_fingerprint(&committed.join(&relative), COPY_LIMIT)?;
            files.insert(relative, fingerprint);
        }
        let mut required: BTreeSet<&str> = ["spec.json", "ckpt.npz", "history.json"].into_iter().collect();
        if terminal_kind == "completed" {
            required.extend(["best.npz", "summary.json"]);
            if execution == "array" {
                required.extend(["initial.npz", "initial_design.json", "final.npz"]);
            } else {
                required.insert("final_model.npz");
                if live_every != 0 {
                    required.insert("live_model.npz");
                }
            }
        } else if execution == "array" {
            required.extend(["initial.npz", "initial_design.json", "best.npz"]);
            if terminal_kind == "numerical_attention" {
                required.insert("numerical_attention.json");
            }
        }
        if required.iter().any(|r| !files.contains_key(*r)) {
            return Err(verr("managed terminal seal is incomplete"));
        }
        lock(job).managed_terminal_manifest = Some(json!({
            "schema": "implexity-private-optimization-terminal-manifest/1",
            "terminal_kind": terminal_kind,
            "files": files,
        }));
        Ok(())
    }

    pub(crate) fn managed_resume_history_prefix(&self, job: &LiveJob) -> JobResult<Vec<Value>> {
        let (manifest, dir, rows) = {
            let j = lock(job);
            (
                j.managed_terminal_manifest.clone(),
                PathBuf::from(j.job_dir.clone().unwrap_or_default()),
                j.rows.len(),
            )
        };
        let kind =
            manifest.as_ref().and_then(|m| m.get("terminal_kind")).and_then(Value::as_str).unwrap_or("");
        if kind != "paused" && kind != "numerical_attention" {
            return Err(verr("resume prefix requires a validated checkpoint seal"));
        }
        self.verify_managed_terminal_seal(manifest.as_ref(), &dir)?;
        let payload = read_json(&dir.join("history.json"), JSON_LIMIT)?;
        let history = payload.get("history").and_then(Value::as_array).cloned();
        match history {
            Some(h) if h.len() == rows && h.iter().all(Value::is_object) => Ok(h),
            _ => Err(verr("sealed resume history differs from committed progress")),
        }
    }

    pub(crate) fn materialize_managed_optimization(&self, job_dir: &Path, committed: &Path) -> JobResult<()> {
        let allowed: BTreeSet<&str> = [
            "spec.json",
            "best.npz",
            "final_model.npz",
            "final.npz",
            "initial.npz",
            "initial_design.json",
            "ckpt.npz",
            "history.json",
            "summary.json",
            "section_zmid.png",
            "live_model.npz",
            "matching_time_guess_consumed.json",
            "resume_warm_start.npz",
            "resume_warm_start.alt.npz",
            "resume_warm_start.json",
            "continuation.json",
            "steer.json",
            "numerical_attention.json",
        ]
        .into_iter()
        .collect();
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(committed)?
            .filter_map(Result::ok)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        for (name, path) in names {
            if allowed.contains(name.as_str()) || is_decision_name(&name) || is_live_epoch_name(&name) {
                copy_regular(&path, &job_dir.join(&name), COPY_LIMIT, true)?;
            }
        }
        crate::epoch_state::copy_epoch_states(committed, job_dir, None)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn finish_managed_success(
        &self,
        job: &LiveJob,
        protocol: &Protocol,
        committed: &Path,
        resume_prefix: &[Value],
    ) -> JobResult<()> {
        let require_history = |history: &[Value], received: &[Value]| -> JobResult<()> {
            if history.len() < resume_prefix.len()
                || history[..resume_prefix.len()] != *resume_prefix
                || received != &history[resume_prefix.len()..]
            {
                return Err(verr("managed optimization iteration protocol drifted"));
            }
            Ok(())
        };
        let job_dir = self.job_dir(job);
        let attention_path = committed.join("numerical_attention.json");
        if attention_path.is_file() {
            let attention = self.validate_numerical_attention_sidecar(job, committed)?;
            if protocol.attention.as_ref() != Some(&attention)
                || protocol.done.is_some()
                || protocol.halted.is_some()
                || !protocol.errors.is_empty()
            {
                return Err(verr("managed numerical attention protocol drifted"));
            }
            let payload = read_json(&committed.join("history.json"), JSON_LIMIT)?;
            let history = payload.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
            require_history(&history, &protocol.rows)?;
            self.seal_managed_terminal(job, committed, "numerical_attention")?;
            self.materialize_managed_optimization(&job_dir, committed)?;
            let manifest = lock(job).managed_terminal_manifest.clone();
            self.verify_managed_terminal_seal(manifest.as_ref(), &job_dir)?;
            let mut last = {
                let mut j = lock(job);
                j.rows.clone_from(&history);
                j.last_apply_t = None;
                j.rows.last().cloned().unwrap_or(Value::Null)
            };
            self.apply_live(job, &mut last)?;
            {
                let mut j = lock(job);
                if let Some(slot) = j.rows.last_mut() {
                    *slot = last;
                }
            }
            let deviation = attention["deviation"].clone();
            let checkpoint = attention["safe_checkpoint"].clone();
            let (id, problem) = {
                let j = lock(job);
                (j.id.clone(), j.provider_problem.clone())
            };
            let event_token = crate::private::sha256_hex(
                format!("{id}:{}", attention["event_digest"].as_str().unwrap_or("")).as_bytes(),
            );
            let retry_ok = deviation["exact_retry_available"].as_bool().unwrap_or(false);
            let explore_ok = deviation["exploratory_continuation_available"].as_bool().unwrap_or(false);
            let mut actions = Vec::new();
            if retry_ok {
                actions.push(json!("retry_exact"));
            }
            if explore_ok {
                actions.push(json!("continue_exploratory"));
            }
            actions.push(json!("discard"));
            let problem_sha256 =
                implexity_core::wire::fingerprint_value(&implexity_core::wire::to_wire(&problem)?);
            let record = json!({
                "schema": "implexity-numerical-attention/1",
                "state": "open",
                "event": "numerical_certification_deviation",
                "event_token": event_token,
                "action": null,
                "outcome": "pending",
                "result_authority": null,
                "solver_id": deviation["solver_id"],
                "solver_prefix": deviation["solver_prefix"],
                "solver_record": deviation["solver_record"],
                "observed_relative_residual": deviation["observed_relative_residual"],
                "nominal_relative_residual_limit": deviation["nominal_relative_residual_limit"],
                "bounded_ceiling": deviation["bounded_ceiling"],
                "exact_retry_budget": {
                    "max_iterations_per_attempt": deviation["retry_max_iterations_per_attempt"],
                    "attempt_limit": deviation["retry_attempt_limit"],
                    "equations_changed": false,
                    "tolerance_changed": false,
                    "certification_limit_changed": false,
                },
                "exact_retry_available": deviation["exact_retry_available"],
                "exploratory_continuation_available": deviation["exploratory_continuation_available"],
                "available_actions": actions,
                "provenance": {
                    "source_job_id": id,
                    "provider": checkpoint["provider"],
                    "solve_id": checkpoint["solve_id"],
                    "design_state_id": checkpoint["design_state_id"],
                    "run_fingerprint": checkpoint["run_fingerprint"],
                    "runtime_source_sha256": checkpoint["runtime_source_sha256"],
                    "problem_sha256": problem_sha256,
                    "requested_policy_digest": checkpoint["requested_policy_digest"],
                    "effective_effort_digest": checkpoint["effective_effort_digest"],
                    "operation_context_digest": checkpoint["operation_context_digest"],
                },
            });
            {
                let mut j = lock(job);
                j.numerical_attention = Some(record);
                j.status = "attention".into();
            }
            self.hold_numerical_attention_live(job);
            let mut j = lock(job);
            j.mark("numerical_attention");
            j.managed_supervisor_reason = Some("bounded_numerical_certification_deviation".into());
            j.message =
                "exact numerical solve missed certification by a bounded finite margin; choose exact retry, \
                         exploratory continuation, or discard"
                    .into();
            return Ok(());
        }
        let summary_path = committed.join("summary.json");
        if summary_path.is_file() {
            let summary = read_json(&summary_path, JSON_LIMIT)?;
            let mut expected_done = summary.as_object().cloned().unwrap_or_default();
            expected_done.shift_remove("history");
            if protocol.done.as_ref() != Some(&Value::Object(expected_done.clone()))
                || protocol.halted.is_some()
            {
                return Err(verr("managed optimization completion protocol drifted"));
            }
            let history = summary.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
            require_history(&history, &protocol.rows)?;
            if !protocol.errors.is_empty() {
                return Err(verr("managed optimization reported an error before completion"));
            }
            self.seal_managed_terminal(job, committed, "completed")?;
            self.materialize_managed_optimization(&job_dir, committed)?;
            let manifest = lock(job).managed_terminal_manifest.clone();
            self.verify_managed_terminal_seal(manifest.as_ref(), &job_dir)?;
            let consumed =
                expected_done.get("matching_time_guess_consumed").filter(|v| !v.is_null()).cloned();
            let consumed = match consumed {
                Some(c) => Some(Value::Object(implexity_solve::matching_time_guess::public_descriptor(&c)?)),
                None => None,
            };
            let mut j = lock(job);
            j.rows = history;
            let lf = expected_done.get("L_first").and_then(Value::as_f64).unwrap_or(f64::NAN);
            let lb = expected_done.get("L_best").and_then(Value::as_f64).unwrap_or(f64::NAN);
            j.summary = Some(expected_done);
            if consumed.is_some() {
                j.matching_time_guess_consumed = consumed;
            }
            j.status = "completed".into();
            j.mark("completed");
            j.progress = 1.0;
            j.managed_supervisor_reason = Some("completed_validated".into());
            j.message = format!(
                "completed: L {} -> best {}; accept to write the optimised values into the model document",
                implexity_geometry::pyfmt::fmt_f(lf, 6),
                implexity_geometry::pyfmt::fmt_f(lb, 6)
            );
            return Ok(());
        }
        let (requested, effort, execution) = {
            let j = lock(job);
            (j.managed_requested_control.clone(), j.computation_effort.clone(), j.provider_execution.clone())
        };
        let halted = protocol.halted.clone().unwrap_or(Value::Null);
        if requested.as_deref() != Some("pause")
            || !halted.is_object()
            || halted.get("op").and_then(Value::as_str) != Some("pause")
            || protocol.done.is_some()
            || !protocol.errors.is_empty()
        {
            return Err(verr("managed optimization pause protocol drifted"));
        }
        if let Some(effort) = &effort {
            implexity_runtime::provider_job_authority::validate_exact_effort_evidence(&halted, effort)?;
        }
        let payload = read_json(&committed.join("history.json"), JSON_LIMIT)?;
        let history = payload.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
        if halted.get("iterations").and_then(Value::as_u64) != Some(history.len() as u64) {
            return Err(verr("managed optimization pause iteration count drifted"));
        }
        require_history(&history, &protocol.rows)?;
        self.seal_managed_terminal(job, committed, "paused")?;
        self.materialize_managed_optimization(&job_dir, committed)?;
        let manifest = lock(job).managed_terminal_manifest.clone();
        self.verify_managed_terminal_seal(manifest.as_ref(), &job_dir)?;
        let n = history.len();
        {
            let mut j = lock(job);
            j.rows = history;
        }
        if execution == "array" && n > 0 {
            let mut last = {
                let mut j = lock(job);
                j.last_apply_t = None;
                j.rows.last().cloned().unwrap_or(Value::Null)
            };
            self.apply_live(job, &mut last)?;
            if let Some(slot) = lock(job).rows.last_mut() {
                *slot = last;
            }
        }
        let mut j = lock(job);
        j.status = "paused".into();
        j.mark("paused");
        j.managed_supervisor_reason = Some("pause_checkpoint_validated".into());
        j.message = format!(
            "paused after {n} iteration(s) (validated checkpoint; resume uses the remaining original wall-time budget)"
        );
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn managed_design_values(
        job: &super::job::ModelOptJob,
        arrays: &[(String, NpyArray)],
    ) -> JobResult<BTreeMap<String, ArrayD<f64>>> {
        let expected_refs: Vec<String> =
            job.plan.iter().map(|e| e.get("ref").map(py_str).unwrap_or_default()).collect();
        if expected_refs.is_empty()
            || expected_refs.iter().collect::<BTreeSet<_>>().len() != expected_refs.len()
        {
            return Err(verr("managed best-design plan is not closed"));
        }
        let refs = text_vector(arrays, "refs", "best-design")?;
        let slots = text_vector(arrays, "slots", "best-design")?;
        let units = text_vector(arrays, "units", "best-design")?;
        let expected_units: Vec<String> = job
            .plan
            .iter()
            .map(|e| {
                e.get("node_units")
                    .filter(|v| truthy(v))
                    .or_else(|| e.get("units").filter(|v| truthy(v)))
                    .map_or_else(|| "-".to_string(), py_str)
            })
            .collect();
        if refs != expected_refs
            || slots.len() != refs.len()
            || slots.iter().collect::<BTreeSet<_>>().len() != slots.len()
            || units != expected_units
            || slots.iter().any(|s| !is_slot(s))
        {
            return Err(verr("managed best-design coordinate table drifted"));
        }
        let mut expected: BTreeSet<String> =
            ["refs", "slots", "units", "solve_id"].iter().map(|s| (*s).to_string()).collect();
        expected.extend(slots.iter().map(|s| format!("p_{s}")));
        if arrays.iter().map(|(k, _)| k.clone()).collect::<BTreeSet<_>>() != expected {
            return Err(verr("managed best-design archive layout drifted"));
        }
        let sid = arrays.iter().find(|(k, _)| k == "solve_id").map(|(_, a)| a);
        if sid.and_then(|a| if a.shape.is_empty() { a.as_scalar_str() } else { None })
            != Some(job.solve_id.as_str())
        {
            return Err(verr("managed best-design solve identity drifted"));
        }
        let mut values = BTreeMap::new();
        for ((r, slot), entry) in refs.iter().zip(&slots).zip(&job.plan) {
            let key = format!("p_{slot}");
            if is_complex(arrays, &key) {
                return Err(verr("managed best-design contains complex data"));
            }
            let value = array_of(arrays, &key).ok_or_else(|| verr("managed best-design is not numeric"))?;
            let expected_shape: Vec<usize> = match job.before_values.get(r) {
                Some(b) => b.shape().to_vec(),
                None => entry
                    .get("start")
                    .and_then(crate::optimize::spec::json_array)
                    .map(|a| a.shape().to_vec())
                    .unwrap_or_default(),
            };
            let bound = |key: &str, which: &str| -> JobResult<Option<ArrayD<f64>>> {
                match entry.get(key).filter(|v| !v.is_null()) {
                    None => Ok(None),
                    Some(v) => implexity_optim::design_state::full_like(&value, v, which, r)
                        .map(Some)
                        .map_err(JobError::from),
                }
            };
            let lo = bound("lo", "lower")?;
            let hi = bound("hi", "upper")?;
            let violates = value.shape() != expected_shape.as_slice()
                || !value.iter().all(|v| v.is_finite())
                || lo.as_ref().is_some_and(|l| value.iter().zip(l.iter()).any(|(v, l)| v < l))
                || hi.as_ref().is_some_and(|h| value.iter().zip(h.iter()).any(|(v, h)| v > h));
            if violates {
                return Err(verr("managed best-design violates shape, bounds, or finiteness"));
            }
            values.insert(r.clone(), value);
        }
        Ok(values)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn managed_load_live_design(
        &self,
        job: &LiveJob,
        row: &mut Value,
        terminal_observation: bool,
    ) -> JobResult<BTreeMap<String, ArrayD<f64>>> {
        let snapshot = lock(job).clone();
        let provider_array = snapshot.provider_execution == "array";
        let producer: BTreeSet<&str> = if provider_array {
            ["file", "bytes", "sha256"].into_iter().collect()
        } else {
            ["file", "bytes", "save_ms"].into_iter().collect()
        };
        let verification: BTreeSet<&str> =
            ["verified_sha256", "verified_design_state_id"].into_iter().collect();
        let full: BTreeSet<&str> = producer.union(&verification).copied().collect();
        let Some(push) = row.get("push").and_then(Value::as_object).cloned() else {
            return Err(verr("managed live snapshot reference is malformed"));
        };
        let keys: BTreeSet<&str> = push.keys().map(String::as_str).collect();
        if keys != producer && keys != full {
            return Err(verr("managed live snapshot reference is malformed"));
        }
        let previously_verified = keys == full;
        let name = push.get("file").and_then(Value::as_str).unwrap_or("").to_string();
        let allowed = if provider_array { is_live_epoch_name(&name) } else { name == "live_model.npz" };
        if !allowed {
            return Err(verr("managed live snapshot name is not approved"));
        }
        let expected_bytes = push
            .get("bytes")
            .filter(|v| implexity_optim::pyval::is_int(v))
            .and_then(Value::as_i64)
            .filter(|b| *b > 0);
        let Some(expected_bytes) = expected_bytes else {
            return Err(verr("managed live snapshot size is malformed"));
        };
        let declared_sha = push.get("sha256").and_then(Value::as_str).map(str::to_string);
        if provider_array {
            if !declared_sha.as_deref().is_some_and(crate::private::is_sha256) {
                return Err(verr("managed live snapshot digest is malformed"));
            }
        } else if !push.get("save_ms").and_then(Value::as_f64).is_some_and(|v| v.is_finite() && v >= 0.0) {
            return Err(verr("managed live snapshot save duration is malformed"));
        }
        let control = snapshot
            .managed_control
            .clone()
            .ok_or_else(|| verr("managed optimization control is unavailable"))?;
        let directory = if terminal_observation {
            self.inner.supervisor.terminal_observation_directory(&control)?
        } else {
            self.managed_output_directory(Some(&control))?
        };
        let hard_limit = (8 * 1024 * 1024_i64)
            .max(snapshot.managed_budget_memory_bytes.unwrap_or(0) / 8)
            .min(512 * 1024 * 1024);
        let loaded = (|| -> JobResult<(BTreeMap<String, ArrayD<f64>>, Value)> {
            let (arrays, fp) =
                read_npz(&directory.join(&name), u64::try_from(hard_limit).unwrap_or(NPZ_LIMIT), None)?;
            if fp.get("bytes").and_then(Value::as_i64) != Some(expected_bytes) {
                return Err(verr("managed live snapshot declared byte count drifted"));
            }
            if provider_array && fp.get("sha256").and_then(Value::as_str) != declared_sha.as_deref() {
                return Err(verr("managed live snapshot declared digest drifted"));
            }
            let values = Self::managed_design_values(&snapshot, &arrays)?;
            Ok((values, fp))
        })();
        let (values, fp) = match loaded {
            Ok(v) => v,
            Err(e)
                if e.is_value_error() || matches!(e.python_class(), "KeyError" | "OSError" | "TypeError") =>
            {
                return Err(verr("managed live snapshot payload is invalid"));
            }
            Err(e) => return Err(e),
        };
        let named = implexity_optim::NamedArrays::from_pairs(values.clone());
        let design_state_id = implexity_optim::design_identity(&named)?;
        let declared_design = row.get("design_state_id").cloned();
        if provider_array && declared_design.as_ref().and_then(Value::as_str).is_none_or(str::is_empty) {
            return Err(verr("managed provider live snapshot omits design identity"));
        }
        if let Some(d) = declared_design.filter(|d| !d.is_null())
            && d.as_str() != Some(design_state_id.as_str())
        {
            return Err(verr("managed live snapshot design identity drifted"));
        }
        let sha = fp.get("sha256").cloned().unwrap_or(Value::Null);
        if previously_verified && push.get("verified_sha256") != Some(&sha) {
            return Err(verr("managed live snapshot verified digest drifted"));
        }
        if previously_verified
            && push.get("verified_design_state_id").and_then(Value::as_str) != Some(design_state_id.as_str())
        {
            return Err(verr("managed live snapshot verified design identity drifted"));
        }
        if let Some(p) = row.get_mut("push").and_then(Value::as_object_mut) {
            p.insert("verified_sha256".into(), sha);
            p.insert("verified_design_state_id".into(), json!(design_state_id));
        }
        Ok(values)
    }

    pub(crate) fn provider_derived_values(
        &self,
        job: &super::job::ModelOptJob,
        values: &BTreeMap<String, ArrayD<f64>>,
    ) -> JobResult<BTreeMap<String, ArrayD<f64>>> {
        if job.provider_derived_plan.is_empty() {
            return Ok(BTreeMap::new());
        }
        let Some(hook) = &job.provider_derived_hook else {
            return Err(verr("provider-derived model plan lost its provider hook"));
        };
        if job.provider_derived_design_base.is_empty() {
            return Err(verr("provider-derived model plan lost its complete design base"));
        }
        let mut design = job.provider_derived_design_base.clone();
        for entry in &job.plan {
            let r = entry.get("ref").map(py_str).unwrap_or_default();
            if let Some(v) = values.get(&r) {
                design.insert(r, v.clone());
            }
        }
        if design.keys().collect::<BTreeSet<_>>()
            != job.provider_derived_design_base.keys().collect::<BTreeSet<_>>()
        {
            return Err(verr("provider-derived model design identity drifted"));
        }
        let raw = (hook.0)(&design).map_err(|e| {
            verr(format!(
                "provider {} failed to derive model outputs: {}",
                implexity_core::py_repr::repr_str(&job.physics_provider),
                e.message()
            ))
        })?;
        super::declare::validated_provider_derived_values(
            &job.provider_derived_plan,
            &raw,
            &format!(
                "provider {} derive_model_updates",
                implexity_core::py_repr::repr_str(&job.physics_provider)
            ),
        )
    }


    pub(crate) fn managed_publish_bytes_once(
        destination: &Path,
        payload: &[u8],
        maximum_bytes: u64,
    ) -> JobResult<bool> {
        let name = destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.is_empty()
            || name == "."
            || name == ".."
            || payload.is_empty()
            || payload.len() as u64 > maximum_bytes
            || maximum_bytes == 0
        {
            return Err(verr("managed immutable artifact contract is invalid"));
        }
        let expected = json!({"bytes": payload.len(), "sha256": crate::private::sha256_hex(payload)});
        match artifact_fingerprint(destination, maximum_bytes) {
            Ok(actual) => {
                if actual != expected {
                    return Err(verr("managed immutable artifact name is already occupied"));
                }
                return Ok(false);
            }
            Err(e) if destination.exists() || destination.is_symlink() => return Err(e),
            Err(_) => {}
        }
        let directory =
            destination.parent().ok_or_else(|| verr("managed immutable artifact directory is unsafe"))?;
        let before = implexity_io::fsguard::stat_nofollow(directory)?;
        if before.is_symlink() || !before.is_dir() || !before.owned {
            return Err(verr("managed immutable artifact directory is unsafe"));
        }
        let temporary = directory.join(format!(".{name}.publish-{}.tmp", crate::private::token_hex(16)?));
        let result = (|| -> JobResult<bool> {
            let mut f = crate::private::create_exclusive(&temporary, 0o600)?;
            crate::private::write_all_sync(&mut f, payload)?;
            drop(f);
            let linked = match std::fs::hard_link(&temporary, destination) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(e) => return Err(e.into()),
            };
            let d = implexity_io::fsguard::Dir::open(directory)?;
            let dm = d.stat()?;
            if !dm.same_object(&before) || !dm.is_dir() || !dm.owned {
                return Err(verr("managed immutable artifact directory drifted"));
            }
            d.sync_all()?;
            Ok(linked)
        })();
        let _ = std::fs::remove_file(&temporary);
        let linked = result?;
        if artifact_fingerprint(destination, maximum_bytes)? != expected {
            return Err(verr("managed immutable artifact verification failed"));
        }
        Ok(linked)
    }

    pub(crate) fn prepare_model_before_bundle(&self, job: &super::job::ModelOptJob) -> JobResult<Value> {
        let canonical = implexity_geometry::document::canonical_bytes(&job.before_doc);
        if crate::private::sha256_hex(&canonical) != job.before_sha256 {
            return Err(verr("pre-run model document identity drifted"));
        }
        let arrays = job.before_doc.get("arrays").cloned().unwrap_or_else(|| json!({}));
        let Some(arrays) = arrays.as_object().cloned().or_else(|| arrays.is_null().then(Map::new)) else {
            return Err(verr("pre-run model array table is malformed"));
        };
        let mut sidecars: BTreeSet<String> = BTreeSet::new();
        for entry in arrays.values() {
            let Some(file) = entry.as_object().and_then(|o| o.get("file")) else { continue };
            let Some(name) = file.as_str().filter(|n| {
                Path::new(n).file_name().map(|f| f.to_string_lossy().into_owned()).as_deref() == Some(*n)
                    && !["", ".", "..", "model_before.json"].contains(n)
            }) else {
                return Err(verr("pre-run model sidecar name is unsafe"));
            };
            sidecars.insert(name.to_string());
        }
        let job_dir = PathBuf::from(job.job_dir.clone().unwrap_or_default());
        for name in &sidecars {
            let source = self.inner.models.dir().join(name);
            let destination = job_dir.join(name);
            let expected = artifact_fingerprint(&source, COPY_LIMIT)?;
            let actual = match artifact_fingerprint(&destination, COPY_LIMIT) {
                Ok(a) => a,
                Err(_) if !destination.exists() => {
                    let _ = copy_regular(&source, &destination, COPY_LIMIT, false);
                    artifact_fingerprint(&destination, COPY_LIMIT)?
                }
                Err(e) => return Err(e),
            };
            if actual != expected {
                return Err(verr(format!("pre-run model sidecar copy drifted for {name}")));
            }
        }
        implexity_geometry::document::build(&job.before_doc, Some(&job_dir), None)?;
        let before_path = job_dir.join("model_before.json");
        let raw = implexity_geometry::document::dumps(&job.before_doc);
        Self::managed_publish_bytes_once(&before_path, raw.as_bytes(), JSON_LIMIT)?;
        Ok(json!({
            "path": before_path.to_string_lossy(),
            "sha256": job.before_sha256,
            "sidecars": sidecars.into_iter().collect::<Vec<_>>(),
        }))
    }
}


pub(crate) fn numerical_attention_decision_name(token: &Value) -> JobResult<String> {
    let Some(t) = token.as_str().filter(|t| !t.is_empty()) else {
        return Err(verr("numerical attention event token is invalid"));
    };
    Ok(format!("numerical_attention_decision.{}.json", crate::private::sha256_hex(t.as_bytes())))
}

fn check_checkpoint_file(path: &Path, what: &str) -> JobResult<()> {
    let md = implexity_io::fsguard::stat_nofollow(path)?;
    if !md.is_owned_single_regular() || md.size < 1 || md.size > COPY_LIMIT {
        return Err(verr(format!("{what} is unsafe")));
    }
    let npz = implexity_io::npz::load_file(path).map_err(|e| verr(e.to_string()))?;
    if npz.files().is_empty() {
        return Err(verr(format!("{what} is empty")));
    }
    Ok(())
}
