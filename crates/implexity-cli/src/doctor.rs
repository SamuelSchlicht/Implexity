// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::args::{Exit, Kind, Parser};
use crate::util::{getenv, oneline, textwrap, which};

pub(crate) const MIN_DISK_GB: f64 = 2.0;
pub(crate) const MIN_FREE_MEM_GB: f64 = 2.0;
const TREE_MARKERS: [&str; 1] = ["viewer"];

const CARGO_LOCK: &str = include_str!("../../../Cargo.lock");

pub(crate) const LINKED: &[(&str, &str, &str)] = &[
    ("faer", "faer", "sparse/dense linear algebra (replaces SciPy SuperLU/LAPACK)"),
    ("ndarray", "ndarray", "N-d arrays (replaces NumPy arrays)"),
    ("rayon", "rayon", "deterministic data parallelism"),
    ("serde_json", "serde_json", "JSON documents and wire formats"),
    ("tokio", "tokio", "the HTTP/WebSocket service runtime"),
    ("axum", "axum", "HTTP routing transport"),
    ("flate2", "flate2", "npz/3MF/PNG compression (Rust backend)"),
];

pub(crate) const REPLACED: &[(&str, &str, &str)] = &[
    (
        "scikit-image",
        "mesh",
        "native Lewiner marching cubes (implexity-mesh::mc); POST /v1/slab and body export always mesh",
    ),
    ("trimesh", "mesh", "native STL/OBJ/PLY readers (binary .ply domain uploads included)"),
    ("lib3mf", "3mf", "native 3MF writer and reader (implexity-mesh::formats)"),
    ("jax", "physics", "implexity-ad forward duals and reverse tapes (exact derivatives)"),
    ("jaxlib", "physics", "same as jax -- compiled ahead of time into this executable"),
    ("optax", "physics", "implexity-optim Adam with the optax state layout"),
    ("pymetis", "sparse", "faer COLAMD/AMD orderings (METIS is not linked)"),
];

#[must_use]
pub(crate) fn locked_version(name: &str) -> Option<String> {
    let mut current: Option<&str> = None;
    for line in CARGO_LOCK.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            current = None;
        } else if let Some(n) = line.strip_prefix("name = ") {
            current = Some(n.trim_matches('"'));
        } else if let Some(v) = line.strip_prefix("version = ")
            && current == Some(name)
        {
            return Some(v.trim_matches('"').to_owned());
        }
    }
    None
}

fn looks_like_tree(path: &Path) -> bool {
    TREE_MARKERS.iter().all(|m| path.join(m).is_dir())
}

#[must_use]
pub(crate) fn tree_root(explicit: Option<&str>) -> Option<PathBuf> {
    for cand in [explicit.map(str::to_owned), getenv("IMPLEXITY_HOME")].into_iter().flatten() {
        if !cand.is_empty() {
            return Some(implexity_io::locate::abspath(Path::new(&cand)));
        }
    }
    for start in [crate::util::exe_dir(), std::env::current_dir().ok()].into_iter().flatten() {
        let mut d = implexity_io::locate::abspath(&start);
        loop {
            if looks_like_tree(&d) {
                return Some(d);
            }
            match d.parent() {
                Some(p) if p != d => d = p.to_path_buf(),
                _ => break,
            }
        }
    }
    implexity_io::bundle_resources::installed_resource_root()
}

#[must_use]
pub(crate) fn state_dir(workspace: Option<&Path>) -> PathBuf {
    workspace.map_or_else(implexity_io::bundle_resources::state_directory, Path::to_path_buf)
}

#[must_use]
pub(crate) fn port_free(host: &str, port: u16) -> (bool, String) {
    use std::net::{TcpListener, TcpStream, ToSocketAddrs};
    let addrs: Vec<std::net::SocketAddr> =
        (host, port).to_socket_addrs().map(Iterator::collect).unwrap_or_default();
    if addrs.iter().any(|a| TcpStream::connect_timeout(a, std::time::Duration::from_secs(1)).is_ok()) {
        return (false, format!("something is already listening on {host}:{port}"));
    }
    match TcpListener::bind((host, port)) {
        Ok(_) => (true, format!("bind to {host}:{port} succeeded")),
        Err(e) => (false, format!("cannot bind {host}:{port}: {e}")),
    }
}

