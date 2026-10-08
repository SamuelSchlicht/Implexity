// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


mod doctor;
mod inspect;
mod optimise;
mod sample;

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use implexity_geometry::document::Model;
use implexity_geometry::errors::{self as E, ModelFileError};
use implexity_geometry::fieldclass::{ClassKind, FieldClass};
use implexity_geometry::{GeometryError, NodeRef};

use crate::args::{Exit, Parsed, Parser};
use crate::util::{wrap_err, wrap_out};

pub(crate) const EXIT_OK: u8 = 0;
pub(crate) const EXIT_CHECK_FAILED: u8 = 1;
pub(crate) const EXIT_USAGE: u8 = 2;
pub(crate) const EXIT_REJECTED: u8 = 3;
pub(crate) const EXIT_FAILED: u8 = 4;
pub(crate) const EXIT_MISSING: u8 = 5;

pub(crate) const WIDTH: usize = 72;

pub(crate) const FC_LEGEND: &str = "EXACT f IS the distance | BOUND |f| <= distance, 1-Lipschitz | LIPSCHITZ(k) f/k is a bound | IMPLICIT sign and zero set only | * the constant was MEASURED, not proven";

pub(crate) fn wrap(prefix: &str, text: &str) {
    wrap_out(prefix, text, WIDTH, 20);
}

pub(crate) fn refuse(headline: &str, problems: &[String], hints: &[String], code: u8) -> u8 {
    eprintln!("implexity model: {headline}");
    for p in problems {
        if p.contains('\n') {
            for line in p.lines() {
                eprintln!("     {line}");
            }
            continue;
        }
        wrap_err(" - ", p, WIDTH + 6, 20);
    }
    if !hints.is_empty() {
        eprintln!();
        for h in hints {
            wrap_err("   try: ", h, WIDTH + 6, 20);
        }
    }
    code
}

pub(crate) fn fc(fc: Option<&FieldClass>) -> String {
    let Some(fc) = fc else { return "?".into() };
    let body = if fc.kind() == ClassKind::Lipschitz {
        format!("LIPSCHITZ({})", implexity_geometry::pyfmt::g(fc.k()))
    } else {
        fc.kind().name().to_owned()
    };
    format!("{body}{}", if fc.measured() { "*" } else { "" })
}

pub(crate) fn fc_json(v: &Value) -> String {
    let Some(kind) = v.get("kind").and_then(Value::as_str) else { return "?".into() };
    let body = if kind == "LIPSCHITZ" {
        format!(
            "LIPSCHITZ({})",
            implexity_geometry::pyfmt::g(v.get("k").and_then(Value::as_f64).unwrap_or(1.0))
        )
    } else {
        kind.to_owned()
    };
    format!("{body}{}", if v.get("measured").is_some_and(|m| m.as_bool() == Some(true)) { "*" } else { "" })
}

pub(crate) fn fc_meaning(fc: Option<&FieldClass>) -> &'static str {
    match fc.map(FieldClass::kind) {
        None => "unknown",
        Some(ClassKind::Exact) => "f IS the signed distance; offset by r is exactly f - r",
        Some(ClassKind::Bound) => "|f| <= distance to the surface, 1-Lipschitz: conservative",
        Some(ClassKind::Lipschitz) => "|grad f| <= k, so f/k is a bound",
        Some(ClassKind::Implicit) => "sign and zero set only; no distance information",
    }
}

pub(crate) fn g(x: f64) -> String {
    implexity_geometry::pyfmt::g(x)
}

pub(crate) fn parse(p: &Parser, argv: &[String]) -> Result<Parsed, u8> {
    p.parse(argv).map_err(|e| match e {
        Exit::Help(t) => {
            print!("{t}");
            EXIT_OK
        }
        Exit::Error(t) => {
            eprint!("{t}");
            EXIT_USAGE
        }
    })
}

pub(crate) fn explain(e: &GeometryError, doing: &str) -> (Vec<String>, Vec<String>) {
    E::explain(e, Some(doing))
}

pub(crate) fn activate_physics() -> Option<String> {
    implexity_bundle::init();
    let mut notes = Vec::new();
    if let Err(e) = implexity_server::startup::initialise_kernel() {
        notes.push(format!("RuntimeError: {e}"));
    }
    implexity_jobs::optimize::node::register_kind();
    if let Err(e) = implexity_core::packages::global()
        .load_configured(std::env::var("IMPLEXITY_PHYSICS_PACKAGES").ok().as_deref())
    {
        notes.push(format!("{}: {}", e.python_class(), e.message()));
    }
    (!notes.is_empty()).then(|| notes.join("; "))
}

