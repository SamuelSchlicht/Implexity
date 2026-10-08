// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::io::Write as _;
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{JobError, JobResult};
use crate::heavy_runtime::{CooperativeOutcome, CooperativeRuntime, SafePointError, cooperative_runtime};
use crate::private::{
    canonical_text, create_exclusive, fsync_dir, sha256_file, sha256_hex, token_hex, write_all_sync,
};
use crate::provider_worker::{emit_line, error_payload, execute};

pub const RESULT_FILE: &str = "result.json";
pub const RESULT_SCHEMA: &str = "implexity-private-managed-provider-result/1";
pub const EXIT_CANCELLED: i32 = 75;
const MAX_REQUEST_BYTES: u64 = 64 * 1024 * 1024;


pub fn atomic_result_json(path: &Path, payload: &Value) -> std::io::Result<()> {
    let mut encoded = canonical_text(payload).into_bytes();
    encoded.push(b'\n');
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".{name}.{}", token_hex(12)?));
    let mut file = create_exclusive(&temporary, 0o600)?;
    write_all_sync(&mut file, &encoded)?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    let _ = fsync_dir(parent);
    Ok(())
}

#[derive(Debug)]
enum BodyStop {
    Safe(SafePointError),
    Failed(JobError),
}

impl From<SafePointError> for BodyStop {
    fn from(e: SafePointError) -> Self {
        Self::Safe(e)
    }
}

impl From<JobError> for BodyStop {
    fn from(e: JobError) -> Self {
        Self::Failed(e)
    }
}

fn uncaught(error: &JobError) -> i32 {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}", error.describe());
    1
}

fn usage(message: &str) -> i32 {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "error: {message}");
    2
}

#[derive(Debug, Default)]
struct Args {
    values: std::collections::BTreeMap<String, String>,
    flags: std::collections::BTreeSet<String>,
}

fn parse_args(args: &[String], valued: &[&str], flags: &[&str]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let name = arg.strip_prefix("--").unwrap_or("");
        if valued.contains(&name) {
            let Some(value) = it.next() else {
                return Err(format!("argument {arg}: expected one argument"));
            };
            out.values.insert(name.to_string(), value.clone());
        } else if flags.contains(&name) {
            out.flags.insert(name.to_string());
        } else {
            return Err(format!("unrecognized arguments: {arg}"));
        }
    }
    Ok(out)
}

fn require(args: &Args, names: &[&str]) -> Result<(), String> {
    let missing: Vec<String> =
        names.iter().filter(|n| !args.values.contains_key(**n)).map(|n| format!("--{n}")).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("the following arguments are required: {}", missing.join(", ")))
    }
}

fn publish(
    runtime: &CooperativeRuntime,
    mode: &str,
    request_sha256: &str,
    result: Map<String, Value>,
) -> JobResult<()> {
    let mut payload = Map::new();
    payload.insert("schema".into(), Value::String(RESULT_SCHEMA.into()));
    payload.insert("mode".into(), Value::String(mode.into()));
    payload.insert("request_sha256".into(), Value::String(request_sha256.into()));
    payload.insert("result".into(), Value::Object(result));
    atomic_result_json(&runtime.partial_directory.join(RESULT_FILE), &Value::Object(payload))?;
    Ok(())
}

fn finish(
    outcome: Result<CooperativeOutcome<(), BodyStop>, crate::heavy_runtime::HeavyRuntimeContractError>,
) -> i32 {
    match outcome {
        Ok(CooperativeOutcome::Completed(())) => 0,
        Ok(
            CooperativeOutcome::Cancelled
            | CooperativeOutcome::Failed(BodyStop::Safe(SafePointError::Cancelled(_))),
        ) => EXIT_CANCELLED,
        Ok(CooperativeOutcome::Failed(BodyStop::Safe(SafePointError::Contract(e)))) | Err(e) => {
            uncaught(&JobError::from(e))
        }
        Ok(CooperativeOutcome::Failed(BodyStop::Failed(e))) => uncaught(&e),
    }
}

fn artifact_root(runtime: &CooperativeRuntime, enabled: bool) -> Value {
    if enabled {
        Value::String(runtime.partial_directory.join("artifacts").to_string_lossy().into_owned())
    } else {
        Value::Null
    }
}