#[must_use]
pub(crate) fn health(host: &str, port: i64, timeout: std::time::Duration) -> Option<Value> {
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

#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) results: Vec<Value>,
    pub(crate) quiet: bool,
}

impl Report {
    fn emit(&mut self, status: &str, name: &str, detail: &str) {
        self.results.push(json!({"status": status, "check": name, "detail": detail}));
        if self.quiet {
            return;
        }
        let tag = match status {
            "ok" => "  ok  ",
            "warn" => " WARN ",
            _ => " FAIL ",
        };
        println!("{tag} {name}");
        if !detail.is_empty() {
            let mut lines = textwrap(detail, 66);
            if lines.is_empty() {
                lines.push(String::new());
            }
            for line in lines {
                println!("         {line}");
            }
        }
    }

    pub(crate) fn ok(&mut self, name: &str, detail: &str) {
        self.emit("ok", name, detail);
    }

    pub(crate) fn warn(&mut self, name: &str, detail: &str) {
        self.emit("warn", name, detail);
    }

    pub(crate) fn fail(&mut self, name: &str, detail: &str) {
        self.emit("fail", name, detail);
    }

    pub(crate) fn either(&mut self, good: bool, bad_status: &str, name: &str, detail: &str) {
        self.emit(if good { "ok" } else { bad_status }, name, detail);
    }

    #[must_use]
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        let n = |s: &str| self.results.iter().filter(|r| r["status"] == s).count();
        (n("fail"), n("warn"), n("ok"))
    }
}

fn check_build(r: &mut Report) {
    let exe = std::env::current_exe().map_or_else(|e| format!("unknown ({e})"), |p| p.display().to_string());
    r.ok(
        "build",
        &format!(
            "implexity Rust build {} (Python compatibility {}) for {} {} ({exe})",
            implexity_core::RUST_VERSION,
            implexity_core::PYTHON_COMPATIBILITY_VERSION,
            implexity_core::runtime_environment::platform_system(),
            implexity_core::runtime_environment::platform_machine()
        ),
    );
}

fn check_linkage(r: &mut Report) {
    let pinned: Vec<String> = LINKED
        .iter()
        .map(|(label, krate, _)| format!("{label} {}", locked_version(krate).unwrap_or_else(|| "?".into())))
        .collect();
    r.ok(
        "linkage",
        &format!(
            "numerical and service libraries are compiled into this executable and pinned by Cargo.lock ({}); no interpreter, virtual environment or site-packages takes part",
            pinned.join(", ")
        ),
    );
}

fn check_install(r: &mut Report) {
    match std::env::current_exe() {
        Ok(p) => r.ok(
            "install",
            &format!("implexity {} at {}", implexity_core::PYTHON_COMPATIBILITY_VERSION, p.display()),
        ),
        Err(e) => r.warn("install", &format!("the running executable cannot be located: {e}")),
    }
    match which("implexity") {
        Some(p) => r.ok("on-path", &p.display().to_string()),
        None => r.warn(
            "on-path",
            "no 'implexity' on PATH. Run it by its full path, or add its directory to PATH; `implexity serve` and the packaging scripts work regardless.",
        ),
    }
    for (name, role) in [
        ("implexity-mcp", "the MCP stdio bridge (study runner, agent clients)"),
        (
            "implexity-egl-worker",
            "the EGL offscreen capture worker (render_viewer_snapshot backend egl_offscreen)",
        ),
    ] {
        match crate::util::sibling_executable(name) {
            Some(p) => r.ok(&format!("executable:{name}"), &format!("{} -- {role}", p.display())),
            None => r.warn(
                &format!("executable:{name}"),
                &format!("not found beside this executable; without it: {role} is unavailable"),
            ),
        }
    }
}

