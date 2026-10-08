// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::contracts::{CaeProvider, FieldValue, ProviderProblem};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::NamedArrays;
use implexity_optim::design_identity;
use implexity_optim::provider_ops::{CachedEvaluation, design_operations};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use crate::private::{canonical_text, compact_text, sha256_hex, token_hex};
use implexity_solve::state_store::{AdaptiveStore, SnapshotStore, StoreBudget};

pub const SCHEMA: &str = "implexity-epoch-field-selection/1";

pub fn capture_budget(name: &str, default: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(value) => value.trim().parse::<u64>().map_err(|_| format!("{name} must be an integer byte budget")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(format!("{name} is not valid Unicode")),
    }
}

fn value_error<T>(message: impl Into<String>) -> Result<T, String> {
    Err(message.into())
}

#[must_use]
pub fn digest(value: &Value) -> String {
    sha256_hex(canonical_text(value).as_bytes())
}

#[must_use]
pub fn array_to_list(array: &ArrayD<f64>) -> Value {
    fn nest(data: &[f64], shape: &[usize]) -> Value {
        match shape.split_first() {
            None => data.first().map_or(Value::Null, |v| Value::from(*v)),
            Some((&n, rest)) => {
                let stride: usize = rest.iter().product();
                Value::Array((0..n).map(|i| nest(&data[i * stride..(i + 1) * stride], rest)).collect())
            }
        }
    }
    let data: Vec<f64> = array.iter().copied().collect();
    nest(&data, array.shape())
}

#[must_use]
pub fn design_to_value(design: &NamedArrays) -> Value {
    Value::Object(design.iter().map(|(k, v)| (k.to_string(), array_to_list(v))).collect())
}

fn byte_len(value: &Value) -> usize {
    compact_text(value).len()
}


pub fn mapped_identity(
    diagnostics: &Map<String, Value>,
    problem: &Value,
    design: &NamedArrays,
    max_bytes: usize,
) -> Result<Map<String, Value>, String> {
    let evidence = diagnostics.get("geometry_design_map").filter(|v| !v.is_null());
    let declared = problem
        .get("context")
        .and_then(Value::as_object)
        .is_some_and(|c| c.get("geometry_design_map").is_some_and(|v| !v.is_null()));
    let Some(evidence) = evidence else {
        if declared {
            return value_error("mapped endpoint is missing geometry-map evidence");
        }
        return Ok(Map::new());
    };
    let Some(owned) = evidence.as_object() else {
        return value_error("malformed geometry-map evidence");
    };
    if canonical_text(evidence).len() > max_bytes {
        return value_error("geometry-map evidence byte budget exceeded");
    }
    for key in ["schema", "map_id", "design_state_id", "physical_design_state_id"] {
        if owned.get(key).and_then(Value::as_str).is_none_or(str::is_empty) {
            return value_error(format!("missing geometry-map identity {key}"));
        }
    }
    let identity = design_identity(design).map_err(|e| e.message().to_string())?;
    if owned["design_state_id"].as_str() != Some(identity.as_str()) {
        return value_error("mapped control identity differs from captured solve");
    }
    if diagnostics.get("design_state_id") != Some(&owned["design_state_id"]) {
        return value_error("mapped diagnostic control identity disagrees");
    }
    let mut out = Map::new();
    out.insert("geometry_design_map".into(), Value::Object(owned.clone()));
    out.insert("geometry_design_map_sha256".into(), Value::String(digest(evidence)));
    out.insert("physical_design_state_id".into(), owned["physical_design_state_id"].clone());
    Ok(out)
}

fn is_finite_number(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_number) && value.and_then(Value::as_f64).is_some_and(f64::is_finite)
}


