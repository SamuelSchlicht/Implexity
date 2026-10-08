// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::time::Instant;

use serde_json::{Value, json};

use implexity_geometry::ParamValue;
use implexity_geometry::eval::{EvalOptions, eval_points, grad_params};
use implexity_geometry::node::make;

use super::{EXIT_CHECK_FAILED, EXIT_OK, WIDTH, parse};
use crate::args::{Kind, Parser};
use crate::doctor::Report;

fn sphere(r: f64) -> Result<implexity_geometry::NodeRef, String> {
    make("sphere", Vec::new(), None, &[("radius_mm", ParamValue::Float(r))], &[]).map_err(|e| e.to_string())
}

fn ck_kernel(r: &mut Report) {
    let reg = implexity_geometry::node::registry();
    let mut modules: Vec<String> =
        reg.iter().map(|(_, e)| e.info.module.rsplit('.').next().unwrap_or_default().to_owned()).collect();
    modules.sort();
    modules.dedup();
    r.ok(
        "kernel",
        &format!(
            "implexity-geometry {} compiled into {} -- {} kind module(s): {}",
            implexity_core::RUST_VERSION,
            std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
            modules.len(),
            modules.join(", ")
        ),
    );
}

fn ck_registry(r: &mut Report) {
    let base = implexity_geometry::node::registry();
    let mut by_mod: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (kind, e) in base.iter() {
        by_mod
            .entry(e.info.module.rsplit('.').next().unwrap_or_default().to_owned())
            .or_default()
            .push(kind.clone());
    }
    let detail: Vec<String> = by_mod
        .iter()
        .map(|(m, k)| {
            format!(
                "{m}: {} ({}{})",
                k.len(),
                k.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                if k.len() > 4 { " ..." } else { "" }
            )
        })
        .collect();
    r.ok("registry", &format!("{} node kind(s) registered -- {}", base.names().len(), detail.join("; ")));
    let missing: Vec<String> =
        implexity_geometry::kernel_registry().names().into_iter().filter(|k| !base.contains(k)).collect();
    if !missing.is_empty() {
        r.fail(
            "registry:kinds",
            &format!("the kernel declares {}, which did not register", missing.join(", ")),
        );
    }
    implexity_jobs::optimize::node::register_kind();
    let after = implexity_geometry::node::registry();
    if after.contains("optimize") {
        r.ok(
            "registry:optimize",
            &format!(
                "the `optimize` kind registered: gradient descent is available as a modelling node ({} kinds in total)",
                after.names().len()
            ),
        );
    } else {
        r.warn(
            "registry:optimize",
            "the `optimize` kind did not register, so a document containing an `optimize` node cannot be built here. Everything else -- validate, show, eval, export -- is unaffected.",
        );
    }
}

fn ck_deps(r: &mut Report) {
    for (label, krate, role) in [
        ("ndarray", "ndarray", "N-d arrays (numpy's role)"),
        ("faer", "faer", "linear algebra"),
        ("rayon", "rayon", "the parallel point-block evaluator"),
    ] {
        match crate::doctor::locked_version(krate) {
            Some(v) => r.ok(&format!("dep:{label}"), &format!("{v}  (compiled in: {role})")),
            None => r.fail(&format!("dep:{label}"), "not recorded in the embedded Cargo.lock"),
        }
    }
    r.ok(
        "dep:autodiff",
        &format!(
            "implexity-ad {} and the kernel's own reverse tape are compiled in: evaluation and parameter gradients need no optional package (jax/jaxlib/optax have no role in this build)",
            implexity_core::RUST_VERSION
        ),
    );
}

fn ck_document(r: &mut Report) {
    let doc = json!({"schema": implexity_geometry::document::SCHEMA, "name": "doctor probe", "units": "mm",
        "parameters": {"r": {"value": 2.5, "units": "mm"}},
        "nodes": {"s": {"kind": "sphere", "params": {"radius_mm": {"bind": "r"}}},
                  "b": {"kind": "box", "params": {"bx_mm": 2.0, "by_mm": 2.0, "bz_mm": 2.0}},
                  "u": {"kind": "union", "children": [{"name": "a", "node": "s"}, {"name": "b", "node": "b"}]}},
        "root": "u"});
    let t0 = Instant::now();
    match implexity_geometry::document::check_roundtrip(&doc, None, None) {
        Ok(rt) => {
            let secs = t0.elapsed().as_secs_f64();
            let t = |k: &str| rt[k].as_bool() == Some(true);
            let good = t("bytes_equal") && t("content_id_equal") && t("structure_id_equal");
            let py = |b: bool| if b { "True" } else { "False" };
            r.either(
                good,
                "fail",
                "document",
                &format!(
                    "a 3-node document built, serialised, re-read and re-serialised in {secs:.3} s: {} bytes, bytes_equal={}, content_id {} (equal={}), structure_id {} (equal={})",
                    rt["bytes"],
                    py(t("bytes_equal")),
                    implexity_core::pyobj::py_str(&rt["content_id"]),
                    py(t("content_id_equal")),
                    implexity_core::pyobj::py_str(&rt["structure_id"]),
                    py(t("structure_id_equal"))
                ),
            );
        }
        Err(e) => r.fail("document", &format!("the probe document was refused: {e}")),
    }
}

