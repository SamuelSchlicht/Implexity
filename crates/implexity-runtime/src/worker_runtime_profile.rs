// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::ThreadId;

use serde_json::{Map, Value};

use crate::canonical::{canonical_sha256, obj, s};

pub const RUNTIME_PROFILE_SCHEMA: &str = "implexity-worker-runtime-profile/2";
pub const SERIAL_RUNTIME_POLICY: &str = "server_exact_serial_v1";
pub const BOUNDED_HOST_RUNTIME_POLICY: &str = "server_exact_bounded_host_v1";
pub const PREIMPORT_BOOTSTRAP_SCHEMA: &str = "implexity-worker-preimport-bootstrap/1";

const MAX_LOGICAL_BATCH_MEMORY_BYTES: i64 = 64 * 1024 * 1024;
const MAX_EXECUTABLE_RESIDENCY_MEMORY_BYTES: i64 = 12 * 1024 * 1024 * 1024;
const MAX_SPARSE_STRUCTURE_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const MANAGED_ENVIRONMENT_KEYS: [&str; 12] = [
    "BLIS_NUM_THREADS",
    "JAX_ENABLE_X64",
    "MKL_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
    "OMP_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "TF_NUM_INTEROP_THREADS",
    "TF_NUM_INTRAOP_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "XLA_FLAGS",
    "IMPLEXITY_WORKER_RUNTIME_PROFILE_SCHEMA",
    "IMPLEXITY_WORKER_RUNTIME_PROFILE_SHA256",
];
pub const BOOTSTRAP_ENVIRONMENT_KEYS: [&str; 3] = [
    "IMPLEXITY_WORKER_PREIMPORT_BOOTSTRAP_SCHEMA",
    "IMPLEXITY_WORKER_PREIMPORT_NUMERICAL_MODULES_JSON",
    "IMPLEXITY_WORKER_PREIMPORT_VALIDATED_PROFILE_SHA256",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerRuntimeProfileError {
    #[error("{0}")]
    Profile(String),
    #[error("{0}")]
    ImportOrder(String),
    #[error("{0}")]
    Environment(String),
}

impl From<WorkerRuntimeProfileError> for implexity_core::CaeError {
    fn from(e: WorkerRuntimeProfileError) -> Self {
        Self::contract(e.to_string())
    }
}

type ProfileResult<T> = Result<T, WorkerRuntimeProfileError>;

fn profile_err<T>(message: &str) -> ProfileResult<T> {
    Err(WorkerRuntimeProfileError::Profile(message.into()))
}


pub fn canonical_environment(value: &BTreeMap<String, String>) -> ProfileResult<BTreeMap<String, String>> {
    let mut upper: BTreeMap<String, String> = BTreeMap::new();
    for (key, item) in value {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return profile_err("worker subprocess environment contains an invalid key");
        }
        if item.contains('\0') {
            return profile_err("worker subprocess environment contains a non-string value");
        }
        let folded = key.to_uppercase();
        if let Some(previous) = upper.get(&folded)
            && previous != key
        {
            return profile_err("worker subprocess environment contains case-colliding keys");
        }
        upper.insert(folded, key.clone());
    }
    Ok(value.clone())
}

#[must_use]
pub fn logical_cpu_count() -> Option<i64> {
    if let Ok(text) = std::fs::read_to_string("/sys/devices/system/cpu/online") {
        let mut count = 0_i64;
        for part in text.trim().split(',').filter(|p| !p.is_empty()) {
            let mut bounds = part.split('-');
            let lo: i64 = bounds.next()?.trim().parse().ok()?;
            let hi: i64 = match bounds.next() {
                Some(h) => h.trim().parse().ok()?,
                None => lo,
            };
            count += hi - lo + 1;
        }
        if count > 0 {
            return Some(count);
        }
    }
    std::thread::available_parallelism().ok().and_then(|n| i64::try_from(n.get()).ok())
}

