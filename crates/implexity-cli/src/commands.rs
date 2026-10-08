// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use implexity_server::{COMPATIBILITY_VERSION, RUST_VERSION, ServiceConfig, ViewerSource};

use crate::args::{Exit, Kind, Parser};

struct CommandSpec {
    name: &'static str,
    description: &'static str,
    run: Runner,
}

enum Runner {
    Ported(fn(&[String]) -> u8),
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "model",
        description: "author and inspect implicit geometry",
        run: Runner::Ported(crate::model_cli::main),
    },
    CommandSpec {
        name: "serve",
        description: "run the preview service",
        run: Runner::Ported(cmd_serve),
    },
    CommandSpec {
        name: "doctor",
        description: "check this machine and say why it is not working",
        run: Runner::Ported(crate::doctor::main),
    },
    CommandSpec {
        name: "open",
        description: "start the service if needed and open the viewer",
        run: Runner::Ported(cmd_open),
    },
    CommandSpec {
        name: "version",
        description: "version, backends, optional dependencies",
        run: Runner::Ported(cmd_version),
    },
    CommandSpec {
        name: "check-gradients",
        description: "FD vs AD spot-check on one free design coordinate",
        run: Runner::Ported(cmd_check_gradients),
    },
    CommandSpec {
        name: "study",
        description: "run, watch and replay MCP study plans through the real bridge",
        run: Runner::Ported(crate::study::main),
    },
];

fn usage() -> String {
    let mut body = String::new();
    for c in COMMANDS {
        let _ = writeln!(body, "    {:<16} {}", c.name, c.description);
    }
    format!(
        "Implexity {COMPATIBILITY_VERSION} -- live preview and topology optimisation of an implicit lattice\n\n\
         usage: implexity <command> [options]\n\n{body}\n\
         Every command takes --help.  The service can also be started directly:\n\n    \
         implexity serve --port 8765          the service\n\n\
         Start here:   implexity doctor       then   implexity open\n"
    )
}

pub(crate) fn main(argv: &[String]) -> u8 {
    let Some(cmd) = argv.first() else {
        print!("{}", usage());
        return 0;
    };
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        print!("{}", usage());
        return 0;
    }
    if matches!(cmd.as_str(), "-V" | "--version") {
        println!("Implexity {COMPATIBILITY_VERSION}");
        return 0;
    }
    if cmd == "validate-templates" {

        return crate::templates::main(&argv[1..]);
    }
    if cmd == "worker" {

        implexity_bundle::init();
        if let Err(e) = implexity_server::startup::initialise_kernel() {
            eprintln!("implexity worker: {e}");
            return 2;
        }
        if let Err(e) = implexity_core::packages::global()
            .load_configured(std::env::var("IMPLEXITY_PHYSICS_PACKAGES").ok().as_deref())
        {
            eprintln!("implexity worker: {}: {}", e.python_class(), e.message());
            return 2;
        }
        let manager=implexity_core::packages::global();
        let plugins=manager.descriptors().and_then(|descriptors| {
            implexity_core::plugins::load(std::env::var("IMPLEXITY_PLUGINS").ok().as_deref(),descriptors,implexity_core::registries::global()).map_err(|e|implexity_core::CaeError::contract(e.to_string()))
        });
        if let Err(e)=plugins { eprintln!("implexity worker: {}",e.message());return 2; }
        return u8::try_from(implexity_jobs::worker_cli::main(&argv[1..])).unwrap_or(1);
    }
    let Some(spec) = COMMANDS.iter().find(|c| c.name == cmd) else {
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        eprintln!(
            "implexity: no command '{cmd}'.  The commands are: {}\nTry 'implexity --help'.",
            names.join(", ")
        );
        return 2;
    };
    let Runner::Ported(f) = spec.run;
    f(&argv[1..])
}

