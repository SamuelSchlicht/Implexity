// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



pub mod analysis;
pub mod gradcheck;
pub mod mma_driver;
pub mod node;
pub mod problem;
pub mod solve;
pub mod spec;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::error::{JobError, JobResult};
pub use spec::OptimizeSpec;

pub const JOB_SPEC_SCHEMA: &str = "implexity-implicit-optimize-job/1";

fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other.clone()],
    }
}


pub fn node_from_job_spec(
    js: &Value,
) -> JobResult<(implexity_geometry::NodeRef, implexity_geometry::document::Model)> {
    node::register_kind();
    let Some(doc) = js.as_object().and_then(|m| m.get("document")) else {
        return Err(JobError::optimize1(format!(
            "the job spec must be an object carrying at least 'document' and 'node'; got {}",
            implexity_core::py_repr::repr_str(implexity_core::pyobj::type_name(js))
        )));
    };
    let base_dir = js.get("base_dir").and_then(Value::as_str).map(PathBuf::from);
    let model = implexity_geometry::document::build(doc, base_dir.as_deref(), None)?;
    let name = js
        .get("node")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .or_else(|| doc.get("root"))
        .map(implexity_core::pyobj::py_str)
        .unwrap_or_default();
    let child = model.node(&name)?;
    let decl = node::Declaration {
        free: list(js.get("free")),
        objective: list(js.get("objective")),
        constraints: list(js.get("constraints")),
        case: js.get("case").cloned().unwrap_or(Value::Null),
        settings: js.get("settings").and_then(Value::as_object).cloned().unwrap_or_default(),
    };
    let n = node::optimize_node(child, &decl)?;
    Ok((n, model))
}


pub fn spec_from_job_spec(js: &Value) -> JobResult<(implexity_geometry::NodeRef, Arc<OptimizeSpec>)> {
    let (n, _) = node_from_job_spec(js)?;
    let op = node::op_of(&n).ok_or_else(|| JobError::runtime("not an optimize node"))?;
    let spec = op.spec(&n)?;
    Ok((n, spec))
}

fn out_line(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn dumps(v: &Value) -> String {
    implexity_core::json::dumps(v, &implexity_core::json::DumpOptions::default())
}

#[derive(Debug, Default)]
struct Args {
    spec_file: Option<String>,
    job_dir: Option<String>,
    resume: bool,
    preflight: bool,
    sensitivity: bool,
    sensitivity_file: Option<String>,
    derivative_file: Option<String>,
    results_file: Option<String>,
    replay: Option<String>,
    bundle: Option<String>,
}

const USAGE: &str = "usage: implexity.implicit.optimize [-h] --spec-file SPEC_FILE [--job-dir JOB_DIR] [--resume] \
[--preflight] [--sensitivity] [--sensitivity-file SENSITIVITY_FILE] [--derivative-file DERIVATIVE_FILE] \
[--results-file RESULTS_FILE] [--replay REPLAY] [--bundle BUNDLE]";

fn usage_error(msg: &str) -> i32 {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{USAGE}\nimplexity.implicit.optimize: error: {msg}");
    2
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("argument {name}: expected one argument"))
        };
        match flag.as_str() {
            "--spec-file" => a.spec_file = Some(value("--spec-file")?),
            "--job-dir" => a.job_dir = Some(value("--job-dir")?),
            "--resume" => a.resume = true,
            "--preflight" => a.preflight = true,
            "--sensitivity" => a.sensitivity = true,
            "--sensitivity-file" => a.sensitivity_file = Some(value("--sensitivity-file")?),
            "--derivative-file" => a.derivative_file = Some(value("--derivative-file")?),
            "--results-file" => a.results_file = Some(value("--results-file")?),
            "--replay" => a.replay = Some(value("--replay")?),
            "--bundle" => a.bundle = Some(value("--bundle")?),
            other => return Err(format!("unrecognized arguments: {other}")),
        }
    }
    if a.spec_file.is_none() {
        return Err("the following arguments are required: --spec-file".into());
    }
    Ok(a)
}

fn is_refusal(e: &JobError) -> bool {
    matches!(
        e.python_class(),
        "OptimizeError" | "ModelError" | "ModelDocError" | "ExprError" | "BoundViolation"
    )
}

