// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::{
    Framework, MAX_MESSAGE, MAX_PLAN, McpClient, RResult, RunnerError, SESSION_SCHEMA, TERMINAL,
    canonical_hash, check_application_case, check_source, child_env, detach, effective_plan, encoded,
    framework_root, free_port, origin, read_json, rerr, sha256_file, source_manifest, terminate_process,
    validate_plan, write_json,
};
use crate::args::{Exit, Kind, Parser};
use crate::util::utc_iso;

const QUALIFICATION: &str =
    "Environment and bounded static dependency audit only; not production or physical validation.";

pub(crate) fn doctor(fw: &Framework) -> RResult<Value> {
    let out = Command::new(&fw.service)
        .arg("--version")
        .env_clear()
        .envs(child_env(None))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| RunnerError::Runner(format!("The selected service executable cannot run: {e}")))?;
    if !out.status.success() {
        let tail = String::from_utf8_lossy(&out.stderr);
        return rerr(format!(
            "The selected service executable cannot run the version probe: {}",
            tail.chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>()
        ));
    }
    let service_version = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let client_version = format!("Implexity {}", implexity_core::PYTHON_COMPATIBILITY_VERSION);
    let system = implexity_core::runtime_environment::platform_system();
    let sha = |p: &Path| sha256_file(p).map_or(Value::Null, Value::from);
    Ok(json!({
        "schema": "implexity-local-mcp-doctor/1", "utc": utc_iso(), "framework": fw.root.display().to_string(),
        "implementation": "rust", "rust_version": implexity_core::RUST_VERSION,
        "platform": format!("{system}-{}", implexity_core::runtime_environment::platform_machine()),
        "system": system,
        "managed_local_execution_supported": matches!(system, "Linux" | "Darwin"),
        "executables": {
            "service": {"path": fw.service.display().to_string(), "sha256": sha(&fw.service)},
            "mcp_bridge": {"path": fw.bridge.display().to_string(), "sha256": sha(&fw.bridge)},
        },
        "service_version": service_version, "client_version": client_version,
        "version_matches_client": service_version == client_version,
        "numerical_libraries": "compiled into the service executable and pinned by its Cargo.lock; nothing is imported at run time",
        "architecture": {"status": "enforced_at_build",
            "detail": "crate layering, the unsafe policy and licence headers are checked by `cargo xtask audit` before a build is released; a compiled executable carries no source tree to re-audit"},
        "qualification": QUALIFICATION,
    }))
}

fn doctor_ok(report: &Value) -> bool {
    report["managed_local_execution_supported"] != json!(false)
        && report["version_matches_client"] == json!(true)
        && matches!(
            report["architecture"]["status"].as_str(),
            Some("passed" | "passed_with_explicit_legacy_debt" | "enforced_at_build")
        )
}

fn require_runtime(report: &Value, allow_unqualified: bool) -> RResult<()> {
    if report["managed_local_execution_supported"] == json!(false) {
        return rerr(
            "The current birth-bound managed process supervisor supports Linux and macOS, not native Windows. Use a Linux execution environment; --allow-unqualified-runtime cannot bypass missing process ownership/safety support. See doctor.json.",
        );
    }
    if !matches!(
        report["architecture"]["status"].as_str(),
        Some("passed" | "passed_with_explicit_legacy_debt" | "enforced_at_build")
    ) {
        return rerr("Architecture audit failed/unavailable; review doctor.json before running.");
    }
    if report["version_matches_client"] != json!(true) && !allow_unqualified {
        return rerr(
            "The service executable's version differs from this client's. Use the matching build or explicitly pass --allow-unqualified-runtime. Nothing is changed automatically.",
        );
    }
    Ok(())
}

struct LocalService {
    fw: Framework,
    output: PathBuf,
    startup_timeout: f64,
    child: Option<Child>,
    url: String,
}

