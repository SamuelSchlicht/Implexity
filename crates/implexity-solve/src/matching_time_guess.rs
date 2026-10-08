// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use implexity_core::contracts::MatchingTimeNewtonGuess;
use implexity_core::error::{CaeError, CaeResult};
use implexity_core::json::{DumpOptions, canonical, dumps, parse_strict, sha256_hex};
use implexity_core::py_repr::repr_str;
use implexity_core::wire::fingerprint_value;
use implexity_io::npy::{NpyArray, parse_header};
use implexity_io::zip::{DEFLATED, ZipArchive};
use implexity_optim::design::{NamedArrays, design_identity};
use serde_json::{Map, Value, json};

pub const CAPSULE_SCHEMA: &str = "implexity-matching-time-newton-guess/1";
pub const REFERENCE_SCHEMA: &str = "implexity-matching-time-newton-guess-ref/1";
pub const CAPSULE_ROLE: &str = "matching_time_newton_guess_only";
pub const MAX_STATE_COUNT: usize = 64;
pub const MAX_STATE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_NPY_MEMBER_OVERHEAD: u64 = 64 * 1024;
pub const PAYLOAD_FORMAT: &str = "deterministic_npz_no_pickle";

const MANIFEST_KEYS: [&str; 12] = [
    "schema",
    "truth_status",
    "role",
    "canonical_cache_admission",
    "execution_identity",
    "provider_identity",
    "provenance",
    "states",
    "state_count",
    "uncompressed_state_bytes",
    "capsule_id",
    "payload",
];
const IDENTITY_KEYS: [&str; 10] = [
    "schema",
    "truth_status",
    "role",
    "canonical_cache_admission",
    "execution_identity",
    "provider_identity",
    "provenance",
    "states",
    "state_count",
    "uncompressed_state_bytes",
];
const PAYLOAD_KEYS: [&str; 4] = ["format", "filename", "bytes", "sha256"];
const STATE_KEYS: [&str; 5] = ["key", "shape", "dtype", "bytes", "sha256"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuessError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Missing(String),
}

impl From<GuessError> for CaeError {
    fn from(e: GuessError) -> Self {
        CaeError::contract(e.to_string())
    }
}

pub type GuessResult<T> = Result<T, GuessError>;

fn invalid(message: impl Into<String>) -> GuessError {
    GuessError::Invalid(message.into())
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[must_use]
pub fn is_capsule_id(s: &str) -> bool {
    s.strip_prefix("guess-").is_some_and(is_hex64)
}

fn strict_nonnegative_int(value: Option<&Value>, label: &str) -> GuessResult<u64> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("matching-time guess {label} must be a nonnegative integer")))
}

fn same_keys(map: &Map<String, Value>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|k| map.contains_key(*k))
}

fn strict_int(value: Option<&Value>) -> Option<i64> {
    value.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64)
}

