// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use implexity_geometry::GeometryError;
use implexity_geometry::document::{self as D, Model, arrays};
use implexity_geometry::eval::{self as EV, Aabb, EvalOptions};
use implexity_geometry::fieldclass::FieldClass;
use implexity_geometry::node::{Mode, NodeRef};
use implexity_geometry::value::{NdArray, ParamValue};
use implexity_io::atomic as fsx;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::py::{jf, now, py_float, py_round, py_str, repr, sha256_hex};
use crate::sync::{ReentrantGuard, ReentrantLock, lock};

pub const MODEL_DIR: &str = "implicit";
pub const MODEL_FILE: &str = "model.json";
pub const RECOVERY_SCHEMA: &str = "implexity-model-persistence-recovery/1";
pub const MAX_PUBLIC_EPOCH_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

#[must_use]
pub fn max_eval_points() -> usize {
    implexity_mesh::model_view::max_eval_points()
}

fn runtime(message: impl Into<String>) -> AuthoringError {
    AuthoringError::runtime("RuntimeError", message)
}

fn storage(e: &fsx::StorageError) -> AuthoringError {
    match e {
        fsx::StorageError::Unsafe(m) => runtime(m.clone()),
        other @ fsx::StorageError::Io(..) => AuthoringError::Io(other.to_string()),
    }
}

fn io_err(path: &Path, e: &std::io::Error) -> AuthoringError {
    AuthoringError::Io(format!("[Errno {}] {}: {}", e.raw_os_error().unwrap_or(0), e, path.display()))
}

fn doc_dumps(doc: &Value) -> String {
    D::dumps(doc)
}

