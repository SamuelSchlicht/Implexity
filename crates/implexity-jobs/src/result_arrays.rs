// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use implexity_io::npy::{NpyArray, NpyData};
use serde_json::{Map, Value};

use crate::artifacts::{ArtifactError, ResultArtifactStore, dtype_name};
use crate::private::sha256_hex;

pub const ROOTS: [&str; 4] = [
    "result_artifacts_v24_provider",
    "sensitivity_artifacts_v24_provider",
    "result_artifacts_v22",
    "sensitivity_artifacts_v22",
];
pub const ENDPOINTS: [&str; 2] =
    ["GET /v1/implicit/result-artifact/<id>/manifest", "GET /v1/implicit/result-artifact/<id>/array"];
pub const MAX_VALUES: usize = 262_144;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArrayReadError {
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    NotFound(String),
}

impl From<ArtifactError> for ArrayReadError {
    fn from(e: ArtifactError) -> Self {
        match e {
            ArtifactError::NotFound(m) => Self::NotFound(m),
            ArtifactError::Invalid(m) => Self::Value(m),
        }
    }
}

#[must_use]
pub fn dtype_str(data: &NpyData) -> String {
    data.descr()
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultTree {
    Object(Vec<(String, ResultTree)>),
    List(Vec<ResultTree>, bool),
    Array(NpyArray),
    Scalar(Value),
}

impl ResultTree {
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Object(m) => {
                Self::Object(m.iter().map(|(k, v)| (k.clone(), Self::from_value(v))).collect())
            }
            Value::Array(a) => Self::List(a.iter().map(Self::from_value).collect(), false),
            other => Self::Scalar(other.clone()),
        }
    }

    #[must_use]
    pub fn from_evaluation(value: &Value) -> Self {
        let Value::Object(m) = value else { return Self::from_value(value) };
        Self::Object(
            m.iter()
                .map(|(k, v)| {
                    let node = if k == "fields" {
                        match v {
                            Value::Object(fields) => Self::Object(
                                fields
                                    .iter()
                                    .map(|(name, f)| {
                                        (
                                            name.clone(),
                                            rectangular_f64(f)
                                                .map_or_else(|| Self::from_value(f), Self::Array),
                                        )
                                    })
                                    .collect(),
                            ),
                            other => Self::from_value(other),
                        }
                    } else {
                        Self::from_value(v)
                    };
                    (k.clone(), node)
                })
                .collect(),
        )
    }
}

fn rectangular_f64(value: &Value) -> Option<NpyArray> {
    fn walk(value: &Value, depth: usize, shape: &mut Vec<usize>, out: &mut Vec<f64>) -> Option<()> {
        match value {
            Value::Array(items) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape[depth] != items.len() {
                    return None;
                }
                for item in items {
                    walk(item, depth + 1, shape, out)?;
                }
                Some(())
            }
            Value::Number(n) if shape.len() == depth => {
                out.push(n.as_f64()?);
                Some(())
            }
            _ => None,
        }
    }
    if !value.is_array() && !value.is_number() {
        return None;
    }
    let mut shape = Vec::new();
    let mut out = Vec::new();
    walk(value, 0, &mut shape, &mut out)?;
    NpyArray::new(shape, NpyData::F64(out)).ok()
}

#[must_use]
pub fn data_bytes(array: &NpyArray) -> Vec<u8> {
    let full = array.to_bytes().unwrap_or_default();
    let header =
        implexity_io::npy::header_bytes(&array.data.descr(), false, &array.shape).map_or(0, |h| h.len());
    full.get(header..).map(<[u8]>::to_vec).unwrap_or_default()
}

fn encode(value: &Value) -> Vec<u8> {
    implexity_core::json::dumps(value, &implexity_core::json::DumpOptions::canonical().ascii(false))
        .into_bytes()
}

fn is_finite_data(data: &NpyData) -> bool {
    crate::artifacts::finite_count(data).is_none_or(|c| c == data.len())
}

