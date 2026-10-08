// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::py::{Arr, canonical_unicode, jf, nested, py_eq, py_str, repr, sha256_hex, uuid_hex};

const CLASS: &str = "SpatialValueError";

fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value(CLASS, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceKind {
    Value,
    ParameterBinding,
    ArrayReference,
}

impl ReferenceKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::ParameterBinding => "parameter_binding",
            Self::ArrayReference => "array_reference",
        }
    }
}

const ARRAY_KEYS: [&str; 7] = ["array", "$array", "array_ref", "array_id", "arrayId", "blob_ref", "data_ref"];
const BIND_KEYS: [&str; 6] = ["$bind", "bind", "binding", "parameter_ref", "parameterId", "param_ref"];
const ARRAY_STORES: [&str; 5] = ["arrays", "array_store", "array_data", "blobs", "resources"];
const PARAMETER_STORES: [&str; 3] = ["parameters", "named_parameters", "bindings"];
const NODE_STORES: [&str; 2] = ["nodes", "graph"];
const PARAM_KEYS: [&str; 4] = ["params", "parameters", "attributes", "attrs"];

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayMutation {
    pub document: Value,
    pub node_id: String,
    pub parameter: String,
    pub storage_id: Option<String>,
    pub copied: bool,
    pub shared_reference_count: usize,
    pub dtype: String,
    pub shape: Vec<usize>,
    pub payload_sha256: String,
}

#[must_use]
pub fn reference_kind(value: &Value) -> ReferenceKind {
    let Some(m) = value.as_object() else { return ReferenceKind::Value };
    if ARRAY_KEYS.iter().any(|k| m.get(*k).is_some_and(|v| *k != "array" || v.is_string())) {
        return ReferenceKind::ArrayReference;
    }
    if BIND_KEYS.iter().any(|k| m.contains_key(*k)) {
        return ReferenceKind::ParameterBinding;
    }
    if m.contains_key("ref") {
        let tag = m.get("kind").or_else(|| m.get("type")).map_or_else(String::new, py_str).to_lowercase();
        if tag.contains("array") || tag.contains("tensor") || tag.contains("blob") {
            return ReferenceKind::ArrayReference;
        }
        return ReferenceKind::ParameterBinding;
    }
    ReferenceKind::Value
}

fn reference_id(value: &Value, kind: ReferenceKind) -> AResult<String> {
    let keys: &[&str] = if kind == ReferenceKind::ArrayReference { &ARRAY_KEYS } else { &BIND_KEYS };
    let m = value.as_object();
    for k in keys {
        if let Some(v) = m.and_then(|m| m.get(*k)) {
            return Ok(py_str(v));
        }
    }
    if let Some(v) = m.and_then(|m| m.get("ref")) {
        return Ok(py_str(v));
    }
    Err(err(format!("Reference has no identifier: {}", repr(value))))
}

fn find_store_name(document: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names.iter().find(|n| document.contains_key(**n)).map(|n| (*n).to_string())
}

fn node_mut<'a>(document: &'a mut Map<String, Value>, node_id: &str) -> AResult<&'a mut Map<String, Value>> {
    let unknown = || err(format!("Unknown node {}", repr(&Value::from(node_id))));
    let Some(store_name) = find_store_name(document, &NODE_STORES) else { return Err(unknown()) };
    let store = document.get_mut(&store_name).ok_or_else(unknown)?;
    match store {
        Value::Object(m) => m.get_mut(node_id).and_then(Value::as_object_mut).ok_or_else(unknown),
        Value::Array(items) => items
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .find(|c| py_str(c.get("id").or_else(|| c.get("node_id")).unwrap_or(&Value::from(""))) == node_id)
            .ok_or_else(unknown),
        _ => Err(unknown()),
    }
}

fn node_parameters(node: &mut Map<String, Value>) -> AResult<&mut Map<String, Value>> {
    let key = PARAM_KEYS.iter().find(|k| node.get(**k).is_some_and(Value::is_object)).copied();
    let key = key.unwrap_or_else(|| {
        node.insert(PARAM_KEYS[0].into(), Value::Object(Map::new()));
        PARAM_KEYS[0]
    });
    node.get_mut(key).and_then(Value::as_object_mut).ok_or_else(|| err("node parameters are not an object"))
}

#[derive(Clone, Debug)]
enum Slot {
    Key(String),
    Index(usize),
}