fn canonical_copy(doc: &Value) -> AResult<Value> {
    implexity_core::json::parse_strict(&doc_dumps(doc))
        .map_err(|e| AuthoringError::value("ValueError", e.to_string()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedSidecar {
    pub file: String,
    pub sha256: String,
}

impl CreatedSidecar {
    fn to_json(&self) -> Value {
        json!({"file": self.file, "sha256": self.sha256})
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryMarker {
    pub before_exists: bool,
    pub backup: Option<String>,
    pub created_sidecars: Vec<CreatedSidecar>,
}

impl RecoveryMarker {
    fn to_json(&self) -> Value {
        json!({
            "schema": RECOVERY_SCHEMA,
            "before_exists": self.before_exists,
            "backup": self.backup,
            "created_sidecars": self.created_sidecars.iter().map(CreatedSidecar::to_json).collect::<Vec<_>>(),
        })
    }
}

fn sidecar_name_ok(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("implexity-array-sha256-") else { return false };
    let Some(hexpart) = rest.strip_suffix(".npy") else { return false };
    hexpart.len() == 64 && hexpart.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Default)]
struct State {
    model: Option<Arc<Model>>,
    loaded_at: Option<f64>,
    load_error: Option<Vec<String>>,
    recovery: Option<RecoveryMarker>,
    live_optimisation: Option<Value>,
}

type StatusState = (Option<RecoveryMarker>, Option<Value>, Option<Vec<String>>);

#[derive(Clone, Debug)]
pub struct PreparedValues {
    pub named: BTreeMap<String, f64>,
    pub direct: Vec<(Value, ParamValue)>,
    pub spatial: Vec<SpatialValue>,
}

#[derive(Clone, Debug)]
pub struct SpatialValue {
    pub entry: Value,
    pub value: NdArray,
    pub encoded: Map<String, Value>,
    pub raw: Vec<u8>,
    pub npy: Vec<u8>,
}

pub struct ModelManager {
    dir: PathBuf,
    path: PathBuf,
    recovery_path: PathBuf,
    dir_identity: (u64, u64),
    authority: ReentrantLock,
    state: Mutex<State>,
}

#[cfg(unix)]
fn dir_identity_of(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn dir_identity_of(_meta: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

#[cfg(unix)]
fn foreign_owner(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    fsx::current_uid().is_some_and(|u| u != meta.uid())
}

#[cfg(not(unix))]
fn foreign_owner(_meta: &std::fs::Metadata) -> bool {
    false
}

impl ModelManager {


    pub fn new(state_dir: &Path) -> AResult<Self> {
        let dir = state_dir.join(MODEL_DIR);
        match std::fs::create_dir_all(state_dir).and_then(|()| std::fs::create_dir(&dir)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(&dir, &e)),
        }
        let meta = std::fs::symlink_metadata(&dir).map_err(|e| io_err(&dir, &e))?;
        if meta.file_type().is_symlink() || !meta.is_dir() || foreign_owner(&meta) {
            return Err(runtime(format!(
                "refusing non-directory, symlinked, or non-owned implicit model storage {}",
                dir.display()
            )));
        }
        let path = dir.join(MODEL_FILE);
        let recovery_path = dir.join(format!("{MODEL_FILE}.recovery.json"));
        let mgr = Self {
            dir_identity: dir_identity_of(&meta),
            dir,
            path,
            recovery_path,
            authority: ReentrantLock::new(),
            state: Mutex::new(State::default()),
        };
        let loaded = (|| -> AResult<()> {
            mgr.recover_pending()?;
            if let Some(stored) = mgr.owned_regular_bytes(&mgr.path, true)? {
                let text = String::from_utf8(stored).map_err(|e| {
                    AuthoringError::model_doc(vec![format!("stored model JSON cannot be decoded: {e}")])
                })?;
                let document = implexity_core::json::parse_strict(&text).map_err(|e| {
                    AuthoringError::model_doc(vec![format!("stored model JSON cannot be decoded: {e}")])
                })?;
                let model = D::build(&document, Some(&mgr.dir), None)?;
                let mut st = lock(&mgr.state);
                st.model = Some(Arc::new(model));
                st.loaded_at = Some(now());
            }
            Ok(())
        })();
        if let Err(e) = loaded {
            let problems =
                if e.is_model_doc() { e.problem_list() } else { vec![format!("{}: {}", e.class(), e)] };
            lock(&mgr.state).load_error = Some(problems);
        }
        Ok(mgr)
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn live_lock(&self) -> ReentrantGuard<'_> {
        self.authority.acquire()
    }

    #[must_use]
    pub fn live_lock_held(&self) -> bool {
        self.authority.held_by_current()
    }

    #[must_use]
    pub fn model(&self) -> Option<Arc<Model>> {
        lock(&self.state).model.clone()
    }

    #[must_use]
    pub fn loaded_at(&self) -> Option<f64> {
        lock(&self.state).loaded_at
    }

    #[must_use]
    pub fn persistence_recovery(&self) -> Option<RecoveryMarker> {
        lock(&self.state).recovery.clone()
    }

    #[must_use]
    pub fn live_optimisation(&self) -> Option<Value> {
        lock(&self.state).live_optimisation.clone()
    }

    pub fn set_live_optimisation(&self, value: Option<Value>) {
        lock(&self.state).live_optimisation = value;
    }

    fn assert_storage_directory(&self) -> AResult<()> {
        let meta = std::fs::symlink_metadata(&self.dir).map_err(|e| io_err(&self.dir, &e))?;
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || foreign_owner(&meta)
            || dir_identity_of(&meta) != self.dir_identity
        {
            return Err(runtime(format!(
                "implicit model storage directory changed identity: {}",
                self.dir.display()
            )));
        }
        Ok(())
    }

    fn fsync_directory(&self) -> AResult<()> {
        self.assert_storage_directory()?;
        fsx::fsync_directory(&self.dir).map_err(|e| storage(&e))
    }

    fn owned_regular_bytes(&self, path: &Path, absent_ok: bool) -> AResult<Option<Vec<u8>>> {
        self.assert_storage_directory()?;
        if path.parent() != Some(self.dir.as_path()) {
            return Err(runtime(format!("persistence path escapes model storage: {}", path.display())));
        }
        fsx::read_owned_regular(path, absent_ok).map_err(|e| storage(&e))
    }

    fn stage(&self, blob: &[u8], role: &str, basename: &str) -> AResult<PathBuf> {
        self.assert_storage_directory()?;
        fsx::stage_exclusive(&self.dir, basename, role, blob).map_err(|e| storage(&e))
    }

    fn unlink_if_present(path: Option<&Path>) -> AResult<()> {
        let Some(p) = path else { return Ok(()) };
        match std::fs::remove_file(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(p, &e)),
        }
    }

    fn link_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::hard_link(source, destination)
    }

    fn replace_model_file(&self, staged: &Path) -> AResult<()> {
        let _existing = self.owned_regular_bytes(&self.path, true)?;
        std::fs::rename(staged, &self.path).map_err(|e| io_err(&self.path, &e))?;
        self.fsync_directory()
    }

    fn restore_model_file(&self, marker: &RecoveryMarker) -> AResult<()> {
        if marker.before_exists {
            let backup = self.dir.join(marker.backup.as_deref().unwrap_or_default());
            let blob = self.owned_regular_bytes(&backup, false)?.unwrap_or_default();
            let staged = self.stage(&blob, "restore", MODEL_FILE)?;
            let moved = std::fs::rename(&staged, &self.path);
            if let Err(e) = moved {
                Self::unlink_if_present(Some(&staged))?;
                return Err(io_err(&self.path, &e));
            }
            self.fsync_directory()?;
        } else if self.owned_regular_bytes(&self.path, true)?.is_some() {
            std::fs::remove_file(&self.path).map_err(|e| io_err(&self.path, &e))?;
            self.fsync_directory()?;
        }
        Ok(())
    }

    fn publish_recovery_marker(&self, marker: &RecoveryMarker) -> AResult<()> {
        let opts = implexity_core::json::DumpOptions::canonical();
        let mut raw = implexity_core::json::dumps(&marker.to_json(), &opts);
        raw.push('\n');
        let staged = self.stage(raw.as_bytes(), "recovery-marker", MODEL_FILE)?;
        let result = (|| -> AResult<()> {
            Self::link_no_replace(&staged, &self.recovery_path)
                .map_err(|e| io_err(&self.recovery_path, &e))?;
            lock(&self.state).recovery = Some(marker.clone());
            std::fs::remove_file(&staged).map_err(|e| io_err(&staged, &e))?;
            self.fsync_directory()
        })();
        Self::unlink_if_present(Some(&staged))?;
        result
    }

    fn read_recovery_marker(&self) -> AResult<Option<RecoveryMarker>> {
        let Some(raw) = self.owned_regular_bytes(&self.recovery_path, true)? else { return Ok(None) };
        let text = String::from_utf8(raw)
            .map_err(|e| runtime(format!("invalid model persistence recovery marker: {e}")))?;
        if !text.is_ascii() {
            return Err(runtime("invalid model persistence recovery marker: not ASCII"));
        }
        let marker = implexity_core::json::parse_strict(&text)
            .map_err(|e| runtime(format!("invalid model persistence recovery marker: {e}")))?;
        let Some(m) = marker.as_object() else {
            return Err(runtime("invalid model persistence recovery marker contract"));
        };
        let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
        keys.sort_unstable();
        if keys != ["backup", "before_exists", "created_sidecars", "schema"]
            || m.get("schema").and_then(Value::as_str) != Some(RECOVERY_SCHEMA)
            || !m.get("before_exists").is_some_and(Value::is_boolean)
            || !m.get("created_sidecars").is_some_and(Value::is_array)
        {
            return Err(runtime("invalid model persistence recovery marker contract"));
        }
        let before_exists = m["before_exists"].as_bool().unwrap_or(false);
        let backup = match m.get("backup") {
            Some(Value::Null) | None => None,
            Some(v) => Some(v.clone()),
        };
        let backup = if before_exists {
            let prefix = format!(".{MODEL_FILE}.backup.");
            match backup.as_ref().and_then(Value::as_str) {
                Some(b) if !b.contains('/') && !b.contains('\\') && b.starts_with(&prefix) => {
                    Some(b.to_string())
                }
                _ => return Err(runtime("invalid model persistence recovery backup")),
            }
        } else if backup.is_some() {
            return Err(runtime("absent pre-call model unexpectedly has a backup"));
        } else {
            None
        };
        let mut rows = Vec::new();
        for row in m["created_sidecars"].as_array().into_iter().flatten() {
            let ok = row.as_object().is_some_and(|r| {
                r.len() == 2
                    && r.get("file").and_then(Value::as_str).is_some_and(sidecar_name_ok)
                    && r.get("sha256").is_some_and(|s| hex64(&py_str(s)))
            });
            if !ok {
                return Err(runtime("invalid created-sidecar recovery entry"));
            }
            rows.push(CreatedSidecar { file: py_str(&row["file"]), sha256: py_str(&row["sha256"]) });
        }
        Ok(Some(RecoveryMarker { before_exists, backup, created_sidecars: rows }))
    }

    fn cleanup_created_sidecars(&self, rows: &[CreatedSidecar]) -> AResult<()> {
        for row in rows {
            let path = self.dir.join(&row.file);
            let Some(blob) = self.owned_regular_bytes(&path, true)? else { continue };
            if sha256_hex(&blob) != row.sha256 {
                return Err(runtime(format!(
                    "refusing to remove a changed transaction sidecar {}",
                    path.display()
                )));
            }
            std::fs::remove_file(&path).map_err(|e| io_err(&path, &e))?;
        }
        if !rows.is_empty() {
            self.fsync_directory()?;
        }
        Ok(())
    }


    pub fn recover_pending(&self) -> AResult<bool> {
        let _g = self.authority.acquire();
        let Some(marker) = self.read_recovery_marker()? else {
            lock(&self.state).recovery = None;
            return Ok(false);
        };
        lock(&self.state).recovery = Some(marker.clone());
        self.restore_model_file(&marker)?;
        Self::unlink_if_present(Some(&self.recovery_path))?;
        if marker.before_exists {
            Self::unlink_if_present(Some(&self.dir.join(marker.backup.as_deref().unwrap_or_default())))?;
        }
        self.cleanup_created_sidecars(&marker.created_sidecars)?;
        self.fsync_directory()?;
        lock(&self.state).recovery = None;
        Ok(true)
    }


    pub fn publish_content_sidecar(&self, filename: &str, blob: &[u8]) -> AResult<bool> {
        if !sidecar_name_ok(filename) || !filename.contains(&sha256_hex(blob)) {
            return Err(runtime("invalid content-addressed sidecar identity"));
        }
        let target = self.dir.join(filename);
        let staged = self.stage(blob, "sidecar", filename)?;
        let mut created = false;
        let result = (|| -> AResult<bool> {
            match Self::link_no_replace(&staged, &target) {
                Ok(()) => {
                    created = true;
                    std::fs::remove_file(&staged).map_err(|e| io_err(&staged, &e))?;
                    self.fsync_directory()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = self.owned_regular_bytes(&target, false)?.unwrap_or_default();
                    if existing != blob {
                        return Err(runtime(format!(
                            "content-addressed sidecar collision at {}",
                            target.display()
                        )));
                    }
                }
                Err(e) => return Err(io_err(&target, &e)),
            }
            if self.owned_regular_bytes(&target, false)?.as_deref() != Some(blob) {
                return Err(runtime("published sidecar failed exact verification"));
            }
            Ok(created)
        })();
        Self::unlink_if_present(Some(&staged))?;
        if result.is_err() && created && self.owned_regular_bytes(&target, true)?.as_deref() == Some(blob) {
            std::fs::remove_file(&target).map_err(|e| io_err(&target, &e))?;
            self.fsync_directory()?;
        }
        result
    }

    fn commit_model(
        &self,
        model: Model,
        norm: &Value,
        created: &[CreatedSidecar],
        loaded_at: Option<f64>,
    ) -> AResult<()> {
        let _g = self.authority.acquire();
        self.recover_pending()?;
        let (before_model, before_loaded, before_error) = {
            let st = lock(&self.state);
            (st.model.clone(), st.loaded_at, st.load_error.clone())
        };
        let old = self.owned_regular_bytes(&self.path, true)?;
        let blob = doc_dumps(norm);
        let mut staged: Option<PathBuf> = None;
        let mut backup: Option<PathBuf> = None;
        let mut marker: Option<RecoveryMarker> = None;
        let mut marker_published = false;
        let mut model = Some(model);
        let attempt = (|| -> AResult<()> {
            staged = Some(self.stage(blob.as_bytes(), "model", MODEL_FILE)?);
            if let Some(o) = &old {
                backup = Some(self.stage(o, "backup", MODEL_FILE)?);
            }
            let mk = RecoveryMarker {
                before_exists: old.is_some(),
                backup: backup.as_ref().and_then(|b| b.file_name()).map(|n| n.to_string_lossy().into_owned()),
                created_sidecars: created.to_vec(),
            };
            marker = Some(mk.clone());
            self.publish_recovery_marker(&mk)?;
            marker_published = true;
            if let Some(s) = staged.take()
                && let Err(e) = self.replace_model_file(&s)
            {
                staged = Some(s);
                return Err(e);
            }
            {
                let mut st = lock(&self.state);
                st.model = model.take().map(Arc::new);
                st.loaded_at = Some(loaded_at.unwrap_or_else(now));
                st.load_error = None;
            }
            std::fs::remove_file(&self.recovery_path).map_err(|e| io_err(&self.recovery_path, &e))?;
            self.fsync_directory()?;
            lock(&self.state).recovery = None;
            Self::unlink_if_present(backup.as_deref())?;
            backup = None;
            Ok(())
        })();
        let outcome = match attempt {
            Ok(()) => Ok(()),
            Err(exc) => {
                {
                    let mut st = lock(&self.state);
                    st.model = before_model;
                    st.loaded_at = before_loaded;
                    st.load_error = before_error;
                }
                let pending = lock(&self.state).recovery.is_some();
                if marker_published || pending {
                    let mk = marker.clone().unwrap_or(RecoveryMarker {
                        before_exists: old.is_some(),
                        backup: None,
                        created_sidecars: created.to_vec(),
                    });
                    let rollback = (|| -> AResult<()> {
                        self.restore_model_file(&mk)?;
                        std::fs::remove_file(&self.recovery_path)
                            .map_err(|e| io_err(&self.recovery_path, &e))?;
                        self.fsync_directory()?;
                        lock(&self.state).recovery = None;
                        self.cleanup_created_sidecars(&mk.created_sidecars)
                    })();
                    match rollback {
                        Ok(()) => Err(exc),
                        Err(rb) => {
                            lock(&self.state).recovery = Some(mk);
                            Err(runtime(format!(
                                "model persistence failed ({}: {}) and exact rollback failed ({}: {}); authority is blocked by {} and will retry recovery before the next mutation",
                                exc.class(),
                                exc,
                                rb.class(),
                                rb,
                                self.recovery_path.display()
                            )))
                        }
                    }
                } else {
                    self.cleanup_created_sidecars(created)?;
                    Err(exc)
                }
            }
        };
        Self::unlink_if_present(staged.as_deref())?;
        if lock(&self.state).recovery.is_none() {
            Self::unlink_if_present(backup.as_deref())?;
        }
        outcome
    }


    pub fn require(&self) -> AResult<Arc<Model>> {
        let st = lock(&self.state);
        if st.recovery.is_some() {
            return Err(runtime(format!(
                "model persistence recovery is pending at {}; exact model authority is unavailable until recovery succeeds",
                self.recovery_path.display()
            )));
        }
        if let Some(m) = &st.model {
            Ok(Arc::clone(m))
        } else {
            let mut problems = st.load_error.clone().unwrap_or_default();
            problems.push(
                "no model document is stored; PUT /v1/implicit/model with one.  GET /v1/implicit/catalogue lists the node kinds a document may name"
                    .into(),
            );
            Err(AuthoringError::model_doc(problems))
        }
    }


    fn with_live_mut<R>(&self, f: impl FnOnce(&mut Model) -> AResult<R>) -> AResult<R> {
        let _g = self.authority.acquire();
        let mut st = lock(&self.state);
        let Some(arc) = st.model.as_mut() else {
            drop(st);
            self.require()?;
            return Err(runtime("the live model disappeared"));
        };
        if Arc::get_mut(arc).is_none() {
            let mut copy = D::build(&arc.to_doc()?, Some(&self.dir), None)?;
            copy.warnings.clone_from(&arc.warnings);
            *arc = Arc::new(copy);
        }
        let m = Arc::get_mut(arc).ok_or_else(|| runtime("the live model is shared"))?;
        f(m)
    }

    fn set_live(&self, model: Model, loaded_at: Option<f64>) {
        let mut st = lock(&self.state);
        st.model = Some(Arc::new(model));
        if let Some(t) = loaded_at {
            st.loaded_at = Some(t);
        }
        st.load_error = None;
    }

    fn rebuild(&self, doc: &Value) -> AResult<Model> {
        Ok(D::build(doc, Some(&self.dir), None)?)
    }


    fn canonical_build(&self, doc: &Value) -> AResult<(Model, Value)> {
        let model = self.rebuild(doc)?;
        let norm = model.to_doc()?;
        if &norm == doc {
            return Ok((model, norm));
        }
        let mut canonical = self.rebuild(&norm)?;
        canonical.warnings.clone_from(&model.warnings);
        Ok((canonical, norm))
    }


    pub fn put(&self, doc: &Value) -> AResult<Value> {
        if !doc.is_object() {
            return Err(AuthoringError::model_doc(vec![
                "the request body must be the model document, a JSON object".into(),
            ]));
        }
        let _g = self.authority.acquire();
        self.recover_pending()?;
        let (mut model, norm) = self.canonical_build(doc)?;
        let loaded_at = now();
        let response = self.status_for(Some(&mut model), Some(loaded_at), self.snapshot_state())?;
        self.commit_model(model, &norm, &[], Some(loaded_at))?;
        Ok(response)
    }


    pub fn snapshot(&self) -> AResult<Value> {
        let _g = self.authority.acquire();
        canonical_copy(&self.require()?.to_doc()?)
    }


    pub fn persist_live(&self) -> AResult<Value> {
        let _g = self.authority.acquire();
        self.recover_pending()?;
        self.require()?;
        let loaded_at = now();
        let snap = self.snapshot_state();
        let (response, norm) = self.with_live_mut(|m| {
            let norm = m.to_doc()?;
            Ok((self.status_for(Some(m), Some(loaded_at), snap)?, norm))
        })?;
        let model = self.rebuild_live_copy()?;
        self.commit_model(model, &norm, &[], Some(loaded_at))?;
        Ok(response)
    }

    fn rebuild_live_copy(&self) -> AResult<Model> {
        let live = self.require()?;
        let mut copy = D::build(&live.to_doc()?, Some(&self.dir), None)?;
        copy.warnings.clone_from(&live.warnings);
        Ok(copy)
    }


    pub fn replace_live(&self, doc: &Value, persist: bool) -> AResult<Value> {
        if !doc.is_object() {
            return Err(AuthoringError::model_doc(vec!["a live model snapshot must be an object".into()]));
        }
        let _g = self.authority.acquire();
        self.recover_pending()?;
        let (mut rebuilt, norm) = self.canonical_build(doc)?;
        let loaded_at = now();
        let response = self.status_for(Some(&mut rebuilt), Some(loaded_at), self.snapshot_state())?;
        if persist {
            self.commit_model(rebuilt, &norm, &[], Some(loaded_at))?;
        } else {
            self.set_live(rebuilt, Some(loaded_at));
        }
        Ok(response)
    }


    pub fn mutate_live(
        &self,
        named: &BTreeMap<String, Value>,
        direct: &[(String, String, f64)],
    ) -> AResult<Value> {
        let _g = self.authority.acquire();
        self.recover_pending()?;
        let before = self.require()?.to_doc()?;
        let result = self.with_live_mut(|m| {
            let mut moved: Vec<Value> = Vec::new();
            if !named.is_empty() {
                let rep = m.set_parameters(named)?;
                if let Some(rows) = rep.get("moved").and_then(Value::as_array) {
                    moved.extend(rows.iter().cloned());
                }
            }
            for (nid, param, value) in direct {
                let node = m.node_table().get(nid).cloned().ok_or_else(|| AuthoringError::Key(repr(&Value::from(nid.clone()))))?;
                let was = node.param(param).map_or(Value::Null, ParamValue::to_json);
                let units = node.info().param(param).map(|s| s.units.clone()).unwrap_or_default();
                m.set_node_param(nid, param, &ParamValue::Float(*value))?;
                let shared = m.parents().get(nid).is_some_and(|p| p.len() > 1);
                moved.push(json!({"node": nid, "param": param, "was": was, "now": jf(*value), "units": units, "shared": shared}));
            }
            let mut broken = Vec::new();
            for (nid, _p, _v) in direct {
                if let Some(node) = m.node_table().get(nid) {
                    let msg = D::node_invariant(node);
                    if !msg.is_empty() {
                        broken.push(format!("nodes.{nid} ({}): {msg}", node.kind()));
                    }
                }
            }
            if !broken.is_empty() {
                return Err(AuthoringError::model_doc(broken));
            }
            m.to_doc()?;
            Ok(json!({"structure_id": m.structure_id(), "content_id": m.content_id(), "moved": moved}))
        });
        if result.is_err() {
            let restored = self.rebuild(&before)?;
            self.set_live(restored, None);
        }
        result
    }

    fn snapshot_state(&self) -> StatusState {
        let st = lock(&self.state);
        (st.recovery.clone(), st.live_optimisation.clone(), st.load_error.clone())
    }

    fn status_for(&self, m: Option<&mut Model>, loaded_at: Option<f64>, snap: StatusState) -> AResult<Value> {
        let (recovery, live, load_error) = snap;
        let recovery_block = recovery.as_ref().map(|r| {
            json!({"blocked": true, "marker": self.recovery_path.display().to_string(),
                   "before_exists": r.before_exists, "created_sidecars": r.created_sidecars.len()})
        });
        let Some(m) = m else {
            let mut out = Map::new();
            out.insert("kind".into(), json!("implicit_model"));
            out.insert("units".into(), json!("mm"));
            out.insert("loaded".into(), json!(false));
            out.insert("path".into(), json!(self.path.display().to_string()));
            out.insert("problems".into(), json!(load_error.unwrap_or_default()));
            out.insert("kinds".into(), json!(implexity_geometry::node::registry().names().len()));
            out.insert(
                "how".into(),
                json!("PUT /v1/implicit/model with the document; GET /v1/implicit/catalogue lists the kinds"),
            );
            if let Some(b) = recovery_block {
                out.insert("persistence_recovery".into(), b);
            }
            return Ok(Value::Object(out));
        };
        let doc = m.to_doc()?;
        let graph = m.describe();
        let names = m.names();
        let shared = m.shared();
        let sha = m.sha256()?;
        let nbytes = m.dumps()?.len();
        let parameters = m.parameter_table();
        let provenance = m.provenance_block()?;
        let mut out = Map::new();
        out.insert("kind".into(), json!("implicit_model"));
        out.insert("units".into(), json!("mm"));
        out.insert("loaded".into(), json!(true));
        out.insert("path".into(), json!(self.path.display().to_string()));
        out.insert("schema".into(), doc["schema"].clone());
        out.insert("name".into(), doc["name"].clone());
        let root = doc.get("root").cloned().unwrap_or(Value::Null);
        let outputs =
            doc.get("outputs").filter(|v| crate::py::truthy(v)).cloned().unwrap_or_else(|| json!({}));
        out.insert("document".into(), doc);
        out.insert("graph".into(), graph);
        out.insert("root".into(), root);
        out.insert("outputs".into(), outputs);
        out.insert("names".into(), json!(names));
        out.insert("shared".into(), json!(shared));
        out.insert("structure_id".into(), json!(m.structure_id()));
        out.insert("content_id".into(), json!(m.content_id()));
        out.insert("sha256".into(), json!(sha));
        out.insert("bytes".into(), json!(nbytes));
        out.insert("parameters".into(), Value::Array(parameters));
        out.insert("warnings".into(), json!(m.warnings));
        out.insert("provenance".into(), provenance["canonical_sha256"].clone());
        out.insert("loaded_at".into(), loaded_at.map_or(Value::Null, jf));
        out.insert("aabb".into(), m.root().map_or(Value::Null, |r| extent_of(&r)));
        let by_node: Map<String, Value> =
            m.node_table().iter().map(|(nid, n)| (nid.clone(), extent_of(n))).collect();
        out.insert("aabb_by_node".into(), Value::Object(by_node));
        if let Some(l) = live {
            out.insert("live_optimisation".into(), l);
        }
        if let Some(b) = recovery_block {
            out.insert("persistence_recovery".into(), b);
        }
        Ok(Value::Object(out))
    }


    pub fn status(&self) -> AResult<Value> {
        let _g = self.authority.acquire();
        let loaded_at = self.loaded_at();
        if self.model().is_none() {
            return self.status_for(None, loaded_at, self.snapshot_state());
        }
        let snap = self.snapshot_state();
        self.with_live_mut(|m| self.status_for(Some(m), loaded_at, snap))
    }


    pub fn edit(&self, request: &Value) -> AResult<Value> {
        if !request.is_object() {
            return Err(AuthoringError::model_doc(vec!["the graph-edit request must be an object".into()]));
        }
        let _g = self.authority.acquire();
        let m = self.require()?;
        if let Some(expected) = request.get("expected_model") {
            let expected = expected.as_object().filter(|m| m.len() == 2).ok_or_else(|| {
                AuthoringError::model_doc(vec!["expected_model needs structure_id and content_id".into()])
            })?;
            for (key, actual) in [("structure_id", m.structure_id()), ("content_id", m.content_id())] {
                let supplied = expected.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| {
                    AuthoringError::model_doc(vec![format!("expected_model.{key} must be a nonempty string")])
                })?;
                if Some(supplied) != actual.as_deref() {
                    return Err(AuthoringError::model_doc(vec![format!("the model changed since the graph edit was prepared ({key})")]));
                }
            }
        }
        let ops = request.get("operations");
        let (norm, mut report) = crate::editing::apply(&m.to_doc()?, ops, Some(&self.dir))?;
        let (before_s, before_c) = (m.structure_id(), m.content_id());
        drop(m);
        let status = self.put(&norm)?;
        if let Some(r) = report.as_object_mut() {
            let recompiled = status.get("structure_id") != Some(&json!(before_s));
            r.insert("structure_id_before".into(), json!(before_s));
            r.insert("content_id_before".into(), json!(before_c));
            r.insert("recompiled".into(), json!(recompiled));
            r.insert("model".into(), status);
        }
        Ok(report)
    }


    pub fn set_parameters(&self, values: &BTreeMap<String, Value>) -> AResult<Value> {
        let _g = self.authority.acquire();
        self.recover_pending()?;
        let m = self.require()?;
        let t0 = std::time::Instant::now();
        let before = canonical_copy(&m.to_doc()?)?;
        drop(m);
        let mut candidate = self.rebuild(&before)?;
        let mut rep = candidate.set_parameters(values)?;
        let norm = candidate.to_doc()?;
        let candidate = self.rebuild(&norm)?;
        let norm = candidate.to_doc()?;
        let r = rep.as_object_mut().ok_or_else(|| runtime("set_parameters returned no report"))?;
        let values_block = r.shift_remove("parameters").unwrap_or(Value::Null);
        r.insert("values".into(), values_block);
        r.insert("parameters".into(), Value::Array(candidate.parameter_table()));
        r.insert("seconds".into(), jf(py_round(t0.elapsed().as_secs_f64(), 6)));
        r.insert("kind".into(), json!("implicit_parameters"));
        r.insert("units".into(), json!("mm"));
        r.insert(
            "shapes".into(),
            json!({
                "parameters": "the parameter TABLE -- one record per named parameter, the same shape GET /v1/implicit/parameters and GET /v1/implicit/model answer with",
                "values": "name -> resolved number, the shape this endpoint TAKES, with the derived parameters filled in",
                "moved": "one row per NODE parameter the edit moved",
            }),
        );
        r.insert(
            "note".into(),
            json!("structure_id is unchanged by construction; the validated graph is atomically published with the stored document and the evaluator keeps its compiled kernel because that cache is keyed on structure_id, not Python object identity"),
        );
        self.commit_model(candidate, &norm, &[], None)?;
        Ok(rep)
    }


    pub fn prepare_model_values(
        plan: &[Value],
        values: &BTreeMap<String, NdArray>,
    ) -> AResult<PreparedValues> {
        let mut named = BTreeMap::new();
        let mut direct = Vec::new();
        let mut spatial = Vec::new();
        let mut problems = Vec::new();
        for e in plan {
            let r = py_str(&e["ref"]);
            let Some(v) = values.get(&r) else { continue };
            let kind = e.get("kind").map(py_str).unwrap_or_default();
            if kind == "spatial_array" {
                if v.ndim() == 0 {
                    problems.push(format!("{r} is a spatial topology coordinate but carries a scalar"));
                    continue;
                }
                if !v.to_f64_vec().iter().all(|x| x.is_finite()) {
                    problems.push(format!("{r} contains non-finite spatial topology values"));
                    continue;
                }
                let (encoded, raw) = arrays::encode_array(v);
                let npy = arrays::npy_bytes(v);
                spatial.push(SpatialValue { entry: e.clone(), value: v.clone(), encoded, raw, npy });
                continue;
            }
            let flat = v.to_f64_vec();
            if !flat.iter().all(|x| x.is_finite()) {
                problems.push(format!("{r} contains non-finite values"));
                continue;
            }
            if kind == "parameter" {
                if flat.len() != 1 {
                    problems.push(format!(
                        "{r} carries {} values and the document's parameter {} is one number",
                        flat.len(),
                        repr(&e["parameter"])
                    ));
                    continue;
                }
                let mut x = D::convert(flat[0], &py_str(&e["node_units"]), &py_str(&e["units"]))?;
                if let Some(lo) = e.get("doc_min").filter(|v| !v.is_null()) {
                    x = x.max(py_float(lo)?);
                }
                if let Some(hi) = e.get("doc_max").filter(|v| !v.is_null()) {
                    x = x.min(py_float(hi)?);
                }
                named.insert(py_str(&e["parameter"]), x);
            } else if v.ndim() == 0 {
                direct.push((e.clone(), ParamValue::Float(flat[0])));
            } else {
                let arr = NdArray::from_f64(v.shape().to_vec(), flat)
                    .ok_or_else(|| runtime("array shape mismatch"))?;
                direct.push((e.clone(), ParamValue::Array(Arc::new(arr))));
            }
        }
        if !problems.is_empty() {
            return Err(AuthoringError::model_doc(problems));
        }
        Ok(PreparedValues { named, direct, spatial })
    }

    fn apply_prepared(
        m: &mut Model,
        prepared: &PreparedValues,
        persistent: bool,
    ) -> AResult<(Vec<Value>, Value, BTreeMap<String, Vec<u8>>, bool)> {
        let mut moved: Vec<Value> = Vec::new();
        let mut sidecars: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut touched: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut pending_params: BTreeMap<String, BTreeMap<String, ParamValue>> = BTreeMap::new();
        let mut rebuild_needed = false;
        if !prepared.named.is_empty() {
            let named: BTreeMap<String, Value> =
                prepared.named.iter().map(|(k, v)| (k.clone(), jf(*v))).collect();
            let rep = m.set_parameters(&named)?;
            if let Some(rows) = rep.get("moved").and_then(Value::as_array) {
                moved.extend(rows.iter().cloned());
            }
        }
        for (e, val) in &prepared.direct {
            let (nid, param) = (py_str(&e["node"]), py_str(&e["param"]));
            let Some(node) = m.node_table().get(&nid).cloned() else {
                return Err(AuthoringError::model_doc(vec![format!("no node {}", repr(&Value::from(nid)))]));
            };
            let was = node.param(&param).map_or(Value::Null, |p| num_json(&p.to_json()));
            m.set_node_param(&nid, &param, val)?;
            touched.insert(nid.clone());
            let shared = m.parents().get(&nid).is_some_and(|p| p.len() > 1);
            moved.push(json!({"node": nid, "param": param, "was": was, "now": num_json(&val.to_json()),
                "units": e["node_units"].clone(), "shared": shared}));
        }
        let mut array_keys = std::collections::BTreeSet::new();
        for s in &prepared.spatial {
            let e = &s.entry;
            let (nid, param) = (py_str(&e["node"]), py_str(&e["param"]));
            let r = py_str(&e["ref"]);
            let Some(node) = m.node_table().get(&nid).cloned() else {
                return Err(AuthoringError::model_doc(vec![format!("no node {}", repr(&Value::from(nid)))]));
            };
            if node.info().param(&param).is_none() {
                return Err(AuthoringError::model_doc(vec![format!(
                    "{} has no parameter {}",
                    node.kind(),
                    repr(&Value::from(param))
                )]));
            }
            let was = node.param(&param).cloned().unwrap_or(ParamValue::Float(f64::NAN));
            let was_shape: Vec<usize> = was.as_ndarray().map(|a| a.shape().to_vec()).unwrap_or_default();
            if was_shape != s.value.shape() {
                return Err(AuthoringError::model_doc(vec![format!(
                    "{r} shape {:?} differs from the authoritative spatial parameter {:?}",
                    s.value.shape(),
                    was_shape
                )]));
            }
            let binding = m.bindings().get(&nid).and_then(|b| b.get(&param)).cloned();
            let mut aliases = vec![(nid.clone(), param.clone())];
            let mut key: Option<String> = None;
            if let Some(b) = &binding {
                let D::Binding::Array { key: bkey } = b else {
                    return Err(AuthoringError::model_doc(vec![format!(
                        "{r} no longer names a writable array parameter"
                    )]));
                };
                let k =
                    e.get("array_key").filter(|v| crate::py::truthy(v)).map_or_else(|| bkey.clone(), py_str);
                if &k != bkey {
                    return Err(AuthoringError::model_doc(vec![
                        "array plan does not match authoritative binding".into(),
                    ]));
                }
                if !array_keys.insert(k.clone()) {
                    return Err(AuthoringError::model_doc(vec![format!(
                        "more than one design coordinate writes authoritative array {}",
                        repr(&Value::from(k))
                    )]));
                }
                let kref = &k;
                aliases = m
                    .bindings()
                    .iter()
                    .flat_map(|(anid, bs)| {
                        bs.iter().filter_map(move |(ap, bb)| match bb {
                            D::Binding::Array { key: kk } if kk == kref => Some((anid.clone(), ap.clone())),
                            _ => None,
                        })
                    })
                    .collect();
                for (anid, ap) in &aliases {
                    let shape = m
                        .node_table()
                        .get(anid)
                        .and_then(|n| n.param(ap))
                        .and_then(|p| p.as_ndarray().ok())
                        .map(|a| a.shape().to_vec())
                        .unwrap_or_default();
                    if shape != s.value.shape() {
                        return Err(AuthoringError::model_doc(vec![format!(
                            "shared array alias has inconsistent shape: {anid}.{ap}"
                        )]));
                    }
                }
                key = Some(bkey.clone());
            }
            let pv = ParamValue::Array(Arc::new(s.value.clone()));
            if key.is_none() {
                m.set_node_param(&nid, &param, &pv)?;
                touched.insert(nid.clone());
            } else {
                for (anid, ap) in &aliases {
                    pending_params.entry(anid.clone()).or_default().insert(ap.clone(), pv.clone());
                    touched.insert(anid.clone());
                }
                rebuild_needed = true;
            }
            if let Some(k) = &key {
                let doc_obj =
                    m.doc.as_object_mut().ok_or_else(|| runtime("model document is not an object"))?;
                let arrays_tbl = doc_obj.entry("arrays").or_insert_with(|| json!({}));
                let arrays_map =
                    arrays_tbl.as_object_mut().ok_or_else(|| runtime("arrays is not an object"))?;
                if persistent {
                    let digest = sha256_hex(&s.npy);
                    let filename = format!("implexity-array-sha256-{digest}.npy");
                    arrays_map.insert(k.clone(), arrays::sidecar_entry(&s.encoded, &filename));
                    if let Some(prior) = sidecars.get(&filename)
                        && *prior != s.npy
                    {
                        return Err(runtime("content-addressed sidecar identity collision"));
                    }
                    sidecars.insert(filename, s.npy.clone());
                } else {
                    arrays_map.insert(k.clone(), arrays::inline_entry(&s.encoded, &s.raw));
                }
            }
            let shared = m.parents().get(&nid).is_some_and(|p| p.len() > 1);
            moved.push(json!({"node": nid, "param": param, "was": num_json(&was.to_json()),
                "now": num_json(&pv.to_json()), "units": e["node_units"].clone(), "shared": shared,
                "spatial": true, "entries": s.value.size()}));
        }
        let mut broken = Vec::new();
        for nid in &touched {
            let Some(node) = m.node_table().get(nid) else { continue };
            let msg = match pending_params.get(nid) {
                Some(changes) => {
                    let mut params = node.params().clone();
                    for (k, v) in changes {
                        params.insert(k.clone(), v.clone());
                    }
                    D::node_invariant(&node.with_params(params))
                }
                None => D::node_invariant(node),
            };
            if !msg.is_empty() {
                broken.push(format!("nodes.{nid} ({}): {msg}", node.kind()));
            }
        }
        if !broken.is_empty() {
            return Err(AuthoringError::model_doc(broken));
        }
        let doc = m.to_doc()?;
        Ok((moved, doc, sidecars, rebuild_needed))
    }

    fn apply_values_report(
        model: &Model,
        source: &str,
        prepared: &PreparedValues,
        moved: &[Value],
        before: &(Option<String>, Option<String>),
        persist: bool,
    ) -> AResult<Value> {
        let set: Map<String, Value> = prepared.named.iter().map(|(k, v)| (k.clone(), jf(*v))).collect();
        Ok(json!({
            "source": source, "set": set,
            "node_params_set": prepared.direct.len(),
            "spatial_arrays_set": prepared.spatial.len(), "moved": moved,
            "persisted": persist,
            "structure_id": model.structure_id(),
            "structure_id_before": before.0,
            "recompiled": model.structure_id() != before.0,
            "content_id": model.content_id(),
            "content_id_before": before.1,
            "sha256": model.sha256()?, "parameters": model.parameter_table(),
        }))
    }


    pub fn detached_document_with_values(
        document: &Value,
        plan: &[Value],
        values: &BTreeMap<String, NdArray>,
        base_dir: Option<&Path>,
    ) -> AResult<(Value, Value)> {
        let base = canonical_copy(document)?;
        let mut candidate = D::build(&base, base_dir, None)?;
        let before = (candidate.structure_id(), candidate.content_id());
        let prepared = Self::prepare_model_values(plan, values)?;
        let (moved, mut candidate_doc, _unused, _rebuild) =
            Self::apply_prepared(&mut candidate, &prepared, false)?;
        let arrays_tbl = candidate_doc.get("arrays").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut inlined = Map::new();
        let mut problems = Vec::new();
        let mut keys: Vec<&String> = arrays_tbl.keys().collect();
        keys.sort();
        for key in keys {
            let entry = &arrays_tbl[key];
            let Some(raw) = entry_raw_bytes(entry, key, base_dir, &mut problems) else { continue };
            let metadata: Map<String, Value> = entry
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, _)| k.as_str() != "b64" && k.as_str() != "file")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            inlined.insert(key.clone(), arrays::inline_entry(&metadata, &raw));
        }
        if !problems.is_empty() {
            return Err(AuthoringError::model_doc(problems));
        }
        if let Some(o) = candidate_doc.as_object_mut() {
            if inlined.is_empty() {
                o.shift_remove("arrays");
            } else {
                o.insert("arrays".into(), Value::Object(inlined));
            }
        }
        let rebuilt = D::build(&candidate_doc, None, None)?;
        let standalone = rebuilt.to_doc()?;
        let encoded = doc_dumps(&standalone);
        if encoded.len() > MAX_PUBLIC_EPOCH_DOCUMENT_BYTES {
            return Err(AuthoringError::value(
                "ValueError",
                "self-contained optimization epoch document exceeds the 64 MiB public model-document bound",
            ));
        }
        let mut report = Self::apply_values_report(
            &rebuilt,
            "detached optimization epoch export",
            &prepared,
            &moved,
            &before,
            false,
        )?;
        if let Some(r) = report.as_object_mut() {
            r.insert("document_bytes".into(), json!(encoded.len()));
        }
        Ok((standalone, report))
    }


    pub fn apply_values(
        &self,
        plan: &[Value],
        values: &BTreeMap<String, NdArray>,
        persist: bool,
        source: &str,
    ) -> AResult<Value> {
        let prepared = Self::prepare_model_values(plan, values)?;
        let _g = self.authority.acquire();
        self.recover_pending()?;
        let m = self.require()?;
        let before_ids = (m.structure_id(), m.content_id());
        let before = canonical_copy(&m.to_doc()?)?;
        drop(m);
        if persist {
            let mut candidate = self.rebuild(&before)?;
            let (moved, candidate_doc, sidecars, _rebuild) =
                Self::apply_prepared(&mut candidate, &prepared, true)?;
            let mut created: Vec<CreatedSidecar> = Vec::new();
            let result = (|| -> AResult<Value> {
                for (filename, blob) in &sidecars {
                    if self.publish_content_sidecar(filename, blob)? {
                        created.push(CreatedSidecar { file: filename.clone(), sha256: sha256_hex(blob) });
                    }
                }
                let candidate = self.rebuild(&candidate_doc)?;
                let candidate_doc = candidate.to_doc()?;
                let report =
                    Self::apply_values_report(&candidate, source, &prepared, &moved, &before_ids, true)?;
                self.commit_model(candidate, &candidate_doc, &created, None)?;
                Ok(report)
            })();
            if result.is_err() && lock(&self.state).recovery.is_none() {
                self.cleanup_created_sidecars(&created)?;
            }
            return result;
        }
        let outcome = self.with_live_mut(|m| {
            let (moved, doc, _sidecars, rebuild_needed) = Self::apply_prepared(m, &prepared, false)?;
            if rebuild_needed {
                let mut rebuilt = D::build(&doc, Some(&self.dir), None)?;
                rebuilt.warnings.clone_from(&m.warnings);
                *m = rebuilt;
            }
            Self::apply_values_report(m, source, &prepared, &moved, &before_ids, false)
        });
        if outcome.is_err() {
            let restored = self.rebuild(&before)?;
            self.set_live(restored, None);
        }
        outcome
    }


    pub fn evaluate(&self, req: &Value) -> AResult<Value> {
        let m = self.require()?;
        evaluate_model(&m, req)
    }
}