#[must_use]
pub fn managed_provider_worker(args: &[String]) -> i32 {
    let parsed = match parse_args(args, &["mode", "request-file", "request-sha256"], &["artifact-output"])
        .and_then(|a| require(&a, &["mode", "request-file", "request-sha256"]).map(|()| a))
    {
        Ok(a) => a,
        Err(m) => return usage(&m),
    };
    let mode = parsed.values["mode"].clone();
    if !matches!(mode.as_str(), "preflight" | "evaluate" | "sensitivity") {
        return usage(&format!("argument --mode: invalid choice: '{mode}'"));
    }
    let request_path = Path::new(&parsed.values["request-file"]).to_path_buf();
    let request_sha256 = parsed.values["request-sha256"].clone();
    let artifact_output = parsed.flags.contains("artifact-output");
    match sha256_file(&request_path) {
        Ok(actual) if actual == request_sha256 && request_sha256.len() == 64 => {}
        Ok(_) => return uncaught(&JobError::runtime("managed provider request identity drifted")),
        Err(e) => return uncaught(&JobError::from(e)),
    }
    finish(cooperative_runtime(|runtime| -> Result<(), BodyStop> {
        let text = std::fs::read_to_string(&request_path).map_err(JobError::from)?;
        let mut request: Map<String, Value> = match serde_json::from_str(&text) {
            Ok(Value::Object(m)) => m,
            Ok(_) => return Err(JobError::runtime("managed provider request is not an object").into()),
            Err(e) => return Err(JobError::of("JSONDecodeError", e.to_string()).into()),
        };
        request.insert("artifact_root".into(), artifact_root(runtime, artifact_output));
        runtime.checkpoint("running")?;
        let result = execute(&Value::Object(request), &mode)?;
        runtime.checkpoint("finalizing")?;
        publish(runtime, &mode, &request_sha256, result)?;
        Ok(())
    }))
}

fn read_regular(path: &Path, label: &str) -> JobResult<Vec<u8>> {
    implexity_runtime::worker_preimport_bootstrap::read_regular_bytes(path, label, MAX_REQUEST_BYTES)
        .map_err(|e| JobError::runtime(e.to_string()))
}

#[must_use]
pub fn accelerated_managed_provider_worker(args: &[String]) -> i32 {
    let valued = [
        "mode",
        "request-file",
        "request-sha256",
        "production-authority-fd",
        "production-nonce-sha256",
        "production-operation-class",
        "source-manifest-sha256",
        "implementation-closure-sha256",
    ];
    let parsed = match parse_args(args, &valued, &["artifact-output"])
        .and_then(|a| require(&a, &valued).map(|()| a))
    {
        Ok(a) => a,
        Err(m) => return usage(&m),
    };
    let mode = parsed.values["mode"].clone();
    if !matches!(mode.as_str(), "evaluate" | "sensitivity") {
        return usage(&format!("argument --mode: invalid choice: '{mode}'"));
    }
    if parsed.values["production-authority-fd"].parse::<i64>().is_err() {
        return usage("argument --production-authority-fd: invalid int value");
    }
    let request_sha256 = parsed.values["request-sha256"].clone();
    let artifact_output = parsed.flags.contains("artifact-output");
    let request_bytes =
        match read_regular(Path::new(&parsed.values["request-file"]), "managed production request") {
            Ok(b) => b,
            Err(e) => return uncaught(&e),
        };
    if sha256_hex(&request_bytes) != request_sha256
        || request_sha256.len() != 64
        || parsed.values["production-operation-class"] != mode
    {
        return uncaught(&JobError::runtime("managed production request identity drifted"));
    }
    finish(cooperative_runtime(|runtime| -> Result<(), BodyStop> {
        let mut request =
            implexity_runtime::exact_acceleration_production_authority::parse_strict_production_json(
                &request_bytes,
                "managed production request",
            )
            .map_err(JobError::from)?;
        request.insert("artifact_root".into(), artifact_root(runtime, artifact_output));
        if let Some(snapshot) = request.get("runtime_packages").filter(|v| !v.is_null()) {
            implexity_core::packages::global().activate_snapshot_value(snapshot).map_err(JobError::from)?;
        }
        let binding = request.get("computation_effort").cloned().unwrap_or(Value::Null);
        implexity_runtime::exact_acceleration_production_authority::build_production_child_execution(
            &binding,
        )
        .map_err(JobError::from)?;
        runtime.checkpoint("running")?;
        let result = execute(&Value::Object(request), &mode)?;
        runtime.checkpoint("finalizing")?;
        publish(runtime, &mode, &request_sha256, result)?;
        Ok(())
    }))
}


