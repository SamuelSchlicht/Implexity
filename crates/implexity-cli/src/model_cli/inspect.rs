// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use implexity_geometry::ParamValue;
use implexity_geometry::document::{self as D, Model};
use implexity_geometry::errors as E;

use super::{
    EXIT_FAILED, EXIT_OK, EXIT_REJECTED, FC_LEGEND, WIDTH, explain, fc, fc_json, g, load, parse, refuse, wrap,
};
use crate::args::{Kind, Parser, PosN};
use crate::util::{clip, dumps_indent, trunc};

fn base_dir(path: &str) -> std::path::PathBuf {
    let abs = implexity_io::locate::abspath(Path::new(path));
    abs.parent().map_or_else(|| abs.clone(), Path::to_path_buf)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_validate(argv: &[String]) -> u8 {
    let p = Parser::new(
        "implexity model validate",
        "check one or more model documents; report EVERY problem at once, each with the file and the line it is on",
    )
    .epilog(
        "A document that validates is one that BUILDS: the same walk constructs the graph, so an arity error, an unknown constructor argument and a node kind's own invariant are found here and not at the first evaluation.  Exit 0 if every file is acceptable, 3 if any is not.",
    )
    .pos("file", PosN::Some, "model document(s)")
    .opt("--roundtrip", Kind::Flag, 0, "also serialise -> deserialise -> serialise and compare the bytes, the sha256 and both ids")
    .opt("--quiet", Kind::Flag, 0, "print nothing for a file that is acceptable")
    .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let as_json = a.flag("--json");
    let quiet = a.flag("--quiet");
    let files = a.pos("file").to_vec();
    let mut out: Vec<Value> = Vec::new();
    let mut rc = EXIT_OK;
    for path in &files {
        let mut row = serde_json::Map::new();
        row.insert("file".into(), json!(path));
        row.insert("valid".into(), json!(false));
        row.insert("problems".into(), json!([]));
        row.insert("warnings".into(), json!([]));
        let (text, raw) = match E::read_json(Path::new(path), "model document") {
            Ok(v) => v,
            Err(e) => {
                row.insert("problems".into(), json!(e.problems));
                out.push(Value::Object(row));
                rc = EXIT_REJECTED;
                if !as_json {
                    refuse(&format!("{path} cannot be read"), &e.problems, &[], EXIT_REJECTED);
                }
                continue;
            }
        };
        let note = if text.contains("\"optimize\"") { super::activate_physics() } else { None };
        let bdir = base_dir(path);
        let (problems, warnings) = match D::problems_of(&raw, Some(&bdir), None) {
            Ok(pw) => pw,
            Err(e) => (explain(&e, &format!("building {path}")).0, Vec::new()),
        };
        let mut problems = E::with_near_misses(&E::annotate(&problems, &text, Some(path)));
        if let Some(n) = note
            && !problems.is_empty()
        {
            problems.push(format!(
                "the configured physics packages could not be activated here, so the 'optimize' kind may not bind -- {n}"
            ));
        }
        let mut valid = problems.is_empty();
        let mut warnings = warnings;
        row.insert("problems".into(), json!(problems));
        if valid {
            match D::build(&raw, Some(&bdir), None) {
                Ok(m) => {
                    let mut kinds: Vec<String> =
                        m.node_table().values().map(|n| n.kind().to_owned()).collect();
                    kinds.sort();
                    kinds.dedup();
                    row.insert("name".into(), m.doc.get("name").cloned().unwrap_or(Value::Null));
                    row.insert("nodes".into(), json!(m.node_table().len()));
                    row.insert("shared".into(), json!(m.shared().len()));
                    row.insert(
                        "parameters".into(),
                        json!(
                            m.doc
                                .get("parameters")
                                .and_then(Value::as_object)
                                .map_or(0, serde_json::Map::len)
                        ),
                    );
                    row.insert("root".into(), m.doc.get("root").cloned().unwrap_or(Value::Null));
                    row.insert("structure_id".into(), json!(m.structure_id()));
                    row.insert("content_id".into(), json!(m.content_id()));
                    row.insert("sha256".into(), m.sha256().map_or(Value::Null, Value::from));
                    row.insert("bytes".into(), json!(text.len()));
                    row.insert("kinds".into(), json!(kinds));
                }
                Err(e) => {
                    let (probs, _) = explain(&e, &format!("building {path}"));
                    row.insert("problems".into(), json!(probs));
                    valid = false;
                }
            }
        }
        if valid && a.flag("--roundtrip") {
            let r = roundtrip(&raw, &bdir);
            if r["ok"] != json!(true) {
                warnings.push(format!(
                    "this document does not survive a round trip: {}",
                    r["why"].as_str().unwrap_or_default()
                ));
            }
            row.insert("roundtrip".into(), r);
        }
        row.insert("warnings".into(), json!(warnings));
        row.insert("valid".into(), json!(valid));
        let problems: Vec<String> = row["problems"]
            .as_array()
            .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        if !problems.is_empty() {
            rc = EXIT_REJECTED;
        }
        let row = Value::Object(row);
        out.push(row.clone());
        if as_json || (valid && quiet) {
            continue;
        }
        if valid {
            println!("{path}  OK");
            println!(
                "   {} -- {} node(s), {} shared, {} named parameter(s), root {}",
                implexity_core::pyobj::py_str(&row["name"]),
                row["nodes"],
                row["shared"],
                row["parameters"],
                implexity_core::pyobj::repr(&row["root"])
            );
            println!(
                "   structure {}   content {}   sha256 {}",
                implexity_core::pyobj::py_str(&row["structure_id"]),
                implexity_core::pyobj::py_str(&row["content_id"]),
                row["sha256"].as_str().map_or(String::new(), |s| s.chars().take(16).collect())
            );
            let kinds: Vec<&str> =
                row["kinds"].as_array().map_or(Vec::new(), |k| k.iter().filter_map(Value::as_str).collect());
            println!("   kinds: {}", kinds.join(", "));
            if a.flag("--roundtrip") {
                println!("   round trip: {}", row["roundtrip"]["detail"].as_str().unwrap_or_default());
            }
            for w in row["warnings"].as_array().into_iter().flatten() {
                wrap("   WARNING  ", w.as_str().unwrap_or_default());
            }
        } else {
            refuse(&format!("{path} -- {} problem(s)", problems.len()), &problems, &[], EXIT_REJECTED);
        }
    }
    if as_json {
        println!("{}", dumps_indent(&json!({"files": out, "ok": rc == EXIT_OK})));
    } else if rc == EXIT_OK && !quiet {
        println!();
        println!("{} file(s) checked, all acceptable.", files.len());
    }
    rc
}

fn roundtrip(raw: &Value, bdir: &Path) -> Value {
    match D::check_roundtrip(raw, Some(bdir), None) {
        Err(implexity_geometry::GeometryError::ModelDoc(p)) => json!({
            "ok": false,
            "why": format!("re-reading what this document serialises to is refused: {}", p.iter().take(3).cloned().collect::<Vec<_>>().join("; ")),
            "detail": format!("FAILED -- {}", p.iter().take(2).cloned().collect::<Vec<_>>().join("; ")),
            "problems": p,
        }),
        Err(e) => json!({
            "ok": false,
            "why": format!("{}: {e}", crate::util::geo_class(&e)),
            "detail": format!("FAILED -- {}: {e}", crate::util::geo_class(&e)),
        }),
        Ok(r) => {
            let t = |k: &str| r[k].as_bool() == Some(true);
            let ok = t("bytes_equal") && t("content_id_equal") && t("structure_id_equal");
            let py = |b: bool| if b { "True" } else { "False" };
            json!({
                "ok": ok,
                "detail": if ok {
                    format!("{} bytes, byte-identical, content_id {} kept", r["bytes"], implexity_core::pyobj::py_str(&r["content_id"]))
                } else {
                    format!("bytes_equal={} content_id_equal={} structure_id_equal={}", py(t("bytes_equal")), py(t("content_id_equal")), py(t("structure_id_equal")))
                },
                "why": "the second serialisation differs from the first",
                "bytes": r["bytes"], "content_id": r["content_id"], "structure_id": r["structure_id"],
                "nodes": r["nodes"], "shared": r["shared"],
            })
        }
    }
}

fn param_text(val: Option<&ParamValue>, default: Option<&ParamValue>) -> (String, bool) {
    let Some(val) = val else { return ("None".into(), false) };
    match val.to_f64_array() {
        Ok((shape, data)) if shape.is_empty() && data.len() == 1 => {
            let v = data[0];
            let same = match default {
                None | Some(ParamValue::List(_) | ParamValue::Tuple(_)) => false,
                Some(d) => d.scalar_f64().is_some_and(|dv| (v - dv).abs() < 1e-12),
            };
            (g(v), same)
        }
        Ok((shape, data)) if !data.is_empty() => {
            let min = data.iter().copied().fold(f64::INFINITY, f64::min);
            let max = data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mean = implexity_geometry::numpy::mean(&data);
            (
                format!(
                    "array{} min {} max {} mean {}",
                    shape_repr(&shape),
                    implexity_geometry::pyfmt::fmt_g(min, 4),
                    implexity_geometry::pyfmt::fmt_g(max, 4),
                    implexity_geometry::pyfmt::fmt_g(mean, 4)
                ),
                false,
            )
        }
        _ => (val.py_obj().repr(), false),
    }
}

pub(crate) fn shape_repr(shape: &[usize]) -> String {
    format!("[{}]", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
}

fn param_lines(model: &Model, nid: &str, everything: bool) -> Vec<String> {
    let Some(node) = model.node_table().get(nid) else { return Vec::new() };
    let binds = model.bindings().get(nid);
    let stored = &model.doc["nodes"][nid]["params"];
    let mut out = Vec::new();
    for name in node.info().sorted_param_names() {
        let Some(spec) = node.info().param(&name) else { continue };
        let show = everything || binds.is_some_and(|b| b.contains_key(&name));
        let (txt, same) = param_text(node.param(&name), spec.default.as_ref());
        if !show && same {
            continue;
        }
        let raw = stored.get(&name);
        let src = match raw.and_then(Value::as_object) {
            Some(m) if m.contains_key("bind") => {
                format!("  <- {}", implexity_core::pyobj::py_str(&m["bind"]))
            }
            Some(m) if m.contains_key("expr") => {
                format!("  <- {}", implexity_core::pyobj::py_str(&m["expr"]))
            }
            Some(m) if m.contains_key("array") => {
                format!("  <- arrays.{}", implexity_core::pyobj::py_str(&m["array"]))
            }
            _ => match binds.and_then(|b| b.get(&name)) {
                Some(b) => format!("  <- {}", b.kind()),
                None => String::new(),
            },
        };
        out.push(format!("{name} = {txt} [{}]{src}", spec.units));
    }
    out
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_show(argv: &[String]) -> u8 {
    let p = Parser::new(
        "implexity model show",
        "the graph as a tree: every node's kind, its field class and its parameters, with shared subtrees marked",
    )
    .epilog(
        "A shared node is printed ONCE, expanded, and its other parents show a back-reference -- because there is one node, not several copies.  The field-class column is the promise each node makes about f(x) away from the surface; see docs/IMPLICIT_CAE.md section 2.",
    )
    .pos("file", PosN::One, "")
    .opt("--node", Kind::Str, 1, "root the tree at this node id or output alias")
    .opt("--params", Kind::Flag, 0, "show every parameter of every node, not just the ones that are bound or non-default")
    .opt("--header", Kind::Flag, 0, "print the document's teaching header (meta.header) and stop")
    .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let file = a.pos1("file").unwrap_or_default().to_owned();
    let mut loaded = match load(&file, None) {
        Ok(l) => l,
        Err(c) => return c,
    };
    if a.flag("--header") {
        let hdr =
            loaded.model.doc.get("meta").and_then(|m| m.get("header")).and_then(Value::as_array).cloned();
        match hdr.filter(|h| !h.is_empty()) {
            Some(lines) => {
                for l in lines {
                    println!("{}", implexity_core::pyobj::py_str(&l));
                }
            }
            None => println!(
                "{}",
                loaded
                    .model
                    .doc
                    .get("doc")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("(this document carries no header)")
            ),
        }
        return EXIT_OK;
    }
    let model = &mut loaded.model;
    let classes = model.field_classes();
    let mut root_id = model.doc.get("root").and_then(Value::as_str).unwrap_or_default().to_owned();
    if let Some(n) = a.str("--node") {
        match model.node(n) {
            Ok(node) => root_id = super::id_of(model, &node),
            Err(e) => {
                let (probs, hints) = explain(&e, "selecting a node");
                return refuse("no such node", &probs, &hints, EXIT_REJECTED);
            }
        }
    }
    let warnings = loaded.warnings.clone();
    if a.flag("--json") {
        let describe = model.describe();
        let out = json!({
            "name": model.doc.get("name"), "doc": model.doc.get("doc"), "root": root_id,
            "graph": describe, "shared": model.shared(), "parameters": model.parameter_table(),
            "structure_id": model.structure_id(), "content_id": model.content_id(), "warnings": warnings,
        });
        println!("{}", dumps_indent(&out));
        return EXIT_OK;
    }
    let doc = model.doc.clone();
    println!("model  {}", implexity_io::locate::abspath(Path::new(&file)).display());
    println!(
        "       {}  {}  {} node(s), {} shared, {} bytes",
        implexity_core::pyobj::repr(doc.get("name").unwrap_or(&Value::Null)),
        implexity_core::pyobj::py_str(doc.get("schema").unwrap_or(&Value::Null)),
        model.node_table().len(),
        model.shared().len(),
        loaded.text.len()
    );
    println!(
        "       structure {}   content {}   root {}",
        model.structure_id().unwrap_or_else(|| "None".into()),
        model.content_id().unwrap_or_else(|| "None".into()),
        implexity_core::pyobj::repr(doc.get("root").unwrap_or(&Value::Null))
    );
    if let Some(d) = doc.get("doc").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        wrap("       ", d);
    }
    println!();
    let mut alias: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(o) = doc.get("outputs").and_then(Value::as_object) {
        for (a, nid) in o {
            alias.entry(implexity_core::pyobj::py_str(nid)).or_default().push(a.clone());
        }
    }
    let everything = a.flag("--params");
    let line = |nid: &str, prefix: &str, connector: &str| {
        let Some(node) = model.node_table().get(nid) else { return };
        let n_parents = model.parents().get(nid).map_or(0, Vec::len);
        let head = format!("{prefix}{connector}{nid}");
        let mut tag = String::new();
        if n_parents > 1 {
            tag = format!("  shared by {n_parents}");
        }
        if let Some(al) = alias.get(nid).filter(|v| !v.is_empty()) {
            let mut al = al.clone();
            al.sort();
            let _ = write!(tag, "  ->{}", al.join(","));
        }
        println!("{:<38} {:<16} {:<13}{tag}", trunc(&head, 38), node.kind(), fc(classes.get(nid)));
        for pline in param_lines(model, nid, everything) {
            println!("{:<38} {pline}", "");
        }
    };
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    seen.insert(root_id.clone());
    line(&root_id, "", "");
    walk_tree(model, &root_id, "", &mut seen, &line);
    println!();
    println!("field classes: {FC_LEGEND}");
    let sh = model.shared();
    if !sh.is_empty() {
        println!();
        for (nid, parents) in &sh {
            wrap(
                "shared  ",
                &format!(
                    "{nid} ({}) has {} parents: {} -- ONE node, so an edit to it is seen by all of them",
                    model.node_table().get(nid).map_or("?", |n| n.kind()),
                    parents.len(),
                    parents.join(", ")
                ),
            );
        }
    }
    for w in &warnings {
        wrap("WARNING  ", w);
    }
    EXIT_OK
}

fn walk_tree(
    model: &Model,
    nid: &str,
    pad: &str,
    seen: &mut std::collections::BTreeSet<String>,
    line: &dyn Fn(&str, &str, &str),
) {
    let kids: Vec<Value> = model.doc["nodes"][nid]["children"].as_array().cloned().unwrap_or_default();
    let n = kids.len();
    for (i, c) in kids.iter().enumerate() {
        let last = i + 1 == n;
        let conn = if last { "`- " } else { "|- " };
        let cid = c["node"].as_str().unwrap_or_default().to_owned();
        if seen.contains(&cid) {
            println!("{:<38} -> shared, printed above", trunc(&format!("{pad}{conn}{cid}"), 38));
            continue;
        }
        seen.insert(cid.clone());
        line(&cid, pad, conn);
        walk_tree(model, &cid, &format!("{pad}{}", if last { "   " } else { "|  " }), seen, line);
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_params(argv: &[String]) -> u8 {
    let p = Parser::new("implexity model params", "the document's named parameter table, resolved, with what each one drives")
        .epilog(
            "A named parameter is the number an engineer edits; a NODE parameter binds to it.  `implexity model set` changes the first kind.  --refs lists the second kind, which is what an Optimize node's `free` set names.",
        )
        .pos("file", PosN::One, "")
        .opt("--refs", Kind::Flag, 0, "list every NODE parameter reference in the graph (a/b:name), which is the spelling `free` takes")
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
    let rows = model.parameter_table();
    if a.flag("--refs") {
        let mut refs = Vec::new();
        if let Some(root) = model.root() {
            for (path, node) in root.walk() {
                let nid = super::id_of(model, &node);
                for name in node.info().sorted_param_names() {
                    let units = node.info().param(&name).map(|s| s.units.clone()).unwrap_or_default();
                    let (shape, data) = node
                        .param(&name)
                        .and_then(|v| v.to_f64_array().ok())
                        .unwrap_or_else(|| (vec![0], Vec::new()));
                    let value = if shape.is_empty() && data.len() == 1 {
                        implexity_geometry::value::json_f64(data[0])
                    } else {
                        json!(format!("array{}", shape_repr(&shape)))
                    };
                    refs.push(json!({
                        "ref": format!("{}:{name}", path.join("/")), "node": nid, "kind": node.kind(),
                        "param": name, "units": units, "size": data.len(),
                        "discrete": node.info().discrete.contains(&name), "value": value,
                    }));
                }
            }
        }
        if a.flag("--json") {
            println!("{}", dumps_indent(&json!({"refs": refs})));
            return EXIT_OK;
        }
        println!("every node parameter, as an Optimize node's `free` set names them");
        println!("{}", "-".repeat(WIDTH));
        println!("  {:<40} {:<9} {:<10} note", "ref", "units", "value");
        for r in &refs {
            let s = |k: &str| r[k].as_str().unwrap_or_default().to_owned();
            let note = if r["discrete"] == json!(true) {
                "DISCRETE -- no derivative".to_owned()
            } else {
                format!("{} of {}", s("kind"), s("node"))
            };
            let val =
                r["value"].as_str().map_or_else(|| g(r["value"].as_f64().unwrap_or(f64::NAN)), str::to_owned);
            println!("  {:<40} {:<9} {:<10} {}", clip(&s("ref"), 40), s("units"), val, clip(&note, 24));
        }
        println!("{}", "-".repeat(WIDTH));
        println!("  a ref is rooted at the node you name it from.  Inside an Optimize node the");
        println!("  model is the child called `model`, so the same parameter is `model/<ref>`.");
        return EXIT_OK;
    }
    if a.flag("--json") {
        println!(
            "{}",
            dumps_indent(
                &json!({"parameters": rows, "warnings": loaded.warnings, "content_id": model.content_id()})
            )
        );
        return EXIT_OK;
    }
    println!("named parameters of {}", implexity_io::locate::abspath(Path::new(&file)).display());
    println!("{}", "-".repeat(WIDTH));
    println!("  {:<12} {:>10} {:<6} {:<15} drives", "name", "value", "units", "range");
    for r in &rows {
        let py = implexity_core::pyobj::py_str;
        let mut rng = String::new();
        if r.get("min").is_some() || r.get("max").is_some() {
            let side = |k: &str| r.get(k).map_or_else(|| "-".to_owned(), py);
            rng = format!("[{}, {}]", side("min"), side("max"));
        }
        if r.get("free").is_some_and(implexity_core::pyobj::truthy) {
            rng.push_str(" FREE");
        }
        let drives: Vec<String> = r["binds"]
            .as_array()
            .map_or(Vec::new(), |b| b.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect());
        let derived = r["derived"] == json!(true);
        let val = if derived {
            "(derived)".to_owned()
        } else {
            r["value"].as_f64().map_or_else(|| "?".to_owned(), g)
        };
        let drive_text = if drives.is_empty() {
            "nothing".to_owned()
        } else {
            clip(
                &format!(
                    "{}: {}",
                    drives.len(),
                    drives.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ),
                24,
            )
        };
        println!("  {:<12} {val:>10} {:<6} {rng:<15} {drive_text}", py(&r["name"]), py(&r["units"]));
        if derived {
            println!("  {:<12} {:>10}   = {}", "", "", py(r.get("expr").unwrap_or(&Value::Null)));
        }
        if let Some(d) = r.get("doc").filter(|d| implexity_core::pyobj::truthy(d)) {
            wrap("               ", &py(d));
        }
    }
    println!("{}", "-".repeat(WIDTH));
    println!("  set one:  implexity model set {file} <name>=<value> -o out.json");
    for w in &loaded.warnings {
        wrap("  WARNING  ", w);
    }
    EXIT_OK
}

fn module_order(m: &str) -> u8 {
    match m {
        "primitives" => 0,
        "ops" => 1,
        "lattice_ops" => 2,
        "interop" => 3,
        "optimize" => 4,
        _ => 9,
    }
}

fn module_note(m: &str) -> &'static str {
    match m {
        "primitives" => "  -- the leaves; every one an exact SDF where it says EXACT",
        "ops" => "  -- the algebra; each with an argued field class",
        "lattice_ops" => "  -- TPMS families and the Lipschitz measurement",
        "interop" => "  -- the outside world in: a mesh, a sampled field",
        "optimize" => "  -- gradient descent, as a node",
        _ => "",
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_catalogue(argv: &[String]) -> u8 {
    let p = Parser::new(
        "implexity model catalogue",
        "every registered node kind: its parameters, their units and defaults, and what it promises about f(x)",
    )
    .epilog(
        "This is read from the REGISTRY and nothing else, so a kind added by a package appears here the moment it registers.  Name a kind for its full entry.",
    )
    .pos("kind", PosN::Optional, "one kind, in full")
    .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let note = super::activate_physics();
    let cat: Vec<Value> = D::catalogue(None).as_array().cloned().unwrap_or_default();
    let by: BTreeMap<String, Value> =
        cat.iter().map(|c| (c["kind"].as_str().unwrap_or_default().to_owned(), c.clone())).collect();
    if let Some(kind) = a.pos1("kind") {
        let Some(c) = by.get(kind) else {
            let names: Vec<String> = by.keys().cloned().collect();
            let hint = E::did_you_mean(kind, &names, E::CUTOFF);
            let mut probs = vec![format!("the {} registered kinds are: {}", by.len(), names.join(", "))];
            if let Some(h) = hint {
                probs.push(format!("did you mean {}?", implexity_core::py_repr::repr_str(&h)));
            }
            return refuse(
                &format!("no node kind {}", implexity_core::py_repr::repr_str(kind)),
                &probs,
                &[],
                EXIT_REJECTED,
            );
        };
        if a.flag("--json") {
            println!("{}", dumps_indent(c));
            return EXIT_OK;
        }
        let s = |v: &Value| implexity_core::pyobj::py_str(v);
        println!("{}      ({})", s(&c["kind"]), s(&c["module"]));
        println!("{}", "-".repeat(WIDTH));
        wrap("  ", &s(&c["doc"]));
        println!();
        let params = c["params"].as_array().cloned().unwrap_or_default();
        if params.is_empty() {
            println!("  (no parameters)");
        } else {
            println!("  {:<16} {:<8} {:<12} meaning", "parameter", "units", "default");
            for q in &params {
                println!(
                    "  {:<16} {:<8} {:<12} {}",
                    s(&q["name"]),
                    s(&q["units"]),
                    clip(&default_text(&q["default"]), 12),
                    clip(&s(&q["doc"]), 30)
                );
            }
        }
        println!();
        let fcn = &c["field_class"];
        println!("  field class -- what this kind promises about f(x) away from");
        println!("                 the zero set");
        match fcn["doc"].as_str().filter(|d| !d.is_empty()) {
            Some(d) => wrap("    ", d),
            None => wrap(
                "    ",
                "this kind states its rule in its own docstring above rather than in a one-line field_class docstring.",
            ),
        }
        if fcn["leaf"].is_object() {
            println!("    as a leaf (no children): {}", fc_json(&fcn["leaf"]));
        } else {
            wrap(
                "    ",
                "its answer depends on its children's classes (and, for a kind with attrs such as `tpms`, on those) so there is no single answer to print here.  `implexity model show <file>` prints the class this kind actually has in a real graph.",
            );
        }
        return EXIT_OK;
    }
    if a.flag("--json") {
        let mut funcs: Vec<&str> = implexity_geometry::document::expr::FUNCS.to_vec();
        funcs.sort_unstable();
        println!(
            "{}",
            dumps_indent(
                &json!({"kinds": cat, "count": cat.len(), "schema": D::SCHEMA, "functions": funcs, "note": note})
            )
        );
        return EXIT_OK;
    }
    println!("{} node kinds        (implexity model catalogue <kind> for one in full)", cat.len());
    println!("{}", "-".repeat(78));
    println!("  {:<22} {:<4} {:<9} what it is", "kind", "par", "as a leaf");
    let mut sorted = cat.clone();
    let module_of = |c: &Value| {
        c["module"].as_str().unwrap_or_default().rsplit('.').next().unwrap_or_default().to_owned()
    };
    sorted.sort_by(|x, y| {
        (module_order(&module_of(x)), x["module"].as_str(), x["kind"].as_str()).cmp(&(
            module_order(&module_of(y)),
            y["module"].as_str(),
            y["kind"].as_str(),
        ))
    });
    let mut current: Option<String> = None;
    for c in &sorted {
        let m = module_of(c);
        if current.as_deref() != Some(m.as_str()) {
            println!("  -- {m}{}", module_note(&m));
            current = Some(m);
        }
        let leaf = &c["field_class"]["leaf"];
        println!(
            "  {:<22} {:<4} {:<9} {}",
            c["kind"].as_str().unwrap_or_default(),
            c["params"].as_array().map_or(0, Vec::len),
            if leaf.is_object() { fc_json(leaf) } else { "-".into() },
            clip(c["doc"].as_str().unwrap_or_default(), 36)
        );
    }
    println!("{}", "-".repeat(78));
    let mut funcs: Vec<&str> = implexity_geometry::document::expr::FUNCS.to_vec();
    funcs.sort_unstable();
    println!("  expressions in a document may call: {}", funcs.join(", "));
    if let Some(n) = note {
        wrap("  NOTE  ", &format!("the configured physics packages could not be activated here: {n}"));
    }
    EXIT_OK
}

fn default_text(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Object(m) if m.contains_key("shape") => format!(
            "array{} {}",
            implexity_core::pyobj::py_str(&m["shape"]),
            implexity_core::pyobj::py_str(m.get("dtype").unwrap_or(&Value::Null))
        ),
        other => implexity_core::pyobj::py_str(other),
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_examples(argv: &[String]) -> u8 {
    let p = Parser::new(
        "implexity model examples",
        "the shipped example models, what each one teaches, and where they are on this machine",
    )
    .epilog(
        "Each example is a runnable document with its explanation in its own meta.header.  --show prints that header; --copy regenerates the three manual geometry templates somewhere you can edit them.",
    )
    .opt("--show", Kind::Str, 1, "print one example's header")
    .metavar(&["KEY"])
    .opt("--copy", Kind::Str, 1, "write the three manual geometry templates into DIR (generated, then validated)")
    .metavar(&["DIR"])
    .opt("--json", Kind::Flag, 0, "");
    let a = match parse(&p, argv) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let manual = implexity_geometry::examples::kernel_examples();
    let keys: Vec<String> = manual.iter().map(|e| e.key.clone()).collect();
    let tree = super::tree_root();
    let d = implexity_geometry::examples::models_dir(tree.as_deref());
    if let Some(dir) = a.str("--copy") {
        let rows = match implexity_geometry::examples::write_all(Path::new(dir), Some(&keys)) {
            Ok(r) => r,
            Err(e) => {
                let (probs, hints) = explain(&e, "generating the examples");
                return refuse("could not write the examples", &probs, &hints, EXIT_FAILED);
            }
        };
        if a.flag("--json") {
            println!("{}", dumps_indent(&json!({"written": rows})));
            return EXIT_OK;
        }
        for r in &rows {
            println!(
                "wrote {:<28} {:>6} bytes  {:>2} nodes  {:>2} parameter(s)",
                implexity_core::pyobj::py_str(&r["path"]),
                r["bytes"].as_u64().unwrap_or(0),
                r["nodes"].as_u64().unwrap_or(0),
                r["parameters"].as_u64().unwrap_or(0)
            );
            for w in r["warnings"].as_array().into_iter().flatten() {
                wrap("      WARNING  ", w.as_str().unwrap_or_default());
            }
        }
        return EXIT_OK;
    }
    if let Some(key) = a.str("--show") {
        let Some(ex) = manual.iter().find(|e| e.key == key || e.filename == key) else {
            let hint = E::did_you_mean(key, &keys, E::CUTOFF);
            let mut probs = vec![format!("the examples are: {}", keys.join(", "))];
            if let Some(h) = hint {
                probs.push(format!("did you mean {}?", implexity_core::py_repr::repr_str(&h)));
            }
            return refuse(
                &format!("no example {}", implexity_core::py_repr::repr_str(key)),
                &probs,
                &[],
                EXIT_REJECTED,
            );
        };
        let doc = ex.document().unwrap_or(Value::Null);
        for line in
            doc.get("meta").and_then(|m| m.get("header")).and_then(Value::as_array).into_iter().flatten()
        {
            println!("{}", implexity_core::pyobj::py_str(line));
        }
        let here = implexity_geometry::examples::path_of(&ex.key, tree.as_deref());
        println!();
        println!(
            "this file: {}",
            here.filter(|h| h.is_file()).map_or_else(
                || "not on this machine -- `implexity model examples --copy DIR` writes it".to_owned(),
                |h| h.display().to_string()
            )
        );
        return EXIT_OK;
    }
    let rows: Vec<Value> = manual
        .iter()
        .map(|e| {
            let mut r = e.describe();
            let path = d.as_ref().map(|d| d.join(&e.filename));
            r["path"] = path.as_ref().map_or(Value::Null, |p| json!(p.display().to_string()));
            r["present"] = json!(path.as_ref().is_some_and(|p| p.is_file()));
            r
        })
        .collect();
    if a.flag("--json") {
        println!(
            "{}",
            dumps_indent(
                &json!({"models_dir": d.as_ref().map(|d| d.display().to_string()), "examples": rows})
            )
        );
        return EXIT_OK;
    }
    println!("the shipped example models");
    println!("{}", "-".repeat(WIDTH));
    if d.is_none() {
        wrap(
            "  ",
            "Manual geometry templates were not found in the selected tree or installed resources. Set IMPLEXITY_HOME to a tree, or IMPLEXITY_MODELS to the directory, or regenerate them anywhere with `implexity model examples --copy DIR`.",
        );
        println!();
    }
    for r in &rows {
        let s = |k: &str| r[k].as_str().unwrap_or_default().to_owned();
        println!(
            "  {:<10} {}{}",
            s("key"),
            s("file"),
            if r["present"] == json!(true) { "" } else { "   (NOT PRESENT)" }
        );
        wrap("             ", &s("title"));
        wrap("    teaches  ", &s("teaches"));
        wrap("    needs    ", &s("needs"));
        for c in r["commands"].as_array().into_iter().flatten() {
            println!("    $ {}", c.as_str().unwrap_or_default());
        }
        println!();
    }
    println!("{}", "-".repeat(WIDTH));
    println!("  implexity model examples --show <key>    the full header");
    println!("  implexity model examples --copy DIR      regenerate them anywhere");
    EXIT_OK
}


