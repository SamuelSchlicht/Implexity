// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

type Bbox = ([f64; 3], [f64; 3]);

use serde_json::{Value, json};

use implexity_geometry::errors as E;
use implexity_geometry::eval::{EvalOptions, eval_points, field_class_of};
use implexity_geometry::node::Mode;
use implexity_geometry::{GeometryError, ParamValue};

use super::{
    EXIT_FAILED, EXIT_MISSING, EXIT_OK, EXIT_REJECTED, box_for, explain, fc, fc_json, fc_meaning, g, id_of,
    load, parse, parse_bbox, refuse, select_node, unsolved_optimise, wrap,
};
use crate::args::{Kind, Parser, PosN, py_float};
use crate::util::{clip, dumps_indent};

fn mode_of(s: Option<&str>) -> Mode {
    if s == Some("smooth") { Mode::Smooth } else { Mode::Exact }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_eval(argv: &[String]) -> u8 {
    let p = Parser::new("implexity model eval", "evaluate the field of any node, at points or on a grid")
        .epilog(
            "f(x) < 0 is INSIDE.  Units are millimetres, in and out.  Examples:\n  implexity model eval m.json --at 0,0,0\n  implexity model eval m.json --node cells --grid 24\n  implexity model eval m.json --grid 16 --bbox -12,-10,-3,12,10,3\n",
        )
        .pos("file", PosN::One, "")
        .opt("--node", Kind::Str, 1, "node id or output alias; default is the root")
        .opt("--at", Kind::Str, 1, "one point in mm; repeatable")
        .append()
        .metavar(&["X,Y,Z"])
        .opt("--grid", Kind::Int, 1, "sample an N x N x N grid over the node's own bounding box (or --bbox)")
        .metavar(&["N"])
        .opt("--bbox", Kind::Str, 1, "the box to sample, in mm")
        .metavar(&["X0,Y0,Z0,X1,Y1,Z1"])
        .opt("--pad", Kind::Float, 1, "grow the node's own box by this many mm (default 1)")
        .opt("--mode", Kind::Str, 1, "sharp booleans (export) or smoothed (optimising)")
        .choices(&["exact", "smooth"])
        .opt("--smooth-r", Kind::Float, 1, "the smoothing radius in smooth mode")
        .metavar(&["MM"])
        .opt("--section", Kind::Flag, 0, "print a mid-z ASCII section of the grid (default on for grids up to 64)")
        .opt("--no-section", Kind::Flag, 0, "")
        .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let file = a.pos1("file").unwrap_or_default().to_owned();
    let loaded = match load(&file, None) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let model = &loaded.model;
    let node = match select_node(model, a.str("--node")) {
        Ok(n) => n,
        Err(c) => return c,
    };
    let Some(node) = node else {
        return refuse(
            "this document has no root",
            &["a document without a root has no field to evaluate; pass --node <id>, or add a \"root\" to the document".into()],
            &[],
            EXIT_REJECTED,
        );
    };
    if let Some(c) = unsolved_optimise(model, Some(&node), &file, "eval: evaluating") {
        return c;
    }
    let nid = id_of(model, &node);
    let mode_s = a.str("--mode").unwrap_or("exact").to_owned();
    let mode = mode_of(Some(&mode_s));
    let as_json = a.flag("--json");
    let (pts, shape, bbox): (Vec<[f64; 3]>, Option<usize>, Option<Bbox>) = if a.strs("--at").is_empty() {
        let n = a.int("--grid").unwrap_or(16);
        if !(2..=128).contains(&n) {
            return refuse(
                "--grid out of range",
                &[format!(
                    "--grid {n}; this command samples between 2 and 128 per axis ({n}^3 = {} points).  A body extraction is `implexity model export`, which streams in slabs",
                    n.saturating_mul(n).saturating_mul(n)
                )],
                &[],
                EXIT_REJECTED,
            );
        }
        let (lo, hi) = if let Some(b) = a.str("--bbox") {
            match parse_bbox(b) {
                Some(v) => v,
                None => {
                    return refuse(
                        "--bbox takes six numbers",
                        &[format!(
                            "--bbox is x0,y0,z0,x1,y1,z1 in mm, got {}",
                            implexity_core::py_repr::repr_str(b)
                        )],
                        &[],
                        EXIT_REJECTED,
                    );
                }
            }
        } else {
            let Some((blo, bhi, note)) = box_for(model, &node) else {
                return refuse(
                    "this node has no extent of its own",
                    &[format!(
                        "{nid} ({}) is defined on all of R^3 -- a plane, a TPMS and a constant have no bounding box, and neither does anything built only out of them -- so only you know which part of it to sample",
                        node.kind()
                    )],
                    &[
                        "pass --bbox x0,y0,z0,x1,y1,z1 in mm".into(),
                        format!(
                            "or evaluate a node that is bounded: `implexity model show {file}` prints the graph"
                        ),
                    ],
                    EXIT_REJECTED,
                );
            };
            if let Some(n) = note
                && !as_json
            {
                wrap("NOTE        ", &n);
            }
            let pad = a.float("--pad").unwrap_or(1.0);
            (blo.map(|v| v - pad), bhi.map(|v| v + pad))
        };
        if !(0..3).all(|k| hi[k] > lo[k]) {
            return refuse(
                "the box is empty",
                &[format!("hi must exceed lo on every axis; got lo {} hi {}", list_f(&lo), list_f(&hi))],
                &[],
                EXIT_REJECTED,
            );
        }
        let n = usize::try_from(n).unwrap_or(2);
        let ax: Vec<Vec<f64>> = (0..3).map(|k| implexity_mesh::numeric::linspace(lo[k], hi[k], n)).collect();
        let mut pts = Vec::with_capacity(n * n * n);
        for &x in &ax[0] {
            for &y in &ax[1] {
                for &z in &ax[2] {
                    pts.push([x, y, z]);
                }
            }
        }
        (pts, Some(n), Some((lo, hi)))
    } else {
        let mut pts = Vec::new();
        for spec in a.strs("--at") {
            let parts: Vec<String> =
                spec.replace(' ', ",").split(',').filter(|q| !q.is_empty()).map(str::to_owned).collect();
            if parts.len() != 3 {
                return refuse(
                    "--at takes three numbers",
                    &[format!(
                        "--at {} has {} component(s); a point is x,y,z in mm",
                        implexity_core::py_repr::repr_str(spec),
                        parts.len()
                    )],
                    &[],
                    EXIT_REJECTED,
                );
            }
            let Ok(v) = parts.iter().map(|q| py_float(q)).collect::<Result<Vec<f64>, ()>>() else {
                return refuse(
                    "--at takes three numbers",
                    &[format!("--at {} is not three numbers", implexity_core::py_repr::repr_str(spec))],
                    &[],
                    EXIT_REJECTED,
                );
            };
            pts.push([v[0], v[1], v[2]]);
        }
        (pts, None, None)
    };
    let opts = EvalOptions { mode, smooth_r_mm: a.float("--smooth-r"), ..EvalOptions::default() };
    let t0 = Instant::now();
    let vals = match eval_points(&node, &pts, &opts) {
        Ok(v) => v,
        Err(e) => {
            let (probs, hints) = explain(&e, &format!("evaluating {nid}"));
            return refuse("evaluation failed", &probs, &hints, EXIT_FAILED);
        }
    };
    let secs = t0.elapsed().as_secs_f64();
    let how = "implexity_geometry::eval::eval_points (compiled closure tree, float64)";
    let fcl = field_class_of(&node, mode).ok();
    let inside_n = vals.iter().filter(|v| **v < 0.0).count();
    #[allow(clippy::cast_precision_loss)]
    let inside = if vals.is_empty() { f64::NAN } else { inside_n as f64 / vals.len() as f64 };
    let vmin = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let vmax = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let shared = model.parents().get(&nid).is_some_and(|v| v.len() > 1);
    let mut out = json!({
        "file": file, "node": nid, "kind": node.kind(), "mode": mode_s, "points": pts.len(),
        "seconds": (secs * 1e4).round() / 1e4, "evaluator": how,
        "field_class": fcl.as_ref().map_or(Value::Null, implexity_geometry::FieldClass::as_json),
        "structure_id": node.structure_id(), "content_id": node.content_id(), "shared": shared,
        "value_range": [jf(vmin), jf(vmax)], "inside_fraction": jf(inside), "fallback": null,
    });
    if let (Some(n), Some((lo, hi))) = (shape, bbox) {
        out["shape"] = json!([n, n, n]);
        out["bbox_mm"] = json!([
            lo.iter().map(|v| jf(*v)).collect::<Vec<_>>(),
            hi.iter().map(|v| jf(*v)).collect::<Vec<_>>()
        ]);
    } else {
        out["points_mm"] =
            json!(pts.iter().map(|p| p.iter().map(|v| jf(*v)).collect::<Vec<_>>()).collect::<Vec<_>>());
        out["values"] = json!(vals.iter().map(|v| jf(*v)).collect::<Vec<_>>());
    }
    if as_json {
        println!("{}", dumps_indent(&out));
        return EXIT_OK;
    }
    println!("node        {nid} ({}){}", node.kind(), if shared { "  [shared]" } else { "" });
    println!("field class {}   -- {}", fc(fcl.as_ref()), fc_meaning(fcl.as_ref()));
    println!(
        "mode        {mode_s}{}",
        a.float("--smooth-r").map_or_else(String::new, |r| format!("  smoothing radius {} mm", g(r)))
    );
    println!("evaluator   {how}");
    if let (Some(n), Some((lo, hi))) = (shape, bbox) {
        println!();
        println!("grid        {n} x {n} x {n} = {} points over", pts.len());
        println!(
            "            [{:.3}, {:.3}, {:.3}] .. [{:.3}, {:.3}, {:.3}] mm",
            lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]
        );
        println!("f range     {vmin:.6} .. {vmax:.6} mm");
        println!("inside      {:.2} % of samples ({inside_n} of {})", 100.0 * inside, pts.len());
        let want = if a.flag("--no-section") {
            false
        } else if a.flag("--section") {
            true
        } else {
            n <= 64
        };
        if want {
            println!();
            section(&vals, n, lo, hi);
        }
    } else {
        println!();
        for (pt, v) in pts.iter().zip(&vals) {
            println!(
                "  f({:7.3}, {:7.3}, {:7.3}) = {v:11.6} mm   {}",
                pt[0],
                pt[1],
                pt[2],
                if *v < 0.0 {
                    "INSIDE"
                } else if v.abs() < 1e-9 {
                    "on the surface"
                } else {
                    "outside"
                }
            );
        }
    }
    println!("seconds     {secs:.4}");
    if fcl.as_ref().is_some_and(|f| f.safe_step_factor().is_none()) {
        wrap(
            "NOTE        ",
            "this field is IMPLICIT: only the sign and the zero set mean anything, so no cell can be proven surface-free and the sampling density is the only thing standing between this grid and a missed feature.",
        );
    }
    EXIT_OK
}