fn ck_evaluate(r: &mut Report) {
    let node = match sphere(3.0) {
        Ok(n) => n,
        Err(e) => return r.fail("evaluate", &format!("a sphere could not be built: {e}")),
    };
    let pts = [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [5.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let t0 = Instant::now();
    let first_call = eval_points(&node, &pts, &EvalOptions::default());
    let first = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let again_call = eval_points(&node, &pts, &EvalOptions::default());
    let again = t1.elapsed().as_secs_f64();
    let (v, _) = match (first_call, again_call) {
        (Ok(v), Ok(w)) => (v, w),
        (Err(e), _) | (_, Err(e)) => {
            return r.fail("evaluate", &format!("the field cannot be evaluated here: {e}"));
        }
    };
    let want = [-3.0, 0.0, 2.0, 1.0];
    let err = v.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    let round6: Vec<String> =
        v.iter().map(|x| implexity_core::py_repr::repr_float((x * 1e6).round() / 1e6)).collect();
    r.either(
        err < 1e-9,
        "fail",
        "evaluate",
        &format!(
            "sphere(r=3) at 4 points gave [{}]; the exact signed distances are [-3.0, 0.0, 2.0, 1.0]; max error {}.  First call {first:.3} s (closure tree built), second {again:.4} s (cached kernel)",
            round6.join(", "),
            implexity_geometry::pyfmt::fmt_e(err, 2)
        ),
    );
}

fn ck_gradient(r: &mut Report) {
    let node = match sphere(3.0) {
        Ok(n) => n,
        Err(e) => return r.fail("gradient", &format!("a sphere could not be built: {e}")),
    };
    let pts = [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]];
    let refs = [implexity_geometry::ParamRef::new(Vec::new(), "radius_mm")];
    let sum = |f: &[f64]| (f.iter().sum::<f64>(), vec![1.0; f.len()]);
    match grad_params(&node, &refs, &sum, &pts, &EvalOptions::default()) {
        Ok(g) => {
            let got = g.first().and_then(|pg| pg.data.first().copied()).unwrap_or(f64::NAN);
            r.either(
                (got + 2.0).abs() < 1e-9,
                "fail",
                "gradient",
                &format!(
                    "d/d(radius) of sum(f) over 2 points = {}; f = |x| - r at each point, so the exact answer is -2.  Error {}.  This is the chain rule reaching a modelling parameter, which is the whole premise",
                    implexity_geometry::pyfmt::fmt_g(got, 12),
                    implexity_geometry::pyfmt::fmt_e((got + 2.0).abs(), 2)
                ),
            );
        }
        Err(e) => r.fail("gradient", &format!("no gradient here: {e}")),
    }
}

fn ck_models(r: &mut Report) {
    let Some(d) = implexity_geometry::examples::models_dir(super::tree_root().as_deref()) else {
        r.warn(
            "models",
            "The manual geometry templates were not found in the selected tree or installed resources. Set IMPLEXITY_HOME or IMPLEXITY_MODELS, or write them anywhere with `implexity model examples --copy DIR`.",
        );
        return;
    };
    let manual = implexity_geometry::examples::kernel_examples();
    let (mut rows, mut bad) = (Vec::new(), Vec::new());
    for ex in &manual {
        let path = d.join(&ex.filename);
        if !path.is_file() {
            bad.push(format!("{} is missing", ex.filename));
            continue;
        }
        match implexity_core::json::read_file(&path) {
            Ok(raw) => match implexity_geometry::document::problems_of(&raw, Some(&d), None) {
                Ok((probs, _)) if probs.is_empty() => rows.push(format!(
                    "{} ({} nodes)",
                    ex.filename,
                    raw["nodes"].as_object().map_or(0, serde_json::Map::len)
                )),
                Ok((probs, _)) => bad.push(format!("{}: {}", ex.filename, probs[0])),
                Err(e) => bad.push(format!("{}: {}: {e}", ex.filename, crate::util::geo_class(&e))),
            },
            Err(e) => bad.push(format!("{}: ValueError: {e}", ex.filename)),
        }
    }
    if bad.is_empty() {
        r.ok(
            "models",
            &format!(
                "{} -- all {} shipped models parse and validate: {}",
                d.display(),
                rows.len(),
                rows.join(", ")
            ),
        );
    } else {
        r.warn(
            "models",
            &format!(
                "{} -- {} of {} shipped models validate ({}); problems: {}",
                d.display(),
                rows.len(),
                manual.len(),
                rows.join("; "),
                bad.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
            ),
        );
    }
}

fn ck_export(r: &mut Report) {
    let avail = implexity_mesh::interop::export_available();
    let have: Vec<&String> =
        avail.iter().filter(|(_, v)| v["available"] == json!(true)).map(|(k, _)| k).collect();
    let missing: Vec<String> = avail
        .iter()
        .filter(|(_, v)| v["available"] != json!(true))
        .map(|(k, v)| format!("{k} ({})", implexity_core::pyobj::py_str(&v["detail"])))
        .collect();
    let tmp = std::env::temp_dir().join(format!("implexity_model_doctor_{}.stl", std::process::id()));
    let result = sphere(3.0).and_then(|node| {
        let opts = implexity_mesh::interop::ExportOptions {
            fmt: Some("stl"),
            spacing_mm: Some(0.6),
            ..implexity_mesh::interop::ExportOptions::default()
        };
        implexity_mesh::interop::export_subtree(&node, &tmp, &opts).map_err(|e| e.to_string())
    });
    let _ = std::fs::remove_file(&tmp);
    match result {
        Ok(rep) => {
            let vol = rep["volume_mm3"].as_f64().unwrap_or(f64::NAN);
            let wt = rep["watertight"] == json!(true);
            let detail = format!(
                "wrote and deleted a real sphere: {} triangles, {} bytes, watertight={}, volume {vol:.2} mm^3 against the exact 113.10 mm^3 ({:.1} %).  Formats here: {}",
                rep["triangles"],
                rep["bytes"],
                if wt { "True" } else { "False" },
                100.0 * vol / 113.0973,
                have.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            );
            r.either(wt, "warn", "export", &detail);
        }
        Err(e) => r.fail("export", &format!("a sphere could not be meshed and written: {e}")),
    }
    if !missing.is_empty() {
        r.warn("export:formats", &format!("not writable here: {}.", missing.join("; ")));
    }
}

fn ck_shader(r: &mut Report) {
    let model = sphere(3.0).and_then(|s| {
        let b = make(
            "box",
            Vec::new(),
            None,
            &[
                ("bx_mm", ParamValue::Float(2.0)),
                ("by_mm", ParamValue::Float(2.0)),
                ("bz_mm", ParamValue::Float(2.0)),
            ],
            &[],
        )
        .map_err(|e| e.to_string())?;
        let u = make("union", vec![s, b], None, &[], &[]).map_err(|e| e.to_string())?;
        make("shell", vec![u], None, &[("thickness_mm", ParamValue::Float(0.4))], &[])
            .map_err(|e| e.to_string())
    });
    let model = match model {
        Ok(m) => m,
        Err(e) => return r.warn("shader", &format!("the 4-node probe model could not be built: {e}")),
    };
    let t0 = Instant::now();
    match implexity_geometry::glsl::transpile(&model, &implexity_geometry::glsl::TranspileOptions::default())
    {
        Ok(res) => {
            let secs = t0.elapsed().as_secs_f64();
            let text = ["source", "glsl", "fragment", "shader", "code"]
                .iter()
                .find_map(|k| res.payload.get(*k).and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned();
            let words: Vec<&str> = ["#version", "float ", "vec3", "min(", "max(", "abs("]
                .into_iter()
                .filter(|w| text.contains(w))
                .collect();
            r.ok(
                "shader",
                &format!(
                    "implexity_geometry::glsl::transpile compiled shell(union(sphere, box)) in {secs:.2} s to {} characters of GLSL; it declares {}.  A model can be ray-marched in the browser here",
                    text.chars().count(),
                    if words.is_empty() { "no keyword this check looked for".to_owned() } else { words.join(", ") }
                ),
            );
        }
        Err(e) => {
            r.warn("shader", &format!("implexity_geometry::glsl::transpile raised on a 4-node model: {e}"));
        }
    }
}

fn ck_bundle(r: &mut Report, bundle_arg: Option<&str>) {
    implexity_bundle::init();
    let Ok(loc) = implexity_io::locate::Locator::new() else {
        r.warn("bundle", "the bundle layouts of the linked distributions could not be read");
        return;
    };
    let env = implexity_io::locate::Environ::Process;
    let start = crate::util::exe_dir().unwrap_or_default();
    let explicit = bundle_arg.filter(|b| !b.is_empty()).map(std::path::PathBuf::from);
    let Some(bundle) = loc.find_bundle(explicit.as_deref(), env, None) else {
        r.warn(
            "bundle",
            &format!(
                "no study bundle found (probed upward from {}). A physics package whose solver tree lives in a study bundle cannot run without one.  Set IMPLEXITY_BUNDLE or pass --bundle.",
                start.display()
            ),
        );
        return;
    };
    let gaps = loc.bundle_gaps(&bundle);
    let how = if explicit.is_some() {
        "--bundle".to_owned()
    } else if crate::util::getenv("IMPLEXITY_BUNDLE").is_some() {
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
                "{} (via {how}) is INCOMPLETE -- missing {}. A package that reads from it will fail naming one of those files, which is exactly the failure this line predicts.",
                bundle.display(),
                gaps.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
            ),
        );
    }
}

