// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value, json};

use implexity_core::backends;
use implexity_runtime::provider_problem_document as ppd;

use crate::entities::{PROBLEM_SCHEMA as TYPED_SCHEMA, ProblemAdapter, adapter_of, contributions};
use crate::error::{AResult, AuthoringError};
use crate::py::{canonical_ascii, jf, now, py_str, repr, sha256_hex, truthy};
use crate::sync::lock;

pub const SCHEMA: &str = "implexity-differentiable-problem/1";

#[must_use]
pub fn problem_error(problems: Vec<String>) -> AuthoringError {
    AuthoringError::problems("ProblemError", problems)
}

#[must_use]
pub fn hash(x: &Value) -> String {
    sha256_hex(canonical_ascii(x).as_bytes())
}

fn adapter(physics_backend: Option<&str>) -> Option<std::sync::Arc<dyn ProblemAdapter>> {
    let name = physics_backend?;
    let backend = backends::selected_physics(contributions(), Some(name))?;
    adapter_of(backend.as_ref())
}

fn capabilities_v13(physics_backend: Option<&str>) -> Value {
    if let Some(a) = adapter(physics_backend) {
        return a.case_bound_capabilities();
    }
    json!({"backend": physics_backend,
        "setup_mode": if physics_backend.is_some() { "unknown" } else { "unset" },
        "analyses": [], "result_fields": false, "responses": false,
        "reverse_mode": false, "jvp": false, "vjp": false})
}

fn empty_decl(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

fn normalise_v13(
    body: &Value,
    model_identity: &Value,
    case_doc: &Value,
    physics_backend: Option<&str>,
) -> AResult<Value> {
    let body = if body.is_null() { json!({}) } else { body.clone() };
    let Some(b) = body.as_object() else {
        return Err(problem_error(vec!["the problem document must be a JSON object".into()]));
    };
    let adapter = adapter(physics_backend);
    let declared =
        b.iter().any(|(k, v)| !["schema", "name", "doc", "backend"].contains(&k.as_str()) && !empty_decl(v));
    let s = |k: &str, d: &str| b.get(k).filter(|v| truthy(v)).map_or_else(|| d.to_string(), py_str);
    let Some(adapter) = adapter else {
        if declared || b.get("backend").is_some_and(truthy) {
            return Err(problem_error(vec![
                "no physics backend with a case-bound problem adapter is active; load a physics package before declaring an engineering problem".into(),
            ]));
        }
        return Ok(json!({
            "schema": SCHEMA, "name": s("name", "unset engineering problem"),
            "backend": null, "model": model_identity,
            "setup": {"mode": "unset", "case_schema": null, "case_name": null, "case_sha256": null,
                      "materials": null, "regions": null, "loads": null, "boundary_conditions": null},
            "analysis": null, "responses": [], "result_fields": [],
            "doc": s("doc", ""),
        }));
    };
    let pb = physics_backend.unwrap_or_default();
    let mut probs = Vec::new();
    let backend = b.get("backend").filter(|v| truthy(v)).map_or_else(|| pb.to_string(), py_str);
    if backend != pb {
        probs.push(format!(
            "problem backend {} does not match the service physics backend {}",
            repr(&Value::from(backend.clone())),
            repr(&Value::from(pb))
        ));
    }
    let analyses = adapter.analysis_types();
    let first = analyses.first().cloned().unwrap_or_default();
    let mut analysis =
        b.get("analysis").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({"type": first}));
    if let Value::String(t) = &analysis {
        analysis = json!({"type": t});
    }
    if !analysis.is_object() {
        probs.push("analysis must be an object or analysis type string".into());
        analysis = json!({"type": first});
    }
    let atype = analysis.get("type").cloned().unwrap_or(Value::Null);
    if !atype.as_str().is_some_and(|t| analyses.iter().any(|a| a == t)) {
        probs.push(format!(
            "backend {} implements analysis.type in {}, got {}",
            repr(&Value::from(pb)),
            implexity_core::pyobj::list_repr(&analyses),
            repr(&atype)
        ));
    }
    for key in ["materials", "regions", "loads", "boundary_conditions"] {
        if let Some(v) = b.get(key)
            && !empty_decl(v)
            && v.as_str() != Some("case")
        {
            probs.push(format!(
                    "{key} overrides are not implemented by backend {}'s case-bound problem; use source='case' or edit the validated case document",
                    repr(&Value::from(pb))
                ));
        }
    }
    if case_doc.is_null() {
        probs
            .push(format!("backend {} is case-bound and no case document is stored", repr(&Value::from(pb))));
    }
    if !probs.is_empty() {
        return Err(problem_error(probs));
    }
    let case_sha = hash(case_doc);
    Ok(json!({
        "schema": SCHEMA,
        "name": s("name", "implicit coupled engineering problem"),
        "backend": backend,
        "model": model_identity,
        "setup": {"mode": "case_bound", "case_schema": case_doc.get("schema").cloned().unwrap_or(Value::Null),
                  "case_name": case_doc.get("name").cloned().unwrap_or(Value::Null), "case_sha256": case_sha,
                  "materials": "case", "regions": "case+implicit-domain", "loads": "case", "boundary_conditions": "case"},
        "analysis": {"type": atype},
        "responses": b.get("responses").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!([])),
        "result_fields": b.get("result_fields").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!([])),
        "doc": s("doc", ""),
    }))
}

