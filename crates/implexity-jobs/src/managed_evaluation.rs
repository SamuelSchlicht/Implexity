// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use implexity_runtime::resource_telemetry::{
    ParentResourceTelemetry, ProcessBirthIdentity, ProcessObservation, ProcessTableReader,
    ResourceTelemetryError, require_generic_phase,
};
use serde_json::{Map, Value};

use crate::heavy_runtime::{
    CHILD_RUNTIME_PHASES, CONTROL_ACKNOWLEDGEMENT_SCHEMA, CONTROL_REQUEST_SCHEMA, HeavyRuntimeContractError,
    MANAGED_CONTROL_TOKEN_ENV, MANAGED_OPERATION_ENV, MANAGED_PARTIAL_DIRECTORY_ENV,
    MANAGED_PRIVATE_DIRECTORY_ENV, PHASE_MESSAGE_SCHEMA, atomic_private_json, authenticated_control_message,
    encode_control_token, read_private_json, require_operation_id, verify_control_message,
};
use crate::private::{
    compare_digest, create_exclusive, epoch_seconds, fsync_dir, hmac_sha256, hmac_sha256_hex, monotonic,
    monotonic_ns, open_nofollow, token_bytes, token_hex, write_all_sync,
};
use implexity_io::fsguard;

const LEGACY_JOURNAL_SCHEMA: &str = "implexity-private-managed-evaluation-journal/1";
const JOURNAL_SCHEMA: &str = "implexity-private-managed-evaluation-journal/2";
const CONTROL_CAPABILITY_SCHEMA: &str = "implexity-private-managed-control-capability/1";
const TERMINAL_RECEIPT_SCHEMA: &str = "implexity-private-managed-terminal-receipt/1";
const PUBLIC_STATUS_SCHEMA: &str = "implexity-managed-evaluation-status/1";
const RESERVED_CHILD_ENV: [&str; 4] = [
    MANAGED_OPERATION_ENV,
    MANAGED_PRIVATE_DIRECTORY_ENV,
    MANAGED_PARTIAL_DIRECTORY_ENV,
    MANAGED_CONTROL_TOKEN_ENV,
];
const ROOT_EXIT_ABSENCE_CONFIRMATIONS: i64 = 2;
pub const MANAGED_PARTIAL_DIRECTORY_ARGUMENT: &str = "__IMPLEXITY_MANAGED_PARTIAL_DIRECTORY__";
const MAX_OUTPUT_READ_BYTES: usize = 1024 * 1024;
const RESTART_KEY_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedEvaluationError {
    #[error("{0}")]
    Error(String),
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Ownership(String),
    #[error("{0}")]
    InUse(String),
    #[error(transparent)]
    Heavy(#[from] HeavyRuntimeContractError),
    #[error(transparent)]
    Telemetry(#[from] ResourceTelemetryError),
}

impl ManagedEvaluationError {
    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Error(_) => "ManagedEvaluationError",
            Self::Contract(_) => "ManagedEvaluationContractError",
            Self::Ownership(_) => "ManagedEvaluationOwnershipError",
            Self::InUse(_) => "ManagedEvaluationManagerInUse",
            Self::Heavy(_) => "HeavyRuntimeContractError",
            Self::Telemetry(_) => "ResourceTelemetryError",
        }
    }
}

type MResult<T> = Result<T, ManagedEvaluationError>;

fn contract<T>(message: &str) -> MResult<T> {
    Err(ManagedEvaluationError::Contract(message.to_string()))
}

fn ownership<T>(message: &str) -> MResult<T> {
    Err(ManagedEvaluationError::Ownership(message.to_string()))
}

fn in_use<T>(message: &str) -> MResult<T> {
    Err(ManagedEvaluationError::InUse(message.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedEvaluationState {
    Created,
    Running,
    CancelRequested,
    Stopping,
    CancellationBlocked,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Discarded,
    Abandoned,
}

impl ManagedEvaluationState {
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::CancelRequested => "cancel_requested",
            Self::Stopping => "stopping",
            Self::CancellationBlocked => "cancellation_blocked",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Discarded => "discarded",
            Self::Abandoned => "abandoned",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "created" => Self::Created,
            "running" => Self::Running,
            "cancel_requested" => Self::CancelRequested,
            "stopping" => Self::Stopping,
            "cancellation_blocked" => Self::CancellationBlocked,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "discarded" => Self::Discarded,
            "abandoned" => Self::Abandoned,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::Discarded
                | Self::Abandoned
        )
    }
}

#[must_use]
pub fn is_terminal_state(value: &str) -> bool {
    ManagedEvaluationState::parse(value).is_some_and(ManagedEvaluationState::terminal)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ManagedEvaluationPolicy {
    pub timeout_s: Option<f64>,
    pub cooperative_grace_s: f64,
    pub term_grace_s: f64,
    pub kill_grace_s: f64,
    pub poll_interval_s: f64,
    pub telemetry_interval_s: f64,
    pub memory_limit_bytes: Option<i64>,
}

impl ManagedEvaluationPolicy {

    pub fn with_timeout(timeout_s: Option<f64>) -> MResult<Self> {
        Self {
            timeout_s,
            cooperative_grace_s: 0.5,
            term_grace_s: 1.0,
            kill_grace_s: 1.0,
            poll_interval_s: 0.05,
            telemetry_interval_s: 0.25,
            memory_limit_bytes: None,
        }
        .checked()
    }


    pub fn checked(self) -> MResult<Self> {
        let durations = [
            ("timeout_s", self.timeout_s),
            ("cooperative_grace_s", Some(self.cooperative_grace_s)),
            ("term_grace_s", Some(self.term_grace_s)),
            ("kill_grace_s", Some(self.kill_grace_s)),
            ("poll_interval_s", Some(self.poll_interval_s)),
            ("telemetry_interval_s", Some(self.telemetry_interval_s)),
        ];
        for (name, value) in durations {
            if let Some(v) = value
                && (!v.is_finite() || v <= 0.0)
            {
                return contract(&format!("{name} must be finite and positive"));
            }
        }
        if self.memory_limit_bytes.is_some_and(|m| m <= 0) {
            return contract("memory_limit_bytes must be a positive integer or null");
        }
        if self.timeout_s.is_none() && self.memory_limit_bytes.is_none() {
            return contract("unbounded wall time requires an explicit positive memory_limit_bytes");
        }
        Ok(self)
    }

    fn to_wire(self) -> Value {
        let mut m = Map::new();
        m.insert("timeout_s".into(), self.timeout_s.map_or(Value::Null, Value::from));
        m.insert("cooperative_grace_s".into(), Value::from(self.cooperative_grace_s));
        m.insert("term_grace_s".into(), Value::from(self.term_grace_s));
        m.insert("kill_grace_s".into(), Value::from(self.kill_grace_s));
        m.insert("poll_interval_s".into(), Value::from(self.poll_interval_s));
        m.insert("telemetry_interval_s".into(), Value::from(self.telemetry_interval_s));
        m.insert("memory_limit_bytes".into(), self.memory_limit_bytes.map_or(Value::Null, Value::from));
        Value::Object(m)
    }

    fn from_wire(value: Option<&Value>) -> MResult<Self> {
        let keys = [
            "timeout_s",
            "cooperative_grace_s",
            "term_grace_s",
            "kill_grace_s",
            "poll_interval_s",
            "telemetry_interval_s",
            "memory_limit_bytes",
        ];
        let Some(m) = value
            .and_then(Value::as_object)
            .filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
        else {
            return contract("managed recovery policy is malformed");
        };
        let malformed = || ManagedEvaluationError::Contract("managed recovery policy is malformed".into());
        let number =
            |k: &str| -> MResult<f64> { m[k].as_f64().filter(|_| m[k].is_number()).ok_or_else(malformed) };
        let timeout = if m["timeout_s"].is_null() { None } else { Some(number("timeout_s")?) };
        let memory = match &m["memory_limit_bytes"] {
            Value::Null => None,
            v if v.is_i64() => v.as_i64(),
            _ => return Err(malformed()),
        };
        Self {
            timeout_s: timeout,
            cooperative_grace_s: number("cooperative_grace_s")?,
            term_grace_s: number("term_grace_s")?,
            kill_grace_s: number("kill_grace_s")?,
            poll_interval_s: number("poll_interval_s")?,
            telemetry_interval_s: number("telemetry_interval_s")?,
            memory_limit_bytes: memory,
        }
        .checked()
        .map_err(|_| malformed())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PublicManagedEvaluationStatus {
    pub operation_id: String,
    pub state: String,
    pub phase: String,
    pub terminal: bool,
    pub cancelable: bool,
    pub terminal_reason: Option<String>,
    pub elapsed_s: f64,
}

impl PublicManagedEvaluationStatus {
    fn checked(self) -> MResult<Self> {
        require_operation_id(&self.operation_id)?;
        if ManagedEvaluationState::parse(&self.state).is_none() {
            return contract("public state is invalid");
        }
        require_generic_phase(&self.phase)?;
        if self.terminal != is_terminal_state(&self.state) {
            return contract("public terminal flag disagrees with state");
        }
        if self.cancelable != (!self.terminal && self.state != "cancellation_blocked") {
            return contract("public cancelable flag disagrees with state");
        }
        if let Some(r) = &self.terminal_reason
            && (r.is_empty() || !r.bytes().all(|c| c.is_ascii_lowercase() || c == b'_'))
        {
            return contract("terminal reason is not a closed label");
        }
        if !self.elapsed_s.is_finite() || self.elapsed_s < 0.0 {
            return contract("public elapsed time is invalid");
        }
        Ok(self)
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(PUBLIC_STATUS_SCHEMA.into()));
        m.insert("operation_id".into(), Value::String(self.operation_id.clone()));
        m.insert("state".into(), Value::String(self.state.clone()));
        m.insert("phase".into(), Value::String(self.phase.clone()));
        m.insert("terminal".into(), Value::Bool(self.terminal));
        m.insert("cancelable".into(), Value::Bool(self.cancelable));
        m.insert("terminal_reason".into(), self.terminal_reason.clone().map_or(Value::Null, Value::String));
        m.insert("elapsed_s".into(), Value::from(self.elapsed_s));
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManagedEvaluationObservation {
    pub timeout_s: Option<f64>,
    pub memory_limit_bytes: Option<i64>,
    pub elapsed_s: f64,
    pub remaining_s: Option<f64>,
    pub peak_owned_rss_bytes: i64,
    pub last_owned_rss_bytes: i64,
    pub owned_process_count: i64,
    pub sample_count: i64,
    pub peak_owned_memory_bytes: i64,
    pub last_owned_memory_bytes: i64,
    pub memory_metric: String,
}

impl ManagedEvaluationObservation {
    fn checked(self) -> MResult<Self> {
        if !["rss", "sum_process_max_rss_physical_footprint"].contains(&self.memory_metric.as_str()) {
            return contract("managed observation memory metric is invalid");
        }
        if self.timeout_s.is_none() != self.remaining_s.is_none() {
            return contract("managed observation deadline/remaining nullability differs");
        }
        for v in [self.timeout_s, Some(self.elapsed_s), self.remaining_s].into_iter().flatten() {
            if !v.is_finite() || v < 0.0 {
                return contract("managed observation duration is invalid");
            }
        }
        if self.memory_limit_bytes.is_some_and(|m| m <= 0) {
            return contract("managed observation memory limit is invalid");
        }
        for v in [
            self.peak_owned_rss_bytes,
            self.last_owned_rss_bytes,
            self.peak_owned_memory_bytes,
            self.last_owned_memory_bytes,
            self.owned_process_count,
            self.sample_count,
        ] {
            if v < 0 {
                return contract("managed observation counter is invalid");
            }
        }
        Ok(self)
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut policy = Map::new();
        policy.insert("wall_time_s".into(), self.timeout_s.map_or(Value::Null, Value::from));
        policy.insert("memory_bytes".into(), self.memory_limit_bytes.map_or(Value::Null, Value::from));
        let mut observed = Map::new();
        observed.insert("elapsed_s".into(), Value::from(self.elapsed_s));
        observed.insert("remaining_s".into(), self.remaining_s.map_or(Value::Null, Value::from));
        observed.insert("peak_owned_rss_bytes".into(), Value::from(self.peak_owned_rss_bytes));
        observed.insert("last_owned_rss_bytes".into(), Value::from(self.last_owned_rss_bytes));
        observed.insert("peak_owned_memory_bytes".into(), Value::from(self.peak_owned_memory_bytes));
        observed.insert("last_owned_memory_bytes".into(), Value::from(self.last_owned_memory_bytes));
        observed.insert("memory_metric".into(), Value::String(self.memory_metric.clone()));
        observed.insert("owned_process_count".into(), Value::from(self.owned_process_count));
        observed.insert("sample_count".into(), Value::from(self.sample_count));
        let mut m = Map::new();
        m.insert("schema".into(), Value::String("implexity-managed-evaluation-observation/1".into()));
        m.insert("policy".into(), Value::Object(policy));
        m.insert("observed".into(), Value::Object(observed));
        Value::Object(m)
    }
}

fn owner_component_ok(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverableManagedEvaluation {
    pub operation_id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub state: String,
    pub phase: String,
    pub root_live: bool,
}

impl RecoverableManagedEvaluation {
    fn checked(self) -> MResult<Self> {
        require_operation_id(&self.operation_id)?;
        if !owner_component_ok(&self.owner_kind) || !owner_component_ok(&self.owner_id) {
            return contract("managed recovery owner is malformed");
        }
        if ManagedEvaluationState::parse(&self.state).is_none() {
            return contract("managed recovery state is invalid");
        }
        require_generic_phase(&self.phase)?;
        Ok(self)
    }
}

static BRANDS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct ManagedEvaluationControl {
    operation_id: String,
    token: Vec<u8>,
    brand: u64,
}

impl std::fmt::Debug for ManagedEvaluationControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ManagedEvaluationControl(operation_id={:?}, token=<redacted>)", self.operation_id)
    }
}

impl ManagedEvaluationControl {
    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
}

pub type TerminalValidator = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;
pub type PartialPreparer<'a> = Box<dyn FnOnce(&Path) -> Result<(), String> + 'a>;
pub type SpawnAuthorizer<'a> = Box<dyn FnOnce(u32, &str) -> Result<(), String> + 'a>;

pub trait Signals: Send + Sync {

    fn group(&self, process_group_id: i64, kill: bool) -> Result<(), SignalError>;

    fn process(&self, pid: i64, kill: bool) -> Result<(), SignalError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalError {
    NoProcess,
    Blocked,
}


#[derive(Debug, Default, Clone, Copy)]
pub struct OsSignals;

#[cfg(unix)]
fn map_signal_result(result: rustix::io::Result<()>) -> Result<(), SignalError> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if e == rustix::io::Errno::SRCH => Err(SignalError::NoProcess),
        Err(_) => Err(SignalError::Blocked),
    }
}

#[cfg(unix)]
fn to_pid(value: i64) -> Result<rustix::process::Pid, SignalError> {
    i32::try_from(value).ok().and_then(rustix::process::Pid::from_raw).ok_or(SignalError::NoProcess)
}

#[cfg(unix)]
impl Signals for OsSignals {
    fn group(&self, process_group_id: i64, kill: bool) -> Result<(), SignalError> {
        let signal = if kill { rustix::process::Signal::KILL } else { rustix::process::Signal::TERM };
        map_signal_result(rustix::process::kill_process_group(to_pid(process_group_id)?, signal))
    }

    fn process(&self, pid: i64, kill: bool) -> Result<(), SignalError> {
        let signal = if kill { rustix::process::Signal::KILL } else { rustix::process::Signal::TERM };
        map_signal_result(rustix::process::kill_process(to_pid(pid)?, signal))
    }
}

#[cfg(windows)]
impl Signals for OsSignals {
    fn group(&self, _process_group_id: i64, _kill: bool) -> Result<(), SignalError> {
        Err(SignalError::Blocked)
    }

    fn process(&self, _pid: i64, _kill: bool) -> Result<(), SignalError> {
        Err(SignalError::Blocked)
    }
}

fn new_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(unix)]
fn unreaped_child_exited(child: &mut Child) -> MResult<bool> {
    let Some(pid) = i32::try_from(child.id()).ok().and_then(rustix::process::Pid::from_raw) else {
        return ownership("unreaped child status inspection is unavailable");
    };
    let options = rustix::process::WaitIdOptions::EXITED
        | rustix::process::WaitIdOptions::NOHANG
        | rustix::process::WaitIdOptions::NOWAIT;
    loop {
        match rustix::process::waitid(rustix::process::WaitId::Pid(pid), options) {
            Ok(None) => return Ok(false),
            Ok(Some(_status)) => return Ok(true),
            Err(e) if e == rustix::io::Errno::INTR => {}
            Err(e) if e == rustix::io::Errno::CHILD => {

                return match child.try_wait() {
                    Ok(Some(_)) => Ok(true),
                    _ => ownership("managed root was reaped outside its supervisor"),
                };
            }
            Err(_) => return ownership("unreaped child status inspection failed"),
        }
    }
}

#[cfg(windows)]
fn unreaped_child_exited(child: &mut Child) -> MResult<bool> {
    match child.try_wait() {
        Ok(status) => Ok(status.is_some()),
        Err(_) => ownership("unreaped child status inspection failed"),
    }
}

fn return_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.code().unwrap_or_else(|| -status.signal().unwrap_or(1))
    }
    #[cfg(windows)]
    {
        status.code().unwrap_or(1)
    }
}