fn closed_profile_payload(policy: &str, logical_cpu_count: i64) -> ProfileResult<Map<String, Value>> {
    if !(1..=(1 << 20)).contains(&logical_cpu_count) {
        return profile_err("worker logical CPU count is outside the closed runtime contract");
    }
    let (threads, concurrency, batch_memory, residency, sparse, eigen) = match policy {
        SERIAL_RUNTIME_POLICY => (1, 1, 0, 0, 0, "false"),
        BOUNDED_HOST_RUNTIME_POLICY => {
            if logical_cpu_count < 2 {
                return profile_err("bounded host runtime requires at least two logical CPUs");
            }
            let threads = logical_cpu_count.min(8);
            let concurrency =
                [2_i64, 4, 8].into_iter().filter(|t| *t <= logical_cpu_count.min(8)).max().unwrap_or(2);
            (
                threads,
                concurrency,
                MAX_LOGICAL_BATCH_MEMORY_BYTES,
                MAX_EXECUTABLE_RESIDENCY_MEMORY_BYTES,
                MAX_SPARSE_STRUCTURE_MEMORY_BYTES,
                "true",
            )
        }
        _ => return profile_err("unsupported server worker-runtime policy"),
    };
    let mut environment = Map::new();
    for (k, v) in [
        ("BLIS_NUM_THREADS", "1".to_string()),
        ("JAX_ENABLE_X64", "true".to_string()),
        ("MKL_NUM_THREADS", "1".to_string()),
        ("NUMEXPR_NUM_THREADS", "1".to_string()),
        ("OMP_NUM_THREADS", "1".to_string()),
        ("OPENBLAS_NUM_THREADS", "1".to_string()),
        ("TF_NUM_INTEROP_THREADS", "1".to_string()),
        ("TF_NUM_INTRAOP_THREADS", threads.to_string()),
        ("VECLIB_MAXIMUM_THREADS", "1".to_string()),
        ("XLA_FLAGS", format!("--xla_cpu_multi_thread_eigen={eigen} intra_op_parallelism_threads={threads}")),
    ] {
        environment.insert(k.into(), Value::String(v));
    }
    let mut payload = Map::new();
    payload.insert("schema".into(), s(RUNTIME_PROFILE_SCHEMA));
    payload.insert("policy".into(), s(policy));
    payload.insert("logical_cpu_count".into(), Value::from(logical_cpu_count));
    payload.insert("worker_thread_limit".into(), Value::from(threads));
    payload.insert("logical_batch_concurrency_limit".into(), Value::from(concurrency));
    payload.insert("logical_batch_memory_limit_bytes".into(), Value::from(batch_memory));
    payload.insert("executable_residency_memory_limit_bytes".into(), Value::from(residency));
    payload.insert("sparse_structure_memory_limit_bytes".into(), Value::from(sparse));
    payload.insert("managed_environment".into(), Value::Object(environment));
    Ok(payload)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRuntimeProfile {
    issuer: u64,
    payload: Map<String, Value>,
    sha256: String,
}

fn int_field(payload: &Map<String, Value>, key: &str) -> i64 {
    payload.get(key).and_then(Value::as_i64).unwrap_or(0)
}

impl WorkerRuntimeProfile {
    #[must_use]
    pub fn policy(&self) -> &str {
        self.payload.get("policy").and_then(Value::as_str).unwrap_or("")
    }

    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    #[must_use]
    pub fn logical_cpu_count(&self) -> i64 {
        int_field(&self.payload, "logical_cpu_count")
    }

    #[must_use]
    pub fn worker_thread_limit(&self) -> i64 {
        int_field(&self.payload, "worker_thread_limit")
    }

    #[must_use]
    pub fn logical_batch_concurrency_limit(&self) -> i64 {
        int_field(&self.payload, "logical_batch_concurrency_limit")
    }

    #[must_use]
    pub fn logical_batch_memory_limit_bytes(&self) -> i64 {
        int_field(&self.payload, "logical_batch_memory_limit_bytes")
    }

    #[must_use]
    pub fn executable_residency_memory_limit_bytes(&self) -> i64 {
        int_field(&self.payload, "executable_residency_memory_limit_bytes")
    }

    #[must_use]
    pub fn sparse_structure_memory_limit_bytes(&self) -> i64 {
        int_field(&self.payload, "sparse_structure_memory_limit_bytes")
    }

    #[must_use]
    pub fn managed_environment(&self) -> BTreeMap<String, String> {
        self.payload
            .get("managed_environment")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn provenance(&self) -> Value {
        obj([
            ("schema", s(RUNTIME_PROFILE_SCHEMA)),
            ("policy", s(self.policy())),
            ("logical_cpu_count", Value::from(self.logical_cpu_count())),
            ("worker_thread_limit", Value::from(self.worker_thread_limit())),
            ("logical_batch_concurrency_limit", Value::from(self.logical_batch_concurrency_limit())),
            ("logical_batch_memory_limit_bytes", Value::from(self.logical_batch_memory_limit_bytes())),
            (
                "executable_residency_memory_limit_bytes",
                Value::from(self.executable_residency_memory_limit_bytes()),
            ),
            ("sparse_structure_memory_limit_bytes", Value::from(self.sparse_structure_memory_limit_bytes())),
            ("profile_sha256", s(&self.sha256)),
        ])
    }


    pub fn sanitized_subprocess_environment(
        &self,
        inherited: &BTreeMap<String, String>,
    ) -> ProfileResult<BTreeMap<String, String>> {
        let mut result = canonical_environment(inherited)?;
        result.retain(|k, _| {
            let upper = k.to_uppercase();
            !MANAGED_ENVIRONMENT_KEYS.contains(&upper.as_str())
                && !BOOTSTRAP_ENVIRONMENT_KEYS.contains(&upper.as_str())
        });
        result.extend(self.managed_environment());
        result.insert("IMPLEXITY_WORKER_RUNTIME_PROFILE_SCHEMA".into(), RUNTIME_PROFILE_SCHEMA.into());
        result.insert("IMPLEXITY_WORKER_RUNTIME_PROFILE_SHA256".into(), self.sha256.clone());
        Ok(result)
    }


    pub fn require_current_environment(
        &self,
        environment: Option<&BTreeMap<String, String>>,
    ) -> ProfileResult<()> {
        let actual = match environment {
            Some(e) => e.clone(),
            None => std::env::vars_os()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect(),
        };
        let checked = canonical_environment(&actual)?;
        let expected = self.sanitized_subprocess_environment(&checked)?;
        for key in MANAGED_ENVIRONMENT_KEYS {
            if checked.get(key) != expected.get(key) {
                return Err(WorkerRuntimeProfileError::Environment(format!(
                    "worker runtime environment drifted at managed key {key}"
                )));
            }
        }
        Ok(())
    }

    fn require_issuer(&self, issuer: u64) -> ProfileResult<()> {
        if self.issuer == issuer {
            Ok(())
        } else {
            profile_err("worker runtime profile was issued by a foreign catalog")
        }
    }
}

static ISSUERS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct WorkerRuntimeProfileIssuer {
    brand: u64,
    owner_pid: u32,
    owner_thread: ThreadId,
}

impl WorkerRuntimeProfileIssuer {
    fn require_owner(&self) -> ProfileResult<()> {
        if std::process::id() != self.owner_pid || std::thread::current().id() != self.owner_thread {
            return profile_err("worker runtime profile issuer is process- and thread-bound");
        }
        Ok(())
    }


    pub fn issue(&self, policy: &str) -> ProfileResult<WorkerRuntimeProfile> {
        self.require_owner()?;
        let Some(count) = logical_cpu_count() else {
            return profile_err("worker logical CPU count is unavailable");
        };
        self.issue_for(policy, count)
    }

    fn issue_for(&self, policy: &str, count: i64) -> ProfileResult<WorkerRuntimeProfile> {
        let payload = closed_profile_payload(policy, count)?;
        let sha256 = canonical_sha256(&Value::Object(payload.clone()));
        Ok(WorkerRuntimeProfile { issuer: self.brand, payload, sha256 })
    }


    pub fn require<'a>(&self, profile: &'a WorkerRuntimeProfile) -> ProfileResult<&'a WorkerRuntimeProfile> {
        self.require_owner()?;
        profile.require_issuer(self.brand)?;
        if Some(profile.logical_cpu_count()) != logical_cpu_count() {
            return profile_err("worker runtime hardware identity drifted");
        }
        Ok(profile)
    }
}

