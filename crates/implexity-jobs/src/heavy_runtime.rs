// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::private::{
    canonical_text, compare_digest, create_exclusive, fsync_dir, hmac_sha256_hex, monotonic_ns,
    open_nofollow, read_limited, token_hex, write_all_sync,
};

pub const MANAGED_OPERATION_ENV: &str = "IMPLEXITY_MANAGED_OPERATION_ID";
pub const MANAGED_PRIVATE_DIRECTORY_ENV: &str = "IMPLEXITY_MANAGED_PRIVATE_DIRECTORY";
pub const MANAGED_PARTIAL_DIRECTORY_ENV: &str = "IMPLEXITY_MANAGED_PARTIAL_DIRECTORY";
pub const MANAGED_CONTROL_TOKEN_ENV: &str = "IMPLEXITY_MANAGED_CONTROL_TOKEN";

pub const CONTROL_REQUEST_SCHEMA: &str = "implexity-private-control-request/1";
pub const CONTROL_ACKNOWLEDGEMENT_SCHEMA: &str = "implexity-private-control-acknowledgement/1";
pub const PHASE_MESSAGE_SCHEMA: &str = "implexity-private-runtime-phase/1";
pub const TERMINAL_RECEIPT_SCHEMA: &str = "implexity-private-managed-terminal-receipt/1";
pub const MAX_PRIVATE_MESSAGE_BYTES: u64 = 8192;
pub const CHILD_RUNTIME_PHASES: [&str; 4] = ["initializing", "running", "finalizing", "stopping"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HeavyRuntimeContractError(pub String);

fn err<T>(message: &str) -> Result<T, HeavyRuntimeContractError> {
    Err(HeavyRuntimeContractError(message.to_string()))
}

pub type HeavyResult<T> = Result<T, HeavyRuntimeContractError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("authenticated cooperative cancellation")]
pub struct CooperativeCancellation;


pub fn require_operation_id(value: &str) -> HeavyResult<&str> {
    if value.len() == 48 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        Ok(value)
    } else {
        err("managed operation identifier is malformed")
    }
}


pub fn encode_control_token(token: &[u8]) -> HeavyResult<String> {
    if token.len() < 32 {
        return err("control token must contain at least 256 bits");
    }
    Ok(base64::engine::general_purpose::URL_SAFE.encode(token))
}


pub fn decode_control_token(encoded: &str) -> HeavyResult<Vec<u8>> {
    if encoded.is_empty() {
        return err("encoded control token is missing");
    }
    let Ok(token) = base64::engine::general_purpose::URL_SAFE.decode(encoded.as_bytes()) else {
        return err("encoded control token is malformed");
    };
    if token.len() < 32 {
        return err("control token must contain at least 256 bits");
    }
    Ok(token)
}

fn canonical_message_bytes(payload: &Map<String, Value>) -> Vec<u8> {
    canonical_text(&Value::Object(payload.clone())).into_bytes()
}


pub fn authenticated_control_message(
    token: &[u8],
    payload: &Map<String, Value>,
) -> HeavyResult<Map<String, Value>> {
    if token.len() < 32 {
        return err("control token must contain at least 256 bits");
    }
    if payload.contains_key("authenticator") {
        return err("private control payload must be an exact dictionary");
    }
    let authenticator = hmac_sha256_hex(token, &canonical_message_bytes(payload));
    let mut body = payload.clone();
    body.insert("authenticator".into(), Value::String(authenticator));
    Ok(body)
}


pub fn verify_control_message(
    token: &[u8],
    message: &Map<String, Value>,
    expected_fields: &[&str],
) -> HeavyResult<Map<String, Value>> {
    if token.len() < 32 {
        return err("control token must contain at least 256 bits");
    }
    let exact = message.len() == expected_fields.len() + 1
        && message.contains_key("authenticator")
        && expected_fields.iter().all(|k| message.contains_key(*k));
    if !exact {
        return err("private control message fields are not canonical");
    }
    let supplied = match message.get("authenticator") {
        Some(Value::String(s)) if crate::private::is_sha256(s) => s.clone(),
        _ => return err("private control authenticator is malformed"),
    };
    let body: Map<String, Value> =
        expected_fields.iter().map(|k| ((*k).to_string(), message[*k].clone())).collect();
    let expected = hmac_sha256_hex(token, &canonical_message_bytes(&body));
    if !compare_digest(supplied.as_bytes(), expected.as_bytes()) {
        return err("private control message authentication failed");
    }
    Ok(body)
}


