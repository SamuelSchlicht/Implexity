// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, dumps, parse_strict_bytes};
use crate::packages::PackageManager;
use crate::sync::lock;

pub const SELECTION_SCHEMA: &str = "implexity-package-selection/2";
pub const BUSY: [&str; 7] = ["created", "queued", "starting", "running", "resuming", "paused", "intervening"];

pub trait PackageService: Send + Sync {
    fn state_dir(&self) -> PathBuf;
    fn try_acquire_evaluation(&self) -> Option<Box<dyn Any>>;
    fn jobs(&self) -> Value;
    fn broadcast(&self, message: Value);
}


pub fn check_generation(expected: Option<&Value>, actual: u64) -> CaeResult<()> {
    let Some(expected) = expected.filter(|v| !v.is_null()) else { return Ok(()) };
    let Some(e) = expected.as_u64().filter(|_| !expected.is_f64()) else {
        return Err(CaeError::contract("physics generation must be a non-negative integer"));
    };
    if e != actual {
        return Err(CaeError::contract("STALE_PHYSICS_GENERATION: inspect packages and rebuild request"));
    }
    Ok(())
}

pub struct PackageSession<'a> {
    svc: &'a dyn PackageService,
    manager: &'a PackageManager,
    path: PathBuf,
    restoration_error: Mutex<Option<String>>,
}

impl<'a> PackageSession<'a> {
    #[must_use]
    pub fn new(svc: &'a dyn PackageService, manager: &'a PackageManager) -> Self {
        let path = svc.state_dir().join("physics_packages.json");
        let session = Self { svc, manager, path, restoration_error: Mutex::new(None) };
        if session.path.exists() {
            let before = manager.selected();
            let restored = (|| -> Result<(), String> {
                let raw = std::fs::read(&session.path).map_err(|e| e.to_string())?;
                let row = parse_strict_bytes(&raw).map_err(|e| e.to_string())?;
                let schema = row.get("schema").and_then(Value::as_str);
                if !matches!(schema, Some("implexity-package-selection/2" | "implexity-package-selection/1"))
                {
                    return Err("unsupported saved package selection schema".into());
                }
                let loaded = row.get("loaded").ok_or_else(|| "'loaded'".to_string())?;
                manager.activate_snapshot_value(loaded).map_err(|e| e.message().to_string())
            })();
            if let Err(message) = restored {
                let _ = manager.activate_snapshot(&before);
                *lock(&session.restoration_error) = Some(message);
            }
        }
        session
    }

    #[must_use]
    pub fn restoration_error(&self) -> Option<String> {
        lock(&self.restoration_error).clone()
    }

    fn blockers(&self) -> Vec<Value> {
        let jobs = self.svc.jobs();
        let rows = match &jobs {
            Value::Object(m) => m.get("jobs").and_then(Value::as_array).cloned().unwrap_or_default(),
            Value::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        rows.iter()
            .filter(|j| j.get("status").and_then(Value::as_str).is_some_and(|s| BUSY.contains(&s)))
            .map(|j| j.get("job_id").or_else(|| j.get("id")).cloned().unwrap_or(Value::Null))
            .collect()
    }


    pub fn status(&self) -> CaeResult<Value> {
        let mut row = self.manager.status()?;
        if let Some(m) = row.as_object_mut() {
            m.insert("restoration_error".into(), json!(self.restoration_error()));
            m.insert("lifecycle_blockers".into(), Value::Array(self.blockers()));
        }
        Ok(row)
    }


    pub fn change(
        &self,
        package: &str,
        operation: &str,
        expected_generation: Option<&Value>,
    ) -> CaeResult<Value> {
        if operation != "load" && operation != "unload" {
            return Err(CaeError::contract("operation must be load or unload"));
        }
        let _g = self.manager.hold();
        let state = self.status()?;
        check_generation(expected_generation, state["generation"].as_u64().unwrap_or(0))?;
        if state["lifecycle_blockers"].as_array().is_some_and(|b| !b.is_empty()) {
            return Err(CaeError::contract(
                "PHYSICS_IN_USE: running or paused job owns the physics snapshot",
            ));
        }
        let Some(eval) = self.svc.try_acquire_evaluation() else {
            return Err(CaeError::contract("PHYSICS_IN_USE: a solver is evaluating"));
        };
        let result = (|| -> CaeResult<()> {
            let before = self.manager.selected();
            if operation == "load" {
                self.manager.load(package)?;
            } else {
                self.manager.unload(package)?;
            }
            let persisted = (|| -> CaeResult<()> {
                let status = self.manager.status()?;
                let doc = json!({
                    "schema": SELECTION_SCHEMA,
                    "loaded": self.manager.selected(),
                    "load_order_fingerprint": status["load_order_fingerprint"],
                });
                if let Some(parent) = self.path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| CaeError::contract(e.to_string()))?;
                }
                let tmp = self.path.with_extension("tmp");
                std::fs::write(&tmp, dumps(&doc, &DumpOptions::default().sorted(true)))
                    .map_err(|e| CaeError::contract(e.to_string()))?;
                std::fs::rename(&tmp, &self.path).map_err(|e| CaeError::contract(e.to_string()))
            })();
            if let Err(e) = persisted {
                self.manager.activate_snapshot(&before)?;
                return Err(e);
            }
            Ok(())
        })();
        drop(eval);
        result?;
        *lock(&self.restoration_error) = None;
        let row = self.status()?;
        let mut message = serde_json::Map::new();
        message.insert("kind".into(), json!("physics_packages_changed"));
        if let Some(m) = row.as_object() {
            for (k, v) in m {
                message.insert(k.clone(), v.clone());
            }
        }
        self.svc.broadcast(Value::Object(message));
        Ok(row)
    }


    pub fn runtime_guard<T>(&self, request: &Value, body: impl FnOnce() -> CaeResult<T>) -> CaeResult<T> {
        let _g = self.manager.hold();
        if let Some(e) = self.restoration_error() {
            return Err(CaeError::contract(format!("PHYSICS_SELECTION_INVALID: {e}")));
        }
        let expected = request.as_object().and_then(|m| m.get("physics_generation"));
        check_generation(expected, self.manager.generation())?;
        body()
    }
}

