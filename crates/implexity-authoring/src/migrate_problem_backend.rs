// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::Path;

use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::problem::{ProblemCall, ProblemStore, hash, normalise};
use crate::py::{py_eq, sha256_hex, truthy};

fn verr(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("ValueError", message)
}


pub fn migrate(
    directory: &Path,
    case_doc: &Value,
    expected_backend: &str,
    new_backend: &str,
) -> AResult<Value> {
    let directory = std::fs::canonicalize(directory).map_err(|e| AuthoringError::Io(e.to_string()))?;
    let path = directory.join("engineering").join("problem.json");
    if !path.is_file() {
        return Err(verr("No stored case-bound problem exists at the supplied state directory"));
    }
    if directory.join("engineering").join("problem.json.engineering-v2.json").exists() {
        return Err(verr(
            "A typed problem sidecar exists; this case-bound identity migration must not replace it",
        ));
    }
    let original = std::fs::read(&path).map_err(|e| AuthoringError::Io(e.to_string()))?;
    let stored = implexity_core::json::parse_strict(&String::from_utf8_lossy(&original))
        .map_err(|e| verr(e.to_string()))?;
    if stored.get("schema").and_then(Value::as_str) != Some("implexity-differentiable-problem/1") {
        return Err(verr("Only case-bound version-1 problems are supported"));
    }
    if expected_backend.is_empty() || new_backend.is_empty() || expected_backend == new_backend {
        return Err(verr("Supply distinct expected and replacement backend identifiers"));
    }
    if stored.get("backend").and_then(Value::as_str) != Some(expected_backend) {
        return Err(verr("Stored backend differs from the explicitly expected identifier"));
    }
    if stored.pointer("/setup/case_sha256").and_then(Value::as_str) != Some(hash(case_doc).as_str()) {
        return Err(verr("The supplied case does not match the original case identity"));
    }
    let identity = stored.get("model").cloned().unwrap_or(Value::Null);
    if !identity.is_object() || !truthy(&identity) {
        return Err(verr("The stored model identity is missing"));
    }
    let call = |backend: &str| ProblemCall {
        model_identity: Some(identity.clone()),
        case_doc: Some(case_doc.clone()),
        physics_backend: Some(backend.to_string()),
        interface: None,
        actor: None,
    };
    let before = normalise(&stored, &call(expected_backend))?;
    let mut candidate = stored.clone();
    if let Some(c) = candidate.as_object_mut() {
        c.insert("backend".into(), json!(new_backend));
    }
    let after = normalise(&candidate, &call(new_backend))?;
    let mut expected = before.clone();
    if let Some(e) = expected.as_object_mut() {
        e.insert("backend".into(), json!(new_backend));
    }
    if !py_eq(&after, &expected) {
        return Err(verr("Validation changed more than the backend identity"));
    }
    let generated = ["problem_id", "updated"];
    let stored_map = stored.as_object().cloned().unwrap_or_default();
    let kept: Map<String, Value> = stored_map
        .iter()
        .filter(|(k, _)| !generated.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !py_eq(&Value::Object(kept), &before) {
        return Err(verr(
            "Stored content requires changes beyond its backend identity; refusing automatic migration",
        ));
    }
    if stored_map.keys().any(|k| before.get(k).is_none() && !generated.contains(&k.as_str())) {
        return Err(verr("Unrecognised stored fields require manual migration; refusing to discard them"));
    }
    let backup =
        path.with_file_name(format!("problem.json.pre-backend-migration-{}.json", crate::py::uuid_hex()));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
            .map_err(|e| AuthoringError::Io(e.to_string()))?;
        f.write_all(&original).map_err(|e| AuthoringError::Io(e.to_string()))?;
    }
    if std::fs::read(&path).map_err(|e| AuthoringError::Io(e.to_string()))? != original {
        return Err(verr("Problem changed during migration; stop the service and retry"));
    }
    let result = ProblemStore::new(&directory)?.put(&after, &call(new_backend))?;
    Ok(json!({"backup": backup.display().to_string(), "backup_sha256": sha256_hex(&original),
        "problem_id": result["problem_id"], "old_backend": expected_backend, "new_backend": new_backend}))
}


pub fn main(args: &[String]) -> AResult<String> {
    let mut state_dir = None;
    let mut case = None;
    let mut expected = None;
    let mut new = None;
    let mut confirmed = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--state-dir" => state_dir = it.next().cloned(),
            "--case" => case = it.next().cloned(),
            "--expected-backend" => expected = it.next().cloned(),
            "--new-backend" => new = it.next().cloned(),
            "--confirm-service-stopped" => confirmed = true,
            other => return Err(verr(format!("unrecognized arguments: {other}"))),
        }
    }
    let (Some(state_dir), Some(case), Some(expected), Some(new)) = (state_dir, case, expected, new) else {
        return Err(verr(
            "the following arguments are required: --state-dir, --case, --expected-backend, --new-backend",
        ));
    };
    if !confirmed {
        return Err(verr("the following arguments are required: --confirm-service-stopped"));
    }
    let text = std::fs::read_to_string(&case).map_err(|e| AuthoringError::Io(e.to_string()))?;
    let mut doc = implexity_core::json::parse_strict(&text).map_err(|e| verr(e.to_string()))?;
    if doc.get("case").is_some_and(Value::is_object) {
        doc = doc["case"].clone();
    }
    let report = migrate(Path::new(&state_dir), &doc, &expected, &new)?;
    Ok(implexity_core::json::dumps(&report, &implexity_core::json::DumpOptions::indented(2)))
}
