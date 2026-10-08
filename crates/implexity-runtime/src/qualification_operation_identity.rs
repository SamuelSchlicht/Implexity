// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::canonical::{canonical_sha256, is_digest, sha256_hex};
use crate::worker_runtime_profile::{
    BOUNDED_HOST_RUNTIME_POLICY, PREIMPORT_BOOTSTRAP_SCHEMA, SERIAL_RUNTIME_POLICY,
    create_worker_runtime_profile_issuer,
};

pub const IDENTITY_EXPECTATION_SCHEMA: &str =
    "implexity-exact-qualification-operation-identity-expectation/1";
pub const DIRECT_PROFILE_ID: &str = "exact_direct";
pub const ACCELERATED_PROFILE_ID: &str = "exact_krylov_bounded_sparse_ilu_v1";

const DIRECT_MECHANISMS: [&str; 1] = ["authoritative_sparse_direct"];
const ACCELERATED_REQUIRED: [&str; 8] = [
    "authoritative_assembled_jacobian",
    "bounded_worker_threads",
    "exact_newton_krylov",
    "final_authoritative_sparse_direct",
    "final_factorization_multi_rhs_reuse",
    "parallel_logical_batch",
    "persistent_fixed_shape_executable_residency",
    "safe_sparse_template_reuse",
];
const ACCELERATED_OPTIONAL: [&str; 2] =
    ["provider_owned_exact_preconditioner", "transactional_krylov_recycle"];
const STAGE57_FROZEN_INPUT_NAMES: [&str; 3] =
    ["intent_screening.json", "model.json", "optimization_baseline_screening.json"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct QualificationOperationIdentityError(pub String);

impl From<QualificationOperationIdentityError> for implexity_core::CaeError {
    fn from(e: QualificationOperationIdentityError) -> Self {
        Self::contract(e.0)
    }
}

type IdResult<T> = Result<T, QualificationOperationIdentityError>;

fn fail<T>(message: impl Into<String>) -> IdResult<T> {
    Err(QualificationOperationIdentityError(message.into()))
}

fn role(record_role: &str) -> Option<(&'static str, &'static str, bool)> {
    match record_role {
        "exact_direct_primal" => Some(("primal", DIRECT_PROFILE_ID, false)),
        "exact_direct_sensitivity" => Some(("sensitivity", DIRECT_PROFILE_ID, false)),
        "accelerated_exact_primal" => Some(("primal", ACCELERATED_PROFILE_ID, true)),
        "accelerated_exact_sensitivity" => Some(("sensitivity", ACCELERATED_PROFILE_ID, true)),
        _ => None,
    }
}


pub fn identity_sha256(value: &Value) -> IdResult<String> {
    if implexity_core::wire::ensure_finite(value).is_err() {
        return fail("qualification identity is not canonical JSON");
    }
    Ok(canonical_sha256(value))
}

fn digest<'a>(value: &'a str, label: &str) -> IdResult<&'a str> {
    if is_digest(value) { Ok(value) } else { fail(format!("{label} is not a lowercase SHA-256 digest")) }
}

fn digest_value<'a>(value: Option<&'a Value>, label: &str) -> IdResult<&'a str> {
    match value.and_then(Value::as_str) {
        Some(v) => digest(v, label),
        None => fail(format!("{label} is not a lowercase SHA-256 digest")),
    }
}

fn mapping(value: &Value, label: &str) -> IdResult<Map<String, Value>> {
    let Value::Object(map) = value else {
        return fail(format!("{label} is not a mapping"));
    };
    if implexity_core::wire::ensure_finite(value).is_err() {
        return fail("qualification identity is not canonical JSON");
    }
    Ok(map.clone())
}

fn file_sha256(path: &Path) -> std::io::Result<(u64, String)> {
    let bytes = std::fs::read(path)?;
    Ok((bytes.len() as u64, sha256_hex(&bytes)))
}