fn wait_with_timeout(child: &mut Child, timeout_s: f64) -> Option<i32> {
    let deadline = monotonic() + timeout_s;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(return_code(status)),
            Ok(None) => {}
            Err(_) => return None,
        }
        if monotonic() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug)]
struct RecordMut {
    token: Vec<u8>,
    child: Option<Child>,
    root_identity: Option<ProcessBirthIdentity>,
    state: ManagedEvaluationState,
    phase: String,
    terminal_reason: Option<String>,
    tracked_descendants: BTreeMap<(i64, String), ProcessBirthIdentity>,
    last_phase_sequence: i64,
    cancel_sequence: i64,
    cancellation_requested: bool,
    timeout_requested: bool,
    memory_exceeded: bool,
    stop_attempted: bool,
    signal_blocked: bool,
    acknowledgement_valid: bool,
    supervisor_error: bool,
    terminal_elapsed_s: Option<f64>,
    root_exit_absence_confirmations: i64,
    peak_owned_rss_bytes: i64,
    last_owned_rss_bytes: i64,
    peak_owned_memory_bytes: i64,
    last_owned_memory_bytes: i64,
    memory_metric: String,
    last_owned_process_count: i64,
    resource_sample_count: i64,
    terminal_observation_directory: Option<PathBuf>,
    pending_returncode: Option<i32>,
}

struct Record {
    operation_id: String,
    token_digest: String,
    private_directory: PathBuf,
    partial_directory: PathBuf,
    committed_directory: PathBuf,
    quarantine_directory: PathBuf,
    policy: ManagedEvaluationPolicy,
    validate_and_prepare_terminal: TerminalValidator,
    started_epoch: f64,
    started_monotonic: f64,
    recovery_owner: Option<(String, String)>,
    reattached: bool,
    m: Mutex<RecordMut>,
}

impl Record {
    fn lock(&self) -> MutexGuard<'_, RecordMut> {
        self.m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug, Clone)]
struct PendingRecovery {
    descriptor: RecoverableManagedEvaluation,
    root_identity: ProcessBirthIdentity,
}

#[derive(Default)]
struct ManagerState {
    records: BTreeMap<String, Arc<Record>>,
    pending_recovery: BTreeMap<String, PendingRecovery>,
    closed: bool,
    starting: usize,
    threads: Vec<std::thread::JoinHandle<()>>,
}

struct Shared {
    operations: PathBuf,
    token_digest_key: Vec<u8>,
    reader: ProcessTableReader,
    signals: Arc<dyn Signals>,
    state: Mutex<ManagerState>,
    condition: Condvar,
}

pub struct ManagedEvaluationManager {
    root: PathBuf,
    shared: Arc<Shared>,
    brand: u64,
    lock_file: Mutex<Option<File>>,
}

impl std::fmt::Debug for ManagedEvaluationManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedEvaluationManager").field("root", &self.root).finish_non_exhaustive()
    }
}

pub struct StartOptions<'a> {
    pub policy: ManagedEvaluationPolicy,
    pub validate_and_prepare_terminal: TerminalValidator,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub replace_env: bool,
    pub prepare_partial: Option<PartialPreparer<'a>>,
    pub stdin: Option<Stdio>,
    pub authorize_spawned_child: Option<SpawnAuthorizer<'a>>,
    pub recovery_owner: Option<(String, String)>,
}

impl StartOptions<'_> {
    #[must_use]
    pub fn new(policy: ManagedEvaluationPolicy, validate_and_prepare_terminal: TerminalValidator) -> Self {
        Self {
            policy,
            validate_and_prepare_terminal,
            cwd: None,
            env: BTreeMap::new(),
            replace_env: false,
            prepare_partial: None,
            stdin: None,
            authorize_spawned_child: None,
            recovery_owner: None,
        }
    }
}

fn identity_to_wire(identity: &ProcessBirthIdentity) -> Value {
    let mut m = Map::new();
    m.insert("pid".into(), Value::from(identity.pid()));
    m.insert("parent_pid".into(), Value::from(identity.parent_pid()));
    m.insert("process_group_id".into(), Value::from(identity.process_group_id()));
    m.insert("birth_marker".into(), Value::String(identity.birth_marker().to_string()));
    Value::Object(m)
}

fn identity_from_wire(value: Option<&Value>) -> MResult<ProcessBirthIdentity> {
    let keys = ["pid", "parent_pid", "process_group_id", "birth_marker"];
    let malformed = || ManagedEvaluationError::Contract("managed recovery identity is malformed".into());
    let m = value
        .and_then(Value::as_object)
        .filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
        .ok_or_else(malformed)?;
    let int = |k: &str| m[k].as_i64().filter(|_| m[k].is_i64()).ok_or_else(malformed);
    let marker = m["birth_marker"].as_str().ok_or_else(malformed)?;
    ProcessBirthIdentity::new(int("pid")?, int("parent_pid")?, int("process_group_id")?, marker)
        .map_err(|_| malformed())
}