fn read_json(path: &str) -> JobResult<Value> {
    implexity_core::json::read_file(Path::new(path)).map_err(|e| JobError::of("OSError", e))
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn main(argv: &[String]) -> i32 {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(m) => return usage_error(&m),
    };
    if args.bundle.is_some() {
        return usage_error(
            "--bundle (legacy physics discovery) is not supported by the Rust worker: physics packages are linked \
             into the binary and selected through IMPLEXITY_PHYSICS_BACKEND",
        );
    }
    let sensitivity_mode = args.sensitivity || args.sensitivity_file.is_some();
    if !args.preflight
        && !sensitivity_mode
        && args.derivative_file.is_none()
        && args.results_file.is_none()
        && args.job_dir.is_none()
    {
        return usage_error(
            "--job-dir is required unless --preflight, --sensitivity, --sensitivity-file, --derivative-file or --results-file",
        );
    }
    let modes =
        [args.preflight, sensitivity_mode, args.derivative_file.is_some(), args.results_file.is_some()];
    if modes.iter().filter(|m| **m).count() > 1 {
        return usage_error(
            "choose one of --preflight, --sensitivity, --sensitivity-file, --derivative-file or --results-file",
        );
    }
    let job_dir = args.job_dir.clone();
    let fail = |err: Value, code: i32| -> i32 {
        out_line(&format!("ERROR {}", dumps(&err)));
        if let Some(d) = &job_dir {
            let d = Path::new(d);
            if std::fs::create_dir_all(d).is_ok() {
                let text = implexity_core::json::dumps(&err, &implexity_core::json::DumpOptions::indented(1));
                let _ = std::fs::write(d.join("error.json"), text);
            }
        }
        code
    };
    let refused = |what: &str, e: &JobError| -> Value { let mut payload = json!({"error": what, "problems": e.problems()}); if let Some(report) = e.solver_recovery() { payload["solver_recovery"] = report.clone(); } payload };
    let crashed = |e: &JobError| -> Value {
        let mut payload = json!({"error": e.describe(), "problems": [e.describe()], "traceback": format!("{}\n", e.describe())});
        if let Some(report) = e.solver_recovery() { payload["solver_recovery"] = report.clone(); }
        payload
    };
    let spec_file = args.spec_file.clone().unwrap_or_default();
    let loaded = read_json(&spec_file).and_then(|js| spec_from_job_spec(&js).map(|(n, s)| (js, n, s)));
    let (_, node_ref, spec) = match loaded {
        Ok(v) => v,
        Err(e) if is_refusal(&e) => return fail(refused("the optimisation was refused", &e), 2),
        Err(e) => return fail(crashed(&e), 3),
    };
    let analysis = |name: &str, what: &str, run: &dyn Fn() -> JobResult<Value>| -> i32 {
        match run() {
            Ok(rep) => {
                out_line(&format!("{name} {}", dumps(&rep)));
                0
            }
            Err(e) if is_refusal(&e) => fail(refused(what, &e), 2),
            Err(e) => fail(crashed(&e), 3),
        }
    };
    if let Some(rf) = &args.results_file {
        return analysis("RESULTS", "the result-field analysis was refused", &|| {
            analysis::result_snapshot(&spec, &read_json(rf)?, None)
        });
    }
    if let Some(df) = &args.derivative_file {
        return analysis("DERIVATIVE", "the derivative analysis was refused", &|| {
            analysis::derivative_operator(&spec, &read_json(df)?, None)
        });
    }
    if sensitivity_mode {
        return analysis("SENSITIVITY", "the sensitivity analysis was refused", &|| {
            let req = match &args.sensitivity_file {
                Some(f) => read_json(f)?,
                None => Value::Object(Map::new()),
            };
            analysis::sensitivity(&spec, &req, None)
        });
    }
    if args.preflight {
        return analysis("PREFLIGHT", "the optimisation was refused", &|| analysis::survey(&spec, None));
    }
    let total = spec.int_setting("iters").max(1);
    let emit = |kind: &str, payload: &Value| {
        if kind == "progress" {
            out_line(&format!(
                "PROGRESS {} {}",
                implexity_geometry::pyfmt::fmt_f(payload["frac"].as_f64().unwrap_or(0.0), 3),
                implexity_core::pyobj::py_str(&payload["message"])
            ));
            return;
        }
        out_line(&format!("{} {}", kind.to_uppercase(), dumps(payload)));
        if kind == "iter" {
            let done = payload["i"].as_i64().unwrap_or(0) + 1;
            #[allow(clippy::cast_precision_loss)]
            let frac = (done as f64 / total as f64).min(0.99);
            out_line(&format!(
                "PROGRESS {} iteration {done}/{total} L = {}",
                implexity_geometry::pyfmt::fmt_f(frac, 3),
                implexity_geometry::pyfmt::fmt_f(payload["L"].as_f64().unwrap_or(f64::NAN), 6)
            ));
        }
    };
    let log = |msg: &str| out_line(msg);
    let replay = match &args.replay {
        None => None,
        Some(f) => match read_json(f) {
            Ok(Value::Object(m)) => {
                Some(m.get("model_timeline").and_then(Value::as_array).cloned().unwrap_or_default())
            }
            Ok(Value::Array(a)) => Some(a),
            Ok(_) => Some(Vec::new()),
            Err(e) => return fail(crashed(&e), 3),
        },
    };
    let Some(op) = node::op_of(&node_ref) else { return fail(json!({"error": "not an optimize node"}), 3) };
    match op.solve(
        &node_ref,
        job_dir.clone().map(PathBuf::from),
        args.resume,
        &emit,
        &log,
        replay.as_deref(),
        true,
    ) {
        Ok(_) => 0,
        Err(e) if e.python_class() == solve::HALTED => 0,
        Err(e) if is_refusal(&e) => fail(refused("the optimisation was refused", &e), 2),
        Err(e) => fail(crashed(&e), 3),
    }
}