fn collect_python_leaves(dir: &Path, out: &mut Vec<PathBuf>) -> IdResult<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return fail("runtime source package root is unavailable");
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return fail("runtime source tree contains an unsafe leaf");
        };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return fail("runtime source tree contains an unsafe leaf");
        };
        let is_py = path.extension().is_some_and(|e| e == "py");

        if meta.is_dir() {
            if is_py {
                out.push(path.clone());
            }
            collect_python_leaves(&path, out)?;
        } else if is_py {
            out.push(path);
        }
    }
    Ok(())
}



pub fn derive_runtime_source_tree_sha256(source_root: Option<&Path>) -> IdResult<String> {
    let Some(root) = source_root.and_then(|r| std::fs::canonicalize(r).ok()) else {
        return fail("runtime source package root is unavailable");
    };
    let package_root = root.join("service").join("implexity");
    let package_meta = std::fs::symlink_metadata(&package_root);
    if !package_meta.is_ok_and(|m| m.is_dir()) {
        return fail("runtime source package root is unavailable");
    }
    let mut leaves = Vec::new();
    collect_python_leaves(&package_root, &mut leaves)?;

    leaves.sort_by(|a, b| a.components().cmp(b.components()));
    let mut rows = Vec::new();
    for path in leaves {
        if path.components().any(|c| c.as_os_str() == "__pycache__") {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path);
        if !meta.is_ok_and(|m| m.is_file()) {
            return fail("runtime source tree contains an unsafe leaf");
        }
        let Ok((size, sha)) = file_sha256(&path) else {
            return fail("runtime source tree contains an unsafe leaf");
        };
        let Some(relative) = path.strip_prefix(&root).ok().and_then(|p| p.to_str()) else {
            return fail("runtime source tree contains an unsafe leaf");
        };
        rows.push(json!({"path": relative.replace('\\', "/"), "bytes": size, "sha256": sha}));
    }
    if rows.is_empty() {
        return fail("runtime source tree is empty");
    }
    identity_sha256(&Value::Array(rows))
}


pub fn derive_stage57_frozen_input_identity_sha256(root: &Path) -> IdResult<String> {
    let Ok(directory) = std::fs::canonicalize(root) else {
        return fail("Stage-57 frozen input root is unavailable");
    };
    if !std::fs::symlink_metadata(&directory).is_ok_and(|m| m.is_dir()) {
        return fail("Stage-57 frozen input root is unavailable");
    }
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return fail("Stage-57 frozen input root is unavailable");
    };
    let mut discovered = Vec::new();
    for entry in entries.flatten() {
        let is_regular = std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_file());
        if is_regular {
            discovered.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    discovered.sort();
    if discovered != STAGE57_FROZEN_INPUT_NAMES {
        return fail("Stage-57 frozen input file closure differs");
    }
    let mut rows = Vec::new();
    for name in STAGE57_FROZEN_INPUT_NAMES {
        let path = directory.join(name);
        let safe = implexity_io::fsguard::stat_nofollow(&path).is_ok_and(|m| m.is_file() && m.nlink == 1);
        let payload = if safe { std::fs::read(&path).ok() } else { None };
        let Some(payload) = payload else {
            return fail("Stage-57 frozen input leaf is unsafe");
        };
        rows.push(json!({"path": name, "bytes": payload.len(), "sha256": sha256_hex(&payload)}));
    }
    identity_sha256(&json!({"schema": "implexity-stage57-frozen-input-identity/1", "files": rows}))
}