fn owner_from_wire(value: Option<&Value>) -> MResult<Option<(String, String)>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => {
            if m.len() != 2 || !m.contains_key("kind") || !m.contains_key("id") {
                return contract("recovery owner must contain exactly kind and id");
            }
            match (m["kind"].as_str(), m["id"].as_str()) {
                (Some(k), Some(i)) if owner_component_ok(k) && owner_component_ok(i) => {
                    Ok(Some((k.to_string(), i.to_string())))
                }
                _ => contract("managed recovery owner is malformed"),
            }
        }
        Some(_) => contract("recovery owner must contain exactly kind and id"),
    }
}

fn owner_to_wire(owner: Option<&(String, String)>) -> Value {
    owner.map_or(Value::Null, |(k, i)| {
        let mut m = Map::new();
        m.insert("kind".into(), Value::String(k.clone()));
        m.insert("id".into(), Value::String(i.clone()));
        Value::Object(m)
    })
}

fn quarantine_path(partial: &Path, quarantine: &Path, reason: &str) -> MResult<Option<PathBuf>> {
    if std::fs::symlink_metadata(partial).is_err() {
        return Ok(None);
    }
    let suffix = token_hex(12).map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
    let target = quarantine.join(format!("{reason}-{suffix}"));
    std::fs::rename(partial, &target).map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
    let mut marker = Map::new();
    marker.insert("schema".into(), Value::String("implexity-private-quarantine-record/1".into()));
    marker.insert("reason".into(), Value::String(reason.into()));
    marker.insert("recorded_epoch".into(), Value::from(epoch_seconds()));
    let is_dir = std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_dir());
    let marker_path = if is_dir {
        target.join("quarantine.json")
    } else {
        quarantine
            .join(format!("{}.json", target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()))
    };
    atomic_private_json(&marker_path, &marker)?;
    Ok(Some(target))
}

impl Shared {
    fn lock_state(&self) -> MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn control_digest(&self, operation_id: &str, token: &[u8]) -> MResult<String> {
        require_operation_id(operation_id)?;
        if token.len() < 32 {
            return contract("control token is malformed");
        }
        let mut message = operation_id.as_bytes().to_vec();
        message.push(0);
        message.extend_from_slice(token);
        Ok(hmac_sha256_hex(&self.token_digest_key, &message))
    }

    fn capability_authenticator(&self, operation_id: &str, nonce: &[u8], ciphertext: &[u8]) -> String {
        let mut message = b"managed-control-capability\0".to_vec();
        message.extend_from_slice(operation_id.as_bytes());
        message.push(0);
        message.extend_from_slice(nonce);
        message.push(0);
        message.extend_from_slice(ciphertext);
        hmac_sha256_hex(&self.token_digest_key, &message)
    }

    fn capability_stream(&self, operation_id: &str, nonce: &[u8]) -> [u8; 32] {
        let mut message = b"managed-control-stream\0".to_vec();
        message.extend_from_slice(operation_id.as_bytes());
        message.push(0);
        message.extend_from_slice(nonce);
        hmac_sha256(&self.token_digest_key, &message)
    }