fn parameter_slot(document: &Map<String, Value>, parameter_id: &str) -> AResult<(String, Slot)> {
    let unknown = || err(format!("Unknown named parameter {}", repr(&Value::from(parameter_id))));
    let store_name = find_store_name(document, &PARAMETER_STORES).ok_or_else(unknown)?;
    match &document[&store_name] {
        Value::Object(m) => {
            if m.contains_key(parameter_id) {
                Ok((store_name, Slot::Key(parameter_id.to_string())))
            } else {
                Err(unknown())
            }
        }
        Value::Array(items) => items
            .iter()
            .position(|e| {
                e.is_object()
                    && py_str(e.get("id").or_else(|| e.get("name")).unwrap_or(&Value::from("")))
                        == parameter_id
            })
            .map(|i| (store_name, Slot::Index(i)))
            .ok_or_else(unknown),
        _ => Err(unknown()),
    }
}

fn array_slot(document: &Map<String, Value>, storage_id: &str) -> AResult<(String, Slot)> {
    let unknown = || err(format!("Unknown array storage id {}", repr(&Value::from(storage_id))));
    let Some(store_name) = find_store_name(document, &ARRAY_STORES) else {

        return Err(unknown());
    };
    match &document[&store_name] {
        Value::Object(m) => {
            if m.contains_key(storage_id) {
                Ok((store_name, Slot::Key(storage_id.to_string())))
            } else {
                Err(unknown())
            }
        }
        Value::Array(items) => items
            .iter()
            .position(|e| {
                e.is_object()
                    && py_str(e.get("id").or_else(|| e.get("name")).unwrap_or(&Value::from(""))) == storage_id
            })
            .map(|i| (store_name, Slot::Index(i)))
            .ok_or_else(unknown),
        _ => Err(unknown()),
    }
}

fn slot_get<'a>(document: &'a Map<String, Value>, store: &str, slot: &Slot) -> &'a Value {
    static NULL: Value = Value::Null;
    match (&document.get(store), slot) {
        (Some(Value::Object(m)), Slot::Key(k)) => m.get(k).unwrap_or(&NULL),
        (Some(Value::Array(a)), Slot::Index(i)) => a.get(*i).unwrap_or(&NULL),
        _ => &NULL,
    }
}

fn slot_set(document: &mut Map<String, Value>, store: &str, slot: &Slot, value: Value) {
    match (document.get_mut(store), slot) {
        (Some(Value::Object(m)), Slot::Key(k)) => {
            m.insert(k.clone(), value);
        }
        (Some(Value::Array(a)), Slot::Index(i)) => {
            if let Some(x) = a.get_mut(*i) {
                *x = value;
            }
        }
        _ => {}
    }
}

fn entry_value(entry: &Value) -> (Value, Option<&'static str>) {
    if let Some(m) = entry.as_object() {
        for k in ["value", "default", "data"] {
            if let Some(v) = m.get(k) {
                return (v.clone(), Some(k));
            }
        }
    }
    (entry.clone(), None)
}

fn with_entry_value(entry: &Value, value_key: Option<&str>, value: Value) -> Value {
    match value_key {
        None => value,
        Some(k) => {
            let mut replacement = entry.clone();
            if let Some(m) = replacement.as_object_mut() {
                m.insert(k.to_string(), value);
            }
            replacement
        }
    }
}

fn decode_array(entry: &Value) -> AResult<Arr> {
    if let Some(m) = entry.as_object() {
        if m.contains_key("b64") {
            return decode_b64(m).map_err(|e| err(format!("invalid canonical inline array: {e}")));
        }
        let data = ["values", "data", "value", "array"].iter().find_map(|k| m.get(*k));
        let Some(data) = data else {
            return Err(err("Array storage entry contains no values/data/value field"));
        };
        let mut arr = Arr::from_json(data)?;
        if let Some(shape) = m.get("shape").filter(|s| !s.is_null()) {
            let dims: Vec<usize> = shape
                .as_array()
                .map(|a| {
                    a.iter().map(|v| crate::py::py_int(v).map(|x| usize::try_from(x).unwrap_or(0))).collect()
                })
                .transpose()?
                .unwrap_or_default();
            if dims.iter().product::<usize>() != arr.size() {
                return Err(crate::py::value_error(format!(
                    "cannot reshape array of size {} into shape ({})",
                    arr.size(),
                    dims.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                )));
            }
            arr.shape = dims;
        }
        return Ok(arr);
    }
    Arr::from_json(entry)
}