#[allow(clippy::too_many_lines)]
pub fn endpoint_selection(
    fields: &BTreeMap<String, FieldValue>,
    metadata: &Map<String, Value>,
    requested: &[String],
    diagnostics: &Map<String, Value>,
) -> Result<(Vec<String>, BTreeMap<String, Map<String, Value>>), String> {
    let mut selected: Vec<String> = requested.to_vec();
    let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < selected.len() {
        let name = selected[index].clone();
        index += 1;
        if !visited.insert(name.clone()) {
            continue;
        }
        if !fields.contains_key(&name) || !metadata.contains_key(&name) {
            return value_error(format!("selected endpoint field or metadata missing: {name}"));
        }
        let Some(row) = metadata[&name].as_object() else {
            return value_error("field metadata must be a mapping");
        };
        for key in ["phase_mask", "physical_support_field", "reference_coordinate_field", "node_field"] {
            match row.get(key) {
                None | Some(Value::Null) => {}
                Some(Value::String(dependency)) if !dependency.is_empty() => {
                    if !selected.contains(dependency) {
                        selected.push(dependency.clone());
                    }
                }
                Some(_) => return value_error("field dependency must be a nonempty name"),
            }
        }
        if selected.len() > 64 {
            return value_error("endpoint dependency field budget exceeded");
        }
    }
    let mut normalized = BTreeMap::new();
    let mut recorded_time: Option<f64> = None;
    let mut recorded_index: Option<i64> = None;
    for name in &selected {
        let Some(mut row) = metadata[name].as_object().cloned() else {
            return value_error("field metadata must be a mapping");
        };
        let canonical = row.get("time_association").filter(|v| !v.is_null()).cloned();
        let legacy = row.get("temporal_association").and_then(Value::as_str);
        let mapped = match legacy {
            Some("final_stored_state") => Some("endpoint"),
            Some("time_invariant_design_derived" | "fixed_design_all_history_steps") => Some("design_static"),
            _ => None,
        };
        if let (Some(c), Some(m)) = (&canonical, mapped)
            && c.as_str() != Some(m)
        {
            return value_error("conflicting field temporal associations");
        }
        let association = match (&canonical, mapped) {
            (Some(c), _) => c.as_str().map(str::to_string),
            (None, Some(m)) => Some(m.to_string()),
            (None, None) => None,
        };
        let Some(association) =
            association.filter(|a| matches!(a.as_str(), "endpoint" | "interval" | "design_static"))
        else {
            return value_error("explicit endpoint, interval or design-static association required");
        };
        if (association == "endpoint" || association == "interval") && !is_finite_number(row.get("time_s")) {
            return value_error("endpoint field requires a finite explicit time");
        }
        if association == "interval" {
            let (start, end) = (row.get("interval_start_s"), row.get("interval_end_s"));
            let ordered = is_finite_number(start)
                && is_finite_number(end)
                && start.and_then(Value::as_f64) < end.and_then(Value::as_f64)
                && end.and_then(Value::as_f64) == row.get("time_s").and_then(Value::as_f64);
            if !ordered {
                return value_error("interval field requires ordered explicit bounds and matching endpoint");
            }
        }
        if association != "design_static" {
            let time = row.get("time_s").and_then(Value::as_f64);
            #[allow(clippy::float_cmp)]
            if recorded_time.is_some() && time != recorded_time {
                return value_error("selected fields belong to different stored times");
            }
            recorded_time = time;
            match row.get("time_index") {
                None | Some(Value::Null) => {}
                Some(v) => {
                    let Some(i) = v.as_i64().filter(|i| v.is_i64() && *i >= 0) else {
                        return value_error("time index must be a nonnegative integer");
                    };
                    if recorded_index.is_some_and(|r| r != i) {
                        return value_error("selected fields belong to different stored indices");
                    }
                    recorded_index = Some(i);
                }
            }
        }
        let axes_have_time = match row.get("axes") {
            Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some("time")),
            Some(Value::String(s)) => s.contains("time"),
            _ => false,
        };
        let history =
            row.get("association").map(implexity_core::pyobj::py_str).is_some_and(|s| s.contains("history"));
        if axes_have_time || history {
            return value_error("full histories are not endpoint fields");
        }
        row.insert("time_association".into(), Value::String(association));
        if row.get("association").and_then(Value::as_str) == Some("cell") && !row.contains_key("registration")
        {
            let Some(registration) = diagnostics.get("field_registration").and_then(Value::as_object) else {
                return value_error("cell endpoint field requires spatial registration");
            };
            row.insert("registration".into(), Value::Object(registration.clone()));
        }
        normalized.insert(name.clone(), row);
    }
    Ok((selected, normalized))
}

pub type WriterReceipt = Map<String, Value>;

pub type FieldWriter<'a> = dyn FnMut(
        &BTreeMap<String, ArrayD<f64>>,
        &Map<String, Value>,
        &Map<String, Value>,
    ) -> Result<WriterReceipt, String>
    + 'a;

struct Pending {
    identity: Map<String, Value>,
    fields: BTreeMap<String, (usize, Vec<usize>)>,
    metadata: Map<String, Value>,
    size: usize,
    metadata_size: usize,
}

pub struct BoundedFieldCapture<'a, 'b> {
    source: Value,
    names: Vec<String>,
    cap: usize,
    metadata_cap: usize,
    writer: &'a mut FieldWriter<'b>,
    pending: BTreeMap<String, Pending>,
    used: usize,
    metadata_used: usize,
    generation: Option<String>,
    storage: AdaptiveStore,
    storage_budget: StoreBudget,
    next_slot: usize,
}

impl std::fmt::Debug for BoundedFieldCapture<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedFieldCapture").field("names", &self.names).finish_non_exhaustive()
    }
}

fn field_array(value: &FieldValue) -> Option<&ArrayD<f64>> {
    match value {
        FieldValue::Array(a) => Some(a),
        FieldValue::Json(_) => None,
    }
}

impl<'a, 'b> BoundedFieldCapture<'a, 'b> {

