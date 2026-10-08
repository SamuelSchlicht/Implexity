// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::{Path, PathBuf};

use implexity_core::contracts::reject_removed_constraint_keys;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError, cae};
use crate::py::{now, py_str, sha256_hex};
use crate::services::{Authoring, ServiceContext};
use crate::sync::ReentrantLock;

pub const SCHEMA: &str = "implexity-optimization-setup/1";
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

fn verr(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("ValueError", message)
}

fn depth_ok(v: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    match v {
        Value::Object(m) => m.values().all(|c| depth_ok(c, depth + 1)),
        Value::Array(a) => a.iter().all(|c| depth_ok(c, depth + 1)),
        _ => true,
    }
}


pub fn canonical_json(value: &Value) -> AResult<String> {
    if !depth_ok(value, 0) {
        return Err(verr("Run setup exceeds the supported nesting depth."));
    }
    let text = crate::py::canonical_ascii(value);
    if text.len() > MAX_BYTES {
        return Err(verr("Run setup exceeds the 16 MiB authoring limit."));
    }
    Ok(text)
}

fn integral_numbers(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), integral_numbers(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(integral_numbers).collect()),
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            if f.is_finite() && f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_992.0 {
                #[allow(clippy::cast_possible_truncation)]
                let i = f as i64;
                json!(i)
            } else {
                v.clone()
            }
        }
        other => other.clone(),
    }
}


pub fn request_digest(request: &Value) -> AResult<String> {
    canonical_json(request)?;
    let value: Map<String, Value> = request
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| k.as_str() != "seq" && k.as_str() != "applied_setup")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    Ok(sha256_hex(canonical_json(&integral_numbers(&Value::Object(value)))?.as_bytes()))
}

fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}


pub fn validate_payload(payload: &Value) -> AResult<()> {
    let keys_ok = payload.as_object().is_some_and(|m| {
        let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
        k.sort_unstable();
        k == ["expected_binding", "expected_revision", "label", "request"]
    });
    if !keys_ok {
        return Err(verr("Save run setup requires label, request, expected_revision and expected_binding."));
    }
    let label = &payload["label"];
    if !label.as_str().is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 160) {
        return Err(verr("Run setup label must contain 1 to 160 characters."));
    }
    let revision = &payload["expected_revision"];
    if !is_int(revision) || revision.as_i64().is_some_and(|r| r < 0) {
        return Err(verr("Expected run-setup revision must be a nonnegative integer."));
    }
    if !payload["expected_binding"].is_object() {
        return Err(verr("Expected run-setup binding must be an object."));
    }
    if !payload["request"].as_object().is_some_and(|m| !m.is_empty()) {
        return Err(verr("Run setup requires a nonempty optimization request."));
    }
    let request = &payload["request"];
    reject_removed_constraint_keys(request, "saved run setup").map_err(cae)?;
    if let Some(settings) = request.get("settings").filter(|s| s.is_object()) {
        reject_removed_constraint_keys(settings, "saved run setup settings").map_err(cae)?;
    }
    if let Some(rows) = request.get("responses").and_then(Value::as_array) {
        for row in rows {
            reject_removed_constraint_keys(row, "saved run setup response").map_err(cae)?;
        }
    }
    canonical_json(payload)?;
    Ok(())
}

fn opt_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    crate::py::py_eq(a.unwrap_or(&Value::Null), b.unwrap_or(&Value::Null))
}

fn read_json(path: &Path) -> AResult<Value> {
    let text =
        std::fs::read_to_string(path).map_err(|e| AuthoringError::Io(format!("{e}: {}", path.display())))?;
    implexity_core::json::parse_strict(&text).map_err(|e| verr(e.to_string()))
}


