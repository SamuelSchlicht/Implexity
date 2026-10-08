// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use serde_json::{Map, Value};

use crate::canonical::{canonical_sha256, is_canonical_text, is_digest};
use crate::computation_effort::MAX_SAFE_JSON_INTEGER;

const IDENTITY_SCHEMA: &str = "implexity-exact-provider-session-identity/3";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Reuse(String),
    #[error("{0}")]
    Residency(String),
    #[error("{0}")]
    Disabled(String),
    #[error("{0}")]
    Capacity(String),
    #[error("{0}")]
    InUse(String),
    #[error("{0}")]
    Callback(String),
    #[error("{0}")]
    Poisoned(String),
    #[error("{0}")]
    Miss(String),
    #[error("{0}")]
    Closed(String),
    #[error("{0}")]
    Process(String),
    #[error("{0}")]
    Thread(String),
    #[error("{0}")]
    Trace(String),
}

type SessionResult<T> = Result<T, SessionError>;

fn contract<T>(m: impl Into<String>) -> SessionResult<T> {
    Err(SessionError::Contract(m.into()))
}

fn checked_text(name: &str, value: &str) -> SessionResult<()> {
    if value.is_empty() || value != value.trim() {
        return contract(format!("{name} must be a non-empty canonical string"));
    }
    if value.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return contract(format!("{name} must not contain control characters"));
    }
    if !is_canonical_text(value) {
        return contract(format!("{name} must use canonical Unicode NFC"));
    }
    Ok(())
}

fn checked_sha256(name: &str, value: &str) -> SessionResult<()> {
    checked_text(name, value)?;
    if is_digest(value) { Ok(()) } else { contract(format!("{name} must be a lowercase SHA-256 digest")) }
}

pub const TEXT_FIELDS: [&str; 5] =
    ["registry_fingerprint", "provider_id", "design_layout_identity", "backend", "precision"];
