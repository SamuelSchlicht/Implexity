// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



mod client;
mod epochs;
mod replay;
mod continuation_plan;
mod run;

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

pub(crate) use client::McpClient;

pub(crate) const VERSION: &str = "1.1.0";
pub(crate) const PROTOCOL: &str = "2025-11-25";
pub(crate) const PLAN_SCHEMA: &str = "implexity-mcp-study-plan/1";
pub(crate) const SESSION_SCHEMA: &str = "implexity-local-mcp-session/1";
pub(crate) const MAX_MESSAGE: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PLAN: u64 = 32 * 1024 * 1024;
pub(crate) const TERMINAL: [&str; 8] =
    ["completed", "failed", "cancelled", "canceled", "stopped", "accepted", "discarded", "error"];
pub(crate) const AUTHOR_ACTIONS: [&str; 5] = [
    "load_physics_package",
    "import_model",
    "set_engineering_problem",
    "set_intent",
    "author_application_case",
];
pub(crate) const MONITOR_POLL_TOOLS: [&str; 1] = ["implexity_inspect_optimization_job"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RunnerError {
    Runner(String),
    Action { action: String, response: Value },
    Ambiguous(String),
    Io(String),
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runner(m) | Self::Ambiguous(m) | Self::Io(m) => f.write_str(m),
            Self::Action { action, response } => {
                let text = implexity_core::pyobj::py_str(response);
                write!(
                    f,
                    "Public action {} failed: {}",
                    implexity_core::py_repr::repr_str(action),
                    text.chars().take(1800).collect::<String>()
                )
            }
        }
    }
}

impl RunnerError {
    pub(crate) fn class(&self) -> &'static str {
        match self {
            Self::Runner(_) => "RunnerError",
            Self::Action { .. } => "ActionError",
            Self::Ambiguous(_) => "AmbiguousRequest",
            Self::Io(_) => "OSError",
        }
    }
}

impl From<std::io::Error> for RunnerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub(crate) type RResult<T> = Result<T, RunnerError>;

pub(crate) fn rerr<T>(m: impl Into<String>) -> RResult<T> {
    Err(RunnerError::Runner(m.into()))
}

pub(crate) fn strict_loads(raw: &[u8]) -> RResult<Value> {
    implexity_core::json::parse_strict_bytes(raw)
        .map_err(|e| RunnerError::Runner(format!("Invalid strict UTF-8 JSON: {e}")))
}

pub(crate) fn encoded(v: &Value) -> Vec<u8> {
    let opts = implexity_core::json::DumpOptions::indented(2).sorted(true).ascii(false);
    let mut s = implexity_core::json::dumps(v, &opts);
    s.push('\n');
    s.into_bytes()
}

pub(crate) fn canonical_hash(v: &Value) -> String {
    let opts = implexity_core::json::DumpOptions::canonical().ascii(false);
    implexity_core::json::sha256_hex(implexity_core::json::dumps(v, &opts).as_bytes())
}

pub(crate) fn dumps_line(v: &Value) -> String {
    implexity_core::json::dumps(v, &implexity_core::json::DumpOptions::default())
}

pub(crate) fn sha256_file(path: &Path) -> RResult<String> {
    implexity_io::digest::sha256_file(path).map_err(|e| RunnerError::Io(format!("{}: {e}", path.display())))
}

pub(crate) fn write_json(path: &Path, value: &Value, replace: bool) -> RResult<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = encoded(value);
    if replace {
        let tmp = path.with_file_name(format!(
            "{}.{}.tmp",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            implexity_io::atomic::unique_token()
        ));
        let result = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&raw)?;
            f.flush()?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        let _ = std::fs::remove_file(&tmp);
        result?;
    } else {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        f.write_all(&raw)?;
        f.flush()?;
        f.sync_all()?;
    }
    Ok(())
}

pub(crate) fn append_line(path: &Path, line: &str, sync: bool) -> RResult<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    if sync {
        f.flush()?;
        f.sync_all()?;
    }
    Ok(())
}

pub(crate) fn read_json(path: &Path, limit: u64) -> RResult<Value> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= limit => strict_loads(&std::fs::read(path)?),
        _ => rerr(format!("Missing or oversized JSON file: {}", path.display())),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Framework {
    pub(crate) root: PathBuf,
    pub(crate) service: PathBuf,
    pub(crate) bridge: PathBuf,
}