fn ck_physics(r: &mut Report) {
    let t0 = Instant::now();
    let note = super::activate_physics();
    let reg = &implexity_core::registries::global().contributions;
    match implexity_authoring::physics_binding::resolve(reg, None) {
        Ok(b) => r.ok(
            "optimise",
            &format!(
                "Optimize nodes bind to {} ({}) in {:.1} s.  Import availability does not verify a solve, sensitivities, material data or final engineering feasibility.",
                b.name(),
                b.label(),
                t0.elapsed().as_secs_f64()
            ),
        ),
        Err(e) => r.warn(
            "optimise",
            &format!("{}{}", e.problem_list().join("; "), note.map_or_else(String::new, |n| format!("  ({n})"))),
        ),
    }
}

fn ck_memory(r: &mut Report) {
    let (gb, total) = crate::doctor::avail_mem_gb();
    let Some(gb) = gb else {
        r.warn("memory", "could not read available memory on this platform");
        return;
    };
    let need = 2.5;
    r.either(
        gb >= need,
        "warn",
        "memory",
        &format!(
            "{gb:.1} GB available of {:.1} GB. This advisory compares available memory with 2.5 GB; actual requirements depend on the selected geometry and physics.",
            total.unwrap_or(0.0)
        ),
    );
}

pub(crate) fn cmd_doctor(argv: &[String]) -> u8 {
    let p = Parser::new(
        "implexity model doctor",
        "can this machine model?  Every check prints its evidence: the version, the path, the number it got and the number it expected",
    )
    .epilog(
        "A FAIL stops the kernel doing what it advertises.  A WARN is a capability you do not have yet, each naming the one thing that gets it.  Exit 1 if anything failed.",
    )
    .opt("--json", Kind::Flag, 0, "")
    .opt("--quick", Kind::Flag, 0, "skip the evaluation, gradient and physics probes")
    .opt("--bundle", Kind::Str, 1, "study bundle root");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let as_json = a.flag("--json");
    let quick = a.flag("--quick");
    let mut r = Report { results: Vec::new(), quiet: as_json };
    if !as_json {
        println!("{}", "-".repeat(WIDTH));
        println!(
            "  implexity model doctor {} -- the implicit CAD kernel",
            implexity_core::PYTHON_COMPATIBILITY_VERSION
        );
        println!("{}", "-".repeat(WIDTH));
    }
    let t0 = Instant::now();
    ck_kernel(&mut r);
    ck_registry(&mut r);
    ck_deps(&mut r);
    ck_document(&mut r);
    if !quick {
        ck_evaluate(&mut r);
        ck_gradient(&mut r);
    }
    ck_models(&mut r);
    ck_export(&mut r);
    ck_shader(&mut r);
    ck_bundle(&mut r, a.str("--bundle"));
    if !quick {
        ck_physics(&mut r);
    }
    ck_memory(&mut r);
    let (failed, warned, passed) = r.counts();
    let secs = t0.elapsed().as_secs_f64();
    if as_json {
        println!(
            "{}",
            crate::util::dumps_indent(&json!({"version": implexity_core::PYTHON_COMPATIBILITY_VERSION,
                "failed": failed, "warned": warned, "passed": passed,
                "seconds": (secs * 100.0).round() / 100.0, "results": r.results}))
        );
    } else {
        println!("{}", "-".repeat(WIDTH));
        println!("{failed} failed, {warned} warned, {passed} passed   ({secs:.1}s)");
        println!();
        if failed > 0 {
            println!(
                "This machine cannot model as it stands. Fix the FAIL lines above; each names what to do."
            );
        } else if warned > 0 {
            println!(
                "The kernel works. The WARN lines are things it cannot do here yet, each with what would get them."
            );
        } else {
            println!(
                "Everything this can check is in place: model, evaluate, differentiate, export, optimise."
            );
        }
    }
    if failed > 0 { EXIT_CHECK_FAILED } else { EXIT_OK }
}