pub const DIGEST_FIELDS: [&str; 16] = [
    "canonical_problem_digest",
    "runtime_source_tree_digest",
    "source_manifest_file_digest",
    "source_manifest_aggregate_digest",
    "operation_signature_digest",
    "shape_signature_digest",
    "runtime_abi_digest",
    "compiler_abi_digest",
    "provider_profile_digest",
    "preconditioner_identity_digest",
    "hardware_target_digest",
    "device_configuration_digest",
    "process_identity_digest",
    "thread_runtime_digest",
    "library_abi_digest",
    "static_arguments_digest",
];
pub const FIELD_NAMES: [&str; 23] = [
    "registry_generation",
    "registry_fingerprint",
    "provider_id",
    "canonical_problem_digest",
    "design_layout_identity",
    "runtime_source_tree_digest",
    "source_manifest_file_digest",
    "source_manifest_aggregate_digest",
    "backend",
    "precision",
    "operation_signature_digest",
    "shape_signature_digest",
    "runtime_abi_digest",
    "compiler_abi_digest",
    "provider_profile_digest",
    "preconditioner_identity_digest",
    "hardware_target_digest",
    "device_configuration_digest",
    "process_identity_digest",
    "thread_runtime_digest",
    "library_abi_digest",
    "static_arguments_digest",
    "compile_flags",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactProviderSessionIdentity {
    registry_generation: i64,
    text: BTreeMap<&'static str, String>,
    compile_flags: Vec<String>,
}

impl ExactProviderSessionIdentity {

    pub fn new(
        registry_generation: i64,
        fields: &BTreeMap<String, String>,
        compile_flags: Vec<String>,
    ) -> SessionResult<Self> {
        if registry_generation < 0 {
            return contract("registry_generation must be a non-negative integer");
        }
        if registry_generation > MAX_SAFE_JSON_INTEGER {
            return contract("registry_generation exceeds the interoperable JSON integer range");
        }
        let mut text = BTreeMap::new();
        for name in TEXT_FIELDS {
            let v = fields.get(name).map_or("", String::as_str);
            checked_text(name, v)?;
            text.insert(name, v.to_string());
        }
        for name in DIGEST_FIELDS {
            let v = fields.get(name).map_or("", String::as_str);
            checked_sha256(name, v)?;
            text.insert(name, v.to_string());
        }
        for (i, flag) in compile_flags.iter().enumerate() {
            checked_text(&format!("compile_flags[{i}]"), flag)?;
        }
        let mut unique = compile_flags.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != compile_flags.len() {
            return contract("compile_flags must not contain duplicates");
        }
        Ok(Self { registry_generation, text, compile_flags })
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.text.get(name).map(String::as_str)
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(IDENTITY_SCHEMA.into()));
        for name in FIELD_NAMES {
            let v = match name {
                "registry_generation" => Value::from(self.registry_generation),
                "compile_flags" => {
                    Value::Array(self.compile_flags.iter().cloned().map(Value::String).collect())
                }
                other => Value::String(self.text.get(other).cloned().unwrap_or_default()),
            };
            m.insert(name.into(), v);
        }
        Value::Object(m)
    }


    pub fn from_wire(row: &Value) -> SessionResult<Self> {
        let Some(m) = row.as_object().filter(|m| {
            m.len() == FIELD_NAMES.len() + 1
                && m.contains_key("schema")
                && FIELD_NAMES.iter().all(|k| m.contains_key(*k))
        }) else {
            return contract("exact provider identity wire fields do not match the canonical schema");
        };
        if m["schema"].as_str() != Some(IDENTITY_SCHEMA) {
            return contract("unsupported exact provider session identity schema");
        }
        let Some(flags) = m["compile_flags"].as_array() else {
            return contract("wire compile_flags must be an array");
        };
        let flags: Vec<String> = flags
            .iter()
            .enumerate()
            .map(|(i, f)| {
                f.as_str().map(str::to_string).ok_or_else(|| {
                    SessionError::Contract(format!("compile_flags[{i}] must be a non-empty canonical string"))
                })
            })
            .collect::<SessionResult<_>>()?;
        let generation = match &m["registry_generation"] {
            Value::Number(n) if !n.is_f64() => n.as_i64().unwrap_or(-1),
            _ => return contract("registry_generation must be a non-negative integer"),
        };
        let mut fields = BTreeMap::new();
        for name in TEXT_FIELDS.iter().chain(&DIGEST_FIELDS) {
            let Some(s) = m[*name].as_str() else {
                return contract(format!("{name} must be a non-empty canonical string"));
            };
            fields.insert((*name).to_string(), s.to_string());
        }
        Self::new(generation, &fields, flags)
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        canonical_sha256(&self.to_wire())
    }

    #[must_use]
    pub fn drift_fields(&self, other: &Self) -> Vec<&'static str> {
        FIELD_NAMES
            .iter()
            .copied()
            .filter(|name| match *name {
                "registry_generation" => self.registry_generation != other.registry_generation,
                "compile_flags" => self.compile_flags != other.compile_flags,
                n => self.text.get(n) != other.text.get(n),
            })
            .collect()
    }


    pub fn require_reusable_with(&self, other: &Self) -> SessionResult<()> {
        let drift = self.drift_fields(other);
        if drift.is_empty() {
            Ok(())
        } else {
            Err(SessionError::Reuse(format!("exact provider session identity drift: {}", drift.join(", "))))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExecutableResidencyPolicy {
    enabled: bool,
    byte_cap: i64,
}

impl ExecutableResidencyPolicy {

    pub fn new(enabled: bool, byte_cap: i64) -> SessionResult<Self> {
        if enabled {
            if byte_cap <= 0 {
                return contract("enabled residency requires a positive byte_cap");
            }
        } else if byte_cap != 0 {
            return contract("disabled residency requires byte_cap zero");
        }
        Ok(Self { enabled, byte_cap })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableResidencyFootprint {
    pub measured_bytes: i64,
    pub conservative_overhead_bytes: i64,
    pub measurement_evidence_digest: String,
}

impl ExecutableResidencyFootprint {

    pub fn new(measured_bytes: i64, conservative_overhead_bytes: i64, digest: &str) -> SessionResult<Self> {
        for (name, v) in
            [("measured_bytes", measured_bytes), ("conservative_overhead_bytes", conservative_overhead_bytes)]
        {
            if v < 0 {
                return contract(format!("{name} must be a non-negative integer"));
            }
        }
        if measured_bytes <= 0 {
            return contract("measured_bytes must be positive");
        }
        checked_sha256("measurement_evidence_digest", digest)?;
        Ok(Self { measured_bytes, conservative_overhead_bytes, measurement_evidence_digest: digest.into() })
    }

    #[must_use]
    pub fn accounted_bytes(&self) -> i64 {
        self.measured_bytes + self.conservative_overhead_bytes
    }
}

pub type Resource = Arc<dyn Any + Send + Sync>;
pub type LifecycleCallback = Arc<dyn Fn(&Resource) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
pub struct ExecutableLifecycle {
    pub on_release: LifecycleCallback,
    pub on_evict: LifecycleCallback,
}

impl std::fmt::Debug for ExecutableLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExecutableLifecycle")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "the Python snapshot record")]
pub struct ExecutableResidencySnapshot {
    pub enabled: bool,
    pub byte_cap: i64,
    pub used_bytes: i64,
    pub entry_count: usize,
    pub active_leases: usize,
    pub hits: i64,
    pub misses: i64,
    pub installs: i64,
    pub releases: i64,
    pub evictions: i64,
    pub poisoned: bool,
    pub retired: bool,
    pub closed: bool,
    pub owner_pid: u32,
}

struct Entry {
    identity: ExactProviderSessionIdentity,
    resource: Resource,
    footprint: ExecutableResidencyFootprint,
    lifecycle: ExecutableLifecycle,
    last_touch: i64,
    active_token: Option<u64>,
    owner_thread: Option<ThreadId>,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "the Python pool's independent lifecycle flags")]
struct PoolState {
    entries: BTreeMap<String, Entry>,
    used_bytes: i64,
    clock: i64,
    hits: i64,
    misses: i64,
    installs: i64,
    releases: i64,
    evictions: i64,
    poisoned: bool,
    retired: bool,
    closed: bool,
    next_token: u64,
}

#[derive(Default)]
struct Shared {
    state: Mutex<PoolState>,
    callback: Mutex<Option<ThreadId>>,
}

impl Shared {
    fn lock(&self) -> SessionResult<std::sync::MutexGuard<'_, PoolState>> {
        let active = self.callback.lock().map_or(None, |c| *c);
        if active == Some(std::thread::current().id()) {
            return Err(SessionError::Residency(
                "residency operations are forbidden from lifecycle callbacks".into(),
            ));
        }
        self.state.lock().map_err(|_| {
            SessionError::Poisoned("residency pool is retired; terminate its owning worker".into())
        })
    }
}

pub struct ExecutableResidencyPool {
    policy: ExecutableResidencyPolicy,
    owner_pid: u32,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ExecutableResidencyPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutableResidencyPool").field("policy", &self.policy).finish_non_exhaustive()
    }
}

fn trace_point(
    policy: &ExecutableResidencyPolicy,
    s: &PoolState,
    event: &str,
    identity: Option<&ExactProviderSessionIdentity>,
    extra: Vec<(&str, Value)>,
) -> SessionResult<()> {
    let enabled =
        implexity_solve::trace::enabled().map_err(|e| SessionError::Trace(e.message().to_string()))?;
    if !enabled {
        return Ok(());
    }
    implexity_solve::trace::point(event, || {
        let mut f = Map::new();
        f.insert("residency_byte_cap".into(), Value::from(policy.byte_cap));
        f.insert("residency_used_bytes".into(), Value::from(s.used_bytes));
        f.insert("residency_entry_count".into(), Value::from(s.entries.len()));
        f.insert(
            "residency_active_leases".into(),
            Value::from(s.entries.values().filter(|e| e.active_token.is_some()).count()),
        );
        f.insert("residency_hits".into(), Value::from(s.hits));
        f.insert("residency_misses".into(), Value::from(s.misses));
        f.insert("residency_installs".into(), Value::from(s.installs));
        f.insert("residency_releases".into(), Value::from(s.releases));
        f.insert("residency_evictions".into(), Value::from(s.evictions));
        if let Some(id) = identity {
            f.insert("provider_session_identity_sha256".into(), Value::String(id.sha256()));
            for (key, field) in [
                ("canonical_problem_sha256", "canonical_problem_digest"),
                ("runtime_source_tree_sha256", "runtime_source_tree_digest"),
                ("source_manifest_file_sha256", "source_manifest_file_digest"),
                ("source_manifest_aggregate_sha256", "source_manifest_aggregate_digest"),
                ("provider_profile_sha256", "provider_profile_digest"),
                ("runtime_abi_sha256", "runtime_abi_digest"),
                ("compiler_abi_sha256", "compiler_abi_digest"),
                ("shape_signature_sha256", "shape_signature_digest"),
            ] {
                f.insert(key.into(), Value::String(id.field(field).unwrap_or_default().to_string()));
            }
        }
        for (k, v) in extra {
            f.insert(k.into(), v);
        }
        f
    })
    .map_err(|e| SessionError::Trace(e.message().to_string()))
}

fn run_callback(
    shared: &Shared,
    s: &mut PoolState,
    name: &str,
    callback: &LifecycleCallback,
    resource: &Resource,
) -> SessionResult<()> {
    if let Ok(mut c) = shared.callback.lock() {
        *c = Some(std::thread::current().id());
    }
    let result = callback(resource);
    if let Ok(mut c) = shared.callback.lock() {
        *c = None;
    }
    if result.is_ok() {
        return Ok(());
    }
    s.poisoned = true;
    s.retired = true;
    Err(SessionError::Callback(format!("{name} callback failed; residency pool is terminally retired")))
}

impl ExecutableResidencyPool {
    #[must_use]
    pub fn new(policy: ExecutableResidencyPolicy) -> Self {
        Self { policy, owner_pid: std::process::id(), shared: Arc::new(Shared::default()) }
    }

    fn lock(&self) -> SessionResult<std::sync::MutexGuard<'_, PoolState>> {
        self.shared.lock()
    }

    fn require_ready(&self, s: &mut PoolState) -> SessionResult<()> {
        if std::process::id() != self.owner_pid {
            s.poisoned = true;
            s.retired = true;
            return Err(SessionError::Process(
                "residency pool cannot be reused after process/fork drift".into(),
            ));
        }
        if s.closed {
            return Err(SessionError::Closed("residency pool is closed".into()));
        }
        if s.poisoned || s.retired {
            return Err(SessionError::Poisoned(
                "residency pool is retired; terminate its owning worker".into(),
            ));
        }
        Ok(())
    }

    fn evict_unlocked(&self, s: &mut PoolState, key: &str) -> SessionResult<bool> {
        let Some(entry) = s.entries.get(key) else { return Ok(false) };
        if entry.active_token.is_some() {
            return Err(SessionError::InUse("leased executable resource cannot be evicted".into()));
        }
        let (callback, resource) = (Arc::clone(&entry.lifecycle.on_evict), Arc::clone(&entry.resource));
        run_callback(&self.shared, s, "evict", &callback, &resource)?;
        let Some(entry) = s.entries.remove(key) else { return Ok(false) };
        s.used_bytes -= entry.footprint.accounted_bytes();
        s.evictions += 1;
        let bytes = entry.footprint.accounted_bytes();
        if let Err(e) = trace_point(
            &self.policy,
            s,
            "exact_provider_residency_eviction",
            Some(&entry.identity),
            vec![("residency_evicted_bytes", Value::from(bytes))],
        ) {
            s.poisoned = true;
            s.retired = true;
            return Err(e);
        }
        Ok(true)
    }


    #[allow(clippy::needless_pass_by_value, reason = "the pool takes ownership of the installed entry")]
    pub fn install(
        &self,
        identity: ExactProviderSessionIdentity,
        resource: Resource,
        footprint: ExecutableResidencyFootprint,
        lifecycle: ExecutableLifecycle,
    ) -> SessionResult<()> {
        let key = identity.sha256();
        let size = footprint.accounted_bytes();
        let mut s = self.lock()?;
        self.require_ready(&mut s)?;
        if !self.policy.enabled {
            return Err(SessionError::Disabled("executable residency is disabled".into()));
        }
        if let Some(existing) = s.entries.get(&key) {
            existing.identity.require_reusable_with(&identity)?;
            return Err(SessionError::Residency("resident identity already exists".into()));
        }
        if size > self.policy.byte_cap {
            return Err(SessionError::Capacity(
                "measured resource footprint exceeds the residency byte cap".into(),
            ));
        }
        let required = s.used_bytes + size - self.policy.byte_cap;
        let mut victims = Vec::new();
        if required > 0 {
            let mut candidates: Vec<(i64, String, i64)> = s
                .entries
                .iter()
                .filter(|(_, e)| e.active_token.is_none())
                .map(|(k, e)| (e.last_touch, k.clone(), e.footprint.accounted_bytes()))
                .collect();
            candidates.sort();
            let mut reclaimable = 0;
            for (_, k, bytes) in candidates {
                victims.push(k);
                reclaimable += bytes;
                if reclaimable >= required {
                    break;
                }
            }
            if reclaimable < required {
                return Err(SessionError::Capacity(
                    "byte cap cannot be satisfied without evicting a live lease".into(),
                ));
            }
        }
        for victim in victims {
            self.evict_unlocked(&mut s, &victim)?;
        }
        s.clock += 1;
        let touch = s.clock;
        let on_evict = Arc::clone(&lifecycle.on_evict);
        s.entries.insert(
            key.clone(),
            Entry {
                identity: identity.clone(),
                resource: Arc::clone(&resource),
                footprint: footprint.clone(),
                lifecycle,
                last_touch: touch,
                active_token: None,
                owner_thread: None,
            },
        );
        s.used_bytes += size;
        s.installs += 1;
        if let Err(trace_error) = trace_point(
            &self.policy,
            &s,
            "exact_provider_residency_install",
            Some(&identity),
            vec![
                ("residency_installed_bytes", Value::from(size)),
                ("residency_measured_bytes", Value::from(footprint.measured_bytes)),
                ("residency_conservative_overhead_bytes", Value::from(footprint.conservative_overhead_bytes)),
                (
                    "residency_measurement_evidence_sha256",
                    Value::String(footprint.measurement_evidence_digest.clone()),
                ),
            ],
        ) {
            s.entries.remove(&key);
            s.used_bytes -= size;
            s.installs -= 1;
            let cleanup =
                run_callback(&self.shared, &mut s, "evict after install trace failure", &on_evict, &resource);
            s.poisoned = true;
            s.retired = true;
            cleanup?;
            return Err(trace_error);
        }
        Ok(())
    }


    pub fn lease(&self, identity: &ExactProviderSessionIdentity) -> SessionResult<ExecutableLease> {
        let key = identity.sha256();
        let mut s = self.lock()?;
        self.require_ready(&mut s)?;
        if !self.policy.enabled {
            return Err(SessionError::Disabled("executable residency is disabled".into()));
        }
        if !s.entries.contains_key(&key) {
            s.misses += 1;
            if let Err(e) =
                trace_point(&self.policy, &s, "exact_provider_residency_miss", Some(identity), vec![])
            {
                s.poisoned = true;
                s.retired = true;
                return Err(e);
            }
            return Err(SessionError::Miss("resident executable identity was not found".into()));
        }
        s.entries[&key].identity.require_reusable_with(identity)?;
        if s.entries.values().any(|e| e.active_token.is_some()) {
            return Err(SessionError::InUse("residency pool permits only one active lease".into()));
        }
        s.next_token += 1;
        let token = s.next_token;
        let owner = std::thread::current().id();
        s.hits += 1;
        s.clock += 1;
        let touch = s.clock;
        let resource = {
            let entry = s
                .entries
                .get_mut(&key)
                .ok_or_else(|| SessionError::Miss("resident executable identity was not found".into()))?;
            entry.active_token = Some(token);
            entry.owner_thread = Some(owner);
            entry.last_touch = touch;
            Arc::clone(&entry.resource)
        };
        if let Err(e) = trace_point(
            &self.policy,
            &s,
            "exact_provider_residency_lease",
            Some(identity),
            vec![("residency_action", Value::String("hit".into()))],
        ) {
            if let Some(entry) = s.entries.get_mut(&key) {
                entry.active_token = None;
                entry.owner_thread = None;
            }
            s.poisoned = true;
            s.retired = true;
            return Err(e);
        }
        Ok(ExecutableLease {
            pool: Arc::clone(&self.shared),
            policy: self.policy,
            owner_pid: self.owner_pid,
            identity: identity.clone(),
            token,
            resource: Some(resource),
            owner_thread: owner,
        })
    }


    pub fn evict(&self, identity: &ExactProviderSessionIdentity) -> SessionResult<bool> {
        let key = identity.sha256();
        let mut s = self.lock()?;
        self.require_ready(&mut s)?;
        if let Some(entry) = s.entries.get(&key) {
            entry.identity.require_reusable_with(identity)?;
        }
        self.evict_unlocked(&mut s, &key)
    }

    fn evict_all(&self, s: &mut PoolState, action: &str) -> SessionResult<()> {
        if s.entries.values().any(|e| e.active_token.is_some()) {
            return Err(SessionError::InUse(format!("cannot {action} residency while a resource is leased")));
        }
        let mut order: Vec<(i64, String)> =
            s.entries.iter().map(|(k, e)| (e.last_touch, k.clone())).collect();
        order.sort();
        for (_, key) in order {
            self.evict_unlocked(s, &key)?;
        }
        Ok(())
    }


    pub fn clear(&self) -> SessionResult<()> {
        let mut s = self.lock()?;
        self.require_ready(&mut s)?;
        self.evict_all(&mut s, "clear")
    }


    pub fn close(&self) -> SessionResult<()> {
        let mut s = self.lock()?;
        self.require_ready(&mut s)?;
        self.evict_all(&mut s, "close")?;
        s.closed = true;
        trace_point(&self.policy, &s, "exact_provider_residency_close", None, vec![])
    }

    #[must_use]
    pub fn snapshot(&self) -> ExecutableResidencySnapshot {
        let s = self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        ExecutableResidencySnapshot {
            enabled: self.policy.enabled,
            byte_cap: self.policy.byte_cap,
            used_bytes: s.used_bytes,
            entry_count: s.entries.len(),
            active_leases: s.entries.values().filter(|e| e.active_token.is_some()).count(),
            hits: s.hits,
            misses: s.misses,
            installs: s.installs,
            releases: s.releases,
            evictions: s.evictions,
            poisoned: s.poisoned,
            retired: s.retired,
            closed: s.closed,
            owner_pid: self.owner_pid,
        }
    }
}