fn finish_parse(result: Result<crate::args::Parsed, Exit>) -> Result<crate::args::Parsed, u8> {
    result.map_err(|e| match e {
        Exit::Help(text) => {
            print!("{text}");
            0
        }
        Exit::Error(text) => {
            eprint!("{text}");
            2
        }
    })
}

const SERVE_DESCRIPTION: &str = "Implexity application-agnostic service. Use --workspace for writable state. Load physics add-ins and author an implicit model in the workbench. An explicit --design or IMPLEXITY_DESIGN selects a generic geometry design file.";

fn serve_parser(prog: &str) -> Parser {
    Parser::new(prog, SERVE_DESCRIPTION)
        .opt(
            "--design",
            Kind::Str,
            1,
            "design .npz; default: $IMPLEXITY_DESIGN",
        )
        .opt("--backend", Kind::Str, 1, "geometry backend: 'auto', 'real', 'synthetic', or a registered one")
        .opt("--workspace", Kind::Str, 1, "writable local workspace directory; overrides IMPLEXITY_CASE_DIR")
        .opt("--host", Kind::Str, 1, "bind address (default 127.0.0.1)")
        .opt("--port", Kind::Int, 1, "port (default 8765)")
        .opt("--workers", Kind::Int, 1, "preview worker threads (default 2)")
        .opt("--cache-mb", Kind::Int, 1, "preview result cache budget in MiB (default 192)")
        .opt_meta("--domain", Kind::Float, &["X", "Y", "Z"], "design envelope [mm] (default 8 16 32)")
        .opt("--design-grid", Kind::Int, 3, "design grid (default 32 64 128)")
        .opt("--period", Kind::Float, 1, "the design's lattice period [mm] (default 4.0)")
        .opt(
            "--no-jit",
            Kind::Flag,
            0,
            "accepted for compatibility; the Rust kernel is compiled ahead of time",
        )
        .opt("--warm", Kind::Flag, 0, "pre-evaluate the common section shapes before serving")
}

fn expand_user(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| (p == "~").then_some(""))
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(p)
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn cmd_serve(argv: &[String]) -> u8 {
    serve_main(argv, "implexity serve")
}

fn exit_with_supervisor() {
    if std::env::var("IMPLEXITY_EXIT_ON_STDIN_EOF").as_deref() != Ok("1") {
        return;
    }
    let _ = std::thread::Builder::new().name("implexity-supervisor".into()).spawn(|| {
        let mut sink = [0u8; 256];
        let mut stdin = std::io::stdin().lock();
        while matches!(std::io::Read::read(&mut stdin, &mut sink), Ok(n) if n > 0) {}
        println!("Implexity: the supervising workbench ended; stopping the service.");
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    });
}

