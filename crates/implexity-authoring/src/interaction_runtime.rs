// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use implexity_geometry::lattice::component_field::{component_catalogue, split_component_id};

use crate::engineering_glyphs::{EngineeringGlyph, available_kinds, upsert_glyph_problem};
use crate::error::{AResult, AuthoringError};
use crate::field_interaction::{
    BrushSample, ControlLattice, DeformationCage, FieldBrushGesture, GridGeometry, deform_spatial_field,
    flatten_json, promote_cage_to_shape_coordinate, read_spatial_field, write_control_lattice,
    write_spatial_field,
};
use crate::interaction_transactions::{InteractionTransaction, InteractionTransactionManager};
use crate::py::{
    Arr, canonical_unicode, get, jf, jfs, np_sum, obj_mut, py_float, py_int, py_str, repr, setdefault_list,
    setdefault_obj, sha256_hex, truthy, uuid_hex,
};
use crate::spatial_selection::{
    SpatialSelection, as_cell_region_definition, find_spatial_selection, write_spatial_selection,
};
use crate::surface_regions::{
    as_surface_region_definition, migrate_surface_patches, normalize_surface_patch,
    normalize_surface_patch_set,
};
use crate::sync::lock;

pub(crate) fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::runtime("InteractionRuntimeError", message)
}

fn key_err(k: &str) -> AuthoringError {
    AuthoringError::Key(repr(&Value::from(k)))
}

pub trait RuntimeStores {

    fn get_document(&self) -> AResult<Value>;

    fn set_document(&self, document: &Value) -> AResult<Value>;

    fn get_problem(&self) -> AResult<Value>;

    fn set_problem(&self, problem: &Value) -> AResult<Value>;

    fn get_revision(&self) -> AResult<String>;
    fn record_event(&self, _kind: &str, _label: &str, _details: &Value) -> Option<AResult<Value>> {
        None
    }
    fn refine_surface(&self, _request: &Value) -> Option<AResult<Value>> {
        None
    }
    fn get_spatial_field(&self, _field_id: &str) -> Option<AResult<Value>> {
        None
    }

    fn run_authority_transaction(
        &self,
        _operation: &str,
        callback: &mut dyn FnMut() -> AResult<Value>,
    ) -> AResult<Value> {
        callback()
    }
    fn prepare_problem_revision(&self, _before: &Value, _revised: &Value) -> Option<AResult<(Value, usize)>> {
        None
    }
    fn external_edit_active(&self) -> bool {
        false
    }