pub struct ExecutableLease {
    pool: Arc<Shared>,
    policy: ExecutableResidencyPolicy,
    owner_pid: u32,
    identity: ExactProviderSessionIdentity,
    token: u64,
    resource: Option<Resource>,
    owner_thread: ThreadId,
}

impl std::fmt::Debug for ExecutableLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<executable lease>")
    }
}

impl ExecutableLease {
    fn require_live(&self, s: &mut PoolState, verb: &str) -> SessionResult<()> {
        if std::process::id() != self.owner_pid {
            s.poisoned = true;
            s.retired = true;
            return Err(SessionError::Process(
                "residency pool cannot be reused after process/fork drift".into(),
            ));
        }
        if s.closed {
            return Err(SessionError::Closed("residency pool is closed".into()));
        }
        if s.poisoned || s.retired {
            return Err(SessionError::Poisoned(
                "residency pool is retired; terminate its owning worker".into(),
            ));
        }
        if std::thread::current().id() != self.owner_thread {
            return Err(SessionError::Thread(format!(
                "executable lease {verb} must occur on its owner thread"
            )));
        }
        let key = self.identity.sha256();
        let Some(entry) = s.entries.get(&key) else {
            return Err(SessionError::Residency("executable lease token is stale".into()));
        };
        if entry.active_token != Some(self.token) || entry.owner_thread != Some(self.owner_thread) {
            return Err(SessionError::Residency("executable lease token is stale".into()));
        }
        entry.identity.require_reusable_with(&self.identity)
    }