pub(crate) fn serve_main(argv: &[String], prog: &str) -> u8 {
    let parser = serve_parser(prog);
    let a = match finish_parse(parser.parse(argv)) {
        Ok(a) => a,
        Err(code) => return code,
    };
    exit_with_supervisor();
    let workspace = match a.str("--workspace").filter(|w| !w.is_empty()) {
        Some(w) => {
            let path = absolute(&expand_user(w));
            if path.exists() && !path.is_dir() {
                if let Exit::Error(t) = parser.error("--workspace must name a directory, not a file") {
                    eprint!("{t}");
                }
                return 2;
            }
            Some(path)
        }
        None => None,
    };
    let backend = a.str("--backend").unwrap_or("auto");
    let ws_queue = match ServiceConfig::ws_queue_from_env() {
        Ok(q) => q,
        Err(e) => {
            eprintln!("{prog}: {e}");
            return 2;
        }
    };
    let host = a.str("--host").unwrap_or("127.0.0.1").to_owned();
    let port = a.ints("--port").map_or(8765, |v| v[0]);
    let Ok(port) = u16::try_from(port) else {
        eprintln!("{prog}: port must be 0-65535");
        return 1;
    };
    let workers = a.ints("--workers").map_or(2, |v| v[0]);
    let cache_mb = a.ints("--cache-mb").map_or(192, |v| v[0]);
    let config = ServiceConfig {
        workers: usize::try_from(workers).unwrap_or(0).max(1),
        cache_mb: u64::try_from(cache_mb).unwrap_or(0),
        ws_queue,
        workspace,
    };
    let geometry = match build_geometry(&a, backend) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{prog}: {e}");
            return 1;
        }
    };
    let viewer = ViewerSource::from_env();
    let source = geometry.design().meta().ok().and_then(|m| m.get("source").cloned());
    let service = match implexity_server::build_geometry_service(&config, viewer.clone(), geometry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{prog}: {e}");
            return 1;
        }
    };
    let source = match source {
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => "None".into(),
    };
    println!("Implexity: backend={} design={source}", service.backend_name());
    let warm = a.flag("--warm");
    let shown_host = host.clone();
    let warm_service = Arc::clone(&service);
    let result = implexity_server::run(&service, &host, port, |bound| {
        println!(
            "Implexity: http://{shown_host}:{}/   (viewer {})",
            bound.port(),
            if viewer.is_present() { "on" } else { "absent" }
        );
        let _ = std::io::stdout().flush();
        if warm {
            let _ = std::thread::Builder::new()
                .name("implexity-warm".into())
                .spawn(move || warm_up(&warm_service));
        }
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{prog}: {e}");
            1
        }
    }
}

fn build_geometry(
    a: &crate::args::Parsed,
    backend: &str,
) -> Result<Arc<implexity_server::geometry::GeometryPreview>, String> {
    implexity_bundle::init();
    implexity_server::startup::initialise_kernel()?;
    let manager = implexity_core::packages::global();
    manager
        .load_configured(std::env::var("IMPLEXITY_PHYSICS_PACKAGES").ok().as_deref())
        .map_err(|e| format!("{}: {}", e.python_class(), e.message()))?;
    let design_path = a.str("--design").filter(|s| !s.is_empty()).map(PathBuf::from)
        .or_else(|| std::env::var_os("IMPLEXITY_DESIGN").filter(|v| !v.is_empty()).map(PathBuf::from));
    let descriptors = manager.descriptors().map_err(|e| e.message().to_string())?;
    implexity_core::plugins::load(
        std::env::var("IMPLEXITY_PLUGINS").ok().as_deref(),
        descriptors,
        implexity_core::registries::global(),
    )
    .map_err(|e| e.to_string())?;
    for line in implexity_core::plugins::describe() {
        println!("Implexity: {line}");
    }
    let name = if backend == "auto" && design_path.is_none() { "synthetic" } else { backend };
    let be =
        implexity_core::backends::geometry(Some(name), design_path.as_deref()).map_err(|e| e.to_string())?;
    let floats = |k: &str, d: [f64; 3]| {
        a.floats(k).map_or(d, |v| std::array::from_fn(|i| v.get(i).copied().unwrap_or(d[i])))
    };
    let grid = a.ints("--design-grid").map_or([32, 64, 128], |v| {
        std::array::from_fn(|i| usize::try_from(v.get(i).copied().unwrap_or(1)).unwrap_or(1))
    });
    let options = implexity_core::backends::GeometryBuildOptions {
        design_path,
        domain_mm: floats("--domain", [8.0, 16.0, 32.0]),
        design_grid: grid,
        period_mm: a.floats("--period").map_or(4.0, |v| v[0]),
        use_jit: !a.flag("--no-jit"),
    };
    let build = be.build(&options)?;
    Ok(Arc::new(implexity_server::geometry::GeometryPreview::from_build(build)?))
}

