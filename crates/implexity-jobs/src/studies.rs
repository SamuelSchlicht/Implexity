// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::private::{canonical_text, epoch_seconds, indented_sorted_text, sha256_hex};

pub const SCHEMA: &str = "implexity-study/1";
pub const STORE_SCHEMA: &str = "implexity-study-store/1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("study rejected:\n  {}", problems.join("\n  "))]
pub struct StudyError {
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StudyStoreError {
    #[error(transparent)]
    Study(#[from] StudyError),
    #[error("{0}")]
    Missing(String),
    #[error("{0}")]
    Io(String),
}

fn reject<T>(problem: impl Into<String>) -> Result<T, StudyError> {
    Err(StudyError { problems: vec![problem.into()] })
}

fn id_of(study: &Value) -> String {
    sha256_hex(canonical_text(study).as_bytes())[..16].to_string()
}

fn name_ok(value: &str) -> bool {
    let mut chars = value.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first_ok
        && value.chars().count() <= 64
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

fn py_str_or(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(v) if implexity_core::pyobj::truthy(v) => implexity_core::pyobj::py_str(v),
        _ => default.to_string(),
    }
}

fn variant(v: &Value, i: usize) -> Result<Value, StudyError> {
    let Some(m) = v.as_object() else {
        return reject(format!("variants[{i}] must be an object"));
    };
    let name = py_str_or(m.get("name"), &format!("variant_{}", i + 1));
    let Some(request) = m.get("request").filter(|r| r.is_object()) else {
        return reject(format!("variants[{i}].request must be an optimisation request object"));
    };
    let mut out = Map::new();
    out.insert("name".into(), Value::String(name));
    out.insert("request".into(), request.clone());
    if let Some(doc) = m.get("doc") {
        out.insert("doc".into(), Value::String(implexity_core::pyobj::py_str(doc)));
    }
    Ok(Value::Object(out))
}


pub fn normalise(body: &Value, model_identity: Option<&Value>) -> Result<Map<String, Value>, StudyError> {
    let Some(b) = body.as_object() else {
        return reject("the study must be a JSON object");
    };
    let name = py_str_or(b.get("name"), "unnamed study");
    let mut variants = b.get("variants").filter(|v| !v.is_null()).cloned();
    if variants.is_none()
        && let Some(request) = b.get("request").filter(|r| r.is_object())
    {
        variants = Some(serde_json::json!([{"name": "baseline", "request": request}]));
    }
    let Some(Value::Array(variants)) = variants.filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    else {
        return reject("a study needs variants=[{name, request}, ...] or one request object");
    };
    let vv: Vec<Value> = variants.iter().enumerate().map(|(i, v)| variant(v, i)).collect::<Result<_, _>>()?;
    let names: Vec<&str> = vv.iter().filter_map(|v| v.get("name").and_then(Value::as_str)).collect();
    let unique: std::collections::BTreeSet<&&str> = names.iter().collect();
    if unique.len() != names.len() {
        return reject("variant names must be unique within a study");
    }
    let tags = match b.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) if !implexity_core::pyobj::truthy(v) => Vec::new(),
        Some(Value::Array(t)) if t.iter().all(Value::is_string) => t.clone(),
        Some(_) => return reject("tags must be a list of strings"),
    };
    let mut core = Map::new();
    core.insert("schema".into(), Value::String(SCHEMA.into()));
    core.insert("name".into(), Value::String(name));
    core.insert("doc".into(), Value::String(py_str_or(b.get("doc"), "")));
    core.insert("tags".into(), Value::Array(tags));
    core.insert("variants".into(), Value::Array(vv));
    if let Some(identity) = model_identity.filter(|v| implexity_core::pyobj::truthy(v)) {
        core.insert("model".into(), identity.clone());
    }
    Ok(core)
}

#[derive(Debug)]
pub struct StudyStore {
    pub dir: PathBuf,
    pub path: PathBuf,
    data: Mutex<Map<String, Value>>,
}

impl StudyStore {

    pub fn new(directory: &std::path::Path) -> Result<Self, StudyStoreError> {
        let dir = directory.join("studies");
        let path = dir.join("studies.json");
        std::fs::create_dir_all(&dir).map_err(|e| StudyStoreError::Io(e.to_string()))?;
        let mut data = Map::new();
        data.insert("schema".into(), Value::String(STORE_SCHEMA.into()));
        data.insert("studies".into(), Value::Object(Map::new()));
        if path.is_file()
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(Value::Object(got)) = serde_json::from_str::<Value>(&text)
            && got.get("schema").and_then(Value::as_str) == Some(STORE_SCHEMA)
            && got.get("studies").is_some_and(Value::is_object)
        {
            data = got;
        }
        Ok(Self { dir, path, data: Mutex::new(data) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Map<String, Value>> {
        self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn save(&self, data: &Map<String, Value>) -> Result<(), StudyStoreError> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, indented_sorted_text(&Value::Object(data.clone())))
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| StudyStoreError::Io(e.to_string()))
    }

    fn studies(data: &mut Map<String, Value>) -> Result<&mut Map<String, Value>, StudyStoreError> {
        data.get_mut("studies")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| StudyStoreError::Io("study store is corrupt".into()))
    }