pub fn atomic_private_json(path: &Path, message: &Map<String, Value>) -> HeavyResult<()> {
    let Some(parent) = path.parent().filter(|p| path.is_absolute() && p.is_dir()) else {
        return err("private message path must have an absolute parent");
    };
    let mut encoded = canonical_message_bytes(message);
    encoded.push(b'\n');
    if encoded.len() as u64 > MAX_PRIVATE_MESSAGE_BYTES {
        return err("private control message is too large");
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let suffix = token_hex(12)
        .map_err(|_| HeavyRuntimeContractError("private control message write failed".into()))?;
    let temporary = parent.join(format!(".{name}.{suffix}"));
    let result = (|| -> std::io::Result<()> {
        let mut file = create_exclusive(&temporary, 0o600)?;
        write_all_sync(&mut file, &encoded)?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        fsync_dir(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return err("private control message write failed");
    }
    Ok(())
}


pub fn read_private_json(path: &Path) -> HeavyResult<Option<Map<String, Value>>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return err("private control message cannot be inspected"),
    };
    if !meta.file_type().is_file() || meta.len() > MAX_PRIVATE_MESSAGE_BYTES {
        return err("private control message is not a small regular file");
    }
    let Ok(data) = open_nofollow(path).and_then(|mut f| read_limited(&mut f, MAX_PRIVATE_MESSAGE_BYTES))
    else {
        return err("private control message cannot be read");
    };
    if data.len() as u64 > MAX_PRIVATE_MESSAGE_BYTES {
        return err("private control message is too large");
    }
    let Ok(text) = std::str::from_utf8(&data) else {
        return err("private control message is invalid JSON");
    };
    let Ok(decoded) = serde_json::from_str::<Value>(text) else {
        return err("private control message is invalid JSON");
    };
    match decoded {
        Value::Object(m) => Ok(Some(m)),
        _ => err("private control message must be an object"),
    }
}

fn require_private_directory(path: &Path) -> HeavyResult<PathBuf> {
    if !path.is_absolute() {
        return err("managed private directory must be absolute");
    }
    let Ok(meta) = implexity_io::fsguard::stat_nofollow(path) else {
        return err("managed private directory is unavailable");
    };
    if !meta.is_dir() || !meta.owned || !meta.owner_only {
        return err("managed private directory ownership or mode is unsafe");
    }
    std::fs::canonicalize(path).or_else(|_| err("managed private directory is unavailable"))
}

fn int_field(body: &Map<String, Value>, key: &str) -> Option<i64> {
    body.get(key).filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64)
}

