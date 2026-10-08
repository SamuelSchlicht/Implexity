// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value, json};

use implexity_jobs::error::JobError;
use implexity_jobs::optimize::node::op_of;
use implexity_jobs::optimize::spec::OptimizeSpec;

use super::{
    EXIT_FAILED, EXIT_MISSING, EXIT_OK, EXIT_REJECTED, WIDTH, fc_json, g, id_of, load, parse, refuse,
};
use crate::args::{Kind, Parser, PosN};
use crate::util::{clip, dumps_indent, home};

fn job_refusal(e: &JobError) -> Vec<String> {
    let p = e.problems();
    if p.is_empty() { vec![format!("{}: {}", e.python_class(), e.message())] } else { p }
}

fn preflight(spec: &OptimizeSpec) -> Vec<String> {
    let mut probs = implexity_geometry::errors::smoothing_trap(&spec.model, &spec.settings);
    let free: Vec<(implexity_geometry::ParamRef, usize)> =
        spec.free.iter().map(|f| (f.child_ref(), f.size)).collect();
    probs.extend(implexity_geometry::errors::array_free_refusals(&spec.model, &free));
    probs
}

fn solve_id(node: &implexity_geometry::NodeRef, spec: &OptimizeSpec) -> String {
    use sha2::Digest as _;
    let subtree = Arc::new(node.with_params(node.params().clone())).content_id();
    let mut h = sha2::Sha256::new();
    h.update(subtree.as_bytes());
    h.update([0x1f]);
    h.update(spec.digest().as_bytes());
    hex::encode(h.finalize())[..16].to_owned()
}

fn f6(v: &Value, prec: usize) -> String {
    v.as_f64().map_or_else(|| implexity_core::pyobj::py_str(v), |x| format!("{x:.prec$}"))
}