    fn write_control_capability(&self, private: &Path, operation_id: &str, token: &[u8]) -> MResult<()> {
        let nonce = token_bytes(32).map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
        let stream = self.capability_stream(operation_id, &nonce);
        if token.len() != stream.len() {
            return contract("restartable control tokens must contain exactly 256 bits");
        }
        let ciphertext: Vec<u8> = token.iter().zip(stream.iter()).map(|(a, b)| a ^ b).collect();
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(CONTROL_CAPABILITY_SCHEMA.into()));
        m.insert("operation_id".into(), Value::String(operation_id.into()));
        m.insert("nonce".into(), Value::String(hex::encode(&nonce)));
        m.insert("ciphertext".into(), Value::String(hex::encode(&ciphertext)));
        m.insert(
            "authenticator".into(),
            Value::String(self.capability_authenticator(operation_id, &nonce, &ciphertext)),
        );
        atomic_private_json(&private.join("control.capability.json"), &m)?;
        Ok(())
    }

    fn read_control_capability(
        &self,
        private: &Path,
        operation_id: &str,
        expected_digest: &str,
    ) -> MResult<Vec<u8>> {
        let message = read_private_json(&private.join("control.capability.json"))?;
        let keys = ["schema", "operation_id", "nonce", "ciphertext", "authenticator"];
        let Some(message) = message.filter(|m| {
            m.len() == keys.len()
                && keys.iter().all(|k| m.contains_key(*k))
                && m.get("schema").and_then(Value::as_str) == Some(CONTROL_CAPABILITY_SCHEMA)
                && m.get("operation_id").and_then(Value::as_str) == Some(operation_id)
        }) else {
            return contract("managed recovery capability is malformed");
        };
        let decode = |k: &str| message[k].as_str().and_then(|s| hex::decode(s).ok());
        let (Some(nonce), Some(ciphertext)) = (decode("nonce"), decode("ciphertext")) else {
            return contract("managed recovery capability is malformed");
        };
        let expected = self.capability_authenticator(operation_id, &nonce, &ciphertext);
        match message["authenticator"].as_str() {
            Some(s) if compare_digest(s.as_bytes(), expected.as_bytes()) => {}
            _ => return contract("managed recovery capability authentication failed"),
        }
        let stream = self.capability_stream(operation_id, &nonce);
        if nonce.len() != 32 || ciphertext.len() != stream.len() {
            return contract("managed recovery capability is malformed");
        }
        let token: Vec<u8> = ciphertext.iter().zip(stream.iter()).map(|(a, b)| a ^ b).collect();
        if !compare_digest(self.control_digest(operation_id, &token)?.as_bytes(), expected_digest.as_bytes())
        {
            return contract("managed recovery capability identity drifted");
        }
        Ok(token)
    }

    fn notify(&self) {
        let _guard = self.lock_state();
        self.condition.notify_all();
    }

    fn write_journal(record: &Record, m: &RecordMut) -> MResult<()> {
        let tracked: Vec<Value> = m.tracked_descendants.values().map(identity_to_wire).collect();
        let mut payload = Map::new();
        payload.insert("schema".into(), Value::String(JOURNAL_SCHEMA.into()));
        payload.insert("operation_id".into(), Value::String(record.operation_id.clone()));
        payload.insert("state".into(), Value::String(m.state.value().into()));
        payload.insert("phase".into(), Value::String(m.phase.clone()));
        payload.insert("started_epoch".into(), Value::from(record.started_epoch));
        payload.insert("updated_epoch".into(), Value::from(epoch_seconds()));
        payload
            .insert("terminal_reason".into(), m.terminal_reason.clone().map_or(Value::Null, Value::String));
        payload.insert("control_token_digest".into(), Value::String(record.token_digest.clone()));
        payload
            .insert("root_identity".into(), m.root_identity.as_ref().map_or(Value::Null, identity_to_wire));
        payload.insert("tracked_descendants".into(), Value::Array(tracked));
        payload.insert("terminal_elapsed_s".into(), m.terminal_elapsed_s.map_or(Value::Null, Value::from));
        payload.insert("policy".into(), record.policy.to_wire());
        payload.insert("recovery_owner".into(), owner_to_wire(record.recovery_owner.as_ref()));
        payload.insert("cancel_sequence".into(), Value::from(m.cancel_sequence));
        payload.insert("cancellation_requested".into(), Value::Bool(m.cancellation_requested));
        payload.insert("timeout_requested".into(), Value::Bool(m.timeout_requested));
        payload.insert("memory_exceeded".into(), Value::Bool(m.memory_exceeded));
        payload.insert("last_phase_sequence".into(), Value::from(m.last_phase_sequence));
        atomic_private_json(&record.private_directory.join("journal.json"), &payload)?;
        Ok(())
    }

    fn public_status(record: &Record, m: &RecordMut) -> MResult<PublicManagedEvaluationStatus> {
        let terminal = m.state.terminal();
        let elapsed = if terminal {
            m.terminal_elapsed_s.unwrap_or(0.0)
        } else {
            monotonic() - record.started_monotonic
        };
        PublicManagedEvaluationStatus {
            operation_id: record.operation_id.clone(),
            state: m.state.value().into(),
            phase: m.phase.clone(),
            terminal,
            cancelable: !terminal && m.state != ManagedEvaluationState::CancellationBlocked,
            terminal_reason: if terminal { m.terminal_reason.clone() } else { None },
            elapsed_s: elapsed.max(0.0),
        }
        .checked()
    }

    fn public_status_from_journal(&self, operation_id: &str) -> MResult<PublicManagedEvaluationStatus> {
        let path = self.operations.join(operation_id).join("journal.json");
        let message = read_private_json(&path)?;
        let Some(message) = message.filter(|m| {
            matches!(m.get("schema").and_then(Value::as_str), Some(s) if s == LEGACY_JOURNAL_SCHEMA || s == JOURNAL_SCHEMA)
        }) else {
            return contract("managed operation does not exist");
        };
        let state = message
            .get("state")
            .and_then(Value::as_str)
            .filter(|s| ManagedEvaluationState::parse(s).is_some());
        let phase = message.get("phase").and_then(Value::as_str);
        let started = message.get("started_epoch").filter(|v| v.is_number()).and_then(Value::as_f64);
        let (Some(state), Some(phase), Some(started)) = (state, phase, started) else {
            return contract("managed operation journal is malformed");
        };
        let terminal = is_terminal_state(state);
        let elapsed = if terminal {
            match message.get("terminal_elapsed_s").filter(|v| v.is_number()).and_then(Value::as_f64) {
                Some(e) if e.is_finite() && e >= 0.0 => e,
                _ => {
                    let Some(updated) =
                        message.get("updated_epoch").filter(|v| v.is_number()).and_then(Value::as_f64)
                    else {
                        return contract("managed operation terminal elapsed time is invalid");
                    };
                    (updated - started).max(0.0)
                }
            }
        } else {
            (epoch_seconds() - started).max(0.0)
        };
        PublicManagedEvaluationStatus {
            operation_id: operation_id.to_string(),
            state: state.to_string(),
            phase: phase.to_string(),
            terminal,
            cancelable: !terminal && state != "cancellation_blocked",
            terminal_reason: if terminal {
                message.get("terminal_reason").and_then(Value::as_str).map(str::to_string)
            } else {
                None
            },
            elapsed_s: elapsed,
        }
        .checked()
    }

    fn root_exited(&self, record: &Record, m: &mut RecordMut) -> MResult<bool> {
        if !record.reattached {
            let Some(child) = m.child.as_mut() else {
                return ownership("managed child process handle is unavailable");
            };
            return unreaped_child_exited(child);
        }
        let Some(root) = m.root_identity.clone() else {
            return ownership("reattached root identity is unavailable");
        };
        let observed = self.reader.observe(root.pid())?;
        Ok(observed.is_none_or(|o| o.identity.stable_key() != root.stable_key()))
    }

    fn refresh_owned(&self, record: &Record, m: &mut RecordMut) -> MResult<Vec<ProcessObservation>> {
        let Some(root) = m.root_identity.clone() else {
            return Ok(Vec::new());
        };
        let group = self.reader.process_group_snapshot(root.process_group_id())?;
        let mut tracked: Vec<ProcessBirthIdentity> = m.tracked_descendants.values().cloned().collect();
        tracked.extend(group.iter().map(|r| r.identity.clone()));
        let tree = self.reader.owned_snapshot(&root, &tracked)?;
        let mut by_key: BTreeMap<(i64, String), ProcessObservation> = BTreeMap::new();
        let mut order: Vec<(i64, String)> = Vec::new();
        for row in group.into_iter().chain(tree) {
            let key = row.identity.stable_key();
            if !by_key.contains_key(&key) {
                order.push(key.clone());
            }
            by_key.insert(key, row);
        }
        let owned: Vec<ProcessObservation> = order.iter().filter_map(|k| by_key.get(k).cloned()).collect();
        let mut changed = false;
        for observed in &owned {
            let key = observed.identity.stable_key();
            if key != root.stable_key() {
                if !m.tracked_descendants.contains_key(&key) {
                    changed = true;
                }
                m.tracked_descendants.insert(key, observed.identity.clone());
            }
        }
        if changed {
            Self::write_journal(record, m)?;
        }
        Ok(owned)
    }

    fn owned_descendants_alive(m: &RecordMut, observations: &[ProcessObservation]) -> bool {
        let root_key = m.root_identity.as_ref().map(ProcessBirthIdentity::stable_key);
        observations.iter().any(|r| Some(r.identity.stable_key()) != root_key)
    }

    fn terminal_receipt_returncode(record: &Record, m: &RecordMut) -> MResult<i32> {
        let Some(message) = read_private_json(&record.private_directory.join("terminal.receipt.json"))?
        else {
            return Ok(1);
        };
        let body = verify_control_message(
            &m.token,
            &message,
            &["schema", "operation_id", "outcome", "monotonic_ns"],
        )?;
        let ok = body.get("schema").and_then(Value::as_str) == Some(TERMINAL_RECEIPT_SCHEMA)
            && body.get("operation_id").and_then(Value::as_str) == Some(record.operation_id.as_str())
            && body.get("outcome").and_then(Value::as_str) == Some("completed")
            && body.get("monotonic_ns").filter(|v| v.is_i64()).and_then(Value::as_i64).is_some_and(|v| v > 0);
        if !ok {
            return contract("managed terminal receipt is malformed");
        }
        Ok(0)
    }

    fn reap_root_if_group_quiescent(
        &self,
        record: &Record,
        m: &mut RecordMut,
        exited_before_census: bool,
        observations: &[ProcessObservation],
    ) -> MResult<(Option<i32>, bool)> {
        let exited_after_census = self.root_exited(record, m)?;
        let descendants_alive = Self::owned_descendants_alive(m, observations);
        if !exited_before_census || !exited_after_census || descendants_alive {
            m.root_exit_absence_confirmations = 0;
            return Ok((None, exited_after_census));
        }
        m.root_exit_absence_confirmations += 1;
        if m.root_exit_absence_confirmations < ROOT_EXIT_ABSENCE_CONFIRMATIONS {
            return Ok((None, true));
        }
        if record.reattached {
            return Ok((Some(Self::terminal_receipt_returncode(record, m)?), true));
        }
        let Some(child) = m.child.as_mut() else {
            return ownership("managed child process handle is unavailable");
        };
        match wait_with_timeout(child, (2.0 * record.policy.poll_interval_s).max(0.05)) {
            Some(code) => Ok((Some(code), true)),
            None => ownership("quiescent managed root could not be reaped"),
        }
    }

    fn ingest_phase(&self, record: &Record, m: &mut RecordMut) -> MResult<()> {
        let Some(message) = read_private_json(&record.private_directory.join("phase.json"))? else {
            return Ok(());
        };
        let body = verify_control_message(
            &m.token,
            &message,
            &["schema", "operation_id", "sequence", "phase", "monotonic_ns"],
        )?;
        let sequence = body.get("sequence").filter(|v| v.is_i64()).and_then(Value::as_i64);
        let stamp = body.get("monotonic_ns").filter(|v| v.is_i64()).and_then(Value::as_i64);
        let ok = body.get("schema").and_then(Value::as_str) == Some(PHASE_MESSAGE_SCHEMA)
            && body.get("operation_id").and_then(Value::as_str) == Some(record.operation_id.as_str())
            && sequence.is_some_and(|s| s > 0)
            && stamp.is_some_and(|s| s > 0);
        let (true, Some(sequence)) = (ok, sequence) else {
            return contract("private phase message is malformed");
        };
        if sequence <= m.last_phase_sequence {
            return Ok(());
        }
        let phase = body.get("phase").and_then(Value::as_str).unwrap_or("");
        require_generic_phase(phase)?;
        if !CHILD_RUNTIME_PHASES.contains(&phase) {
            return contract("child published a parent-owned lifecycle phase");
        }
        m.last_phase_sequence = sequence;
        if m.phase != "stopping" {
            m.phase = phase.to_string();
        }
        Self::write_journal(record, m)?;
        Ok(())
    }

    fn write_control_request(record: &Record, m: &RecordMut) -> MResult<()> {
        let mut payload = Map::new();
        payload.insert("schema".into(), Value::String(CONTROL_REQUEST_SCHEMA.into()));
        payload.insert("operation_id".into(), Value::String(record.operation_id.clone()));
        payload.insert("sequence".into(), Value::from(m.cancel_sequence));
        payload.insert("action".into(), Value::String("cancel".into()));
        payload.insert("issued_monotonic_ns".into(), Value::from(monotonic_ns()));
        atomic_private_json(
            &record.private_directory.join("control.request.json"),
            &authenticated_control_message(&m.token, &payload)?,
        )?;
        Ok(())
    }

    fn ingest_acknowledgement(record: &Record, m: &mut RecordMut) -> MResult<()> {
        let Some(message) =
            read_private_json(&record.private_directory.join("control.acknowledgement.json"))?
        else {
            return Ok(());
        };
        let body = verify_control_message(
            &m.token,
            &message,
            &["schema", "operation_id", "sequence", "action", "acknowledged_monotonic_ns"],
        )?;
        let ok = body.get("schema").and_then(Value::as_str) == Some(CONTROL_ACKNOWLEDGEMENT_SCHEMA)
            && body.get("operation_id").and_then(Value::as_str) == Some(record.operation_id.as_str())
            && body.get("sequence").and_then(Value::as_i64) == Some(m.cancel_sequence)
            && body.get("sequence").is_some_and(Value::is_i64)
            && body.get("action").and_then(Value::as_str) == Some("cancel_acknowledged")
            && body
                .get("acknowledged_monotonic_ns")
                .filter(|v| v.is_i64())
                .and_then(Value::as_i64)
                .is_some_and(|v| v > 0);
        if !ok {
            return contract("private cancellation acknowledgement is malformed");
        }
        m.acknowledgement_valid = true;
        Ok(())
    }

    fn bounded_stop(&self, record: &Record) -> MResult<()> {
        {
            let mut m = record.lock();
            m.state = ManagedEvaluationState::Stopping;
            m.phase = "stopping".into();
            Self::write_journal(record, &m)?;
        }
        self.notify();
        if self.wait_until_owned_exit(record, record.policy.cooperative_grace_s)? {
            let mut m = record.lock();
            Self::ingest_acknowledgement(record, &mut m)?;
            return Ok(());
        }
        if !self.signal_owned(record, false)? {
            self.mark_cancellation_blocked(record)?;
            return Ok(());
        }
        if self.wait_until_owned_exit(record, record.policy.term_grace_s)? {
            return Ok(());
        }
        if !self.signal_owned(record, true)? {
            self.mark_cancellation_blocked(record)?;
            return Ok(());
        }
        if !self.wait_until_owned_exit(record, record.policy.kill_grace_s)? {
            self.mark_cancellation_blocked(record)?;
        }
        Ok(())
    }

    fn wait_until_owned_exit(&self, record: &Record, duration_s: f64) -> MResult<bool> {
        let deadline = monotonic() + duration_s;
        loop {
            let reaped = {
                let mut m = record.lock();
                let exited_before = self.root_exited(record, &mut m)?;
                let observations = self.refresh_owned(record, &mut m)?;
                let (code, _) =
                    self.reap_root_if_group_quiescent(record, &mut m, exited_before, &observations)?;
                if let Some(code) = code {
                    m.pending_returncode = Some(code);
                    true
                } else {
                    false
                }
            };
            if reaped {
                return Ok(true);
            }
            let now = monotonic();
            if now >= deadline {
                return Ok(false);
            }
            std::thread::sleep(crate::worker_cli::duration_from_secs(
                record.policy.poll_interval_s.min((deadline - now).max(0.001)),
            ));
        }
    }

    fn signal_owned(&self, record: &Record, kill: bool) -> MResult<bool> {
        let mut m = record.lock();
        let Some(root) = m.root_identity.clone() else {
            return self.root_exited(record, &mut m);
        };
        let census = (|| -> MResult<(bool, Vec<ProcessObservation>, bool)> {
            let before = self.root_exited(record, &mut m)?;
            let snapshot = self.refresh_owned(record, &mut m)?;
            let after = self.root_exited(record, &mut m)?;
            Ok((before, snapshot, after))
        })();
        let (exited_before, snapshot, exited_after) = match census {
            Ok(v) => v,
            Err(ManagedEvaluationError::Telemetry(_)) => return Ok(false),
            Err(e) => return Err(e),
        };
        let by_key: BTreeMap<(i64, String), ProcessBirthIdentity> =
            snapshot.iter().map(|r| (r.identity.stable_key(), r.identity.clone())).collect();
        let mut blocked = false;
        let group_members = snapshot.iter().any(|r| r.identity.process_group_id() == root.process_group_id());
        if (group_members || !exited_before || !exited_after)
            && let Err(SignalError::Blocked) = self.signals.group(root.process_group_id(), kill)
        {
            blocked = true;
        }
        for key in m.tracked_descendants.keys() {
            let Some(current) = by_key.get(key) else { continue };
            if current.process_group_id() == root.process_group_id() {
                continue;
            }
            if let Err(SignalError::Blocked) = self.signals.process(current.pid(), kill) {
                blocked = true;
            }
        }
        Ok(!blocked)
    }

    fn mark_cancellation_blocked(&self, record: &Record) -> MResult<()> {
        {
            let mut m = record.lock();
            m.signal_blocked = true;
            m.state = ManagedEvaluationState::CancellationBlocked;
            m.phase = "stopping".into();
            m.terminal_reason = None;
            Self::write_journal(record, &m)?;
        }
        self.notify();
        Ok(())
    }

    fn finalize(
        &self,
        record: &Record,
        returncode: Option<i32>,
        telemetry: Option<&ParentResourceTelemetry>,
    ) -> MResult<()> {
        let (state, reason) = {
            let m = record.lock();
            if m.state.terminal() {
                return Ok(());
            }
            if m.signal_blocked {
                (ManagedEvaluationState::Discarded, "discarded_after_blocked_cancel")
            } else if m.memory_exceeded {
                (ManagedEvaluationState::Failed, "memory_budget_exceeded")
            } else if m.timeout_requested {
                (ManagedEvaluationState::TimedOut, "deadline_exceeded")
            } else if m.cancellation_requested {
                (ManagedEvaluationState::Cancelled, "cancelled_by_owner")
            } else if m.supervisor_error {
                (ManagedEvaluationState::Failed, "supervisor_failed")
            } else if returncode != Some(0) {
                (ManagedEvaluationState::Failed, "child_failed")
            } else {
                (ManagedEvaluationState::Succeeded, "")
            }
        };
        let (state, reason) = if state == ManagedEvaluationState::Succeeded {
            let promote = || -> Result<(), String> {
                let is_real_dir =
                    |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
                if !is_real_dir(&record.partial_directory) {
                    return Err("terminal partial directory is unavailable".into());
                }
                (record.validate_and_prepare_terminal)(&record.partial_directory)?;
                if !is_real_dir(&record.partial_directory) {
                    return Err("terminal validator invalidated the partial directory".into());
                }
                std::fs::rename(&record.partial_directory, &record.committed_directory)
                    .map_err(|e| e.to_string())?;
                fsync_dir(&record.private_directory).map_err(|e| e.to_string())
            };
            match promote() {
                Ok(()) => (ManagedEvaluationState::Succeeded, "validated_and_promoted"),
                Err(error) => {
                    eprintln!("Managed terminal validation failed: {error}");
                    (ManagedEvaluationState::Failed, "terminal_validation_failed")
                },
            }
        } else {
            (state, reason)
        };
        let observation = if state == ManagedEvaluationState::Succeeded {
            None
        } else {
            quarantine_path(&record.partial_directory, &record.quarantine_directory, reason)?
        };
        let _ = std::fs::remove_file(record.private_directory.join("control.capability.json"));
        {
            let mut m = record.lock();
            if observation.is_some() {
                m.terminal_observation_directory = observation;
            }
            m.terminal_elapsed_s = Some((monotonic() - record.started_monotonic).max(0.0));
            m.state = state;
            m.phase = "terminal".into();
            m.terminal_reason = Some(reason.to_string());
            Self::write_journal(record, &m)?;
        }
        self.notify();
        if let Some(t) = telemetry {
            let _ = t.sample("terminal", &[]);
        }
        let mut m = record.lock();
        m.token.iter_mut().for_each(|b| *b = 0);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn supervise(self: &Arc<Self>, record: &Arc<Record>) {
        let mut telemetry: Option<ParentResourceTelemetry> = None;
        let mut last_telemetry = f64::NEG_INFINITY;
        let outcome = (|| -> MResult<()> {
            telemetry =
                Some(ParentResourceTelemetry::new(&record.private_directory.join("resource.jsonl"), 256)?);
            loop {
                let (returncode, root_exited, observations, phase) = {
                    let mut m = record.lock();
                    if let Some(code) = m.pending_returncode.take() {
                        drop(m);
                        self.finalize(record, Some(code), telemetry.as_ref())?;
                        return Ok(());
                    }
                    let exited_before = self.root_exited(record, &mut m)?;
                    let observations = self.refresh_owned(record, &mut m)?;
                    let (code, exited) =
                        self.reap_root_if_group_quiescent(record, &mut m, exited_before, &observations)?;
                    self.ingest_phase(record, &mut m)?;
                    (code, exited, observations, m.phase.clone())
                };
                self.notify();
                let now = monotonic();
                let mut memory_exceeded = false;
                if now - last_telemetry >= record.policy.telemetry_interval_s {
                    if let Some(t) = telemetry.as_ref() {
                        let sample = t.sample(&phase, &observations)?;
                        let mut m = record.lock();
                        m.last_owned_rss_bytes = sample.owned_rss_bytes;
                        m.peak_owned_rss_bytes = m.peak_owned_rss_bytes.max(m.last_owned_rss_bytes);
                        m.last_owned_process_count = sample.owned_process_count;
                        m.last_owned_memory_bytes = sample.owned_memory_bytes;
                        m.peak_owned_memory_bytes = m.peak_owned_memory_bytes.max(m.last_owned_memory_bytes);
                        m.memory_metric.clone_from(&sample.memory_metric);
                        m.resource_sample_count += 1;
                        memory_exceeded = record
                            .policy
                            .memory_limit_bytes
                            .is_some_and(|limit| sample.owned_memory_bytes > limit);
                    }
                    last_telemetry = now;
                }
                if let Some(code) = returncode {
                    self.finalize(record, Some(code), telemetry.as_ref())?;
                    return Ok(());
                }
                let should_stop = {
                    let mut m = record.lock();
                    let descendants_alive = Self::owned_descendants_alive(&m, &observations);
                    let timed_out =
                        record.policy.timeout_s.is_some_and(|t| now - record.started_monotonic >= t);
                    if timed_out && !m.timeout_requested {
                        m.timeout_requested = true;
                        m.cancel_sequence += 1;
                        Self::write_control_request(record, &m)?;
                        m.state = ManagedEvaluationState::Stopping;
                        m.phase = "stopping".into();
                        Self::write_journal(record, &m)?;
                    }
                    if memory_exceeded && !m.memory_exceeded {
                        m.memory_exceeded = true;
                        m.cancel_sequence += 1;
                        Self::write_control_request(record, &m)?;
                        m.state = ManagedEvaluationState::Stopping;
                        m.phase = "stopping".into();
                        Self::write_journal(record, &m)?;
                    }
                    let mut should_stop =
                        (m.cancellation_requested || m.timeout_requested || m.memory_exceeded)
                            && !m.stop_attempted;
                    if root_exited && descendants_alive && !m.stop_attempted {
                        m.supervisor_error = true;
                        m.terminal_reason = Some("descendant_cleanup_failed".into());
                        should_stop = true;
                    }
                    if should_stop {
                        m.stop_attempted = true;
                    }
                    should_stop
                };
                if should_stop {
                    self.bounded_stop(record)?;
                }
                std::thread::sleep(crate::worker_cli::duration_from_secs(record.policy.poll_interval_s));
            }
        })();
        if outcome.is_ok() {
            return;
        }
        let stop_needed = {
            let mut m = record.lock();
            m.supervisor_error = true;
            if !m.state.terminal() {
                m.state = ManagedEvaluationState::Stopping;
                m.phase = "stopping".into();
                m.terminal_reason = Some("supervisor_failed".into());
                let _ = Self::write_journal(record, &m);
            }
            let needed = !m.stop_attempted;
            m.stop_attempted = true;
            needed
        };
        if stop_needed && self.bounded_stop(record).is_err() {
            let _ = self.mark_cancellation_blocked(record);
        }
        loop {
            let attempt = (|| -> MResult<Option<i32>> {
                let mut m = record.lock();
                if let Some(code) = m.pending_returncode.take() {
                    return Ok(Some(code));
                }
                let exited_before = self.root_exited(record, &mut m)?;
                let observations = self.refresh_owned(record, &mut m)?;
                Ok(self.reap_root_if_group_quiescent(record, &mut m, exited_before, &observations)?.0)
            })();
            match attempt {
                Ok(Some(code)) => {
                    let _ = self.finalize(record, Some(code), telemetry.as_ref());
                    return;
                }
                Ok(None) => {}
                Err(_) => {
                    let _ = self.mark_cancellation_blocked(record);
                }
            }
            std::thread::sleep(crate::worker_cli::duration_from_secs(record.policy.poll_interval_s));
        }
    }

    fn capture_root_identity(
        &self,
        child: &mut Child,
        policy: &ManagedEvaluationPolicy,
    ) -> MResult<ProcessBirthIdentity> {
        let deadline = monotonic() + policy.cooperative_grace_s.clamp(0.25, 1.0);
        let pid = i64::from(child.id());
        loop {
            if let Some(observed) = self.reader.observe(pid)? {
                let identity = observed.identity;
                if identity.process_group_id() != pid {
                    self.terminate_unbound_spawn(child, policy)?;
                    return ownership("new child did not own its expected process group");
                }
                return Ok(identity);
            }
            if unreaped_child_exited(child)? {
                self.terminate_unbound_spawn(child, policy)?;
                return ownership("exited child birth identity could not be captured");
            }
            if monotonic() >= deadline {
                self.terminate_unbound_spawn(child, policy)?;
                return ownership("child birth identity could not be captured");
            }
            std::thread::sleep(crate::worker_cli::duration_from_secs(policy.poll_interval_s.min(0.01)));
        }
    }

    fn terminate_unbound_spawn(&self, child: &mut Child, policy: &ManagedEvaluationPolicy) -> MResult<()> {
        let group_id = i64::from(child.id());
        let mut kill = false;
        let mut stage_deadline = monotonic() + policy.term_grace_s;
        let mut next_signal = 0.0_f64;
        let mut confirmations = 0;
        loop {
            let exited_before = unreaped_child_exited(child)?;
            let rows = self.reader.process_group_snapshot(group_id)?;
            let exited_after = unreaped_child_exited(child)?;
            let descendants = rows.iter().any(|r| r.identity.pid() != group_id);
            if exited_before && exited_after && !descendants {
                confirmations += 1;
                if confirmations >= ROOT_EXIT_ABSENCE_CONFIRMATIONS {
                    return match wait_with_timeout(child, (2.0 * policy.poll_interval_s).max(0.05)) {
                        Some(_) => Ok(()),
                        None => ownership("quiescent unbound root could not be reaped"),
                    };
                }
            } else {
                confirmations = 0;
            }
            let now = monotonic();
            if now >= next_signal {
                let signalled = self.signals.group(group_id, kill).is_ok();
                if !signalled && !exited_after {
                    let _ = self.signals.process(group_id, kill);
                }
                next_signal = now + policy.poll_interval_s.max(0.01);
            }
            if !kill && now >= stage_deadline {
                kill = true;
                stage_deadline = now + policy.kill_grace_s;
                next_signal = 0.0;
            } else if kill && now >= stage_deadline {
                stage_deadline = now + policy.kill_grace_s;
            }
            std::thread::sleep(crate::worker_cli::duration_from_secs(policy.poll_interval_s));
        }
    }
}