#[must_use]
pub fn capabilities(physics_backend: Option<&str>) -> Value {
    let mut out = capabilities_v13(physics_backend).as_object().cloned().unwrap_or_default();
    if let Some(m) = crate::entities::capabilities(physics_backend).as_object() {
        for (k, v) in m {
            out.insert(k.clone(), v.clone());
        }
    }
    let a = physics_backend.filter(|b| !b.is_empty()).and_then(|b| adapter(Some(b)));
    out.insert("adapter".into(), a.map_or(Value::Null, |a| a.capabilities()));
    Value::Object(out)
}

#[derive(Clone, Debug, Default)]
pub struct ProblemCall {
    pub model_identity: Option<Value>,
    pub case_doc: Option<Value>,
    pub physics_backend: Option<String>,
    pub interface: Option<String>,
    pub actor: Option<String>,
}

impl ProblemCall {
    fn legacy(&self) -> bool {
        self.model_identity.is_some() || self.case_doc.is_some()
    }
}

fn model_key(model: &Value) -> String {
    for key in ["content_id", "model_id", "structure_id"] {
        if let Some(v) = model.get(key).filter(|v| truthy(v)) {
            return py_str(v);
        }
    }
    "__default__".into()
}

fn active() -> MutexGuard<'static, HashMap<String, Value>> {
    static ACTIVE: std::sync::OnceLock<Mutex<HashMap<String, Value>>> = std::sync::OnceLock::new();
    lock(ACTIVE.get_or_init(|| Mutex::new(HashMap::new())))
}

fn compile_typed(normal: &Value, base_case: &Value) -> AResult<Value> {
    let backend = normal.get("backend").and_then(Value::as_str);
    let (_, adapter) = crate::entities::problem_adapter(backend)?;
    adapter.compile_problem(normal, base_case)
}


pub fn normalise(value: &Value, call: &ProblemCall) -> AResult<Value> {
    let schema = value.as_object().and_then(|m| m.get("schema"));
    let is_v1 = schema.and_then(Value::as_str) == Some(SCHEMA);
    if is_v1 || (schema.is_none() && call.legacy()) {
        let mut canonical_value = value.clone();
        if let (Some(s), Some(m)) = (schema.cloned(), canonical_value.as_object_mut()) {
            m.insert("schema".into(), s);
        }
        let empty = json!({});
        let model_identity = call.model_identity.as_ref().filter(|v| truthy(v)).unwrap_or(&empty);
        let case_doc = call.case_doc.as_ref().filter(|v| truthy(v)).unwrap_or(&empty);
        return normalise_v13(&canonical_value, model_identity, case_doc, call.physics_backend.as_deref());
    }
    let mut normal =
        crate::entities::normalise_problem(value, call.model_identity.as_ref(), call.case_doc.as_ref())?;
    let base_case = normal.get("base_case").filter(|v| truthy(v)).cloned().or_else(|| call.case_doc.clone());
    if let Some(base) = base_case.filter(|v| !v.is_null()) {
        let compiled = compile_typed(&normal, &base)?;
        if let Some(m) = normal.as_object_mut() {
            m.insert("compiled_case".into(), compiled.get("case").cloned().unwrap_or(Value::Null));
            m.insert("compiled_case_id".into(), compiled.get("case_id").cloned().unwrap_or(Value::Null));
            m.insert("entity_counts".into(), compiled.get("entity_counts").cloned().unwrap_or(Value::Null));
        }
    }
    Ok(normal)
}