fn jf(v: f64) -> Value {
    implexity_geometry::value::json_f64(v)
}

fn list_f(v: &[f64; 3]) -> String {
    format!("[{}]", v.iter().map(|x| implexity_core::py_repr::repr_float(*x)).collect::<Vec<_>>().join(", "))
}

const RAMP: &[u8] = b"#@%*+=-:. ";

fn section(vals: &[f64], n: usize, lo: [f64; 3], hi: [f64; 3]) {
    let rows = 21usize;
    let nz = n;
    let at = |i: usize, j: usize| vals[(i * n + j) * n + nz / 2];
    let (nx, ny) = (n, n);
    let sy = (ny / rows).max(1);
    let sx = (nx / (2 * rows)).max(1);
    let mut lo_v = -1e-9f64;
    for i in 0..nx {
        for j in 0..ny {
            lo_v = lo_v.min(at(i, j));
        }
    }
    println!("  mid-z section (z = {:.2} mm), '#' inside", 0.5 * (lo[2] + hi[2]));
    let mut j = ny.cast_signed() - 1;
    while j >= 0 {
        #[allow(clippy::cast_sign_loss)]
        let ju = j as usize;
        let mut line = String::new();
        let mut i = 0;
        while i < nx {
            let v = at(i, ju);
            if v >= 0.0 {
                line.push(' ');
            } else {
                let t = (v / lo_v).clamp(0.0, 0.999);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let k = ((1.0 - t) * (RAMP.len() - 2) as f64) as usize;
                line.push(char::from(RAMP[k.min(RAMP.len() - 2)]));
            }
            i += sx;
        }
        println!("   {line}");
        j -= isize::try_from(sy).unwrap_or(1);
    }
}