fn warm_up(service: &Arc<implexity_server::Service>) {
    let t0 = Instant::now();
    for lod in ["drag", "settle", "read"] {
        for (normal, w, h) in [([0, 0, 1], 8, 16), ([1, 0, 0], 16, 32), ([0, 1, 0], 8, 32)] {
            let req = serde_json::json!({"normal": normal, "width_mm": w, "height_mm": h, "lod": lod,
                                         "want": ["contours"], "channel": "warm"});
            if let Ok(job) =
                implexity_server::ws::submit(service, implexity_server::preview::PreviewOp::Section, &req)
            {
                let _ = job.wait(Duration::from_mins(5));
            }
        }
    }
    println!("Implexity: warm-up done in {:.1}s", t0.elapsed().as_secs_f64());
    let _ = std::io::stdout().flush();
}

fn health(host: &str, port: i64, timeout: Duration) -> Option<Value> {
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .http_status_as_error(true)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent.get(format!("http://{host}:{port}/v1/health")).call().ok()?;
    let text = response.body_mut().read_to_string().ok()?;
    serde_json::from_str(&text).ok()
}

fn state_dir() -> PathBuf {
    implexity_server::service::state_directory(None)
}

fn tail(path: &Path, n: usize) {
    let Ok(text) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    if lines.is_empty() {
        return;
    }
    eprintln!("--- last {} lines of {} ---", lines.len() - start, path.display());
    for line in &lines[start..] {
        eprintln!("  {line}");
    }
}

