// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use implexity_core::CaeError;
use serde_json::{Map, Value};

use crate::canonical::{canonical_sha256, is_digest};
use crate::worker_runtime_profile::{WorkerRuntimeProfile, WorkerRuntimeProfileIssuer};

pub const EXACT_EXECUTION_BINDING_SCHEMA: &str = "implexity-private-exact-execution-binding/2";
pub const NONDEFAULT_EXACT_PROFILE: &str = "nondefault_exact_profile";
pub const PARALLEL_LOGICAL_BATCH: &str = "parallel_logical_batch";
pub const EXECUTABLE_RESIDENCY: &str = "executable_residency";
pub const SPARSE_STRUCTURE_REUSE: &str = "sparse_structure_reuse";
pub const PROVIDER_OWNED_EXACT_PRECONDITIONER: &str = "provider_owned_exact_preconditioner";
pub const TRANSACTIONAL_KRYLOV_RECYCLE: &str = "transactional_krylov_recycle";
pub const FEATURES: [&str; 6] = [
    EXECUTABLE_RESIDENCY,
    NONDEFAULT_EXACT_PROFILE,
    PARALLEL_LOGICAL_BATCH,
    PROVIDER_OWNED_EXACT_PRECONDITIONER,
    SPARSE_STRUCTURE_REUSE,
    TRANSACTIONAL_KRYLOV_RECYCLE,
];
const MAX_LOGICAL_BATCH_CONCURRENCY: i64 = 64;
const MAX_LOGICAL_BATCH_MEMORY_BYTES: i64 = 64 * 1024 * 1024;
const MAX_EXECUTABLE_RESIDENCY_MEMORY_BYTES: i64 = 12 * 1024 * 1024 * 1024;
const MAX_SPARSE_STRUCTURE_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExactExecutionAuthorityError {
    #[error("{0}")]
    Authority(String),
    #[error("{0}")]
    Replay(String),
    #[error("{0}")]
    Scope(String),
    #[error("{0}")]
    Drift(String),
}

impl From<ExactExecutionAuthorityError> for CaeError {
    fn from(e: ExactExecutionAuthorityError) -> Self {
        CaeError::contract(e.to_string())
    }
}

type AuthResult<T> = Result<T, ExactExecutionAuthorityError>;

fn authority<T>(m: &str) -> AuthResult<T> {
    Err(ExactExecutionAuthorityError::Authority(m.into()))
}