pub fn derive_provider_identity(
    provider_id: &str,
    provider_descriptor: &Value,
    physics_snapshot: &Value,
) -> IdResult<Value> {
    let provider = provider_id.trim();
    let descriptor = mapping(provider_descriptor, "provider descriptor")?;
    let snapshot = mapping(physics_snapshot, "physics snapshot")?;
    let registry = digest_value(snapshot.get("registry_fingerprint"), "provider registry fingerprint")?;
    let load_order = digest_value(snapshot.get("load_order_fingerprint"), "provider load-order fingerprint")?;
    let loaded = snapshot.get("loaded").and_then(Value::as_array);
    let loaded_ok = loaded.is_some_and(|items| {
        let mut seen = std::collections::BTreeSet::new();
        !items.is_empty() && items.iter().all(|i| i.as_str().is_some_and(|s| !s.is_empty() && seen.insert(s)))
    });
    if provider.is_empty() || descriptor.get("name").and_then(Value::as_str) != Some(provider) || !loaded_ok {
        return fail("provider identity source facts are incomplete");
    }
    let generation_hex = sha256_hex(format!("{registry}:{load_order}").as_bytes());
    let generation = u64::from_str_radix(&generation_hex[..13], 16).unwrap_or_default();
    let descriptor_value = Value::Object(descriptor);
    let descriptor_sha = identity_sha256(&descriptor_value)?;
    Ok(json!({
        "provider_id": provider,
        "provider_descriptor": descriptor_value,
        "provider_descriptor_sha256": descriptor_sha,
        "registry_generation": generation,
        "registry_fingerprint": registry,
        "load_order_fingerprint": load_order,
        "loaded_package_ids": loaded.cloned().unwrap_or_default(),
    }))
}


pub fn derive_environment_identity() -> IdResult<Value> {
    fail("benchmark numerical runtime is unavailable")
}

fn is_design_state_id(value: &str) -> bool {
    value.strip_prefix("design-").is_some_and(is_digest)
}


pub fn derive_stage57_science_contract(
    scientific_scope_sha256: &str,
    provider_id: &str,
    frozen_input_identity_sha256: &str,
    design_state_id: &str,
) -> IdResult<Value> {
    let scope = digest(scientific_scope_sha256, "scientific scope")?;
    let frozen = digest(frozen_input_identity_sha256, "frozen input identity")?;
    let provider = provider_id.trim();
    if provider.is_empty() || !is_design_state_id(design_state_id) {
        return fail("Stage-57 science identity is incomplete");
    }
    Ok(json!({
        "schema": "implexity-stage57-exact-science-contract/1",
        "scientific_scope_sha256": scope,
        "provider_id": provider,
        "full_grid": [16, 32, 32],
        "cell_spacing_mm": [0.5, 0.5, 0.5],
        "preserved_times_s": [0.0, 1.0, 10.0],
        "state_size": 135_231,
        "gradient_size": 16_384,
        "frozen_input_identity_sha256": frozen,
        "design_state_id": design_state_id,
    }))
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionProfileInputs<'a> {
    pub record_role: &'a str,
    pub exact_solver_profile: &'a Value,
    pub source_manifest_file_sha256: &'a str,
    pub source_manifest_aggregate_sha256: &'a str,
    pub runtime_source_tree_sha256: &'a str,
    pub provider_id: &'a str,
    pub science_contract_sha256: &'a str,
}

fn mechanism_inventory(accelerated: bool) -> Vec<&'static str> {
    if accelerated {
        let mut all: Vec<&str> =
            ACCELERATED_REQUIRED.iter().chain(ACCELERATED_OPTIONAL.iter()).copied().collect();
        all.sort_unstable();
        all
    } else {
        DIRECT_MECHANISMS.to_vec()
    }
}