fn check_tree(r: &mut Report, tree: Option<&str>) -> Option<PathBuf> {
    let viewer = implexity_server::ViewerSource::from_env();
    let root = tree_root(tree);
    match &root {
        None => r.warn(
            "tree",
            "No source viewer directory found. The embedded viewer remains available. Set IMPLEXITY_HOME or pass --tree to inspect a source viewer.",
        ),
        Some(root) => {
            let (have, missing): (Vec<&str>, Vec<&str>) =
                ["viewer"].into_iter().partition(|s| root.join(s).is_dir());
            let mut detail = format!(
                "{} -- has {}",
                root.display(),
                have.iter().map(|s| format!("{s}/")).collect::<Vec<_>>().join(", ")
            );
            if !missing.is_empty() {
                let _ = write!(
                    detail,
                    "; missing {}",
                    missing.iter().map(|s| format!("{s}/")).collect::<Vec<_>>().join(", ")
                );
            }
            r.either(missing.is_empty(), "warn", "tree", &detail);
        }
    }
    match viewer.read("model.html") {
        Some(bytes) => {
            #[allow(clippy::cast_precision_loss)]
            let kb = bytes.len() as f64 / 1024.0;
            r.ok("viewer", &format!("{} (model.html, {kb:.0} kB)", viewer.describe()));
        }
        None => r.warn(
            "viewer",
            "no viewer/model.html found; the service serves the API but http://host:port/ will 404.",
        ),
    }
    root
}

fn check_required(r: &mut Report) {
    for (label, krate, role) in LINKED {
        match locked_version(krate) {
            Some(v) => r.ok(&format!("require:{label}"), &format!("{v}  (compiled in: {role})")),
            None => r.fail(&format!("require:{label}"), "not recorded in the embedded Cargo.lock"),
        }
    }
}

fn check_profile(r: &mut Report, profile: &str) {
    if profile == "physics" {
        for (label, krate, _) in LINKED.iter().filter(|(_, k, _)| matches!(*k, "faer" | "ndarray" | "rayon"))
        {
            let v = locked_version(krate).unwrap_or_else(|| "?".into());
            r.ok(
                &format!("physics:{label}"),
                &format!("{label} {v} is compiled in and matches the build's pin (Cargo.lock). Probe execution is a separate check."),
            );
        }
    }
    if profile == "core" {
        r.ok(
            "physics:optional",
            "Physics dependencies and external studies are not required for core GUI/model editing. Select --profile physics to check the numerical environment.",
        );
    }
}

fn check_optional(r: &mut Report) {
    for (label, extra, native) in REPLACED {
        r.ok(&format!("optional:{label}"), &format!("not needed [{extra}]: {native}"));
    }
}

const PROBE_X: [f64; 3] = [1.0, 2.0, 3.0];
const PROBE_Y: f64 = 14.0;
const PROBE_G: [f64; 3] = [2.0, 4.0, 6.0];
const PROBE_TOL: f64 = 1e-6;

fn check_autodiff(r: &mut Report, quick: bool, required: bool) {
    let bad = if required { "fail" } else { "warn" };
    if quick {
        r.emit(bad, "autodiff", "--quick: not probed. Probing evaluates one small forward-mode kernel.");
        return;
    }
    if required {
        r.ok(
            "autodiff:x64",
            "binary64 is the only floating-point precision of the numerical kernels; the probe uses float64.",
        );
    }
    let result = implexity_ad::forward::gradient::<3, _>(
        |x: &[implexity_ad::Dual<3>]| {
            x.iter().fold(implexity_ad::Dual::<3>::constant(0.0), |acc, v| acc + *v * *v)
        },
        &PROBE_X,
    );
    match result {
        Err(e) => {
            r.fail("autodiff", &format!("forward-mode probe raised {e}; the AD kernels are not functional."));
        }
        Ok((y, g)) => {
            if !y.is_finite() || (y - PROBE_Y).abs() > PROBE_TOL {
                r.fail(
                    "autodiff",
                    &format!("primal probe returned {y}, expected {PROBE_Y:.1} -- the build is not functionally live."),
                );
            } else if g.len() != 3 || g.iter().zip(PROBE_G).any(|(a, b)| (a - b).abs() > PROBE_TOL) {
                r.fail(
                    "autodiff",
                    &format!(
                        "gradient probe returned {g:?}, expected {PROBE_G:?} -- the AD is not functional."
                    ),
                );
            } else {
                r.ok(
                    "autodiff",
                    &format!(
                        "forward duals live (probe: sum(x**2) at x=[1,2,3] -> y=14.0, dy/dx=[2,4,6]; implexity-ad {})",
                        implexity_core::RUST_VERSION
                    ),
                );
            }
        }
    }
}