fn homogeneous_kind(items: &[ResultTree]) -> Option<char> {
    let mut kinds = std::collections::BTreeSet::new();
    let mut pending: Vec<&ResultTree> = items.iter().collect();
    while let Some(item) = pending.pop() {
        match item {
            ResultTree::List(inner, _) => pending.extend(inner.iter()),
            ResultTree::Scalar(Value::Bool(_)) => {
                kinds.insert('b');
            }
            ResultTree::Scalar(Value::Number(n)) if n.is_f64() => {
                kinds.insert('f');
            }
            ResultTree::Scalar(Value::Number(_)) => {
                kinds.insert('i');
            }
            _ => return None,
        }
        if kinds.len() > 1 {
            return None;
        }
    }
    kinds.into_iter().next()
}

fn list_to_array(items: &[ResultTree], kind: char) -> Option<NpyArray> {
    fn walk(node: &ResultTree, depth: usize, shape: &mut Vec<usize>, out: &mut Vec<Value>) -> Option<()> {
        match node {
            ResultTree::List(items, _) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape[depth] != items.len() {
                    return None;
                }
                for item in items {
                    walk(item, depth + 1, shape, out)?;
                }
                Some(())
            }
            ResultTree::Scalar(v) if shape.len() == depth => {
                out.push(v.clone());
                Some(())
            }
            _ => None,
        }
    }
    let mut shape = Vec::new();
    let mut values = Vec::new();
    walk(&ResultTree::List(items.to_vec(), false), 0, &mut shape, &mut values)?;
    let data = match kind {
        'b' => NpyData::Bool(values.iter().map(|v| v.as_bool().unwrap_or(false)).collect()),
        'i' => NpyData::I64(values.iter().map(Value::as_i64).collect::<Option<Vec<_>>>()?),
        _ => NpyData::F64(values.iter().map(Value::as_f64).collect::<Option<Vec<_>>>()?),
    };
    NpyArray::new(shape, data).ok()
}

enum Slot {
    Ref(String),
    Json(Value),
}

enum Node {
    Object(Vec<(String, Node)>),
    List(Vec<Node>),
    Leaf(Slot),
}

struct Externalizer<'a> {
    result: &'a ResultTree,
    arrays: BTreeMap<String, NpyArray>,
    metadata: Map<String, Value>,
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