fn num_json(v: &Value) -> Value {
    match crate::py::Arr::from_json(v) {
        Ok(a) if a.ndim() == 0 => a.data.first().map_or(Value::Null, |x| jf(*x)),
        Ok(a) => Value::Array(a.data.iter().map(|x| jf(*x)).collect()),
        Err(_) => Value::Null,
    }
}

fn entry_raw_bytes(
    entry: &Value,
    key: &str,
    base_dir: Option<&Path>,
    problems: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let where_ = format!("arrays.{key}");
    if let Some(b) = entry.get("b64") {
        return match arrays::b64decode_strict(&py_str(b)) {
            Ok(r) => Some(r),
            Err(msg) => {
                problems.push(format!("{where_}.b64 is not valid base64: {msg}"));
                None
            }
        };
    }
    let rel = entry.get("file").map(py_str).unwrap_or_default();
    let Some(dir) = base_dir else {
        problems.push(format!(
            "{where_} names the sidecar file {}, but this document was loaded without a directory to resolve it against (pass base_dir, or read the document from a path)",
            repr(&Value::from(rel))
        ));
        return None;
    };
    let path = dir.join(&rel);
    match std::fs::read(&path) {
        Ok(blob) => arrays::npy_payload(&blob, &where_, problems),
        Err(e) => {
            problems.push(format!("{where_}: cannot read {}: {e}", path.display()));
            None
        }
    }
}