#[must_use]
pub fn create_worker_runtime_profile_issuer() -> WorkerRuntimeProfileIssuer {
    WorkerRuntimeProfileIssuer {
        brand: ISSUERS.fetch_add(1, Ordering::Relaxed),
        owner_pid: std::process::id(),
        owner_thread: std::thread::current().id(),
    }
}


pub fn profile_sha256_for(policy: &str, logical_cpu_count: i64) -> ProfileResult<String> {
    Ok(canonical_sha256(&Value::Object(closed_profile_payload(policy, logical_cpu_count)?)))
}


pub fn activate_environment(
    profile: &WorkerRuntimeProfile,
    issuer: &WorkerRuntimeProfileIssuer,
    target: &mut BTreeMap<String, String>,
) -> ProfileResult<()> {
    issuer.require(profile)?;
    let sanitized = profile.sanitized_subprocess_environment(target)?;
    target.retain(|k, _| {
        let upper = k.to_uppercase();
        !MANAGED_ENVIRONMENT_KEYS.contains(&upper.as_str())
            && !BOOTSTRAP_ENVIRONMENT_KEYS.contains(&upper.as_str())
    });
    for key in MANAGED_ENVIRONMENT_KEYS {
        if let Some(v) = sanitized.get(key) {
            target.insert(key.into(), v.clone());
        }
    }
    profile.require_current_environment(Some(target))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapEvidence {
    pub schema: String,
    pub validated_profile_sha256: String,
    pub numerical_modules: Vec<String>,
}

static BOOTSTRAP: OnceLock<BootstrapEvidence> = OnceLock::new();

pub const BOOTSTRAP_TARGETS: [&str; 3] = [
    "implexity.cae.accelerated_provider_job",
    "implexity.implicit.accelerated_managed_provider_worker",
    "implexity.implicit.qualification_managed_provider_worker",
];


pub fn bootstrap_worker(
    target: &str,
    policy: &str,
    expected_sha256: &str,
) -> ProfileResult<BootstrapEvidence> {
    if !BOOTSTRAP_TARGETS.contains(&target) {
        return profile_err("worker pre-import bootstrap target is not closed");
    }
    if !crate::canonical::is_digest(expected_sha256) {
        return profile_err("worker runtime profile is not a lowercase SHA-256 digest");
    }
    if BOOTSTRAP.get().is_some() {
        return Err(WorkerRuntimeProfileError::ImportOrder(
            "numerical modules loaded before worker profile validation: implexity".into(),
        ));
    }
    let issuer = create_worker_runtime_profile_issuer();
    let profile = issuer.issue(policy)?;
    issuer.require(&profile)?;
    if profile.sha256() != expected_sha256 {
        return profile_err("worker runtime profile identity drifted");
    }
    profile.require_current_environment(None)?;
    let evidence = BootstrapEvidence {
        schema: PREIMPORT_BOOTSTRAP_SCHEMA.into(),
        validated_profile_sha256: profile.sha256().to_string(),
        numerical_modules: Vec::new(),
    };
    let _ = BOOTSTRAP.set(evidence.clone());
    Ok(evidence)
}


pub fn require_preimport_bootstrap(profile: &WorkerRuntimeProfile) -> ProfileResult<()> {
    match BOOTSTRAP.get() {
        Some(e)
            if e.schema == PREIMPORT_BOOTSTRAP_SCHEMA
                && e.validated_profile_sha256 == profile.sha256()
                && e.numerical_modules.is_empty() =>
        {
            Ok(())
        }
        _ => Err(WorkerRuntimeProfileError::ImportOrder(
            "worker lacks source-owned zero-numerical-import bootstrap evidence".into(),
        )),
    }
}

