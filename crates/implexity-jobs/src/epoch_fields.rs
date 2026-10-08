// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use implexity_io::npy::{NpyArray, NpyData};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use crate::artifacts::{ArtifactError, Limits, ResultArtifactStore};
use crate::private::{canonical_text, sha256_hex};
use crate::result_arrays::data_bytes;

type AResult<T> = Result<T, ArtifactError>;

fn fail<T>(message: impl Into<String>) -> AResult<T> {
    Err(ArtifactError::Invalid(message.into()))
}

fn json_len(value: &Value) -> usize {
    canonical_text(value).len()
}

fn digest(value: &Value) -> String {
    sha256_hex(canonical_text(value).as_bytes())
}

const RECEIPT_KEYS: [&str; 6] =
    ["schema", "artifact_id", "payload_sha256", "identities_sha256", "metadata_sha256", "fields_sha256"];

#[derive(Debug)]
pub struct EpochFieldArtifactAdapter {
    store: ResultArtifactStore,
    max_bytes: u64,
    metadata_cap: usize,
    max_fields: usize,
    prepared: Option<BTreeMap<String, usize>>,
    limits: Limits,
}

impl EpochFieldArtifactAdapter {

    pub fn new(
        store: ResultArtifactStore,
        max_bytes: u64,
        max_metadata_bytes: usize,
        max_fields: usize,
    ) -> AResult<Self> {
        if max_bytes == 0 || max_metadata_bytes == 0 || max_fields == 0 {
            return fail("positive integer capture limits required");
        }
        let overhead =
            u64::try_from(max_fields).unwrap_or(u64::MAX).saturating_mul(16384 + 4096).saturating_add(65536);
        let limits = Limits {
            max_manifest_bytes: (max_metadata_bytes as u64).saturating_add(overhead),
            max_field_bytes: max_bytes,
            max_uncompressed_bytes: max_bytes.saturating_add(overhead),
            max_payload_bytes: max_bytes.saturating_add(overhead),
            max_fields,
            ..Limits::default()
        };
        Ok(Self { store, max_bytes, metadata_cap: max_metadata_bytes, max_fields, prepared: None, limits })
    }


    pub fn for_selection(root: &Path, selection: &Map<String, Value>) -> AResult<Self> {
        let max_bytes = selection.get("max_bytes").and_then(Value::as_u64).unwrap_or(0);
        let max_meta = selection.get("max_metadata_bytes").and_then(Value::as_u64).unwrap_or(0);
        Self::new(ResultArtifactStore::new(root)?, max_bytes, usize::try_from(max_meta).unwrap_or(0), 64)
    }

    fn metadata(&self, identities: &Value, metadata: &Value) -> AResult<usize> {
        if !identities.is_object() || !metadata.is_object() {
            return fail("typed identity and field metadata required");
        }
        let size = json_len(identities) + json_len(metadata);
        if size > self.metadata_cap {
            return fail("epoch identity/metadata budget exceeded");
        }
        Ok(size)
    }


    pub fn write(
        &self,
        fields: &BTreeMap<String, ArrayD<f64>>,
        identities: &Map<String, Value>,
        metadata: &Map<String, Value>,
    ) -> AResult<Map<String, Value>> {
        if metadata.values().any(|v| !v.is_object()) {
            return fail("typed field metadata mapping required");
        }
        let mut stored = Map::new();
        stored.insert("fields".into(), Value::Object(metadata.clone()));
        let stored = Value::Object(stored);
        self.metadata(&Value::Object(identities.clone()), &stored)?;
        if fields.is_empty() || fields.len() > self.max_fields {
            return fail("bounded nonempty fields required");
        }
        let mut size: u64 = 0;
        for value in fields.values() {
            size = size.saturating_add(u64::try_from(value.len()).unwrap_or(u64::MAX).saturating_mul(8));
            if size > self.max_bytes {
                return fail("epoch array budget exceeded");
            }
            if value.is_empty() || value.iter().any(|v| !v.is_finite()) {
                return fail("finite nonempty real field required");
            }
        }
        let Value::Object(stored) = stored else { return fail("typed field metadata mapping required") };
        let manifest = self.store.create_f64_streamed(fields, identities, &stored, &self.limits)?;
        Ok(Self::receipt(&manifest))
    }