pub fn binding(ctx: &dyn ServiceContext, a: &Authoring) -> AResult<Value> {
    let model = a.models.require()?;
    let problem = if a.problems.path.exists() {
        let p = read_json(&a.problems.path)?;
        match p.as_object() {
            Some(m)
                if m.get("schema").and_then(Value::as_str)
                    == Some("implexity-provider-engineering-problem/1") =>
            {
                let mut out = Map::new();
                for k in ["schema", "provider", "problem"] {
                    out.insert(
                        k.into(),
                        m.get(k).cloned().ok_or_else(|| AuthoringError::Key(format!("'{k}'")))?,
                    );
                }
                Value::Object(out)
            }
            Some(m) => Value::Object(
                m.iter()
                    .filter(|(k, _)| {
                        !["updated", "model", "setup", "problem_id", "provenance"].contains(&k.as_str())
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            None => p,
        }
    } else {
        Value::Null
    };
    Ok(json!({
        "structure_id": model.structure_id(), "content_id": model.content_id(), "model_sha256": model.sha256()?,
        "problem_sha256": sha256_hex(canonical_json(&problem)?.as_bytes()),
        "physics_generation": ctx.package_generation()?,
    }))
}

#[derive(Debug)]
pub struct OptimizationSetupStore {
    pub path: PathBuf,
    lock: ReentrantLock,
}

impl OptimizationSetupStore {

    pub fn new(directory: &Path) -> AResult<Self> {
        crate::setup_transaction::recover(directory)?;
        Ok(Self { path: directory.join("optimization_setup.json"), lock: ReentrantLock::new() })
    }

    pub fn lock(&self) -> crate::sync::ReentrantGuard<'_> {
        self.lock.acquire()
    }

    fn dir(&self) -> PathBuf {
        self.path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }


    pub fn read(&self) -> AResult<Option<Value>> {
        crate::setup_transaction::recover(&self.dir())?;
        if !self.path.exists() {
            return Ok(None);
        }
        let result = read_json(&self.path)?;
        let ok = result.as_object().is_some_and(|m| {
            m.get("schema").and_then(Value::as_str) == Some(SCHEMA)
                && m.get("revision").is_some_and(|r| is_int(r) && r.as_i64().is_some_and(|v| v >= 1))
                && m.get("request").and_then(Value::as_object).is_some_and(|r| !r.is_empty())
                && m.get("binding").is_some_and(Value::is_object)
                && m.get("label").and_then(Value::as_str).is_some_and(|l| !l.trim().is_empty())
                && m.get("preflight_authorized") == Some(&Value::Bool(false))
        });
        if !ok {
            return Err(verr("Saved run setup is malformed. It was not replaced."));
        }
        canonical_json(&result)?;
        if let Some(applied) = result.get("applied") {
            let request: Map<String, Value> = result["request"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter(|(k, _)| k.as_str() != "seq" && k.as_str() != "applied_setup")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let ok = applied.as_object().is_some_and(|a| {
                let mut k: Vec<&str> = a.keys().map(String::as_str).collect();
                k.sort_unstable();
                k == ["problem_id", "request_sha256", "review_id", "schema"]
                    && a.get("schema").and_then(Value::as_str) == Some("implexity-guided-setup/1")
                    && ["review_id", "request_sha256", "problem_id"]
                        .iter()
                        .all(|k| a.get(*k).and_then(Value::as_str).is_some_and(|s| !s.is_empty()))
            });
            if !ok || py_str(&applied["request_sha256"]) != request_digest(&Value::Object(request))? {
                return Err(verr("Saved applied-setup identity is malformed. It was not reauthorized."));
            }
        }
        Ok(Some(result))
    }


    pub fn inspect(&self, current_binding: &Value) -> AResult<Value> {
        let _g = self.lock.acquire();
        let record = self.read()?;
        let changed: Vec<String> = match &record {
            None => Vec::new(),
            Some(r) => {
                let saved = r["binding"].as_object().cloned().unwrap_or_default();
                let cur = current_binding.as_object().cloned().unwrap_or_default();
                let mut keys: Vec<String> = saved.keys().chain(cur.keys()).cloned().collect();
                keys.sort();
                keys.dedup();
                keys.into_iter().filter(|k| !opt_eq(saved.get(k), cur.get(k))).collect()
            }
        };
        Ok(json!({"schema": SCHEMA, "record": record,
            "revision": record.as_ref().map_or(json!(0), |r| r["revision"].clone()),
            "current_binding": current_binding, "stale": !changed.is_empty(),
            "changed_binding_fields": changed, "preflight_authorized": false,
            "note": "Saved authoring declaration only. Fresh model-aware preflight is required."}))
    }


    pub fn save(&self, payload: &Value, current_binding: &Value) -> AResult<Value> {
        validate_payload(payload)?;
        let _g = self.lock.acquire();
        let old = self.read()?;
        let revision = old.as_ref().and_then(|r| r["revision"].as_i64()).unwrap_or(0);
        if payload["expected_revision"].as_i64() != Some(revision) {
            return Err(verr("Run setup changed in another client. Reload before saving."));
        }
        if !crate::py::py_eq(&payload["expected_binding"], current_binding) {
            return Err(verr("Model, physics or problem changed. Refresh and review before saving."));
        }
        let record = json!({"schema": SCHEMA, "revision": revision + 1,
            "label": payload["label"].as_str().unwrap_or_default().trim(), "saved_at": now(),
            "binding": current_binding, "request": payload["request"], "preflight_authorized": false});
        let mut text = canonical_json(&record)?;
        text.push('\n');
        write_exclusive_then_replace(&self.path, text.as_bytes())?;
        self.inspect(current_binding)
    }
}


pub fn write_exclusive_then_replace(path: &Path, bytes: &[u8]) -> AResult<()> {
    use std::io::Write;
    let io = |e: std::io::Error| AuthoringError::Io(format!("{e}: {}", path.display()));
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(io)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.tmp", crate::py::uuid_hex()));
    let result = (|| -> AResult<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(io)?;
        f.write_all(bytes).map_err(io)?;
        f.flush().map_err(io)?;
        f.sync_all().map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}


pub fn inspect(ctx: &dyn ServiceContext) -> AResult<Value> {
    let a = ctx.authoring()?;
    let _p = a.problems.lock();
    let _s = a.setup.lock();
    a.setup.inspect(&binding(ctx, &a)?)
}


pub fn save(ctx: &dyn ServiceContext, payload: &Value) -> AResult<Value> {
    let a = ctx.authoring()?;
    a.manipulation.run_reserved_idle("save run setup", None, || {
        let _p = a.problems.lock();
        let _s = a.setup.lock();
        a.setup.save(payload, &binding(ctx, &a)?)
    })
}


pub fn validate_revision_payload(payload: &Value) -> AResult<()> {
    let keys_ok = payload.as_object().is_some_and(|m| {
        let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
        k.sort_unstable();
        k == ["expected_model", "expected_problem_id", "problem", "provider"]
    });
    if !keys_ok {
        return Err(verr(
            "Problem revision requires provider, problem, expected_problem_id and expected_model.",
        ));
    }
    for key in ["provider", "expected_problem_id"] {
        let ok =
            payload[key].as_str().is_some_and(|s| !s.is_empty() && s == s.trim() && s.chars().count() <= 160);
        if !ok {
            return Err(verr(format!("{key} requires canonical nonempty text.")));
        }
    }
    if !payload["problem"].is_object() {
        return Err(verr("Revised problem must be an object."));
    }
    let ok = payload["expected_model"].as_object().is_some_and(|m| {
        let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
        k.sort_unstable();
        k == ["content_id", "structure_id"] && m.values().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
    });
    if !ok {
        return Err(verr("Problem revision requires both expected model identities."));
    }
    canonical_json(payload)?;
    Ok(())
}


pub fn problem_document(
    provider: &dyn implexity_core::contracts::CaeProvider,
    normalised: &implexity_core::contracts::ProviderProblem,
) -> AResult<Value> {
    match implexity_optim::provider_ops::design_operations(provider)
        .and_then(|o| o.problem_document(normalised))
    {
        Some(d) => d.map_err(cae),
        None => normalised.downcast_ref::<Value>().cloned().ok_or_else(|| {
            verr(format!("provider {} normalise_problem must return an object", provider.name()))
        }),
    }
}


pub fn revise_problem(ctx: &dyn ServiceContext, payload: &Value) -> AResult<Value> {
    validate_revision_payload(payload)?;
    let a = ctx.authoring()?;
    a.manipulation.run_reserved_idle("revise engineering problem", None, || {
        let model = a.models.require()?;
        let identity = json!({"structure_id": model.structure_id(), "content_id": model.content_id(), "sha256": model.sha256()?});
        if ["structure_id", "content_id"].iter().any(|k| payload["expected_model"][*k] != identity[*k]) {
            return Err(verr("Geometry changed while the physics editor was open. Reload before applying."));
        }
        let _g = a.problems.lock();
        let previous = read_json(&a.problems.path)?;
        let schema = implexity_runtime::provider_problem_document::SCHEMA;
        if previous.get("schema").and_then(Value::as_str) != Some(schema) || previous.get("provider") != Some(&payload["provider"]) {
            return Err(verr("The stored physics provider changed. Reload the problem."));
        }
        if previous.get("problem_id") != Some(&payload["expected_problem_id"]) {
            return Err(verr("The physics problem changed in another client. Reload before applying."));
        }
        let registries = implexity_core::registries::global();
        let token = registries.providers.binding_token();
        let provider = registries.providers.get(&py_str(&payload["provider"])).map_err(cae)?;
        let old = previous.get("problem").cloned().unwrap_or(Value::Null);
        provider.normalise_problem(&old).map_err(cae)?;
        let revised_in = payload["problem"].clone();
        let replan = implexity_optim::provider_ops::design_operations(provider.as_ref()).and_then(|o| o.replan_problem_revision(&old, &revised_in));
        let owned = replan.is_some();
        let revised = if let Some(r) = replan { r.map_err(cae)? } else {
            let n = provider.normalise_problem(&revised_in).map_err(cae)?;
            problem_document(provider.as_ref(), &n)?
        };
        if token != registries.providers.binding_token() {
            return Err(verr("Physics packages changed during revision. Reload before applying."));
        }
        let call = crate::problem::ProblemCall {
            model_identity: Some(identity),
            case_doc: ctx.current_case(),
            physics_backend: ctx.physics_backend(),
            interface: Some("public_agent_action".into()),
            actor: Some("engineering_agent".into()),
        };
        a.problems.put(
            &json!({"schema": schema, "provider": payload["provider"], "problem": revised,
                "expected_model": payload["expected_model"],
                "provenance": {"operation": "explicit_problem_revision", "previous_problem_id": previous["problem_id"],
                    "provider_owned_replan": owned}}),
            &call,
        )
    })
}