fn locator() -> Option<implexity_io::locate::Locator> {
    implexity_bundle::init();
    implexity_io::locate::Locator::new().ok()
}

fn check_bundle(r: &mut Report, bundle_arg: Option<&str>) -> Option<PathBuf> {
    let loc = locator()?;
    let env = implexity_io::locate::Environ::Process;
    let start = crate::util::exe_dir().unwrap_or_default();
    let explicit = bundle_arg.filter(|b| !b.is_empty()).map(PathBuf::from);
    let Some(bundle) = loc.find_bundle(explicit.as_deref(), env, None) else {
        let cands = loc.probe_candidates(&start);
        r.warn(
            "bundle",
            &format!(
                "no study bundle found. Probed upward from {} and saw {} marker match(es). Without one the service runs on the SYNTHETIC backend: every preview endpoint works, the physics does not. Set IMPLEXITY_BUNDLE or pass --bundle.",
                start.display(),
                cands.len()
            ),
        );
        return None;
    };
    let gaps = loc.bundle_gaps(&bundle);
    let how = if explicit.is_some() {
        "--bundle".to_owned()
    } else if getenv("IMPLEXITY_BUNDLE").is_some() {
        "$IMPLEXITY_BUNDLE".to_owned()
    } else {
        format!("an upward probe from {}", start.display())
    };
    if gaps.is_empty() {
        r.ok(
            "bundle",
            &format!(
                "{} (via {how}) -- complete: both markers, all {} required module files, and a resolvable design",
                bundle.display(),
                loc.required_files(Some(&bundle)).len()
            ),
        );
    } else {
        r.warn(
            "bundle",
            &format!(
                "{} (via {how}) is INCOMPLETE -- missing {}{}. The real backend will fail and /v1/domain will answer 500; this is the failure the locator's completeness test exists to name.",
                bundle.display(),
                gaps.iter().take(6).cloned().collect::<Vec<_>>().join(", "),
                if gaps.len() > 6 { " ..." } else { "" }
            ),
        );
    }
    for (path, pgaps) in loc.probe_candidates(&start) {
        if path == bundle {
            continue;
        }
        r.warn(
            "bundle:also-seen",
            &format!(
                "{} is also a marker match and was {}. A bundle chosen by accident looks exactly like one chosen on purpose until the first HTTP 500.",
                path.display(),
                if pgaps.is_empty() {
                    "complete".to_owned()
                } else {
                    format!("skipped -- missing {}", pgaps.iter().take(3).cloned().collect::<Vec<_>>().join(", "))
                }
            ),
        );
    }
    Some(bundle)
}

fn check_design(r: &mut Report, bundle: Option<&Path>, design_arg: Option<&str>) -> Option<PathBuf> {
    let loc = locator()?;
    let env = implexity_io::locate::Environ::Process;
    let explicit = design_arg.filter(|d| !d.is_empty()).map(PathBuf::from);
    let Some(design) = loc.resolve_design(bundle, explicit.as_deref(), env) else {
        r.warn(
            "design",
            "no design found (looked at $IMPLEXITY_DESIGN, then the bundle layout's declared design, then the newest design matching its declared pattern). The synthetic backend generates its own design.",
        );
        return None;
    };
    if !design.is_file() {
        r.fail(
            "design",
            &format!(
                "{} does not exist. --design/$IMPLEXITY_DESIGN names a file that is not there.",
                design.display()
            ),
        );
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let size = std::fs::metadata(&design).map_or(0.0, |m| m.len() as f64) / 1024.0;
    let keys = match implexity_io::npz::load_file(&design) {
        Ok(z) => {
            let names: Vec<String> = z.members().iter().map(|(n, _)| n.clone()).collect();
            format!(
                "; {} array(s): {}{}",
                names.len(),
                names.iter().take(6).cloned().collect::<Vec<_>>().join(", "),
                if names.len() > 6 { " ..." } else { "" }
            )
        }
        Err(e) => format!("; could not be read as a .npz: {e}"),
    };
    r.ok("design", &format!("{} ({size:.0} kB){keys}", design.display()));
    match loc.resolve_lattice_full(bundle, env) {
        Some(lattice) if lattice.is_dir() => {
            let n = std::fs::read_dir(&lattice).map_or(0, |it| {
                it.filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().rsplit_once('.').is_some_and(|(_, x)| x == "py"))
                    .count()
            });
            r.ok("lattice_full", &format!("{} ({n} .py files)", lattice.display()));
        }
        _ => r.warn(
            "lattice_full",
            "not found -- the real backend cannot read the bundle's geometry code directory. $IMPLEXITY_LATTICE_FULL overrides it independently of the bundle.",
        ),
    }
    Some(design)
}