#[must_use]
pub fn over_budget(n: usize) -> AuthoringError {
    AuthoringError::model_doc(vec![format!(
        "{n} points; the cap is {} (IMPLEXITY_IMPLICIT_MAX_POINTS).  This is a preview surface on a shared 8 GB cgroup, not an extraction: ask for a coarser grid, or POST /v1/body for a body",
        max_eval_points()
    )])
}

pub type EvalPoints = (Vec<[f64; 3]>, Option<[usize; 3]>, Option<Value>);


pub fn points_of(req: &Value) -> AResult<EvalPoints> {
    if let Some(raw) = req.get("points_mm") {
        if let Some(list) = raw.as_array()
            && list.len() > max_eval_points()
        {
            return Err(over_budget(list.len()));
        }
        let p = crate::py::Arr::from_json(raw)?;
        if p.ndim() != 2 || p.shape[1] != 3 {
            let shape: Vec<String> = p.shape.iter().map(ToString::to_string).collect();
            return Err(AuthoringError::model_doc(vec![format!(
                "points_mm must be [[x, y, z], ...] in mm, got shape [{}]",
                shape.join(", ")
            )]));
        }
        let pts = p.data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        return Ok((pts, None, None));
    }
    let Some(g) = req.get("grid").filter(|g| g.is_object()) else {
        return Err(AuthoringError::model_doc(vec![
            "evaluate takes either {\"points_mm\": [[x,y,z], ...]} or {\"grid\": {\"bbox_mm\": [[x0,y0,z0],[x1,y1,z1]], \"n\": [nx,ny,nz]}}".into(),
        ]));
    };
    let bb = g.get("bbox_mm");
    let n_raw = g.get("n").cloned().unwrap_or_else(|| json!([16, 16, 16]));
    let is_int = |v: &Value| v.is_i64() || v.is_u64();
    let n_list = if is_int(&n_raw) { json!([n_raw.clone(), n_raw.clone(), n_raw]) } else { n_raw };
    let bb_ok = bb
        .and_then(Value::as_array)
        .is_some_and(|b| b.len() == 2 && b.iter().all(|v| v.as_array().is_some_and(|a| a.len() == 3)));
    if !bb_ok {
        return Err(AuthoringError::model_doc(vec![
            "grid.bbox_mm must be [[x0,y0,z0],[x1,y1,z1]] in mm".into(),
        ]));
    }
    let n_ok = n_list.as_array().is_some_and(|a| {
        a.len() == 3 && a.iter().all(|v| is_int(v) && v.as_i64().is_some_and(|x| (2..=512).contains(&x)))
    });
    if !n_ok {
        return Err(AuthoringError::model_doc(vec!["grid.n must be three integers in [2, 512]".into()]));
    }
    let n: Vec<usize> = n_list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .filter_map(|x| usize::try_from(x).ok())
        .collect();
    let total = n[0] * n[1] * n[2];
    if total > max_eval_points() {
        return Err(over_budget(total));
    }
    let bbv = bb.and_then(Value::as_array).cloned().unwrap_or_default();
    let lo = crate::py::Arr::from_json(&bbv[0])?.data;
    let hi = crate::py::Arr::from_json(&bbv[1])?.data;
    if !(0..3).all(|a| hi[a] > lo[a]) {
        return Err(AuthoringError::model_doc(vec!["grid.bbox_mm must have hi > lo on every axis".into()]));
    }
    let ax: Vec<Vec<f64>> = (0..3).map(|a| implexity_mesh::numeric::linspace(lo[a], hi[a], n[a])).collect();
    let mut pts = Vec::with_capacity(total);
    for x in &ax[0] {
        for y in &ax[1] {
            for z in &ax[2] {
                pts.push([*x, *y, *z]);
            }
        }
    }
    let echo = json!({"bbox_mm": [lo.iter().map(|v| jf(*v)).collect::<Vec<_>>(), hi.iter().map(|v| jf(*v)).collect::<Vec<_>>()], "n": n});
    Ok((pts, Some([n[0], n[1], n[2]]), Some(echo)))
}