#[allow(clippy::too_many_lines)]
fn strict_manifest(manifest: &Value) -> GuessResult<&Map<String, Value>> {
    let m = manifest
        .as_object()
        .filter(|m| same_keys(m, &MANIFEST_KEYS))
        .ok_or_else(|| invalid("matching-time guess manifest fields are missing or unexpected"))?;
    if m["schema"] != CAPSULE_SCHEMA {
        return Err(invalid("matching-time guess schema is unsupported"));
    }
    if m["truth_status"] != "exact_guarded_source"
        || m["role"] != CAPSULE_ROLE
        || m["canonical_cache_admission"] != false
    {
        return Err(invalid("matching-time guess attempts to claim unsupported state authority"));
    }
    let payload_error = || invalid("matching-time guess payload declaration is invalid");
    let payload =
        m["payload"].as_object().filter(|p| same_keys(p, &PAYLOAD_KEYS)).ok_or_else(payload_error)?;
    if payload["format"] != PAYLOAD_FORMAT
        || payload["filename"] != "states.npz"
        || !payload["sha256"].as_str().is_some_and(is_hex64)
    {
        return Err(payload_error());
    }
    strict_nonnegative_int(payload.get("bytes"), "payload byte count")?;
    if !m["execution_identity"].is_object() {
        return Err(invalid("matching-time guess execution identity is invalid"));
    }
    if m["provider_identity"].as_object().is_none_or(Map::is_empty) || !m["provenance"].is_object() {
        return Err(invalid("matching-time guess provider identity/provenance is invalid"));
    }
    if !m["capsule_id"].as_str().is_some_and(is_capsule_id) {
        return Err(invalid("matching-time guess capsule identity is invalid"));
    }
    let declaration = || invalid("matching-time guess state declaration is invalid");
    let states = m["states"].as_array().ok_or_else(declaration)?;
    let count = strict_int(m.get("state_count")).ok_or_else(declaration)?;
    if count <= 0 || count > 64 || usize::try_from(count).ok() != Some(states.len()) {
        return Err(declaration());
    }
    let mut declared_total = 0u64;
    for (index, row) in states.iter().enumerate() {
        let row = row
            .as_object()
            .filter(|r| same_keys(r, &STATE_KEYS) && r["key"] == format!("state_{index:06}").as_str())
            .ok_or_else(declaration)?;
        let shape = row["shape"].as_array();
        let size = shape.filter(|s| s.len() == 1).and_then(|s| strict_int(s.first())).filter(|n| *n > 0);
        let (Some(size), true, true) =
            (size, row["dtype"] == "<f8", row["sha256"].as_str().is_some_and(is_hex64))
        else {
            return Err(invalid(format!("matching-time guess state {index} declaration is invalid")));
        };
        let declared = strict_nonnegative_int(row.get("bytes"), &format!("state {index} byte count"))?;
        if u64::try_from(size).ok().map(|s| s * 8) != Some(declared) {
            return Err(invalid(format!("matching-time guess state {index} shape/byte count is invalid")));
        }
        declared_total += declared;
    }
    let uncompressed =
        strict_nonnegative_int(m.get("uncompressed_state_bytes"), "uncompressed state byte count")?;
    if uncompressed != declared_total || uncompressed > MAX_STATE_BYTES {
        return Err(invalid("matching-time guess uncompressed payload declaration is invalid"));
    }
    let execution = m["execution_identity"]
        .as_object()
        .ok_or_else(|| invalid("matching-time guess execution identity is invalid"))?;
    let required = [
        "schema",
        "provider_id",
        "provider_state_problem_id",
        "design_state_id",
        "source",
        "packages",
        "backend",
    ];
    let ok = same_keys(execution, &required)
        && execution["schema"] == "implexity-matching-time-execution-identity/1"
        && ["provider_id", "provider_state_problem_id", "design_state_id"]
            .iter()
            .all(|k| execution[*k].as_str().is_some_and(|s| !s.is_empty()))
        && ["source", "packages", "backend"].iter().all(|k| execution[*k].is_object());
    if !ok {
        return Err(invalid("matching-time guess execution identity is invalid"));
    }
    Ok(m)
}

fn identity_digest(manifest: &Map<String, Value>, payload_sha: &str) -> String {
    let mut doc: Map<String, Value> =
        IDENTITY_KEYS.iter().map(|k| ((*k).to_string(), manifest[*k].clone())).collect();
    doc.insert("payload_sha256".into(), json!(payload_sha));
    format!("guess-{}", sha256_hex(canonical(&Value::Object(doc)).as_bytes()))
}

fn sha256_file(path: &Path) -> GuessResult<String> {
    implexity_io::digest::sha256_file(path).map_err(|_| invalid("matching-time guess payload cannot be read"))
}

fn state_bytes(values: &[f64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn normalise_states(
    guess: &MatchingTimeNewtonGuess,
) -> GuessResult<(BTreeMap<String, NpyArray>, Vec<Value>, u64)> {
    if guess.states().len() > MAX_STATE_COUNT {
        return Err(invalid("matching-time guess exceeds the state-count limit"));
    }
    let mut arrays = BTreeMap::new();
    let mut rows = Vec::new();
    let mut total = 0u64;
    for (index, state) in guess.states().iter().enumerate() {
        let key = format!("state_{index:06}");
        let bytes = state_bytes(state);
        let nbytes = bytes.len() as u64;
        total += nbytes;
        rows.push(json!({
            "key": key, "shape": [state.len()], "dtype": "<f8", "bytes": nbytes,
            "sha256": sha256_hex(&bytes),
        }));
        arrays.insert(key, NpyArray::vector_f64(state.to_vec()));
    }
    if total > MAX_STATE_BYTES {
        return Err(invalid("matching-time guess exceeds the payload limit"));
    }
    Ok((arrays, rows, total))
}

fn npy_header(member: &[u8]) -> GuessResult<(Vec<usize>, bool, String, usize)> {
    if member.len() < 8 || &member[..6] != b"\x93NUMPY" {
        return Err(invalid("matching-time guess payload archive is invalid"));
    }
    let (len, start) = match (member[6], member[7]) {
        (1, 0) => (member.get(8..10).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]]))), 10),
        (2, 0) => (
            member
                .get(8..12)
                .and_then(|b| usize::try_from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok()),
            12,
        ),
        _ => return Err(invalid("matching-time guess NPY format is unsupported")),
    };
    let len = len.ok_or_else(|| invalid("matching-time guess payload archive is invalid"))?;
    let text = member
        .get(start..start + len)
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| invalid("matching-time guess payload archive is invalid"))?;
    let header = parse_header(text).map_err(|_| invalid("matching-time guess payload archive is invalid"))?;
    Ok((header.shape, header.fortran_order, header.descr, start + len))
}