fn open_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {

        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach(_cmd: &mut Command) {}

fn cmd_open(argv: &[String]) -> u8 {
    let parser = Parser::new(
        "implexity open",
        "start the service if it is not already up, then open the viewer in a browser",
    )
    .opt("--host", Kind::Str, 1, "service address (default 127.0.0.1)")
    .opt("--port", Kind::Int, 1, "service port (default 8765)")
    .opt("--backend", Kind::Str, 1, "passed to the service if one has to be started")
    .opt("--bundle", Kind::Str, 1, "passed to the service if one has to be started")
    .opt("--tree", Kind::Str, 1, "the unpacked implexity tree")
    .opt("--timeout", Kind::Float, 1, "seconds to wait for the service to answer /v1/health (default 90)")
    .opt("--no-browser", Kind::Flag, 0, "print the URL instead of opening it");
    let a = match finish_parse(parser.parse(argv)) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let host = a.str("--host").unwrap_or("127.0.0.1").to_owned();
    let port = a.ints("--port").map_or(8765, |v| v[0]);
    let timeout = a.floats("--timeout").map_or(90.0, |v| v[0]);
    let url = format!("http://{host}:{port}/");
    if let Some(h) = health(&host, port, Duration::from_millis(1500)) {
        println!(
            "implexity: a service is already answering on {url} (backend {})",
            h.get("backend").map_or_else(
                || "None".to_owned(),
                |b| b.as_str().map_or_else(|| b.to_string(), str::to_owned)
            )
        );
    } else {
        let rc = start_service(&a, &host, port, timeout);
        if rc != 0 {
            return rc;
        }
    }
    if a.flag("--no-browser") {
        println!("implexity: {url}");
        return 0;
    }
    let ok = open_browser(&url);
    println!(
        "implexity: {url}  ({})",
        if ok { "opened in your browser" } else { "no browser could be launched -- open it yourself" }
    );
    0
}

fn start_service(a: &crate::args::Parsed, host: &str, port: i64, timeout: f64) -> u8 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("implexity: could not start the service: {e}");
            return 4;
        }
    };
    let mut args: Vec<String> =
        vec!["serve".into(), "--host".into(), host.into(), "--port".into(), port.to_string()];
    if let Some(b) = a.str("--backend") {
        args.extend(["--backend".into(), b.into()]);
    }
    if let Some(b) = a.str("--bundle") {
        args.extend(["--bundle".into(), b.into()]);
    }
    let dir = state_dir();
    let log = dir.join(format!("implexity.{port}.log"));
    println!("implexity: starting the service -- {} {}", exe.display(), args.join(" "));
    println!("implexity: log {}", log.display());
    let spawned = (|| -> std::io::Result<u32> {
        std::fs::create_dir_all(&dir)?;
        let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
        let err = out.try_clone()?;
        let mut cmd = Command::new(&exe);
        cmd.args(&args).stdin(Stdio::null()).stdout(out).stderr(err);
        detach(&mut cmd);
        let child = cmd.spawn()?;
        Ok(child.id())
    })();
    match spawned {
        Ok(pid) => {
            let pidf = dir.join(format!("implexity.{port}.pid"));
            if std::fs::write(&pidf, pid.to_string()).is_ok() {
                println!("implexity: pid {pid} ({})", pidf.display());
            }
        }
        Err(e) => {
            eprintln!("implexity: could not start the service: {e}");
            return 4;
        }
    }
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f64() < timeout {
        if let Some(h) = health(host, port, Duration::from_millis(1500)) {
            println!(
                "implexity: up in {:.1}s -- backend {}",
                t0.elapsed().as_secs_f64(),
                h.get("backend").and_then(Value::as_str).unwrap_or("None")
            );
            return 0;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!("implexity: the service did not answer /v1/health within {timeout:.0}s.");
    tail(&log, 25);
    5
}

#[allow(clippy::too_many_lines)]
fn cmd_version(argv: &[String]) -> u8 {
    let parser = Parser::new("implexity version", "version, backends, optional dependencies").opt(
        "--json",
        Kind::Flag,
        0,
        "",
    );
    let a = match finish_parse(parser.parse(argv)) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let root = crate::doctor::tree_root(None);
    let warn = crate::doctor::load_registries();
    let geo = crate::doctor::geometry_backends(None);
    let phys = crate::doctor::physics_backends();
    let viewer = ViewerSource::from_env();
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let platform = format!(
        "{} {}",
        implexity_core::runtime_environment::platform_system(),
        implexity_core::runtime_environment::platform_machine()
    );
    let mut deps = serde_json::Map::new();
    for (label, krate, role) in crate::doctor::LINKED {
        let v = crate::doctor::locked_version(krate);
        deps.insert(
            (*label).to_owned(),
            json!({"present": v.is_some(), "detail": v.unwrap_or_else(|| "not in Cargo.lock".into()), "extra": null, "role": role}),
        );
    }
    for (label, extra, native) in crate::doctor::REPLACED {
        deps.insert(
            (*label).to_owned(),
            json!({"present": false, "detail": format!("not needed: {native}"), "extra": extra, "replaced_natively": true}),
        );
    }
    let fmts = implexity_mesh::exporters::catalogue();
    let info = json!({
        "version": COMPATIBILITY_VERSION,
        "rust_version": RUST_VERSION,
        "implementation": "rust",
        "python": null,
        "executable": exe,
        "platform": platform,
        "tree": root.as_ref().map(|r| r.display().to_string()),
        "viewer": viewer.describe(),
        "state": state_dir().display().to_string(),
        "geometry_backends": geo.iter().map(|(n, p, ok, d)| json!({"name": n, "priority": p, "available": ok, "detail": d})).collect::<Vec<_>>(),
        "physics_backends": phys.iter().map(|(n, ok, d)| json!({"name": n, "available": ok, "detail": d})).collect::<Vec<_>>(),
        "dependencies": deps,
        "formats": fmts,
        "plugin_warnings": warn,
    });
    if a.flag("--json") {
        println!("{}", crate::util::dumps_indent(&info));
        return 0;
    }
    println!("implexity {COMPATIBILITY_VERSION}");
    println!("  build        Rust port {RUST_VERSION}  ({exe})");
    println!("  platform     {platform}");
    println!(
        "  tree         {}",
        root.map_or_else(|| "(not found -- set $IMPLEXITY_HOME)".into(), |r| r.display().to_string())
    );
    println!("  viewer       {}", viewer.describe());
    println!("  state        {}", state_dir().display());
    println!();
    println!("  geometry backends (--backend, 'auto' takes the highest priority that is available)");
    for (name, prio, ok, detail) in &geo {
        println!(
            "    {name:<10} prio {prio:<3} {:<13} {}",
            if *ok { "AVAILABLE" } else { "unavailable" },
            crate::util::clip(detail, 44)
        );
    }
    println!("  physics backends (IMPLEXITY_PHYSICS_BACKEND)");
    for (name, ok, detail) in &phys {
        println!(
            "    {name:<10} {:<18} {}",
            if *ok { "AVAILABLE" } else { "unavailable" },
            crate::util::clip(detail, 48)
        );
    }
    println!();
    println!("  dependencies (compiled in)");
    for (label, krate, _) in crate::doctor::LINKED {
        let v = crate::doctor::locked_version(krate).unwrap_or_else(|| "?".into());
        println!("    {label:<14} {:<9} {}", "present", crate::util::clip(&v, 40));
    }
    println!("  optional Python extras (replaced natively)");
    for (label, extra, native) in crate::doctor::REPLACED {
        println!("    {label:<14} {:<9} {}  [{extra}]", "native", crate::util::clip(native, 40));
    }
    println!();
    println!("  export formats (POST /v1/body)");
    for f in &fmts {
        let s = |k: &str| f[k].as_str().unwrap_or_default().to_owned();
        println!(
            "    {:<6} .{:<5} {:<13} {}",
            s("format"),
            s("extension"),
            if f["available"].as_bool() == Some(true) { "available" } else { "UNAVAILABLE" },
            f["requires"].as_str().unwrap_or("standard library")
        );
    }
    for w in &info["plugin_warnings"].as_array().cloned().unwrap_or_default() {
        println!("\n  PLUGIN WARNING (a service start would fail): {}", w.as_str().unwrap_or_default());
    }
    0
}

fn cmd_check_gradients(argv: &[String]) -> u8 {
    let parser = Parser::new(
        "implexity check-gradients",
        "finite-difference vs AD on one free design coordinate of the start design",
    )
    .opt(
        "--spec-file",
        Kind::Str,
        1,
        "the optimisation job spec (document, node, free, objective, constraints, case) -- the same file \
         `implexity worker optimize --spec-file` takes",
    )
    .opt("--slot", Kind::Str, 1, "free-parameter slot to probe (default: the first free entry)")
    .opt("--index", Kind::Int, 1, "flat index inside the slot when it is an array (default 0)")
    .opt(
        "--step",
        Kind::Float,
        1,
        "central-difference step, relative to max(1, |x|) (default: eps**(1/3) of the loss dtype, ~6e-6 in float64)",
    )
    .opt(
        "--tol",
        Kind::Float,
        1,
        "acceptable relative error |AD-FD|/max(|AD|,|FD|) (default 1e-3; a central difference at eps**(1/3) is \
         itself only good to ~eps**(2/3))",
    )
    ;
    let a = match finish_parse(parser.parse(argv)) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let fail = |message: &str| -> u8 {
        match parser.error(message) {
            Exit::Error(text) | Exit::Help(text) => eprint!("{text}"),
        }
        2
    };
    let Some(spec_file) = a.str("--spec-file") else {
        return fail("the following arguments are required: --spec-file");
    };
    let tol = a.floats("--tol").map_or(1e-3, |v| v[0]);
    if !tol.is_finite() || tol <= 0.0 {
        return fail("--tol must be finite and positive");
    }
    let step = a.floats("--step").map(|v| v[0]);
    if step.is_some_and(|s| !s.is_finite() || s <= 0.0) {
        return fail("--step must be finite and positive");
    }
    let index = a.ints("--index").map_or(0, |v| v[0]);
    let code = implexity_jobs::optimize::gradcheck::run_check(spec_file, a.str("--slot"), index, step, tol);
    u8::try_from(code).unwrap_or(1)
}