impl RecordMut {
    fn new(token: Vec<u8>, state: ManagedEvaluationState, phase: &str) -> Self {
        Self {
            token,
            child: None,
            root_identity: None,
            state,
            phase: phase.to_string(),
            terminal_reason: None,
            tracked_descendants: BTreeMap::new(),
            last_phase_sequence: 0,
            cancel_sequence: 0,
            cancellation_requested: false,
            timeout_requested: false,
            memory_exceeded: false,
            stop_attempted: false,
            signal_blocked: false,
            acknowledgement_valid: false,
            supervisor_error: false,
            terminal_elapsed_s: None,
            root_exit_absence_confirmations: 0,
            peak_owned_rss_bytes: 0,
            last_owned_rss_bytes: 0,
            peak_owned_memory_bytes: 0,
            last_owned_memory_bytes: 0,
            memory_metric: "rss".into(),
            last_owned_process_count: 0,
            resource_sample_count: 0,
            terminal_observation_directory: None,
            pending_returncode: None,
        }
    }
}

impl ManagedEvaluationManager {

    pub fn new(private_root: &Path, token_digest_key: Option<Vec<u8>>) -> MResult<Self> {
        Self::with_signals(private_root, token_digest_key, Arc::new(OsSignals))
    }


    pub fn with_signals(
        private_root: &Path,
        token_digest_key: Option<Vec<u8>>,
        signals: Arc<dyn Signals>,
    ) -> MResult<Self> {
        if !private_root.is_absolute() {
            return contract("managed root must be absolute");
        }
        let io = |e: std::io::Error| ManagedEvaluationError::Error(e.to_string());
        fsguard::create_dir_all_owner_only(private_root).map_err(io)?;
        let meta = fsguard::stat_nofollow(private_root).map_err(io)?;
        if !meta.is_dir() || !meta.owned || !meta.owner_only {
            return contract("managed root ownership or mode is unsafe");
        }
        let root = std::fs::canonicalize(private_root).map_err(io)?;
        let operations = root.join("operations");
        match fsguard::create_dir_owner_only(&operations) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(e)),
        }
        let ometa = fsguard::stat_nofollow(&operations).map_err(io)?;
        if !ometa.is_dir() || !ometa.owned {
            return contract("managed operations directory is unsafe");
        }
        fsguard::set_owner_only(&operations, true).map_err(io)?;
        let key = match token_digest_key {
            Some(k) => k,
            None => load_or_create_restart_key(&root)?,
        };
        if key.len() < 32 {
            return contract("token digest key must contain at least 256 bits");
        }
        let lock_path = root.join(".manager.lock");
        let lock_file = fsguard::open_or_create_nofollow(&lock_path, 0o600).map_err(io)?;

        match lock_file.try_lock() {
            Ok(()) => {}
            Err(_) => return in_use("managed root already has a live owner"),
        }
        let shared = Arc::new(Shared {
            operations,
            token_digest_key: key,
            reader: ProcessTableReader::new()?,
            signals,
            state: Mutex::new(ManagerState::default()),
            condition: Condvar::new(),
        });
        let manager = Self {
            root,
            shared,
            brand: BRANDS.fetch_add(1, Ordering::Relaxed),
            lock_file: Mutex::new(Some(lock_file)),
        };
        if let Err(e) = manager.recover_abandoned_without_signals() {
            manager.release_lock();
            return Err(e);
        }
        Ok(manager)
    }

    fn release_lock(&self) {
        let mut guard = self.lock_file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(file) = guard.take() {
            let _ = file.unlock();
        }
        self.shared.lock_state().closed = true;
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }


    pub fn start(&self, command: &[String], options: StartOptions<'_>) -> MResult<ManagedEvaluationControl> {
        {
            let mut state = self.shared.lock_state();
            if state.closed {
                return contract("managed evaluation manager is closed");
            }
            if !state.pending_recovery.is_empty() {
                return in_use("a prior recoverable managed operation must be reattached first");
            }
            state.starting += 1;
        }
        let result = self.start_reserved(command, options);
        let mut state = self.shared.lock_state();
        state.starting -= 1;
        self.shared.condition.notify_all();
        result
    }

    #[allow(clippy::too_many_lines)]
    fn start_reserved(
        &self,
        command: &[String],
        options: StartOptions<'_>,
    ) -> MResult<ManagedEvaluationControl> {
        let policy = options.policy.checked()?;
        let owner = match &options.recovery_owner {
            None => None,
            Some((k, i)) => {
                if !owner_component_ok(k) || !owner_component_ok(i) {
                    return contract("managed recovery owner is malformed");
                }
                Some((k.clone(), i.clone()))
            }
        };
        if command.is_empty() {
            return contract("child command must be a non-empty sequence");
        }
        if command.iter().any(|v| v.is_empty() || v.contains('\0')) {
            return contract("child command contains an invalid argument");
        }
        if let Some(cwd) = &options.cwd
            && (!cwd.is_absolute() || !cwd.is_dir())
        {
            return contract("child cwd must be an existing absolute directory");
        }
        if RESERVED_CHILD_ENV.iter().any(|k| options.env.contains_key(*k)) {
            return contract("child environment collides with managed controls");
        }
        let io = |e: std::io::Error| ManagedEvaluationError::Error(e.to_string());
        let (operation_id, private) = self.new_private_operation_directory()?;
        let partial = private.join("partial");
        let quarantine = private.join("quarantine");
        fsguard::create_dir_owner_only(&partial).map_err(io)?;
        fsguard::create_dir_owner_only(&quarantine).map_err(io)?;
        let committed = private.join("committed");
        let token = token_bytes(32).map_err(io)?;
        let digest = self.shared.control_digest(&operation_id, &token)?;
        if owner.is_some() {
            self.shared.write_control_capability(&private, &operation_id, &token)?;
        }
        let started_epoch = epoch_seconds();
        let started_monotonic = monotonic();
        let initial = {
            let mut m = Map::new();
            m.insert("schema".into(), Value::String(JOURNAL_SCHEMA.into()));
            m.insert("operation_id".into(), Value::String(operation_id.clone()));
            m.insert("state".into(), Value::String("created".into()));
            m.insert("phase".into(), Value::String("created".into()));
            m.insert("started_epoch".into(), Value::from(started_epoch));
            m.insert("updated_epoch".into(), Value::from(started_epoch));
            m.insert("terminal_reason".into(), Value::Null);
            m.insert("control_token_digest".into(), Value::String(digest.clone()));
            m.insert("root_identity".into(), Value::Null);
            m.insert("tracked_descendants".into(), Value::Array(Vec::new()));
            m.insert("terminal_elapsed_s".into(), Value::Null);
            m.insert("policy".into(), policy.to_wire());
            m.insert("recovery_owner".into(), owner_to_wire(owner.as_ref()));
            m.insert("cancel_sequence".into(), Value::from(0));
            m.insert("cancellation_requested".into(), Value::Bool(false));
            m.insert("timeout_requested".into(), Value::Bool(false));
            m.insert("memory_exceeded".into(), Value::Bool(false));
            m.insert("last_phase_sequence".into(), Value::from(0));
            m
        };
        let journal = private.join("journal.json");
        atomic_private_json(&journal, &initial)?;
        let fail_journal = |reason: &str, elapsed: bool| -> MResult<()> {
            let mut failed = initial.clone();
            failed.insert("state".into(), Value::String("failed".into()));
            failed.insert("phase".into(), Value::String("terminal".into()));
            failed.insert("updated_epoch".into(), Value::from(epoch_seconds()));
            failed.insert("terminal_reason".into(), Value::String(reason.into()));
            if elapsed {
                failed.insert(
                    "terminal_elapsed_s".into(),
                    Value::from((monotonic() - started_monotonic).max(0.0)),
                );
            }
            atomic_private_json(&journal, &failed)?;
            Ok(())
        };
        if let Some(prepare) = options.prepare_partial
            && let Err(message) = prepare(&partial)
        {
            quarantine_path(&partial, &quarantine, "preparation_failed")?;
            fail_journal("preparation_failed", true)?;
            return Err(ManagedEvaluationError::Error(message));
        }
        let marker_prefix = format!("{MANAGED_PARTIAL_DIRECTORY_ARGUMENT}/");
        let mut resolved: Vec<String> = Vec::with_capacity(command.len());
        for value in command {
            if value == MANAGED_PARTIAL_DIRECTORY_ARGUMENT {
                resolved.push(partial.to_string_lossy().into_owned());
            } else if let Some(relative) = value.strip_prefix(&marker_prefix) {
                let parts: Vec<&str> = relative.split('/').collect();
                if relative.is_empty()
                    || relative.starts_with('/')
                    || parts.iter().any(|p| matches!(*p, "" | "." | ".."))
                {
                    return contract("managed partial command path is invalid");
                }
                resolved.push(
                    parts.iter().fold(partial.clone(), |acc, p| acc.join(p)).to_string_lossy().into_owned(),
                );
            } else {
                resolved.push(value.clone());
            }
        }
        let stdout = create_exclusive(&private.join("stdout.log"), 0o600).map_err(io)?;
        let stderr = create_exclusive(&private.join("stderr.log"), 0o600).map_err(io)?;
        let mut cmd = Command::new(&resolved[0]);
        cmd.args(&resolved[1..]);
        if let Some(cwd) = &options.cwd {
            cmd.current_dir(cwd);
        }
        if options.replace_env {
            cmd.env_clear();
        }
        cmd.envs(&options.env);
        cmd.env(MANAGED_OPERATION_ENV, &operation_id);
        cmd.env(MANAGED_PRIVATE_DIRECTORY_ENV, &private);
        cmd.env(MANAGED_PARTIAL_DIRECTORY_ENV, &partial);
        cmd.env(MANAGED_CONTROL_TOKEN_ENV, encode_control_token(&token)?);
        cmd.stdin(options.stdin.unwrap_or_else(Stdio::null));
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));
        new_process_group(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                quarantine_path(&partial, &quarantine, "spawn_failed")?;
                fail_journal("spawn_failed", false)?;
                return Err(io(e));
            }
        };
        if let Some(authorize) = options.authorize_spawned_child
            && let Err(message) = authorize(child.id(), &operation_id)
        {
            if self.shared.signals.group(i64::from(child.id()), true).is_err() {
                let _ = child.kill();
            }
            let _ = wait_with_timeout(&mut child, 5.0);
            quarantine_path(&partial, &quarantine, "child_authorization_failed")?;
            fail_journal("child_authorization_failed", true)?;
            return Err(ManagedEvaluationError::Error(message));
        }
        let root_identity = match self.shared.capture_root_identity(&mut child, &policy) {
            Ok(identity) => identity,
            Err(e) => {
                quarantine_path(&partial, &quarantine, "ownership_failed")?;
                fail_journal("ownership_failed", false)?;
                return Err(e);
            }
        };
        let mut m = RecordMut::new(token.clone(), ManagedEvaluationState::Running, "initializing");
        m.child = Some(child);
        m.root_identity = Some(root_identity);
        let record = Arc::new(Record {
            operation_id: operation_id.clone(),
            token_digest: digest,
            private_directory: private,
            partial_directory: partial,
            committed_directory: committed,
            quarantine_directory: quarantine,
            policy,
            validate_and_prepare_terminal: options.validate_and_prepare_terminal,
            started_epoch,
            started_monotonic,
            recovery_owner: owner,
            reattached: false,
            m: Mutex::new(m),
        });
        self.spawn_supervisor(&record, &format!("managed-evaluation-{}", &operation_id[..12]))?;
        Ok(ManagedEvaluationControl { operation_id, token, brand: self.brand })
    }

    fn spawn_supervisor(&self, record: &Arc<Record>, name: &str) -> MResult<()> {
        let mut state = self.shared.lock_state();
        state.records.insert(record.operation_id.clone(), Arc::clone(record));
        {
            let m = record.lock();
            Shared::write_journal(record, &m)?;
        }
        let shared = Arc::clone(&self.shared);
        let thread_record = Arc::clone(record);
        let handle = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || shared.supervise(&thread_record))
            .map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
        state.threads.push(handle);
        Ok(())
    }

    fn new_private_operation_directory(&self) -> MResult<(String, PathBuf)> {
        for _ in 0..128 {
            let operation_id = token_hex(24).map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
            let private = self.shared.operations.join(&operation_id);
            match fsguard::create_dir_owner_only(&private) {
                Ok(()) => {
                    let resolved = std::fs::canonicalize(&private)
                        .map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
                    return Ok((operation_id, resolved));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(ManagedEvaluationError::Error(e.to_string())),
            }
        }
        Err(ManagedEvaluationError::Error("could not allocate a unique managed operation".into()))
    }

    fn authenticate(&self, control: &ManagedEvaluationControl) -> MResult<Arc<Record>> {
        if control.brand != self.brand {
            return contract("control authority is not owned by this manager");
        }
        let record = self.shared.lock_state().records.get(&control.operation_id).cloned();
        let Some(record) = record else {
            return contract("managed operation is not live in this manager");
        };
        let supplied = self.shared.control_digest(&record.operation_id, &control.token)?;
        if !compare_digest(supplied.as_bytes(), record.token_digest.as_bytes()) {
            return contract("control authority authentication failed");
        }
        Ok(record)
    }


    pub fn status(&self, operation_id: &str) -> MResult<PublicManagedEvaluationStatus> {
        require_operation_id(operation_id)?;
        let record = self.shared.lock_state().records.get(operation_id).cloned();
        if let Some(record) = record {
            let m = record.lock();
            return Shared::public_status(&record, &m);
        }
        self.shared.public_status_from_journal(operation_id)
    }


    pub fn request_cancel(
        &self,
        control: &ManagedEvaluationControl,
    ) -> MResult<PublicManagedEvaluationStatus> {
        let record = self.authenticate(control)?;
        let status = {
            let mut m = record.lock();
            if !m.state.terminal() && !m.cancellation_requested {
                m.cancellation_requested = true;
                m.cancel_sequence += 1;
                Shared::write_control_request(&record, &m)?;
                m.state = ManagedEvaluationState::CancelRequested;
                m.phase = "stopping".into();
                Shared::write_journal(&record, &m)?;
            }
            Shared::public_status(&record, &m)?
        };
        self.shared.notify();
        Ok(status)
    }


    pub fn partial_directory(&self, control: &ManagedEvaluationControl) -> MResult<PathBuf> {
        let record = self.authenticate(control)?;
        if record.lock().state.terminal() {
            return contract("managed operation has no active partial directory");
        }
        let ok = std::fs::symlink_metadata(&record.partial_directory).is_ok_and(|m| m.file_type().is_dir());
        if !ok {
            return Err(ManagedEvaluationError::Error(
                "managed operation partial directory is unavailable".into(),
            ));
        }
        Ok(record.partial_directory.clone())
    }


    pub fn observation(&self, control: &ManagedEvaluationControl) -> MResult<ManagedEvaluationObservation> {
        let record = self.authenticate(control)?;
        let m = record.lock();
        let status = Shared::public_status(&record, &m)?;
        ManagedEvaluationObservation {
            timeout_s: record.policy.timeout_s,
            memory_limit_bytes: record.policy.memory_limit_bytes,
            elapsed_s: status.elapsed_s,
            remaining_s: record.policy.timeout_s.map(|t| (t - status.elapsed_s).max(0.0)),
            peak_owned_rss_bytes: m.peak_owned_rss_bytes,
            last_owned_rss_bytes: m.last_owned_rss_bytes,
            owned_process_count: m.last_owned_process_count,
            sample_count: m.resource_sample_count,
            peak_owned_memory_bytes: m.peak_owned_memory_bytes,
            last_owned_memory_bytes: m.last_owned_memory_bytes,
            memory_metric: m.memory_metric.clone(),
        }
        .checked()
    }


    pub fn read_output(
        &self,
        control: &ManagedEvaluationControl,
        stream: &str,
        offset: u64,
        maximum_bytes: usize,
    ) -> MResult<(Vec<u8>, u64, bool)> {
        use std::io::{Read, Seek, SeekFrom};
        if stream != "stdout" && stream != "stderr" {
            return contract("managed output stream is invalid");
        }
        if maximum_bytes == 0 || maximum_bytes > MAX_OUTPUT_READ_BYTES {
            return contract("managed output read size is invalid");
        }
        let record = self.authenticate(control)?;
        let path = record.private_directory.join(format!("{stream}.log"));
        let unavailable = || ManagedEvaluationError::Error("managed output stream is unavailable".into());
        let before = fsguard::stat_nofollow(&path).map_err(|_| unavailable())?;
        if !before.is_owned_single_regular() {
            return Err(ManagedEvaluationError::Error("managed output stream is unsafe".into()));
        }
        let failed = || ManagedEvaluationError::Error("managed output stream read failed".into());
        let mut file = open_nofollow(&path).map_err(|_| failed())?;
        let opened = fsguard::stat_file(&file).map_err(|_| failed())?;
        if !opened.same_object(&before) || !opened.is_file() || !opened.owned {
            return Err(ManagedEvaluationError::Error("managed output stream identity drifted".into()));
        }
        let size = opened.size;
        if offset > size {
            return Err(ManagedEvaluationError::Error("managed output stream was truncated".into()));
        }
        file.seek(SeekFrom::Start(offset)).map_err(|_| failed())?;
        let mut payload = Vec::with_capacity(maximum_bytes.min(65536));
        file.take(maximum_bytes as u64).read_to_end(&mut payload).map_err(|_| failed())?;
        let next = offset + payload.len() as u64;
        Ok((payload, next, next >= size))
    }


    pub fn solver_recovery(&self, control: &ManagedEvaluationControl) -> MResult<Option<Value>> {
        use std::io::{Read, Seek, SeekFrom};
        let record = self.authenticate(control)?;
        if record.lock().state != ManagedEvaluationState::Failed {
            return Ok(None);
        }
        let path = record.private_directory.join("stdout.log");
        let unavailable = || ManagedEvaluationError::Error("managed solver notice is unavailable".into());
        let before = fsguard::stat_nofollow(&path).map_err(|_| unavailable())?;
        if !before.is_owned_single_regular() {
            return Err(unavailable());
        }
        let mut file = open_nofollow(&path).map_err(|_| unavailable())?;
        let opened = fsguard::stat_file(&file).map_err(|_| unavailable())?;
        if !opened.same_object(&before) || !opened.is_file() || !opened.owned {
            return Err(unavailable());
        }
        file.seek(SeekFrom::Start(opened.size.saturating_sub(65536))).map_err(|_| unavailable())?;
        let mut bytes = Vec::new();
        file.take(65536).read_to_end(&mut bytes).map_err(|_| unavailable())?;
        for line in bytes.split(|b| *b == b'\n').rev() {
            let Some(payload) = line.strip_prefix(b"ERROR ") else { continue };
            let Ok(error) = serde_json::from_slice::<Value>(payload) else { continue };
            let Some(report) = error.get("solver_recovery") else { return Ok(None) };
            return Ok(implexity_core::error::validate_solver_recovery(report).ok()
                .filter(|r| r["status"] == "needs_attention"));
        }
        Ok(None)
    }

    pub fn terminal_observation_directory(&self, control: &ManagedEvaluationControl) -> MResult<PathBuf> {
        let record = self.authenticate(control)?;
        let m = record.lock();
        if !m.state.terminal() || m.state == ManagedEvaluationState::Succeeded {
            return contract("operation has no failed-terminal observation authority");
        }
        let is_real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        match &m.terminal_observation_directory {
            Some(d)
                if d.parent() == Some(record.quarantine_directory.as_path())
                    && is_real_dir(&record.quarantine_directory)
                    && is_real_dir(d) =>
            {
                Ok(d.clone())
            }
            _ => Err(ManagedEvaluationError::Error(
                "retained terminal observation directory unavailable".into(),
            )),
        }
    }


    pub fn committed_directory(&self, control: &ManagedEvaluationControl) -> MResult<PathBuf> {
        let record = self.authenticate(control)?;
        if record.lock().state != ManagedEvaluationState::Succeeded {
            return contract("managed operation has no committed terminal");
        }
        let ok = std::fs::symlink_metadata(&record.committed_directory).is_ok_and(|m| m.file_type().is_dir());
        if !ok {
            return Err(ManagedEvaluationError::Error("committed terminal directory is unavailable".into()));
        }
        Ok(record.committed_directory.clone())
    }


    pub fn wait(&self, operation_id: &str, timeout_s: Option<f64>) -> MResult<PublicManagedEvaluationStatus> {
        require_operation_id(operation_id)?;
        if timeout_s.is_some_and(|t| !t.is_finite() || t < 0.0) {
            return contract("observer timeout must be finite and non-negative");
        }
        let deadline = timeout_s.map(|t| monotonic() + t);
        let mut state = self.shared.lock_state();
        loop {
            let Some(record) = state.records.get(operation_id).cloned() else {
                drop(state);
                return self.shared.public_status_from_journal(operation_id);
            };
            let status = {
                let m = record.lock();
                Shared::public_status(&record, &m)?
            };
            if status.terminal {
                return Ok(status);
            }
            let wait = match deadline {
                Some(d) => {
                    let remaining = d - monotonic();
                    if remaining <= 0.0 {
                        return Ok(status);
                    }
                    remaining.min(record.policy.poll_interval_s)
                }
                None => record.policy.poll_interval_s,
            };
            state = self
                .shared
                .condition
                .wait_timeout(state, crate::worker_cli::duration_from_secs(wait))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }


    pub fn close(&self) -> MResult<()> {
        let threads = {
            let mut state = self.shared.lock_state();
            if state.closed {
                return Ok(());
            }
            if state.starting > 0 {
                return Err(ManagedEvaluationError::Error(
                    "cannot close while an operation start is in progress".into(),
                ));
            }
            if state.records.values().any(|r| !r.lock().state.terminal()) {
                return Err(ManagedEvaluationError::Error(
                    "cannot close a manager with nonterminal operations".into(),
                ));
            }
            state.closed = true;
            std::mem::take(&mut state.threads)
        };
        for handle in threads {
            let _ = handle.join();
        }
        let mut guard = self.lock_file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(file) = guard.take() {
            let _ = file.unlock();
        }
        Ok(())
    }

    #[must_use]
    pub fn recoverable_operations(&self) -> Vec<RecoverableManagedEvaluation> {
        self.shared.lock_state().pending_recovery.values().map(|p| p.descriptor.clone()).collect()
    }


    #[allow(clippy::too_many_lines)]
    pub fn reattach(
        &self,
        operation_id: &str,
        validate_and_prepare_terminal: TerminalValidator,
    ) -> MResult<ManagedEvaluationControl> {
        require_operation_id(operation_id)?;
        let mut state = self.shared.lock_state();
        if state.closed {
            return contract("managed evaluation manager is closed");
        }
        let Some(pending) = state.pending_recovery.get(operation_id).cloned() else {
            return contract("managed operation is not pending restart recovery");
        };
        let private = self.shared.operations.join(operation_id);
        let message = read_private_json(&private.join("journal.json"))?;
        let Some(message) =
            message.filter(|m| m.get("schema").and_then(Value::as_str) == Some(JOURNAL_SCHEMA))
        else {
            return contract("managed recovery journal is unavailable");
        };
        let owner = owner_from_wire(message.get("recovery_owner"))?;
        let descriptor = &pending.descriptor;
        if owner != Some((descriptor.owner_kind.clone(), descriptor.owner_id.clone())) {
            return contract("managed recovery owner identity drifted");
        }
        let policy = ManagedEvaluationPolicy::from_wire(message.get("policy"))?;
        let digest = match message.get("control_token_digest").and_then(Value::as_str) {
            Some(d) if crate::private::is_sha256(d) => d.to_string(),
            _ => return contract("managed recovery token digest is malformed"),
        };
        let token = self.shared.read_control_capability(&private, operation_id, &digest)?;
        let root_identity = identity_from_wire(message.get("root_identity"))?;
        if root_identity != pending.root_identity {
            return contract("managed recovery root identity drifted");
        }
        let observed = self.shared.reader.observe(root_identity.pid())?;
        let exact_root_live = observed.is_some_and(|o| o.identity.stable_key() == root_identity.stable_key());
        let group = self.shared.reader.process_group_snapshot(root_identity.process_group_id())?;
        if descriptor.root_live {
            if !exact_root_live {
                return ownership("managed recovery root birth identity is no longer live");
            }
        } else if exact_root_live || !group.is_empty() {
            return ownership("managed recovery liveness changed before reattachment");
        }
        let Some(rows) =
            message.get("tracked_descendants").map_or(Some(&Vec::new()), Value::as_array).cloned()
        else {
            return contract("managed recovery descendants are malformed");
        };
        let mut tracked = BTreeMap::new();
        for row in &rows {
            let identity = identity_from_wire(Some(row))?;
            tracked.insert(identity.stable_key(), identity);
        }
        if !descriptor.root_live {
            for identity in tracked.values() {
                if self
                    .shared
                    .reader
                    .observe(identity.pid())?
                    .is_some_and(|o| o.identity.stable_key() == identity.stable_key())
                {
                    return ownership("managed recovery descendant is still live");
                }
            }
        }
        let lifecycle = message.get("state").and_then(Value::as_str).and_then(ManagedEvaluationState::parse);
        let phase = message.get("phase").and_then(Value::as_str).filter(|p| require_generic_phase(p).is_ok());
        let (Some(lifecycle), Some(phase)) = (lifecycle, phase) else {
            return contract("managed recovery lifecycle is malformed");
        };
        if lifecycle.terminal() {
            return contract("terminal managed operation cannot be reattached");
        }
        let Some(started_epoch) = message
            .get("started_epoch")
            .filter(|v| v.is_number())
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
        else {
            return contract("managed recovery start time is malformed");
        };
        let elapsed = (epoch_seconds() - started_epoch).max(0.0);
        let int_or = |k: &str| -> Option<i64> {
            match message.get(k) {
                None => Some(0),
                Some(v) if v.is_i64() => v.as_i64().filter(|x| *x >= 0),
                Some(_) => None,
            }
        };
        let bool_or = |k: &str| -> Option<bool> {
            match message.get(k) {
                None => Some(false),
                Some(Value::Bool(b)) => Some(*b),
                Some(_) => None,
            }
        };
        let (Some(last_phase), Some(cancel_seq), Some(cancel_req), Some(timeout_req), Some(memory_exc)) = (
            int_or("last_phase_sequence"),
            int_or("cancel_sequence"),
            bool_or("cancellation_requested"),
            bool_or("timeout_requested"),
            bool_or("memory_exceeded"),
        ) else {
            return contract("managed recovery durable state is malformed");
        };
        let mut m = RecordMut::new(token.clone(), lifecycle, phase);
        m.root_identity = Some(root_identity);
        m.tracked_descendants = tracked;
        m.last_phase_sequence = last_phase;
        m.cancel_sequence = cancel_seq;
        m.cancellation_requested = cancel_req;
        m.timeout_requested = timeout_req;
        m.memory_exceeded = memory_exc;
        let private_resolved =
            std::fs::canonicalize(&private).map_err(|e| ManagedEvaluationError::Error(e.to_string()))?;
        let record = Arc::new(Record {
            operation_id: operation_id.to_string(),
            token_digest: digest,
            partial_directory: private.join("partial"),
            committed_directory: private.join("committed"),
            quarantine_directory: private.join("quarantine"),
            private_directory: private_resolved,
            policy,
            validate_and_prepare_terminal,
            started_epoch,
            started_monotonic: monotonic() - elapsed,
            recovery_owner: owner,
            reattached: true,
            m: Mutex::new(m),
        });
        state.pending_recovery.remove(operation_id);
        drop(state);
        self.spawn_supervisor(&record, &format!("managed-recovery-{}", &operation_id[..12]))?;
        Ok(ManagedEvaluationControl { operation_id: operation_id.to_string(), token, brand: self.brand })
    }

    #[allow(clippy::too_many_lines)]
    fn recover_abandoned_without_signals(&self) -> MResult<()> {
        let io = |e: std::io::Error| ManagedEvaluationError::Error(e.to_string());
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&self.shared.operations).map_err(io)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for private in entries {
            let is_dir = std::fs::symlink_metadata(&private).is_ok_and(|m| m.file_type().is_dir());
            if !is_dir {
                continue;
            }
            let name = private.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if require_operation_id(&name).is_err() {
                continue;
            }
            let journal_path = private.join("journal.json");
            let Some(message) = read_private_json(&journal_path)? else { continue };
            let schema = message.get("schema").and_then(Value::as_str).unwrap_or("");
            if schema != LEGACY_JOURNAL_SCHEMA && schema != JOURNAL_SCHEMA {
                continue;
            }
            let state = message.get("state").and_then(Value::as_str).unwrap_or("");
            if is_terminal_state(state) {
                continue;
            }
            let Some(root_row) = message.get("root_identity").filter(|v| !v.is_null()) else {
                return in_use("prior managed operation has no provably absent birth identity");
            };
            let mut raw = vec![root_row.clone()];
            match message.get("tracked_descendants") {
                None => {}
                Some(Value::Array(rows)) => raw.extend(rows.iter().cloned()),
                Some(_) => return in_use("prior managed operation descendant identity is malformed"),
            }
            let identities: Vec<ProcessBirthIdentity> =
                match raw.iter().map(|r| identity_from_wire(Some(r))).collect() {
                    Ok(v) => v,
                    Err(_) => return in_use("prior managed operation identity is malformed"),
                };
            let census = (|| -> Result<(Vec<ProcessBirthIdentity>, bool), ResourceTelemetryError> {
                let mut live = Vec::new();
                for identity in &identities {
                    if self
                        .shared
                        .reader
                        .observe(identity.pid())?
                        .is_some_and(|o| o.identity.stable_key() == identity.stable_key())
                    {
                        live.push(identity.clone());
                    }
                }
                let group =
                    !self.shared.reader.process_group_snapshot(identities[0].process_group_id())?.is_empty();
                Ok((live, group))
            })();
            let Ok((exact_live, group)) = census else {
                return in_use("prior managed operation absence cannot be proved");
            };
            let owner = if schema == JOURNAL_SCHEMA {
                match owner_from_wire(message.get("recovery_owner")) {
                    Ok(o) => o,
                    Err(_) => return in_use("prior managed recovery owner is malformed"),
                }
            } else {
                None
            };
            if let Some((kind, owner_id)) = owner {
                let root_live =
                    exact_live.first().is_some_and(|l| l.stable_key() == identities[0].stable_key());
                if !root_live && (!exact_live.is_empty() || group) {
                    return in_use("prior managed process group is still executing ambiguously");
                }
                let descriptor = (|| -> MResult<RecoverableManagedEvaluation> {
                    ManagedEvaluationPolicy::from_wire(message.get("policy"))?;
                    let Some(digest) = message.get("control_token_digest").and_then(Value::as_str) else {
                        return contract("managed recovery token digest is malformed");
                    };
                    self.shared.read_control_capability(&private, &name, digest)?;
                    RecoverableManagedEvaluation {
                        operation_id: name.clone(),
                        owner_kind: kind.clone(),
                        owner_id: owner_id.clone(),
                        state: state.to_string(),
                        phase: implexity_core::pyobj::py_str(message.get("phase").unwrap_or(&Value::Null)),
                        root_live,
                    }
                    .checked()
                })();
                let Ok(descriptor) = descriptor else {
                    return in_use("prior managed recovery contract is invalid");
                };
                self.shared.lock_state().pending_recovery.insert(
                    name.clone(),
                    PendingRecovery { descriptor, root_identity: identities[0].clone() },
                );
                continue;
            }
            if !exact_live.is_empty() {
                return in_use("prior managed operation is still executing");
            }
            if group {
                return in_use("prior managed process group is still executing");
            }
            let quarantine = private.join("quarantine");
            match fsguard::create_dir_owner_only(&quarantine) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io(e)),
            }
            quarantine_path(&private.join("partial"), &quarantine, "abandoned_on_restart")?;
            let mut recovered = message.clone();
            let started = message.get("started_epoch").and_then(Value::as_f64).unwrap_or_else(epoch_seconds);
            recovered.insert("state".into(), Value::String("abandoned".into()));
            recovered.insert("phase".into(), Value::String("terminal".into()));
            recovered.insert("terminal_reason".into(), Value::String("abandoned_on_restart".into()));
            recovered.insert("terminal_elapsed_s".into(), Value::from((epoch_seconds() - started).max(0.0)));
            recovered.insert("updated_epoch".into(), Value::from(epoch_seconds()));
            atomic_private_json(&journal_path, &recovered)?;
        }
        Ok(())
    }
}