fn mode_of(mode: &str) -> Mode {
    if mode == "smooth" { Mode::Smooth } else { Mode::Exact }
}

pub fn evaluate_node(
    node: &NodeRef,
    pts: &[[f64; 3]],
    mode: &str,
    want: &str,
    smooth_r_mm: Option<f64>,
) -> AResult<(Vec<f64>, &'static str, &'static str)> {
    if !matches!(want, "native" | "auto" | "jax" | "numpy") {
        return Err(AuthoringError::model_doc(vec![format!(
            "evaluator must be native, auto, jax or numpy, got {}",
            repr(&Value::from(want))
        )]));
    }
    let opts = EvalOptions { mode: mode_of(mode), smooth_r_mm, ..EvalOptions::default() };
    let values = EV::eval_points(node, pts, &opts).map_err(|e| match e {
        GeometryError::Model(msg) => {
            AuthoringError::model_doc(vec![format!("this graph cannot be evaluated: {msg}")])
        }
        other => AuthoringError::Geometry(other),
    })?;
    Ok((values, "native", "implexity_geometry::eval::eval_points"))
}

#[must_use]
pub fn field_class_json(node: &NodeRef, mode: &str) -> Value {
    if let Ok(fc) = EV::field_class_of(node, mode_of(mode)) {
        return fc.as_json();
    }
    let kids: Result<Vec<FieldClass>, GeometryError> =
        node.children().iter().map(|c| c.op().field_class(c, &[])).collect();
    kids.and_then(|k| node.op().field_class(node, &k)).map_or(Value::Null, |fc| fc.as_json())
}