fn is_le_f8(descr: &str) -> bool {
    matches!(descr, "<f8" | "=f8" | "f8" | "float64" | "d" | "<d")
}

fn zip_error(e: &implexity_io::zip::ZipError) -> GuessError {
    if e.0.contains("is encrypted") {
        invalid("matching-time guess payload members cannot be encrypted")
    } else {
        invalid("matching-time guess payload archive is invalid")
    }
}

#[allow(clippy::too_many_lines)]
fn validate_payload(raw: &[u8], states_meta: &[Value], decode: bool) -> GuessResult<Vec<Vec<f64>>> {
    let count = states_meta.len();
    let expected: Vec<String> = (0..count).map(|i| format!("state_{i:06}")).collect();
    let archive = ZipArchive::new(raw).map_err(|e| zip_error(&e))?;
    let entries = archive.entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let wanted: Vec<String> = expected.iter().map(|k| format!("{k}.npy")).collect();
    if names != wanted.iter().map(String::as_str).collect::<Vec<_>>() {
        return Err(invalid("matching-time guess payload members are missing, reordered, or unexpected"));
    }
    if entries.iter().any(|e| e.flags & 1 != 0) {
        return Err(invalid("matching-time guess payload members cannot be encrypted"));
    }
    if entries.iter().any(|e| e.method != DEFLATED) {
        return Err(invalid("matching-time guess payload compression is noncanonical"));
    }

    let expanded: u64 = entries.iter().fold(0_u64, |acc, e| acc.saturating_add(e.size));
    let limit = (count as u64).saturating_mul(MAX_NPY_MEMBER_OVERHEAD).saturating_add(MAX_STATE_BYTES);
    if expanded > limit {
        return Err(invalid("matching-time guess expanded payload exceeds its byte limit"));
    }
    let mut members = Vec::with_capacity(count);
    for (index, (entry, row)) in entries.iter().zip(states_meta).enumerate() {
        let member = archive.read_entry(entry).map_err(|e| zip_error(&e))?;
        let (shape, fortran, descr, position) = npy_header(&member)?;
        let declared = strict_nonnegative_int(row.get("bytes"), &format!("state {index} byte count"))?;
        let declared_shape: Option<Vec<usize>> = row
            .get("shape")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok())).collect());
        if declared_shape.as_deref() != Some(shape.as_slice())
            || shape.len() != 1
            || fortran
            || !is_le_f8(&descr)
            || (shape[0] as u64).checked_mul(8) != Some(declared)
            || (position as u64).checked_add(declared) != Some(entry.size)
        {
            return Err(invalid(format!("matching-time guess state {index} NPY header is invalid")));
        }
        members.push(member);
    }
    if !decode {
        return Ok(Vec::new());
    }
    let mut states = Vec::with_capacity(count);
    for (index, (member, row)) in members.iter().zip(states_meta).enumerate() {
        let key = &expected[index];
        let row = row
            .as_object()
            .filter(|r| same_keys(r, &STATE_KEYS) && r["key"] == key.as_str())
            .ok_or_else(|| invalid("matching-time guess state ordering is invalid"))?;
        let shape_ok = row["shape"]
            .as_array()
            .is_some_and(|s| !s.is_empty() && s.iter().all(|v| strict_int(Some(v)).is_some_and(|n| n > 0)));
        if !shape_ok {
            return Err(invalid(format!("matching-time guess state {index} shape is invalid")));
        }
        let array = NpyArray::from_bytes(member)
            .map_err(|_| invalid("matching-time guess payload cannot be decoded safely"))?;
        let value = array
            .as_f64()
            .filter(|a| a.ndim() == 1 && !a.is_empty() && a.iter().all(|v| v.is_finite()))
            .ok_or_else(|| invalid(format!("matching-time guess state {index} has invalid data")))?;
        let values: Vec<f64> = value.iter().copied().collect();
        let bytes = state_bytes(&values);
        if row["shape"] != json!([values.len()])
            || row["dtype"] != "<f8"
            || strict_nonnegative_int(row.get("bytes"), &format!("state {index} byte count"))?
                != bytes.len() as u64
            || row["sha256"] != sha256_hex(&bytes).as_str()
        {
            return Err(invalid(format!("matching-time guess state {index} identity mismatch")));
        }
        states.push(values);
    }
    Ok(states)
}