pub fn register_active_problem(value: &Value, model: Option<&Value>, case: Option<&Value>) -> AResult<Value> {
    let call =
        ProblemCall { model_identity: model.cloned(), case_doc: case.cloned(), ..ProblemCall::default() };
    let mut typed = value.clone();
    if typed.get("schema").is_none()
        && let Some(m) = typed.as_object_mut()
    {
        m.insert("schema".into(), Value::from(TYPED_SCHEMA));
    }
    let normal = normalise_typed(&typed, &call)?;
    let key = model_key(model.filter(|m| truthy(m)).unwrap_or(&normal["model"]));
    let mut a = active();
    a.insert(key, normal.clone());
    a.insert("__default__".into(), normal.clone());
    Ok(normal)
}

fn normalise_typed(value: &Value, call: &ProblemCall) -> AResult<Value> {
    let mut normal =
        crate::entities::normalise_problem(value, call.model_identity.as_ref(), call.case_doc.as_ref())?;
    let base_case = normal.get("base_case").filter(|v| truthy(v)).cloned().or_else(|| call.case_doc.clone());
    if let Some(base) = base_case.filter(|v| !v.is_null()) {
        let compiled = compile_typed(&normal, &base)?;
        if let Some(m) = normal.as_object_mut() {
            m.insert("compiled_case".into(), compiled.get("case").cloned().unwrap_or(Value::Null));
            m.insert("compiled_case_id".into(), compiled.get("case_id").cloned().unwrap_or(Value::Null));
            m.insert("entity_counts".into(), compiled.get("entity_counts").cloned().unwrap_or(Value::Null));
        }
    }
    Ok(normal)
}


pub fn effective_case_for(model: Option<&Value>, base_case: &Value) -> AResult<Value> {
    let current_key = model.map_or_else(|| "__default__".to_string(), model_key);
    let a = active();
    let mut chosen = a.get(&current_key).cloned();
    if chosen.is_none()
        && let Some(candidate) = a.get("__default__").cloned()
    {
        let declared = model_key(&candidate["model"]);
        if current_key != "__default__" && declared != "__default__" && declared != current_key {
            return Err(crate::entities::err(format!(
                "the active differentiable engineering problem belongs to model {}, not the requested model {}",
                repr(&Value::from(declared)),
                repr(&Value::from(current_key))
            )));
        }
        chosen = Some(candidate);
    }
    let Some(act) = chosen.filter(|v| v.get("schema").and_then(Value::as_str) == Some(TYPED_SCHEMA)) else {
        return Ok(base_case.clone());
    };
    drop(a);
    let compiled = compile_typed(&act, base_case)?;
    let mut updated = act;
    if let Some(m) = updated.as_object_mut() {
        m.insert("compiled_case".into(), compiled.get("case").cloned().unwrap_or(Value::Null));
        m.insert("compiled_case_id".into(), compiled.get("case_id").cloned().unwrap_or(Value::Null));
        m.insert("entity_counts".into(), compiled.get("entity_counts").cloned().unwrap_or(Value::Null));
    }
    let mut a = active();
    for v in a.values_mut() {
        if v.get("problem_id") == updated.get("problem_id") {
            v.clone_from(&updated);
        }
    }
    Ok(compiled.get("case").cloned().unwrap_or(Value::Null))
}

fn typed_public_declaration(value: &Value) -> Value {
    let mut out = value.as_object().cloned().unwrap_or_default();
    for key in [
        "problem_id",
        "compiled_case",
        "compiled_case_id",
        "entity_counts",
        "model",
        "base_case",
        "capabilities",
        "updated",
    ] {
        out.shift_remove(key);
    }
    out.insert("schema".into(), Value::from(TYPED_SCHEMA));
    Value::Object(out)
}

fn write_json(path: &Path, value: &Value) -> AResult<()> {
    let text =
        implexity_core::json::dumps(value, &implexity_core::json::DumpOptions::indented(2).sorted(true));
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    ));
    std::fs::write(&tmp, text).map_err(|e| AuthoringError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| AuthoringError::Io(e.to_string()))
}

#[derive(Debug)]
pub struct ProblemStore {
    pub dir: PathBuf,
    pub path: PathBuf,
    lock: crate::sync::ReentrantLock,
    typed: Mutex<Option<Value>>,
}

impl ProblemStore {