pub(crate) fn framework_root(value: Option<&str>) -> RResult<Framework> {
    let root = match value {
        Some(v) => crate::util::expand_abs(v),
        None => crate::util::exe_dir()
            .ok_or_else(|| RunnerError::Runner("the running executable cannot be located".into()))?,
    };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let exe = |name: &str| root.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let (service, bridge) = (exe("implexity"), exe("implexity-mcp"));
    if !service.is_file() || !bridge.is_file() {
        return rerr(format!(
            "--framework must point to the directory containing the implexity and implexity-mcp executables ({} does not).",
            root.display()
        ));
    }
    Ok(Framework { root, service, bridge })
}

pub(crate) fn source_manifest(fw: &Framework) -> RResult<Value> {
    let mut files = Map::new();
    let mut names: Vec<PathBuf> = vec![fw.service.clone(), fw.bridge.clone()];
    let egl = fw.root.join(format!("implexity-egl-worker{}", std::env::consts::EXE_SUFFIX));
    if egl.is_file() {
        names.push(egl);
    }
    for p in &names {
        let rel = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        files.insert(rel, json!(sha256_file(p)?));
    }
    let client = std::env::current_exe().map_err(RunnerError::from)?;
    let client_sha = sha256_file(&client)?;
    let sha = canonical_hash(&json!({"files": files, "client_sha256": client_sha}));
    Ok(json!({
        "schema": "implexity-local-source-fingerprint/1", "files": files, "client_sha256": client_sha,
        "sha256": sha, "scope": "service, MCP bridge and capture executables (the viewer is embedded in the service) and the executing client",
    }))
}

pub(crate) fn check_source(fw: &Framework, expected: &Value) -> RResult<Value> {
    let current = source_manifest(fw)?;
    if current["sha256"] != expected["sha256"] {
        return rerr(
            "Runtime source changed after this session began. No acceptance or rendering is permitted from mixed source; stop and review the run.",
        );
    }
    Ok(current)
}

pub(crate) fn origin(value: &str) -> RResult<(String, String, u16)> {
    let bad = || RunnerError::Runner("Use a literal loopback HTTP origin, e.g. http://127.0.0.1:8765".into());
    let refuse =
        || RunnerError::Runner("Only credential-free, literal loopback HTTP origins are allowed.".into());
    let Some((scheme, rest)) = value.split_once("://") else { return Err(bad()) };
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err(refuse());
    }
    let (host, port) = if let Some(h) = authority.strip_prefix('[') {
        let (h, after) = h.split_once(']').ok_or_else(bad)?;
        (h.to_owned(), after.strip_prefix(':').map(str::to_owned))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p.to_owned())),
            None => (authority.to_owned(), None),
        }
    };
    let address: std::net::IpAddr = host.parse().map_err(|_| bad())?;
    let port: u32 = match port {
        Some(p) => p.parse().map_err(|_| bad())?,
        None => 80,
    };
    if !scheme.eq_ignore_ascii_case("http")
        || !address.is_loopback()
        || !(tail.is_empty() || tail == "/")
        || !(1..=65535).contains(&port)
    {
        return Err(refuse());
    }
    let port = u16::try_from(port).map_err(|_| bad())?;
    let host = address.to_string();
    let shown = if address.is_ipv6() { format!("[{host}]") } else { host.clone() };
    Ok((format!("http://{shown}:{port}"), host, port))
}

pub(crate) fn free_port() -> RResult<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(l.local_addr()?.port())
}

fn canonical_default(v: &Value) -> String {
    implexity_core::json::canonical(v)
}