fn arr_value(a: &ndarray::ArrayD<f64>) -> Value {
    if a.ndim() == 0 {
        implexity_geometry::value::json_f64(a.iter().copied().next().unwrap_or(f64::NAN))
    } else {
        implexity_jobs::optimize::spec::safe(a)
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_optimise(argv: &[String]) -> u8 {
    let p = Parser::new("implexity model optimise", "run the document's Optimize node and report what moved")
        .epilog(
            "--dry-run makes every refusal that does not need a gradient: the free set, the objective, the field class, the occupancy of the case box and which terms can be scored on this model.  It costs seconds; the run costs minutes and gigabytes.  Do it first.",
        )
        .pos("file", PosN::One, "")
        .opt("--node", Kind::Str, 1, "the Optimize node; default is the root if it is one")
        .opt("--dry-run", Kind::Flag, 0, "build the problem and make every refusal, without a gradient")
        .opt("--iters", Kind::Int, 1, "override the document's iteration count")
        .opt("--lr", Kind::Float, 1, "override the step, in units of each parameter's declared range")
        .opt("--out", Kind::Str, 1, "the job directory (ckpt.npz, history.json, best.npz)")
        .metavar(&["DIR"])
        .opt("--resume", Kind::Flag, 0, "continue from a checkpoint in --out")
        .opt("--quiet", Kind::Flag, 0, "do not echo the solver's own log")
        .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let file = a.pos1("file").unwrap_or_default().to_owned();
    let loaded = match load(&file, Some(true)) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let model = &loaded.model;
    let node = match super::select_node(model, a.str("--node")) {
        Ok(n) => n,
        Err(c) => return c,
    };
    let op = node.as_ref().and_then(|n| op_of(n));
    let (Some(node), Some(op)) = (node.clone(), op) else {
        let mut kinds: Vec<String> = model.node_table().values().map(|n| n.kind().to_owned()).collect();
        kinds.sort();
        kinds.dedup();
        let opts: Vec<String> = model
            .node_table()
            .iter()
            .filter(|(_, n)| n.kind() == "optimize")
            .map(|(i, _)| i.clone())
            .collect();
        let (shown, kind) = node.as_ref().map_or_else(
            || ("(no root)".to_owned(), "missing".to_owned()),
            |n| (id_of(model, n), n.kind().to_owned()),
        );
        let mut probs = vec![
            format!(
                "{shown} is a {} node, not an `optimize` node.  An optimisation is a NODE in this kernel: the document declares which parameters are free, what the objective is and which case it is scored against, and this command runs it.",
                implexity_core::py_repr::repr_str(&kind)
            ),
            format!("this document's kinds are: {}", kinds.join(", ")),
        ];
        if let Some(first) = opts.first() {
            probs.push(format!(
                "it does contain optimize node(s): {} -- pass --node {first}",
                opts.join(", ")
            ));
        }
        return refuse(
            "there is nothing to optimise here",
            &probs,
            &["Author the optimization problem explicitly. See PUBLICATION_SCOPE.md and docs/local_mcp_runner.md. Application studies are external.".into()],
            EXIT_REJECTED,
        );
    };
    let mut decl = op.declaration().clone();
    if let Some(it) = a.int("--iters") {
        decl.settings.insert("iters".into(), json!(it));
    }
    if let Some(lr) = a.float("--lr") {
        decl.settings.insert("lr".into(), implexity_geometry::value::json_f64(lr));
    }
    let Some(child) = node.children().first().cloned() else {
        return refuse(
            "the problem was refused",
            &["an Optimize node has one child".into()],
            &[],
            EXIT_REJECTED,
        );
    };
    let spec = match OptimizeSpec::new(
        &child,
        &decl.free,
        &decl.objective,
        &decl.constraints,
        &decl.case,
        &decl.settings,
    ) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            return refuse(
                "this optimisation would not do what it says -- refused before the compile",
                &job_refusal(&e),
                &[
                    "every one of these is a property of the DOCUMENT and costs nothing to fix; the run costs minutes".into(),
                    "See PUBLICATION_SCOPE.md for supported problem and provider boundaries".into(),
                ],
                EXIT_REJECTED,
            );
        }
    };
    let pre = preflight(&spec);
    if !pre.is_empty() {
        return refuse(
            "this optimisation would not do what it says -- refused before the compile",
            &pre,
            &[
                "every one of these is a property of the DOCUMENT and costs nothing to fix; the run costs minutes".into(),
                "See PUBLICATION_SCOPE.md for supported problem and provider boundaries".into(),
            ],
            EXIT_REJECTED,
        );
    }
    let t0 = Instant::now();
    let prob = match implexity_jobs::optimize::problem::Problem::new(&spec, None, false) {
        Ok(pr) => pr,
        Err(e) => {
            let code = if implexity_jobs::optimize::problem::is_capability_refusal(&e) {
                EXIT_MISSING
            } else {
                EXIT_REJECTED
            };
            return refuse("the problem was refused", &job_refusal(&e), &[], code);
        }
    };
    let prep_s = t0.elapsed().as_secs_f64();
    let sid = solve_id(&node, &spec);
    let describe = spec.describe();
    let free_rows: Vec<Value> =
        spec.free.iter().map(implexity_jobs::optimize::spec::Free::describe).collect();
    let head = json!({
        "file": file, "node": id_of(model, &node), "model": spec.model.kind(),
        "field_class": describe["field_class"], "free": free_rows, "objective": spec.objective,
        "settings": spec.settings, "case": spec.norm.get("name").cloned().unwrap_or(Value::Null),
        "grid": spec.norm.get("grid").cloned().unwrap_or(Value::Null),
        "probe": prob.probe, "terms": prob.terms, "lattice": prob.lattice(),
        "prepare_seconds": (prep_s * 100.0).round() / 100.0, "solve_id": sid,
    });
    let as_json = a.flag("--json");
    if !as_json {
        print_problem(&head);
    }
    if a.flag("--dry-run") {
        if as_json {
            let mut m = Map::new();
            m.insert("dry_run".into(), json!(true));
            if let Some(h) = head.as_object() {
                m.extend(h.clone());
            }
            println!("{}", dumps_indent(&Value::Object(m)));
        } else {
            println!();
            println!("  DRY RUN: every refusal that does not need a gradient has been made and none fired.");
            println!("  Run it:  implexity model optimise {file}");
        }
        return EXIT_OK;
    }
    let out_dir = a.str("--out").map_or_else(
        || {
            std::env::var("IMPLEXITY_CASE_DIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map_or_else(
                    || home().unwrap_or_else(|| PathBuf::from("~")).join(".implexity"),
                    PathBuf::from,
                )
                .join("implicit_opt")
                .join(&sid)
        },
        PathBuf::from,
    );
    let iters = spec.settings.get("iters").cloned().unwrap_or(Value::Null);
    let lr = spec.settings.get("lr").and_then(Value::as_f64).unwrap_or(f64::NAN);
    if !as_json {
        println!();
        println!("  job         {}", out_dir.display());
        println!(
            "  running {} iteration(s) at lr {} -- this is a coupled physics solve",
            implexity_core::pyobj::py_str(&iters),
            g(lr)
        );
        println!("  {}", "-".repeat(WIDTH - 2));
    }
    let quiet = a.flag("--quiet") || as_json;
    let log = move |m: &str| {
        if !quiet {
            for line in m.lines() {
                let clipped: String = line.chars().take(WIDTH + 40).collect();
                println!("  | {clipped}");
            }
        }
    };
    let emit = |_kind: &str, _payload: &Value| {};
    let t1 = Instant::now();
    let outcome = match implexity_jobs::optimize::solve::solve(
        &spec,
        &out_dir,
        a.flag("--resume"),
        &emit,
        &log,
        None,
    ) {
        Ok(o) => o,
        Err(e) => return refuse("the run failed", &job_refusal(&e), &[], EXIT_FAILED),
    };
    let wall = t1.elapsed().as_secs_f64();
    let summary = outcome.summary;
    let hist = summary.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
    let rows: Vec<Value> = hist
        .iter()
        .enumerate()
        .map(|(i, r)| json!({"i": i, "L": r.get("L"), "T_peak_w_K": r.get("T_peak_w_K"), "V": r.get("V")}))
        .collect();
    let free_start: Map<String, Value> =
        summary.get("free_start").and_then(Value::as_object).cloned().unwrap_or_default();
    let free_final: Map<String, Value> = spec
        .free
        .iter()
        .map(|f| (f.ref_str(), outcome.best.get(&f.slot).map_or_else(|| arr_value(&f.start), arr_value)))
        .collect();
    let mut body = head.as_object().cloned().unwrap_or_default();
    for (k, v) in [
        ("job_dir", json!(out_dir.display().to_string())),
        ("wall_s", json!((wall * 10.0).round() / 10.0)),
        ("from_cache", json!(false)),
        ("peak_rss_mb", summary.get("peak_rss_mb").cloned().unwrap_or(Value::Null)),
        ("iterations", json!(hist.len())),
        ("history", Value::Array(rows.clone())),
        ("L_first", summary.get("L_first").cloned().unwrap_or(Value::Null)),
        ("L_best", summary.get("L_best").cloned().unwrap_or(Value::Null)),
        ("free_start", Value::Object(free_start.clone())),
        ("free_final", Value::Object(free_final.clone())),
    ] {
        body.insert(k.into(), v);
    }
    if as_json {
        println!("{}", dumps_indent(&Value::Object(body)));
        return EXIT_OK;
    }
    println!("  {}", "-".repeat(WIDTH - 2));
    println!();
    println!("  iteration table");
    println!("  {:<4} {:<12} {:<12} V", "i", "L", "T_peak_w [K]");
    for r in &rows {
        println!(
            "  {:<4} {:<12} {:<12} {}",
            r["i"].as_u64().unwrap_or(0),
            f6(&r["L"], 6),
            f6(&r["T_peak_w_K"], 1),
            f6(&r["V"], 5)
        );
    }
    println!();
    println!("  what moved");
    println!("  {:<42} {:<14} {:<14} units", "free parameter", "start", "final");
    for fr in &spec.free {
        let k = fr.ref_str();
        let (sa, sb) = (flat(free_start.get(&k)), flat(free_final.get(&k)));
        println!(
            "  {:<42} {:<14} {:<14} {}",
            clip(&k, 42),
            num_text(free_start.get(&k)),
            num_text(free_final.get(&k)),
            fr.units
        );
        if sa.len() > 1 && sa.len() == sb.len() {
            let mn = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
            let mx = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let moved = sa.iter().zip(&sb).filter(|(x, y)| (*y - *x).abs() > 1e-12).count();
            println!(
                "  {:<42}   range {} .. {} -> {} .. {}, {moved} of {} moved",
                "",
                implexity_geometry::pyfmt::fmt_g(mn(&sa), 4),
                implexity_geometry::pyfmt::fmt_g(mx(&sa), 4),
                implexity_geometry::pyfmt::fmt_g(mn(&sb), 4),
                implexity_geometry::pyfmt::fmt_g(mx(&sb), 4),
                sa.len()
            );
        }
    }
    if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
        println!();
        if let (Some(l0), Some(l1)) =
            (first["L"].as_f64().filter(|v| *v != 0.0), last["L"].as_f64().filter(|v| *v != 0.0))
        {
            println!(
                "  L           {l0:.6} -> {l1:.6}  ({:+.1} %), best {:.6}",
                100.0 * (l1 / l0 - 1.0),
                summary.get("L_best").and_then(Value::as_f64).unwrap_or(l1)
            );
        }
        if let (Some(t0), Some(t1)) = (
            first["T_peak_w_K"].as_f64().filter(|v| *v != 0.0),
            last["T_peak_w_K"].as_f64().filter(|v| *v != 0.0),
        ) {
            println!("  T_peak_w    {t0:.1} -> {t1:.1} K  ({:+.1} K)", t1 - t0);
        }
    }
    println!(
        "  {wall:.1} s wall, peak RSS {} MB",
        implexity_core::pyobj::py_str(summary.get("peak_rss_mb").unwrap_or(&Value::Null))
    );
    println!("  job record  {}", out_dir.display());
    EXIT_OK
}