fn decode_b64(m: &Map<String, Value>) -> Result<Arr, String> {
    let dtype_name = m.get("dtype").map(py_str).ok_or_else(|| "'dtype'".to_string())?;
    let dtype = implexity_geometry::value::DType::from_name(&dtype_name)
        .ok_or_else(|| format!("data type {} not understood", repr(&Value::from(dtype_name.clone()))))?;
    let b64 = m.get("b64").and_then(Value::as_str).ok_or_else(|| {
        "argument should be a bytes-like object or ASCII string, not 'NoneType'".to_string()
    })?;
    let raw = implexity_geometry::document::arrays::b64decode_strict(b64)?;
    let digest = m.get("sha256").map(py_str).ok_or_else(|| "'sha256'".to_string())?;
    if sha256_hex(&raw) != digest {
        return Err("array checksum mismatch".into());
    }
    let shape: Vec<usize> = m
        .get("shape")
        .and_then(Value::as_array)
        .ok_or_else(|| "'shape'".to_string())?
        .iter()
        .map(|v| {
            v.as_u64()
                .map(|x| usize::try_from(x).unwrap_or(0))
                .ok_or_else(|| "shape must be integers".to_string())
        })
        .collect::<Result<_, _>>()?;
    let n: usize = shape.iter().product();
    if raw.len() % dtype.itemsize() != 0 || raw.len() / dtype.itemsize() != n {
        return Err(format!(
            "cannot reshape array of size {} into shape ({})",
            raw.len() / dtype.itemsize().max(1),
            shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    let nd = implexity_geometry::value::NdArray::from_le_bytes(dtype, shape.clone(), &raw)
        .ok_or_else(|| "array payload does not match its shape".to_string())?;
    Ok(Arr::new(shape, nd.to_f64_vec()))
}

fn float_bytes(arr: &Arr) -> Vec<u8> {
    let mut raw = Vec::with_capacity(arr.size() * 8);
    for v in &arr.data {
        raw.extend_from_slice(&v.to_le_bytes());
    }
    raw
}

fn encode_array_like(entry: &Value, arr: &Arr, storage_id: Option<&str>) -> Value {
    let flat_values = || Value::Array(arr.data.iter().map(|v| jf(*v)).collect());
    let Some(m) = entry.as_object() else { return arr.to_json() };
    let values = if m.get("shape").is_some_and(|s| !s.is_null()) { flat_values() } else { arr.to_json() };
    let mut result = m.clone();
    if result.contains_key("b64") {
        let raw = float_bytes(arr);
        result.insert("dtype".into(), Value::from("float64"));
        result.insert("shape".into(), json!(arr.shape));
        result.insert("b64".into(), Value::from(base64::engine::general_purpose::STANDARD.encode(&raw)));
        result.insert("sha256".into(), Value::from(sha256_hex(&raw)));
        return Value::Object(result);
    }
    let value_key = ["values", "data", "value", "array"]
        .iter()
        .find(|k| result.contains_key(**k))
        .copied()
        .unwrap_or("values");
    result.insert(value_key.into(), values);
    result.insert("shape".into(), json!(arr.shape));
    result.insert("dtype".into(), Value::from("float64"));
    if let Some(sid) = storage_id {
        if result.contains_key("id") {
            result.insert("id".into(), Value::from(sid));
        } else if result.contains_key("name") {
            result.insert("name".into(), Value::from(sid));
        }
    }
    Value::Object(result)
}

fn walk_values<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(m) => {
            out.push(value);
            for v in m.values() {
                walk_values(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                walk_values(v, out);
            }
        }
        _ => {}
    }
}

fn reference_count(document: &Value, storage_id: &str) -> usize {
    let mut all = Vec::new();
    walk_values(document, &mut all);
    all.into_iter()
        .filter(|v| reference_kind(v) == ReferenceKind::ArrayReference)
        .filter(|v| reference_id(v, ReferenceKind::ArrayReference).is_ok_and(|id| id == storage_id))
        .count()
}

fn doc_map(document: &Value) -> AResult<Map<String, Value>> {
    document.as_object().cloned().ok_or_else(|| crate::py::type_error("the document must be an object"))
}


pub fn resolve_array_parameter(document: &Value, node_id: &str, parameter: &str) -> AResult<Arr> {
    let mut doc = doc_map(document)?;
    let value = {
        let params = node_parameters(node_mut(&mut doc, node_id)?)?;
        params.get(parameter).cloned().ok_or_else(|| {
            err(format!(
                "Node {} has no parameter {}",
                repr(&Value::from(node_id)),
                repr(&Value::from(parameter))
            ))
        })?
    };
    let mut value = value;
    if reference_kind(&value) == ReferenceKind::ParameterBinding {
        let pid = reference_id(&value, ReferenceKind::ParameterBinding)?;
        let (store, slot) = parameter_slot(&doc, &pid)?;
        value = entry_value(slot_get(&doc, &store, &slot)).0;
    }
    if reference_kind(&value) == ReferenceKind::ArrayReference {
        let storage_id = reference_id(&value, ReferenceKind::ArrayReference)?;
        let (store, slot) = array_slot(&doc, &storage_id)?;
        return decode_array(slot_get(&doc, &store, &slot));
    }
    Arr::from_json(&value)
}


pub fn persist_array_parameter(
    document: &Value,
    node_id: &str,
    parameter: &str,
    values: &Arr,
    share_policy: &str,
    expected_revision: Option<&Value>,
) -> AResult<ArrayMutation> {
    if share_policy != "preserve" && share_policy != "copy" {
        return Err(err("share_policy must be 'preserve' or 'copy'"));
    }
    let mut doc = doc_map(document)?;
    if let Some(expected) = expected_revision.filter(|v| !v.is_null()) {
        let current = doc.get("revision").or_else(|| doc.get("version")).cloned().unwrap_or(Value::Null);
        if !py_eq(&current, expected) {
            return Err(err(format!(
                "Document revision changed: expected {}, found {}",
                repr(expected),
                repr(&current)
            )));
        }
    }
    let arr = values;
    if arr.ndim() == 0 {
        return Err(err("A spatial array must have at least one dimension"));
    }
    if !arr.all_finite() {
        return Err(err("Spatial array contains non-finite values"));
    }
    let owner_entry = {
        let params = node_parameters(node_mut(&mut doc, node_id)?)?;
        params.get(parameter).cloned().ok_or_else(|| {
            err(format!(
                "Node {} has no parameter {}",
                repr(&Value::from(node_id)),
                repr(&Value::from(parameter))
            ))
        })?
    };

    enum Owner {
        Param,
        Named(String, Slot, Value, Option<&'static str>),
    }
    let mut owner = Owner::Param;
    let mut value = owner_entry.clone();
    if reference_kind(&value) == ReferenceKind::ParameterBinding {
        let pid = reference_id(&value, ReferenceKind::ParameterBinding)?;
        let (store, slot) = parameter_slot(&doc, &pid)?;
        let p_entry = slot_get(&doc, &store, &slot).clone();
        let (v, key) = entry_value(&p_entry);
        value = v;
        owner = Owner::Named(store, slot, p_entry, key);
    }
    let set_owner = |doc: &mut Map<String, Value>, new_value: Value| -> AResult<()> {
        match &owner {
            Owner::Param => {
                let params = node_parameters(node_mut(doc, node_id)?)?;
                params.insert(parameter.to_string(), new_value);
            }
            Owner::Named(store, slot, entry, key) => {
                let v = with_entry_value(entry, *key, new_value);
                slot_set(doc, store, slot, v);
            }
        }
        Ok(())
    };
    let mut storage_id = None;
    let mut copied = false;
    let mut shared_count = 0;
    if reference_kind(&value) == ReferenceKind::ArrayReference {
        let sid = reference_id(&value, ReferenceKind::ArrayReference)?;
        let (store, slot) = array_slot(&doc, &sid)?;
        let s_entry = slot_get(&doc, &store, &slot).clone();
        shared_count = reference_count(&Value::Object(doc.clone()), &sid);
        if share_policy == "copy" && shared_count > 1 {
            let new_id = format!("{sid}__edit_{}", &uuid_hex()[..12]);
            let encoded = encode_array_like(&s_entry, arr, Some(&new_id));
            match doc.get_mut(&store) {
                Some(Value::Object(m)) => {
                    m.insert(new_id.clone(), encoded);
                }
                Some(Value::Array(a)) => a.push(encoded),
                _ => return Err(err("Unsupported array store")),
            }
            let mut replacement_ref = value.as_object().cloned().unwrap_or_default();
            if let Some(k) = ARRAY_KEYS.iter().find(|k| replacement_ref.contains_key(**k)) {
                replacement_ref.insert((*k).to_string(), Value::from(new_id.clone()));
            } else {
                replacement_ref.insert("ref".into(), Value::from(new_id.clone()));
                replacement_ref.insert("kind".into(), Value::from("array"));
            }
            set_owner(&mut doc, Value::Object(replacement_ref))?;
            storage_id = Some(new_id);
            copied = true;
        } else {
            let encoded = encode_array_like(&s_entry, arr, Some(&sid));
            slot_set(&mut doc, &store, &slot, encoded);
            storage_id = Some(sid);
        }
    } else {
        set_owner(&mut doc, arr.to_json())?;
    }
    let payload = float_bytes(arr);
    Ok(ArrayMutation {
        document: Value::Object(doc),
        node_id: node_id.to_string(),
        parameter: parameter.to_string(),
        storage_id,
        copied,
        shared_reference_count: shared_count,
        dtype: "float64".into(),
        shape: arr.shape.clone(),
        payload_sha256: sha256_hex(&payload),
    })
}

#[must_use]
pub fn canonical_document_sha256(document: &Value) -> String {
    sha256_hex(canonical_unicode(document).as_bytes())
}

#[must_use]
pub fn array_json(arr: &Arr) -> Value {
    nested(&arr.shape, &arr.data)
}