pub(crate) fn check_application_case(result: &Value, optimization: &Value) -> RResult<()> {
    let execution = result.get("execution").filter(|e| e.is_object());
    let status = execution.and_then(|e| e.get("status")).and_then(Value::as_str);
    if execution.is_none() || !matches!(status, Some("completed" | "stopped_before:preflight_optimization")) {
        let text = implexity_core::json::dumps(
            execution.unwrap_or(&Value::Null),
            &implexity_core::json::DumpOptions::default(),
        );
        return rerr(format!(
            "Application case authoring did not complete: {}",
            text.chars().take(1500).collect::<String>()
        ));
    }
    let steps = execution.and_then(|e| e.get("steps")).and_then(Value::as_array).cloned().unwrap_or_default();
    if !steps.iter().all(|s| s.is_object() && s.get("ok") == Some(&json!(true))) {
        return rerr("An application case authoring step failed.");
    }
    let actions: Vec<&str> = steps.iter().filter_map(|s| s.get("action").and_then(Value::as_str)).collect();
    if actions.iter().filter(|a| **a == "import_model").count() != 1
        || !actions.contains(&"set_engineering_problem")
    {
        return rerr("An application case must import exactly one model and author the engineering problem.");
    }
    let output = result.get("output").cloned().unwrap_or(Value::Null);
    let authored_problem =
        output.get("engineering_problem").and_then(|e| e.get("problem")).cloned().unwrap_or(Value::Null);
    let authored_physics =
        output.get("optimization_request").and_then(|e| e.get("physics")).cloned().unwrap_or(Value::Null);
    let physics = optimization.get("physics").cloned().unwrap_or(Value::Null);
    if canonical_default(&authored_problem)
        != canonical_default(physics.get("problem").unwrap_or(&Value::Null))
        || canonical_default(&authored_physics) != canonical_default(&physics)
    {
        return rerr("Application case problem and optimization problem differ; refusing split authority.");
    }
    Ok(())
}

fn scene_name_ok(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub(crate) fn validate_plan(value: &Value) -> RResult<Value> {
    const KEYS: [&str; 8] =
        ["schema", "name", "description", "provenance", "authoring", "optimization", "scenes", "limitations"];
    let Some(obj) = value.as_object() else { return rerr("Unexpected study-plan schema/keys.") };
    if obj.keys().any(|k| !KEYS.contains(&k.as_str()))
        || obj.get("schema").and_then(Value::as_str) != Some(PLAN_SCHEMA)
    {
        return rerr("Unexpected study-plan schema/keys.");
    }
    if obj.get("name").and_then(Value::as_str).is_none_or(|n| n.trim().is_empty()) {
        return rerr("Plan requires a nonempty name.");
    }
    let actions = match obj.get("authoring").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return rerr("Plan requires explicit public authoring actions."),
    };
    for a in &actions {
        let ok = a.as_object().is_some_and(|m| {
            m.len() == 2
                && m.get("action").and_then(Value::as_str).is_some_and(|x| AUTHOR_ACTIONS.contains(&x))
                && m.get("payload").is_some_and(Value::is_object)
        });
        if !ok {
            return rerr("Authoring supports only explicit package/model/problem/intent actions.");
        }
    }
    let name_of = |a: &Value| a["action"].as_str().unwrap_or_default().to_owned();
    let cases: Vec<&Value> = actions.iter().filter(|a| name_of(a) == "author_application_case").collect();
    if cases.is_empty() {
        if actions.iter().filter(|a| name_of(a) == "import_model").count() != 1 {
            return rerr("Plan must import exactly one authoritative model document.");
        }
        if !actions.iter().any(|a| name_of(a) == "set_engineering_problem") {
            return rerr("Plan must author an engineering problem through the public action.");
        }
    } else {
        let p = &cases[0]["payload"];
        let stop = p.get("stop_before").cloned().unwrap_or_else(|| json!("preflight_optimization"));
        if cases.len() != 1
            || p.get("execute") != Some(&json!(true))
            || stop != json!("preflight_optimization")
            || actions
                .iter()
                .any(|a| matches!(name_of(a).as_str(), "import_model" | "set_engineering_problem"))
        {
            return rerr(
                "An application case must be the single executed model/problem authoring action (execute=true, stop_before=preflight_optimization).",
            );
        }
    }
    let Some(optimization) = obj.get("optimization").filter(|o| o.get("physics").is_some() && o.is_object())
    else {
        return rerr("Plan requires an explicit native optimization payload.");
    };
    if !optimization.get("settings").is_some_and(Value::is_object) {
        return rerr("Plan must explicitly specify optimizer settings.");
    }
    for a in &actions {
        if name_of(a) == "set_engineering_problem"
            && let Some(provider) = a["payload"].get("provider")
        {
            let physics = &optimization["physics"];
            if Some(provider) != physics.get("provider")
                || a["payload"].get("problem") != physics.get("problem")
            {
                return rerr("Authored problem and optimization problem differ; refusing split authority.");
            }
        }
    }
    let scenes = obj.get("scenes").cloned().unwrap_or_else(|| json!([]));
    let Some(scenes) = scenes.as_array().filter(|s| s.len() <= 16) else {
        return rerr("scenes must contain at most sixteen native scene specifications.");
    };
    let mut names = std::collections::BTreeSet::new();
    for s in scenes {
        let ok = s.as_object().is_some_and(|m| {
            m.len() == 2
                && m.get("name").and_then(Value::as_str).is_some_and(scene_name_ok)
                && m.get("scene").is_some_and(Value::is_object)
        });
        let name = s.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        if !ok || names.contains(&name) {
            return rerr("Invalid, repeated or unsafe scene name/specification.");
        }
        names.insert(name);
    }
    strict_loads(&encoded(value))
}