fn num(v: &Value) -> String {
    match v {
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), g),
        Value::Bool(b) => g(f64::from(u8::from(*b))),
        Value::Array(_) => {
            let pv = ParamValue::from_json(v);
            match pv.and_then(|p| p.to_f64_array().ok()) {
                Some((shape, data)) if shape.is_empty() && data.len() == 1 => g(data[0]),
                Some((shape, _)) => format!("array{}", super::inspect::shape_repr(&shape)),
                None => implexity_core::pyobj::repr(v),
            }
        }
        other => implexity_core::pyobj::repr(other),
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_set(argv: &[String]) -> u8 {
    let p = Parser::new("implexity model set", "set named parameters and report exactly what moved")
        .epilog(
            "A parameter edit must not recompile: the node objects and their wiring are mutated in place, so structure_id CANNOT change and content_id does.  Both are printed before and after, measured rather than claimed.  Nothing is written unless you pass -o or --in-place.",
        )
        .pos("file", PosN::One, "")
        .pos("assignment", PosN::Any, "a NAMED parameter of the document's table")
        .opt(
            "--node-param",
            Kind::Str,
            1,
            "set one NODE's parameter directly, bypassing the table.  NODE is a node id or an output alias -- not a child path -- and every parent that shares that node sees the new value immediately, because there is one node",
        )
        .append()
        .metavar(&["NODE:PARAM=VALUE"])
        .opt("--out", Kind::Str, 1, "write the edited document here")
        .short("-o")
        .opt("--in-place", Kind::Flag, 0, "overwrite the input file")
        .opt("--dry-run", Kind::Flag, 0, "explicitly change nothing (the default anyway)")
        .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let file = a.pos1("file").unwrap_or_default().to_owned();
    let mut values: BTreeMap<String, f64> = BTreeMap::new();
    let mut bad: Vec<String> = Vec::new();
    for asg in a.pos("assignment") {
        let Some((k, v)) = asg.split_once('=') else {
            bad.push(format!(
                "{} is not an assignment; write NAME=VALUE (for instance wall=1.6)",
                implexity_core::py_repr::repr_str(asg)
            ));
            continue;
        };
        match py_float(v) {
            Ok(x) => {
                values.insert(k.trim().to_owned(), x);
            }
            Err(()) => bad.push(format!(
                "{}={v} -- the value must be a finite number; a named parameter is a number, and a RELATIONSHIP between numbers is an {{\"expr\": ...}} in the document",
                k.trim()
            )),
        }
    }
    let mut direct: Vec<(String, String, f64)> = Vec::new();
    for asg in a.strs("--node-param") {
        let Some((lhs, v)) = asg.split_once('=') else {
            bad.push(format!(
                "{} is not an assignment; write NODE:PARAM=VALUE (for instance bolt:radius_mm=3.0)",
                implexity_core::py_repr::repr_str(asg)
            ));
            continue;
        };
        let r = match E::parse_ref(lhs) {
            Ok(r) => r,
            Err(e) => {
                bad.extend(e.problems);
                continue;
            }
        };
        if r.path.len() != 1 {
            bad.push(format!(
                "--node-param {asg}: {} is a {}, and this option takes a NODE ID (or an output alias) followed by a colon and the parameter, as `bolt:radius_mm`.  A slash-separated CHILD PATH is what an Optimize node's `free` set uses; `implexity model params <file> --refs` prints those, and `implexity model show <file>` prints the node ids.",
                implexity_core::py_repr::repr_str(&if r.path.is_empty() { "(nothing)".to_owned() } else { r.path.join("/") }),
                if r.path.len() > 1 { "child path" } else { "reference with no node before the colon" }
            ));
            continue;
        }
        match py_float(v) {
            Ok(x) => direct.push((r.path[0].clone(), r.name.clone(), x)),
            Err(()) => bad.push(format!("--node-param {asg}: the value must be a finite number")),
        }
    }
    if values.is_empty() && direct.is_empty() && bad.is_empty() {
        bad.push(
            "nothing to set: give NAME=VALUE for a named parameter, or --node-param NODE:PARAM=VALUE for a node's own"
                .into(),
        );
    }
    if !bad.is_empty() {
        return refuse(&format!("{} bad assignment(s)", bad.len()), &bad, &[], EXIT_REJECTED);
    }
    let mut loaded = match load(&file, None) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let model = &mut loaded.model;
    let id0 = (model.structure_id(), model.content_id());
    let mut direct_rep: Vec<Value> = Vec::new();
    for (nid, param, v) in &direct {
        if let Some(bt) = E::binding_text(&model.doc, nid, param) {
            return refuse(
                &format!("{nid}:{param} is bound and was not set"),
                &[format!(
                    "{nid}.{param} takes its value from {bt}, so setting it directly would be silently undone the next time any named parameter is edited -- the binding is re-resolved and overwrites it."
                )],
                &[
                    format!("set what it reads instead:  implexity model set {file} <name>=<value>"),
                    format!("implexity model params {file}  shows every named parameter and what it drives"),
                ],
                EXIT_REJECTED,
            );
        }
        match model.set_node_param(nid, param, &ParamValue::Float(*v)) {
            Ok(r) => direct_rep.push(r),
            Err(GeometryError::ModelDoc(probs)) => {
                return refuse(
                    &format!("{} node parameter(s) refused", probs.len()),
                    &E::with_near_misses(&probs),
                    &[format!("implexity model show {file}  prints every node id and its parameters")],
                    EXIT_REJECTED,
                );
            }
            Err(e) => {
                let (probs, hints) = explain(&e, &format!("setting {nid}:{param}"));
                return refuse("the edit was refused", &probs, &hints, EXIT_REJECTED);
            }
        }
    }
    let t0 = Instant::now();
    let mut rep = if values.is_empty() {
        json!({
            "set": {}, "moved": [], "parameters": {}, "warnings": model.warnings,
            "structure_id": model.structure_id(), "structure_id_before": id0.0,
            "recompiled": model.structure_id() != id0.0,
            "content_id": model.content_id(), "content_id_before": id0.1, "seconds": 0.0,
        })
    } else {
        let vals: BTreeMap<String, Value> = values.iter().map(|(k, v)| (k.clone(), jf(*v))).collect();
        match model.set_parameters(&vals) {
            Ok(mut r) => {
                r["seconds"] = jf(t0.elapsed().as_secs_f64());
                r
            }
            Err(GeometryError::ModelDoc(probs)) => {
                return refuse(
                    &format!("{} parameter(s) refused", probs.len()),
                    &E::with_near_misses(&probs),
                    &[format!("implexity model params {file}  lists every name this document declares")],
                    EXIT_REJECTED,
                );
            }
            Err(e) => {
                let (probs, hints) = explain(&e, "setting parameters");
                return refuse("the edit was refused", &probs, &hints, EXIT_REJECTED);
            }
        }
    };
    rep["node_params"] = Value::Array(direct_rep.clone());
    let as_json = a.flag("--json");
    if as_json {
        println!("{}", dumps_indent(&rep));
    } else {
        let mut parts: Vec<String> = values.iter().map(|(k, v)| format!("{k} = {}", g(*v))).collect();
        parts.extend(
            direct_rep.iter().zip(&direct).map(|(d, (_, p, v))| {
                format!("{}:{p} = {}", implexity_core::pyobj::py_str(&d["node"]), g(*v))
            }),
        );
        println!("set  {}", parts.join(", "));
        for d in &direct_rep {
            println!();
            println!(
                "  {} was set DIRECTLY on the node, bypassing the table; it has {} parent(s){}",
                implexity_core::pyobj::py_str(&d["node"]),
                d["parents"].as_array().map_or(0, Vec::len),
                if d["shared"] == json!(true) { " and IS SHARED, so all of them see it" } else { "" }
            );
        }
        if !values.is_empty() {
            println!();
            println!("  {:<28} {:<12} {:<12} units", "node parameter", "was", "now");
            let moved = rep["moved"].as_array().cloned().unwrap_or_default();
            for mv in &moved {
                println!(
                    "  {:<28} {:<12} {:<12} {:<5}{}",
                    clip(
                        &format!(
                            "{}.{}",
                            implexity_core::pyobj::py_str(&mv["node"]),
                            implexity_core::pyobj::py_str(&mv["param"])
                        ),
                        28
                    ),
                    num(&mv["was"]),
                    num(&mv["now"]),
                    implexity_core::pyobj::py_str(&mv["units"]),
                    if mv["shared"] == json!(true) { "  SHARED" } else { "" }
                );
            }
            if moved.is_empty() {
                println!("  (nothing moved: every bound value is already what you asked for)");
            }
        }
        println!();
        let s = |k: &str| implexity_core::pyobj::py_str(&rep[k]);
        println!(
            "  structure_id  {} -> {}   {}",
            s("structure_id_before"),
            s("structure_id"),
            if rep["recompiled"] == json!(true) {
                "RECOMPILED"
            } else {
                "unchanged, so the compiled kernel is kept"
            }
        );
        println!(
            "  content_id    {} -> {}   (the provenance key moves, as it must)",
            s("content_id_before"),
            s("content_id")
        );
        if !values.is_empty() {
            println!(
                "  {:.0} us to push the edit through the graph",
                1e6 * rep["seconds"].as_f64().unwrap_or(0.0)
            );
        }
        for w in rep["warnings"].as_array().into_iter().flatten() {
            wrap("  WARNING  ", w.as_str().unwrap_or_default());
        }
    }
    let target = a.str("--out").map(str::to_owned).or_else(|| a.flag("--in-place").then(|| file.clone()));
    let Some(target) = target.filter(|_| !a.flag("--dry-run")) else {
        if !as_json {
            println!();
            println!("  nothing was written.  Add -o FILE to save it, or --in-place to overwrite {file}");
        }
        return EXIT_OK;
    };
    let base = implexity_io::locate::abspath(Path::new(&file));
    let bdir = base.parent().map(Path::to_path_buf);
    let text = match model.to_doc().and_then(|d| {
        let text = implexity_geometry::document::dumps(&d);
        let reread: Value = serde_json::from_str(&text).map_err(|e| GeometryError::Value(e.to_string()))?;
        implexity_geometry::document::build(&reread, bdir.as_deref(), None).map(|_| text)
    }) {
        Ok(t) => t,
        Err(e) => {
            let (probs, hints) = explain(&e, "re-reading what this document would be saved as");
            let mut all = vec![
                "writing this document would produce a file that cannot be read back, so nothing was saved.  What it serialises to was rejected:".to_owned(),
            ];
            all.extend(probs);
            let mut hints = hints;
            hints.push(format!(
                "the graph in memory is fine: `implexity model show {file}` and `implexity model eval {file}` work"
            ));
            return refuse("the edit is correct and was NOT written", &all, &hints, EXIT_FAILED);
        }
    };
    let tmp = format!("{target}.tmp");
    if let Err(e) = std::fs::write(&tmp, text.as_bytes()).and_then(|()| std::fs::rename(&tmp, &target)) {
        let _ = std::fs::remove_file(&tmp);
        return refuse("the edit was NOT written", &[format!("{target}: {e}")], &[], EXIT_FAILED);
    }
    if !as_json {
        println!();
        println!("  wrote {target} ({} bytes)", std::fs::metadata(&target).map_or(0, |m| m.len()));
    }
    EXIT_OK
}