pub(crate) struct Loaded {
    pub(crate) model: Model,
    pub(crate) text: String,
    pub(crate) warnings: Vec<String>,
}

fn base_dir(path: &str) -> std::path::PathBuf {
    let abs = implexity_io::locate::abspath(Path::new(path));
    abs.parent().map_or_else(|| abs.clone(), Path::to_path_buf)
}


fn load_inner(path: &str, physics: Option<bool>) -> Result<Loaded, LoadError> {
    let (text, raw) = E::read_json(Path::new(path), "model document").map_err(LoadError::File)?;
    let want = physics.unwrap_or_else(|| text.contains("\"optimize\""));
    let note = if want { activate_physics() } else { None };
    match implexity_geometry::document::build(&raw, Some(&base_dir(path)), None) {
        Ok(model) => {
            let warnings = model.warnings.clone();
            Ok(Loaded { model, text, warnings })
        }
        Err(GeometryError::ModelDoc(problems)) => {
            let mut probs = E::with_near_misses(&E::annotate(&problems, &text, Some(path)));
            if let Some(n) = note {
                probs.push(format!(
                    "the configured physics packages could not be activated, so the 'optimize' kind may not bind here -- {n}"
                ));
            }
            Err(LoadError::File(ModelFileError { problems: probs, path: Some(path.to_owned()) }))
        }
        Err(other) => Err(LoadError::Other(other)),
    }
}

enum LoadError {
    File(ModelFileError),
    Other(GeometryError),
}

pub(crate) fn load(path: &str, physics: Option<bool>) -> Result<Loaded, u8> {
    let doing = format!("reading {path}");
    load_inner(path, physics).map_err(|e| match e {
        LoadError::File(f) => {
            refuse(&format!("{doing} -- {} problem(s)", f.problems.len()), &f.problems, &[], EXIT_REJECTED)
        }
        LoadError::Other(g) => {
            let (probs, hints) = explain(&g, &doing);
            refuse(&format!("{doing} failed"), &probs, &hints, EXIT_FAILED)
        }
    })
}

pub(crate) fn select_node(model: &Model, name: Option<&str>) -> Result<Option<NodeRef>, u8> {
    match name {
        Some(n) => model.node(n).map(Some).map_err(|e| {
            let (probs, hints) = explain(&e, "selecting a node");
            refuse("no such node", &probs, &hints, EXIT_REJECTED)
        }),
        None => Ok(model.root()),
    }
}

pub(crate) fn id_of(model: &Model, node: &NodeRef) -> String {
    model.id_of(node).unwrap_or_else(|| node.kind().to_owned())
}

pub(crate) fn unsolved_optimise(model: &Model, node: Option<&NodeRef>, path: &str, what: &str) -> Option<u8> {
    let node = node?;
    if node.kind() != "optimize" {
        return None;
    }
    let outs = model.doc.get("outputs").and_then(Value::as_object).cloned().unwrap_or_default();
    let kid = node.children().first().and_then(|c| model.id_of(c));
    let mut alias: Vec<String> =
        outs.iter().filter(|(_, nid)| nid.as_str() == kid.as_deref()).map(|(a, _)| a.clone()).collect();
    alias.sort();
    let nid = id_of(model, node);
    let mut hints = vec![format!(
        "implexity model optimise {path}            -- request an explicit solve with authored physics and constraints"
    )];
    if let Some(kid) = &kid {
        hints.push(format!(
            "implexity model {} {path} --node {}   -- the model at its AUTHORED parameters, which is what a preview usually means",
            what.split_whitespace().next().unwrap_or(""),
            alias.first().unwrap_or(kid)
        ));
    }
    Some(refuse(
        &format!("{nid} is an optimisation that has not been run"),
        &[format!(
            "{nid} is an `optimize` node: its value is its child AT THE OPTIMISED PARAMETERS, and nothing has solved it in this process.  {what} it would start a coupled physics solve inside the evaluator -- minutes and gigabytes, from a command that looks like a preview."
        )],
        &hints,
        EXIT_REJECTED,
    ))
}