    pub fn new(
        source_identity: &Map<String, Value>,
        selected_fields: &[String],
        max_bytes: usize,
        writer: &'a mut FieldWriter<'b>,
        max_metadata_bytes: usize,
    ) -> Result<Self, String> {
        if source_identity.is_empty() {
            return value_error("explicit source identities required");
        }
        if max_bytes == 0 {
            return value_error("positive byte cap and writer required");
        }
        if max_metadata_bytes == 0 {
            return value_error("positive metadata byte cap required");
        }
        let unique: std::collections::BTreeSet<&String> = selected_fields.iter().collect();
        if selected_fields.is_empty()
            || unique.len() != selected_fields.len()
            || selected_fields.iter().any(String::is_empty)
        {
            return value_error("explicit unique endpoint field selection required");
        }
        let ram = capture_budget("IMPLEXITY_EPOCH_STAGING_RAM_BYTES", implexity_io::storage_budget::default_memory_bytes())?;
        let storage_budget = StoreBudget { ram_bytes: ram.min(max_bytes as u64), disk_bytes: max_bytes as u64,
            disk_root: Some(std::env::temp_dir().join("implexity-epoch-staging")) };
        Ok(Self {
            source: Value::Object(source_identity.clone()),
            names: selected_fields.to_vec(),
            cap: max_bytes,
            metadata_cap: max_metadata_bytes,
            writer,
            pending: BTreeMap::new(),
            used: 0,
            metadata_used: 0,
            generation: None,
            storage: AdaptiveStore::new(&storage_budget), storage_budget, next_slot: 0,
        })
    }


    pub fn begin_candidate(&mut self) -> Result<(), String> {
        self.pending.clear();
        self.storage = AdaptiveStore::new(&self.storage_budget);
        self.next_slot = 0;
        self.used = 0;
        self.metadata_used = 0;
        self.generation = Some(token_hex(16).map_err(|e| e.to_string())?);
        Ok(())
    }

    fn identity(
        &self,
        provider: &str,
        problem: &Value,
        design: &NamedArrays,
        operating_point: usize,
        metadata: &Map<String, Value>,
    ) -> Map<String, Value> {
        let mut identity = Map::new();
        identity.insert("source".into(), self.source.clone());
        identity.insert("provider".into(), Value::String(provider.into()));
        identity.insert("problem_sha256".into(), Value::String(digest(problem)));
        identity.insert("design_sha256".into(), Value::String(digest(&design_to_value(design))));
        identity.insert("operating_point".into(), Value::from(operating_point));
        identity.insert("metadata_sha256".into(), Value::String(digest(&Value::Object(metadata.clone()))));
        identity
    }


    pub fn capture(
        &mut self,
        fields: &BTreeMap<String, FieldValue>,
        diagnostics: &Map<String, Value>,
        provider: &str,
        problem: &Value,
        design: &NamedArrays,
        operating_point: usize,
    ) -> Result<Value, String> {
        let Some(generation) = self.generation.clone() else {
            return value_error("candidate scope required");
        };
        if provider.is_empty() {
            return value_error("explicit provider/operating point required");
        }
        if let Some(claimed) = diagnostics.get("design_state_id").filter(|v| !v.is_null()) {
            let identity = design_identity(design).map_err(|e| e.message().to_string())?;
            if claimed.as_str() != Some(identity.as_str()) {
                return value_error("cached field design identity differs from selected design");
            }
        }
        let empty = Map::new();
        let metadata = match diagnostics.get("field_metadata") {
            None => &empty,
            Some(Value::Object(m)) => m,
            Some(_) => return value_error("field metadata required"),
        };
        let (selected, metadata) = endpoint_selection(fields, metadata, &self.names, diagnostics)?;
        let mut owned = BTreeMap::new();
        let mut meta = Map::new();
        let mut size = 0_usize;
        for name in &selected {
            let (Some(value), Some(row)) = (fields.get(name), metadata.get(name)) else {
                return value_error(format!("selected endpoint field or metadata missing: {name}"));
            };
            let Some(array) = field_array(value).filter(|a| !a.is_empty() && a.iter().all(|v| v.is_finite()))
            else {
                return value_error("finite nonempty real endpoint field required");
            };
            size = size.checked_add(array.len().checked_mul(8).ok_or("endpoint size overflow")?).ok_or("endpoint size overflow")?;
            if self.used.checked_add(size).is_none_or(|n| n > self.cap) {
                return value_error(format!("candidate endpoint fields need {} bytes; requested retention budget is {} bytes", self.used.saturating_add(size), self.cap));
            }
            let encoded = Value::Object(row.clone());
            if byte_len(&encoded) > self.metadata_cap {
                return value_error("endpoint metadata byte budget exceeded");
            }
            let slot = self.next_slot;
            self.next_slot = self.next_slot.checked_add(1).ok_or("endpoint slot overflow")?;
            owned.insert(name.clone(), (slot, array.shape().to_vec()));
            meta.insert(name.clone(), encoded);
        }
        let mapped = mapped_identity(diagnostics, problem, design, self.metadata_cap)?;
        let mut metadata_size = byte_len(&Value::Object(meta.clone()));
        if !mapped.is_empty() {
            metadata_size += byte_len(&Value::Object(mapped.clone()));
        }
        if self.metadata_used + metadata_size > self.metadata_cap {
            return value_error("endpoint metadata byte budget exceeded");
        }
        let mut identity = self.identity(provider, problem, design, operating_point, &meta);
        identity.extend(mapped);
        let token = token_hex(16).map_err(|e| e.to_string())?;
        let mut staged = Vec::new();
        for (name, (slot, _)) in &owned {
            let array = field_array(&fields[name]).ok_or("validated endpoint field unavailable")?;
            let result = if let Some(data) = array.as_slice() { self.storage.put(*slot, 0, data) } else {
                let data: Vec<f64> = array.iter().copied().collect(); self.storage.put(*slot, 0, &data)
            };
            if let Err(error) = result {
                for staged_slot in staged { self.storage.release(staged_slot).map_err(|e| e.message().to_string())?; }
                return value_error(error.message());
            }
            staged.push(*slot);
        }
        self.pending.insert(
            token.clone(),
            Pending { identity: identity.clone(), fields: owned, metadata: meta, size, metadata_size },
        );
        self.used += size;
        self.metadata_used += metadata_size;
        let mut out = Map::new();
        out.insert("schema".into(), Value::String("pending-endpoint-fields/1".into()));
        out.insert("token".into(), Value::String(token));
        out.insert("generation".into(), Value::String(generation));
        out.insert("identity".into(), Value::Object(identity));
        Ok(Value::Object(out))
    }