pub(crate) fn load_registries() -> Vec<String> {
    implexity_bundle::init();
    let mut warn = Vec::new();
    if let Err(e) = implexity_server::startup::initialise_kernel() {
        warn.push(format!("RuntimeError: {e}"));
    }
    let manager = implexity_core::packages::global();
    match manager.descriptors() {
        Ok(descriptors) => {
            if let Err(e) = implexity_core::plugins::load(
                getenv("IMPLEXITY_PLUGINS").as_deref(),
                descriptors,
                implexity_core::registries::global(),
            ) {
                warn.push(format!("PluginError: {e}"));
            }
        }
        Err(e) => warn.push(format!("{}: {}", e.python_class(), e.message())),
    }
    warn
}

pub(crate) fn physics_backends() -> Vec<(String, bool, String)> {
    let reg = &implexity_core::registries::global().contributions;
    implexity_core::backends::physics_backends(reg)
        .into_iter()
        .map(|(name, be)| {
            let (ok, detail) = be.available();
            (name, ok, detail)
        })
        .collect()
}

pub(crate) fn geometry_backends(design: Option<&Path>) -> Vec<(String, i64, bool, String)> {
    implexity_core::backends::geometry_names()
        .into_iter()
        .filter_map(|n| implexity_core::backends::geometry(Some(&n), None).ok())
        .map(|b| {
            let (ok, detail) = b.available(design);
            (b.name().to_owned(), b.priority(), ok, detail)
        })
        .collect()
}

fn check_backends(r: &mut Report, design: Option<&Path>, quick: bool) {
    for w in load_registries() {
        r.fail(
            "plugins",
            &format!(
                "{w} -- a plugin named in $IMPLEXITY_PLUGINS could not be loaded, and the service refuses to start without one it was told to load."
            ),
        );
    }
    let geo = geometry_backends(design);
    r.ok(
        "registry:geometry",
        &format!(
            "{} registered: {}",
            geo.len(),
            geo.iter().map(|(n, p, _, _)| format!("{n}(prio {p})")).collect::<Vec<_>>().join(", ")
        ),
    );
    let phys = physics_backends();
    r.ok(
        "registry:physics",
        &format!(
            "{} active (contributed by loaded packages): {}",
            phys.len(),
            if phys.is_empty() {
                "none -- no physics package loaded".to_owned()
            } else {
                phys.iter().map(|p| p.0.clone()).collect::<Vec<_>>().join(", ")
            }
        ),
    );
    let loaded = implexity_core::plugins::describe();
    if loaded.is_empty() {
        r.ok("registry:plugins", "none loaded ($IMPLEXITY_PLUGINS unset) -- the normal case");
    } else {
        r.ok("registry:plugins", &loaded.join("; "));
    }
    if quick {
        r.warn(
            "backend:availability",
            "--quick: not probed. Probing 'real' reads the study bundle's design, which is seconds and memory.",
        );
        return;
    }
    for (name, _, avail, detail) in &geo {
        if *avail {
            r.ok(&format!("backend:{name}"), detail);
        } else {
            r.warn(
                &format!("backend:{name}"),
                &format!("unavailable: {}. `--backend auto` will skip it.", oneline(detail, 180)),
            );
        }
    }
    match implexity_core::backends::geometry(Some("auto"), design) {
        Ok(chosen) => r.ok(
            "backend:auto",
            &format!(
                "resolves to '{}' -- this is what `implexity serve` (no --backend) will serve",
                chosen.name()
            ),
        ),
        Err(e) => r.fail("backend:auto", &format!("no geometry backend reports itself available: {e}")),
    }
    for (name, avail, detail) in phys {
        if avail {
            r.ok(&format!("physics:{name}"), &detail);
        } else {
            r.warn(
                &format!("physics:{name}"),
                &format!(
                    "unavailable: {}. The endpoints its package contributes need it; the preview endpoints do not.",
                    oneline(&detail, 180)
                ),
            );
        }
    }
}