    pub fn new(directory: &Path) -> AResult<Self> {
        crate::setup_transaction::recover(directory)?;
        let dir = directory.join("engineering");
        std::fs::create_dir_all(&dir).map_err(|e| AuthoringError::Io(e.to_string()))?;
        Ok(Self {
            path: dir.join("problem.json"),
            dir,
            lock: crate::sync::ReentrantLock::new(),
            typed: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn lock(&self) -> crate::sync::ReentrantGuard<'_> {
        self.lock.acquire()
    }

    fn sidecar(&self) -> PathBuf {
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.path.with_file_name(format!("{name}.engineering-v2.json"))
    }

    fn model_dir(&self) -> PathBuf {
        self.dir.parent().map_or_else(|| self.dir.clone(), Path::to_path_buf)
    }

    fn clear_typed(&self) {
        let cached = lock(&self.typed).take();
        {
            let mut a = active();
            if let Some(c) = &cached {
                a.remove(&model_key(&c["model"]));
            }
            a.remove("__default__");
        }
        let _ = std::fs::remove_file(self.sidecar());
    }


    pub fn put(&self, value: &Value, call: &ProblemCall) -> AResult<Value> {
        let _g = self.lock.acquire();
        crate::setup_transaction::recover(&self.model_dir())?;
        if ppd::is_provider_problem_envelope(value) {
            let empty = json!({});
            let model = call.model_identity.as_ref().unwrap_or(&empty);
            let normal = ppd::normalise_provider_problem_envelope(
                value,
                model,
                call.interface.as_deref().unwrap_or("public_api"),
                call.actor.as_deref().unwrap_or("client"),
            )
            .map_err(|e| problem_error(vec![e.message().to_string()]))?;
            write_json(&self.path, &normal)?;
            self.clear_typed();
            return Ok(normal);
        }
        if value.get("schema").and_then(Value::as_str) == Some(TYPED_SCHEMA) && value.is_object() {
            let normal =
                register_active_problem(value, call.model_identity.as_ref(), call.case_doc.as_ref())?;
            *lock(&self.typed) = Some(normal.clone());
            let persisted: Map<String, Value> = normal
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter(|(k, _)| {
                            !["compiled_case", "compiled_case_id", "entity_counts"].contains(&k.as_str())
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let sidecar = self.sidecar();
            if let Some(p) = sidecar.parent() {
                std::fs::create_dir_all(p).map_err(|e| AuthoringError::Io(e.to_string()))?;
            }
            write_json(&sidecar, &Value::Object(persisted))?;
            return Ok(normal);
        }
        let mut rec = normalise(value, call)?;
        let id = hash(&rec)[..16].to_string();
        if let Some(m) = rec.as_object_mut() {
            m.insert("problem_id".into(), Value::from(id));
            m.insert("updated".into(), jf(now()));
        }
        write_json(&self.path, &rec)?;
        self.clear_typed();
        Ok(rec)
    }


    pub fn get(&self, call: &ProblemCall) -> AResult<Value> {
        let _g = self.lock.acquire();
        crate::setup_transaction::recover(&self.model_dir())?;
        let mut cached = lock(&self.typed).clone();
        let sidecar = self.sidecar();
        if cached.is_none() && sidecar.is_file() {
            cached =
                std::fs::read_to_string(&sidecar).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        }
        if let Some(c) = cached {
            let model = call.model_identity.clone().filter(truthy).or_else(|| c.get("model").cloned());
            let case = call.case_doc.clone().filter(truthy).or_else(|| c.get("base_case").cloned());
            let normal =
                register_active_problem(&typed_public_declaration(&c), model.as_ref(), case.as_ref())?;
            *lock(&self.typed) = Some(normal.clone());
            return Ok(normal);
        }
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                let stored: Value = implexity_core::json::parse_strict(&text)
                    .map_err(|e| AuthoringError::value("JSONDecodeError", e.message.clone()))?;
                if stored.get("schema").and_then(Value::as_str) == Some(ppd::SCHEMA) {
                    let declaration = ppd::declaration_for_rebind(&stored)
                        .map_err(|e| problem_error(vec![e.message().to_string()]))?;
                    let mut rebind = call.clone();
                    rebind.interface = Some("state_reload".into());
                    rebind.actor = Some("service".into());
                    return self.put(&declaration, &rebind);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AuthoringError::Io(e.to_string())),
        }

        if self.path.is_file() {
            let loaded =
                std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
            if let Some(Value::Object(mut body)) = loaded {
                for k in ["model", "setup", "problem_id", "updated"] {
                    body.shift_remove(k);
                }
                match self.put(&Value::Object(body), call) {
                    Ok(r) => return Ok(r),
                    Err(e) if e.class() == "ProblemError" => return Err(e),
                    Err(_) => {}
                }
            }
        }
        self.put(&json!({}), call)
    }


    pub fn effective_case(&self, model: Option<&Value>, case: &Value) -> AResult<Value> {
        if let Some(c) = lock(&self.typed).clone() {
            let key = model_key(model.filter(|m| truthy(m)).unwrap_or(&c["model"]));
            active().insert(key, c);
        }
        effective_case_for(model, case)
    }
}