    pub fn commit(
        &mut self,
        selection: &Value,
        provider: &str,
        problem: &Value,
        design: &NamedArrays,
        operating_point: usize,
    ) -> Result<Value, String> {
        let generation = selection.get("generation").and_then(Value::as_str);
        if generation.is_none() || generation != self.generation.as_deref() {
            return value_error("stale candidate field token");
        }
        let Some(token) = selection.get("token").and_then(Value::as_str).map(str::to_string) else {
            return value_error("unknown or already committed field token");
        };
        let Some(entry) = self.pending.get(&token) else {
            return value_error("unknown or already committed field token");
        };
        let mut expected = self.identity(provider, problem, design, operating_point, &entry.metadata);
        if let Some(map) = entry.identity.get("geometry_design_map").and_then(Value::as_object) {
            let mut diagnostics = Map::new();
            diagnostics.insert("geometry_design_map".into(), Value::Object(map.clone()));
            diagnostics
                .insert("design_state_id".into(), map.get("design_state_id").cloned().unwrap_or(Value::Null));
            expected.extend(mapped_identity(&diagnostics, problem, design, self.metadata_cap)?);
        }
        if Value::Object(entry.identity.clone()) != Value::Object(expected)
            || selection.get("identity") != Some(&Value::Object(entry.identity.clone()))
        {
            return value_error("selected solve identity differs from captured fields");
        }
        let writer_fields: BTreeMap<String, ArrayD<f64>> = entry.fields.iter().map(|(name,(slot,shape))| {
            let (_, data) = self.storage.get(*slot).map_err(|e| e.message().to_string())?;
            let array = ArrayD::from_shape_vec(ndarray::IxDyn(shape), data).map_err(|e| e.to_string())?;
            Ok((name.clone(),array))
        }).collect::<Result<_,String>>()?;
        let hashes: BTreeMap<String,String> = writer_fields.iter().map(|(k,v)|(k.clone(),array_sha256(v))).collect();
        let writer_identity = entry.identity.clone();
        let writer_metadata = entry.metadata.clone();
        let receipt = (self.writer)(&writer_fields, &writer_identity, &writer_metadata)?;
        let receipt_ok = receipt.get("artifact_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
        if !receipt_ok {
            return value_error("immutable artifact writer receipt required");
        }
        let entry = self.pending.remove(&token).ok_or("unknown or already committed field token")?;
        let mut specs = Map::new();
        for (k, (slot, shape)) in &entry.fields {
            self.storage.release(*slot).map_err(|e| e.message().to_string())?;
            let mut spec = Map::new();
            spec.insert("shape".into(), Value::Array(shape.iter().map(|d| Value::from(*d)).collect()));
            spec.insert("dtype".into(), Value::String("<f8".into()));
            specs.insert(k.clone(), Value::Object(spec));
        }
        let mut out = Map::new();
        out.insert("schema".into(), Value::String("selected-endpoint-fields/1".into()));
        out.insert("identity".into(), Value::Object(entry.identity));
        out.insert("artifact".into(), Value::Object(receipt));
        out.insert(
            "field_sha256".into(),
            Value::Object(hashes.into_iter().map(|(k, v)| (k, Value::String(v))).collect()),
        );
        out.insert("field_specs".into(), Value::Object(specs));
        out.insert("fields".into(), Value::Object(entry.metadata.clone()));
        out.insert("captured_without_additional_evaluation".into(), Value::Bool(true));
        self.used -= entry.size;
        self.metadata_used -= entry.metadata_size;
        Ok(Value::Object(out))
    }

    pub fn reject_candidate(&mut self) {
        self.pending.clear();
        self.storage = AdaptiveStore::new(&self.storage_budget);
        self.next_slot = 0;
        self.used = 0;
        self.metadata_used = 0;
        self.generation = None;
    }
}

#[must_use]
pub fn array_sha256(array: &ArrayD<f64>) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for v in array {
        hasher.update(v.to_le_bytes());
    }
    hex::encode(hasher.finalize())
}