    pub fn create(&self, body: &Value, model_identity: Option<&Value>) -> Result<Value, StudyStoreError> {
        let core = normalise(body, model_identity)?;
        let requested = py_str_or(body.get("id"), "");
        let ident = if requested.is_empty() {
            format!("study_{}", &id_of(&Value::Object(core.clone()))[..10])
        } else {
            if !name_ok(&requested) {
                return Err(StudyError {
                    problems: vec![format!(
                        "study id {} is invalid",
                        implexity_core::py_repr::repr_str(&requested)
                    )],
                }
                .into());
            }
            requested
        };
        let mut data = self.lock();
        if Self::studies(&mut data)?.contains_key(&ident) {
            return Err(StudyError {
                problems: vec![format!("study {} already exists", implexity_core::py_repr::repr_str(&ident))],
            }
            .into());
        }
        let mut rec = core;
        rec.insert("id".into(), Value::String(ident.clone()));
        rec.insert("created".into(), Value::from(epoch_seconds()));
        rec.insert("updated".into(), Value::from(epoch_seconds()));
        rec.insert("runs".into(), Value::Array(Vec::new()));
        Self::studies(&mut data)?.insert(ident, Value::Object(rec.clone()));
        self.save(&data)?;
        Ok(Value::Object(rec))
    }

    #[must_use]
    pub fn list(&self) -> Vec<Value> {
        let mut data = self.lock();
        let mut rows: Vec<Value> =
            Self::studies(&mut data).map(|s| s.values().cloned().collect()).unwrap_or_default();
        rows.sort_by(|a, b| {
            let key = |v: &Value| v.get("updated").and_then(Value::as_f64).unwrap_or(0.0);
            key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal)
        });
        rows
    }


    pub fn get(&self, ident: &str) -> Result<Value, StudyStoreError> {
        let mut data = self.lock();
        Self::studies(&mut data)?
            .get(ident)
            .cloned()
            .ok_or_else(|| StudyStoreError::Missing(implexity_core::py_repr::repr_str(ident)))
    }


    pub fn delete(&self, ident: &str) -> Result<Value, StudyStoreError> {
        let mut data = self.lock();
        let rec = Self::studies(&mut data)?
            .shift_remove(ident)
            .ok_or_else(|| StudyStoreError::Missing(implexity_core::py_repr::repr_str(ident)))?;
        self.save(&data)?;
        Ok(rec)
    }


    pub fn variant_request(&self, ident: &str, variant: &str) -> Result<Value, StudyStoreError> {
        let rec = self.get(ident)?;
        let variants = rec.get("variants").and_then(Value::as_array).cloned().unwrap_or_default();
        for v in &variants {
            if v.get("name").and_then(Value::as_str) == Some(variant) {
                return Ok(v.get("request").cloned().unwrap_or(Value::Null));
            }
        }
        let names: Vec<String> = variants
            .iter()
            .filter_map(|v| v.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        Err(StudyError {
            problems: vec![format!(
                "study {} has no variant {}; it has {}",
                implexity_core::py_repr::repr_str(ident),
                implexity_core::py_repr::repr_str(variant),
                implexity_core::pyobj::list_repr(&names)
            )],
        }
        .into())
    }


    pub fn attach_run(
        &self,
        ident: &str,
        variant: &str,
        job_id: &str,
        solve_id: Option<&str>,
    ) -> Result<Value, StudyStoreError> {
        let mut data = self.lock();
        let studies = Self::studies(&mut data)?;
        let Some(Value::Object(rec)) = studies.get_mut(ident) else {
            return Err(StudyStoreError::Missing(implexity_core::py_repr::repr_str(ident)));
        };
        let mut run = Map::new();
        run.insert("variant".into(), Value::String(variant.into()));
        run.insert("job_id".into(), Value::String(job_id.into()));
        run.insert("solve_id".into(), solve_id.map_or(Value::Null, |s| Value::String(s.into())));
        run.insert("started".into(), Value::from(epoch_seconds()));
        match rec.get_mut("runs") {
            Some(Value::Array(runs)) => runs.push(Value::Object(run)),
            _ => {
                rec.insert("runs".into(), Value::Array(vec![Value::Object(run)]));
            }
        }
        rec.insert("updated".into(), Value::from(epoch_seconds()));
        let out = Value::Object(rec.clone());
        self.save(&data)?;
        Ok(out)
    }
}