pub fn read_bound_request(path: &Path, expected_sha256: &str) -> JobResult<Map<String, Value>> {
    if !crate::private::is_sha256(expected_sha256) {
        return Err(JobError::runtime("managed qualification request digest is malformed"));
    }
    let mut file = crate::private::open_nofollow(path)?;
    let before = implexity_io::fsguard::stat_file(&file)?;
    if !before.is_file() || before.nlink != 1 || before.size < 2 || before.size > MAX_REQUEST_BYTES {
        return Err(JobError::runtime("managed qualification request file is unsafe"));
    }
    let raw = crate::private::read_limited(&mut file, MAX_REQUEST_BYTES)?;
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_REQUEST_BYTES {
        return Err(JobError::runtime("managed qualification request exceeds its byte bound"));
    }
    let after = implexity_io::fsguard::stat_file(&file)?;

    if before.data_identity() != after.data_identity()
        || before.ctime != after.ctime
        || u64::try_from(raw.len()).ok() != Some(before.size)
    {
        return Err(JobError::runtime("managed qualification request changed while acquired"));
    }
    if sha256_hex(&raw) != expected_sha256 {
        return Err(JobError::runtime("managed qualification request identity drifted"));
    }
    match implexity_core::json::parse_strict_bytes(&raw) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(JobError::runtime("managed qualification request is not an object")),
        Err(e) if e.message.to_ascii_lowercase().contains("duplicate") => {
            Err(JobError::runtime("managed qualification request contains a duplicate key"))
        }
        Err(e) if e.message.starts_with("non-finite number ") => {
            let constant =
                e.message.trim_start_matches("non-finite number ").trim_end_matches(" is not admissible");
            Err(JobError::runtime(format!(
                "managed qualification request contains nonfinite number {constant}"
            )))
        }
        Err(e) if e.message.contains("overflow") => {
            Err(JobError::runtime("managed qualification request contains a nonfinite number"))
        }
        Err(_) => Err(JobError::runtime("managed qualification request is not strict UTF-8 JSON")),
    }
}

#[must_use]
pub fn qualification_managed_provider_worker(args: &[String]) -> i32 {
    let valued = [
        "mode",
        "request-file",
        "request-sha256",
        "qualification-authority-fd",
        "qualification-nonce-sha256",
        "record-role",
        "release-candidate-id",
        "source-manifest",
        "source-manifest-sha256",
        "prefreeze-closure",
        "prefreeze-closure-sha256",
    ];
    let parsed = match parse_args(args, &valued, &["artifact-output"])
        .and_then(|a| require(&a, &valued).map(|()| a))
    {
        Ok(a) => a,
        Err(m) => return usage(&m),
    };
    let mode = parsed.values["mode"].clone();
    if !matches!(mode.as_str(), "evaluate" | "sensitivity") {
        return usage(&format!("argument --mode: invalid choice: '{mode}'"));
    }
    if parsed.values["qualification-authority-fd"].parse::<i64>().is_err() {
        return usage("argument --qualification-authority-fd: invalid int value");
    }
    let request_sha256 = parsed.values["request-sha256"].clone();
    let artifact_output = parsed.flags.contains("artifact-output");
    let request = match read_bound_request(Path::new(&parsed.values["request-file"]), &request_sha256) {
        Ok(r) => r,
        Err(e) => return uncaught(&e),
    };
    finish(cooperative_runtime(|runtime| -> Result<(), BodyStop> {
        let mut request = request;
        request.insert("artifact_root".into(), artifact_root(runtime, artifact_output));
        implexity_runtime::qualification_benchmark_authority::build_qualification_child_execution()
            .map_err(JobError::from)?;
        runtime.checkpoint("running")?;
        let result = execute(&Value::Object(request), &mode)?;
        runtime.checkpoint("finalizing")?;
        publish(runtime, &mode, &request_sha256, result)?;
        Ok(())
    }))
}

#[must_use]
pub fn accelerated_provider_job(args: &[String]) -> i32 {
    let valued = [
        "spec-file",
        "job-dir",
        "production-authority-fd",
        "production-nonce-sha256",
        "production-operation-class",
        "source-manifest-sha256",
        "implementation-closure-sha256",
    ];
    let parsed = match parse_args(args, &valued, &["resume"]).and_then(|a| require(&a, &valued).map(|()| a)) {
        Ok(a) => a,
        Err(m) => return usage(&m),
    };
    if parsed.values["production-authority-fd"].parse::<i64>().is_err() {
        return usage("argument --production-authority-fd: invalid int value");
    }
    let outcome = (|| -> JobResult<()> {
        if parsed.values["production-operation-class"] != "optimize" {
            return Err(JobError::runtime("production optimisation operation drifted"));
        }
        let spec_path = Path::new(&parsed.values["spec-file"]);
        let spec_bytes = read_regular(spec_path, "managed production optimisation specification")?;
        let request_sha256 = sha256_hex(&spec_bytes);
        let spec = implexity_runtime::exact_acceleration_production_authority::parse_strict_production_json(
            &spec_bytes,
            "managed production optimisation specification",
        )?;
        if let Some(snapshot) = spec.get("runtime_packages").filter(|v| !v.is_null()) {
            implexity_core::packages::global().activate_snapshot_value(snapshot)?;
        }
        let binding = spec.get("computation_effort").cloned().unwrap_or(Value::Null);
        implexity_runtime::exact_acceleration_production_authority::build_production_child_execution(
            &binding,
        )?;
        crate::provider_job::run_expected(
            spec_path,
            Path::new(&parsed.values["job-dir"]),
            parsed.flags.contains("resume"),
            Some(&request_sha256),
        )?;
        Ok(())
    })();
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            emit_line("ERROR ", &error_payload(&e));
            2
        }
    }
}