pub fn normalise_selection(raw: Option<&Value>) -> CaeResult<Option<Map<String, Value>>> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let allowed = ["schema", "fields", "max_bytes", "max_metadata_bytes", "every", "include_sensitivities"];
    let Some(m) = raw.as_object().filter(|m| m.keys().all(|k| allowed.contains(&k.as_str()))) else {
        return Err(CaeError::contract("unsupported epoch field selection"));
    };
    if m.get("schema").is_some_and(|s| s.as_str() != Some(SCHEMA)) {
        return Err(CaeError::contract("unsupported epoch field schema"));
    }
    let fields = m.get("fields").and_then(Value::as_array);
    let valid = fields.is_some_and(|f| {
        let names: Vec<&str> = f.iter().filter_map(Value::as_str).collect();
        let unique: std::collections::BTreeSet<&&str> = names.iter().collect();
        (!f.is_empty() || m.get("include_sensitivities") == Some(&Value::Bool(true)))
            && f.len() <= 64
            && names.len() == f.len()
            && names.iter().all(|n| !n.is_empty())
            && unique.len() == names.len()
    });
    let (true, Some(fields)) = (valid, fields) else {
        return Err(CaeError::contract("explicit unique endpoint fields required"));
    };
    let mut out = Map::new();
    out.insert("schema".into(), Value::String(SCHEMA.into()));
    out.insert("fields".into(), Value::Array(fields.clone()));
    if let Some(value) = m.get("include_sensitivities") {
        if !value.is_boolean() {
            return Err(CaeError::contract("include_sensitivities must be boolean"));
        }
        out.insert("include_sensitivities".into(), value.clone());
    }
    let capacity = implexity_io::storage_budget::default_memory_bytes();
    let defaults = [
        ("max_bytes", capture_budget("IMPLEXITY_EPOCH_FIELD_BUDGET_BYTES", capacity).map_err(CaeError::contract)?),
        ("max_metadata_bytes", capture_budget("IMPLEXITY_EPOCH_METADATA_BUDGET_BYTES", (capacity / 64).max(65536)).map_err(CaeError::contract)?),
        ("every", 1),
    ];
    for (name, default) in defaults {
        let value = match m.get(name) { None => Some(default), Some(v) => v.as_u64() };
        let Some(value) = value.filter(|v| *v > 0 && usize::try_from(*v).is_ok() && i64::try_from(*v).is_ok()) else {
            return Err(CaeError::contract(format!("{name} must be a positive platform-representable integer for epoch capture")));
        };
        out.insert(name.into(), Value::from(value));
    }
    Ok(Some(out))
}