    fn run_history_read(&self, callback: &mut dyn FnMut() -> AResult<Value>) -> AResult<Value> {
        callback()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RtState {
    pub json: Value,
    pub values: Option<Vec<f64>>,
    pub preview_values: Option<Vec<f64>>,
}

impl RtState {
    fn of(json: Value) -> Self {
        Self { json, values: None, preview_values: None }
    }
}

#[derive(Clone, Debug)]
struct HistoryRecord {
    label: String,
    before: Value,
    after: Value,
    entry_id: String,
    origin: String,
}

#[derive(Clone, Debug)]
struct FieldState {
    id: String,
    values: Vec<f64>,
    grid: GridGeometry,
    lower: Option<f64>,
    upper: Option<f64>,
    protected_masks: Vec<(String, Value)>,
    metadata: Value,
}

#[derive(Clone, Debug)]
enum Gesture {
    RigidMove,
    Sculpt { field_id: String },
    Brush { gesture: Box<FieldBrushGesture>, field_metadata: Value },
    Selection { baseline: Box<SpatialSelection>, current: Box<SpatialSelection> },
    Glyph { baseline: Box<EngineeringGlyph> },
    Cage { baseline: Box<DeformationCage>, field: Option<Box<FieldState>> },
    Lattice { baseline: Box<ControlLattice>, field: Option<Box<FieldState>> },
    Patch { baseline: Value },
}

#[derive(Default)]
struct Inner {
    gestures: HashMap<String, Gesture>,
    history_before: HashMap<String, Value>,
    undo: Vec<HistoryRecord>,
    redo: Vec<HistoryRecord>,
    sequence: u64,
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub lower: Value,
    pub upper: Value,
    pub protected_masks: BTreeMap<String, Vec<bool>>,
    pub metadata: Value,
}

pub type AuthoritativeField = (String, Vec<f64>, GridGeometry, FieldInfo, Value);

fn grid_from_field(field: &Value) -> AResult<GridGeometry> {
    let grid_raw = get(field, "grid").cloned().unwrap_or_else(|| json!({}));
    let source =
        if grid_raw.is_object() && grid_raw.get("origin").is_some() && grid_raw.get("basis").is_some() {
            Some(grid_raw.clone())
        } else if grid_raw.is_object() {
            grid_raw.get("bounds_mm").or_else(|| get(field, "bounds_mm")).cloned()
        } else {
            get(field, "bounds_mm").cloned()
        };
    let Some(source) = source.filter(|v| !v.is_null()) else {
        return Err(err("spatial field requires an exact grid registration"));
    };
    let shape: Vec<i64> = get(field, "shape")
        .ok_or_else(|| key_err("shape"))?
        .as_array()
        .ok_or_else(|| crate::py::type_error("shape must be a list"))?
        .iter()
        .map(py_int)
        .collect::<AResult<_>>()?;
    GridGeometry::from_values(&shape, &source)
}

fn protected_masks(field: &Value, size: usize) -> AResult<BTreeMap<String, Vec<bool>>> {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(m)) = get(field, "protected_masks").filter(|v| truthy(v)) {
        for (name, raw) in m {
            let value =
                if raw.is_object() { raw.get("values").cloned().unwrap_or(Value::Null) } else { raw.clone() };
            let (_, mask) = crate::py::bool_array(&value)?;
            if mask.len() != size {
                return Err(err(format!(
                    "protected mask {} does not match field shape",
                    repr(&Value::from(name.clone()))
                )));
            }
            out.insert(name.clone(), mask);
        }
    }
    Ok(out)
}

fn f64_bytes(values: &[f64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn summary(shape: &[usize], values: &[f64]) -> Value {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    json!({"shape": shape, "minimum": jf(min), "maximum": jf(max),
        "mean": jf(np_sum(values) / values.len() as f64)})
}

fn opt_f64(v: &Value) -> AResult<Option<f64>> {
    if v.is_null() { Ok(None) } else { py_float(v).map(Some) }
}

fn mask_pairs(masks: &BTreeMap<String, Vec<bool>>) -> Vec<(String, Value)> {
    masks
        .iter()
        .map(|(k, m)| (k.clone(), Value::Array(m.iter().map(|b| Value::Bool(*b)).collect())))
        .collect()
}

pub struct InteractionRuntime {
    pub transactions: InteractionTransactionManager<RtState>,
    inner: Mutex<Inner>,
    history_epoch: String,
}

const KINDS: [&str; 8] = [
    "geometry_sculpt",
    "field_brush",
    "spatial_selection",
    "control_lattice",
    "deformation_cage",
    "glyph",
    "surface_patch",
    "rigid_move",
];

impl InteractionRuntime {
    #[must_use]
    pub fn new(timeout_seconds: f64) -> Self {
        Self {
            transactions: InteractionTransactionManager::new(timeout_seconds),
            inner: Mutex::new(Inner::default()),
            history_epoch: uuid_hex(),
        }
    }


    pub fn bundle(stores: &dyn RuntimeStores) -> AResult<Value> {
        Ok(json!({"document": stores.get_document()?, "problem": stores.get_problem()?}))
    }

    #[must_use]
    pub fn bundle_id(bundle: &Value) -> String {
        sha256_hex(canonical_unicode(bundle).as_bytes())
    }

    fn under_authority(
        stores: &dyn RuntimeStores,
        operation: &str,
        callback: &mut dyn FnMut() -> AResult<Value>,
    ) -> AResult<Value> {
        stores.run_authority_transaction(operation, callback)
    }


    pub fn history_state(&self, stores: &dyn RuntimeStores) -> AResult<Value> {
        stores.run_history_read(&mut || self.history_state_raw(stores))
    }

    #[must_use]
    pub fn empty_history_state(&self) -> Value {
        let inner = lock(&self.inner);
        json!({
            "schema": "implexity-manual-command-history/1",
            "epoch": self.history_epoch,
            "sequence": inner.sequence,
            "revision": format!("{}:{}:no-model", self.history_epoch, inner.sequence),
            "state_id": null,
            "document_revision": null,
            "model_loaded": false,
            "undo": 0, "redo": 0,
            "stored_undo": inner.undo.len(), "stored_redo": inner.redo.len(),
            "undo_head": null, "redo_head": null,
            "can_undo": false, "can_redo": false,
            "stale": false, "active_transaction": false,
            "persistence": "service_session",
        })
    }

    fn history_state_raw(&self, stores: &dyn RuntimeStores) -> AResult<Value> {
        let current = Self::bundle_id(&Self::bundle(stores)?);
        let (undo_last, redo_last, n_undo, n_redo, sequence) = {
            let inner = lock(&self.inner);
            (
                inner.undo.last().cloned(),
                inner.redo.last().cloned(),
                inner.undo.len(),
                inner.redo.len(),
                inner.sequence,
            )
        };
        let head = |r: &Option<HistoryRecord>, after: bool| -> Value {
            match r {
                None => Value::Null,
                Some(rec) => {
                    let side = if after { &rec.after } else { &rec.before };
                    json!({"entry_id": rec.entry_id, "label": rec.label, "origin": rec.origin,
                        "matches_current": Self::bundle_id(side) == current})
                }
            }
        };
        let undo_head = head(&undo_last, true);
        let redo_head = head(&redo_last, false);
        let active = self.transactions.active_count() > 0 || stores.external_edit_active();
        let matches = |h: &Value| h.get("matches_current").and_then(Value::as_bool).unwrap_or(false);
        let undo_ok = !undo_head.is_null() && matches(&undo_head) && !active;
        let redo_ok = !redo_head.is_null() && matches(&redo_head) && !active;
        let stale =
            (!undo_head.is_null() && !matches(&undo_head)) || (!redo_head.is_null() && !matches(&redo_head));
        Ok(json!({
            "schema": "implexity-manual-command-history/1",
            "epoch": self.history_epoch,
            "sequence": sequence,
            "revision": format!("{}:{}:{}", self.history_epoch, sequence, current),
            "state_id": current,
            "document_revision": stores.get_revision()?,
            "model_loaded": true,
            "undo": if undo_ok { n_undo } else { 0 },
            "redo": if redo_ok { n_redo } else { 0 },
            "stored_undo": n_undo, "stored_redo": n_redo,
            "undo_head": undo_head, "redo_head": redo_head,
            "can_undo": undo_ok, "can_redo": redo_ok,
            "stale": stale, "active_transaction": active,
            "persistence": "service_session",
        }))
    }


    pub fn record_external(
        &self,
        stores: &dyn RuntimeStores,
        before: &Value,
        label: &str,
        origin: &str,
    ) -> AResult<Value> {
        let after = Self::bundle(stores)?;
        if Self::bundle_id(before) != Self::bundle_id(&after) {
            let mut inner = lock(&self.inner);
            if inner.undo.last().is_some_and(|r| Self::bundle_id(&r.after) != Self::bundle_id(before)) {
                inner.undo.clear();
            }
            inner.undo.push(HistoryRecord {
                label: label.to_string(),
                before: before.clone(),
                after,
                entry_id: uuid_hex(),
                origin: origin.to_string(),
            });
            let n = inner.undo.len();
            if n > 100 {
                inner.undo.drain(..n - 100);
            }
            inner.redo.clear();
            inner.sequence += 1;
        }
        let mut state = self.history_state(stores)?;
        if let Some(m) = state.as_object_mut() {
            m.insert("label".into(), Value::from(label));
        }
        Ok(state)
    }


    pub fn record_mutation(
        &self,
        stores: &dyn RuntimeStores,
        callback: &mut dyn FnMut() -> AResult<Value>,
        label: &str,
        origin: &str,
    ) -> AResult<Value> {
        if self.transactions.active_count() > 0 {
            return Err(err("finish or cancel the active viewport transaction first"));
        }
        let before = Self::bundle(stores)?;
        let mut result = callback()?;
        let state = self.record_external(stores, &before, label, origin)?;
        if let Some(m) = result.as_object_mut() {
            m.insert("history".into(), state);
        }
        Ok(result)
    }

    #[must_use]
    pub fn default_field_id(document: &Value) -> String {
        let topology =
            crate::py::path_obj(document, &["meta", "implexity", "topology"]).cloned().unwrap_or_default();
        let fid = topology
            .get("array_key")
            .filter(|v| truthy(v))
            .or_else(|| topology.get("field_id").filter(|v| truthy(v)))
            .map_or_else(String::new, py_str)
            .trim()
            .to_string();
        if !fid.is_empty() {
            return fid;
        }
        let mut r =
            topology.get("ref").filter(|v| truthy(v)).map_or_else(String::new, py_str).trim().to_string();
        if let Some(rest) = r.strip_prefix("model/") {
            r = rest.to_string();
        } else if let Some(rest) = r.strip_prefix("model:") {
            r = rest.to_string();
            if !r.contains(':') {
                match document.get("root").and_then(Value::as_str) {
                    Some(root) if !root.is_empty() => r = format!("{root}:{r}"),
                    _ => return String::new(),
                }
            }
        }
        if let Some((node_id, parameter)) = r.rsplit_once(':') {
            let spec = document
                .get("nodes")
                .and_then(|n| n.get(node_id))
                .and_then(|n| n.get("params"))
                .and_then(|p| p.get(parameter));
            if let Some(a) = spec.filter(|s| s.is_object()).and_then(|s| s.get("array")).filter(|a| truthy(a))
            {
                return py_str(a);
            }
        }
        String::new()
    }


    pub fn authoritative_field(
        &self,
        stores: &dyn RuntimeStores,
        request: &Value,
    ) -> AResult<AuthoritativeField> {
        let mut field_id = get(request, "field_id").map_or_else(String::new, py_str).trim().to_string();
        if field_id.is_empty() {
            field_id = Self::default_field_id(&stores.get_document()?);
        }
        if field_id.is_empty() {
            return Err(err("an authoritative field_id is required"));
        }
        let (values, grid, mut masks, metadata, bounds, mut identity) = if let Some(result) =
            stores.get_spatial_field(&field_id)
        {
            let authoritative = result?;
            if !authoritative.is_object() {
                return Err(err(format!(
                    "authoritative spatial field {} is unavailable",
                    repr(&Value::from(field_id.clone()))
                )));
            }
            let parsed = (|| -> AResult<(GridGeometry, Vec<f64>, BTreeMap<String, Vec<bool>>)> {
                let grid = grid_from_field(&authoritative)?;
                let values = Arr::from_json(authoritative.get("values").ok_or_else(|| key_err("values"))?)?;
                if values.size() != grid.size() {
                    return Err(crate::py::value_error(format!(
                        "cannot reshape array of size {} into shape {}",
                        values.size(),
                        crate::field_interaction::shape_repr(&grid.shape)
                    )));
                }
                let masks = protected_masks(&authoritative, grid.size())?;
                Ok((grid, values.data, masks))
            })();
            let (grid, values, masks) = parsed.map_err(|e| {
                err(format!(
                    "authoritative spatial field {} is malformed: {e}",
                    repr(&Value::from(field_id.clone()))
                ))
            })?;
            let metadata =
                authoritative.get("metadata").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
            let bounds =
                authoritative.get("bounds").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
            let identity =
                authoritative.get("identity").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
            (values, grid, masks, metadata, bounds, identity)
        } else {
            let document = stores.get_document()?;
            let decoded = read_spatial_field(&document, &field_id).map_err(|e| {
                err(format!(
                    "authoritative spatial field {} is unavailable: {e}",
                    repr(&Value::from(field_id.clone()))
                ))
            })?;
            let source_entry = decoded.entry.clone().filter(Value::is_object).unwrap_or_else(|| json!({}));
            let mut metadata = source_entry.as_object().cloned().unwrap_or_default();
            if let Some(Value::Object(fm)) = &decoded.metadata {
                for (k, v) in fm {
                    metadata.insert(k.clone(), v.clone());
                }
            }
            let metadata = Value::Object(metadata);
            let masks_raw = metadata.get("protected_masks").cloned().unwrap_or_else(|| json!({}));
            let masks = protected_masks(&json!({"protected_masks": masks_raw}), decoded.grid.size())?;
            let bounds = metadata.get("bounds").cloned().unwrap_or_else(|| json!({}));
            let payload = source_entry
                .get("sha256")
                .filter(|v| truthy(v))
                .map_or_else(|| sha256_hex(&f64_bytes(&decoded.values)), py_str);
            let identity = json!({
                "document_revision": stores.get_revision()?,
                "registration_id": decoded.grid.registration.to_wire().get("registration_id").cloned().unwrap_or(Value::Null),
                "payload_sha256": payload,
            });
            (decoded.values, decoded.grid, masks, metadata, bounds, identity)
        };
        let hold = crate::geometry_holds::field_held_mask(&stores.get_document()?, &field_id, &grid.shape)?;
        if hold.iter().any(|h| *h) {
            masks.insert("manual_and_optimization_hold".into(), hold);
        }
        if !identity.is_object() {
            identity = json!({});
        }
        let idm = identity.as_object_mut().ok_or_else(|| err("identity must be an object"))?;
        if !idm.get("document_revision").is_some_and(truthy) {
            idm.insert("document_revision".into(), Value::from(stores.get_revision()?));
        }
        if !idm.get("registration_id").is_some_and(truthy) {
            idm.insert(
                "registration_id".into(),
                grid.registration.to_wire().get("registration_id").cloned().unwrap_or(Value::Null),
            );
        }
        if !idm.get("payload_sha256").is_some_and(truthy) {
            idm.insert("payload_sha256".into(), Value::from(sha256_hex(&f64_bytes(&values))));
        }
        let supplied = get(request, "field_identity")
            .filter(|v| truthy(v))
            .or_else(|| get(request, "field").filter(|f| f.is_object()).and_then(|f| f.get("identity")));
        if let Some(Value::Object(s)) = supplied {
            for key in ["registration_id", "payload_sha256"] {
                if let Some(v) = s.get(key).filter(|v| !v.is_null())
                    && py_str(v) != idm.get(key).map_or_else(|| "None".into(), py_str)
                {
                    return Err(err(format!("spatial field {key} is stale")));
                }
            }
        }
        let info = FieldInfo {
            lower: bounds.get("lower").cloned().unwrap_or(Value::Null),
            upper: bounds.get("upper").cloned().unwrap_or(Value::Null),
            protected_masks: masks,
            metadata: if metadata.is_object() { metadata } else { json!({}) },
        };
        Ok((field_id, values, grid, info, identity))
    }


    pub fn field(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        if !get(request, "field_id").is_some_and(truthy) {
            let document = stores.get_document()?;
            let base = Self::default_field_id(&document);
            if !base.is_empty()
                && let Some(components) = component_catalogue(&document, &base)?
            {
                return Ok(json!({"schema": "implexity-component-selection/1", "selection_required": true,
                        "components": components}));
            }
        }
        let (field_id, values, grid, info, identity) = self.authoritative_field(stores, request)?;
        let document = stores.get_document()?;
        let mut interaction = crate::py::path_obj(&document, &["meta", "implexity", "interaction"])
            .cloned()
            .unwrap_or_default();
        if interaction.is_empty() {
            interaction =
                crate::py::path_obj(&document, &["extensions", "interaction"]).cloned().unwrap_or_default();
        }
        let mut region_names: HashMap<String, String> = HashMap::new();
        let problem = stores.get_problem()?;
        for region in problem.get("regions").and_then(Value::as_array).cloned().unwrap_or_default() {
            let selector = region.get("selector").filter(|s| s.is_object());
            if let Some(sel) =
                selector.filter(|s| s.get("kind").and_then(Value::as_str) == Some("cell_selection"))
            {
                let name = region
                    .get("name")
                    .filter(|v| truthy(v))
                    .or_else(|| region.get("id").filter(|v| truthy(v)))
                    .map_or_else(|| "Current selection".to_string(), py_str);
                region_names.insert(sel.get("id").map_or_else(String::new, py_str), name);
            }
        }
        let mut selections = Vec::new();
        let mut stale = Vec::new();
        let mut invalid = Vec::new();
        for item in
            interaction.get("spatial_selections").and_then(Value::as_array).cloned().unwrap_or_default()
        {
            if !item.is_object() || item.get("field_id").map_or_else(|| "None".into(), py_str) != field_id {
                continue;
            }
            let item_identity = item.get("field_identity").cloned().unwrap_or_else(|| json!({}));
            let valid = item_identity.is_object()
                && ["registration_id", "payload_sha256"].iter().all(|k| {
                    item_identity.get(*k).map_or_else(String::new, py_str)
                        == identity.get(*k).map_or_else(String::new, py_str)
                });
            let id = item.get("id").map_or_else(String::new, py_str);
            if !valid {
                stale.push(id);
                continue;
            }
            let parsed = SpatialSelection::from_mapping(
                &item,
                &values,
                grid.clone(),
                &info.protected_masks,
                Some(&identity),
            );
            let Ok(sel) = parsed else {
                invalid.push(id);
                continue;
            };
            let mut s = sel.serialise(true);
            if let Some(name) = region_names.get(&id) {
                s["region_name"] = Value::from(name.clone());
            } else if let Some(n) = item.get("region_name").filter(|v| truthy(v)) {
                s["region_name"] = Value::from(py_str(n));
            }
            selections.push(s);
        }
        let mut active = interaction.get("active_spatial_selection").cloned().unwrap_or(Value::Null);
        if !selections.iter().any(|s| s.get("id") == Some(&active)) {
            active = Value::Null;
        }
        let masks: Map<String, Value> = info
            .protected_masks
            .iter()
            .map(|(k, m)| (k.clone(), Value::Array(m.iter().map(|b| Value::Bool(*b)).collect())))
            .collect();
        Ok(json!({
            "sculpt_selections": crate::geometry_selection::catalogue(&document, &field_id, &grid)?,
            "schema": "implexity-spatial-field/2",
            "field_id": field_id,
            "shape": grid.shape,
            "values": jfs(&values),
            "grid": grid.serialise(),
            "lower": info.lower, "upper": info.upper,
            "protected_masks": masks,
            "identity": identity,
            "selections": selections,
            "active_selection_id": active,
            "stale_selection_ids": stale,
            "invalid_selection_ids": invalid,
        }))
    }

    fn constrained_field_preview(document: &Value, field: &FieldState, values: &[f64]) -> AResult<Vec<f64>> {
        let prepared = write_spatial_field(
            document,
            &field.id,
            values,
            &field.grid,
            None,
            None,
            Some(&field.protected_masks),
        )?;
        Ok(read_spatial_field(&prepared, &field.id)?.values)
    }

    fn field_state(prepared: &AuthoritativeField) -> AResult<FieldState> {
        let (field_id, values, grid, info, identity) = prepared;
        let mut metadata = info.metadata.as_object().cloned().unwrap_or_default();
        metadata.insert("identity".into(), identity.clone());
        Ok(FieldState {
            id: field_id.clone(),
            values: values.clone(),
            grid: grid.clone(),
            lower: opt_f64(&info.lower)?,
            upper: opt_f64(&info.upper)?,
            protected_masks: mask_pairs(&info.protected_masks),
            metadata: Value::Object(metadata),
        })
    }


    pub fn begin(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        let kind = get(request, "kind").map_or_else(String::new, py_str).trim().to_lowercase();
        if !KINDS.contains(&kind.as_str()) {
            return Err(err(format!("unsupported interaction kind: {}", repr(&Value::from(kind)))));
        }
        let revision = stores.get_revision()?;
        let before_bundle = Self::bundle(stores)?;
        let mut snapshot = if kind == "glyph" || kind == "surface_patch" {
            before_bundle["problem"].clone()
        } else {
            before_bundle["document"].clone()
        };
        let mut prepared_move = None;
        if kind == "rigid_move" {
            snapshot = before_bundle.clone();
            let policy = get(request, "attachment_policy").map_or_else(|| "attached".to_string(), py_str);
            let node_id = get(request, "node_id").filter(|v| !v.is_null()).map(py_str);
            prepared_move = Some(crate::rigid_translation::translate_bundle(
                &snapshot["document"],
                &snapshot["problem"],
                &json!([0.0, 0.0, 0.0]),
                &policy,
                node_id.as_deref(),
            )?);
        }
        if (kind == "glyph" || kind == "surface_patch")
            && snapshot.get("schema").map_or_else(String::new, py_str).ends_with("differentiable-problem/1")
        {
            return Err(err(
                "Open CAE setup and create a validated region-capable problem first. The current case-bound problem cannot save surface conditions.",
            ));
        }
        let mut requested_field_id =
            get(request, "field_id").map_or_else(String::new, py_str).trim().to_string();
        if requested_field_id.is_empty()
            && kind == "deformation_cage"
            && let Some(cage) = get(request, "cage").filter(|c| c.is_object())
        {
            requested_field_id = cage
                .get("target")
                .filter(|t| truthy(t))
                .and_then(|t| t.get("field_id"))
                .map_or_else(String::new, py_str)
                .trim()
                .to_string();
        }
        let needs_field = ["geometry_sculpt", "field_brush", "spatial_selection"].contains(&kind.as_str())
            || ((kind == "control_lattice" || kind == "deformation_cage") && !requested_field_id.is_empty());
        let mut field_request = request.clone();
        if !requested_field_id.is_empty()
            && let Some(m) = field_request.as_object_mut()
        {
            m.insert("field_id".into(), Value::from(requested_field_id.clone()));
        }
        let prepared_field =
            if needs_field { Some(self.authoritative_field(stores, &field_request)?) } else { None };
        if (kind == "control_lattice" || kind == "deformation_cage")
            && get(request, "field").is_some_and(|f| !f.is_null())
            && prepared_field.is_none()
        {
            return Err(err("browser-supplied field values are not authoritative"));
        }
        let mut prepared_selection = None;
        if kind == "spatial_selection" {
            let (field_id, values, grid, info, identity) =
                prepared_field.as_ref().ok_or_else(|| err("missing field"))?;
            if grid.registration.centering == "node" {
                return Err(err(
                    "Native control nodes are not physical cells. Use geometry-following regions for boundary conditions; component views support field brushes.",
                ));
            }
            let encoded = get(request, "selection").filter(|v| truthy(v)).cloned().or_else(|| {
                find_spatial_selection(
                    &snapshot,
                    get(request, "selection_id").filter(|v| truthy(v)).map(py_str).as_deref(),
                )
            });
            if let Some(enc) = encoded.filter(Value::is_object) {
                if enc.get("field_id").map_or_else(String::new, py_str) != *field_id {
                    return Err(err("the persisted selection belongs to a different spatial field"));
                }
                let enc_identity =
                    enc.get("field_identity").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
                if !enc_identity.is_object() {
                    return Err(err("the persisted selection lacks field identity evidence"));
                }
                for key in ["registration_id", "payload_sha256"] {
                    if enc_identity.get(key).map_or_else(String::new, py_str)
                        != identity.get(key).map_or_else(String::new, py_str)
                    {
                        return Err(err(format!("the persisted selection has a stale field {key}")));
                    }
                }
                prepared_selection = Some(SpatialSelection::from_mapping(
                    &enc,
                    values,
                    grid.clone(),
                    &info.protected_masks,
                    Some(identity),
                )?);
            } else {
                let sid = get(request, "selection_id").filter(|v| !v.is_null()).map(py_str);
                prepared_selection = Some(SpatialSelection::new(
                    field_id,
                    values,
                    grid.clone(),
                    sid.as_deref(),
                    0,
                    None,
                    &info.protected_masks,
                    Some(identity),
                    &[],
                    0.5,
                )?);
            }
        }
        if kind == "geometry_sculpt" {
            let field_id = &prepared_field.as_ref().ok_or_else(|| err("missing field"))?.0;
            crate::geometry_sculpt::verify_topology_target(&snapshot, field_id)?;
            if split_component_id(field_id)?.is_none() {
                crate::geometry_sculpt::verify_scalar_semantics(&snapshot, field_id)?;
            }
            if !get(request, "preview_geometry").is_none_or(Value::is_boolean) {
                return Err(err("preview_geometry must be Boolean"));
            }
            let count = get(request, "preview_count").cloned().unwrap_or(json!(32));
            if !(count.is_i64() || count.is_u64()) || !count.as_i64().is_some_and(|c| (8..=48).contains(&c)) {
                return Err(err("preview_count must be an integer in 8..48"));
            }
        }
        let mut metadata = request.as_object().cloned().unwrap_or_default();
        if let Some(pf) = &prepared_field {
            metadata.insert("field_id".into(), Value::from(pf.0.clone()));
        }
        let info = self.transactions.begin(
            &kind,
            &revision,
            RtState::of(snapshot.clone()),
            Value::Object(metadata),
        )?;
        let tx_id = py_str(&info["transaction_id"]);
        lock(&self.inner).history_before.insert(tx_id.clone(), before_bundle);
        let (gesture, initial) = match kind.as_str() {
            "rigid_move" => (Gesture::RigidMove, RtState::of(prepared_move.unwrap_or(Value::Null))),
            "geometry_sculpt" => {
                let field_id = prepared_field.as_ref().map(|p| p.0.clone()).unwrap_or_default();
                if split_component_id(&field_id)?.is_none() {
                    crate::geometry_sculpt::verify_scalar_semantics(&snapshot, &field_id)?;
                }
                (
                    Gesture::Sculpt { field_id },
                    RtState::of(json!({"document": snapshot, "evidence": {"changed_control_values": 0}})),
                )
            }
            "field_brush" => {
                let (_, values, grid, finfo, identity) =
                    prepared_field.as_ref().ok_or_else(|| err("missing field"))?;
                let gesture = FieldBrushGesture::new(
                    &Arr::new(grid.shape.to_vec(), values.clone()),
                    grid.clone(),
                    opt_f64(&finfo.lower)?,
                    opt_f64(&finfo.upper)?,
                    &mask_pairs(&finfo.protected_masks),
                )?;
                let mut fm = finfo.metadata.as_object().cloned().unwrap_or_default();
                fm.insert("identity".into(), identity.clone());
                let initial = RtState {
                    json: json!({"gesture": gesture.serialise(), "document": snapshot}),
                    values: Some(gesture.preview()?),
                    preview_values: None,
                };
                (Gesture::Brush { gesture: Box::new(gesture), field_metadata: Value::Object(fm) }, initial)
            }
            "spatial_selection" => {
                let sel = prepared_selection.ok_or_else(|| err("missing selection"))?;
                let initial = RtState::of(json!({"selection": sel.serialise(true), "document": snapshot}));
                (Gesture::Selection { baseline: Box::new(sel.clone()), current: Box::new(sel) }, initial)
            }
            "glyph" => {
                let baseline =
                    EngineeringGlyph::from_mapping(get(request, "glyph").ok_or_else(|| key_err("glyph"))?)?;
                let current = EngineeringGlyph::from_mapping(&baseline.serialise()?)?;
                let initial = RtState::of(
                    json!({"glyph": current.serialise()?, "problem": upsert_glyph_problem(&snapshot, &current)?}),
                );
                (Gesture::Glyph { baseline: Box::new(baseline) }, initial)
            }
            "deformation_cage" => {
                let baseline = match get(request, "cage").filter(|c| c.is_object()) {
                    Some(c) => DeformationCage::from_mapping(c)?,
                    None => DeformationCage::create(
                        get(request, "bounds_mm").ok_or_else(|| key_err("bounds_mm"))?,
                        shape_arg(get(request, "shape"))?,
                        get(request, "target").ok_or_else(|| key_err("target"))?,
                    )?,
                };
                let field = prepared_field.as_ref().map(Self::field_state).transpose()?.map(Box::new);
                let current = DeformationCage::from_mapping(&baseline.serialise())?;
                let initial = RtState::of(json!({"cage": current.serialise(), "document": snapshot}));
                (Gesture::Cage { baseline: Box::new(baseline), field }, initial)
            }
            "control_lattice" => {
                let baseline = match get(request, "lattice").filter(|c| c.is_object()) {
                    Some(v) => ControlLattice::from_mapping(v)?,
                    None => ControlLattice::create(
                        get(request, "bounds_mm").ok_or_else(|| key_err("bounds_mm"))?,
                        shape_arg(get(request, "shape"))?,
                    )?,
                };
                let field = prepared_field.as_ref().map(Self::field_state).transpose()?.map(Box::new);
                let current = ControlLattice::from_mapping(&baseline.serialise())?;
                let initial = RtState::of(json!({"lattice": current.serialise(), "document": snapshot}));
                (Gesture::Lattice { baseline: Box::new(baseline), field }, initial)
            }
            _ => {
                let baseline = normalize_surface_patch(
                    get(request, "patch").ok_or_else(|| key_err("patch"))?,
                    None,
                    true,
                )?;
                let initial =
                    RtState::of(json!({"patch": baseline, "problem": migrate_surface_patches(&snapshot)?}));
                (Gesture::Patch { baseline }, initial)
            }
        };
        lock(&self.inner).gestures.insert(tx_id.clone(), gesture);
        self.transactions.prime(&tx_id, initial.clone(), 0)?;
        let mut info = self.transactions.describe(&tx_id)?;
        if let Some(m) = info.as_object_mut() {
            if kind == "glyph" {
                m.insert("entity_ids".into(), json!({"glyph_id": initial.json["glyph"]["id"]}));
            } else if kind == "surface_patch" {
                m.insert("entity_ids".into(), json!({"patch_id": initial.json["patch"]["id"]}));
            }
        }
        Ok(info)
    }


    pub fn preview(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        let _ = stores;
        let tx_id = py_str(get(request, "transaction_id").ok_or_else(|| key_err("transaction_id"))?);
        let sequence = py_int(get(request, "sequence").ok_or_else(|| key_err("sequence"))?)?;
        let operation = match get(request, "operation") {
            None => json!({}),
            Some(Value::Object(m)) => Value::Object(m.clone()),
            Some(other) => {
                return Err(crate::py::type_error(format!(
                    "'{}' object is not a mapping",
                    crate::py::type_name(other)
                )));
            }
        };
        let description = self.transactions.describe(&tx_id)?;
        let kind = py_str(&description["kind"]);
        let mut apply = |snapshot: RtState, payload: &Value, metadata: &Value| -> AResult<RtState> {
            self.apply_preview(&tx_id, &kind, snapshot, payload, metadata)
        };
        let (desc, state, accepted) = self.transactions.preview(&tx_id, sequence, &operation, &mut apply)?;
        let mut result = desc.as_object().cloned().unwrap_or_default();
        let mut state_json = state.json.as_object().cloned().unwrap_or_default();
        if let Some(values) = &state.values {
            let shape: Vec<usize> = match lock(&self.inner).gestures.get(&tx_id) {
                Some(Gesture::Brush { gesture, .. }) => gesture.grid.shape.to_vec(),
                _ => vec![values.len()],
            };
            state_json.insert("field_summary".into(), summary(&shape, values));
        }
        state_json.shift_remove("preview_values");
        if kind == "geometry_sculpt" {
            state_json.shift_remove("document");
        }
        result.insert("state".into(), Value::Object(state_json));
        result.insert("accepted".into(), Value::Bool(accepted));
        Ok(Value::Object(result))
    }

    fn apply_preview(
        &self,
        tx_id: &str,
        kind: &str,
        snapshot: RtState,
        payload: &Value,
        metadata: &Value,
    ) -> AResult<RtState> {
        let snapshot = snapshot.json;
        let gesture = lock(&self.inner).gestures.get(tx_id).cloned().ok_or_else(|| key_err(tx_id))?;
        match (kind, gesture) {
            ("rigid_move", _) => {
                let policy =
                    get(metadata, "attachment_policy").map_or_else(|| "attached".to_string(), py_str);
                let node_id = get(metadata, "node_id").filter(|v| !v.is_null()).map(py_str);
                Ok(RtState::of(crate::rigid_translation::translate_bundle(
                    &snapshot["document"],
                    &snapshot["problem"],
                    get(payload, "delta_mm").ok_or_else(|| key_err("delta_mm"))?,
                    &policy,
                    node_id.as_deref(),
                )?))
            }
            ("geometry_sculpt", Gesture::Sculpt { field_id }) => {
                let (document, evidence) =
                    crate::geometry_sculpt::apply_sculpt(&snapshot, &field_id, payload)?;
                let mut state = json!({"document": document, "evidence": evidence});
                if get(metadata, "preview_geometry").is_some_and(truthy) {
                    let count = get(metadata, "preview_count").cloned().unwrap_or(json!(32));
                    state["preview_field"] =
                        crate::geometry_sculpt::preview_field(&state["document"], &count)?;
                }
                Ok(RtState::of(state))
            }
            ("field_brush", Gesture::Brush { mut gesture, field_metadata }) => {
                let samples: Vec<Value> = match get(payload, "samples") {
                    Some(Value::Array(a)) => a.clone(),
                    Some(other) => {
                        return Err(crate::py::type_error(format!(
                            "'{}' object is not iterable",
                            crate::py::type_name(other)
                        )));
                    }
                    None => vec![get(payload, "sample").cloned().unwrap_or(Value::Null)],
                };
                gesture.samples.clear();
                for s in samples.iter().filter(|s| !s.is_null()) {
                    gesture.samples.push(BrushSample::from_mapping(s)?);
                }
                let values = gesture.preview()?;
                let topology = implexity_geometry::topology_monitor::monitor_if_applicable(
                    &gesture.baseline,
                    &values,
                    &gesture.grid.shape,
                    field_metadata.as_object(),
                )
                .map_err(|e| crate::py::value_error(e.0))?;
                let json =
                    json!({"gesture": gesture.serialise(), "document": snapshot, "topology": topology});
                if let Some(Gesture::Brush { gesture: g, .. }) = lock(&self.inner).gestures.get_mut(tx_id) {
                    g.samples.clone_from(&gesture.samples);
                }
                Ok(RtState { json, values: Some(values), preview_values: None })
            }
            ("spatial_selection", Gesture::Selection { baseline, .. }) => {
                let current = baseline.apply(payload).map_err(|e| err(e.to_string()))?;
                let state = json!({"selection": current.serialise(true), "document": snapshot});
                if let Some(Gesture::Selection { current: c, .. }) = lock(&self.inner).gestures.get_mut(tx_id)
                {
                    **c = current;
                }
                Ok(RtState::of(state))
            }
            ("control_lattice" | "deformation_cage", g) => {
                let (original, field, cage) = match g {
                    Gesture::Cage { baseline, field } => (baseline.lattice.clone(), field, Some(baseline)),
                    Gesture::Lattice { baseline, field } => (*baseline, field, None),
                    _ => return Err(err("invalid interaction state")),
                };
                let mut working = ControlLattice::from_mapping(&original.serialise())?;
                let index: Vec<i64> = get(payload, "index")
                    .ok_or_else(|| key_err("index"))?
                    .as_array()
                    .ok_or_else(|| crate::py::type_error("index must be a sequence"))?
                    .iter()
                    .map(py_int)
                    .collect::<AResult<_>>()?;
                if index.len() != 3 {
                    return Err(crate::field_interaction::err(
                        "control-point index lies outside the lattice",
                    ));
                }
                let axes: Vec<i64> = match get(payload, "symmetry_axes") {
                    None => Vec::new(),
                    Some(Value::Array(a)) => a.iter().map(py_int).collect::<AResult<_>>()?,
                    Some(_) => return Err(crate::py::type_error("symmetry_axes must be a sequence")),
                };
                working.move_control(
                    [index[0], index[1], index[2]],
                    get(payload, "delta_mm").ok_or_else(|| key_err("delta_mm"))?,
                    py_float(get(payload, "influence_radius").unwrap_or(&json!(0.0)))?,
                    &get(payload, "falloff").map_or_else(|| "smoothstep".to_string(), py_str),
                    &axes,
                )?;
                let mut json_state = if let Some(c) = &cage {
                    let mut current = DeformationCage::from_mapping(&c.serialise())?;
                    current.lattice = working.clone();
                    json!({"cage": current.serialise(), "document": snapshot})
                } else {
                    json!({"lattice": working.serialise(), "document": snapshot})
                };
                let mut preview_values = None;
                if let Some(f) = field {
                    let warped = deform_spatial_field(&f.values, &f.grid, &working)?;
                    let warped = Self::constrained_field_preview(&snapshot, &f, &warped)?;
                    json_state["field_summary"] = summary(&f.grid.shape, &warped);
                    let md = f.metadata.as_object().cloned();
                    json_state["topology"] = implexity_geometry::topology_monitor::monitor_if_applicable(
                        &f.values,
                        &warped,
                        &f.grid.shape,
                        md.as_ref(),
                    )
                    .map_err(|e| crate::py::value_error(e.0))?
                    .unwrap_or(Value::Null);
                    preview_values = Some(warped);
                }
                Ok(RtState { json: json_state, values: None, preview_values })
            }
            ("glyph", Gesture::Glyph { baseline }) => {
                let mut glyph = EngineeringGlyph::from_mapping(&baseline.serialise()?)?;
                let handle = get(payload, "handle").map_or_else(|| "anchor".to_string(), py_str);
                match handle.as_str() {
                    "anchor" => {
                        if let Some(a) = get(payload, "anchor_mm").filter(|v| !v.is_null()) {
                            let mut patch = normalize_surface_patch(&glyph.patch, None, true)?;
                            let anchor = floats(a)?;
                            patch["anchor_mm"] = jfs(&anchor);
                            patch["point"] = jfs(&anchor);
                            patch["center"] = jfs(&anchor);
                            if let Some(n) = get(payload, "normal").filter(|v| !v.is_null()) {
                                let normal = floats(n)?;
                                patch["normal"] = jfs(&normal);
                                patch["surface_normal"] = jfs(&normal);
                            }
                            glyph.patch = normalize_surface_patch(&patch, None, true)?;
                        } else {
                            glyph.drag_anchor(
                                get(payload, "delta_mm").ok_or_else(|| key_err("delta_mm"))?,
                                None,
                            )?;
                        }
                    }
                    "direction" => glyph.drag_direction(
                        get(payload, "delta").ok_or_else(|| key_err("delta"))?,
                        py_float(get(payload, "gain").unwrap_or(&json!(1.0)))?,
                    )?,
                    "magnitude" => {
                        let snap = get(payload, "snap").filter(|v| !v.is_null()).map(py_float).transpose()?;
                        glyph.drag_magnitude(
                            py_float(get(payload, "delta").ok_or_else(|| key_err("delta"))?)?,
                            py_float(get(payload, "sensitivity").unwrap_or(&json!(1.0)))?,
                            snap,
                        )?;
                    }
                    "radius" => {
                        let snap =
                            get(payload, "snap_mm").filter(|v| !v.is_null()).map(py_float).transpose()?;
                        glyph.drag_radius(
                            py_float(get(payload, "delta_mm").ok_or_else(|| key_err("delta_mm"))?)?,
                            1.0e-6,
                            snap,
                        )?;
                    }
                    _ => {
                        return Err(err(format!("unsupported glyph handle: {}", repr(&Value::from(handle)))));
                    }
                }
                Ok(RtState::of(
                    json!({"glyph": glyph.serialise()?, "problem": upsert_glyph_problem(&snapshot, &glyph)?}),
                ))
            }
            ("surface_patch", Gesture::Patch { baseline }) => {
                if get(payload, "patches").is_some_and(truthy) {
                    let current = normalize_surface_patch_set(&json!({
                        "kind": "surface_patch_set",
                        "combination": get(payload, "combination").cloned().unwrap_or(json!("union")),
                        "patches": payload["patches"],
                    }))?;
                    return Ok(RtState::of(
                        json!({"patch_set": current, "problem": migrate_surface_patches(&snapshot)?}),
                    ));
                }
                let mut patch = normalize_surface_patch(&baseline, None, true)?;
                let anchor: Vec<f64> = if let Some(a) = get(payload, "anchor_mm").filter(|v| !v.is_null()) {
                    Arr::from_json(a)?.data
                } else {
                    let a = Arr::from_json(&patch["anchor_mm"])?;
                    let d = Arr::from_opt(get(payload, "delta_mm"))?;
                    if d.shape != a.shape {
                        return Err(crate::py::value_error("operands could not be broadcast together"));
                    }
                    a.data.iter().zip(&d.data).map(|(x, y)| x + y).collect()
                };
                patch["anchor_mm"] = jfs(&anchor);
                patch["point"] = jfs(&anchor);
                patch["center"] = jfs(&anchor);
                if let Some(n) = get(payload, "normal").filter(|v| !v.is_null()) {
                    let normal = floats(n)?;
                    patch["normal"] = jfs(&normal);
                    patch["surface_normal"] = jfs(&normal);
                }
                let patch = normalize_surface_patch(&patch, None, true)?;
                Ok(RtState::of(json!({"patch": patch, "problem": migrate_surface_patches(&snapshot)?})))
            }
            _ => Err(err("invalid interaction state")),
        }
    }


    pub fn refine(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        let Some(result) = stores.refine_surface(request) else {
            return Err(err("exact implicit-surface refinement is unavailable"));
        };
        let result = result?;
        if !result.is_object() {
            return Err(err("exact refinement returned no surface hit"));
        }
        let hit = result.get("hit").cloned().unwrap_or_else(|| result.clone());
        let identity = hit.get("model_identity");
        let clip = hit.get("clip_evidence");
        let identity_ok = identity.is_some_and(|i| {
            i.is_object()
                && ["structure_id", "content_id", "revision"]
                    .iter()
                    .any(|k| i.get(*k).is_some_and(|v| !v.is_null()))
        });
        if hit.get("exact") != Some(&Value::Bool(true)) || !identity_ok {
            return Err(err("surface refinement lacks exact model-identity evidence"));
        }
        let clip_ok = clip.is_some_and(|c| {
            c.is_object()
                && c.get("active").is_some_and(Value::is_boolean)
                && !c.get("hit_on_clip_cap").is_some_and(truthy)
        });
        if !clip_ok {
            return Err(err("surface refinement lacks valid clipping evidence"));
        }
        Ok(json!({"hit": hit}))
    }

    fn restore(stores: &dyn RuntimeStores, before: &Value) {
        let _ = stores.set_document(&before["document"]);
        let _ = stores.set_problem(&before["problem"]);
    }

    fn commit_state(
        &self,
        stores: &dyn RuntimeStores,
        tx_id: &str,
        kind: &str,
        state: &RtState,
        tx: &InteractionTransaction<RtState>,
    ) -> AResult<Value> {
        let before = lock(&self.inner).history_before.get(tx_id).cloned().unwrap_or(Value::Null);
        let gesture = lock(&self.inner).gestures.get(tx_id).cloned().ok_or_else(|| key_err(tx_id))?;
        let snap = &tx.snapshot.json;
        match (kind, gesture) {
            ("rigid_move", _) => {
                if Self::bundle_id(&Self::bundle(stores)?) != Self::bundle_id(&before) {
                    return Err(err("the model or engineering conditions changed during Move"));
                }
                let applied = (|| -> AResult<(Value, Value)> {
                    Ok((
                        stores.set_document(&state.json["document"])?,
                        stores.set_problem(&state.json["problem"])?,
                    ))
                })();
                let (d, p) = match applied {
                    Ok(v) => v,
                    Err(e) => {
                        stores.set_document(&before["document"])?;
                        stores.set_problem(&before["problem"])?;
                        return Err(e);
                    }
                };
                Ok(json!({"result": {"document": d, "problem": p},
                    "evidence": state.json.get("evidence").cloned().unwrap_or_else(|| json!({})),
                    "final_sequence": tx.latest_applied}))
            }
            ("geometry_sculpt", Gesture::Sculpt { field_id }) => {
                let (mut problem, rebound) = crate::geometry_freeze::rebind_matching_maps(
                    &before["problem"],
                    snap,
                    &state.json["document"],
                )?;
                if Self::bundle_id(&Self::bundle(stores)?) != Self::bundle_id(&before) {
                    return Err(err("the model or engineering conditions changed during sculpt"));
                }
                let mut plans_rebuilt = 0;
                if rebound > 0
                    && let Some(r) = stores.prepare_problem_revision(&before["problem"], &problem)
                {
                    let (p, n) = r?;
                    problem = p;
                    plans_rebuilt = n;
                }
                let applied = (|| -> AResult<Value> {
                    let result = stores.set_document(&state.json["document"])?;
                    if rebound > 0 {
                        stores.set_problem(&problem)?;
                    }
                    Ok(result)
                })();
                let result = match applied {
                    Ok(r) => r,
                    Err(e) => {
                        stores.set_document(&before["document"])?;
                        stores.set_problem(&before["problem"])?;
                        return Err(e);
                    }
                };
                let mut evidence =
                    state.json.get("evidence").and_then(Value::as_object).cloned().unwrap_or_default();
                evidence.insert("geometry_declarations_rebound".into(), json!(rebound));
                evidence.insert("physics_plans_rebuilt".into(), json!(plans_rebuilt));
                Ok(
                    json!({"result": result, "evidence": evidence, "field_id": field_id, "final_sequence": tx.latest_applied}),
                )
            }
            ("field_brush", Gesture::Brush { gesture, .. }) => {
                let field_id = tx.metadata.get("field_id").map_or_else(|| "None".into(), py_str);
                let values = state.values.clone().ok_or_else(|| key_err("values"))?;
                let masks: Vec<(String, Value)> = gesture
                    .protected_masks
                    .iter()
                    .map(|(k, m)| (k.clone(), Value::Array(m.iter().map(|b| Value::Bool(*b)).collect())))
                    .collect();
                let document = write_spatial_field(
                    snap,
                    &field_id,
                    &values,
                    &gesture.grid,
                    gesture.lower,
                    gesture.upper,
                    Some(&masks),
                )?;
                let persisted = stores.set_document(&document)?;
                Ok(json!({"result": persisted, "field_id": field_id, "final_sequence": tx.latest_applied}))
            }
            ("spatial_selection", Gesture::Selection { current, .. }) => {
                let selection = SpatialSelection::from_mapping(
                    &state.json["selection"],
                    &current.values,
                    current.grid.clone(),
                    &current.protected_masks,
                    Some(&current.field_identity),
                )?;
                let region_name = tx.metadata.get("region_name").filter(|v| truthy(v)).map(py_str);
                let document = write_spatial_selection(snap, &selection, region_name.as_deref())?;
                let persisted_document = stores.set_document(&document)?;
                let mut region = Value::Null;
                let mut persisted_problem = Value::Null;
                if tx.metadata.get("save_region").is_none_or(truthy) {
                    let mut problem = before["problem"].clone();
                    let region_id = tx.metadata.get("region_id").filter(|v| truthy(v)).map(py_str);
                    region =
                        as_cell_region_definition(&selection, region_name.as_deref(), region_id.as_deref());
                    {
                        let regions = setdefault_list(obj_mut(&mut problem)?, "regions")?;
                        let rid = py_str(&region["id"]);
                        regions.retain(|r| r.get("id").map_or_else(|| "None".into(), py_str) != rid);
                        regions.push(region.clone());
                    }
                    match stores.set_problem(&problem) {
                        Ok(p) => persisted_problem = p,
                        Err(e) => {
                            Self::restore(stores, &before);
                            return Err(e);
                        }
                    }
                }
                let mut entity_ids = json!({"selection_id": selection.id});
                if !region.is_null() {
                    entity_ids["region_id"] = region["id"].clone();
                }
                Ok(json!({
                    "result": {"document": persisted_document, "problem": persisted_problem},
                    "selection": find_spatial_selection(&document, Some(&selection.id)),
                    "entities": if region.is_null() { json!({}) } else { json!({"region": region}) },
                    "entity_ids": entity_ids,
                    "final_sequence": tx.latest_applied,
                }))
            }
            ("control_lattice", Gesture::Lattice { field, .. }) => {
                let mut document = snap.clone();
                let encoded = state.json["lattice"].clone();
                {
                    let root = obj_mut(&mut document)?;
                    let interaction = if root.get("schema").is_some_and(truthy) {
                        setdefault_obj(
                            setdefault_obj(setdefault_obj(root, "meta")?, "implexity")?,
                            "interaction",
                        )?
                    } else {
                        setdefault_obj(setdefault_obj(root, "extensions")?, "interaction")?
                    };
                    let lattices = setdefault_list(interaction, "control_lattices")?;
                    lattices.retain(|i| i.get("id") != encoded.get("id"));
                    lattices.push(encoded.clone());
                }
                if let Some(f) = field {
                    let values = match &state.preview_values {
                        Some(v) => v.clone(),
                        None => deform_spatial_field(
                            &f.values,
                            &f.grid,
                            &ControlLattice::from_mapping(&encoded)?,
                        )?,
                    };
                    document = write_spatial_field(
                        &document,
                        &f.id,
                        &values,
                        &f.grid,
                        f.lower,
                        f.upper,
                        Some(&f.protected_masks),
                    )?;
                }
                let persisted = stores.set_document(&document)?;
                Ok(
                    json!({"result": persisted, "entity_ids": {"lattice_id": encoded["id"]}, "final_sequence": tx.latest_applied}),
                )
            }
            ("deformation_cage", Gesture::Cage { field, .. }) => {
                let cage = DeformationCage::from_mapping(&state.json["cage"])?;
                let mut document = write_control_lattice(snap, &cage)?;
                if let Some(f) = field {
                    let values = match &state.preview_values {
                        Some(v) => v.clone(),
                        None => deform_spatial_field(&f.values, &f.grid, &cage.lattice)?,
                    };
                    document = write_spatial_field(
                        &document,
                        &f.id,
                        &values,
                        &f.grid,
                        f.lower,
                        f.upper,
                        Some(&f.protected_masks),
                    )?;
                }
                let persisted = stores.set_document(&document)?;
                Ok(
                    json!({"result": persisted, "entity_ids": {"cage_id": cage.serialise()["id"]}, "final_sequence": tx.latest_applied}),
                )
            }
            ("glyph", _) => {
                let glyph = EngineeringGlyph::from_mapping(&state.json["glyph"])?;
                let persisted = stores.set_problem(&upsert_glyph_problem(snap, &glyph)?)?;
                Ok(json!({"result": persisted, "entities": glyph.to_problem_objects(None, None)?,
                    "entity_ids": {"glyph_id": glyph.serialise()?["id"]}, "final_sequence": tx.latest_applied}))
            }
            ("surface_patch", _) => {
                let mut problem = snap.clone();
                let current = state
                    .json
                    .get("patch_set")
                    .or_else(|| state.json.get("patch"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let region = as_surface_region_definition(&current, None, None)?;
                {
                    let regions = setdefault_list(obj_mut(&mut problem)?, "regions")?;
                    regions.retain(|r| r.get("id") != region.get("id"));
                    regions.push(region.clone());
                }
                let persisted = stores.set_problem(&problem)?;
                Ok(
                    json!({"result": persisted, "entities": {"region": region}, "entity_ids": {"region_id": region["id"]},
                    "final_sequence": tx.latest_applied}),
                )
            }
            _ => Err(err("invalid interaction state")),
        }
    }


    pub fn commit(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        let tx_id = py_str(get(request, "transaction_id").ok_or_else(|| key_err("transaction_id"))?);
        let description = self.transactions.describe(&tx_id)?;
        let kind = py_str(&description["kind"]);
        let expected = get(request, "final_sequence").filter(|v| !v.is_null()).map(py_int).transpose()?;
        let mut run = || -> AResult<Value> {
            let live = stores.get_revision()?;
            let mut commit = |state: RtState, tx: &InteractionTransaction<RtState>| {
                self.commit_state(stores, &tx_id, &kind, &state, tx)
            };
            let mut result = self.transactions.commit(&tx_id, &live, expected, &mut commit)?;
            let before = lock(&self.inner).history_before.get(&tx_id).cloned();
            if let Some(before) = before {
                let after = Self::bundle(stores)?;
                if Self::bundle_id(&before) != Self::bundle_id(&after) {
                    let label = match kind.as_str() {
                        "rigid_move" => "Move geometry",
                        "geometry_sculpt" => "Geometry sculpt / protection",
                        "field_brush" => "Topology brush",
                        "spatial_selection" => "Selection and region",
                        "control_lattice" => "Control lattice",
                        "deformation_cage" => "Deformation cage",
                        "glyph" => "Engineering condition",
                        "surface_patch" => "Surface region",
                        _ => "Manual interaction",
                    };
                    let origin = description["metadata"]
                        .get("_client_origin")
                        .map_or_else(|| "interaction".into(), py_str);
                    let history = self.record_external(stores, &before, label, &origin)?;
                    if let Some(m) = result.as_object_mut() {
                        m.insert("history".into(), history);
                    }
                }
            }
            let label = match kind.as_str() {
                "geometry_sculpt" => "Manual geometry sculpt / protection edit",
                "field_brush" => "Manual topology/field brush edit",
                "spatial_selection" => "Manual exact selection/region edit",
                "control_lattice" => "Manual control-lattice edit",
                "deformation_cage" => "Manual deformation-cage edit",
                "glyph" => "Engineering condition edited",
                "surface_patch" => "Engineering surface region edited",
                _ => "Manual engineering interaction",
            };
            let _ = stores.record_event(
                "manual_interaction",
                label,
                &json!({"kind": kind, "transaction_id": tx_id}),
            );
            Ok(result)
        };
        let outcome = Self::under_authority(stores, "commit a manual interaction", &mut run);
        if outcome.is_err() {
            let _ = self.transactions.cancel(&tx_id);
        }
        {
            let mut inner = lock(&self.inner);
            inner.gestures.remove(&tx_id);
            inner.history_before.remove(&tx_id);
        }
        outcome
    }


    pub fn cancel(&self, request: &Value) -> AResult<Value> {
        let tx_id = py_str(get(request, "transaction_id").ok_or_else(|| key_err("transaction_id"))?);
        let result = self.transactions.cancel(&tx_id);
        let mut inner = lock(&self.inner);
        inner.gestures.remove(&tx_id);
        inner.history_before.remove(&tx_id);
        result.map(|s| s.json)
    }


    pub fn history(&self, stores: &dyn RuntimeStores, action: &str, request: &Value) -> AResult<Value> {
        let action = action.to_lowercase();
        let req = request.as_object().cloned().unwrap_or_default();
        let allowed = ["expected_history_revision", "expected_entry_id"];
        if req.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(err("unknown manual-history request fields"));
        }
        if !req.is_empty()
            && (req.len() != 2
                || allowed
                    .iter()
                    .any(|k| !req.get(*k).and_then(Value::as_str).is_some_and(|s| !s.is_empty())))
        {
            return Err(err("both expected_history_revision and expected_entry_id are required"));
        }
        let mut change = || -> AResult<Value> {
            if self.transactions.active_count() > 0 {
                return Err(err("finish or cancel the active gesture before changing manual history"));
            }
            if action != "undo" && action != "redo" {
                return Err(err("history action must be undo or redo"));
            }
            let record = {
                let inner = lock(&self.inner);
                let source = if action == "undo" { &inner.undo } else { &inner.redo };
                source.last().cloned()
            };
            let Some(record) = record else {
                return Err(err(format!("there is no manual interaction to {action}")));
            };
            if !req.is_empty() {
                let observed = self.history_state(stores)?;
                if req["expected_history_revision"] != observed["revision"]
                    || req["expected_entry_id"].as_str() != Some(record.entry_id.as_str())
                {
                    return Err(err(
                        "manual history changed in another client; refresh its current head before undo/redo",
                    ));
                }
            }
            let current = Self::bundle(stores)?;
            let (expected, replacement) = if action == "undo" {
                (&record.after, &record.before)
            } else {
                (&record.before, &record.after)
            };
            if Self::bundle_id(&current) != Self::bundle_id(expected) {
                return Err(err("manual history is stale because the document or problem changed"));
            }
            let applied = (|| -> AResult<(Value, Value)> {
                Ok((
                    stores.set_document(&replacement["document"])?,
                    stores.set_problem(&replacement["problem"])?,
                ))
            })();
            let (d, p) = match applied {
                Ok(v) => v,
                Err(e) => {
                    Self::restore(stores, &current);
                    return Err(e);
                }
            };
            let (n_undo, n_redo) = {
                let mut inner = lock(&self.inner);
                let rec = if action == "undo" { inner.undo.pop() } else { inner.redo.pop() };
                if let Some(rec) = rec {
                    if action == "undo" {
                        inner.redo.push(rec);
                    } else {
                        inner.undo.push(rec);
                    }
                }
                inner.sequence += 1;
                (inner.undo.len(), inner.redo.len())
            };
            Ok(json!({
                "action": action, "label": record.label, "entry_id": record.entry_id,
                "undo": n_undo, "redo": n_redo,
                "history": self.history_state(stores)?,
                "result": {"document": d, "problem": p},
            }))
        };
        Self::under_authority(stores, "change manual interaction history", &mut change)
    }


    pub fn undo(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        self.history(stores, "undo", request)
    }


    pub fn redo(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        self.history(stores, "redo", request)
    }


    pub fn promote_cage(&self, stores: &dyn RuntimeStores, request: &Value) -> AResult<Value> {
        let cage_id = get(request, "cage_id").map_or_else(String::new, py_str).trim().to_string();
        let mut promote = || -> AResult<Value> {
            if cage_id.is_empty() {
                return Err(err("cage_id is required"));
            }
            if self.transactions.active_count() > 0 {
                return Err(err("finish or cancel the active gesture before promoting a cage"));
            }
            let base = stores.get_revision()?;
            let coordinate = get(request, "coordinate").filter(|v| truthy(v)).map(py_str);
            let document =
                promote_cage_to_shape_coordinate(&stores.get_document()?, &cage_id, coordinate.as_deref())?;
            if stores.get_revision()? != base {
                return Err(err("document changed while the cage promotion was prepared"));
            }
            let persisted = stores.set_document(&document)?;
            Ok(json!({"result": persisted, "entity_ids": {"cage_id": cage_id}, "promoted": true}))
        };
        Self::under_authority(stores, "promote a deformation cage", &mut promote)
    }

    #[must_use]
    pub fn capabilities() -> Value {
        json!({
            "geometry_sculpt": crate::geometry_sculpt::capabilities(),
            "schema": "implexity-interaction-capabilities/1",
            "transactions": ["begin", "preview", "commit", "cancel", "refine", "history_state", "undo", "redo"],
            "kinds": ["rigid_move", "geometry_sculpt", "field_brush", "spatial_selection", "control_lattice", "deformation_cage", "glyph", "surface_patch"],
            "glyph_kinds": available_kinds(None),
            "field_brush_modes": ["add", "subtract", "set", "smooth"],
            "selection_kinds": ["click", "brush", "box", "lasso", "flood", "bounds", "indices"],
            "selection_operations": ["replace", "add", "subtract", "intersect"],
            "selection_visibility": ["front", "through"],
            "selection_snap": ["none", "cell", "grid", "feature"],
            "falloffs": ["constant", "linear", "smoothstep", "gaussian"],
            "coordinate_system": "model_mm",
            "preview_policy": "immutable-baseline-last-requested-wins",
        })
    }
}

fn floats(v: &Value) -> AResult<Vec<f64>> {
    match v {
        Value::Array(a) => a.iter().map(py_float).collect(),
        other => {
            Err(crate::py::type_error(format!("'{}' object is not iterable", crate::py::type_name(other))))
        }
    }
}

fn shape_arg(v: Option<&Value>) -> AResult<[i64; 3]> {
    let Some(v) = v else { return Ok([3, 3, 3]) };
    let items = flatten_json(v).unwrap_or_default();
    if items.len() != 3 {
        return Err(crate::field_interaction::err("shape must contain three integers"));
    }
    Ok([py_int(items[0])?, py_int(items[1])?, py_int(items[2])?])
}