fn check_formats(r: &mut Report) {
    let cat = implexity_mesh::exporters::catalogue();
    r.ok(
        "formats",
        &format!(
            "{} registered, catalogue order: {}",
            cat.len(),
            cat.iter().filter_map(|f| f["format"].as_str()).collect::<Vec<_>>().join(", ")
        ),
    );
    for f in &cat {
        let s = |k: &str| f[k].as_str().unwrap_or_default().to_owned();
        let name = format!("format:{}", s("format"));
        if f["available"].as_bool() == Some(true) {
            r.ok(&name, &format!("writes .{} as {} -- {}", s("extension"), s("mime"), s("detail")));
        } else {
            r.warn(
                &name,
                &format!(
                    "UNAVAILABLE: {}. Needs {}. POST /v1/body will refuse it by name.",
                    oneline(&s("detail"), 180),
                    s("requires")
                ),
            );
        }
    }
}

fn check_port(r: &mut Report, host: &str, port: i64) {
    let Ok(p) = u16::try_from(port) else {
        r.fail("port", &format!("{port} is not a TCP port"));
        return;
    };
    let (free, detail) = port_free(host, p);
    if free {
        r.ok("port", &detail);
        return;
    }
    match health(host, port, std::time::Duration::from_millis(1500)) {
        Some(h) => r.warn(
            "port",
            &format!(
                "{detail} -- and it answers /v1/health with backend {}, so it is a implexity service. `implexity open --port {port}` will use it rather than starting another.",
                h.get("backend").map_or_else(|| "None".into(), implexity_core::pyobj::repr)
            ),
        ),
        None => r.fail(
            "port",
            &format!("{detail} -- and it is NOT a implexity service (no /v1/health). Pick another port with --port."),
        ),
    }
}