#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn capture_cached_epoch(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    operating_points: &[usize],
    epoch: i64,
    source_identity: &Map<String, Value>,
    selection: Option<&Value>,
    writer: &mut FieldWriter<'_>,
) -> CaeResult<Value> {
    let config = normalise_selection(selection)?;
    let mut base = Map::new();
    base.insert("schema".into(), Value::String("implexity-recorded-epoch-fields/1".into()));
    base.insert("epoch".into(), Value::from(epoch));
    base.insert("design_state_id".into(), Value::String(design_identity(design)?));
    base.insert("additional_physics_evaluations".into(), Value::from(0));
    base.insert("engineering_acceptance".into(), Value::Bool(false));
    if let Some(selection)=&config {
        base.insert("requested_fields".into(),selection["fields"].clone());
        base.insert("retention_budget_bytes".into(),selection["max_bytes"].clone());
        base.insert("retention_metadata_budget_bytes".into(),selection["max_metadata_bytes"].clone());
    }
    let with = |status: &str, extra: &[(&str, Value)]| -> Value {
        let mut out = base.clone();
        out.insert("status".into(), Value::String(status.into()));
        for (k, v) in extra {
            out.insert((*k).to_string(), v.clone());
        }
        if !out.contains_key("records") {
            out.insert("records".into(), Value::Array(Vec::new()));
        }
        Value::Object(out)
    };
    let every = config.as_ref().and_then(|c| c.get("every")).and_then(Value::as_i64).unwrap_or(1);
    let Some(config) = config.filter(|_| epoch.rem_euclid(every) == 0) else {
        return Ok(with("not_requested", &[]));
    };
    let Some(ops) = design_operations(provider) else {
        return Ok(with("unavailable", &[("reason", Value::String("cache_hook_missing".into()))]));
    };
    let provider_id = provider.name().to_string();
    if provider_id.is_empty() {
        return Ok(with("unavailable", &[("reason", Value::String("provider_identity_missing".into()))]));
    }
    let problem_value = match ops.problem_document(problem) {
        Some(Ok(v)) => v,
        Some(Err(e)) => return Err(e),
        None => Value::Null,
    };
    let names: Vec<String> = config["fields"]
        .as_array()
        .map(|f| f.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    if names.is_empty() {
        return Ok(with("not_requested", &[]));
    }
    let max_bytes = usize::try_from(config["max_bytes"].as_i64().unwrap_or(1)).unwrap_or(1);
    let max_meta = usize::try_from(config["max_metadata_bytes"].as_i64().unwrap_or(1)).unwrap_or(1);
    let mut capture = BoundedFieldCapture::new(source_identity, &names, max_bytes, writer, max_meta)
        .map_err(CaeError::contract)?;
    capture.begin_candidate().map_err(CaeError::contract)?;
    let mut tokens = Vec::new();
    let result = (|| -> Result<Value, String> {
        for point in operating_points {
            let raw = match ops.cached_evaluation_design(problem, design, *point) {
                Some(Ok(v)) => v,
                Some(Err(e)) => return Err(format!("{}: {}", e.python_class(), e.message())),

                None => {
                    return Ok(with(
                        "unavailable",
                        &[("reason", Value::String("cache_hook_missing".into()))],
                    ));
                }
            };
            let evaluation = match raw {
                CachedEvaluation::Unavailable(m) => {
                    let reason: String = m
                        .get("reason")
                        .map_or_else(|| "cache_unavailable".to_string(), implexity_core::pyobj::py_str)
                        .chars()
                        .take(512)
                        .collect();
                    return Ok(with("unavailable", &[("reason", Value::String(reason))]));
                }
                CachedEvaluation::Available(e) => e,
            };
            let token = capture
                .capture(
                    &evaluation.fields,
                    &evaluation.diagnostics,
                    &provider_id,
                    &problem_value,
                    design,
                    *point,
                )
                .map_err(|m| format!("ValueError: {m}"))?;
            tokens.push((*point, token));
        }
        let mut records = Vec::new();
        for (point, token) in &tokens {
            records.push(
                capture
                    .commit(token, &provider_id, &problem_value, design, *point)
                    .map_err(|m| format!("ValueError: {m}"))?,
            );
        }
        Ok(with(
            "captured",
            &[
                ("records", Value::Array(records)),
                ("selection", Value::Object(config.clone())),
                ("source", Value::Object(source_identity.clone())),
            ],
        ))
    })();
    capture.reject_candidate();
    match result {
        Ok(v) => Ok(v),
        Err(message) => {
            let reason: String = message.chars().take(1024).collect();
            Ok(with("unavailable", &[("reason", Value::String(reason))]))
        }
    }
}


#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn retain_epoch_sensitivities(
    capture: &mut Value,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    point: &implexity_optim::search::ExactPoint,
    total: &NamedArrays,
    operating_points: &[i64],
    responses: &[String],
    source: &Map<String, Value>,
    selection: Option<&Value>,
    writer: &mut FieldWriter<'_>,
) {
    let result = (|| -> Result<Vec<Value>, String> {
        let config = normalise_selection(selection)
            .map_err(|e| e.message().to_string())?
            .ok_or("epoch selection missing")?;
        if config.get("include_sensitivities") != Some(&Value::Bool(true)) {
            return Ok(Vec::new());
        }
        let every = config["every"].as_i64().unwrap_or(1);
        let epoch = source
            .get("epoch")
            .and_then(Value::as_i64)
            .ok_or("epoch missing")?;
        if epoch.rem_euclid(every) != 0 {
            return Ok(Vec::new());
        }
        let ops = design_operations(provider).ok_or("cache hook missing")?;
        let document = ops
            .problem_document(problem)
            .ok_or("problem identity unavailable")?
            .map_err(|e| e.message().to_string())?;
        let identity = design_identity(&point.design).map_err(|e| e.message().to_string())?;
        if capture.get("design_state_id").and_then(Value::as_str) != Some(identity.as_str()) {
            return Err("sensitivity design differs from published checkpoint".into());
        }
        let per_point = point.diagnostics.get("points").and_then(Value::as_array);
        let metadata_for = |index: usize| -> Result<Map<String, Value>, String> {
            if operating_points.len() == 1 && per_point.is_none() {
                Ok(point.diagnostics.clone())
            } else {
                per_point
                    .and_then(|rows| rows.get(index))
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or_else(|| "per-point sensitivity metadata unavailable".into())
            }
        };
        if operating_points.is_empty() || operating_points.iter().any(|p| *p < 0)
            || operating_points.iter().collect::<std::collections::BTreeSet<_>>().len() != operating_points.len() {
            return Err("explicit unique sensitivity operating points required".into());
        }
        let max_bytes = config["max_bytes"].as_u64().ok_or("retention byte budget missing")?;
        let ram = capture_budget("IMPLEXITY_EPOCH_STAGING_RAM_BYTES", implexity_io::storage_budget::default_memory_bytes())?;
        let mut staging = AdaptiveStore::new(&StoreBudget { ram_bytes: ram.min(max_bytes), disk_bytes: max_bytes,
            disk_root: Some(std::env::temp_dir().join("implexity-epoch-staging")) });
        let mut next_slot = 0usize;
        let mut pending = Vec::new();
        let mut bytes = 0usize;
        let mut metadata_bytes = 0usize;
        let mut scopes: Vec<(Option<i64>, Vec<(usize, String, &NamedArrays)>, Map<String, Value>)> =
            Vec::new();
        for (index, op) in operating_points.iter().enumerate() {
            for (response_index, response) in responses.iter().enumerate() {
                let key = crate::stage_search::point_key(*op, response);
                let gradients = point
                    .gradients
                    .get(&key)
                    .ok_or("response gradients unavailable")?;
                scopes.push((Some(*op), vec![(response_index, response.clone(), gradients)], metadata_for(index)?));
            }
        }
        let mut aggregate_meta = metadata_for(0)?;
        for index in 1..operating_points.len() {
            let other = metadata_for(index)?;
            for key in ["design_field_registrations", "design_field_layouts"] {
                if aggregate_meta.get(key) != other.get(key) {
                    aggregate_meta.remove(key);
                }
            }
        }
        scopes.push((None, vec![(0, "weighted_total".into(), total)], aggregate_meta));
        for (op, entries, diagnostics) in scopes {
            let registrations = diagnostics
                .get("design_field_registrations")
                .and_then(Value::as_object);
            let layouts = diagnostics
                .get("design_field_layouts")
                .and_then(Value::as_object);
            let mut arrays = BTreeMap::new();
            let mut metadata = Map::new();
            let mut scope_bytes = 0usize;
            for (response_index, response, gradients) in &entries {
                if gradients.names() != point.design.names() {
                    return Err("gradient coordinate layout differs from published design".into());
                }
                for (coordinate_index, (coordinate, gradient)) in gradients.iter().enumerate() {
                    let design = point
                        .design
                        .get(coordinate)
                        .ok_or("unknown gradient coordinate")?;
                    if gradient.shape() != design.shape()
                        || gradient.is_empty()
                        || gradient.iter().any(|x| !x.is_finite())
                    {
                        return Err("invalid retained sensitivity array".into());
                    }
                    let name = if op.is_none() {
                        format!("sensitivity_total_c{coordinate_index:03}")
                    } else {
                        format!("sensitivity_r{response_index:03}_c{coordinate_index:03}")
                    };
                    let registration = registrations.and_then(|m| m.get(coordinate));
                    let layout = layouts.and_then(|m| m.get(coordinate));
                    let split = gradient.ndim() == 4 && layout.is_some_and(|layout|
                        registration.is_some_and(|r| !r.is_null())
                        || layout.get("schema").and_then(Value::as_str) == Some("implexity-component-field-layout/2"));
                    let added_fields = 1 + if split { gradient.shape()[0] } else { 0 };
                    if arrays.len().saturating_add(added_fields) > 64 {
                        return Err("sensitivity field budget exceeded".into());
                    }
                    let added_bytes = gradient.len().checked_mul(if split { 16 } else { 8 })
                        .ok_or("sensitivity byte overflow")?;
                    scope_bytes = scope_bytes.checked_add(added_bytes).ok_or("sensitivity byte overflow")?;
                    if bytes.saturating_add(scope_bytes) > config["max_bytes"].as_u64().unwrap_or(0) as usize {
                        return Err(format!("sensitivity fields exceed retention budget: {} data bytes requested, budget {}", bytes.saturating_add(scope_bytes), config["max_bytes"]));
                    }
                    let mut row = serde_json::json!({"shape":gradient.shape(), "rank":"array",
                        "association":"design_coordinate", "signed":true, "response":response,
                        "parameter":coordinate, "source":"computed_optimization_gradient",
                        "scope":if op.is_some(){"operating_point"}else{"aggregate"},
                        "operating_points":operating_points, "registration":null})
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                    if gradient.ndim() == 3
                        && let Some(registration) = registration
                    {
                        let grid =
                            implexity_geometry::field_registration::GridRegistration::from_wire(
                                registration,
                            )
                            .map_err(|_| "invalid gradient registration")?;
                        if grid.shape.as_slice() != gradient.shape() {
                            return Err("gradient registration shape mismatch".into());
                        }
                        row.insert("rank".into(), Value::String("scalar".into()));
                        row.insert("association".into(), Value::String(grid.centering));
                        row.insert("registration".into(), registration.clone());
                    }
                    arrays.insert(name.clone(), gradient.clone());
                    metadata.insert(name.clone(), Value::Object(row));
                    if gradient.ndim() == 4
                        && let Some(layout) = layouts.and_then(|m| m.get(coordinate))
                        && (registration.is_some_and(|r| !r.is_null())
                            || layout.get("schema").and_then(Value::as_str) == Some("implexity-component-field-layout/2"))
                    {
                        implexity_solve::gradient_fields::add_registered_gradient_components(
                            &mut arrays,
                            &mut metadata,
                            &name,
                            gradient,
                            layout,
                            registration,
                        )
                        .map_err(|e| e.message().to_string())?;
                    }
                }
            }
            if arrays.is_empty() || arrays.len() > 64 {
                return Err("sensitivity field budget exceeded".into());
            }
            bytes = arrays
                .values()
                .try_fold(bytes, |sum, a| sum.checked_add(a.len().checked_mul(8)?))
                .ok_or("sensitivity byte overflow")?;
            let mut ident = Map::new();
            ident.insert("source".into(), Value::Object(source.clone()));
            ident.insert(
                "provider".into(),
                Value::String(provider.name().to_string()),
            );
            ident.insert("problem_sha256".into(), Value::String(digest(&document)));
            ident.insert(
                "design_sha256".into(),
                Value::String(digest(&design_to_value(&point.design))),
            );
            ident.insert("design_state_id".into(), Value::String(identity.clone()));
            ident.insert(
                "operating_point".into(),
                op.map_or(Value::Null, Value::from),
            );
            ident.insert(
                "scope".into(),
                Value::String(
                    if op.is_some() {
                        "operating_point"
                    } else {
                        "aggregate"
                    }
                    .into(),
                ),
            );
            ident.insert(
                "operating_points".into(),
                serde_json::json!(operating_points),
            );
            ident.extend(mapped_identity(
                &diagnostics,
                &document,
                &point.design,
                config["max_metadata_bytes"].as_u64().unwrap_or(0) as usize,
            )?);
            metadata_bytes = metadata_bytes
                .saturating_add(byte_len(&Value::Object(ident.clone())))
                .saturating_add(byte_len(&serde_json::json!({"fields":metadata})));
            if bytes > config["max_bytes"].as_u64().unwrap_or(0) as usize
                || metadata_bytes > config["max_metadata_bytes"].as_u64().unwrap_or(0) as usize
            {
                return Err(format!("sensitivity retention budget exceeded: {bytes} data bytes and {metadata_bytes} metadata bytes requested, budgets {} and {}",config["max_bytes"],config["max_metadata_bytes"]));
            }
            let mut stored = BTreeMap::new();
            for (name,array) in arrays {
                let slot = next_slot; next_slot = next_slot.checked_add(1).ok_or("sensitivity slot overflow")?;
                if let Some(data) = array.as_slice() { staging.put(slot,0,data).map_err(|e| e.message().to_string())?; }
                else { let data: Vec<f64> = array.iter().copied().collect(); staging.put(slot,0,&data).map_err(|e| e.message().to_string())?; }
                stored.insert(name,(slot,array.shape().to_vec()));
            }
            pending.push((stored, ident, metadata));
        }
        let existing = capture
            .get("records")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for record in &existing {
            metadata_bytes = metadata_bytes
                .saturating_add(byte_len(&record["identity"]))
                .saturating_add(byte_len(&serde_json::json!({"fields":record["fields"]})));
            let specs = record
                .get("field_specs")
                .and_then(Value::as_object)
                .ok_or("invalid endpoint specs")?;
            for spec in specs.values() {
                let shape: Vec<usize> = serde_json::from_value(spec["shape"].clone())
                    .map_err(|_| "invalid endpoint shape")?;
                let count = shape
                    .iter()
                    .try_fold(1usize, |a, b| a.checked_mul(*b))
                    .ok_or("endpoint size overflow")?;
                bytes = bytes
                    .checked_add(count.checked_mul(8).ok_or("endpoint size overflow")?)
                    .ok_or("epoch size overflow")?;
            }
        }
        if bytes > config["max_bytes"].as_u64().unwrap_or(0) as usize
            || metadata_bytes > config["max_metadata_bytes"].as_u64().unwrap_or(0) as usize
        {
            return Err(format!("combined epoch retention budget exceeded: {bytes} data bytes and {metadata_bytes} metadata bytes requested, budgets {} and {}",config["max_bytes"],config["max_metadata_bytes"]));
        }
        let mut records = Vec::new();
        for (stored, ident, metadata) in pending {
            let arrays: BTreeMap<String,ArrayD<f64>> = stored.iter().map(|(name,(slot,shape))| {
                let (_,data)=staging.get(*slot).map_err(|e|e.message().to_string())?;
                Ok((name.clone(),ArrayD::from_shape_vec(ndarray::IxDyn(shape),data).map_err(|e|e.to_string())?))
            }).collect::<Result<_,String>>()?;
            let receipt = writer(&arrays, &ident, &metadata)?;
            for (slot,_) in stored.values() {staging.release(*slot).map_err(|e|e.message().to_string())?;}
            if !receipt
                .get("artifact_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
            {
                return Err("artifact receipt missing".into());
            }
            let specs: Map<String, Value> = arrays
                .iter()
                .map(|(k, a)| {
                    (
                        k.clone(),
                        serde_json::json!({"shape":a.shape(),"dtype":"<f8"}),
                    )
                })
                .collect();
            let hashes: Map<String, Value> = arrays
                .iter()
                .map(|(k, a)| (k.clone(), Value::String(array_sha256(a))))
                .collect();
            records.push(
                serde_json::json!({"schema":"selected-endpoint-fields/1", "identity":ident,
                "artifact":receipt,"field_specs":specs,"field_sha256":hashes,
                "captured_without_additional_evaluation":true,"fields":metadata}),
            );
        }
        Ok(records)
    })();
    if let Some(out) = capture.as_object_mut() {
        match result {
            Ok(records) if !records.is_empty() => {
                out.insert(
                    "sensitivity_status".into(),
                    Value::String("captured".into()),
                );
                out.insert("source".into(), Value::Object(source.clone()));
                out.insert(
                    "selection".into(),
                    selection.cloned().unwrap_or(Value::Null),
                );
                out.insert("status".into(), Value::String("captured".into()));
                out.entry("records")
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .map(|r| r.extend(records));
            }
            Ok(_) => {}
            Err(reason) => {
                out.insert(
                    "sensitivity_status".into(),
                    Value::String("unavailable".into()),
                );
                out.insert(
                    "sensitivity_reason".into(),
                    Value::String(reason.chars().take(1024).collect()),
                );
            }
        }
    }
}