#[derive(Debug)]
pub struct CooperativeRuntime {
    pub operation_id: String,
    pub private_directory: PathBuf,
    pub partial_directory: PathBuf,
    token: Vec<u8>,
    phase_sequence: i64,
    last_request_sequence: i64,
    closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SafePointError {
    #[error(transparent)]
    Contract(#[from] HeavyRuntimeContractError),
    #[error(transparent)]
    Cancelled(#[from] CooperativeCancellation),
}

impl CooperativeRuntime {

    pub fn new(
        operation_id: &str,
        private_directory: &Path,
        partial_directory: &Path,
        control_token: Vec<u8>,
    ) -> HeavyResult<Self> {
        let operation_id = require_operation_id(operation_id)?.to_string();
        let private_directory = require_private_directory(private_directory)?;
        if !partial_directory.is_absolute() {
            return err("managed partial directory must be absolute");
        }
        let Ok(partial) = std::fs::canonicalize(partial_directory) else {
            return err("managed partial directory is unavailable");
        };
        if partial.parent() != Some(private_directory.as_path()) {
            return err("managed partial directory escaped its operation");
        }
        let Ok(meta) = implexity_io::fsguard::stat_nofollow(&partial) else {
            return err("managed partial directory is unavailable");
        };
        if !meta.is_dir() || !meta.owned || !meta.owner_only {
            return err("managed partial directory ownership or mode is unsafe");
        }
        if control_token.len() < 32 {
            return err("control token must contain at least 256 bits");
        }
        Ok(Self {
            operation_id,
            private_directory,
            partial_directory: partial,
            token: control_token,
            phase_sequence: 0,
            last_request_sequence: 0,
            closed: false,
        })
    }


    pub fn from_environment() -> HeavyResult<Self> {
        let get = |k: &str| std::env::var(k).ok();
        let (Some(operation), Some(private), Some(partial), Some(token)) = (
            get(MANAGED_OPERATION_ENV),
            get(MANAGED_PRIVATE_DIRECTORY_ENV),
            get(MANAGED_PARTIAL_DIRECTORY_ENV),
            get(MANAGED_CONTROL_TOKEN_ENV),
        ) else {
            return err("managed child launch envelope is incomplete");
        };
        Self::new(&operation, Path::new(&private), Path::new(&partial), decode_control_token(&token)?)
    }


    pub fn emit_phase(&mut self, phase: &str) -> HeavyResult<()> {
        if self.closed {
            return err("cooperative runtime is closed");
        }
        implexity_runtime::resource_telemetry::require_generic_phase(phase)
            .map_err(|e| HeavyRuntimeContractError(e.0))?;
        if !CHILD_RUNTIME_PHASES.contains(&phase) {
            return err("child cannot publish a parent-owned lifecycle phase");
        }
        self.phase_sequence += 1;
        let mut payload = Map::new();
        payload.insert("schema".into(), Value::String(PHASE_MESSAGE_SCHEMA.into()));
        payload.insert("operation_id".into(), Value::String(self.operation_id.clone()));
        payload.insert("sequence".into(), Value::from(self.phase_sequence));
        payload.insert("phase".into(), Value::String(phase.into()));
        payload.insert("monotonic_ns".into(), Value::from(monotonic_ns()));
        atomic_private_json(
            &self.private_directory.join("phase.json"),
            &authenticated_control_message(&self.token, &payload)?,
        )
    }


    pub fn checkpoint(&mut self, phase: &str) -> Result<(), SafePointError> {
        self.emit_phase(phase)?;
        let Some(message) = read_private_json(&self.private_directory.join("control.request.json"))? else {
            return Ok(());
        };
        let body = verify_control_message(
            &self.token,
            &message,
            &["schema", "operation_id", "sequence", "action", "issued_monotonic_ns"],
        )?;
        let sequence = int_field(&body, "sequence");
        let issued = int_field(&body, "issued_monotonic_ns");
        let valid = body.get("schema").and_then(Value::as_str) == Some(CONTROL_REQUEST_SCHEMA)
            && body.get("operation_id").and_then(Value::as_str) == Some(self.operation_id.as_str())
            && body.get("action").and_then(Value::as_str) == Some("cancel")
            && sequence.is_some_and(|s| s > 0)
            && issued.is_some_and(|s| s > 0);
        let (true, Some(sequence)) = (valid, sequence) else {
            return Err(HeavyRuntimeContractError("private cancellation request is malformed".into()).into());
        };
        if sequence <= self.last_request_sequence {
            return Ok(());
        }
        self.last_request_sequence = sequence;
        let mut ack = Map::new();
        ack.insert("schema".into(), Value::String(CONTROL_ACKNOWLEDGEMENT_SCHEMA.into()));
        ack.insert("operation_id".into(), Value::String(self.operation_id.clone()));
        ack.insert("sequence".into(), Value::from(sequence));
        ack.insert("action".into(), Value::String("cancel_acknowledged".into()));
        ack.insert("acknowledged_monotonic_ns".into(), Value::from(monotonic_ns()));
        atomic_private_json(
            &self.private_directory.join("control.acknowledgement.json"),
            &authenticated_control_message(&self.token, &ack)?,
        )?;
        self.emit_phase("stopping")?;
        Err(CooperativeCancellation.into())
    }

    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.token.iter_mut().for_each(|b| *b = 0);
            self.token.clear();
        }
    }


    pub fn emit_terminal_receipt(&mut self) -> HeavyResult<()> {
        if self.closed {
            return err("cooperative runtime is closed");
        }
        let mut payload = Map::new();
        payload.insert("schema".into(), Value::String(TERMINAL_RECEIPT_SCHEMA.into()));
        payload.insert("operation_id".into(), Value::String(self.operation_id.clone()));
        payload.insert("outcome".into(), Value::String("completed".into()));
        payload.insert("monotonic_ns".into(), Value::from(monotonic_ns()));
        atomic_private_json(
            &self.private_directory.join("terminal.receipt.json"),
            &authenticated_control_message(&self.token, &payload)?,
        )
    }
}

impl Drop for CooperativeRuntime {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug)]
pub enum CooperativeOutcome<T, E> {
    Completed(T),
    Cancelled,
    Failed(E),
}


pub fn cooperative_runtime<T, E>(
    body: impl FnOnce(&mut CooperativeRuntime) -> Result<T, E>,
) -> Result<CooperativeOutcome<T, E>, HeavyRuntimeContractError> {
    let mut runtime = CooperativeRuntime::from_environment()?;
    runtime.emit_phase("initializing")?;
    match runtime.checkpoint("running") {
        Ok(()) => {}
        Err(SafePointError::Cancelled(_)) => return Ok(CooperativeOutcome::Cancelled),
        Err(SafePointError::Contract(e)) => return Err(e),
    }
    let value = match body(&mut runtime) {
        Ok(v) => v,
        Err(e) => return Ok(CooperativeOutcome::Failed(e)),
    };
    runtime.emit_phase("finalizing")?;
    runtime.emit_terminal_receipt()?;
    runtime.close();
    Ok(CooperativeOutcome::Completed(value))
}