pub fn derive_execution_profile(inputs: &ExecutionProfileInputs<'_>) -> IdResult<Value> {
    let Some((_operation, profile_id, accelerated)) = role(inputs.record_role) else {
        return fail("qualification record role is not closed");
    };
    let solver = mapping(inputs.exact_solver_profile, "exact solver profile")?;
    let expected_policy = if accelerated { ACCELERATED_PROFILE_ID } else { "exact_direct_default" };
    if solver.get("solver_policy").and_then(Value::as_str) != Some(expected_policy) {
        return fail("exact solver policy differs from qualification role");
    }
    let source_sha = digest(inputs.source_manifest_file_sha256, "source manifest file digest")?;
    let aggregate_sha = digest(inputs.source_manifest_aggregate_sha256, "source manifest aggregate")?;
    let runtime_sha = digest(inputs.runtime_source_tree_sha256, "runtime source tree")?;
    let science_sha = digest(inputs.science_contract_sha256, "science contract")?;
    let provider = inputs.provider_id.trim();
    if provider.is_empty() {
        return fail("provider id is absent");
    }
    let fixed_shape_sha = identity_sha256(&json!({
        "schema": "implexity-exact-fixed-shape-identity/1",
        "science_contract_sha256": science_sha,
    }))?;
    let compile_cache_sha = identity_sha256(&json!({
        "schema": "implexity-exact-compile-cache-identity/1",
        "provider_id": provider,
        "profile_id": profile_id,
        "runtime_source_tree_sha256": runtime_sha,
        "fixed_shape_identity_sha256": fixed_shape_sha,
    }))?;
    let policy = json!({
        "schema": "implexity-exact-execution-policy/1",
        "profile_id": profile_id,
        "truth_status": "authoritative_exact",
        "approximate": false,
        "provider_exact_solver_profile": Value::Object(solver),
    });
    let compile_identity = json!({
        "schema": "implexity-exact-compile-identity/1",
        "source_manifest_sha256": source_sha,
        "source_manifest_aggregate_sha256": aggregate_sha,
        "runtime_source_tree_sha256": runtime_sha,
        "provider_id": provider,
        "fixed_shape_identity_sha256": fixed_shape_sha,
        "compile_cache_identity_sha256": compile_cache_sha,
    });
    let mechanisms = json!(mechanism_inventory(accelerated));
    let policy_sha = identity_sha256(&policy)?;
    let compile_sha = identity_sha256(&compile_identity)?;
    let mechanisms_sha = identity_sha256(&mechanisms)?;
    let mut core = Map::new();
    core.insert("profile_id".into(), json!(profile_id));
    core.insert("truth_status".into(), json!("authoritative_exact"));
    core.insert("approximate".into(), Value::Bool(false));
    core.insert("policy".into(), policy);
    core.insert("policy_sha256".into(), json!(policy_sha));
    core.insert("compile_identity".into(), compile_identity);
    core.insert("compile_identity_sha256".into(), json!(compile_sha));
    core.insert("mechanism_inventory".into(), mechanisms);
    core.insert("mechanism_inventory_sha256".into(), json!(mechanisms_sha));
    let profile_sha = identity_sha256(&Value::Object(core.clone()))?;
    core.insert("profile_identity_sha256".into(), json!(profile_sha));
    Ok(Value::Object(core))
}

#[derive(Debug, Clone, Copy)]
pub struct DeriveInputs<'a> {
    pub record_role: &'a str,
    pub run_id: &'a str,
    pub source_manifest_file_sha256: &'a str,
    pub source_manifest_aggregate_sha256: &'a str,
    pub scientific_scope_sha256: &'a str,
    pub provider_id: &'a str,
    pub provider_descriptor: &'a Value,
    pub physics_snapshot: &'a Value,
    pub exact_solver_profile: &'a Value,
    pub frozen_input_identity_sha256: &'a str,
    pub design_state_id: &'a str,
    pub source_root: Option<&'a Path>,
}


pub fn derive_qualification_operation_identity(inputs: &DeriveInputs<'_>) -> IdResult<Value> {
    let runtime_sha = derive_runtime_source_tree_sha256(inputs.source_root)?;
    let provider =
        derive_provider_identity(inputs.provider_id, inputs.provider_descriptor, inputs.physics_snapshot)?;
    let environment = derive_environment_identity()?;
    let science = derive_stage57_science_contract(
        inputs.scientific_scope_sha256,
        inputs.provider_id,
        inputs.frozen_input_identity_sha256,
        inputs.design_state_id,
    )?;
    let science_sha = identity_sha256(&science)?;
    let profile = derive_execution_profile(&ExecutionProfileInputs {
        record_role: inputs.record_role,
        exact_solver_profile: inputs.exact_solver_profile,
        source_manifest_file_sha256: inputs.source_manifest_file_sha256,
        source_manifest_aggregate_sha256: inputs.source_manifest_aggregate_sha256,
        runtime_source_tree_sha256: &runtime_sha,
        provider_id: inputs.provider_id,
        science_contract_sha256: &science_sha,
    })?;
    build_qualification_operation_identity(&BuildInputs {
        record_role: inputs.record_role,
        run_id: inputs.run_id,
        source_manifest_file_sha256: inputs.source_manifest_file_sha256,
        source_manifest_aggregate_sha256: inputs.source_manifest_aggregate_sha256,
        scientific_scope_sha256: inputs.scientific_scope_sha256,
        runtime_source_tree_sha256: &runtime_sha,
        provider_identity: &provider,
        environment_identity: &environment,
        execution_profile: &profile,
        science_contract: &science,
    })
}