impl LocalService {
    fn new(fw: &Framework, output: &Path, startup_timeout: f64) -> RResult<Self> {
        Ok(Self {
            fw: fw.clone(),
            output: output.to_path_buf(),
            startup_timeout,
            child: None,
            url: format!("http://127.0.0.1:{}", free_port()?),
        })
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    fn start(&mut self) -> RResult<()> {
        let case = self.output.join("case");
        std::fs::create_dir(&case)?;
        let case = std::fs::canonicalize(&case).unwrap_or(case);
        let (_, host, port) = origin(&self.url)?;
        let args: Vec<String> = [
            "serve",
            "--host",
            &host,
            "--port",
            &port.to_string(),
            "--backend",
            "synthetic",
            "--workers",
            "1",
            "--cache-mb",
            "64",
            "--design-grid",
            "2",
            "2",
            "2",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let log =
            std::fs::OpenOptions::new().write(true).create_new(true).open(self.output.join("service.log"))?;
        let err = log.try_clone()?;
        let mut cmd = Command::new(&self.fw.service);
        cmd.args(&args)
            .env_clear()
            .envs(child_env(Some(&case)))
            .current_dir(&case)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        detach(&mut cmd);
        let child =
            cmd.spawn().map_err(|e| RunnerError::Io(format!("{}: {e}", self.fw.service.display())))?;
        let mut command = vec![self.fw.service.display().to_string()];
        command.extend(args);
        write_json(
            &self.output.join("service_process.json"),
            &json!({"pid": child.id(), "command": command, "base_url": self.url, "case_dir": case.display().to_string(), "utc": utc_iso()}),
            false,
        )?;
        self.child = Some(child);
        let started = Instant::now();
        let budget = super::seconds(self.startup_timeout);
        let addr: std::net::SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|_| RunnerError::Runner("bad service address".into()))?;
        while started.elapsed() < budget {
            if let Some(c) = self.child.as_mut()
                && let Ok(Some(status)) = c.try_wait()
            {
                return rerr(format!(
                    "Service exited with {}; see service.log.",
                    status.code().unwrap_or(-1)
                ));
            }
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
                std::thread::sleep(Duration::from_millis(100));
                if let Some(c) = self.child.as_mut()
                    && matches!(c.try_wait(), Ok(Some(_)))
                {
                    return rerr("Owned service did not bind its port; no existing service will be reused.");
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        rerr("Service did not start within the budget; see service.log.")
    }

    fn close(&mut self) {
        if let Some(mut c) = self.child.take() {
            terminate_process(&mut c, Duration::from_secs(5));
        }
    }

    fn release(&mut self) {
        self.child = None;
    }
}

fn job_iterations(job: &Value) -> RResult<i64> {
    match job.get("iterations") {
        None => Ok(0),
        Some(v) if v.is_i64() && v.as_i64().is_some_and(|n| n >= 0) => Ok(v.as_i64().unwrap_or(0)),
        _ => rerr("Invalid public job iteration count."),
    }
}

fn job_id_ok(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn job_id_from(result: &Value) -> RResult<String> {
    let ident = result.get("job_id").or_else(|| result.get("id"));
    match ident.and_then(Value::as_str) {
        Some(s) if job_id_ok(s) => Ok(s.to_owned()),
        _ => rerr("Public action did not return a valid job identity."),
    }
}

fn assert_job_id(job: &Value, ident: &str) -> RResult<()> {
    if job_id_from(job)? != ident {
        return rerr("Response belongs to a different optimization job.");
    }
    Ok(())
}

fn model_id(state: &Value) -> RResult<String> {
    match state.get("model").and_then(|m| m.get("content_id")).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s.to_owned()),
        _ => rerr("No authoritative model identity in inspect_state."),
    }
}

fn objective_origin(job: &Value) -> (Value, &'static str) {
    if let Some(first) =
        job.get("history").and_then(Value::as_array).and_then(|h| h.first()).filter(|r| r.is_object())
        && let Some(v) =
            first.get("L_first").filter(|v| v.as_f64().is_some_and(f64::is_finite) && !v.is_boolean())
    {
        return (v.clone(), "history[0].L_first");
    }
    (
        job.get("L_first").cloned().unwrap_or(Value::Null),
        "job.L_first (no committed initial record available)",
    )
}

fn py(v: Option<&Value>) -> String {
    v.map_or_else(|| "None".to_owned(), implexity_core::pyobj::py_str)
}

fn residual(n: &Value) -> Option<&Value> {
    n.get("residual_norm").or_else(|| n.get("residual_l2")).or_else(|| n.get("residual"))
}

fn print_progress(job: &Value) {
    let n = job
        .get("numerical_progress")
        .filter(|x| implexity_core::pyobj::truthy(x))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let summary =
        job.get("summary").filter(|x| implexity_core::pyobj::truthy(x)).cloned().unwrap_or_else(|| json!({}));
    let s = |v: &Value, k: &str| v.get(k).map_or_else(String::new, implexity_core::pyobj::py_str);
    println!(
        "[{}] {} | updates={} | L={} | {}/{} role={} residual={} {}",
        utc_iso(),
        py(job.get("status")),
        job_iterations(job).unwrap_or(0),
        py(job.get("L_last")),
        s(&n, "event"),
        s(&n, "phase"),
        s(&n, "evaluation_role"),
        py(residual(&n)),
        s(&summary, "termination_reason")
    );
}

fn csv_field(v: &Value) -> String {
    let text = match v {
        Value::Null => String::new(),
        other => implexity_core::pyobj::py_str(other),
    };
    if text.contains([',', '"', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text }
}

fn save_status(output: &Path, job: &Value) -> RResult<()> {
    write_json(&output.join("job_latest.json"), job, true)?;
    let n = job
        .get("numerical_progress")
        .filter(|x| implexity_core::pyobj::truthy(x))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let g = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let row: Vec<(&str, Value)> = vec![
        ("observed_utc", json!(utc_iso())),
        ("status", g(job, "status")),
        ("updates", json!(job_iterations(job)?)),
        ("objective_first", objective_origin(job).0),
        ("reported_summary_objective_first", g(job, "L_first")),
        ("objective_last", g(job, "L_last")),
        ("objective_best", g(job, "L_best")),
        ("elapsed_s", g(job, "elapsed_s")),
        ("event", g(&n, "event")),
        ("phase", g(&n, "phase")),
        ("evaluation_role", g(&n, "evaluation_role")),
        ("residual", residual(&n).cloned().unwrap_or(Value::Null)),
    ];
    let path = output.join("progress.csv");
    let mut text = String::new();
    if !path.exists() {
        text.push_str(&row.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(","));
        text.push_str("\r\n");
    }
    text.push_str(&row.iter().map(|(_, v)| csv_field(v)).collect::<Vec<_>>().join(","));
    text.push_str("\r\n");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    std::io::Write::flush(&mut f)?;
    Ok(())
}

const ACTIVE: [&str; 7] = ["queued", "running", "paused", "pausing", "resuming", "stopping", "intervening"];

fn wait_job(
    client: &mut McpClient,
    ident: &str,
    output: &Path,
    poll: f64,
    timeout: f64,
) -> RResult<(Value, &'static str)> {
    let started = Instant::now();
    loop {
        let (job, _) =
            client.action("inspect_optimization_job", &json!({"job_id": ident, "view": "monitor"}))?;
        assert_job_id(&job, ident)?;
        save_status(output, &job)?;
        print_progress(&job);
        let status = job.get("status").and_then(Value::as_str).unwrap_or_default().to_owned();
        if TERMINAL.contains(&status.as_str()) {
            return Ok((job, "terminal"));
        }
        if job.get("numerical_attention").is_some_and(implexity_core::pyobj::truthy) {
            return Ok((job, "numerical_attention_requires_review"));
        }
        if timeout > 0.0 && started.elapsed().as_secs_f64() >= timeout {
            return Ok((job, "client_wall_budget"));
        }
        if !ACTIVE.contains(&status.as_str()) {
            return rerr(format!(
                "Unknown active job state {}; no automated mutation.",
                implexity_core::pyobj::repr(job.get("status").unwrap_or(&Value::Null))
            ));
        }
        std::thread::sleep(super::seconds(poll));
    }
}

fn stop_job(client: &mut McpClient, ident: &str, output: &Path) -> RResult<Value> {
    let (mut job, _) =
        client.action("inspect_optimization_job", &json!({"job_id": ident, "view": "monitor"}))?;
    assert_job_id(&job, ident)?;
    if TERMINAL.contains(&job.get("status").and_then(Value::as_str).unwrap_or_default()) {
        return Ok(job);
    }
    client.action("optimization_operation", &json!({"job_id": ident, "op": "stop", "view": "monitor"}))?;
    let deadline = Instant::now() + Duration::from_mins(1);
    while Instant::now() < deadline {
        let (j, _) =
            client.action("inspect_optimization_job", &json!({"job_id": ident, "view": "monitor"}))?;
        assert_job_id(&j, ident)?;
        save_status(output, &j)?;
        job = j;
        if TERMINAL.contains(&job.get("status").and_then(Value::as_str).unwrap_or_default()) {
            return Ok(job);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    rerr("Public stop has not reached terminal state; service must remain running for inspection.")
}

fn export_epochs(client: &mut McpClient, ident: &str, job: &Value, output: &Path) -> RResult<Vec<Value>> {
    let count = job_iterations(job)?;
    let dir = output.join("epochs");
    std::fs::create_dir_all(&dir)?;
    let mut epochs = Vec::new();
    for epoch in 0..count {
        let (result, _) =
            client.action("export_optimization_epoch", &json!({"job_id": ident, "epoch": epoch}))?;
        if result.get("job_id").and_then(Value::as_str) != Some(ident)
            || result.get("epoch").and_then(Value::as_i64) != Some(epoch)
            || result.get("live_model_mutated") != Some(&json!(false))
        {
            return rerr("Epoch export identity/read-only guard failed.");
        }
        if !result.get("document").is_some_and(Value::is_object) {
            return rerr("Epoch export has no actual model document.");
        }
        let target = dir.join(format!("epoch_{epoch:06}.json"));
        if target.exists() {
            if read_json(&target, u64::try_from(MAX_MESSAGE).unwrap_or(u64::MAX))? != result {
                return rerr("Saved epoch content changed; refusing overwrite.");
            }
        } else {
            write_json(&target, &result, false)?;
        }
        let pick: Map<String, Value> =
            ["epoch", "iteration", "content_id", "design_state_id", "checkpoint", "canonical_eligible"]
                .iter()
                .map(|k| ((*k).to_owned(), result.get(*k).cloned().unwrap_or(Value::Null)))
                .collect();
        epochs.push(Value::Object(pick));
    }
    Ok(epochs)
}

fn decode_image(result: &Value, wire: &Value, expected: &str) -> RResult<Vec<u8>> {
    use base64::Engine as _;
    if result.get("source").and_then(|s| s.get("content_id")).and_then(Value::as_str) != Some(expected) {
        return rerr("Native image belongs to a different model state.");
    }
    let images: Vec<&Value> = wire
        .get("content")
        .and_then(Value::as_array)
        .map(|c| c.iter().filter(|x| x.get("type") == Some(&json!("image"))).collect())
        .unwrap_or_default();
    let meta = result.get("image").cloned().unwrap_or_else(|| json!({}));
    if images.len() != 1 || images[0].get("mimeType").and_then(Value::as_str) != Some("image/png") {
        return rerr("Native renderer did not return exactly one actual PNG.");
    }
    let raw = images[0]
        .get("data")
        .and_then(Value::as_str)
        .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
        .ok_or_else(|| RunnerError::Runner("Malformed PNG encoding.".into()))?;
    let bytes_ok = meta.get("bytes").and_then(Value::as_u64) == u64::try_from(raw.len()).ok();
    let sha_ok =
        meta.get("sha256").and_then(Value::as_str) == Some(implexity_io::digest::sha256_hex(&raw).as_str());
    if raw.len() > 8 * 1024 * 1024
        || raw.len() < 24
        || &raw[..8] != b"\x89PNG\r\n\x1a\n"
        || !bytes_ok
        || !sha_ok
    {
        return rerr("Native PNG size/hash/signature mismatch.");
    }
    let w = u32::from_be_bytes([raw[16], raw[17], raw[18], raw[19]]);
    let h = u32::from_be_bytes([raw[20], raw[21], raw[22], raw[23]]);
    if meta.get("width_px").and_then(Value::as_u64) != Some(u64::from(w))
        || meta.get("height_px").and_then(Value::as_u64) != Some(u64::from(h))
    {
        return rerr("Native PNG dimensions mismatch.");
    }
    Ok(raw)
}

fn render_scenes(
    client: &mut McpClient,
    plan: &Value,
    output: &Path,
    backend: &str,
    phase: &str,
    expected: &str,
) -> RResult<Vec<Value>> {
    let dir = output.join("images").join(phase);
    if dir.exists() {
        return Err(RunnerError::Io(format!("[Errno 17] File exists: '{}'", dir.display())));
    }
    std::fs::create_dir_all(&dir)?;
    let mut results = Vec::new();
    for spec in plan.get("scenes").and_then(Value::as_array).cloned().unwrap_or_default() {
        let name = spec["name"].as_str().unwrap_or_default().to_owned();
        let payload = json!({"expected_content_id": expected, "backend": backend, "viewport_width_px": 1100,
                             "viewport_height_px": 900, "scene": spec["scene"]});
        let attempt = (|| -> RResult<Value> {
            let (result, wire) = client.action("render_viewer_snapshot", &payload)?;
            let raw = decode_image(&result, &wire, expected)?;
            {
                use std::io::Write as _;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dir.join(format!("{name}.png")))?;
                f.write_all(&raw)?;
            }
            write_json(&dir.join(format!("{name}.json")), &result, false)?;
            Ok(json!({"name": name, "status": "rendered", "phase": phase, "content_id": expected,
                      "image": result["image"], "renderer": result.get("renderer").cloned().unwrap_or(Value::Null)}))
        })();
        match attempt {
            Ok(v) => results.push(v),
            Err(e @ RunnerError::Action { .. }) => {
                let record =
                    json!({"name": name, "status": "failed", "phase": phase, "error": e.to_string()});
                write_json(&dir.join(format!("{name}_failure.json")), &record, false)?;
                results.push(record);
                eprintln!("{e}");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(results)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn collect(
    client: &mut McpClient,
    fw: &Framework,
    manifest: &Value,
    plan: &Value,
    ident: &str,
    job: &Value,
    output: &Path,
    accept: bool,
    render: &str,
) -> RResult<Map<String, Value>> {
    check_source(fw, manifest)?;
    let summary_of = job.get("summary").cloned().unwrap_or(Value::Null);
    let (first, source) = objective_origin(job);
    let g = |k: &str| job.get(k).cloned().unwrap_or(Value::Null);
    let mut summary: Map<String, Value> = [
        ("job_id", json!(ident)),
        ("status", g("status")),
        ("updates", json!(job_iterations(job)?)),
        ("objective_first", first),
        ("reported_summary_objective_first", g("L_first")),
        ("objective_last", g("L_last")),
        ("objective_best", g("L_best")),
        ("objective_origin_source", json!(source)),
        ("optimization_converged", summary_of.get("optimization_converged").cloned().unwrap_or(Value::Null)),
        ("termination_reason", summary_of.get("termination_reason").cloned().unwrap_or(Value::Null)),
        ("accepted", json!(false)),
        ("physical_validation", json!("not established by client or job completion")),
        ("execution_error", g("error")),
        ("epochs", json!([])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    match export_epochs(client, ident, job, output) {
        Ok(e) => {
            summary.insert("epochs".into(), Value::Array(e));
        }
        Err(e @ RunnerError::Action { .. }) => {
            if job.get("status").and_then(Value::as_str) == Some("completed") {
                return Err(e);
            }
            summary.insert("epoch_export_error".into(), json!(e.to_string()));
            write_json(
                &output.join("epoch_export_error.json"),
                &json!({"error": e.to_string(), "primary_job_error": g("error"), "read_only": true}),
                false,
            )?;
        }
        Err(e) => return Err(e),
    }
    if accept {
        if job.get("status").and_then(Value::as_str) != Some("completed")
            || job_iterations(job)? < 1
            || job.get("canonical_eligible") != Some(&json!(true))
        {
            return rerr(
                "Refusing automatic acceptance: need a completed eligible job with at least one actual update.",
            );
        }
        match client
            .action("optimization_operation", &json!({"job_id": ident, "op": "accept", "view": "monitor"}))
        {
            Err(RunnerError::Action { action, response }) => {
                let e = RunnerError::Action { action, response: response.clone() };
                summary.insert("acceptance_status".into(), json!("refused"));
                summary.insert("acceptance_error".into(), json!(e.to_string()));
                write_json(&output.join("acceptance_refused.json"), &response, false)?;
            }
            Err(e) => return Err(e),
            Ok((result, _)) => {
                assert_job_id(&result, ident)?;
                let Some(accepted) = result.get("accepted").filter(|a| a.is_object()).cloned() else {
                    return rerr("No confirmed normal acceptance receipt.");
                };
                write_json(&output.join("acceptance.json"), &result, false)?;
                let selected = accepted
                    .get("engineering_admission")
                    .filter(|x| implexity_core::pyobj::truthy(x))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let pick = |a: &str, b: &str| {
                    accepted.get(a).cloned().or_else(|| selected.get(b).cloned()).unwrap_or(Value::Null)
                };
                summary.insert("accepted".into(), json!(true));
                summary.insert("acceptance_status".into(), json!("accepted"));
                summary.insert("accepted_objective".into(), pick("objective", "objective"));
                summary.insert("accepted_epoch".into(), pick("selected_epoch", "epoch"));
                summary.insert("accepted_source_file".into(), pick("selected_source_file", "file"));
                summary.insert("acceptance".into(), accepted);
            }
        }
    }
    let (state, _) = client.action("inspect_state", &json!({}))?;
    write_json(&output.join("final_public_state.json"), &state, false)?;
    let (renderables, _) = client.action("inspect_renderables", &json!({}))?;
    write_json(&output.join("renderables.json"), &renderables, false)?;
    if render != "none" {
        if summary.get("accepted") == Some(&json!(true)) {
            let expected = model_id(&state)?;
            if summary.get("acceptance").and_then(|a| a.get("document_content_id")).and_then(Value::as_str)
                != Some(expected.as_str())
            {
                return rerr("Accepted model no longer matches the current model.");
            }
            let r = render_scenes(client, plan, output, render, "accepted", &expected)?;
            summary.insert("rendering".into(), Value::Array(r));
        } else {
            summary.insert(
                "rendering".into(),
                json!([{"status": "not_requested_without_acceptance",
                        "reason": "Native capture reads the current model; no checkpoint import/temporary mutation is used."}]),
            );
        }
    }
    Ok(summary)
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for e in entries {
        let ty = e.file_type()?;
        if ty.is_dir() {
            walk_files(&e.path(), out)?;
        } else if ty.is_file() {
            out.push(e.path());
        }
    }
    Ok(())
}

pub(crate) fn archive_run(output: &Path) -> RResult<Value> {
    let target = output.join("run_evidence.zip");
    let mut all = Vec::new();
    walk_files(output, &mut all)?;
    let case = output.join("case");
    let paths: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            !p.starts_with(&case)
                && name != "run_evidence.zip"
                && name != "run_evidence_integrity.json"
                && name.rsplit_once('.').is_none_or(|(_, ext)| ext != "tmp")
        })
        .collect();
    let rel = |p: &Path| {
        p.strip_prefix(output)
            .map_or_else(|_| p.display().to_string(), |r| r.to_string_lossy().replace('\\', "/"))
    };
    let mut manifest = Map::new();
    for p in &paths {
        let len = std::fs::metadata(p)?.len();
        manifest.insert(rel(p), json!({"bytes": len, "sha256": sha256_file(p)?}));
    }
    let build = || -> RResult<Vec<u8>> {
        let mut w = implexity_io::zip::ZipWriter::new();
        for p in &paths {
            let data = std::fs::read(p)?;
            w.add_member(
                &rel(p),
                &data,
                implexity_io::zip::DEFLATED,
                3,
                implexity_io::zip::MemberOptions::now(),
            )
            .map_err(|e| RunnerError::Runner(e.0))?;
        }
        w.add_member(
            "CLIENT_EVIDENCE_MANIFEST.json",
            &encoded(&Value::Object(manifest.clone())),
            implexity_io::zip::DEFLATED,
            3,
            implexity_io::zip::MemberOptions::now(),
        )
        .map_err(|e| RunnerError::Runner(e.0))?;
        w.finish().map_err(|e| RunnerError::Runner(e.0))
    };
    let bytes = build()?;
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&target)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    let verify = || -> RResult<()> {
        let raw = std::fs::read(&target)?;
        let z = implexity_io::zip::ZipArchive::new(&raw)
            .map_err(|_| RunnerError::Runner("Evidence ZIP CRC failure.".into()))?;
        for e in z.entries() {
            z.read_entry(e).map_err(|_| RunnerError::Runner("Evidence ZIP CRC failure.".into()))?;
        }
        for (name, item) in &manifest {
            let data =
                z.read(name).map_err(|_| RunnerError::Runner("Evidence ZIP content mismatch.".into()))?;
            if Some(implexity_io::digest::sha256_hex(&data).as_str()) != item["sha256"].as_str() {
                return rerr("Evidence ZIP content mismatch.");
            }
        }
        Ok(())
    };
    if let Err(e) = verify() {
        let _ = std::fs::rename(
            &target,
            output.join(format!("UNVERIFIED_evidence_{}.zip", implexity_io::atomic::unique_token())),
        );
        return Err(e);
    }
    let receipt = json!({"schema": "implexity-local-evidence-archive/1", "files": manifest.len(),
        "bytes": std::fs::metadata(&target)?.len(), "sha256": sha256_file(&target)?, "verified": true,
        "scope": "Client public requests/responses, outputs and logs. Private service case retained separately on disk."});
    write_json(&output.join("run_evidence_integrity.json"), &receipt, false)?;
    Ok(receipt)
}

#[allow(clippy::struct_excessive_bools)]
struct RunArgs {
    framework: Option<String>,
    plan: String,
    output: String,
    base_url: Option<String>,
    iterations: Option<i64>,
    budget_seconds: Option<f64>,
    monitor_timeout: f64,
    poll: f64,
    rpc_timeout: f64,
    startup_timeout: f64,
    preflight_only: bool,
    accept: bool,
    render: String,
    render_initial: String,
    allow_unqualified_runtime: bool,
    keep_server: bool,
    resume_from: Option<String>,
    resume_epoch: Option<i64>,
}

const CONTINUATION_SCHEMA: &str = "implexity-optimization-continuation/1";

pub(super) fn is_generation(dir: &Path) -> bool {
    dir.join("history.json").is_file() && dir.join("ckpt.npz").is_file() && dir.join("initial.npz").is_file()
}

pub(super) fn recorded_generations(run: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(run.join("case/implicit/opt"), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if !dir.is_dir() || dir.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        if is_generation(&dir) {
            found.push(dir);
            continue;
        }
        if depth < 5
            && let Ok(children) = std::fs::read_dir(&dir)
        {
            stack.extend(children.flatten().map(|e| (e.path(), depth + 1)));
        }
    }
    found.sort();
    found
}

pub(super) fn history_length(dir: &Path) -> usize {
    read_json(&dir.join("history.json"), 1 << 30)
        .ok()
        .and_then(|v| v.get("history").and_then(Value::as_array).map(Vec::len))
        .unwrap_or(0)
}

fn prepare_continuation(
    source: &str,
    epoch: Option<i64>,
    plan: &Value,
    output: &Path,
) -> RResult<(Value, Value)> {
    let path = crate::util::expand_abs(source);
    let (generation, run_dir) = if is_generation(&path) {
        (path.clone(), None)
    } else {
        let candidates = recorded_generations(&path);
        let Some(best) = candidates.iter().max_by_key(|d| history_length(d)).cloned() else {
            return rerr(format!("No recorded provider-job generation under {}.", path.display()));
        };
        (best, Some(path.clone()))
    };
    if let Some(run) = &run_dir
        && let Ok(previous) = read_json(&run.join("effective_plan.json"), MAX_PLAN)
    {
        if !super::continuation_plan::resume_plan_equivalent(&previous, plan) {
            return rerr(
                "The resumed run's effective plan differs from this plan in more than the iteration count; refusing to continue it.",
            );
        }
    }
    let length = history_length(&generation);
    if length == 0 {
        return rerr("The source generation has no recorded history.");
    }
    if let Some(e) = epoch
        && (e < 0 || usize::try_from(e).unwrap_or(usize::MAX) >= length)
    {
        return rerr(format!("--resume-epoch must lie in 0..{}.", length - 1));
    }
    let target = output.join("continued_from");
    std::fs::create_dir_all(&target)?;
    let mut copied = Map::new();
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&generation)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let extension = Path::new(&name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        let keep = entry.file_type()?.is_file()
            && !name.contains(".tmp")
            && matches!(extension.as_deref(), Some("npz" | "json"));
        if keep {
            std::fs::copy(entry.path(), target.join(&name))?;
            copied.insert(name.clone(), json!(sha256_file(&target.join(&name))?));
        }
    }

    let upto = epoch.and_then(|e| usize::try_from(e).ok());
    implexity_jobs::epoch_state::copy_epoch_states(&generation, &target, upto)
        .map_err(|e| RunnerError::Runner(format!("copying the epoch states: {e}")))?;
    for relative in implexity_jobs::epoch_state::epoch_state_files(&target) {
        copied.insert(relative.clone(), json!(sha256_file(&target.join(&relative))?));
    }
    let source_dir = std::fs::canonicalize(&target).unwrap_or(target);
    let request = json!({"schema": CONTINUATION_SCHEMA, "source_dir": source_dir.display().to_string(),
                         "epoch": epoch});
    let record = json!({"schema": "implexity-study-continuation/1", "utc": utc_iso(),
        "resume_from": path.display().to_string(), "source_generation": generation.display().to_string(),
        "source_history_length": length, "requested_epoch": epoch,
        "seed_epoch": epoch.map_or_else(|| json!("last row from which the search continues"), Value::from),
        "copied_files": copied, "request": request});
    Ok((request, record))
}

fn run_outcome(status: &str, termination: Option<&str>) -> &'static str {
    match (status, termination) {
        ("completed", Some("converged")) => "converged",
        ("completed", Some("line_search")) => "stalled",
        ("completed", Some("penalty_limit")) => "completed_early",
        ("completed", _) => "completed",
        ("failed" | "error", _) => "error",
        ("stopped", _) => "stopped",
        _ => "incomplete",
    }
}

#[allow(clippy::too_many_lines)]
fn run_study(args: &RunArgs) -> RResult<u8> {
    if args.resume_epoch.is_some() && args.resume_from.is_none() {
        return rerr("--resume-epoch requires --resume-from.");
    }
    let fw = framework_root(args.framework.as_deref())?;
    let plan_path = crate::util::expand_abs(&args.plan);
    let original = validate_plan(&read_json(&plan_path, MAX_PLAN)?)?;
    let (plan, overrides) = effective_plan(&original, args.iterations, args.budget_seconds)?;
    let output = crate::util::expand_abs(&args.output);
    if output.starts_with(&fw.root) {
        return rerr("Keep run output outside the framework directory (e.g. ../ImplexityRuns/run01).");
    }
    if output.exists() {
        return Err(RunnerError::Io(format!("[Errno 17] File exists: '{}'", output.display())));
    }
    std::fs::create_dir_all(&output)?;
    let mut report = Map::new();
    let mut service: Option<LocalService> = None;
    let mut client: Option<McpClient> = None;
    let mut ident: Option<String> = None;
    let mut service_safe_to_close = true;
    let mut exit_code: u8 = 1;
    let manifest = source_manifest(&fw)?;
    write_json(&output.join("source_manifest.json"), &manifest, false)?;
    write_json(&output.join("original_plan.json"), &original, false)?;
    write_json(&output.join("effective_plan.json"), &plan, false)?;
    write_json(&output.join("requested_overrides.json"), &Value::Array(overrides), false)?;
    let mut url = String::new();
    let body = (|| -> RResult<()> {
        let environment = doctor(&fw)?;
        write_json(&output.join("doctor.json"), &environment, false)?;
        require_runtime(&environment, args.allow_unqualified_runtime)?;
        if environment["version_matches_client"] != json!(true) {
            println!(
                "WARNING: explicitly permitted a service build that differs from this client; see doctor.json."
            );
        }
        if let Some(b) = &args.base_url {
            url = origin(b)?.0;
        } else {
            let mut s = LocalService::new(&fw, &output, args.startup_timeout)?;
            let started = s.start();
            url.clone_from(&s.url);
            service = Some(s);
            started?;
        }
        let mut session = json!({"schema": SESSION_SCHEMA, "created_utc": utc_iso(), "framework": fw.root.display().to_string(),
            "base_url": url, "output": output.display().to_string(), "job_id": null,
            "source_sha256": manifest["sha256"], "plan_sha256": canonical_hash(&plan),
            "owned_service": service.is_some(), "owned_pid": service.as_ref().and_then(LocalService::pid)});
        write_json(&output.join("session.json"), &session, false)?;
        client = Some(McpClient::open(&fw, &url, &output.join("mcp"), args.rpc_timeout)?);
        let c = client.as_mut().ok_or_else(|| RunnerError::Runner("no client".into()))?;
        let (state, _) = c.action("inspect_state", &json!({}))?;
        write_json(&output.join("initial_public_state.json"), &state, false)?;
        let jobs = state
            .get("optimization_jobs")
            .filter(|j| implexity_core::pyobj::truthy(j))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !jobs.is_object() || !jobs.get("jobs").is_none_or(Value::is_array) {
            return rerr("Unexpected public job-list contract.");
        }
        if state.get("model").and_then(|m| m.get("loaded")).is_some_and(implexity_core::pyobj::truthy)
            || jobs.get("jobs").is_some_and(implexity_core::pyobj::truthy)
            || jobs.get("active").is_some_and(implexity_core::pyobj::truthy)
        {
            return rerr(
                "Study authoring requires an empty live session; refusing to overwrite an existing model or jobs.",
            );
        }
        for row in plan["authoring"].as_array().cloned().unwrap_or_default() {
            let action = row["action"].as_str().unwrap_or_default();
            let (authored, _) = c.action(action, &row["payload"])?;
            println!("Authored: {action}");
            if action == "author_application_case" {
                check_application_case(&authored, &plan["optimization"])?;
                for step in authored["execution"]["steps"].as_array().into_iter().flatten() {
                    println!("Authored (application case): {}", py(step.get("action")));
                }
            }
        }
        let (state, _) = c.action("inspect_state", &json!({}))?;
        write_json(&output.join("authored_public_state.json"), &state, false)?;
        let (preflight, _) = c.action("preflight_optimization", &plan["optimization"])?;
        write_json(&output.join("preflight.json"), &preflight, false)?;
        if preflight.get("ok") != Some(&json!(true)) {
            return rerr("Framework preflight did not authorize start; see preflight.json.");
        }
        println!("Preflight completed. This is not physical validity or convergence.");
        if args.render_initial != "none" {
            let r = render_scenes(c, &plan, &output, &args.render_initial, "initial", &model_id(&state)?)?;
            report.insert("initial_rendering".into(), Value::Array(r));
        }
        if args.preflight_only {
            report.insert("status".into(), json!("preflight_only"));
            report.insert("accepted".into(), json!(false));
            report.insert("updates".into(), json!(0));
            exit_code = 0;
            return Ok(());
        }
        check_source(&fw, &manifest)?;
        let mut start_payload = plan["optimization"].clone();
        if let Some(source) = &args.resume_from {
            let (request, record) = prepare_continuation(source, args.resume_epoch, &plan, &output)?;
            write_json(&output.join("continuation.json"), &record, false)?;
            println!(
                "Continuing {} from epoch {} ({} recorded rows).",
                record["source_generation"].as_str().unwrap_or_default(),
                record["seed_epoch"],
                record["source_history_length"]
            );
            report.insert("continuation".into(), record);
            if let Some(o) = start_payload.as_object_mut() {
                o.insert("continuation".into(), request);
            }
        }
        write_json(
            &output.join("start_intent.json"),
            &json!({"utc": utc_iso(), "payload_sha256": canonical_hash(&start_payload)}),
            false,
        )?;
        service_safe_to_close = false;
        let (start, _) = c.action("start_optimization", &start_payload)?;
        let id = job_id_from(&start)?;
        ident = Some(id.clone());
        session["job_id"] = json!(id);
        write_json(&output.join("session.json"), &session, true)?;
        write_json(&output.join("start_receipt.json"), &start, false)?;
        println!("Job {id}; live viewer {url}/viewer/model.html");
        let (mut job, reason) = wait_job(c, &id, &output, args.poll, args.monitor_timeout)?;
        if reason != "terminal" {
            write_json(
                &output.join("stop_reason.json"),
                &json!({"reason": reason, "utc": utc_iso()}),
                false,
            )?;
            job = stop_job(c, &id, &output)?;
        }
        let status = job.get("status").and_then(Value::as_str).unwrap_or_default().to_owned();
        service_safe_to_close = TERMINAL.contains(&status.as_str());
        let collected = collect(
            c,
            &fw,
            &manifest,
            &plan,
            &id,
            &job,
            &output,
            args.accept && reason == "terminal" && status == "completed",
            &args.render,
        )?;
        report.extend(collected);
        report.insert("monitor_end".into(), json!(reason));
        report.insert(
            "run_outcome".into(),
            json!(run_outcome(
                &status,
                job.get("summary").and_then(|s| s.get("termination_reason")).and_then(Value::as_str)
            )),
        );
        exit_code = if status == "completed"
            && job_iterations(&job)? > 0
            && (!args.accept || report.get("accepted") == Some(&json!(true)))
        {
            0
        } else {
            3
        };
        let rendering_failed = report
            .get("rendering")
            .and_then(Value::as_array)
            .is_some_and(|r| r.iter().any(|x| x.get("status").and_then(Value::as_str) != Some("rendered")));
        if exit_code == 0 && rendering_failed {
            exit_code = 4;
        }
        Ok(())
    })();
    if let Err(e) = body {
        report.insert("status".into(), json!("failed"));
        report.insert("error".into(), json!(e.to_string()));
        report.insert("error_type".into(), json!(e.class()));
        eprintln!("{e}");
        exit_code = if matches!(e, RunnerError::Ambiguous(_)) { 2 } else { 1 };
        if matches!(e, RunnerError::Ambiguous(_)) {
            service_safe_to_close = false;
        }
    }
    if client.is_some() && !service_safe_to_close {
        let reconcile = (|| -> RResult<()> {
            match &ident {
                None => {
                    if client.as_ref().is_some_and(|c| c.poisoned) {
                        if let Some(mut c) = client.take() {
                            c.close();
                        }
                        client = Some(McpClient::open(
                            &fw,
                            &url,
                            &output.join("mcp_reconcile"),
                            args.rpc_timeout,
                        )?);
                    }
                    let c = client.as_mut().ok_or_else(|| RunnerError::Runner("no client".into()))?;
                    let (state, _) = c.action("inspect_state", &json!({}))?;
                    write_json(&output.join("ambiguous_public_state.json"), &state, false)?;
                    report.insert("reconciliation_required".into(), json!(true));
                }
                Some(id) => {
                    if client.as_ref().is_some_and(|c| c.poisoned) {
                        if let Some(mut c) = client.take() {
                            c.close();
                        }
                        client =
                            Some(McpClient::open(&fw, &url, &output.join("mcp_stop"), args.rpc_timeout)?);
                    }
                    let c = client.as_mut().ok_or_else(|| RunnerError::Runner("no client".into()))?;
                    let job = stop_job(c, id, &output)?;
                    report.insert("stop_status".into(), job.get("status").cloned().unwrap_or(Value::Null));
                    service_safe_to_close =
                        TERMINAL.contains(&job.get("status").and_then(Value::as_str).unwrap_or_default());
                }
            }
            Ok(())
        })();
        if let Err(e) = reconcile {
            report.insert("stop_error".into(), json!(e.to_string()));
        }
    }
    let actual_mcp = client.is_some();
    if let Some(mut c) = client.take() {
        c.close();
    }
    if let Some(s) = service.as_mut() {
        if service_safe_to_close && !args.keep_server {
            s.close();
            report.insert("owned_service_stopped".into(), json!(true));
        } else {
            report.insert(
                "owned_service_left_running".into(),
                json!({"pid": s.pid(), "base_url": s.url,
                       "reason": "explicit --keep-server or unresolved public stop/start outcome"}),
            );
            println!("Service retained for public inspection: {}", s.url);
            s.release();
        }
    }
    match source_manifest(&fw) {
        Ok(after) => {
            let same = after["sha256"] == manifest["sha256"];
            report.insert("runtime_source_unchanged".into(), json!(same));
            if !same {
                exit_code = 1;
            }
        }
        Err(e) => {
            report.insert("source_check_error".into(), json!(e.to_string()));
            exit_code = 1;
        }
    }
    report.insert("schema".into(), json!("implexity-local-mcp-run-report/1"));
    report.insert("utc".into(), json!(utc_iso()));
    report.insert("exit_code".into(), json!(exit_code));
    report.insert("framework".into(), json!(fw.root.display().to_string()));
    report.insert("plan_name".into(), plan["name"].clone());
    report.insert("actual_mcp".into(), json!(actual_mcp));
    report.insert("limitations".into(), plan.get("limitations").cloned().unwrap_or_else(|| json!([])));
    report.insert(
        "note".into(),
        json!("This client implements no physical equations or gradient update. Success is not production qualification."),
    );
    write_json(&output.join("run_report.json"), &Value::Object(report.clone()), false)?;
    if !report.contains_key("owned_service_left_running")
        && let Err(e) = archive_run(&output)
    {
        eprintln!("Evidence packaging failed: {e}");
        exit_code = 1;
        write_json(
            &output.join("packaging_failure.json"),
            &json!({"error": e.to_string(), "exit_code": 1}),
            false,
        )?;
    }
    println!("Saved run: {}\nExit code: {exit_code}", output.display());
    Ok(exit_code)
}

fn require_resume_contract(preflight: &Value, allow_unqualified: bool) -> RResult<()> {
    let c = preflight.get("native_runtime_contract").cloned().unwrap_or_else(|| json!({}));
    let ok = c.get("schema").and_then(Value::as_str) == Some("implexity-native-runtime-contract/1")
        && c.get("pause_resume_replay").and_then(Value::as_str)
            == Some("exact_checkpoint_numerical_response_verification_v1");
    if !ok && !allow_unqualified {
        return rerr(
            "This server did not declare the corrected numerical replay contract. Older native resume is unqualified; use --allow-unqualified-resume only for diagnosis.",
        );
    }
    Ok(())
}

struct SessionArgs {
    command: String,
    session: String,
    framework: Option<String>,
    rpc_timeout: f64,
    op: Option<String>,
    allow_unqualified_resume: bool,
    poll: f64,
    monitor_timeout: f64,
    accept: bool,
    render: String,
}

fn connect_session(args: &SessionArgs) -> RResult<u8> {
    let session_path = crate::util::expand_abs(&args.session);
    let session = read_json(&session_path, MAX_PLAN)?;
    let job_ok = session.get("job_id").and_then(Value::as_str).is_some_and(job_id_ok);
    if session.get("schema").and_then(Value::as_str) != Some(SESSION_SCHEMA) || !job_ok {
        return rerr(
            "Session has no confirmed job ID. Inspect the saved MCP transcript; do not restart authoring.",
        );
    }
    let fw = framework_root(Some(
        args.framework.as_deref().or_else(|| session["framework"].as_str()).unwrap_or_default(),
    ))?;
    let output = session_path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    if args.command == "control" && args.op.as_deref() == Some("resume") {
        require_resume_contract(
            &read_json(&output.join("preflight.json"), MAX_PLAN)?,
            args.allow_unqualified_resume,
        )?;
    }
    let manifest = read_json(&output.join("source_manifest.json"), MAX_PLAN)?;
    check_source(&fw, &manifest)?;
    let plan = validate_plan(&read_json(&output.join("effective_plan.json"), MAX_PLAN)?)?;
    if Some(canonical_hash(&plan).as_str()) != session.get("plan_sha256").and_then(Value::as_str) {
        return rerr("Saved plan identity changed.");
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let stamp =
        implexity_io::digest::format_utc(secs).replace(['-', ':'], "").trim_end_matches('Z').to_owned();
    let token: String =
        implexity_io::atomic::unique_token().chars().filter(char::is_ascii_hexdigit).take(8).collect();
    let operation_dir = output.join(format!("operation_{stamp}_{token}"));
    std::fs::create_dir(&operation_dir)?;
    let ident = session["job_id"].as_str().unwrap_or_default().to_owned();
    let base = session["base_url"].as_str().unwrap_or_default().to_owned();
    let mut client = McpClient::open(&fw, &base, &operation_dir.join("mcp"), args.rpc_timeout)?;
    let (mut job, _) =
        client.action("inspect_optimization_job", &json!({"job_id": ident, "view": "monitor"}))?;
    assert_job_id(&job, &ident)?;
    let mut result = Value::Null;
    if args.command == "control" {
        let op = args.op.clone().unwrap_or_default();
        if op == "accept" {
            return rerr("Use watch --accept so eligibility, exports and identities are checked together.");
        }
        if op == "stop" {
            job = stop_job(&mut client, &ident, &operation_dir)?;
        } else {
            job = client
                .action("optimization_operation", &json!({"job_id": ident, "op": op, "view": "monitor"}))?
                .0;
        }
        write_json(&operation_dir.join("result.json"), &job, false)?;
        print_progress(&job);
    } else {
        let (j, reason) = wait_job(&mut client, &ident, &operation_dir, args.poll, args.monitor_timeout)?;
        job = j;
        if reason != "terminal" {
            write_json(
                &operation_dir.join("observation_end.json"),
                &json!({"reason": reason, "job": job}),
                false,
            )?;
            println!("Observation ended; no hidden stop/resume/restart was issued.");
            return Ok(3);
        }
        result = Value::Object(collect(
            &mut client,
            &fw,
            &manifest,
            &plan,
            &ident,
            &job,
            &operation_dir,
            args.accept,
            &args.render,
        )?);
        write_json(&operation_dir.join("result.json"), &result, false)?;
    }
    client.close();
    archive_run(&operation_dir)?;
    println!("Saved operation: {}", operation_dir.display());
    if args.command == "control" {
        return Ok(0);
    }
    if job.get("status").and_then(Value::as_str) != Some("completed")
        || job_iterations(&job)? < 1
        || (args.accept && result.get("accepted") != Some(&json!(true)))
    {
        return Ok(3);
    }
    let failed = result
        .get("rendering")
        .and_then(Value::as_array)
        .is_some_and(|r| r.iter().any(|x| x.get("status").and_then(Value::as_str) != Some("rendered")));
    Ok(if failed { 4 } else { 0 })
}

const RENDERS: &[&str] = &["none", "browser", "egl_offscreen"];

#[allow(clippy::too_many_lines)]
fn parsers() -> Vec<(&'static str, Parser)> {
    let framework_help =
        "directory holding the implexity and implexity-mcp executables (default: this executable's)";
    vec![
        (
            "doctor",
            Parser::new("implexity study doctor", "Read the executables and platform; start no solver.")
                .opt("--framework", Kind::Str, 1, framework_help)
                .opt("--output", Kind::Str, 1, "also write the report here"),
        ),
        (
            "run",
            Parser::new("implexity study run", "Author, preflight and optionally optimize through real MCP.")
                .opt("--framework", Kind::Str, 1, framework_help)
                .opt("--plan", Kind::Str, 1, "the study plan (implexity-mcp-study-plan/1)")
                .required()
                .opt("--output", Kind::Str, 1, "a new run directory outside the framework directory")
                .required()
                .opt("--base-url", Kind::Str, 1, "Use an already running EMPTY local service; do not launch or stop it.")
                .opt("--iterations", Kind::Int, 1, "Explicitly override only the native update count.")
                .opt("--budget-seconds", Kind::Float, 1, "Explicit native operation wall budget (1..14400 s).")
                .opt("--monitor-timeout", Kind::Float, 1, "Client observation budget; 0 means no extra limit.")
                .opt("--poll", Kind::Float, 1, "seconds between monitor polls (default 5)")
                .opt("--rpc-timeout", Kind::Float, 1, "seconds per MCP request (default 180)")
                .opt("--startup-timeout", Kind::Float, 1, "seconds for the owned service to bind (default 120)")
                .opt("--preflight-only", Kind::Flag, 0, "")
                .opt("--accept", Kind::Flag, 0, "Explicitly authorize normal acceptance of a completed eligible job.")
                .opt("--render", Kind::Str, 1, "Capture accepted state only; no fallback.")
                .choices(RENDERS)
                .opt("--render-initial", Kind::Str, 1, "")
                .choices(RENDERS)
                .opt(
                    "--allow-unqualified-runtime",
                    Kind::Flag,
                    0,
                    "Record and explicitly permit a service build whose version differs from this client.",
                )
                .opt("--keep-server", Kind::Flag, 0, "Leave owned service available for GUI/public control after this command.")
                .opt(
                    "--resume-from",
                    Kind::Str,
                    1,
                    "Warm-resume: continue the recorded optimization of an earlier run directory of the same plan (or a provider-job generation directory) from its last committed epoch, with its optimizer state, to --iterations updates in total.",
                )
                .opt("--resume-epoch", Kind::Int, 1, "With --resume-from: continue from this recorded epoch instead of the last."),
        ),
        (
            "watch",
            Parser::new("implexity study watch", "Reconnect to a still-live job; does not restart or reauthor it.")
                .opt("--session", Kind::Str, 1, "the run's session.json")
                .required()
                .opt("--framework", Kind::Str, 1, "Explicit framework path after moving the executables; hashes must still match.")
                .opt("--rpc-timeout", Kind::Float, 1, "")
                .opt("--poll", Kind::Float, 1, "")
                .opt("--monitor-timeout", Kind::Float, 1, "")
                .opt("--accept", Kind::Flag, 0, "")
                .opt("--render", Kind::Str, 1, "")
                .choices(RENDERS),
        ),
        (
            "control",
            Parser::new("implexity study control", "Reconnect to a still-live job; does not restart or reauthor it.")
                .opt("--session", Kind::Str, 1, "the run's session.json")
                .required()
                .opt("--framework", Kind::Str, 1, "Explicit framework path after moving the executables; hashes must still match.")
                .opt("--rpc-timeout", Kind::Float, 1, "")
                .opt("--op", Kind::Str, 1, "")
                .choices(&["pause", "resume", "stop", "discard"])
                .required()
                .opt(
                    "--allow-unqualified-resume",
                    Kind::Flag,
                    0,
                    "Permit a legacy server without the corrected replay contract for diagnosis; no server safety check is bypassed.",
                ),
        ),
        (
            "replay",
            Parser::new("implexity study replay", "Replay explicit actions through the real MCP stdio bridge (no solver calls).")
                .opt("--actions", Kind::Str, 1, "a JSON list of {action, payload} records")
                .required()
                .opt("--output", Kind::Str, 1, "a new directory for the transcript")
                .required()
                .opt("--base-url", Kind::Str, 1, "the running service (default http://127.0.0.1:8765)")
                .opt("--timeout", Kind::Float, 1, "seconds per request (default 180)")
                .opt("--framework", Kind::Str, 1, framework_help),
        ),
        (
            "reproduce-epoch",
            Parser::new(
                "implexity study reproduce-epoch",
                "Re-evaluate one recorded epoch's design from its per-epoch state and compare the recorded responses.",
            )
            .opt("--from", Kind::Str, 1, "a run directory or a provider-job generation directory")
            .required()
            .opt("--epoch", Kind::Int, 1, "the recorded epoch (default: the last)")
            .opt("--output", Kind::Str, 1, "a new directory for the reproduction job and its report")
            .required()
            .opt("--framework", Kind::Str, 1, framework_help),
        ),
        (
            "epoch-index",
            Parser::new(
                "implexity study epoch-index",
                "Write the per-epoch restart-state index of a run's recorded generations.",
            )
            .opt("--from", Kind::Str, 1, "a run directory or a provider-job generation directory")
            .required()
            .opt("--output", Kind::Str, 1, "the index file (default: <from>/epochs_state_index.json)"),
        ),
    ]
}

fn usage() -> String {
    "usage: implexity study {doctor,run,watch,control,replay,reproduce-epoch,epoch-index,legacy-run,legacy-uniform} ...\n\n\
     Operate Implexity's REAL MCP bridge (implexity-mcp) from a local client.\n\n\
     No solver, geometry generator, material law, mesh repair or alternate optimizer is\n\
     implemented here. Studies are declarative JSON plans. All engineering actions use\n\
     tools/call on the existing stdio bridge. See docs/local_mcp_runner.md.\n\n\
     commands:\n    \
     doctor    Read the executables and platform; start no solver.\n    \
     run       Author, preflight and optionally optimize through real MCP.\n    \
     watch     Reconnect to a still-live job; does not restart or reauthor it.\n    \
     control   Reconnect to a still-live job; does not restart or reauthor it.\n    \
     replay    Replay explicit action/payload records through the bridge.\n    \
     reproduce-epoch  Re-evaluate a recorded epoch from its per-epoch state; compare responses.\n    \
     epoch-index      Index what each recorded epoch of a run can be restarted from.\n    \
"
        .to_owned()
}

#[allow(clippy::too_many_lines)]
pub(crate) fn main(argv: &[String]) -> u8 {
    let Some(cmd) = argv.first() else {
        eprintln!("{}implexity study: error: the following arguments are required: command", usage());
        return 2;
    };
    if matches!(cmd.as_str(), "-h" | "--help") {
        print!("{}", usage());
        return 0;
    }
    let all = parsers();
    let Some((_, p)) = all.iter().find(|(n, _)| n == cmd) else {
        eprintln!(
            "{}implexity study: error: argument command: invalid choice: '{cmd}' (choose from 'doctor', 'run', 'watch', 'control', 'replay', 'reproduce-epoch', 'epoch-index')",
            usage()
        );
        return 2;
    };
    let a = match p.parse(&argv[1..]) {
        Ok(a) => a,
        Err(Exit::Help(t)) => {
            print!("{t}");
            return 0;
        }
        Err(Exit::Error(t)) => {
            eprint!("{t}");
            return 2;
        }
    };
    let result = (|| -> RResult<u8> {
        let f = |k: &str, d: f64| a.float(k).unwrap_or(d);
        for (key, default, min) in [
            ("--poll", 5.0, 0.1),
            ("--rpc-timeout", 180.0, 0.1),
            ("--startup-timeout", 120.0, 0.1),
            ("--monitor-timeout", 0.0, 0.0),
        ] {
            let v = f(key, default);
            if !v.is_finite() || v < min {
                return rerr(format!("Invalid {}.", key.trim_start_matches("--").replace('-', "_")));
            }
        }
        if !(1.0..=14700.0).contains(&f("--rpc-timeout", 180.0)) {
            return rerr("RPC timeout must be 1..14700 seconds.");
        }
        match cmd.as_str() {
            "doctor" => {
                let fw = framework_root(a.str("--framework"))?;
                let report = doctor(&fw)?;
                println!(
                    "{}",
                    implexity_core::json::dumps(&report, &implexity_core::json::DumpOptions::indented(2))
                );
                if let Some(o) = a.str("--output") {
                    write_json(&crate::util::expand_abs(o), &report, false)?;
                }
                Ok(if doctor_ok(&report) { 0 } else { 2 })
            }
            "run" => run_study(&RunArgs {
                framework: a.str("--framework").map(str::to_owned),
                plan: a.str("--plan").unwrap_or_default().to_owned(),
                output: a.str("--output").unwrap_or_default().to_owned(),
                base_url: a.str("--base-url").map(str::to_owned),
                iterations: a.int("--iterations"),
                budget_seconds: a.float("--budget-seconds"),
                monitor_timeout: f("--monitor-timeout", 0.0),
                poll: f("--poll", 5.0),
                rpc_timeout: f("--rpc-timeout", 180.0),
                startup_timeout: f("--startup-timeout", 120.0),
                preflight_only: a.flag("--preflight-only"),
                accept: a.flag("--accept"),
                render: a.str("--render").unwrap_or("none").to_owned(),
                render_initial: a.str("--render-initial").unwrap_or("none").to_owned(),
                allow_unqualified_runtime: a.flag("--allow-unqualified-runtime"),
                keep_server: a.flag("--keep-server"),
                resume_from: a.str("--resume-from").map(str::to_owned),
                resume_epoch: a.int("--resume-epoch"),
            }),
            "reproduce-epoch" => super::epochs::reproduce_epoch(
                a.str("--from").unwrap_or_default(),
                a.int("--epoch"),
                a.str("--output").unwrap_or_default(),
                a.str("--framework"),
            ),
            "epoch-index" => {
                super::epochs::epoch_index(a.str("--from").unwrap_or_default(), a.str("--output"))
            }
            "replay" => super::replay::main(
                a.str("--actions").unwrap_or_default(),
                a.str("--output").unwrap_or_default(),
                a.str("--base-url").unwrap_or("http://127.0.0.1:8765"),
                f("--timeout", 180.0),
                a.str("--framework"),
            ),
            _ => connect_session(&SessionArgs {
                command: cmd.clone(),
                session: a.str("--session").unwrap_or_default().to_owned(),
                framework: a.str("--framework").map(str::to_owned),
                rpc_timeout: f("--rpc-timeout", 180.0),
                op: a.str("--op").map(str::to_owned),
                allow_unqualified_resume: a.flag("--allow-unqualified-resume"),
                poll: f("--poll", 5.0),
                monitor_timeout: f("--monitor-timeout", 0.0),
                accept: a.flag("--accept"),
                render: a.str("--render").unwrap_or("none").to_owned(),
            }),
        }
    })();
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    }
}