    fn receipt(manifest: &Map<String, Value>) -> Map<String, Value> {
        let get = |k: &str| manifest.get(k).cloned().unwrap_or(Value::Null);
        let mut out = Map::new();
        out.insert("schema".into(), Value::String("implexity-epoch-field-artifact/1".into()));
        out.insert("artifact_id".into(), get("artifact_id"));
        out.insert("payload_sha256".into(), get("payload_sha256"));
        out.insert("identities_sha256".into(), Value::String(digest(&get("identities"))));
        out.insert("metadata_sha256".into(), Value::String(digest(&get("metadata"))));
        out.insert("fields_sha256".into(), Value::String(digest(&get("fields"))));
        out
    }

    fn inspect(&self, receipt: &Value) -> AResult<(Map<String, Value>, u64)> {
        let Some(r) = receipt
            .as_object()
            .filter(|r| r.len() == RECEIPT_KEYS.len() && RECEIPT_KEYS.iter().all(|k| r.contains_key(*k)))
        else {
            return fail("malformed epoch artifact receipt");
        };
        let id = r.get("artifact_id").and_then(Value::as_str).unwrap_or("");
        let (manifest, inspected) = self.store.inspect_streamed(id, &self.limits)?;
        if *r != Self::receipt(&manifest) {
            return fail("epoch artifact receipt identity mismatch");
        }
        let wrapper = manifest.get("metadata").and_then(Value::as_object);
        let well_formed = wrapper.is_some_and(|w| {
            w.len() == 1
                && w.get("fields")
                    .and_then(Value::as_object)
                    .is_some_and(|f| f.values().all(Value::is_object))
        });
        if !well_formed {
            return fail("canonical epoch metadata wrapper required");
        }
        self.metadata(
            manifest.get("identities").unwrap_or(&Value::Null),
            manifest.get("metadata").unwrap_or(&Value::Null),
        )?;
        if inspected.values().any(|row| !matches!(row.descr.chars().nth(1), Some('f' | 'i' | 'u'))) {
            return fail("non-real epoch field refused");
        }
        let size: u64 = inspected.values().map(|row| row.nbytes).sum();
        if size > self.max_bytes {
            return fail("epoch array budget exceeded");
        }
        Ok((manifest, size))
    }


    pub fn prepare_epoch(&mut self, receipts: &[Value]) -> AResult<()> {
        self.prepared = None;
        let mut admitted: BTreeMap<String, usize> = BTreeMap::new();
        let mut total: u64 = 0;
        let mut metadata_total = 0_usize;
        for receipt in receipts {
            let (manifest, size) = self.inspect(receipt)?;
            total = total.saturating_add(size);
            if total > self.max_bytes {
                return fail("aggregate epoch array budget exceeded");
            }
            metadata_total += self.metadata(
                manifest.get("identities").unwrap_or(&Value::Null),
                manifest.get("metadata").unwrap_or(&Value::Null),
            )?;
            if metadata_total > self.metadata_cap {
                return fail("aggregate epoch identity/metadata budget exceeded");
            }
            *admitted.entry(digest(receipt)).or_insert(0) += 1;
        }
        self.prepared = Some(admitted);
        Ok(())
    }