pub(crate) fn box_for(model: &Model, node: &NodeRef) -> Option<([f64; 3], [f64; 3], Option<String>)> {
    use implexity_mesh::interop::subtree_box;
    if let Some((lo, hi)) = subtree_box(node) {
        return Some((lo, hi, None));
    }
    let root = model.root();
    if let Some(r) = &root
        && !std::sync::Arc::ptr_eq(r, node)
        && let Some((lo, hi)) = subtree_box(r)
    {
        return Some((
            lo,
            hi,
            Some(format!(
                "{} has no bounding box of its own, so the ROOT's box was used.  Pass --bbox to sample somewhere else.",
                id_of(model, node)
            )),
        ));
    }
    let walk: Vec<NodeRef> = match &root {
        Some(r) => r.walk().into_iter().map(|(_, n)| n).collect(),
        None => vec![node.clone()],
    };
    let boxes: Vec<([f64; 3], [f64; 3])> = walk.iter().filter_map(subtree_box).collect();
    if boxes.is_empty() {
        return None;
    }
    let mut lo = boxes[0].0;
    let mut hi = boxes[0].1;
    for (l, h) in &boxes[1..] {
        for a in 0..3 {
            lo[a] = lo[a].min(l[a]);
            hi[a] = hi[a].max(h[a]);
        }
    }
    Some((
        lo,
        hi,
        Some(format!(
            "no node in this graph reports a bounding box, so the box below is an ESTIMATE: the union of the {} subtree(s) that do report one.  That is correct when the unbounded parts (a TPMS, a plane) are cut away before the root, and wrong when one survives to the surface.  Check the result, or pass --bbox.",
            boxes.len()
        )),
    ))
}

pub(crate) fn parse_bbox(text: &str) -> Option<([f64; 3], [f64; 3])> {
    let v: Vec<f64> = text.split(',').map(|q| crate::args::py_float(q).ok()).collect::<Option<_>>()?;
    (v.len() == 6).then(|| ([v[0], v[1], v[2]], [v[3], v[4], v[5]]))
}

pub(crate) fn tree_root() -> Option<std::path::PathBuf> {
    crate::doctor::tree_root(None)
}

type Handler = fn(&[String]) -> u8;

const COMMANDS: &[(&str, Handler, &str)] = &[
    ("doctor", doctor::cmd_doctor, "can this machine model?  every check with evidence"),
    ("catalogue", inspect::cmd_catalogue, "the node kinds, their parameters and units"),
    ("examples", inspect::cmd_examples, "the shipped example models and what they teach"),
    ("validate", inspect::cmd_validate, "every problem in a document, with file and line"),
    ("show", inspect::cmd_show, "the graph as a tree: kind, field class, parameters"),
    ("eval", sample::cmd_eval, "the field of any node, at a point or on a grid"),
    ("params", inspect::cmd_params, "the named parameter table, and what each drives"),
    ("set", sample::cmd_set, "set named parameters; prove the structure did not move"),
    ("export", sample::cmd_export, "a body or a STEP solid from any node"),
    ("optimise", optimise::cmd_optimise, "run the Optimize node and report it"),
];

pub(crate) fn main(argv: &[String]) -> u8 {
    let Some(cmd) = argv.first() else {
        print!("{}", usage());
        return EXIT_OK;
    };
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        print!("{}", usage());
        return EXIT_OK;
    }
    let handler = if cmd == "optimize" {
        Some(optimise::cmd_optimise as Handler)
    } else {
        COMMANDS.iter().find(|(n, _, _)| n == cmd).map(|(_, h, _)| *h)
    };
    let Some(handler) = handler else {
        let names: Vec<String> = COMMANDS.iter().map(|(n, _, _)| (*n).to_owned()).collect();
        let hint = E::did_you_mean(cmd, &names, E::CUTOFF);
        eprintln!(
            "implexity model: no command {}.  The commands are: {}\n{}Try 'implexity model --help'.",
            implexity_core::py_repr::repr_str(cmd),
            names.join(", "),
            hint.map_or_else(String::new, |h| format!(
                "Did you mean {}?\n",
                implexity_core::py_repr::repr_str(&h)
            ))
        );
        return EXIT_USAGE;
    };
    handler(&argv[1..])
}

fn usage() -> String {
    let body = COMMANDS.iter().fold(String::new(), |mut acc, (n, _, d)| {
        let _ = writeln!(acc, "    {n:<10} {d}");
        acc
    });
    format!(
        "implexity model -- the implicit CAD kernel: a DAG of pure functions R^3 -> R\n\n\
         usage: implexity model <command> [options]\n\n{body}\n\
         Every command takes --help, and most take --json.\n\n\
         A model is a DOCUMENT: a table of nodes, a table of named parameters, and\n\
         bindings between them.  docs/implicit_authoring.md describes manual authoring and the schema source.\n\
         PUBLICATION_SCOPE.md records which physics paths are qualified or excluded.\n\n\
         Start here:   implexity model doctor\n              \
         implexity model examples\n              \
         implexity model show models/01_bracket.json\n"
    )
}