fn check_state(r: &mut Report, workspace: Option<&Path>) {
    let d = state_dir(workspace);
    let how = if workspace.is_some() {
        "--workspace"
    } else if getenv("IMPLEXITY_CASE_DIR").is_some() {
        "$IMPLEXITY_CASE_DIR"
    } else {
        "the resolved default workspace"
    };
    let probe = d.join(format!(".implexity-doctor-{}", implexity_io::atomic::unique_token()));
    let result = (|| -> std::io::Result<usize> {
        use std::io::Write as _;
        std::fs::create_dir_all(&d)?;
        let mut fh = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe)?;
        let payload = b"implexity doctor\n";
        fh.write_all(payload)?;
        fh.flush()?;
        fh.sync_all()?;
        drop(fh);
        std::fs::remove_file(&probe)?;
        Ok(payload.len())
    })();
    match result {
        Ok(n) => r.ok(
            "state-dir",
            &format!(
                "{} ({how}): exclusively created, flushed and removed a {n}-byte probe. Existing files were not touched.",
                d.display()
            ),
        ),
        Err(e) => {
            let _ = std::fs::remove_file(&probe);
            r.fail(
                "state-dir",
                &format!(
                    "{} ({how}) is not writable: {e}. Choose a writable workspace with IMPLEXITY_CASE_DIR or --workspace.",
                    d.display()
                ),
            );
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn check_disk(r: &mut Report, target: &Path) {
    let probe = std::iter::successors(Some(target), |p| p.parent()).find(|p| p.exists()).unwrap_or(target);
    match (fs4::available_space(probe), fs4::total_space(probe)) {
        (Ok(free), Ok(total)) => {
            let free = free as f64 / 1024f64.powi(3);
            if free < MIN_DISK_GB {
                r.warn(
                    "disk",
                    &format!(
                        "{free:.1} GiB free at {}. This is below the {MIN_DISK_GB:.0} GiB advisory level. Required storage depends on the model, saved histories and exports; no job is authorized by this probe.",
                        target.display()
                    ),
                );
            } else {
                r.ok(
                    "disk",
                    &format!(
                        "{free:.1} GB free of {:.1} GB at {}",
                        total as f64 / 1024f64.powi(3),
                        target.display()
                    ),
                );
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            r.warn("disk", &format!("could not measure free space at {}: {e}", target.display()));
        }
    }
}

#[must_use]
pub(crate) fn memory_observation() -> Value {
    implexity_core::runtime_environment::memory_observation(Path::new("/proc"))
}

#[must_use]
#[allow(clippy::cast_precision_loss)]
pub(crate) fn avail_mem_gb() -> (Option<f64>, Option<f64>) {
    let rec = memory_observation();
    let gb = |k: &str| rec[k].as_f64().map(|v| v / 1024f64.powi(3));
    (gb("effective_available_bytes"), gb("effective_capacity_bytes"))
}

#[allow(clippy::cast_precision_loss)]
fn check_memory(r: &mut Report) {
    let rec = memory_observation();
    let Some(available) = rec["effective_available_bytes"].as_f64() else {
        r.warn(
            "memory",
            "Available memory is unknown for at least one observed limit. Set and validate a job-specific memory budget.",
        );
        return;
    };
    let total = rec["effective_capacity_bytes"]
        .as_f64()
        .map_or_else(|| "unknown".to_owned(), |t| format!("{:.2} GiB", t / 1024f64.powi(3)));
    let detail = format!(
        "{:.2} GiB available; effective capacity {total}; {} visible cgroup records. This is a changing observation, not a job allocation or a guarantee that a model fits.",
        available / 1024f64.powi(3),
        rec["cgroups"].as_array().map_or(0, Vec::len)
    );
    r.either(available >= MIN_FREE_MEM_GB * 1024f64.powi(3), "warn", "memory", &detail);
}

fn check_physics_linked(r: &mut Report) {
    implexity_bundle::init();
    let linked = implexity_core::packages::linked_installers();
    match implexity_core::packages::global().descriptors() {
        Ok(descriptors) => {
            for d in descriptors {
                if linked.contains(&d.installer) {
                    r.ok(
                        &format!("physics-link:{}", d.package_id),
                        &format!(
                            "{} linked (installer {}); not a field-solver qualification.",
                            d.package_id, d.installer
                        ),
                    );
                } else {
                    r.fail(
                        &format!("physics-link:{}", d.package_id),
                        &format!(
                            "catalogued by {} but its installer {} is not linked into this build",
                            d.distribution, d.installer
                        ),
                    );
                }
            }
        }
        Err(e) => r.fail("physics-link", &format!("{}: {}", e.python_class(), e.message())),
    }
}

fn parser() -> Parser {
    Parser::new(
        "implexity doctor",
        "inspect the selected installation profile without implying model or engineering qualification",
    )
    .opt("--json", Kind::Flag, 0, "machine-readable; the same checks")
    .opt("--host", Kind::Str, 1, "service address (default 127.0.0.1)")
    .opt("--port", Kind::Int, 1, "the port the service would listen on (default 8765)")
    .opt(
        "--profile",
        Kind::Str,
        1,
        "core: GUI/model installation; physics: numerical profile and AD probe; legacy: external study adapters",
    )
    .choices(&["core", "physics", "legacy"])
    .opt(
        "--allow-runtime-mismatch",
        Kind::Flag,
        0,
        "explicit diagnostic override for installed numerical versions; never permits missing/broken dependencies",
    )
    .opt("--bundle", Kind::Str, 1, "explicit legacy study bundle root (requires --profile legacy)")
    .opt("--design", Kind::Str, 1, "design .npz")
    .opt("--tree", Kind::Str, 1, "the unpacked implexity tree")
    .opt("--workspace", Kind::Str, 1, "probe this writable workspace instead of the resolved default")
    .opt("--quick", Kind::Flag, 0, "skip the AD/backend probes; an incomplete physics profile exits unsuccessfully")
}

#[allow(clippy::too_many_lines)]
pub(crate) fn main(argv: &[String]) -> u8 {
    let p = parser();
    let a = match p.parse(argv) {
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
    let refuse = |m: &str| -> u8 {
        if let Exit::Error(t) = p.error(m) {
            eprint!("{t}");
        }
        2
    };
    let profile = a.str("--profile").unwrap_or("core").to_owned();
    if (a.str("--bundle").is_some() || a.str("--design").is_some()) && profile != "legacy" {
        return refuse("--bundle and --design are legacy diagnostics. Select --profile legacy explicitly.");
    }
    if a.flag("--allow-runtime-mismatch") && profile != "physics" {
        return refuse("--allow-runtime-mismatch applies only to --profile physics.");
    }
    let workspace = match a.str("--workspace").filter(|w| !w.is_empty()) {
        Some(w) => {
            let path = crate::util::expand_abs(w);
            if path.exists() && !path.is_dir() {
                return refuse("--workspace must name a directory, not a file");
            }
            Some(path)
        }
        None => None,
    };
    let json_out = a.flag("--json");
    let quick = a.flag("--quick");
    let host = a.str("--host").unwrap_or("127.0.0.1").to_owned();
    let port = a.int("--port").unwrap_or(8765);
    let mut r = Report { results: Vec::new(), quiet: json_out };
    if !json_out {
        println!("{}", "-".repeat(72));
        println!(
            "  implexity doctor {} -- {} {}",
            implexity_core::PYTHON_COMPATIBILITY_VERSION,
            implexity_core::runtime_environment::platform_system(),
            implexity_core::runtime_environment::platform_machine()
        );
        println!("{}", "-".repeat(72));
    }
    let t0 = Instant::now();
    check_build(&mut r);
    check_linkage(&mut r);
    check_install(&mut r);
    check_tree(&mut r, a.str("--tree"));
    check_required(&mut r);
    check_profile(&mut r, &profile);
    if profile == "legacy" {
        check_optional(&mut r);
        check_autodiff(&mut r, quick, false);
        let bundle = check_bundle(&mut r, a.str("--bundle"));
        let design = check_design(&mut r, bundle.as_deref(), a.str("--design"));
        check_backends(&mut r, design.as_deref(), quick);
        check_formats(&mut r);
    } else if profile == "physics" {
        check_physics_linked(&mut r);
        check_autodiff(&mut r, quick, true);
    }
    check_port(&mut r, &host, port);
    check_state(&mut r, workspace.as_deref());
    check_disk(&mut r, &state_dir(workspace.as_deref()));
    check_memory(&mut r);
    let (failed, warned, passed) = r.counts();
    let secs = t0.elapsed().as_secs_f64();
    if json_out {
        let probe =
            profile == "physics" && r.results.iter().any(|x| x["check"] == "autodiff" && x["status"] == "ok");
        let out = json!({
            "version": implexity_core::PYTHON_COMPATIBILITY_VERSION,
            "profile": profile,
            "runtime": implexity_core::runtime_environment::inspect_runtime_environment(),
            "engineering_qualification": false,
            "numerical_probe_executed": probe,
            "failed": failed, "warned": warned, "passed": passed,
            "seconds": (secs * 100.0).round() / 100.0,
            "results": r.results,
        });
        println!("{}", crate::util::dumps_indent(&out));
    } else {
        println!("{}", "-".repeat(72));
        println!("{failed} failed, {warned} warned, {passed} passed   ({secs:.1}s)");
        println!();
        if failed > 0 {
            println!(
                "Required checks for this diagnostic profile did not pass. Fix the FAIL lines before relying on this installation for that profile."
            );
        } else if warned > 0 {
            println!(
                "Required profile checks passed with warnings. Read their scope before use. This is not physical validation or a model preflight."
            );
        } else {
            println!(
                "Selected installation checks passed. Model preflight, physical verification and engineering acceptance remain separate."
            );
        }
    }
    u8::from(failed > 0)
}