pub fn evaluate_model(m: &Model, req: &Value) -> AResult<Value> {
    let name = req
        .get("node")
        .filter(|v| crate::py::truthy(v))
        .or_else(|| m.doc.get("root"))
        .map(py_str)
        .unwrap_or_default();
    let node = m.node(&name)?;
    let mode_v = req.get("mode").cloned().unwrap_or_else(|| json!("exact"));
    let mode = match mode_v.as_str() {
        Some(s @ ("exact" | "smooth")) => s.to_string(),
        _ => {
            return Err(AuthoringError::model_doc(vec![format!(
                "mode must be 'exact' or 'smooth', got {}",
                repr(&mode_v)
            )]));
        }
    };
    let (pts, shape, grid) = points_of(req)?;
    let want = req.get("evaluator").map_or_else(|| "auto".to_string(), py_str);
    let smooth = match req.get("smooth_r_mm") {
        None | Some(Value::Null) => None,
        Some(v) => Some(py_float(v)?),
    };
    let t0 = std::time::Instant::now();
    let (values, used, detail) = evaluate_node(&node, &pts, &mode, &want, smooth)?;
    let secs = t0.elapsed().as_secs_f64();
    let nid = m.id_of(&node);
    let shared = nid.as_ref().and_then(|n| m.parents().get(n)).is_some_and(|p| p.len() > 1);
    let mut out = Map::new();
    out.insert("kind".into(), json!("implicit_evaluate"));
    out.insert("units".into(), json!("mm"));
    out.insert("node".into(), json!(nid));
    out.insert("requested".into(), json!(name));
    out.insert("kind_of_node".into(), json!(node.kind()));
    out.insert("mode".into(), json!(mode));
    out.insert("evaluator".into(), json!(used));
    out.insert("evaluator_detail".into(), json!(detail));
    out.insert("points".into(), json!(pts.len()));
    out.insert("seconds".into(), jf(py_round(secs, 4)));
    out.insert("structure_id".into(), json!(node.structure_id()));
    out.insert("content_id".into(), json!(node.content_id()));
    out.insert("shared".into(), json!(shared));
    out.insert("field_class".into(), field_class_json(&node, &mode));
    out.insert("values".into(), Value::Array(values.iter().map(|v| jf(*v)).collect()));
    if let Some(s) = shape {
        out.insert("shape".into(), json!(s));
        out.insert("grid".into(), grid.unwrap_or(Value::Null));
    }
    let lo = values
        .iter()
        .copied()
        .fold(f64::INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) });
    let hi = values
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) });
    out.insert("value_range".into(), json!([jf(lo), jf(hi)]));
    #[allow(clippy::cast_precision_loss)]
    let inside = values.iter().filter(|v| **v < 0.0).count() as f64 / values.len().max(1) as f64;
    out.insert("inside_fraction".into(), jf(inside));
    out.insert("aabb".into(), extent_of(&node));
    Ok(Value::Object(out))
}