fn digest(value: &str, label: &str) -> AuthResult<String> {
    if is_digest(value) {
        Ok(value.to_string())
    } else {
        authority(&format!("{label} must be a lowercase SHA-256 digest"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactExecutionBinding {
    session_sha256: String,
    operation_sha256: String,
    provider_profile_sha256: String,
    policy_sha256: String,
    compile_sha256: String,
    runtime_profile_sha256: String,
    qualification_sha256: String,
    features: Vec<String>,
    logical_batch_concurrency: i64,
    logical_batch_memory_limit_bytes: i64,
    executable_residency_memory_limit_bytes: i64,
    sparse_structure_memory_limit_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingArgs {
    pub session_sha256: String,
    pub operation_sha256: String,
    pub provider_profile_sha256: String,
    pub policy_sha256: String,
    pub compile_sha256: String,
    pub runtime_profile_sha256: String,
    pub qualification_sha256: String,
    pub features: Vec<String>,
    pub logical_batch_concurrency: i64,
    pub logical_batch_memory_limit_bytes: i64,
    pub executable_residency_memory_limit_bytes: i64,
    pub sparse_structure_memory_limit_bytes: i64,
}

impl ExactExecutionBinding {

    pub fn new(args: BindingArgs) -> AuthResult<Self> {
        let session_sha256 = digest(&args.session_sha256, "session_sha256")?;
        let operation_sha256 = digest(&args.operation_sha256, "operation_sha256")?;
        let provider_profile_sha256 = digest(&args.provider_profile_sha256, "provider_profile_sha256")?;
        let policy_sha256 = digest(&args.policy_sha256, "policy_sha256")?;
        let compile_sha256 = digest(&args.compile_sha256, "compile_sha256")?;
        let runtime_profile_sha256 = digest(&args.runtime_profile_sha256, "runtime_profile_sha256")?;
        let qualification_sha256 = digest(&args.qualification_sha256, "qualification_sha256")?;
        let mut sorted = args.features.clone();
        sorted.sort();
        sorted.dedup();
        if sorted != args.features || !args.features.iter().all(|f| FEATURES.contains(&f.as_str())) {
            return authority("exact-execution features must be a sorted unique closed tuple");
        }
        let has = |f: &str| args.features.iter().any(|x| x == f);
        let concurrency = args.logical_batch_concurrency;
        let memory = args.logical_batch_memory_limit_bytes;
        if has(PARALLEL_LOGICAL_BATCH) {
            if !(2..=MAX_LOGICAL_BATCH_CONCURRENCY).contains(&concurrency) {
                return authority("parallel logical-batch concurrency must be in [2,64]");
            }
            if !(1..=MAX_LOGICAL_BATCH_MEMORY_BYTES).contains(&memory) {
                return authority("parallel logical-batch memory limit must be in [1,67108864]");
            }
        } else if concurrency != 1 || memory != 0 {
            return authority("disabled logical-batch execution must retain serial zero-budget facts");
        }
        let residency = args.executable_residency_memory_limit_bytes;
        let resident = has(EXECUTABLE_RESIDENCY);
        if resident {
            if !(1..=MAX_EXECUTABLE_RESIDENCY_MEMORY_BYTES).contains(&residency) {
                return authority("executable residency memory limit must be in [1,12884901888]");
            }
        } else if residency != 0 {
            return authority("disabled executable residency must retain a zero budget");
        }
        let sparse = args.sparse_structure_memory_limit_bytes;
        if has(SPARSE_STRUCTURE_REUSE) {
            if !(1..=MAX_SPARSE_STRUCTURE_MEMORY_BYTES).contains(&sparse) {
                return authority("sparse-structure reuse memory limit must be in [1,2147483648]");
            }
            if !resident || sparse > residency {
                return authority("sparse-structure reuse requires residency and a sub-budget");
            }
        } else if sparse != 0 {
            return authority("disabled sparse-structure reuse must retain a zero budget");
        }
        if (has(PROVIDER_OWNED_EXACT_PRECONDITIONER) || has(TRANSACTIONAL_KRYLOV_RECYCLE))
            && !has(NONDEFAULT_EXACT_PROFILE)
        {
            return authority("accelerated solve mechanisms require a nondefault profile");
        }
        Ok(Self {
            session_sha256,
            operation_sha256,
            provider_profile_sha256,
            policy_sha256,
            compile_sha256,
            runtime_profile_sha256,
            qualification_sha256,
            features: args.features,
            logical_batch_concurrency: concurrency,
            logical_batch_memory_limit_bytes: memory,
            executable_residency_memory_limit_bytes: residency,
            sparse_structure_memory_limit_bytes: sparse,
        })
    }

    #[must_use]
    pub fn features(&self) -> &[String] {
        &self.features
    }

    #[must_use]
    pub fn digest_field(&self, name: &str) -> Option<&str> {
        Some(match name {
            "session_sha256" => &self.session_sha256,
            "operation_sha256" => &self.operation_sha256,
            "provider_profile_sha256" => &self.provider_profile_sha256,
            "policy_sha256" => &self.policy_sha256,
            "compile_sha256" => &self.compile_sha256,
            "runtime_profile_sha256" => &self.runtime_profile_sha256,
            "qualification_sha256" => &self.qualification_sha256,
            _ => return None,
        })
    }

    #[must_use]
    pub fn logical_batch_concurrency(&self) -> i64 {
        self.logical_batch_concurrency
    }

    #[must_use]
    pub fn logical_batch_memory_limit_bytes(&self) -> i64 {
        self.logical_batch_memory_limit_bytes
    }

    #[must_use]
    pub fn executable_residency_memory_limit_bytes(&self) -> i64 {
        self.executable_residency_memory_limit_bytes
    }

    #[must_use]
    pub fn sparse_structure_memory_limit_bytes(&self) -> i64 {
        self.sparse_structure_memory_limit_bytes
    }

    #[must_use]
    pub fn to_provenance(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(EXACT_EXECUTION_BINDING_SCHEMA.into()));
        for name in [
            "session_sha256",
            "operation_sha256",
            "provider_profile_sha256",
            "policy_sha256",
            "compile_sha256",
            "runtime_profile_sha256",
            "qualification_sha256",
        ] {
            m.insert(name.into(), Value::String(self.digest_field(name).unwrap_or_default().to_string()));
        }
        m.insert("features".into(), Value::Array(self.features.iter().cloned().map(Value::String).collect()));
        m.insert("logical_batch_concurrency".into(), Value::from(self.logical_batch_concurrency));
        m.insert(
            "logical_batch_memory_limit_bytes".into(),
            Value::from(self.logical_batch_memory_limit_bytes),
        );
        m.insert(
            "executable_residency_memory_limit_bytes".into(),
            Value::from(self.executable_residency_memory_limit_bytes),
        );
        m.insert(
            "sparse_structure_memory_limit_bytes".into(),
            Value::from(self.sparse_structure_memory_limit_bytes),
        );
        Value::Object(m)
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        canonical_sha256(&self.to_provenance())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordState {
    Issued,
    Active,
    Drifted,
}

#[derive(Debug)]
struct Record {
    binding: ExactExecutionBinding,
    runtime_profile: WorkerRuntimeProfile,
    owner_pid: u32,
    owner_thread: ThreadId,
    state: RecordState,
}

#[derive(Debug)]
struct LedgerState {
    records: HashMap<u64, Record>,
    active_token: Option<u64>,
}

#[derive(Debug)]
struct Ledger {
    brand: u64,
    runtime_issuer: WorkerRuntimeProfileIssuer,
    owner_pid: u32,
    owner_thread: ThreadId,
    next: AtomicU64,
    state: Mutex<LedgerState>,
}

impl Ledger {
    fn require_owner(&self) -> AuthResult<()> {
        if std::process::id() != self.owner_pid || std::thread::current().id() != self.owner_thread {
            return Err(ExactExecutionAuthorityError::Scope(
                "exact-execution authority is process- and thread-bound".into(),
            ));
        }
        Ok(())
    }

    fn lock(&self) -> AuthResult<std::sync::MutexGuard<'_, LedgerState>> {
        self.state.lock().map_err(|_| {
            ExactExecutionAuthorityError::Authority("exact-execution ledger is unavailable".into())
        })
    }

    fn runtime_ok(&self, profile: &WorkerRuntimeProfile) -> bool {
        self.runtime_issuer.require(profile).is_ok() && profile.require_current_environment(None).is_ok()
    }
}

static BRANDS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct ExactExecutionLease {
    brand: u64,
    token: u64,
    pid: u32,
    thread: ThreadId,
}

#[derive(Debug, Clone)]
pub struct ExactExecutionCapability {
    ledger: Arc<Ledger>,
    token: u64,
    binding: ExactExecutionBinding,
    runtime_profile: WorkerRuntimeProfile,
    pid: u32,
    thread: ThreadId,
}

thread_local! {
    static ACTIVE_CAPABILITY: RefCell<Option<ExactExecutionCapability>> = const { RefCell::new(None) };
}

impl ExactExecutionCapability {

    pub fn binding(&self) -> AuthResult<&ExactExecutionBinding> {
        self.require_active()?;
        Ok(&self.binding)
    }


    pub fn supports(&self, feature: &str) -> AuthResult<bool> {
        self.require_active()?;
        if !FEATURES.contains(&feature) {
            return authority("unknown exact-execution feature");
        }
        Ok(self.binding.features.iter().any(|f| f == feature))
    }

    fn require_active(&self) -> AuthResult<()> {
        let ledger = &self.ledger;
        let mut state = ledger.lock()?;
        ledger.require_owner()?;
        if std::process::id() != self.pid || std::thread::current().id() != self.thread {
            return Err(ExactExecutionAuthorityError::Scope(
                "exact-execution capability is outside its owner scope".into(),
            ));
        }
        let active = state.active_token;
        let Some(record) = state.records.get_mut(&self.token) else {
            return Err(ExactExecutionAuthorityError::Scope(
                "exact-execution capability is stale or inactive".into(),
            ));
        };
        if record.state != RecordState::Active || active != Some(self.token) {
            return Err(ExactExecutionAuthorityError::Scope(
                "exact-execution capability is stale or inactive".into(),
            ));
        }
        if !ledger.runtime_ok(&self.runtime_profile) {
            record.state = RecordState::Drifted;
            return Err(ExactExecutionAuthorityError::Drift("exact-execution worker runtime drifted".into()));
        }
        Ok(())
    }

    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.ledger, &other.ledger) && self.token == other.token
    }
}

#[derive(Debug)]
pub struct ExactExecutionAuthorityIssuer {
    ledger: Arc<Ledger>,
}

impl ExactExecutionAuthorityIssuer {

    pub fn issue(
        &self,
        binding: ExactExecutionBinding,
        runtime_profile: &WorkerRuntimeProfile,
    ) -> AuthResult<ExactExecutionLease> {
        let ledger = &self.ledger;
        let mut state = ledger.lock()?;
        ledger.require_owner()?;
        ledger
            .runtime_issuer
            .require(runtime_profile)
            .map_err(|e| ExactExecutionAuthorityError::Authority(e.to_string()))?;
        runtime_profile
            .require_current_environment(None)
            .map_err(|e| ExactExecutionAuthorityError::Authority(e.to_string()))?;
        if binding.runtime_profile_sha256 != runtime_profile.sha256() {
            return Err(ExactExecutionAuthorityError::Drift(
                "execution binding does not match the worker runtime profile".into(),
            ));
        }
        let has = |f: &str| binding.features.iter().any(|x| x == f);
        if has(PARALLEL_LOGICAL_BATCH)
            && (binding.logical_batch_concurrency > runtime_profile.logical_batch_concurrency_limit()
                || binding.logical_batch_memory_limit_bytes
                    > runtime_profile.logical_batch_memory_limit_bytes())
        {
            return authority("logical-batch request exceeds the server runtime profile");
        }
        if has(EXECUTABLE_RESIDENCY)
            && binding.executable_residency_memory_limit_bytes
                > runtime_profile.executable_residency_memory_limit_bytes()
        {
            return authority("executable-residency request exceeds the server runtime profile");
        }
        if has(SPARSE_STRUCTURE_REUSE)
            && binding.sparse_structure_memory_limit_bytes
                > runtime_profile.sparse_structure_memory_limit_bytes()
        {
            return authority("sparse-structure request exceeds the server runtime profile");
        }
        let token = ledger.next.fetch_add(1, Ordering::Relaxed);
        state.records.insert(
            token,
            Record {
                binding,
                runtime_profile: runtime_profile.clone(),
                owner_pid: std::process::id(),
                owner_thread: std::thread::current().id(),
                state: RecordState::Issued,
            },
        );
        Ok(ExactExecutionLease {
            brand: ledger.brand,
            token,
            pid: std::process::id(),
            thread: std::thread::current().id(),
        })
    }
}

#[derive(Debug)]
pub struct ExactExecutionAuthorityGate {
    ledger: Arc<Ledger>,
}

#[derive(Debug)]
pub struct ActiveExactExecution {
    capability: ExactExecutionCapability,
    owner_pid: u32,
    owner_thread: ThreadId,
}

impl ActiveExactExecution {
    #[must_use]
    pub fn capability(&self) -> &ExactExecutionCapability {
        &self.capability
    }


    pub fn close(mut self) -> AuthResult<()> {
        let result = self.finish();
        std::mem::forget(self);
        result
    }

    fn finish(&mut self) -> AuthResult<()> {
        let ledger = Arc::clone(&self.capability.ledger);
        let token = self.capability.token;
        let mut scope_error = None;
        if std::process::id() != self.owner_pid || std::thread::current().id() != self.owner_thread {
            scope_error = Some(ExactExecutionAuthorityError::Scope(
                "exact-execution scope exited on a foreign owner".into(),
            ));
        }
        if let Ok(mut state) = ledger.state.lock() {
            if scope_error.is_none() && state.active_token != Some(token) {
                scope_error =
                    Some(ExactExecutionAuthorityError::Scope("exact-execution scope token drifted".into()));
            }
            state.active_token = None;
            state.records.remove(&token);
        }
        let cleared = ACTIVE_CAPABILITY.with(|slot| {
            let mut slot = slot.borrow_mut();
            let mine = slot.as_ref().is_some_and(|c| c.same(&self.capability));
            if mine {
                *slot = None;
            }
            mine
        });
        if !cleared && scope_error.is_none() {
            scope_error = Some(ExactExecutionAuthorityError::Scope(
                "exact-execution scope exited in a foreign context".into(),
            ));
        }
        scope_error.map_or(Ok(()), Err)
    }
}

impl Drop for ActiveExactExecution {
    fn drop(&mut self) {

        let _ = self.finish();
    }
}

impl ExactExecutionAuthorityGate {
    #[allow(clippy::needless_pass_by_value, reason = "leases are move-only: activation consumes them")]

    pub fn activate(
        &self,
        lease: ExactExecutionLease,
        expected: &ExactExecutionBinding,
    ) -> AuthResult<ActiveExactExecution> {
        let ledger = &self.ledger;
        let mut state = ledger.lock()?;
        ledger.require_owner()?;
        if lease.brand != ledger.brand {
            return Err(ExactExecutionAuthorityError::Replay(
                "exact-execution lease belongs to a foreign boundary".into(),
            ));
        }
        let valid = state.records.get(&lease.token).is_some_and(|r| {
            r.state == RecordState::Issued
                && lease.pid == std::process::id()
                && lease.thread == std::thread::current().id()
        });
        if !valid {
            return Err(ExactExecutionAuthorityError::Replay(
                "exact-execution lease is stale or crossed its owner".into(),
            ));
        }
        if state.records.get(&lease.token).is_some_and(|r| &r.binding != expected) {
            state.records.remove(&lease.token);
            return Err(ExactExecutionAuthorityError::Drift(
                "exact-execution lease does not match the expected binding".into(),
            ));
        }
        let nested = state.active_token.is_some() || ACTIVE_CAPABILITY.with(|slot| slot.borrow().is_some());
        if nested {
            return Err(ExactExecutionAuthorityError::Scope(
                "nested exact-execution authority scopes are forbidden".into(),
            ));
        }
        let runtime_ok =
            state.records.get(&lease.token).is_some_and(|r| ledger.runtime_ok(&r.runtime_profile));
        if !runtime_ok {
            state.records.remove(&lease.token);
            return Err(ExactExecutionAuthorityError::Drift(
                "exact-execution worker runtime drifted before activation".into(),
            ));
        }
        let Some(record) = state.records.get_mut(&lease.token) else {
            return Err(ExactExecutionAuthorityError::Replay(
                "exact-execution lease is stale or crossed its owner".into(),
            ));
        };
        record.state = RecordState::Active;
        let capability = ExactExecutionCapability {
            ledger: Arc::clone(ledger),
            token: lease.token,
            binding: record.binding.clone(),
            runtime_profile: record.runtime_profile.clone(),
            pid: record.owner_pid,
            thread: record.owner_thread,
        };
        let (owner_pid, owner_thread) = (record.owner_pid, record.owner_thread);
        state.active_token = Some(lease.token);
        drop(state);
        ACTIVE_CAPABILITY.with(|slot| *slot.borrow_mut() = Some(capability.clone()));
        Ok(ActiveExactExecution { capability, owner_pid, owner_thread })
    }
}

#[must_use]
pub fn create_exact_execution_authority(
    runtime_issuer: WorkerRuntimeProfileIssuer,
) -> (ExactExecutionAuthorityIssuer, ExactExecutionAuthorityGate) {
    let ledger = Arc::new(Ledger {
        brand: BRANDS.fetch_add(1, Ordering::Relaxed),
        runtime_issuer,
        owner_pid: std::process::id(),
        owner_thread: std::thread::current().id(),
        next: AtomicU64::new(1),
        state: Mutex::new(LedgerState { records: HashMap::new(), active_token: None }),
    });
    (ExactExecutionAuthorityIssuer { ledger: Arc::clone(&ledger) }, ExactExecutionAuthorityGate { ledger })
}


pub fn current_exact_execution_capability(required: bool) -> AuthResult<Option<ExactExecutionCapability>> {
    let value = ACTIVE_CAPABILITY.with(|slot| slot.borrow().clone());
    let Some(capability) = value else {
        if required {
            return Err(ExactExecutionAuthorityError::Scope(
                "a live private exact-execution capability is required".into(),
            ));
        }
        return Ok(None);
    };
    match capability.require_active() {
        Ok(()) => Ok(Some(capability)),
        Err(ExactExecutionAuthorityError::Scope(m)) => {
            ACTIVE_CAPABILITY.with(|slot| *slot.borrow_mut() = None);
            if required { Err(ExactExecutionAuthorityError::Scope(m)) } else { Ok(None) }
        }
        Err(other) => Err(other),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpectedBinding<'a> {
    pub session_sha256: Option<&'a str>,
    pub operation_sha256: Option<&'a str>,
    pub provider_profile_sha256: Option<&'a str>,
    pub policy_sha256: Option<&'a str>,
    pub compile_sha256: Option<&'a str>,
    pub runtime_profile_sha256: Option<&'a str>,
    pub qualification_sha256: Option<&'a str>,
}


pub fn require_exact_execution_capability(
    value: Option<&ExactExecutionCapability>,
    required_features: &[&str],
    expected: &ExpectedBinding<'_>,
    label: &str,
) -> AuthResult<ExactExecutionCapability> {
    let capability = match value {
        Some(c) => c.clone(),
        None => current_exact_execution_capability(true)?.ok_or_else(|| {
            ExactExecutionAuthorityError::Scope(format!("{label} must be a live private capability"))
        })?,
    };
    capability.require_active()?;
    let current = current_exact_execution_capability(true)?;
    if !current.is_some_and(|c| c.same(&capability)) {
        return Err(ExactExecutionAuthorityError::Scope(format!(
            "{label} is not the capability active in this context"
        )));
    }
    if required_features.iter().any(|f| !FEATURES.contains(f)) {
        return authority("required exact-execution features are not canonical");
    }
    let mut missing: Vec<String> = required_features
        .iter()
        .filter(|f| !capability.binding.features.iter().any(|x| x == *f))
        .map(|f| (*f).to_string())
        .collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        return authority(&format!(
            "{label} lacks required features: {}",
            implexity_core::pyobj::list_repr(&missing)
        ));
    }
    for (name, wanted) in [
        ("session_sha256", expected.session_sha256),
        ("operation_sha256", expected.operation_sha256),
        ("provider_profile_sha256", expected.provider_profile_sha256),
        ("policy_sha256", expected.policy_sha256),
        ("compile_sha256", expected.compile_sha256),
        ("runtime_profile_sha256", expected.runtime_profile_sha256),
        ("qualification_sha256", expected.qualification_sha256),
    ] {
        let Some(wanted) = wanted else { continue };
        let wanted = digest(wanted, &format!("expected {name}"))?;
        if capability.binding.digest_field(name) != Some(wanted.as_str()) {
            return Err(ExactExecutionAuthorityError::Drift(format!("{label} binding drifted at {name}")));
        }
    }
    Ok(capability)
}