impl Drop for ManagedEvaluationManager {
    fn drop(&mut self) {
        let mut guard = self.lock_file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(file) = guard.take() {
            let _ = file.unlock();
        }
    }
}

fn load_or_create_restart_key(root: &Path) -> MResult<Vec<u8>> {
    use std::io::Read;
    let path = root.join(".restart.key");
    let io = |e: std::io::Error| ManagedEvaluationError::Error(e.to_string());
    let mut file = match open_nofollow(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let candidate = token_bytes(RESTART_KEY_BYTES).map_err(io)?;
            match create_exclusive(&path, 0o600) {
                Ok(mut f) => {
                    write_all_sync(&mut f, &candidate).map_err(io)?;
                    drop(f);
                    fsync_dir(root).map_err(io)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io(e)),
            }
            open_nofollow(&path).map_err(io)?
        }
        Err(e) => return Err(io(e)),
    };
    let meta = fsguard::stat_file(&file).map_err(io)?;
    if !meta.is_owned_single_regular() || !meta.owner_only || meta.size != RESTART_KEY_BYTES as u64 {
        return contract("managed restart key ownership or mode is unsafe");
    }
    let mut key = Vec::new();
    (&mut file).take(RESTART_KEY_BYTES as u64 + 1).read_to_end(&mut key).map_err(io)?;
    if key.len() != RESTART_KEY_BYTES {
        return contract("managed restart key is malformed");
    }
    Ok(key)
}