    pub fn resource(&self) -> SessionResult<Resource> {
        if std::thread::current().id() != self.owner_thread {
            return Err(SessionError::Thread("executable lease is thread-affine".into()));
        }
        let Some(own) = &self.resource else {
            return Err(SessionError::Residency("executable lease is closed".into()));
        };
        let mut s = self.pool.lock()?;
        self.require_live(&mut s, "access")?;
        let current = &s.entries[&self.identity.sha256()].resource;
        if !Arc::ptr_eq(current, own) {
            return Err(SessionError::Residency(
                "resident executable resource differs from its lease".into(),
            ));
        }
        Ok(Arc::clone(current))
    }


    pub fn close(&mut self) -> SessionResult<()> {
        if std::thread::current().id() != self.owner_thread {
            return Err(SessionError::Thread("executable lease is thread-affine".into()));
        }
        if self.resource.is_none() {
            return Err(SessionError::Residency("executable lease was already closed".into()));
        }
        let result = self.release();
        self.resource = None;
        result
    }

    fn release(&self) -> SessionResult<()> {
        let mut s = self.pool.lock()?;
        self.require_live(&mut s, "release")?;
        let key = self.identity.sha256();
        let (callback, resource) = {
            let e = &s.entries[&key];
            (Arc::clone(&e.lifecycle.on_release), Arc::clone(&e.resource))
        };
        let outcome = run_callback(&self.pool, &mut s, "release", &callback, &resource);
        s.clock += 1;
        let touch = s.clock;
        if let Some(e) = s.entries.get_mut(&key) {
            e.active_token = None;
            e.owner_thread = None;
            e.last_touch = touch;
        }
        outcome?;
        s.releases += 1;
        if let Err(e) =
            trace_point(&self.policy, &s, "exact_provider_residency_release", Some(&self.identity), vec![])
        {
            s.poisoned = true;
            s.retired = true;
            return Err(e);
        }
        Ok(())
    }
}

impl Drop for ExecutableLease {
    fn drop(&mut self) {
        if self.resource.is_some() && std::thread::current().id() == self.owner_thread {

            let _ = self.close();
        }
    }
}