fn descriptor(manifest: &Map<String, Value>) -> Map<String, Value> {
    let execution = &manifest["execution_identity"];
    let provider = &manifest["provider_identity"];
    let mut out = Map::new();
    out.insert("schema".into(), json!(REFERENCE_SCHEMA));
    out.insert("capsule_id".into(), manifest["capsule_id"].clone());
    out.insert("payload_sha256".into(), manifest["payload"]["sha256"].clone());
    out.insert("payload_bytes".into(), manifest["payload"]["bytes"].clone());
    out.insert("design_state_id".into(), execution["design_state_id"].clone());
    out.insert("provider_state_problem_id".into(), execution["provider_state_problem_id"].clone());
    out.insert("lifecycle_owner".into(), provider.get("lifecycle_owner").cloned().unwrap_or(Value::Null));
    out.insert("state_layout_id".into(), provider.get("state_layout_id").cloned().unwrap_or(Value::Null));
    out.insert("truth_status".into(), manifest["truth_status"].clone());
    out.insert("role".into(), manifest["role"].clone());
    out.insert("canonical_cache_admission".into(), json!(false));
    out
}


pub fn public_descriptor(value: &Value) -> GuessResult<Map<String, Value>> {
    let value = value
        .as_object()
        .ok_or_else(|| invalid("matching-time guess lifecycle descriptor must be a mapping"))?;
    let keys = [
        "schema",
        "capsule_id",
        "payload_sha256",
        "payload_bytes",
        "design_state_id",
        "provider_state_problem_id",
        "lifecycle_owner",
        "state_layout_id",
        "truth_status",
        "role",
        "canonical_cache_admission",
    ];
    if value.keys().any(|k| !keys.contains(&k.as_str()) && k != "consumed" && k != "installation") {
        return Err(invalid(
            "matching-time guess lifecycle descriptor contains private or unexpected fields",
        ));
    }
    let mut out: Map<String, Value> =
        keys.iter().filter_map(|k| value.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect();
    if out.get("schema").and_then(Value::as_str) != Some(REFERENCE_SCHEMA)
        || !out.get("capsule_id").and_then(Value::as_str).is_some_and(is_capsule_id)
        || out.get("canonical_cache_admission") != Some(&Value::Bool(false))
    {
        return Err(invalid("matching-time guess lifecycle descriptor is invalid or unsafe"));
    }
    if let Some(consumed) = value.get("consumed") {
        if consumed != &Value::Bool(true) {
            return Err(invalid("matching-time guess consumed evidence is invalid"));
        }
        out.insert("consumed".into(), json!(true));
    }
    match value.get("installation") {
        None | Some(Value::Null) => {}
        Some(Value::Object(installation)) => {
            let mut safe: Map<String, Value> =
                ["schema", "lifecycle_owner", "design_state_id", "canonical_cache_admission", "installed_as"]
                    .iter()
                    .filter_map(|k| installation.get(*k).map(|v| ((*k).to_string(), v.clone())))
                    .collect();
            if let Some(Value::Object(ack)) = installation.get("provider_acknowledgement") {
                let filtered: Map<String, Value> =
                    ["design_state_id", "installed_as", "canonical_cache_admission"]
                        .iter()
                        .filter_map(|k| ack.get(*k).map(|v| ((*k).to_string(), v.clone())))
                        .collect();
                safe.insert("provider_acknowledgement".into(), Value::Object(filtered));
            }
            if safe.get("design_state_id") != out.get("design_state_id")
                || safe.get("canonical_cache_admission") != Some(&Value::Bool(false))
            {
                return Err(invalid("matching-time guess installation evidence is stale or unsafe"));
            }
            out.insert("installation".into(), Value::Object(safe));
        }
        Some(_) => return Err(invalid("matching-time guess installation evidence is invalid")),
    }
    Ok(out)
}

#[must_use]
pub fn runtime_source_identity() -> Value {
    let exe = std::env::current_exe().ok();
    let digest = exe.as_deref().and_then(|p| implexity_io::digest::sha256_file(p).ok());
    json!({
        "schema": "implexity-runtime-source-identity/1",
        "scope": "implexity-rs executable",
        "version": env!("CARGO_PKG_VERSION"),
        "file_count": usize::from(digest.is_some()),
        "runtime_aggregate_sha256": digest,
        "declared_source_manifest_aggregate_sha256": Value::Null,
    })
}


pub fn runtime_package_identity() -> CaeResult<Value> {
    let current = implexity_core::packages::global().status()?;
    let get = |k: &str| current.get(k).cloned().unwrap_or(Value::Null);
    let list = |k: &str| match current.get(k) {
        Some(Value::Array(a)) => Value::Array(a.clone()),
        _ => json!([]),
    };
    Ok(json!({
        "schema": "implexity-runtime-package-identity/1",
        "generation": get("generation"),
        "loaded": list("loaded"),
        "load_order_fingerprint": get("load_order_fingerprint"),
        "loaded_manifests": list("loaded_manifests"),
        "registry_fingerprint": get("registry_fingerprint"),
    }))
}

#[must_use]
pub fn runtime_backend_identity() -> Value {
    json!({
        "schema": "implexity-runtime-backend-identity/1",
        "implementation": "rust",
        "crate_version": env!("CARGO_PKG_VERSION"),
        "system": std::env::consts::OS,
        "machine": std::env::consts::ARCH,
        "byteorder": if cfg!(target_endian = "little") { "little" } else { "big" },
        "linear_algebra": "faer",
        "debug_assertions": cfg!(debug_assertions),
    })
}


pub fn execution_identity(
    provider_id: &str,
    problem: &Value,
    design: &NamedArrays,
    source: Option<Value>,
    packages: Option<Value>,
    backend: Option<Value>,
) -> CaeResult<Value> {
    let provider = provider_id.trim();
    if provider.is_empty() {
        return Err(invalid("matching-time guess provider id is empty").into());
    }
    let packages = match packages {
        Some(p) => p,
        None => runtime_package_identity()?,
    };
    Ok(json!({
        "schema": "implexity-matching-time-execution-identity/1",
        "provider_id": provider,
        "provider_state_problem_id": format!("problem-{}", fingerprint_value(problem)),
        "design_state_id": design_identity(design)?,
        "source": source.unwrap_or_else(runtime_source_identity),
        "packages": packages,
        "backend": backend.unwrap_or_else(runtime_backend_identity),
    }))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[derive(Debug, Clone)]
pub struct MatchingTimeGuessStore {
    root: PathBuf,
}

impl MatchingTimeGuessStore {

    pub fn new(root: impl AsRef<Path>) -> GuessResult<Self> {
        let raw = root.as_ref();
        if is_symlink(raw) {
            return Err(invalid("matching-time guess store cannot be a symlink"));
        }
        fs::create_dir_all(raw).map_err(|_| invalid("matching-time guess store cannot be created safely"))?;
        if !raw.is_dir() {
            return Err(invalid("matching-time guess store must be a directory"));
        }
        let root =
            raw.canonicalize().map_err(|_| invalid("matching-time guess store cannot be created safely"))?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn folder(&self, capsule_id: &str) -> GuessResult<PathBuf> {
        if !is_capsule_id(capsule_id) {
            return Err(invalid("invalid matching-time guess capsule id"));
        }
        Ok(self.root.join(capsule_id))
    }


    pub fn create(
        &self,
        guess: &MatchingTimeNewtonGuess,
        execution: &Value,
    ) -> GuessResult<Map<String, Value>> {
        let (arrays, states, total) = normalise_states(guess)?;
        let mut identity = Map::new();
        identity.insert("schema".into(), json!(CAPSULE_SCHEMA));
        identity.insert("truth_status".into(), json!("exact_guarded_source"));
        identity.insert("role".into(), json!(CAPSULE_ROLE));
        identity.insert("canonical_cache_admission".into(), json!(false));
        identity.insert("execution_identity".into(), execution.clone());
        identity.insert("provider_identity".into(), Value::Object(guess.provider_identity().clone()));
        identity.insert("provenance".into(), Value::Object(guess.provenance().clone()));
        identity.insert("state_count".into(), json!(states.len()));
        identity.insert("states".into(), Value::Array(states));
        identity.insert("uncompressed_state_bytes".into(), json!(total));
        let payload = implexity_io::npz::save_deterministic(&arrays).map_err(|e| invalid(e.to_string()))?;
        let payload_sha = sha256_hex(&payload);
        let capsule_id = identity_digest(&identity, &payload_sha);
        let mut manifest = identity;
        manifest.insert("capsule_id".into(), json!(capsule_id));
        manifest.insert(
            "payload".into(),
            json!({"format": PAYLOAD_FORMAT, "filename": "states.npz", "bytes": payload.len(), "sha256": payload_sha}),
        );
        let final_dir = self.folder(&capsule_id)?;
        if is_symlink(&final_dir) {
            return Err(invalid("matching-time capsule destination cannot be a symlink"));
        }
        if final_dir.exists() {
            let (existing, descriptor) = self.load(&capsule_id, execution)?;
            let same = existing.provider_identity() == guess.provider_identity()
                && existing.states().len() == guess.states().len()
                && existing.states().iter().zip(guess.states()).all(|(a, b)| a == b);
            if !same {
                return Err(invalid("existing matching-time capsule has conflicting content"));
            }
            return Ok(descriptor);
        }
        let temporary = self.root.join(format!(".guess-{}", implexity_io::atomic::unique_token()));
        let write = || -> std::io::Result<()> {
            fs::create_dir(&temporary)?;
            fs::write(temporary.join("states.npz"), &payload)?;
            let text = dumps(&Value::Object(manifest.clone()), &DumpOptions::indented(2).sorted(true));
            fs::write(temporary.join("manifest.json"), text.as_bytes())
        };
        if let Err(e) = write() {
            let _ = fs::remove_dir_all(&temporary);
            return Err(invalid(format!("matching-time guess capsule cannot be written: {e}")));
        }
        if fs::rename(&temporary, &final_dir).is_err() {
            let _ = fs::remove_dir_all(&temporary);
            if is_symlink(&final_dir) {
                return Err(invalid("matching-time capsule destination became a symlink"));
            }
            if !final_dir.exists() {
                return Err(invalid("matching-time guess capsule cannot be published"));
            }
        }
        Ok(self.load(&capsule_id, execution)?.1)
    }

    fn regular_file(path: &Path, label: &str, limit: u64) -> GuessResult<()> {
        let meta = fs::symlink_metadata(path)
            .map_err(|_| invalid(format!("matching-time guess {label} is missing")))?;
        if !meta.file_type().is_file() {
            return Err(invalid(format!("matching-time guess {label} must be a regular file")));
        }
        if meta.len() > limit {
            return Err(invalid(format!("matching-time guess {label} exceeds its byte limit")));
        }
        Ok(())
    }

    fn missing(capsule_id: &str) -> GuessError {
        GuessError::Missing(format!("required matching-time guess {} is missing", repr_str(capsule_id)))
    }


    #[allow(clippy::too_many_lines)]
    pub fn load(
        &self,
        capsule_id: &str,
        expected_execution: &Value,
    ) -> GuessResult<(MatchingTimeNewtonGuess, Map<String, Value>)> {
        let folder = self.folder(capsule_id)?;
        let not_regular = || {
            invalid(format!(
                "matching-time guess {} is not a regular capsule directory",
                repr_str(capsule_id)
            ))
        };
        if is_symlink(&folder) {
            return Err(not_regular());
        }
        if !folder.exists() {
            return Err(Self::missing(capsule_id));
        }
        if !folder.is_dir() {
            return Err(not_regular());
        }
        let manifest_path = folder.join("manifest.json");
        let payload_path = folder.join("states.npz");
        Self::regular_file(&manifest_path, "manifest", MAX_MANIFEST_BYTES)?;
        Self::regular_file(&payload_path, "payload", MAX_STATE_BYTES)?;
        let manifest_value = fs::read(&manifest_path)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| parse_strict(&t).ok())
            .ok_or_else(|| invalid("matching-time guess manifest is not valid JSON"))?;
        let manifest = strict_manifest(&manifest_value)?;
        let actual = manifest["execution_identity"].as_object().cloned().unwrap_or_default();
        let wanted = expected_execution.as_object().cloned().unwrap_or_default();
        if actual != wanted {
            let mut keys: Vec<&String> =
                actual.keys().chain(wanted.keys()).filter(|k| actual.get(*k) != wanted.get(*k)).collect();
            keys.sort();
            keys.dedup();
            let joined: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
            return Err(invalid(format!(
                "matching-time guess execution identity mismatch: {}",
                joined.join(", ")
            )));
        }
        let raw = fs::read(&payload_path).map_err(|_| invalid("matching-time guess payload is missing"))?;
        let actual_sha = sha256_hex(&raw);
        let payload = &manifest["payload"];
        if payload["sha256"] != actual_sha.as_str()
            || strict_nonnegative_int(payload.get("bytes"), "payload byte count")? != raw.len() as u64
        {
            return Err(invalid("matching-time guess payload hash/size mismatch"));
        }
        let states_meta = manifest["states"].as_array().cloned().unwrap_or_default();
        let states = validate_payload(&raw, &states_meta, true)?;
        let total: u64 = states.iter().map(|s| s.len() as u64 * 8).sum();
        if total
            != strict_nonnegative_int(
                manifest.get("uncompressed_state_bytes"),
                "uncompressed state byte count",
            )?
            || total > MAX_STATE_BYTES
        {
            return Err(invalid("matching-time guess uncompressed payload size mismatch"));
        }
        let expected_id = identity_digest(manifest, &actual_sha);
        if manifest["capsule_id"] != capsule_id || capsule_id != expected_id {
            return Err(invalid("matching-time guess capsule identity mismatch"));
        }
        let guess = MatchingTimeNewtonGuess::new(
            states,
            manifest["provider_identity"].as_object().cloned().unwrap_or_default(),
            manifest["provenance"].as_object().cloned().unwrap_or_default(),
        )
        .map_err(|e| invalid(e.to_string()))?;
        Ok((guess, descriptor(manifest)))
    }


    pub fn inspect(&self, capsule_id: &str) -> GuessResult<Map<String, Value>> {
        let folder = self.folder(capsule_id)?;
        let manifest_path = folder.join("manifest.json");
        let bad = || invalid("matching-time guess capsule directory/manifest is invalid");
        if is_symlink(&folder) {
            return Err(bad());
        }
        if !folder.exists() {
            return Err(Self::missing(capsule_id));
        }
        if !folder.is_dir() || !manifest_path.is_file() || is_symlink(&manifest_path) {
            return Err(bad());
        }
        if fs::metadata(&manifest_path).map_or(0, |m| m.len()) > MAX_MANIFEST_BYTES {
            return Err(invalid("matching-time guess manifest exceeds its byte limit"));
        }
        let manifest_value = fs::read(&manifest_path)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| parse_strict(&t).ok())
            .ok_or_else(|| invalid("matching-time guess manifest is invalid"))?;
        let manifest = strict_manifest(&manifest_value)?;
        if manifest["capsule_id"] != capsule_id {
            return Err(invalid("matching-time guess manifest identity mismatch"));
        }
        let payload_path = folder.join("states.npz");
        let meta = fs::symlink_metadata(&payload_path)
            .map_err(|_| invalid("matching-time guess payload is missing"))?;
        if !meta.file_type().is_file() || meta.len() > MAX_STATE_BYTES {
            return Err(invalid("matching-time guess payload must be a bounded regular file"));
        }
        let payload_sha = sha256_file(&payload_path)?;
        if manifest["payload"]["sha256"] != payload_sha.as_str()
            || meta.len() != strict_nonnegative_int(manifest["payload"].get("bytes"), "payload byte count")?
        {
            return Err(invalid("matching-time guess payload hash/size mismatch"));
        }
        let raw = fs::read(&payload_path).map_err(|_| invalid("matching-time guess payload is missing"))?;
        validate_payload(&raw, manifest["states"].as_array().map_or(&[][..], Vec::as_slice), false)?;
        if identity_digest(manifest, &payload_sha) != capsule_id {
            return Err(invalid("matching-time guess capsule identity mismatch"));
        }
        Ok(descriptor(manifest))
    }
}