fn box_record(lo: [f64; 3], hi: [f64; 3], known: bool, source: &str, note: &str) -> Value {
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diag = (0.0 + size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt();
    json!({
        "known": known, "unbounded": false, "source": source,
        "bbox_mm": [lo.map(jf), hi.map(jf)], "size_mm": size.map(jf),
        "center_mm": [jf(f64::midpoint(lo[0], hi[0])), jf(f64::midpoint(lo[1], hi[1])), jf(f64::midpoint(lo[2], hi[2]))],
        "diagonal_mm": jf(diag), "note": note,
    })
}

#[must_use]
pub fn extent_of(node: &NodeRef) -> Value {
    if let Some((lo, hi)) = EV::aabb_of(node) {
        return box_record(
            lo,
            hi,
            true,
            "eval.aabb_of",
            "a conservative bound of THIS node's zero set: the surface is inside it, and the bound is folded from the graph's own nodes rather than sampled",
        );
    }
    let mut acc: Option<Aabb> = None;
    for (_path, n) in node.walk() {
        let Some((blo, bhi)) = EV::aabb_of(&n) else { continue };
        acc = Some(match acc {
            None => (blo, bhi),
            Some((lo, hi)) => (
                [lo[0].min(blo[0]), lo[1].min(blo[1]), lo[2].min(blo[2])],
                [hi[0].max(bhi[0]), hi[1].max(bhi[1]), hi[2].max(bhi[2])],
            ),
        });
    }
    match acc {
        None => json!({
            "known": false, "unbounded": true, "source": null, "bbox_mm": null,
            "size_mm": null, "center_mm": null, "diagonal_mm": null,
            "note": "no node in this subtree has a known bound, so its zero set is unbounded -- which is the TRUE answer for a plane, a TPMS or a lattice, not a missing measurement.  Frame on the region you want to see; the field is defined everywhere",
        }),
        Some((lo, hi)) => box_record(
            lo,
            hi,
            false,
            "frame_hint",
            "this node's OWN bound is unknown (something beneath it is unbounded), so this is the union of the bounds that are known beneath it: somewhere to point a camera, and not a claim that the surface is inside it",
        ),
    }
}

pub struct DetachedModelView {
    model: Arc<Model>,
    lock: ReentrantLock,
}

impl DetachedModelView {
    #[must_use]
    pub fn new(model: Model) -> Self {
        Self { model: Arc::new(model), lock: ReentrantLock::new() }
    }


    pub fn from_document(document: &Value, base_dir: Option<&Path>) -> AResult<Self> {
        Ok(Self::new(D::build(document, base_dir, None)?))
    }

    #[must_use]
    pub fn require(&self) -> Arc<Model> {
        Arc::clone(&self.model)
    }

    pub fn live_lock(&self) -> ReentrantGuard<'_> {
        self.lock.acquire()
    }

    #[must_use]
    pub fn status(&self) -> Value {
        let m = &self.model;
        json!({"kind": "implicit_model", "units": "mm", "loaded": true, "detached": true,
            "structure_id": m.structure_id(), "content_id": m.content_id(),
            "aabb": m.root().map_or(Value::Null, |r| extent_of(&r))})
    }


    pub fn evaluate(&self, req: &Value) -> AResult<Value> {
        evaluate_model(&self.model, req)
    }
}

#[derive(Clone)]
pub struct HistoryStore(pub Arc<ModelManager>);

impl implexity_geometry::engineering_history::HistoryModels for HistoryStore {
    fn dir(&self) -> PathBuf {
        self.0.dir.clone()
    }

    fn status(&self) -> implexity_geometry::GResult<Value> {
        self.0.status().map_err(to_geometry)
    }

    fn snapshot(&self) -> implexity_geometry::GResult<Value> {
        self.0.snapshot().map_err(to_geometry)
    }

    fn put(&self, doc: &Value) -> implexity_geometry::GResult<()> {
        self.0.put(doc).map(|_| ()).map_err(to_geometry)
    }
}

#[must_use]
pub fn to_geometry(e: AuthoringError) -> GeometryError {
    match e {
        AuthoringError::Geometry(g) => g,
        AuthoringError::Problems { problems, .. } => GeometryError::ModelDoc(problems),
        AuthoringError::Io(m) => GeometryError::Io(m),
        other => GeometryError::Value(other.to_string()),
    }
}