impl Externalizer<'_> {
    fn field_metadata(&self, field: &str) -> Option<(Map<String, Value>, Option<Value>)> {
        let ResultTree::Object(top) = self.result else { return None };
        let diagnostics = top.iter().find(|(k, _)| k == "diagnostics").map(|(_, v)| v)?;
        let ResultTree::Object(diag) = diagnostics else { return None };
        let to_value = |t: &ResultTree| tree_json(t);
        let metadata = diag.iter().find(|(k, _)| k == "field_metadata").map(|(_, v)| to_value(v));
        let registration = diag.iter().find(|(k, _)| k == "field_registration").map(|(_, v)| to_value(v));
        let authored =
            metadata.and_then(|m| m.get(field).and_then(Value::as_object).cloned()).unwrap_or_default();
        Some((authored, registration))
    }

    #[allow(clippy::too_many_lines)]
    fn externalize(
        &mut self,
        array: &NpyArray,
        pointer: &str,
        representation: &str,
    ) -> Result<Slot, ArtifactError> {
        if matches!(array.data, NpyData::Unicode { .. } | NpyData::Bytes { .. }) {
            return Err(ArtifactError::Invalid(format!(
                "unsupported authored result array dtype {} at {pointer}",
                dtype_name(&array.data)
            )));
        }

        if array.data.is_empty() {
            let mut m = Map::new();
            m.insert("schema".into(), Value::String("implexity-empty-array/1".into()));
            m.insert("shape".into(), Value::Array(array.shape.iter().map(|d| Value::from(*d)).collect()));
            m.insert("dtype".into(), Value::String(dtype_str(&array.data)));
            m.insert("original_representation".into(), Value::String(representation.into()));
            m.insert("json_pointer".into(), Value::String(pointer.into()));
            m.insert("values".into(), Value::Array(Vec::new()));
            m.insert("empty".into(), Value::Bool(true));
            m.insert("read_required".into(), Value::Bool(false));
            m.insert("exact_sha256".into(), Value::String(sha256_hex(b"")));
            return Ok(Slot::Json(Value::Object(m)));
        }
        if !is_finite_data(&array.data) {
            return Err(ArtifactError::Invalid(format!("nonfinite authored result array at {pointer}")));
        }
        let key = format!("array_{:06}", self.arrays.len());
        let original_dtype = dtype_str(&array.data);
        let is_bool = matches!(array.data, NpyData::Bool(_));
        let stored = match &array.data {
            NpyData::Bool(b) => {
                NpyArray::new(array.shape.clone(), NpyData::U8(b.iter().map(|x| u8::from(*x)).collect()))
                    .map_err(|e| ArtifactError::Invalid(e.to_string()))?
            }
            _ => array.clone(),
        };
        let stored_bytes = data_bytes(&stored);
        let mut row = Map::new();
        row.insert("json_pointer".into(), Value::String(pointer.into()));
        row.insert("original_representation".into(), Value::String(representation.into()));
        row.insert("original_dtype".into(), Value::String(original_dtype));
        row.insert("stored_dtype".into(), Value::String(dtype_str(&stored.data)));
        row.insert(
            "boolean_encoding".into(),
            if is_bool { Value::String("uint8_0_1".into()) } else { Value::Null },
        );
        row.insert("empty".into(), Value::Bool(false));
        row.insert("read_required".into(), Value::Bool(true));
        row.insert("exact_sha256".into(), Value::String(sha256_hex(&stored_bytes)));
        if let Some(rest) = pointer.strip_prefix("/fields/")
            && !rest.contains('/')
        {
            let field = rest.replace("~1", "/").replace("~0", "~");
            row.insert(
                "field_metadata_pointer".into(),
                Value::String(format!("/diagnostics/field_metadata/{rest}")),
            );
            let (authored, registration) = self.field_metadata(&field).unwrap_or_default();
            for name in ["units", "association", "rank", "geometric_role", "coordinate_units", "tensor_convention"] {
                if let Some(v) =
                    authored.get(name).filter(|v| v.is_string() || v.is_number() || v.is_boolean())
                {
                    row.insert(name.into(), v.clone());
                }
            }
            if authored.get("association").and_then(Value::as_str) == Some("cell") {
                let wire = authored.get("registration").cloned().or(registration);
                if let Some(wire) = wire.filter(|w| !w.is_null()) {
                    if !wire.is_object() || encode(&wire).len() > 4096 {
                        return Err(ArtifactError::Invalid(
                            "bounded explicit field registration required".into(),
                        ));
                    }
                    let shape_ok = wire.get("shape").and_then(Value::as_array).is_some_and(|s| {
                        s.len() == 3 && s.iter().all(|v| v.is_u64() && v.as_u64().is_some_and(|x| x > 0))
                    });
                    if !shape_ok {
                        return Err(ArtifactError::Invalid(
                            "field registration requires three positive integer dimensions".into(),
                        ));
                    }
                    let registration =
                        implexity_geometry::field_registration::GridRegistration::from_wire(&wire)
                            .map_err(|e| ArtifactError::Invalid(e.to_string()))?;
                    let mut expected: Vec<usize> = registration.shape.to_vec();
                    match authored.get("rank").and_then(Value::as_str) {
                        Some("vector") => expected.push(3),
                        Some("tensor") => {
                            let count = authored.get("components").and_then(Value::as_array).map(Vec::len);
                            if array.shape.len() == 4 && count.is_some_and(|n| n == 6 || n == 9) {
                                expected.push(count.unwrap_or(0));
                            } else if array.shape.len() == 5 && count.is_none_or(|n| n == 9) {
                                expected.extend([3, 3]);
                            } else {
                                return Err(ArtifactError::Invalid(
                                    "explicit tensor components must match its stored shape".into(),
                                ));
                            }
                        }
                        _ => {}
                    }
                    if registration.centering != "cell" || array.shape != expected {
                        return Err(ArtifactError::Invalid(
                            "explicit cell field registration shape mismatch".into(),
                        ));
                    }
                    row.insert("registration".into(), registration.to_wire());
                }
                if let Some(frame) = authored.get("component_frame").filter(|v| !v.is_null()) {
                    match frame.as_str() {
                        Some(f) if !f.is_empty() && f.chars().count() <= 128 => {
                            row.insert("component_frame".into(), frame.clone());
                        }
                        _ => {
                            return Err(ArtifactError::Invalid(
                                "bounded explicit component frame required".into(),
                            ));
                        }
                    }
                }
                if let Some(components) = authored.get("components").filter(|v| !v.is_null()) {
                    let names: Option<Vec<&str>> =
                        components.as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
                    let ok = names.as_ref().is_some_and(|n| {
                        let set: std::collections::BTreeSet<&&str> = n.iter().collect();
                        if authored.get("rank").and_then(Value::as_str) == Some("tensor") {
                            (n.len() == 6 || n.len() == 9)
                                && components.as_array().is_some_and(|a| a.len() == n.len())
                                && set.len() == n.len()
                                && n.iter().all(|c| !c.is_empty() && c.chars().count() <= 128)
                        } else {
                            n.len() == 3
                                && components.as_array().is_some_and(|a| a.len() == 3)
                                && set.len() == 3
                                && n.iter().all(|c| matches!(*c, "x" | "y" | "z"))
                        }
                    });
                    if !ok {
                        return Err(ArtifactError::Invalid(
                            "explicit component order must match a spatial vector or tensor".into(),
                        ));
                    }
                    row.insert("components".into(), components.clone());
                }
            }
        }
        self.metadata.insert(key.clone(), Value::Object(row));
        self.arrays.insert(key.clone(), stored);
        Ok(Slot::Ref(key))
    }

    fn visit(&mut self, node: &ResultTree, pointer: &str) -> Result<Node, ArtifactError> {
        Ok(match node {
            ResultTree::Object(items) => Node::Object(
                items
                    .iter()
                    .map(|(k, v)| {
                        Ok((k.clone(), self.visit(v, &format!("{pointer}/{}", escape_pointer(k)))?))
                    })
                    .collect::<Result<_, ArtifactError>>()?,
            ),
            ResultTree::Array(a) => Node::Leaf(self.externalize(a, pointer, "ndarray")?),
            ResultTree::List(items, tuple) => {
                if let Some(kind) = homogeneous_kind(items)
                    && let Some(array) = list_to_array(items, kind)
                    && array.data.len() >= 256
                {
                    return Ok(Node::Leaf(self.externalize(
                        &array,
                        pointer,
                        if *tuple { "tuple" } else { "list" },
                    )?));
                }
                Node::List(
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, v)| self.visit(v, &format!("{pointer}/{i}")))
                        .collect::<Result<_, ArtifactError>>()?,
                )
            }
            ResultTree::Scalar(v) => Node::Leaf(Slot::Json(v.clone())),
        })
    }
}

