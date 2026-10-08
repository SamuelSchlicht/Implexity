// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::document::json::canonical;
use crate::error::{GResult, GeometryError};

pub const HISTORY_SCHEMA: &str = "implexity-engineering-history/1";
pub const SNAPSHOT_SCHEMA: &str = "implexity-engineering-snapshot/1";

pub trait HistoryModels: Send + Sync {
    fn dir(&self) -> PathBuf;

    fn status(&self) -> GResult<Value>;

    fn snapshot(&self) -> GResult<Value>;

    fn put(&self, doc: &Value) -> GResult<()>;
}

fn io(e: &std::io::Error) -> GeometryError {
    GeometryError::Io(e.to_string())
}

fn random_hex16() -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    format!("{:016x}", h.finish())
}

pub struct EngineeringHistory<M: HistoryModels> {
    models: M,
    path: PathBuf,
    lock: Mutex<()>,
}

impl<M: HistoryModels> EngineeringHistory<M> {
    pub fn new(models: M) -> Self {
        let path = models.dir().join("engineering_history.jsonl");
        Self { models, path, lock: Mutex::new(()) }
    }

    fn append_locked(
        &self,
        kind: &str,
        label: &str,
        details: Option<&Map<String, Value>>,
        parent: Option<&str>,
        actor: Option<&str>,
    ) -> GResult<Value> {
        let status = self.models.status().unwrap_or_else(|_| json!({}));
        #[allow(clippy::cast_precision_loss)]
        let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let row = json!({
            "schema": HISTORY_SCHEMA, "id": random_hex16(), "time": time, "kind": kind, "label": label,
            "actor": actor.filter(|a| !a.is_empty()).unwrap_or("User"), "parent": parent,
            "model": {"structure_id": status.get("structure_id").cloned().unwrap_or(Value::Null),
                      "content_id": status.get("content_id").cloned().unwrap_or(Value::Null)},
            "details": Value::Object(details.cloned().unwrap_or_default()),
        });
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p).map_err(|e| io(&e))?;
        }
        let mut fh =
            std::fs::OpenOptions::new().create(true).append(true).open(&self.path).map_err(|e| io(&e))?;
        fh.write_all(format!("{}\n", canonical(&row)).as_bytes()).map_err(|e| io(&e))?;
        fh.flush().map_err(|e| io(&e))?;
        fh.sync_all().map_err(|e| io(&e))?;
        Ok(row)
    }


    pub fn append(
        &self,
        kind: &str,
        label: &str,
        details: Option<&Map<String, Value>>,
        parent: Option<&str>,
        actor: Option<&str>,
    ) -> GResult<Value> {
        let _g = self.lock.lock().map_err(|_| GeometryError::Value("history lock poisoned".into()))?;
        self.append_locked(kind, label, details, parent, actor)
    }

    fn list_locked(&self, limit: usize) -> Value {
        let rows: Vec<Value> = std::fs::read_to_string(&self.path)
            .map(|t| t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect())
            .unwrap_or_default();
        let keep = limit.clamp(1, 2000);
        let start = rows.len().saturating_sub(keep);
        json!({"schema": HISTORY_SCHEMA, "entries": rows[start..].to_vec(), "count": rows.len()})
    }


    pub fn list(&self, limit: usize) -> GResult<Value> {
        let _g = self.lock.lock().map_err(|_| GeometryError::Value("history lock poisoned".into()))?;
        Ok(self.list_locked(limit))
    }


    pub fn snapshot(
        &self,
        label: &str,
        details: Option<&Map<String, Value>>,
        parent: Option<&str>,
    ) -> GResult<Value> {
        let _g = self.lock.lock().map_err(|_| GeometryError::Value("history lock poisoned".into()))?;
        let doc = self.models.snapshot()?;
        let payload = canonical(&doc).into_bytes();
        let digest = hex::encode(Sha256::digest(&payload));
        let snap_dir = self.models.dir().join("engineering_snapshots");
        std::fs::create_dir_all(&snap_dir).map_err(|e| io(&e))?;
        let path = snap_dir.join(format!("{digest}.json"));
        if !path.exists() {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &payload).map_err(|e| io(&e))?;
            std::fs::rename(&tmp, &path).map_err(|e| io(&e))?;
        }
        let mut d = details.cloned().unwrap_or_default();
        d.insert("snapshot_sha256".into(), json!(digest));
        d.insert("snapshot_file".into(), json!(format!("engineering_snapshots/{digest}.json")));
        d.insert("requires_physics_recompute".into(), json!(true));
        self.append_locked("state_snapshot", label, Some(&d), parent, None)
    }


    pub fn restore_snapshot(&self, entry_id: &str) -> GResult<Value> {
        let _g = self.lock.lock().map_err(|_| GeometryError::Value("history lock poisoned".into()))?;
        let entries = self.list_locked(2000)["entries"].as_array().cloned().unwrap_or_default();
        let row = entries.into_iter().find(|x| x.get("id").and_then(Value::as_str) == Some(entry_id));
        let Some(row) = row.filter(|r| r.get("kind").and_then(Value::as_str) == Some("state_snapshot"))
        else {

            return Err(GeometryError::Value(crate::pyfmt::str_repr(&format!(
                "engineering snapshot {} not found",
                crate::pyfmt::str_repr(entry_id)
            ))));
        };
        let Some(rel) = row["details"].get("snapshot_file").and_then(Value::as_str).filter(|s| !s.is_empty())
        else {
            return Err(GeometryError::Value("engineering snapshot has no snapshot artifact".into()));
        };
        let payload = std::fs::read(self.models.dir().join(rel)).map_err(|e| io(&e))?;
        let digest = hex::encode(Sha256::digest(&payload));
        if Some(digest.as_str()) != row["details"].get("snapshot_sha256").and_then(Value::as_str) {
            return Err(GeometryError::Value("engineering snapshot checksum mismatch".into()));
        }
        let doc: Value = serde_json::from_slice(&payload).map_err(|e| GeometryError::Value(e.to_string()))?;
        self.models.put(&doc)?;
        let label = format!(
            "Restored {}",
            row.get("label").and_then(Value::as_str).unwrap_or("engineering snapshot")
        );
        let mut d = Map::new();
        d.insert("snapshot_entry_id".into(), json!(entry_id));
        d.insert("requires_physics_recompute".into(), json!(true));
        let restored = self.append_locked("state_restore", &label, Some(&d), Some(entry_id), None)?;
        Ok(json!({"restored": true, "snapshot": row, "history": restored}))
    }
}