fn fmt_of(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "stl" => Some("stl"),
        "ply" => Some("ply"),
        "3mf" => Some("3mf"),
        "step" | "stp" => Some("step"),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_export(argv: &[String]) -> u8 {
    let p = Parser::new("implexity model export", "a node's zero set as a mesh (stl, ply, 3mf) or a STEP B-rep solid")
        .epilog(
            "The mesher is bodyexport's own slabbed marching cubes and the writers are its writers -- there is no second marching cubes here.  A STEP solid needs a watertight mesh; it is checked and reported before anything is written.",
        )
        .pos("file", PosN::One, "")
        .opt("--out", Kind::Str, 1, "output path")
        .short("-o")
        .required()
        .opt("--node", Kind::Str, 1, "node id or output alias; default is the root")
        .opt("--format", Kind::Str, 1, "default: from the output extension")
        .choices(&["stl", "ply", "3mf", "step"])
        .opt("--spacing", Kind::Float, 1, "sample spacing; the wall you care about needs three or four samples across it")
        .metavar(&["MM"])
        .opt("--bbox", Kind::Str, 1, "")
        .metavar(&["X0,Y0,Z0,X1,Y1,Z1"])
        .opt("--pad", Kind::Float, 1, "grow the box by this (default 2 x spacing)")
        .metavar(&["MM"])
        .opt("--mode", Kind::Str, 1, "")
        .choices(&["exact", "smooth"])
        .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let file = a.pos1("file").unwrap_or_default().to_owned();
    let out_path = a.str("--out").unwrap_or_default().to_owned();
    let loaded = match load(&file, None) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let model = &loaded.model;
    let node = match select_node(model, a.str("--node")) {
        Ok(Some(n)) => n,
        Ok(None) => {
            return refuse(
                "this document has no root",
                &["a document without a root has nothing to export; pass --node <id>".into()],
                &[],
                EXIT_REJECTED,
            );
        }
        Err(c) => return c,
    };
    if let Some(c) = unsolved_optimise(model, Some(&node), &file, "export: meshing") {
        return c;
    }
    let as_json = a.flag("--json");
    let mut bbox = None;
    if let Some(b) = a.str("--bbox") {
        match parse_bbox(b) {
            Some(v) => bbox = Some(v),
            None => {
                return refuse(
                    "--bbox takes six numbers",
                    &[format!(
                        "--bbox is x0,y0,z0,x1,y1,z1 in mm, got {}",
                        implexity_core::py_repr::repr_str(b)
                    )],
                    &[],
                    EXIT_REJECTED,
                );
            }
        }
    }
    let mut box_note = None;
    if bbox.is_none()
        && let Some((lo, hi, note)) = box_for(model, &node)
        && let Some(n) = note
    {
        bbox = Some((lo, hi));
        if !as_json {
            wrap("NOTE        ", &n);
        }
        box_note = Some(n);
    }
    let fmt = a.str("--format").or_else(|| fmt_of(&out_path));
    let Some(fmt) = fmt else {
        return refuse(
            "no format",
            &[format!(
                "{} has no extension this recognises and --format was not given; the formats are {}",
                implexity_core::py_repr::repr_str(&out_path),
                implexity_mesh::interop::EXPORT_FORMATS.join(", ")
            )],
            &[],
            EXIT_REJECTED,
        );
    };
    let avail = implexity_mesh::interop::export_available();
    if let Some(v) = avail.get(fmt)
        && v["available"] == json!(false)
    {
        return refuse(
            &format!("{fmt} cannot be written here"),
            &[format!("{fmt}: {}", implexity_core::pyobj::py_str(&v["detail"]))],
            &[
                "export stl or ply, which need nothing but the kernel".into(),
                "`implexity model doctor` lists every format and what each one needs".into(),
            ],
            EXIT_MISSING,
        );
    }
    let name = model
        .doc
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("implexity body")
        .to_owned();
    let opts = implexity_mesh::interop::ExportOptions {
        fmt: Some(fmt),
        spacing_mm: a.float("--spacing"),
        bbox_mm: bbox,
        pad_mm: a.float("--pad"),
        mode: mode_of(a.str("--mode")),
        name: &name,
        ..implexity_mesh::interop::ExportOptions::default()
    };
    let t0 = Instant::now();
    let mut rep = match implexity_mesh::interop::export_subtree(&node, Path::new(&out_path), &opts) {
        Ok(r) => r,
        Err(e) => {
            return refuse(
                "the export failed",
                &[e.to_string()],
                &[format!("`implexity model show {file}` prints the graph and every node's field class")],
                EXIT_FAILED,
            );
        }
    };
    rep["seconds"] = jf((t0.elapsed().as_secs_f64() * 1e3).round() / 1e3);
    if let Some(n) = &box_note {
        rep["bbox_note"] = json!(n);
    }
    if as_json {
        println!("{}", dumps_indent(&rep));
        return EXIT_OK;
    }
    let nid = id_of(model, &node);
    let fnum = |k: &str| rep[k].as_f64().unwrap_or(f64::NAN);
    println!("export      {nid} -> {out_path}");
    println!("node        {nid} ({})   field class {}", node.kind(), fc_json(&rep["field_class"]));
    let grid: Vec<String> =
        rep["grid"].as_array().map_or(Vec::new(), |g| g.iter().map(ToString::to_string).collect());
    println!(
        "sampled     {} at {:.4} mm = {} points over",
        grid.join("x"),
        fnum("spacing_mm"),
        rep["samples"]
    );
    let corner = |i: usize| -> String {
        let v: Vec<String> = rep["bbox_mm"][i].as_array().map_or(Vec::new(), |c| {
            c.iter().map(|x| format!("'{:.2}'", x.as_f64().unwrap_or(f64::NAN))).collect()
        });
        format!("[{}]", v.join(", "))
    };
    println!("            {} .. {} mm", corner(0), corner(1));
    println!(
        "f range     {:.4} .. {:.4} mm",
        rep["field_range"][0].as_f64().unwrap_or(f64::NAN),
        rep["field_range"][1].as_f64().unwrap_or(f64::NAN)
    );
    if let Some(empty) = rep.get("empty").filter(|e| implexity_core::pyobj::truthy(e)) {
        wrap("EMPTY       ", &implexity_core::pyobj::py_str(empty));
        return EXIT_FAILED;
    }
    println!(
        "mesh        {} triangles, {} vertices, {} degenerate dropped",
        rep["triangles"], rep["vertices"], rep["degenerate_dropped"]
    );
    println!("watertight  {}", if rep["watertight"] == json!(true) { "yes" } else { "NO" });
    if let Some(n) = rep.get("watertight_note").filter(|e| implexity_core::pyobj::truthy(e)) {
        wrap("            ", &implexity_core::pyobj::py_str(n));
    }
    println!("volume      {:.3} mm^3   area {:.3} mm^2", fnum("volume_mm3"), fnum("area_mm2"));
    if let Some(b) = rep.get("brep").filter(|e| implexity_core::pyobj::truthy(e)) {
        println!(
            "B-rep       {}",
            clip(&implexity_core::json::dumps(b, &implexity_core::json::DumpOptions::default()), 300)
        );
    }
    println!(
        "wrote       {} ({} bytes) in {:.2} s",
        implexity_core::pyobj::py_str(&rep["path"]),
        rep["bytes"],
        fnum("seconds")
    );
    if let Some(n) = rep.get("field_class_is_measured").filter(|e| implexity_core::pyobj::truthy(e)) {
        wrap("NOTE        ", &implexity_core::pyobj::py_str(n));
    }
    if let Some(n) = rep.get("meshing_note").filter(|e| implexity_core::pyobj::truthy(e)) {
        wrap("            ", &implexity_core::pyobj::py_str(n));
    }
    EXIT_OK
}