fn tree_json(tree: &ResultTree) -> Value {
    match tree {
        ResultTree::Object(items) => {
            Value::Object(items.iter().map(|(k, v)| (k.clone(), tree_json(v))).collect())
        }
        ResultTree::List(items, _) => Value::Array(items.iter().map(tree_json).collect()),
        ResultTree::Array(a) => array_json(a),
        ResultTree::Scalar(v) => v.clone(),
    }
}

fn array_json(array: &NpyArray) -> Value {
    let flat = flat_json(&array.data);
    fn nest(flat: &[Value], shape: &[usize]) -> Value {
        match shape.split_first() {
            None => flat.first().cloned().unwrap_or(Value::Null),
            Some((&n, rest)) => {
                let stride: usize = rest.iter().product::<usize>().max(1);
                Value::Array(
                    (0..n)
                        .map(|i| {
                            nest(
                                &flat[(i * stride).min(flat.len())..((i + 1) * stride).min(flat.len())],
                                rest,
                            )
                        })
                        .collect(),
                )
            }
        }
    }
    nest(&flat, &array.shape)
}

fn flat_json(data: &NpyData) -> Vec<Value> {
    match data {
        NpyData::Bool(v) => v.iter().map(|x| Value::Bool(*x)).collect(),
        NpyData::I8(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::U8(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::I16(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::U16(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::I32(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::U32(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::I64(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::U64(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::F32(v) => v.iter().map(|x| Value::from(f64::from(*x))).collect(),
        NpyData::F64(v) => v.iter().map(|x| Value::from(*x)).collect(),
        NpyData::C64(v) => v
            .iter()
            .map(|x| Value::Array(vec![Value::from(f64::from(x[0])), Value::from(f64::from(x[1]))]))
            .collect(),
        NpyData::C128(v) => {
            v.iter().map(|x| Value::Array(vec![Value::from(x[0]), Value::from(x[1])])).collect()
        }
        NpyData::Unicode { values, .. } => values.iter().map(|s| Value::String(s.clone())).collect(),
        NpyData::Bytes { values, .. } => {
            values.iter().map(|b| Value::String(String::from_utf8_lossy(b).into_owned())).collect()
        }
    }
}

fn resolve(node: &Node, references: &Map<String, Value>) -> Value {
    match node {
        Node::Object(items) => {
            Value::Object(items.iter().map(|(k, v)| (k.clone(), resolve(v, references))).collect())
        }
        Node::List(items) => Value::Array(items.iter().map(|v| resolve(v, references)).collect()),
        Node::Leaf(Slot::Json(v)) => v.clone(),
        Node::Leaf(Slot::Ref(key)) => references.get(key).cloned().unwrap_or(Value::Null),
    }
}

#[must_use]
pub fn array_references(
    artifact: &Map<String, Value>,
    field_metadata: &Map<String, Value>,
) -> Map<String, Value> {
    let aid = artifact.get("artifact_id").and_then(Value::as_str).unwrap_or("");
    let base = format!("/v1/implicit/result-artifact/{aid}");
    let mut out = Map::new();
    let empty = Map::new();
    for (name, meta) in artifact.get("fields").and_then(Value::as_object).unwrap_or(&empty) {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String("implexity-result-array-ref/1".into()));
        m.insert("artifact_id".into(), Value::String(aid.into()));
        m.insert("field_name".into(), Value::String(name.clone()));
        m.insert("manifest".into(), Value::String(format!("{base}/manifest")));
        m.insert("array".into(), Value::String(format!("{base}/array?field={}", quote(name))));
        m.insert("shape".into(), meta.get("shape").cloned().unwrap_or(Value::Null));
        m.insert("dtype".into(), meta.get("dtype").cloned().unwrap_or(Value::Null));
        m.insert(
            "metadata".into(),
            Value::Object(field_metadata.get(name).and_then(Value::as_object).cloned().unwrap_or_default()),
        );
        out.insert(name.clone(), Value::Object(m));
    }
    out
}

#[must_use]
pub fn quote(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{b:02X}"));
        }
    }
    out
}


pub fn authored_evaluation_artifact_view(
    result: &ResultTree,
    problem: &Value,
    design: &Value,
    root: &Path,
    max_envelope_bytes: usize,
) -> Result<Value, ArtifactError> {
    let mut identities = Map::new();
    identities.insert("source".into(), Value::String("explicit_authored_case".into()));
    identities.insert("problem_sha256".into(), Value::String(sha256_hex(&encode(problem))));
    identities.insert("design_sha256".into(), Value::String(sha256_hex(&encode(design))));
    let mut ext = Externalizer { result, arrays: BTreeMap::new(), metadata: Map::new() };
    let tree = ext.visit(result, "")?;
    let preview_fields: Map<String, Value> = ext
        .arrays
        .iter()
        .map(|(k, v)| {
            let mut m = Map::new();
            m.insert("shape".into(), Value::Array(v.shape.iter().map(|d| Value::from(*d)).collect()));
            m.insert("dtype".into(), Value::String(dtype_name(&v.data)));
            (k.clone(), Value::Object(m))
        })
        .collect();
    let preview_id = format!("result-{}", "0".repeat(32));
    let mut preview = Map::new();
    preview.insert("artifact_id".into(), Value::String(preview_id.clone()));
    preview.insert("fields".into(), Value::Object(preview_fields));
    let references =
        if ext.arrays.is_empty() { Map::new() } else { array_references(&preview, &ext.metadata) };
    let mut envelope = Map::new();
    envelope.insert("schema".into(), Value::String("implexity-authored-evaluation-artifact-view/1".into()));
    envelope.insert("view".into(), Value::String("artifact".into()));
    envelope.insert("evaluation".into(), resolve(&tree, &references));
    envelope.insert("identities".into(), Value::Object(identities.clone()));
    envelope.insert(
        "artifact_id".into(),
        if ext.arrays.is_empty() { Value::Null } else { Value::String(preview_id) },
    );
    envelope.insert("array_count".into(), Value::from(ext.arrays.len()));
    envelope.insert("read_action".into(), Value::String("read_result_array".into()));
    envelope.insert("max_chunk_values".into(), Value::from(4096));
    if encode(&Value::Object(envelope.clone())).len() > max_envelope_bytes {
        return Err(ArtifactError::Invalid(
            "authored artifact-view envelope exceeds 8 MiB after array externalization; no data truncated"
                .into(),
        ));
    }
    if !ext.arrays.is_empty() {
        let mut metadata = Map::new();
        metadata.insert("fields".into(), Value::Object(ext.metadata.clone()));
        metadata.insert(
            "scope".into(),
            Value::String("complete authored evaluation arrays; no inferred CAD identity".into()),
        );
        let artifact = ResultArtifactStore::new(root)?.create(&ext.arrays, &identities, &metadata)?;
        let references = array_references(&artifact, &ext.metadata);
        envelope.insert("evaluation".into(), resolve(&tree, &references));
        envelope.insert("artifact_id".into(), artifact.get("artifact_id").cloned().unwrap_or(Value::Null));
    }
    if encode(&Value::Object(envelope.clone())).len() > max_envelope_bytes {
        return Err(ArtifactError::Invalid(format!(
            "authored artifact-view envelope exceeds 8 MiB; stored artifact {}",
            implexity_core::pyobj::py_str(envelope.get("artifact_id").unwrap_or(&Value::Null))
        )));
    }
    Ok(Value::Object(envelope))
}


pub fn resolve_store(model_dir: &Path, artifact_id: &str) -> Result<ResultArtifactStore, ArrayReadError> {
    let valid = artifact_id.strip_prefix("result-").is_some_and(|r| {
        r.len() == 32 && r.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    });
    if !valid {
        return Err(ArrayReadError::Value("invalid artifact identity".into()));
    }
    let mut matches: Vec<PathBuf> = Vec::new();
    for name in ROOTS {
        let root = model_dir.join("opt").join(name);
        let candidates = if name.ends_with("_provider") {
            vec![root.clone(), root.join("design_states")]
        } else {
            vec![root]
        };
        for candidate in candidates {
            if candidate.join(artifact_id).join("manifest.json").is_file() {
                matches.push(candidate);
            }
        }
    }
    if matches.is_empty() {
        return Err(ArrayReadError::NotFound(artifact_id.into()));
    }
    if matches.len() > 1 {
        let keys = ["schema", "identities", "fields", "metadata", "payload_sha256"];
        let docs: Vec<Map<String, Value>> = matches
            .iter()
            .map(|r| ResultArtifactStore::new(r).and_then(|s| s.get(artifact_id)))
            .collect::<Result<_, _>>()?;
        let project = |d: &Map<String, Value>| -> Vec<Option<Value>> {
            keys.iter().map(|k| d.get(*k).cloned()).collect()
        };
        let reference = project(&docs[0]);
        if docs[1..].iter().any(|d| project(d) != reference) {
            return Err(ArrayReadError::Value("ambiguous result artifact content".into()));
        }
    }
    Ok(ResultArtifactStore::new(&matches[0])?)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArrayChunk {
    Raw(Vec<u8>),
    Json(Vec<Value>),
}


pub fn read_array(
    store: &ResultArtifactStore,
    artifact_id: &str,
    field: &str,
    offset: i64,
    count: Option<i64>,
    encoding: &str,
) -> Result<(Map<String, Value>, ArrayChunk), ArrayReadError> {
    if encoding != "raw" && encoding != "json" {
        return Err(ArrayReadError::Value("array encoding must be raw or json".into()));
    }
    let manifest = store.get(artifact_id)?;
    if !manifest.get("fields").and_then(Value::as_object).is_some_and(|f| f.contains_key(field)) {
        return Err(ArrayReadError::NotFound(implexity_core::py_repr::repr_str(field)));
    }
    let npz = implexity_io::npz::load_file(&store.payload_path(artifact_id)?)
        .map_err(|e| ArrayReadError::Value(e.to_string()))?;
    let Some(array) = npz.get(field) else {
        return Err(ArrayReadError::NotFound(implexity_core::py_repr::repr_str(field)));
    };
    let numeric = !matches!(array.data, NpyData::Bool(_) | NpyData::Unicode { .. } | NpyData::Bytes { .. });
    if !numeric || !is_finite_data(&array.data) {
        return Err(ArrayReadError::Value("result array is not finite numeric data".into()));
    }
    if encoding == "json" {

    }
    let total = i64::try_from(array.data.len()).unwrap_or(i64::MAX);
    let max_values = i64::try_from(MAX_VALUES).unwrap_or(i64::MAX);
    let count = count.unwrap_or_else(|| (total - offset).min(max_values));
    if offset < 0 || count < 1 || count > max_values || offset + count > total {
        return Err(ArrayReadError::Value("array range outside data or transfer limit".into()));
    }
    let bytes = data_bytes(array);
    let itemsize = bytes.len() / array.data.len().max(1);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let (start, end) = (offset as usize * itemsize, (offset + count) as usize * itemsize);
    let body = bytes[start..end].to_vec();
    let metadata = manifest
        .get("metadata")
        .and_then(|m| m.get("fields"))
        .and_then(|f| f.get(field))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut header = Map::new();
    header.insert("schema".into(), Value::String("implexity-result-array/1".into()));
    header.insert("artifact_id".into(), Value::String(artifact_id.into()));
    header.insert("field".into(), Value::String(field.into()));
    header.insert("shape".into(), Value::Array(array.shape.iter().map(|d| Value::from(*d)).collect()));
    header.insert("dtype".into(), Value::String(dtype_str(&array.data)));
    header.insert("order".into(), Value::String("C".into()));
    header.insert("offset".into(), Value::from(offset));
    header.insert("count".into(), Value::from(count));
    header.insert("total_values".into(), Value::from(total));
    header.insert("exact_sha256".into(), Value::String(sha256_hex(&bytes)));
    header.insert("chunk_sha256".into(), Value::String(sha256_hex(&body)));
    header.insert("metadata".into(), metadata);
    header.insert("identities".into(), manifest.get("identities").cloned().unwrap_or(Value::Null));
    if encoding == "raw" {
        return Ok((header, ArrayChunk::Raw(body)));
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let values: Vec<Value> = flat_json(&array.data)[offset as usize..(offset + count) as usize].to_vec();
    if matches!(array.data, NpyData::C64(_) | NpyData::C128(_)) {
        header.insert("value_encoding".into(), Value::String("complex_real_imaginary_pairs".into()));
    }
    Ok((header, ArrayChunk::Json(values)))
}