pub(crate) fn effective_plan(
    plan: &Value,
    iterations: Option<i64>,
    budget: Option<f64>,
) -> RResult<(Value, Vec<Value>)> {
    let mut result = plan.clone();
    let mut edits = Vec::new();
    if let Some(it) = iterations {
        if !(1..=100_000).contains(&it) {
            return rerr("Iterations must be a positive integer <= 100000.");
        }
        if let Some((stage, before, after)) = continuation_plan::extend_terminal_stage(&mut result, it)
            .map_err(RunnerError::Runner)?
        {
            edits.push(json!({"path": format!("/optimization/schedule/{stage}/iterations"), "before": before, "after": after}));
        }
        let settings = &mut result["optimization"]["settings"];
        edits.push(json!({"path": "/optimization/settings/iterations", "before": settings.get("iterations").cloned().unwrap_or(Value::Null), "after": it}));
        settings["iterations"] = json!(it);
    }
    if let Some(b) = budget {
        if !b.is_finite() || !(1.0..=14400.0).contains(&b) {
            return rerr("Native wall budget must be 1..14400 seconds.");
        }
        let opt = result["optimization"]
            .as_object_mut()
            .ok_or_else(|| RunnerError::Runner("optimization must be an object".into()))?;
        let effort = opt.entry("computation_effort").or_insert_with(|| json!({"preset": "exact"}));
        let budgets = effort
            .as_object_mut()
            .ok_or_else(|| RunnerError::Runner("computation_effort must be an object".into()))?
            .entry("hard_budgets")
            .or_insert_with(|| json!({}));
        edits.push(json!({"path": "/optimization/computation_effort/hard_budgets/wall_time_s",
                          "before": budgets.get("wall_time_s").cloned().unwrap_or(Value::Null), "after": b}));
        budgets["wall_time_s"] = implexity_geometry::value::json_f64(b);
    }
    Ok((result, edits))
}

pub(crate) fn child_env(case_dir: Option<&Path>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> =
        std::env::vars().filter(|(k, _)| !k.starts_with("IMPLEXITY_")).collect();
    let set = |env: &mut Vec<(String, String)>, k: &str, v: String| {
        env.retain(|(x, _)| x != k);
        env.push((k.to_owned(), v));
    };
    set(&mut env, "IMPLEXITY_PLUGINS", String::new());
    set(&mut env, "IMPLEXITY_PHYSICS_PACKAGES", String::new());
    for key in ["NO_PROXY", "no_proxy"] {
        let prior = std::env::var(key).unwrap_or_default();
        let joined: Vec<&str> =
            [prior.as_str(), "localhost", "127.0.0.1", "::1"].into_iter().filter(|x| !x.is_empty()).collect();
        set(&mut env, key, joined.join(","));
    }
    if let Some(c) = case_dir {
        set(&mut env, "IMPLEXITY_CASE_DIR", c.display().to_string());
    }
    env
}

pub(crate) fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0000_0200);
    }
}

pub(crate) fn terminate_process(child: &mut std::process::Child, timeout: std::time::Duration) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    #[cfg(unix)]
    {
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_child(child),
            rustix::process::Signal::TERM,
        );
    }
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn seconds(value: f64) -> std::time::Duration {
    implexity_jobs::worker_cli::duration_from_secs(value)
}

pub(crate) fn is_monitor_poll(method: &str, params: &Value) -> bool {
    method == "tools/call"
        && params.get("name").and_then(Value::as_str).is_some_and(|n| MONITOR_POLL_TOOLS.contains(&n))
        && params.get("arguments").and_then(|a| a.get("view")).and_then(Value::as_str) == Some("monitor")
}

pub(crate) fn main(argv: &[String]) -> u8 {
    run::main(argv)
}

