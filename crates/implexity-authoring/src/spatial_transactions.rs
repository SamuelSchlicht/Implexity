// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