fn is_release_candidate_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let tail_ok = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-');
    (8..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes[1..].iter().all(tail_ok)
}

#[derive(Debug, Clone, Copy)]
pub struct SourceBoundInputs<'a> {
    pub record_role: &'a str,
    pub run_id: &'a str,
    pub release_candidate_id: &'a str,
    pub source_manifest_path: &'a Path,
    pub source_manifest_file_sha256: &'a str,
    pub source_manifest_aggregate_sha256: &'a str,
    pub scientific_scope_sha256: &'a str,
    pub frozen_input_root: &'a Path,
    pub provider_id: &'a str,
    pub exact_solver_profile: &'a Value,
}


pub fn derive_source_bound_qualification_operation_identity(
    inputs: &SourceBoundInputs<'_>,
) -> IdResult<Value> {
    if !is_release_candidate_id(inputs.release_candidate_id) {
        return fail("release candidate id is not canonical");
    }
    let Ok(manifest_path) = std::fs::canonicalize(inputs.source_manifest_path) else {
        return fail("source manifest is unavailable");
    };
    let supplied_manifest_sha = digest(inputs.source_manifest_file_sha256, "source manifest file digest")?;
    let supplied_aggregate_sha =
        digest(inputs.source_manifest_aggregate_sha256, "source manifest aggregate")?;
    let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
        return fail("source manifest is unavailable");
    };
    if sha256_hex(&manifest_bytes) != supplied_manifest_sha {
        return fail("source manifest file digest drifted");
    }
    let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
        return fail("source manifest is not canonical JSON");
    };
    if !manifest.is_object()
        || manifest.get("schema").and_then(Value::as_str) != Some("implexity-source-manifest/2")
        || manifest.get("aggregate_sha256").and_then(Value::as_str) != Some(supplied_aggregate_sha)
    {
        return fail("source manifest aggregate identity drifted");
    }
    let Ok(input_root) = std::fs::canonicalize(inputs.frozen_input_root) else {
        return fail("Stage-57 frozen input root is unavailable");
    };
    let spec = input_root.parent().map(|p| p.join("benchmark_spec.py"));
    if !spec.is_some_and(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())) {
        return fail("Stage-57 benchmark specification is unavailable");
    }
    fail("Stage-57 benchmark specification cannot be loaded")
}

#[derive(Debug, Clone, Copy)]
pub struct BuildInputs<'a> {
    pub record_role: &'a str,
    pub run_id: &'a str,
    pub source_manifest_file_sha256: &'a str,
    pub source_manifest_aggregate_sha256: &'a str,
    pub scientific_scope_sha256: &'a str,
    pub runtime_source_tree_sha256: &'a str,
    pub provider_identity: &'a Value,
    pub environment_identity: &'a Value,
    pub execution_profile: &'a Value,
    pub science_contract: &'a Value,
}

fn is_run_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (3..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn strict_int(value: Option<&Value>) -> Option<i64> {
    value.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64)
}