    pub fn read(&mut self, receipt: &Value, selected_fields: Option<&[String]>) -> AResult<EpochFieldRead> {
        let key = digest(receipt);
        match self.prepared.as_ref() {
            None => return fail("prepare whole epoch before reading arrays"),
            Some(p) if p.get(&key).copied().unwrap_or(0) < 1 => {
                return fail("unprepared or already consumed epoch receipt");
            }
            Some(_) => {}
        }
        let (manifest, _) = self.inspect(receipt)?;
        let id = receipt.get("artifact_id").and_then(Value::as_str).unwrap_or("");
        let fields = self.store.read_arrays_streamed(id, selected_fields, &self.limits)?;
        let (after, _) = self.inspect(receipt)?;
        if Self::receipt(&after) != Self::receipt(&manifest) {
            return fail("epoch artifact changed during read");
        }
        if let Some(p) = self.prepared.as_mut()
            && let Some(count) = p.get_mut(&key)
        {
            *count -= 1;
        }
        Ok(EpochFieldRead {
            fields: fields.into_iter().collect(),
            identity: manifest.get("identities").cloned().unwrap_or(Value::Null),
            metadata: manifest.get("metadata").and_then(|m| m.get("fields")).cloned().unwrap_or(Value::Null),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EpochFieldRead {
    pub fields: BTreeMap<String, NpyArray>,
    pub identity: Value,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EpochFieldError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("{0}")]
    MissingField(String),
}

fn unavailable(capture: &Value) -> Value {
    serde_json::json!({"available":false,"reason":capture.get("reason").cloned().unwrap_or(Value::String("epoch_fields_not_retained".into())),
        "retention_status":capture.get("status"),"requested_fields":capture.get("requested_fields"),
        "retention_budget_bytes":capture.get("retention_budget_bytes"),"retention_metadata_budget_bytes":capture.get("retention_metadata_budget_bytes"),
        "solve_started":false,"final_acceptance_performed":false})
}


#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn read_recorded_field(root: &Path, capture: &Value, epoch: &Value, design_state_id: &Value, checkpoint_sha256: &Value, field: &str, operating_point: i64, maximum_bytes: i64) -> Result<Value, EpochFieldError> {
    read_recorded_field_mode(root, capture, epoch, design_state_id, checkpoint_sha256, field, operating_point, maximum_bytes, "payload")
}

pub fn read_recorded_field_mode(
    root: &Path,
    capture: &Value,
    epoch: &Value,
    design_state_id: &Value,
    checkpoint_sha256: &Value,
    field: &str,
    operating_point: i64,
    maximum_bytes: i64,
    mode: &str,
) -> Result<Value, EpochFieldError> {
    let invalid = |m: &str| EpochFieldError::Artifact(ArtifactError::Invalid(m.to_string()));
    if !matches!(mode, "payload" | "summary") { return Err(invalid("field read mode must be payload or summary")); }
    let length = field.chars().count();
    if !(1..=256).contains(&length) {
        return Err(invalid("explicit bounded field name required"));
    }
    if operating_point < 0 {
        return Err(invalid("nonnegative operating point required"));
    }
    if maximum_bytes <= 0 || usize::try_from(maximum_bytes).is_err() {
        return Err(invalid("public read budget must be a positive platform-representable byte count"));
    }
    let Some(capture) =
        capture.as_object().filter(|c| c.get("status").and_then(Value::as_str) == Some("captured"))
    else {
        return Ok(unavailable(capture));
    };
    let source = capture.get("source");
    if capture.get("epoch") != Some(epoch)
        || capture.get("design_state_id") != Some(design_state_id)
        || source.and_then(|s| s.get("epoch")) != Some(epoch)
        || source.and_then(|s| s.get("checkpoint_sha256")) != Some(checkpoint_sha256)
    {
        return Err(invalid("captured field/checkpoint identity mismatch"));
    }
    let records: Vec<&Value> =
        capture.get("records").and_then(Value::as_array).map(|r| r.iter().collect()).unwrap_or_default();
    let matching: Vec<&&Value> = records
        .iter()
        .filter(|r| {
            let aggregate = r.get("identity").and_then(|i| i.get("scope")).and_then(Value::as_str)
                == Some("aggregate");
            let matching_point = r.get("identity").and_then(|i| i.get("operating_point")).and_then(Value::as_i64)
                == Some(operating_point);
            (aggregate || matching_point)
                && r.get("field_specs").and_then(Value::as_object).is_some_and(|s| s.contains_key(field))
        })
        .collect();
    if matching.len() != 1 {
        if matching.is_empty() {
            return Err(EpochFieldError::MissingField(implexity_core::py_repr::repr_str(field)));
        }
        return Err(invalid("no unique retained field scope"));
    }
    let record = *matching[0];
    if record.get("identity").and_then(|i| i.get("source")) != source {
        return Err(invalid("field source differs from the recorded epoch"));
    }
    let config = crate::epoch_capture::normalise_selection(capture.get("selection"))
        .map_err(|e| invalid(e.message()))?
        .ok_or_else(|| invalid("epoch field selection is missing"))?;
    let mut adapter = EpochFieldArtifactAdapter::for_selection(root, &config)?;
    let receipts: Vec<Value> =
        records.iter().map(|r| r.get("artifact").cloned().unwrap_or(Value::Null)).collect();
    adapter.prepare_epoch(&receipts)?;
    let specs = record.get("field_specs").and_then(Value::as_object);
    let hashes = record.get("field_sha256").and_then(Value::as_object);
    let (Some(spec), Some(expected_digest)) =
        (specs.and_then(|s| s.get(field)), hashes.and_then(|h| h.get(field)))
    else {
        return Err(EpochFieldError::MissingField(implexity_core::py_repr::repr_str(field)));
    };
    let shape: Vec<u64> = match spec.get("shape").and_then(Value::as_array) {
        Some(items) if items.iter().all(Value::is_u64) => items.iter().filter_map(Value::as_u64).collect(),
        _ => return Err(invalid("invalid recorded field shape")),
    };
    let dtype = spec.get("dtype").and_then(Value::as_str).unwrap_or("");
    let itemsize: u64 = dtype.get(2..).and_then(|s| s.parse().ok()).unwrap_or(0);
    let nbytes = shape.iter().try_fold(1_u64, |a, d| a.checked_mul(*d)).and_then(|n| n.checked_mul(itemsize));
    let read_budget = if mode == "summary" { 256 * 1024 * 1024 } else { u64::try_from(maximum_bytes).unwrap_or(0) };
    if nbytes.is_none_or(|n| n > read_budget) {
        return Err(invalid("selected field exceeds requested public read budget"));
    }
    let nbytes = nbytes.unwrap_or(0);
    let retained =
        adapter.read(record.get("artifact").unwrap_or(&Value::Null), Some(&[field.to_string()]))?;
    if Some(&retained.identity) != record.get("identity") {
        return Err(invalid("artifact/epoch source identities disagree"));
    }
    let Some(array) = retained.fields.get(field) else {
        return Err(EpochFieldError::MissingField(implexity_core::py_repr::repr_str(field)));
    };
    let payload = data_bytes(array);
    let sha = sha256_hex(&payload);
    let actual_shape: Vec<u64> = array.shape.iter().map(|d| u64::try_from(*d).unwrap_or(u64::MAX)).collect();
    if actual_shape != shape
        || array.data.descr() != dtype
        || Some(sha.as_str()) != expected_digest.as_str()
        || u64::try_from(payload.len()).ok() != Some(nbytes)
    {
        return Err(invalid("retained field content identity mismatch"));
    }
    let mut out = Map::new();
    out.insert("schema".into(), Value::String("implexity-epoch-field-read/1".into()));
    out.insert("available".into(), Value::Bool(true));
    out.insert("epoch".into(), epoch.clone());
    out.insert("operating_point".into(), record["identity"]["operating_point"].clone());
    out.insert("scope".into(), record["identity"].get("scope").cloned().unwrap_or(Value::String("operating_point".into())));
    out.insert("operating_points".into(), record["identity"].get("operating_points").cloned().unwrap_or(serde_json::json!([operating_point])));
    out.insert("design_state_id".into(), design_state_id.clone());
    out.insert("checkpoint_sha256".into(), checkpoint_sha256.clone());
    out.insert("solve_id".into(), source.and_then(|s| s.get("solve_id")).cloned().unwrap_or(Value::Null));
    out.insert("field".into(), Value::String(field.into()));
    out.insert("metadata".into(), retained.metadata.get(field).cloned().unwrap_or(Value::Null));
    out.insert("dtype".into(), Value::String(array.data.descr()));
    out.insert("shape".into(), Value::Array(array.shape.iter().map(|d| Value::from(*d)).collect()));
    out.insert("encoding".into(), Value::String(if mode == "summary" { "summary" } else { "base64-C" }.into()));
    out.insert("sha256".into(), Value::String(sha));
    out.insert("bytes".into(), Value::from(payload.len()));
    if mode == "payload" {
        out.insert("data".into(), Value::String(base64::engine::general_purpose::STANDARD.encode(&payload)));
    } else {
        let values: Option<Box<dyn Iterator<Item=f64> + '_>> = match &array.data {
            NpyData::F64(v) => Some(Box::new(v.iter().copied())),
            NpyData::F32(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::I8(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::U8(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::I16(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::U16(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::I32(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::U32(v) => Some(Box::new(v.iter().map(|x| f64::from(*x)))),
            NpyData::I64(v) => Some(Box::new(v.iter().map(|x| *x as f64))),
            NpyData::U64(v) => Some(Box::new(v.iter().map(|x| *x as f64))),
            NpyData::Bool(v) => Some(Box::new(v.iter().map(|x| if *x { 1.0 } else { 0.0 }))),
            _ => None,
        };
        let mut count = 0usize;
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        if let Some(values) = values { for value in values.filter(|v| v.is_finite()) { count += 1; low = low.min(value); high = high.max(value); } }
        out.insert("finite_count".into(), Value::from(count));
        out.insert("finite_min".into(), if count == 0 { Value::Null } else { serde_json::json!(low) });
        out.insert("finite_max".into(), if count == 0 { Value::Null } else { serde_json::json!(high) });
    }
    out.insert("solve_started".into(), Value::Bool(false));
    out.insert("live_model_mutated".into(), Value::Bool(false));
    out.insert("final_acceptance_performed".into(), Value::Bool(false));
    Ok(Value::Object(out))
}

