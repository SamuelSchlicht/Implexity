// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::error::{AResult, AuthoringError};
use crate::field_brush::{BrushGrid, BrushStroke, FieldBrushTransaction};
use crate::py::{py_eq, uuid_hex};
use crate::surface_authoring::{ObjectValues, SurfaceHit, author_at_surface};

fn err(message: &str) -> AuthoringError {
    AuthoringError::runtime("SpatialTransactionError", message)
}

struct FieldState {
    base_revision: Value,
    tx: FieldBrushTransaction,
    sequence: i64,
    last_document: Option<Value>,
}

pub type DocFn = Box<dyn Fn() -> AResult<Value> + Send + Sync>;
pub type DocSink = Box<dyn Fn(&Value) -> AResult<Value> + Send + Sync>;

pub struct SpatialEditManager {
    get_document: DocFn,
    validate_document: DocSink,
    commit_document: DocSink,
    preview_document: Option<DocSink>,
    field: Mutex<HashMap<String, FieldState>>,
}

fn revision_of(doc: &Value) -> Value {
    doc.get("revision").or_else(|| doc.get("version")).cloned().unwrap_or(Value::Null)
}

impl SpatialEditManager {
    #[must_use]
    pub fn new(
        get_document: DocFn,
        validate_document: DocSink,
        commit_document: DocSink,
        preview_document: Option<DocSink>,
    ) -> Self {
        Self {
            get_document,
            validate_document,
            commit_document,
            preview_document,
            field: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, FieldState>> {
        self.field.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }


    pub fn begin_field(
        &self,
        node_id: &str,
        parameter: &str,
        grid: BrushGrid,
        lower: Option<f64>,
        upper: Option<f64>,
        share_policy: &str,
    ) -> AResult<Value> {
        let mut field = self.lock();
        let doc = (self.get_document)()?;
        let revision = revision_of(&doc);
        let token = uuid_hex();
        let tx = FieldBrushTransaction::new(&doc, node_id, parameter, grid, lower, upper, share_policy)?;
        let shape = tx.baseline.shape.clone();
        field.insert(
            token.clone(),
            FieldState { base_revision: revision.clone(), tx, sequence: 0, last_document: None },
        );
        Ok(json!({"token": token, "base_revision": revision, "shape": shape}))
    }


    pub fn preview_field(&self, token: &str, sequence: i64, strokes: &[BrushStroke]) -> AResult<Value> {
        let mut field = self.lock();
        let state = field.get_mut(token).ok_or_else(|| err("Unknown field transaction"))?;
        if sequence <= state.sequence {
            return Ok(json!({"token": token, "sequence": state.sequence, "stale": true}));
        }
        let current = (self.get_document)()?;
        if !py_eq(&revision_of(&current), &state.base_revision) {
            return Err(err("Authoritative document changed during field gesture"));
        }
        state.tx.set_strokes(strokes)?;
        let mutation = state.tx.preview_document()?;
        (self.validate_document)(&mutation.document)?;
        state.sequence = sequence;
        state.last_document = Some(mutation.document.clone());
        if let Some(p) = &self.preview_document {
            p(&mutation.document)?;
        }
        Ok(json!({"token": token, "sequence": sequence, "stale": false,
            "payload_sha256": mutation.payload_sha256, "shape": mutation.shape}))
    }


    pub fn commit_field(&self, token: &str) -> AResult<Value> {
        let mut state = self.lock().remove(token).ok_or_else(|| err("Unknown field transaction"))?;
        let document = match state.last_document.take() {
            Some(d) => d,
            None => state.tx.commit()?.document,
        };
        (self.validate_document)(&document)?;
        (self.commit_document)(&document)
    }


    pub fn cancel_field(&self, token: &str) -> AResult<Value> {
        let mut state = self.lock().remove(token).ok_or_else(|| err("Unknown field transaction"))?;
        let original =
            if state.tx.closed() { state.tx.document_at_begin.clone() } else { state.tx.cancel()? };
        if let Some(p) = &self.preview_document {
            p(&original)?;
        }
        Ok(original)
    }
}

pub struct EngineeringAuthoringManager {
    get_problem: DocFn,
    validate_problem: DocSink,
    commit_problem: DocSink,
    lock: Mutex<()>,
}

impl EngineeringAuthoringManager {
    #[must_use]
    pub fn new(get_problem: DocFn, validate_problem: DocSink, commit_problem: DocSink) -> Self {
        Self { get_problem, validate_problem, commit_problem, lock: Mutex::new(()) }
    }


    pub fn place(
        &self,
        hit: &SurfaceHit,
        object_type: &str,
        radius: f64,
        values: &ObjectValues,
    ) -> AResult<Value> {
        let _g = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (problem, patch, obj) =
            author_at_surface(&(self.get_problem)()?, hit, object_type, radius, values)?;
        (self.validate_problem)(&problem)?;
        let result = (self.commit_problem)(&problem)?;
        Ok(json!({"result": result, "patch": patch, "object": obj}))
    }
}