fn profile_is_consistent(
    profile: &Map<String, Value>,
    profile_id: &str,
    provider_id: &str,
    digests: [&str; 3],
) -> IdResult<Option<Vec<String>>> {
    let [source_sha, aggregate_sha, runtime_tree_sha] = digests;
    let policy = profile.get("policy").filter(|v| v.is_object());
    let compile = profile.get("compile_identity").filter(|v| v.is_object());
    let mechanisms = profile.get("mechanism_inventory").and_then(Value::as_array);
    let (Some(policy), Some(compile), Some(mechanisms)) = (policy, compile, mechanisms) else {
        return Ok(None);
    };
    let Some(names) = mechanisms.iter().map(|m| m.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let mut sorted_unique = names.clone();
    sorted_unique.sort();
    sorted_unique.dedup();
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let ok = profile.get("profile_id").and_then(Value::as_str) == Some(profile_id)
        && profile.get("truth_status").and_then(Value::as_str) == Some("authoritative_exact")
        && profile.get("approximate") == Some(&Value::Bool(false))
        && policy.get("profile_id").and_then(Value::as_str) == Some(profile_id)
        && text(&Value::Object(profile.clone()), "policy_sha256") == Some(identity_sha256(policy)?)
        && text(compile, "source_manifest_sha256").as_deref() == Some(source_sha)
        && text(compile, "source_manifest_aggregate_sha256").as_deref() == Some(aggregate_sha)
        && text(compile, "runtime_source_tree_sha256").as_deref() == Some(runtime_tree_sha)
        && text(compile, "provider_id").as_deref() == Some(provider_id)
        && profile.get("compile_identity_sha256").and_then(Value::as_str)
            == Some(identity_sha256(compile)?.as_str())
        && names == sorted_unique
        && profile.get("mechanism_inventory_sha256").and_then(Value::as_str)
            == Some(identity_sha256(&Value::Array(mechanisms.clone()))?.as_str());
    Ok(ok.then_some(names))
}


#[allow(clippy::too_many_lines)]
pub fn build_qualification_operation_identity(inputs: &BuildInputs<'_>) -> IdResult<Value> {
    let Some((operation_kind, profile_id, accelerated)) = role(inputs.record_role) else {
        return fail("qualification record role is not closed");
    };
    if !is_run_id(inputs.run_id) {
        return fail("qualification run id is not canonical");
    }
    let source_sha = digest(inputs.source_manifest_file_sha256, "source manifest file digest")?;
    let aggregate_sha = digest(inputs.source_manifest_aggregate_sha256, "source manifest aggregate")?;
    let scope_sha = digest(inputs.scientific_scope_sha256, "scientific scope")?;
    let runtime_tree_sha = digest(inputs.runtime_source_tree_sha256, "runtime source tree")?;
    let provider = mapping(inputs.provider_identity, "provider identity")?;
    let environment = mapping(inputs.environment_identity, "environment identity")?;
    let profile = mapping(inputs.execution_profile, "execution profile")?;
    let science = mapping(inputs.science_contract, "science contract")?;

    let descriptor = provider.get("provider_descriptor").filter(|d| d.is_object());
    let descriptor_ok = match descriptor {
        Some(d) => {
            provider.get("provider_descriptor_sha256").and_then(Value::as_str)
                == Some(identity_sha256(d)?.as_str())
        }
        None => false,
    };
    if !descriptor_ok {
        return fail("provider descriptor identity drifted");
    }
    let provider_id = provider.get("provider_id").and_then(Value::as_str).unwrap_or_default().to_string();
    if provider_id.is_empty()
        || descriptor.and_then(|d| d.get("name")).and_then(Value::as_str) != Some(&provider_id)
    {
        return fail("provider identity is incomplete");
    }
    let provider_value = Value::Object(provider);
    let provider_sha = identity_sha256(&provider_value)?;

    let hardware = environment.get("hardware").and_then(Value::as_object);
    let hardware_cpus = hardware.and_then(|h| strict_int(h.get("logical_cpu_count")));
    if environment.get("schema").and_then(Value::as_str) != Some("implexity-exact-benchmark-environment/1")
        || environment.get("compatibility_policy").and_then(Value::as_str)
            != Some("exact_identity_required_for_performance_activation")
        || hardware_cpus.is_none_or(|c| c < 1)
    {
        return fail("benchmark environment identity is incomplete");
    }
    let hardware_cpus = hardware_cpus.unwrap_or_default();
    let environment_value = Value::Object(environment);
    let environment_sha = identity_sha256(&environment_value)?;

    let Some(mechanisms) = profile_is_consistent(
        &profile,
        profile_id,
        &provider_id,
        [source_sha, aggregate_sha, runtime_tree_sha],
    )?
    else {
        return fail("execution profile identity is incomplete");
    };
    let allowed =
        |m: &String| ACCELERATED_REQUIRED.contains(&m.as_str()) || ACCELERATED_OPTIONAL.contains(&m.as_str());
    if accelerated {
        let complete = ACCELERATED_REQUIRED.iter().all(|r| mechanisms.iter().any(|m| m == r));
        if !complete || !mechanisms.iter().all(allowed) {
            return fail("accelerated mechanism inventory is incomplete");
        }
    } else if mechanisms != DIRECT_MECHANISMS {
        return fail("direct mechanism inventory differs");
    }
    let mut profile_core = profile.clone();
    profile_core.remove("profile_identity_sha256");
    let profile_sha = identity_sha256(&Value::Object(profile_core))?;
    if profile.get("profile_identity_sha256").and_then(Value::as_str) != Some(profile_sha.as_str()) {
        return fail("execution profile digest drifted");
    }

    if science.get("schema").and_then(Value::as_str) != Some("implexity-stage57-exact-science-contract/1")
        || science.get("scientific_scope_sha256").and_then(Value::as_str) != Some(scope_sha)
        || science.get("provider_id").and_then(Value::as_str) != Some(provider_id.as_str())
    {
        return fail("science contract identity is incomplete");
    }
    let science_value = Value::Object(science);
    let science_sha = identity_sha256(&science_value)?;

    let runtime_policy = if accelerated { BOUNDED_HOST_RUNTIME_POLICY } else { SERIAL_RUNTIME_POLICY };
    let runtime = create_worker_runtime_profile_issuer()
        .issue(runtime_policy)
        .map_err(|e| QualificationOperationIdentityError(e.to_string()))?;
    if runtime.logical_cpu_count() != hardware_cpus {
        return fail("environment and selected runtime CPU identities differ");
    }
    Ok(json!({
        "schema": IDENTITY_EXPECTATION_SCHEMA,
        "record_role": inputs.record_role,
        "operation_kind": operation_kind,
        "run_id": inputs.run_id,
        "profile_id": profile_id,
        "runtime_source_tree_sha256": runtime_tree_sha,
        "source_manifest_file_sha256": source_sha,
        "source_manifest_aggregate_sha256": aggregate_sha,
        "scientific_scope_sha256": scope_sha,
        "provider_identity_sha256": provider_sha,
        "environment_identity_sha256": environment_sha,
        "execution_profile_identity_sha256": profile_sha,
        "science_contract_sha256": science_sha,
        "runtime_profile_policy": runtime.policy(),
        "runtime_profile_sha256": runtime.sha256(),
        "logical_cpu_count": runtime.logical_cpu_count(),
        "worker_thread_limit": runtime.worker_thread_limit(),
        "logical_batch_concurrency_limit": runtime.logical_batch_concurrency_limit(),
        "bounded_worker_threads_active": accelerated,
        "worker_preimport_bootstrap_schema": PREIMPORT_BOOTSTRAP_SCHEMA,
        "worker_preimport_validated_profile_sha256": runtime.sha256(),
        "numerical_modules_loaded_before_profile_validation": [],
        "fresh_process": true,
        "fresh_cache": true,
        "cross_record_state_reused": false,
        "matching_time_guess_count": 0,
        "identity_documents": {
            "provider_identity": provider_value,
            "environment_identity": environment_value,
            "execution_profile": inputs.execution_profile,
            "science_contract": science_value,
        },
        "canonical_promotion_authorized": false,
    }))
}


pub fn trace_fields(
    expectation: &Value,
    operation_session_id: &str,
    cache_instance_id: &str,
) -> IdResult<Map<String, Value>> {
    let Value::Object(map) = expectation else {
        return fail("qualification operation expectation is absent");
    };
    let omitted = [
        "schema",
        "record_role",
        "runtime_profile_sha256",
        "identity_documents",
        "canonical_promotion_authorized",
    ];
    let mut result: Map<String, Value> = map
        .iter()
        .filter(|(k, _)| !omitted.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    result.insert("operation_session_id".into(), json!(operation_session_id));
    result.insert("cache_instance_id".into(), json!(cache_instance_id));
    result.insert("phase".into(), json!("point"));
    Ok(result)
}