fn flat(v: Option<&Value>) -> Vec<f64> {
    fn walk(v: &Value, out: &mut Vec<f64>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            other => out.push(other.as_f64().unwrap_or(f64::NAN)),
        }
    }
    let mut out = Vec::new();
    if let Some(v) = v {
        walk(v, &mut out);
    }
    out
}

fn num_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::Array(_)) => {
            let mut shape = Vec::new();
            let mut cur = v;
            while let Some(Value::Array(a)) = cur {
                shape.push(a.len());
                cur = a.first();
            }
            format!("array{}", super::inspect::shape_repr(&shape))
        }
        Some(x) => x.as_f64().map_or_else(|| "None".into(), g),
        None => "None".into(),
    }
}

fn print_problem(head: &Value) {
    let s = |v: &Value| implexity_core::pyobj::py_str(v);
    println!(
        "optimise    {} of {}",
        s(&head["node"]),
        implexity_io::locate::abspath(Path::new(head["file"].as_str().unwrap_or_default())).display()
    );
    println!(
        "model       {}, field class {} -- the promise the occupancy band is measured against",
        s(&head["model"]),
        fc_json(&head["field_class"])
    );
    let grid: Vec<i64> =
        head["grid"].as_array().map_or(Vec::new(), |g| g.iter().filter_map(Value::as_i64).collect());
    println!(
        "case        {}  grid {}  ({} elements)",
        implexity_core::pyobj::repr(&head["case"]),
        s(&head["grid"]),
        grid.iter().product::<i64>()
    );
    println!();
    println!(
        "  free parameters -- one learning rate means {} % of EACH declared range per step",
        g(100.0 * head["settings"]["lr"].as_f64().unwrap_or(f64::NAN))
    );
    println!("  {:<42} {:<10} {:<20} size", "ref", "units", "range");
    for f in head["free"].as_array().into_iter().flatten() {
        println!(
            "  {:<42} {:<10} {:<20} {}",
            clip(&s(&f["ref"]), 42),
            s(&f["units"]),
            format!("[{}, {}]", s(&f["lo"]), s(&f["hi"])),
            f["size"].as_u64().unwrap_or(0)
        );
        if f["scale_from"].as_str() != Some("bounds") {
            println!("  {:<42}   scale from {}", "", s(&f["scale_from"]));
        }
    }
    println!();
    println!("  objective");
    let terms: Vec<&str> =
        head["terms"].as_array().map_or(Vec::new(), |t| t.iter().filter_map(Value::as_str).collect());
    for t in head["objective"]["terms"].as_array().into_iter().flatten() {
        let name = t["term"].as_str().unwrap_or_default();
        println!(
            "    {:<22} weight {:<8} {}",
            name,
            g(t.get("weight").and_then(Value::as_f64).unwrap_or(1.0)),
            if terms.contains(&name) { "SCORED" } else { "not scored" }
        );
    }
    let pr = &head["probe"];
    let fnum = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
    println!();
    println!("  the model in the case box");
    println!(
        "    occupies          {:.4} of the box ({} of {} element centres are in the band)",
        fnum(&pr["V_model"]),
        s(&pr["band_cells"]),
        s(&pr["cells"])
    );
    println!("    on the loaded face {:.4}  (axis {})", fnum(&pr["face_solid"]), s(&pr["face_axis"]));
    println!(
        "    lattice volfrac   target {:.4}, reached {:.4} by {}",
        fnum(&head["lattice"]["target_volfrac"]),
        fnum(&head["lattice"]["reached_volfrac"]),
        s(&head["lattice"]["projection"])
    );
    println!("  prepared in {:.1} s   solve id {}", fnum(&head["prepare_seconds"]), s(&head["solve_id"]));
}
