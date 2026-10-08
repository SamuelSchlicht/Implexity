// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



pub mod arrays;
pub mod expr;
pub mod json;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{GResult, GeometryError};
use crate::eval::node_id;
use crate::fieldclass::FieldClass;
use crate::node::{Attr, ConstructArgs, Node, NodeRef, ParamRef, Registry, registry};
use crate::pyfmt::{self, str_repr};
use crate::value::{DType, NdArray, ParamValue, json_to_pyobj};

pub use expr::{Expr, eval_tree, evaluate_expr, expr_names, parse_expr};

pub const SCHEMA: &str = "implexity-model/1";

pub const DOC_UNITS: &str = "mm";

pub const TOP_FIELDS: [&str; 10] =
    ["schema", "name", "doc", "units", "parameters", "nodes", "root", "outputs", "arrays", "meta"];

pub const NODE_FIELDS: [&str; 6] = ["kind", "params", "children", "units", "attrs", "doc"];

pub const PARAM_FIELDS: [&str; 7] = ["value", "expr", "units", "doc", "min", "max", "free"];

pub const NAME_PATTERN: &str = "[A-Za-z_][A-Za-z0-9_.:-]{0,63}\\Z";

#[must_use]
pub fn array_inline_max_bytes() -> usize {
    std::env::var("IMPLEXITY_MODEL_INLINE_ARRAY_BYTES")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(4096)
}

#[must_use]
pub fn unit_info(u: &str) -> (String, f64) {
    let u = u.trim();
    let (d, f) = match u {
        "m" => ("length", 1.0),
        "mm" => ("length", 1e-3),
        "cm" => ("length", 1e-2),
        "um" | "micron" => ("length", 1e-6),
        "in" => ("length", 0.0254),
        "rad" => ("angle", 1.0),
        "deg" => ("angle", std::f64::consts::PI / 180.0),
        "-" | "" | "1" | "count" | "frac" => ("dimensionless", 1.0),
        other => return (format!("?{other}"), 1.0),
    };
    (d.to_string(), f)
}

#[must_use]
pub fn is_length(u: &str) -> bool {
    unit_info(u).0 == "length"
}


pub fn convert(value: f64, frm: &str, to: &str) -> GResult<f64> {
    let (df, ff) = unit_info(frm);
    let (dt, ft) = unit_info(to);
    if df != dt {
        return Err(GeometryError::Value(format!("cannot convert {frm} ({df}) to {to} ({dt})")));
    }
    #[allow(clippy::float_cmp)]
    if ff == ft {
        return Ok(value);
    }
    Ok(value * ff / ft)
}

fn name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

fn is_num(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.as_f64().is_some_and(f64::is_finite))
}

#[must_use]
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => json_to_pyobj(other).repr(),
    }
}

#[must_use]
pub fn py_repr(v: &Value) -> String {
    json_to_pyobj(v).repr()
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn sorted_keys(m: &Map<String, Value>) -> Vec<String> {
    let mut v: Vec<String> = m.keys().cloned().collect();
    v.sort();
    v
}

fn list_repr(items: &[String]) -> String {
    pyfmt::list_repr(items)
}

#[must_use]
pub fn canonical_bytes(doc: &Value) -> Vec<u8> {
    json::canonical(doc).into_bytes()
}

#[must_use]
pub fn dumps(doc: &Value) -> String {
    let mut s = json::indented(doc, 1);
    s.push('\n');
    s
}

#[must_use]
pub fn sha256_of(doc: &Value) -> String {
    hex::encode(Sha256::digest(canonical_bytes(doc)))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    Bind {
        name: String,
        src_unit: String,
    },
    Expr {
        tree: Expr,
        src_unit: String,
    },
    Array {
        key: String,
    },
}

impl Binding {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bind { .. } => "bind",
            Self::Expr { .. } => "expr",
            Self::Array { .. } => "array",
        }
    }

    #[must_use]
    pub fn spec_repr(&self) -> String {
        match self {
            Self::Bind { name, .. } => str_repr(name),
            Self::Expr { tree, .. } => tree.py_repr(),
            Self::Array { key } => str_repr(key),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Resolved {
    Value(ParamValue),
    Array(String),
}

#[derive(Default)]
struct Ctx {
    values: BTreeMap<String, f64>,
    param_units: BTreeMap<String, String>,
    raw: BTreeMap<String, Vec<u8>>,
    order: Vec<String>,
    parents: BTreeMap<String, Vec<String>>,
    bindings: BTreeMap<String, BTreeMap<String, Binding>>,
    resolved: BTreeMap<String, BTreeMap<String, Resolved>>,
    objs: BTreeMap<String, NodeRef>,
    ids: HashMap<usize, String>,
}

struct Checked {
    out: Map<String, Value>,
    problems: Vec<String>,
    warnings: Vec<String>,
    ctx: Ctx,
}

fn check(doc: &Value, base_dir: Option<&Path>, reg: &Registry, construct: bool) -> GResult<Checked> {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    let mut out = Map::new();
    let mut ctx = Ctx::default();
    let Some(d) = doc.as_object() else {
        return Ok(Checked {
            out,
            problems: vec!["the model document must be a JSON object".into()],
            warnings,
            ctx,
        });
    };
    let extra: Vec<String> = d.keys().filter(|k| !TOP_FIELDS.contains(&k.as_str())).cloned().collect();
    if !extra.is_empty() {
        let mut e = extra;
        e.sort();
        problems.push(format!(
            "unknown top-level field(s) {}; a model document has {}",
            list_repr(&e),
            TOP_FIELDS.join(", ")
        ));
    }
    let schema = d.get("schema").cloned().unwrap_or(Value::Null);
    let schema = if schema.as_str() == Some(SCHEMA) {
        SCHEMA.to_string()
    } else {
        problems.push(format!(
            "schema must be one of {}, got {}",
            list_repr(&[SCHEMA.to_string()]),
            py_repr(&schema)
        ));
        SCHEMA.to_string()
    };
    out.insert("schema".into(), Value::from(schema));
    out.insert("name".into(), Value::from(d.get("name").map_or_else(|| "unnamed model".to_string(), py_str)));
    if truthy(d.get("doc")) {
        out.insert("doc".into(), Value::from(py_str(&d["doc"])));
    }
    let units = d.get("units").cloned().unwrap_or_else(|| Value::from(DOC_UNITS));
    if units.as_str() != Some(DOC_UNITS) {
        problems.push(format!(
            "units must be {}: every length in a model document's own tables is millimetres, which is what docs/PROTOCOL.md declares for the wire.  Got {}",
            str_repr(DOC_UNITS),
            py_repr(&units)
        ));
    }
    out.insert("units".into(), Value::from(DOC_UNITS));
    if let Some(meta) = d.get("meta")
        && !meta.is_null()
    {
        if meta.is_object() {
            out.insert("meta".into(), json_check(meta, "meta", &mut problems, 0));
        } else {
            problems.push("meta must be an object of free-form author metadata".into());
        }
    }
    let arrays = check_arrays(d, base_dir, &mut problems, &mut ctx);
    out.insert("arrays".into(), Value::Object(arrays));
    let params = check_parameters(d, &mut problems, &mut ctx);
    out.insert("parameters".into(), Value::Object(params));
    let nodes = check_nodes(d, reg, &mut problems, &mut ctx);
    out.insert("nodes".into(), Value::Object(nodes));
    check_roots(d, &mut out, &mut problems, &mut warnings, &ctx);
    if construct {
        construct_all(&out, reg, &mut problems, &mut ctx)?;
    }
    Ok(Checked { out, problems, warnings, ctx })
}

fn json_check(obj: &Value, where_: &str, problems: &mut Vec<String>, depth: usize) -> Value {
    if depth > 16 {
        problems.push(format!("{where_} nests deeper than 16"));
        return Value::Null;
    }
    match obj {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), json_check(v, &format!("{where_}.{k}"), problems, depth + 1)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .enumerate()
                .map(|(i, v)| json_check(v, &format!("{where_}[{i}]"), problems, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn check_arrays(
    doc: &Map<String, Value>,
    base_dir: Option<&Path>,
    problems: &mut Vec<String>,
    ctx: &mut Ctx,
) -> Map<String, Value> {
    let mut out = Map::new();
    let raw_in = match doc.get("arrays") {
        None | Some(Value::Null) => return out,
        Some(Value::Object(m)) if m.is_empty() => return out,
        Some(Value::Object(m)) => m,
        Some(v) if !truthy(Some(v)) => return out,
        Some(_) => {
            problems.push("arrays must be an object of name -> array entry".into());
            return out;
        }
    };
    for key in sorted_keys(raw_in) {
        let where_ = format!("arrays.{key}");
        if !name_ok(&key) {
            problems.push(format!("{where_}: an array key must match {NAME_PATTERN}"));
            continue;
        }
        let Some(e) = raw_in[&key].as_object() else {
            problems.push(format!("{where_} must be an object {{dtype, shape, sha256, b64|file}}"));
            continue;
        };
        let allowed = ["dtype", "shape", "sha256", "b64", "file", "doc", "units"];
        let mut bad: Vec<String> = e.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
        if !bad.is_empty() {
            bad.sort();
            problems.push(format!(
                "{where_}: unknown field(s) {}; an array entry has {}",
                list_repr(&bad),
                allowed.join(", ")
            ));
        }
        let dt = e.get("dtype").cloned().unwrap_or(Value::Null);
        let Some(dts) = dt.as_str().filter(|s| arrays::itemsize(s).is_some()) else {
            problems.push(format!(
                "{where_}.dtype must be one of {}, got {}",
                arrays::dtype_list(),
                py_repr(&dt)
            ));
            continue;
        };
        let shape = e.get("shape").cloned().unwrap_or(Value::Null);
        let shape_ok = match &shape {
            Value::Array(a) => !a.is_empty() && a.iter().all(Value::is_u64),
            _ => false,
        };
        if !shape_ok {
            problems.push(format!(
                "{where_}.shape must be a non-empty list of non-negative integers, got {}",
                py_repr(&shape)
            ));
            continue;
        }
        if e.get("sha256").and_then(Value::as_str).is_none_or(|s| s.chars().count() != 64) {
            problems
                .push(format!("{where_}.sha256 must be the 64-hex sha256 of the raw little-endian buffer"));
            continue;
        }
        let (has_b64, has_file) = (e.contains_key("b64"), e.contains_key("file"));
        if has_b64 == has_file {
            problems.push(format!(
                "{where_} must carry exactly one of b64 (inline) and file (a .npy sidecar), not {}",
                if has_b64 { "both" } else { "neither" }
            ));
            continue;
        }
        if has_file {
            let f = py_str(&e["file"]);
            let ok = !f.is_empty()
                && f.len() <= 96
                && f.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
            if !ok {
                problems.push(format!(
                    "{where_}.file must be a plain file name beside the document, got {}",
                    py_repr(&e["file"])
                ));
                continue;
            }
        }
        let mut norm = Map::new();
        norm.insert("dtype".into(), Value::from(dts));
        norm.insert("shape".into(), shape.clone());
        norm.insert("sha256".into(), e["sha256"].clone());
        if truthy(e.get("doc")) {
            norm.insert("doc".into(), Value::from(py_str(&e["doc"])));
        }
        if truthy(e.get("units")) {
            norm.insert("units".into(), Value::from(py_str(&e["units"])));
        }
        let Some(buf) = array_bytes(e, dts, &shape, &key, base_dir, problems) else { continue };
        if has_b64 {
            norm.insert(
                "b64".into(),
                Value::from(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &buf)),
            );
        } else {
            norm.insert("file".into(), e["file"].clone());
        }
        out.insert(key.clone(), Value::Object(norm));
        ctx.raw.insert(key, buf);
    }
    out
}

fn array_bytes(
    e: &Map<String, Value>,
    dtype: &str,
    shape: &Value,
    key: &str,
    base_dir: Option<&Path>,
    problems: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let where_ = format!("arrays.{key}");
    let raw = if let Some(b) = e.get("b64") {
        let s = b.as_str().map_or_else(|| py_str(b), str::to_string);
        match arrays::b64decode_strict(&s) {
            Ok(r) => r,
            Err(msg) => {
                problems.push(format!("{where_}.b64 is not valid base64: {msg}"));
                return None;
            }
        }
    } else {
        let rel = py_str(&e["file"]);
        let Some(dir) = base_dir else {
            problems.push(format!(
                "{where_} names the sidecar file {}, but this document was loaded without a directory to resolve it against (pass base_dir, or read the document from a path)",
                str_repr(&rel)
            ));
            return None;
        };
        let path = dir.join(&rel);
        match std::fs::read(&path) {
            Ok(blob) => arrays::npy_payload(&blob, &where_, problems)?,
            Err(err) => {
                problems.push(format!(
                    "{where_}: cannot read the sidecar {}: {}",
                    path.display(),
                    io_msg(&err, &path)
                ));
                return None;
            }
        }
    };
    let n: usize = shape
        .as_array()
        .map_or(0, |a| a.iter().filter_map(Value::as_u64).map(|u| usize::try_from(u).unwrap_or(0)).product());
    let want = n * arrays::itemsize(dtype).unwrap_or(1);
    if raw.len() != want {
        problems.push(format!(
            "{where_}: {} bytes for a {} array of shape {}, which needs {}",
            raw.len(),
            dtype,
            json_to_pyobj(shape).repr(),
            want
        ));
        return None;
    }
    let got = arrays::sha256_hex(&raw);
    let claimed = py_str(&e["sha256"]);
    if got != claimed {
        problems.push(format!(
            "{where_}: the bytes hash to {} but the document says {} -- the array has been replaced since the document was written",
            &got[..16],
            claimed.chars().take(16).collect::<String>()
        ));
        return None;
    }
    Some(raw)
}

fn io_msg(err: &std::io::Error, path: &Path) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => {
            format!("[Errno 2] No such file or directory: {}", str_repr(&path.display().to_string()))
        }
        std::io::ErrorKind::PermissionDenied => {
            format!("[Errno 13] Permission denied: {}", str_repr(&path.display().to_string()))
        }
        _ => err.to_string(),
    }
}

fn check_parameters(
    doc: &Map<String, Value>,
    problems: &mut Vec<String>,
    ctx: &mut Ctx,
) -> Map<String, Value> {
    let mut out = Map::new();
    let raw_in = match doc.get("parameters") {
        None | Some(Value::Null) => return out,
        Some(Value::Object(m)) => m.clone(),
        Some(v) if !truthy(Some(v)) => return out,
        Some(_) => {
            problems.push("parameters must be an object of name -> {value|expr, units, ...}".into());
            return out;
        }
    };
    let mut trees: BTreeMap<String, Expr> = BTreeMap::new();
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in sorted_keys(&raw_in) {
        let where_ = format!("parameters.{name}");
        if !name_ok(&name) {
            problems.push(format!("{where_}: a parameter name must match {NAME_PATTERN}"));
            continue;
        }
        let mut p = raw_in[&name].clone();
        if is_num(&p) {
            let mut m = Map::new();
            m.insert("value".into(), p);
            p = Value::Object(m);
        }
        let Some(p) = p.as_object().cloned() else {
            problems.push(format!(
                "{where_} must be a number or an object {{value|expr, units, doc, min, max, free}}"
            ));
            continue;
        };
        let mut bad: Vec<String> =
            p.keys().filter(|k| !PARAM_FIELDS.contains(&k.as_str())).cloned().collect();
        if !bad.is_empty() {
            bad.sort();
            problems.push(format!(
                "{where_}: unknown field(s) {}; a parameter has {}",
                list_repr(&bad),
                PARAM_FIELDS.join(", ")
            ));
        }
        let u = p.get("units").map_or_else(|| DOC_UNITS.to_string(), py_str);
        let (dim, _) = unit_info(&u);
        let mut entry = Map::new();
        entry.insert("units".into(), Value::from(u.clone()));
        if dim == "length" && u != DOC_UNITS {
            entry.insert("units".into(), Value::from(DOC_UNITS));
        }
        let (has_v, has_e) = (p.contains_key("value"), p.contains_key("expr"));
        if has_v == has_e {
            problems.push(format!(
                "{where_} must carry exactly one of value and expr, not {}",
                if has_v { "both" } else { "neither" }
            ));
            continue;
        }
        if has_v {
            if !is_num(&p["value"]) {
                problems
                    .push(format!("{where_}.value must be a finite number, got {}", py_repr(&p["value"])));
                continue;
            }
            let mut v = p["value"].as_f64().unwrap_or(0.0);
            if dim == "length" && u != DOC_UNITS {
                v = convert(v, &u, DOC_UNITS).unwrap_or(v);
            }
            entry.insert("value".into(), crate::value::json_f64(v));
        } else {
            let text = p["expr"].as_str().map(str::to_string);
            let parsed = match &text {
                Some(t) => parse_expr(t),
                None => Err(GeometryError::Expr(format!(
                    "an expression must be a string, got {}",
                    json_type_name(&p["expr"])
                ))),
            };
            match parsed {
                Ok(tree) => {
                    deps.insert(name.clone(), expr_names(&tree));
                    trees.insert(name.clone(), tree);
                    entry.insert("expr".into(), Value::from(py_str(&p["expr"])));
                }
                Err(e) => {
                    problems.push(format!("{where_}.expr: {e}"));
                    continue;
                }
            }
        }
        for bound in ["min", "max"] {
            if let Some(b) = p.get(bound) {
                if is_num(b) {
                    let mut bv = b.as_f64().unwrap_or(0.0);
                    if dim == "length" && u != DOC_UNITS {
                        bv = convert(bv, &u, DOC_UNITS).unwrap_or(bv);
                    }
                    entry.insert(bound.into(), crate::value::json_f64(bv));
                } else {
                    problems.push(format!("{where_}.{bound} must be a finite number, got {}", py_repr(b)));
                }
            }
        }
        if let (Some(mn), Some(mx)) =
            (entry.get("min").and_then(Value::as_f64), entry.get("max").and_then(Value::as_f64))
            && mn > mx
        {
            problems.push(format!("{where_}: min {} is above max {}", pyfmt::g(mn), pyfmt::g(mx)));
        }
        if truthy(p.get("doc")) {
            entry.insert("doc".into(), Value::from(py_str(&p["doc"])));
        }
        if truthy(p.get("free")) {
            entry.insert("free".into(), Value::from(true));
        }
        out.insert(name, Value::Object(entry));
    }
    let mut values: BTreeMap<String, f64> = out
        .iter()
        .filter_map(|(n, e)| e.get("value").and_then(Value::as_f64).map(|v| (n.clone(), v)))
        .collect();
    for (name, dset) in &deps {
        let missing: Vec<String> = dset.iter().filter(|d| !out.contains_key(*d)).cloned().collect();
        if !missing.is_empty() {
            problems.push(format!(
                "parameters.{}.expr reads {}, which {} not a parameter of this document (it has {})",
                name,
                missing.iter().map(|m| str_repr(m)).collect::<Vec<_>>().join(", "),
                if missing.len() == 1 { "is" } else { "are" },
                if out.is_empty() { "none".to_string() } else { sorted_keys(&out).join(", ") }
            ));
        }
    }
    let graph: BTreeMap<String, Vec<String>> = deps
        .iter()
        .map(|(n, ds)| (n.clone(), ds.iter().filter(|d| out.contains_key(*d)).cloned().collect()))
        .collect();
    let (order, cycles) = toposort(&graph);
    for cyc in cycles {
        let mut chain = cyc.clone();
        chain.push(cyc[0].clone());
        problems.push(format!(
            "parameters: {} depend on each other in a cycle ({}); a parameter cannot be defined in terms of itself",
            cyc.join(", "),
            chain.join(" -> ")
        ));
    }
    for name in order {
        let Some(tree) = trees.get(&name) else { continue };
        match eval_tree(tree, &values) {
            Ok(v) => {
                values.insert(name, v);
            }
            Err(e) => problems.push(format!("parameters.{name}.expr: {e}")),
        }
    }
    for name in sorted_keys(&out) {
        let Some(v) = values.get(&name).copied() else { continue };
        let e = &out[&name];
        let units = e.get("units").map(py_str).unwrap_or_default();
        if let Some(mn) = e.get("min").and_then(Value::as_f64)
            && v < mn - 1e-12
        {
            problems.push(format!(
                "parameters.{} is {} {}, below its declared minimum {}",
                name,
                pyfmt::g(v),
                units,
                pyfmt::g(mn)
            ));
        }
        if let Some(mx) = e.get("max").and_then(Value::as_f64)
            && v > mx + 1e-12
        {
            problems.push(format!(
                "parameters.{} is {} {}, above its declared maximum {}",
                name,
                pyfmt::g(v),
                units,
                pyfmt::g(mx)
            ));
        }
    }
    ctx.values = values;
    ctx.param_units =
        out.iter().map(|(n, e)| (n.clone(), e.get("units").map(py_str).unwrap_or_default())).collect();
    out
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[must_use]
pub fn toposort(graph: &BTreeMap<String, Vec<String>>) -> (Vec<String>, Vec<Vec<String>>) {
    let mut colour: BTreeMap<String, u8> = BTreeMap::new();
    let mut order = Vec::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();
    fn walk(
        n: &str,
        stack: &mut Vec<String>,
        graph: &BTreeMap<String, Vec<String>>,
        colour: &mut BTreeMap<String, u8>,
        order: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        match colour.get(n) {
            Some(1) => {
                let c = stack
                    .iter()
                    .position(|s| s == n)
                    .map_or_else(|| vec![n.to_string()], |i| stack[i..].to_vec());
                cycles.push(c);
                return;
            }
            Some(2) => return,
            _ => {}
        }
        colour.insert(n.to_string(), 1);
        let mut deps: Vec<String> = graph.get(n).cloned().unwrap_or_default();
        deps.sort();
        stack.push(n.to_string());
        for d in deps {
            walk(&d, stack, graph, colour, order, cycles);
        }
        stack.pop();
        colour.insert(n.to_string(), 2);
        order.push(n.to_string());
    }
    for n in graph.keys() {
        walk(n, &mut Vec::new(), graph, &mut colour, &mut order, &mut cycles);
    }
    let mut seen = BTreeSet::new();
    let mut uniq = Vec::new();
    for c in cycles {
        let mut k = c.clone();
        k.sort();
        if seen.insert(k) {
            uniq.push(c);
        }
    }
    (order, uniq)
}

fn check_nodes(
    doc: &Map<String, Value>,
    reg: &Registry,
    problems: &mut Vec<String>,
    ctx: &mut Ctx,
) -> Map<String, Value> {
    let mut out = Map::new();
    let raw_in = match doc.get("nodes") {
        None => {
            problems.push(
                "nodes must be an object of id -> node entry; a model document is a node TABLE, not a nested tree, because a shared subtree has to be stored once"
                    .into(),
            );
            return out;
        }
        Some(Value::Object(m)) => m,
        Some(_) => {
            problems.push("nodes must be an object of id -> node entry".into());
            return out;
        }
    };
    let mut child_ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for nid in sorted_keys(raw_in) {
        let where_ = format!("nodes.{nid}");
        if !name_ok(&nid) {
            problems.push(format!("{where_}: a node id must match {NAME_PATTERN}"));
            continue;
        }
        let Some(e) = raw_in[&nid].as_object() else {
            problems.push(format!("{where_} must be an object {{kind, params, children, units, attrs}}"));
            continue;
        };
        let mut bad: Vec<String> = e.keys().filter(|k| !NODE_FIELDS.contains(&k.as_str())).cloned().collect();
        if !bad.is_empty() {
            bad.sort();
            problems.push(format!(
                "{where_}: unknown field(s) {}; a node entry has {}",
                list_repr(&bad),
                NODE_FIELDS.join(", ")
            ));
        }
        let kind = e.get("kind").cloned().unwrap_or(Value::Null);
        let mut info = None;
        match kind.as_str() {
            Some(k) if !k.is_empty() => match reg.get(k) {
                Some(entry) => info = Some(Arc::clone(&entry.info)),
                None => problems.push(format!(
                    "{}.kind {} is not registered; the registered kinds are {}.  A kind is registered by importing the module that declares it (primitives, ops, lattice_ops, interop, optimize)",
                    where_,
                    str_repr(k),
                    if reg.names().is_empty() { "none".to_string() } else { reg.names().join(", ") }
                )),
            },
            _ => problems.push(format!("{where_}.kind must name a registered node kind")),
        }
        let mut norm = Map::new();
        norm.insert("kind".into(), Value::from(kind.as_str().unwrap_or("?")));
        if truthy(e.get("doc")) {
            norm.insert("doc".into(), Value::from(py_str(&e["doc"])));
        }
        let mut kids = Vec::new();
        let mut names = Vec::new();
        let ch = match e.get("children") {
            None => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => {
                problems.push(format!(
                    "{where_}.children must be a LIST of {{name, node}} -- a list because child ORDER is structural (difference(a, b) is not difference(b, a)) and a JSON object's key order is not preserved by a canonical dump"
                ));
                Vec::new()
            }
        };
        for (i, c) in ch.iter().enumerate() {
            let ok = c.as_object().is_some_and(|o| {
                o.keys().all(|k| k == "name" || k == "node")
                    && o.get("name").is_some_and(Value::is_string)
                    && o.get("node").is_some_and(Value::is_string)
            });
            if !ok {
                problems.push(format!(
                    "{where_}.children[{i}] must be {{\"name\": \"<child name>\", \"node\": \"<node id>\"}}"
                ));
                continue;
            }
            let cid = c["node"].as_str().unwrap_or_default().to_string();
            if !raw_in.contains_key(&cid) {
                problems.push(format!(
                    "{}.children[{}] refers to node {}, which is not in the table",
                    where_,
                    i,
                    str_repr(&cid)
                ));
                continue;
            }
            names.push(c["name"].as_str().unwrap_or_default().to_string());
            kids.push(cid);
        }
        let dups: BTreeSet<String> =
            names.iter().filter(|n| names.iter().filter(|m| m == n).count() > 1).cloned().collect();
        if !dups.is_empty() {
            problems.push(format!(
                "{}: duplicate child name(s) {}",
                where_,
                list_repr(&dups.into_iter().collect::<Vec<_>>())
            ));
        }
        norm.insert(
            "children".into(),
            Value::Array(
                names.iter().zip(&kids).map(|(n, k)| serde_json::json!({"name": n, "node": k})).collect(),
            ),
        );
        for k in &kids {
            ctx.parents.entry(k.clone()).or_default().push(nid.clone());
        }
        child_ids.insert(nid.clone(), kids);

        let params_in = match e.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(v) if !truthy(Some(v)) => Map::new(),
            Some(_) => {
                problems.push(format!("{where_}.params must be an object"));
                Map::new()
            }
        };
        let declared: Vec<crate::node::ParamSpec> =
            info.as_ref().map(|i| i.params.clone()).unwrap_or_default();
        let declared_names: BTreeSet<String> = declared.iter().map(|p| p.name.clone()).collect();
        if let Some(i) = &info {
            let unknown: Vec<String> =
                sorted_keys(&params_in).into_iter().filter(|k| !declared_names.contains(k)).collect();
            if !unknown.is_empty() {
                problems.push(format!(
                    "{}: {} has no parameter {}; it has {}",
                    where_,
                    i.kind,
                    unknown.join(", "),
                    if declared_names.is_empty() {
                        "none".to_string()
                    } else {
                        declared_names.iter().cloned().collect::<Vec<_>>().join(", ")
                    }
                ));
            }
        }
        let units_in = match e.get("units") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(v) if !truthy(Some(v)) => Map::new(),
            Some(_) => {
                problems.push(format!("{where_}.units must be an object of parameter -> unit"));
                Map::new()
            }
        };
        if let Some(i) = &info {
            for p in sorted_keys(&units_in) {
                let u = &units_in[&p];
                match i.param(&p) {
                    None => problems.push(format!(
                        "{}.units names {}, which is not a parameter of {}",
                        where_,
                        str_repr(&p),
                        i.kind
                    )),
                    Some(spec) => {
                        if py_str(u) != spec.units {
                            problems.push(format!(
                                "{}.units says {} is in {} but {} declares it in {} -- the node kind's units changed under this stored document, so its numbers now mean something else",
                                where_,
                                str_repr(&p),
                                py_repr(u),
                                i.kind,
                                str_repr(&spec.units)
                            ));
                        }
                    }
                }
            }
        }
        let mut pnorm = Map::new();
        let mut punits = Map::new();
        let mut binds = BTreeMap::new();
        let mut sorted_specs = declared.clone();
        sorted_specs.sort_by(|a, b| a.name.cmp(&b.name));
        for spec in &sorted_specs {
            punits.insert(spec.name.clone(), Value::from(spec.units.clone()));
            let (val, stored, bind) = check_param(
                params_in.get(&spec.name),
                spec,
                ctx,
                problems,
                &format!("{}.params.{}", where_, spec.name),
            );
            if let Some(s) = stored {
                pnorm.insert(spec.name.clone(), s);
            }
            if let Some(b) = bind {
                binds.insert(spec.name.clone(), b);
            }
            if let Some(v) = val {
                ctx.resolved.entry(nid.clone()).or_default().insert(spec.name.clone(), v);
            }
        }
        norm.insert("params".into(), Value::Object(pnorm));
        norm.insert("units".into(), Value::Object(punits));
        if !binds.is_empty() {
            ctx.bindings.insert(nid.clone(), binds);
        }
        if let Some(a) = e.get("attrs")
            && !a.is_null()
        {
            match a.as_object() {
                Some(m) => {
                    let attrs: Map<String, Value> = m
                        .iter()
                        .map(|(k, v)| (k.clone(), json_check(v, &format!("{where_}.attrs.{k}"), problems, 0)))
                        .collect();
                    norm.insert("attrs".into(), Value::Object(attrs));
                }
                None => {
                    problems
                        .push(format!("{where_}.attrs must be an object of constructor keyword -> value"));
                }
            }
        }
        out.insert(nid, Value::Object(norm));
    }
    let (order, cycles) = toposort(&child_ids);
    for cyc in cycles {
        let mut chain = cyc.clone();
        chain.push(cyc[0].clone());
        problems.push(format!(
            "nodes: {} reach each other in a cycle ({}).  The model is a DIRECTED ACYCLIC graph: a subtree may be SHARED by two parents, which is what the node table is for, but it may not contain itself",
            cyc.join(", "),
            chain.join(" -> ")
        ));
    }
    ctx.order = order.into_iter().filter(|n| out.contains_key(n)).collect();
    out
}

fn check_param(
    spec: Option<&Value>,
    declared: &crate::node::ParamSpec,
    ctx: &Ctx,
    problems: &mut Vec<String>,
    where_: &str,
) -> (Option<Resolved>, Option<Value>, Option<Binding>) {
    let dst_unit = declared.units.as_str();
    let Some(spec) = spec else {
        return match &declared.default {
            None => {
                problems.push(format!("{where_} has no value and the kind declares no default"));
                (None, None, None)
            }
            Some(d) => (Some(Resolved::Value(d.clone())), Some(d.to_json()), None),
        };
    };
    if let Value::Object(m) = spec {
        let forms = ["value", "bind", "expr", "array"];
        let tags: Vec<&str> = forms.iter().copied().filter(|t| m.contains_key(*t)).collect();
        if tags.len() != 1 {
            problems.push(format!(
                "{}: a bound parameter is exactly one of {}, got {}",
                where_,
                forms.iter().map(|t| format!("{{\"{t}\": ...}}")).collect::<Vec<_>>().join(", "),
                list_repr(&sorted_keys(m))
            ));
            return (None, None, None);
        }
        let tag = tags[0];
        let allowed: &[&str] = match tag {
            "value" | "expr" => &[tag, "units"],
            _ => &[tag],
        };
        let mut bad: Vec<String> = m.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
        if !bad.is_empty() {
            bad.sort();
            problems.push(format!(
                "{}: a {} binding has no field(s) {}",
                where_,
                str_repr(tag),
                list_repr(&bad)
            ));
        }
        match tag {
            "value" => {
                let u = m.get("units").map_or_else(|| dst_unit.to_string(), py_str);
                if unit_info(&u).0 != unit_info(dst_unit).0 {
                    problems.push(format!(
                        "{} is declared in {} but the literal is given in {}, which is a different dimension",
                        where_,
                        str_repr(dst_unit),
                        str_repr(&u)
                    ));
                    return (None, None, None);
                }
                if !is_num(&m["value"]) {
                    problems.push(format!(
                        "{}.value must be a finite number, got {}",
                        where_,
                        py_repr(&m["value"])
                    ));
                    return (None, None, None);
                }
                let v = convert(m["value"].as_f64().unwrap_or(0.0), &u, dst_unit).unwrap_or(f64::NAN);
                return (Some(Resolved::Value(ParamValue::Float(v))), Some(crate::value::json_f64(v)), None);
            }
            "array" => {
                let key = py_str(&m["array"]);
                if !ctx.raw.contains_key(&key) {
                    let have: Vec<String> = ctx.raw.keys().cloned().collect();
                    problems.push(format!(
                        "{} refers to array {}, which is not in the document's array table (it has {})",
                        where_,
                        py_repr(&m["array"]),
                        if have.is_empty() { "none".to_string() } else { have.join(", ") }
                    ));
                    return (None, None, None);
                }
                return (
                    Some(Resolved::Array(key.clone())),
                    Some(serde_json::json!({"array": m["array"].clone()})),
                    Some(Binding::Array { key }),
                );
            }
            "bind" => {
                let name = py_str(&m["bind"]);
                let Some(v) = ctx.values.get(&name).copied() else {
                    let have: Vec<String> = ctx.values.keys().cloned().collect();
                    problems.push(format!(
                        "{} binds to parameter {}, which this document does not declare (it declares {})",
                        where_,
                        py_repr(&m["bind"]),
                        if have.is_empty() { "none".to_string() } else { have.join(", ") }
                    ));
                    return (None, None, None);
                };
                let src_unit = ctx.param_units.get(&name).cloned().unwrap_or_else(|| DOC_UNITS.to_string());
                if unit_info(&src_unit).0 != unit_info(dst_unit).0 {
                    problems.push(format!(
                        "{} is declared in {} ({}) but is bound to parameter {}, declared in {} ({}) -- a binding converts between units of the same dimension and refuses to guess across them",
                        where_,
                        str_repr(dst_unit),
                        unit_info(dst_unit).0,
                        str_repr(&name),
                        str_repr(&src_unit),
                        unit_info(&src_unit).0
                    ));
                    return (None, None, None);
                }
                let v = convert(v, &src_unit, dst_unit).unwrap_or(f64::NAN);
                return (
                    Some(Resolved::Value(ParamValue::Float(v))),
                    Some(serde_json::json!({"bind": m["bind"].clone()})),
                    Some(Binding::Bind { name, src_unit }),
                );
            }
            _ => {}
        }

        let default_unit = if is_length(dst_unit) { DOC_UNITS.to_string() } else { dst_unit.to_string() };
        let src_unit = m.get("units").map_or(default_unit, py_str);
        if unit_info(&src_unit).0 != unit_info(dst_unit).0 {
            problems.push(format!(
                "{} is declared in {} ({}) but its expression is declared to produce {} ({})",
                where_,
                str_repr(dst_unit),
                unit_info(dst_unit).0,
                str_repr(&src_unit),
                unit_info(&src_unit).0
            ));
            return (None, None, None);
        }
        let parsed = match m["expr"].as_str() {
            Some(t) => parse_expr(t),
            None => Err(GeometryError::Expr(format!(
                "an expression must be a string, got {}",
                json_type_name(&m["expr"])
            ))),
        };
        let tree = match parsed {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{where_}.expr: {e}"));
                return (None, None, None);
            }
        };
        let mut stored = Map::new();
        stored.insert("expr".into(), Value::from(py_str(&m["expr"])));
        if m.contains_key("units") {
            stored.insert("units".into(), Value::from(src_unit.clone()));
        }
        let bind = Binding::Expr { tree: tree.clone(), src_unit: src_unit.clone() };
        return match eval_tree(&tree, &ctx.values).and_then(|v| convert(v, &src_unit, dst_unit)) {
            Ok(v) => (Some(Resolved::Value(ParamValue::Float(v))), Some(Value::Object(stored)), Some(bind)),
            Err(e) => {
                problems.push(format!("{where_}.expr: {e}"));
                (None, Some(Value::Object(stored)), Some(bind))
            }
        };
    }
    if let Value::Array(items) = spec {
        if flat_numbers(items, where_, problems, 0) {
            let v = ParamValue::from_json(spec).unwrap_or(ParamValue::List(Vec::new()));
            return (Some(Resolved::Value(v)), Some(spec.clone()), None);
        }
        return (None, None, None);
    }
    match spec {
        Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            let v = ParamValue::from_json(spec).unwrap_or(ParamValue::Float(0.0));
            (Some(Resolved::Value(v)), Some(spec.clone()), None)
        }
        other => {
            problems.push(format!(
                "{}: {} is not a value a node parameter can hold (a number, a list of numbers, a string, or a binding)",
                where_,
                py_repr(other)
            ));
            (None, None, None)
        }
    }
}

fn flat_numbers(seq: &[Value], where_: &str, problems: &mut Vec<String>, depth: usize) -> bool {
    if depth > 8 {
        problems.push(format!("{where_} nests deeper than 8"));
        return false;
    }
    for v in seq {
        match v {
            Value::Array(inner) => {
                if !flat_numbers(inner, where_, problems, depth + 1) {
                    return false;
                }
            }
            Value::Bool(_) => {}
            v if is_num(v) => {}
            other => {
                problems.push(format!(
                    "{} contains {}; a list parameter holds numbers",
                    where_,
                    py_repr(other)
                ));
                return false;
            }
        }
    }
    true
}

fn check_roots(
    doc: &Map<String, Value>,
    out: &mut Map<String, Value>,
    problems: &mut Vec<String>,
    warnings: &mut Vec<String>,
    ctx: &Ctx,
) {
    let nodes = out.get("nodes").and_then(Value::as_object).cloned().unwrap_or_default();
    match doc.get("root") {
        None | Some(Value::Null) => {
            if !nodes.is_empty() {
                problems.push(format!(
                    "root must name the node id the model evaluates to; the table has {}",
                    sorted_keys(&nodes).join(", ")
                ));
            }
        }
        Some(Value::String(r)) if nodes.contains_key(r) => {
            out.insert("root".into(), Value::from(r.clone()));
        }
        Some(r) => problems.push(format!(
            "root {} is not a node in the table (it has {})",
            py_repr(r),
            if nodes.is_empty() { "none".to_string() } else { sorted_keys(&nodes).join(", ") }
        )),
    }
    let outs = match doc.get("outputs") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(v) if !truthy(Some(v)) => Map::new(),
        Some(_) => {
            problems.push(
                "outputs must be an object of name -> node id: the named subtrees a UI can preview".into(),
            );
            Map::new()
        }
    };
    let mut norm = Map::new();
    for name in sorted_keys(&outs) {
        if !name_ok(&name) {
            problems.push(format!("outputs.{name}: a name must match {NAME_PATTERN}"));
            continue;
        }
        let target = &outs[&name];
        let Some(t) = target.as_str().filter(|t| nodes.contains_key(*t)) else {
            problems.push(format!(
                "outputs.{} refers to node {}, which is not in the table",
                name,
                py_repr(target)
            ));
            continue;
        };
        if nodes.contains_key(&name) {
            problems.push(format!(
                "outputs.{name} collides with a node id; an output name and a node id share one namespace so 'evaluate this name' has one answer"
            ));
            continue;
        }
        norm.insert(name, Value::from(t));
    }
    if !norm.is_empty() {
        out.insert("outputs".into(), Value::Object(norm.clone()));
    }
    if let Some(root) = out.get("root").and_then(Value::as_str).map(str::to_string) {
        let mut seen = BTreeSet::new();
        let reach = |start: &str, seen: &mut BTreeSet<String>| {
            let mut stack = vec![start.to_string()];
            while let Some(nid) = stack.pop() {
                if !seen.insert(nid.clone()) {
                    continue;
                }
                if let Some(ch) = nodes.get(&nid).and_then(|n| n.get("children")).and_then(Value::as_array) {
                    stack.extend(
                        ch.iter().filter_map(|c| c.get("node").and_then(Value::as_str).map(str::to_string)),
                    );
                }
            }
        };
        reach(&root, &mut seen);
        let mut targets: Vec<String> = norm.values().filter_map(Value::as_str).map(str::to_string).collect();
        targets.sort();
        for t in targets {
            reach(&t, &mut seen);
        }
        let orphans: Vec<String> = sorted_keys(&nodes).into_iter().filter(|n| !seen.contains(n)).collect();
        if !orphans.is_empty() {
            warnings.push(format!(
                "node(s) {} are in the table but no root or output reaches them; they are kept (a detached subtree is a normal state mid-edit) but they are not part of the model",
                orphans.join(", ")
            ));
        }
    }
    let params = out.get("parameters").and_then(Value::as_object).cloned().unwrap_or_default();
    let used = bound_names(&ctx.bindings, &params);
    let unused: Vec<String> = sorted_keys(&params).into_iter().filter(|n| !used.contains(n)).collect();
    if !unused.is_empty() {
        warnings.push(format!("parameter(s) {} are declared and nothing binds to them", unused.join(", ")));
    }
    let arrays = out.get("arrays").and_then(Value::as_object).cloned().unwrap_or_default();
    let refs = array_refs(&nodes);
    let unused_a: Vec<String> = sorted_keys(&arrays).into_iter().filter(|k| !refs.contains(k)).collect();
    if !unused_a.is_empty() {
        warnings
            .push(format!("array(s) {} are in the table and nothing references them", unused_a.join(", ")));
    }
}

fn bound_names(
    bindings: &BTreeMap<String, BTreeMap<String, Binding>>,
    parameters: &Map<String, Value>,
) -> BTreeSet<String> {
    let mut used = BTreeSet::new();
    for binds in bindings.values() {
        for b in binds.values() {
            match b {
                Binding::Bind { name, .. } => {
                    used.insert(name.clone());
                }
                Binding::Expr { tree, .. } => used.extend(expr_names(tree)),
                Binding::Array { .. } => {}
            }
        }
    }
    for (name, decl) in parameters {
        let Some(d) = decl.as_object() else { continue };
        if let Some(e) = d.get("expr") {
            if let Some(tree) = e.as_str().and_then(|t| parse_expr(t).ok()) {
                used.extend(expr_names(&tree).into_iter().filter(|n| n != name));
            }
        } else if let Some(b) = d.get("bind") {
            let b = py_str(b);
            if &b != name {
                used.insert(b);
            }
        }
    }
    used
}

fn array_refs(nodes: &Map<String, Value>) -> BTreeSet<String> {
    let mut used = BTreeSet::new();
    for e in nodes.values() {
        if let Some(p) = e.get("params").and_then(Value::as_object) {
            for v in p.values() {
                if let Some(k) = v.as_object().and_then(|o| o.get("array")) {
                    used.insert(py_str(k));
                }
            }
        }
        fn walk(v: &Value, used: &mut BTreeSet<String>) {
            match v {
                Value::Object(m) => {
                    if let Some(k) = m.get("$array") {
                        used.insert(py_str(k));
                    }
                    m.values().for_each(|x| walk(x, used));
                }
                Value::Array(a) => a.iter().for_each(|x| walk(x, used)),
                _ => {}
            }
        }
        if let Some(a) = e.get("attrs") {
            walk(a, &mut used);
        }
    }
    used
}

fn decode_attr(
    v: &Value,
    arrays: &BTreeMap<String, Arc<NdArray>>,
    problems: &mut Vec<String>,
    where_: &str,
) -> Attr {
    match v {
        Value::Object(m) => {
            let tags: Vec<&String> = m.keys().filter(|k| k.starts_with('$')).collect();
            if !tags.is_empty() {
                let tag = tags[0].as_str();
                if m.len() != 1 || !["$ref", "$fieldclass", "$array"].contains(&tag) {
                    problems.push(format!(
                        "{}: {} is not a tagged attribute; the tags are $ref, $fieldclass, $array",
                        where_,
                        list_repr(&sorted_keys(m))
                    ));
                    return Attr::Null;
                }
                let val = &m[tag];
                return match tag {
                    "$ref" => match ParamRef::parse(&py_str(val)) {
                        Ok(r) => Attr::Ref(r),
                        Err(e) => {
                            problems.push(format!("{where_}: {e}"));
                            Attr::Null
                        }
                    },
                    "$fieldclass" => match FieldClass::from_json(val) {
                        Ok(fc) => Attr::FieldClass(fc),
                        Err(e) => {
                            problems.push(format!("{where_}: {e}"));
                            Attr::Null
                        }
                    },
                    _ => {
                        let key = py_str(val);
                        if let Some(a) = arrays.get(&key) {
                            Attr::Array(Arc::clone(a))
                        } else {
                            let have: Vec<String> = arrays.keys().cloned().collect();
                            problems.push(format!(
                                "{}: no array {} in the document's array table (it has {})",
                                where_,
                                py_repr(val),
                                if have.is_empty() { "none".to_string() } else { have.join(", ") }
                            ));
                            Attr::Null
                        }
                    }
                };
            }
            Attr::Dict(
                m.iter()
                    .map(|(k, x)| (k.clone(), decode_attr(x, arrays, problems, &format!("{where_}.{k}"))))
                    .collect(),
            )
        }
        Value::Array(a) => Attr::List(
            a.iter()
                .enumerate()
                .map(|(i, x)| decode_attr(x, arrays, problems, &format!("{where_}[{i}]")))
                .collect(),
        ),
        other => Attr::from_json(other),
    }
}

#[must_use]
pub fn node_invariant(node: &Node) -> String {
    match node.op().validate(node) {
        Ok(Some(m)) => m,
        Ok(None) => String::new(),
        Err(e) => format!("{}: {}", exc_type_name(&e), e),
    }
}

fn exc_type_name(e: &GeometryError) -> &'static str {
    match e {
        GeometryError::Model(_) => "ModelError",
        GeometryError::Transpile { .. } => "TranspileError",
        GeometryError::BoundViolation(_) => "BoundViolation",
        GeometryError::Expr(_) => "ExprError",
        GeometryError::ModelDoc(_) => "ModelDocError",
        _ => "ValueError",
    }
}

fn construct_all(
    norm: &Map<String, Value>,
    reg: &Registry,
    problems: &mut Vec<String>,
    ctx: &mut Ctx,
) -> GResult<()> {
    let mut arrays: BTreeMap<String, Arc<NdArray>> = BTreeMap::new();
    let table = norm.get("arrays").and_then(Value::as_object).cloned().unwrap_or_default();
    for (key, raw) in &ctx.raw {
        match table.get(key).map(|e| arrays::decode_array(e, raw)) {
            Some(Ok(a)) => {
                arrays.insert(key.clone(), Arc::new(a));
            }
            Some(Err(e)) => problems.push(format!("arrays.{key} cannot be materialised: {e}")),
            None => {}
        }
    }
    let nodes = norm.get("nodes").and_then(Value::as_object).cloned().unwrap_or_default();
    let order = ctx.order.clone();
    for nid in &order {
        let Some(entry) = nodes.get(nid) else { continue };
        let kind = entry.get("kind").and_then(Value::as_str).unwrap_or("?").to_string();
        if !reg.contains(&kind) {
            continue;
        }
        let children_spec = entry.get("children").and_then(Value::as_array).cloned().unwrap_or_default();
        let child_ids: Vec<String> = children_spec
            .iter()
            .filter_map(|c| c.get("node").and_then(Value::as_str).map(str::to_string))
            .collect();
        if child_ids.iter().any(|c| !ctx.objs.contains_key(c)) {
            continue;
        }
        let mut params = BTreeMap::new();
        let mut missing = false;
        if let Some(res) = ctx.resolved.get(nid) {
            for (p, v) in res {
                match v {
                    Resolved::Value(pv) => {
                        params.insert(p.clone(), pv.clone());
                    }
                    Resolved::Array(k) => match arrays.get(k) {
                        Some(a) => {
                            params.insert(p.clone(), ParamValue::Array(Arc::clone(a)));
                        }
                        None => missing = true,
                    },
                }
            }
        }
        if missing {
            continue;
        }
        let mut attrs = BTreeMap::new();
        if let Some(a) = entry.get("attrs").and_then(Value::as_object) {
            for (k, v) in a {
                attrs.insert(k.clone(), decode_attr(v, &arrays, problems, &format!("nodes.{nid}.attrs.{k}")));
            }
        }
        let clash: Vec<String> = attrs.keys().filter(|k| params.contains_key(*k)).cloned().collect();
        if !clash.is_empty() {
            problems.push(format!(
                "nodes.{}: attrs {} collide with parameters of the same name",
                nid,
                list_repr(&clash)
            ));
            continue;
        }
        let children: Vec<NodeRef> = child_ids.iter().filter_map(|c| ctx.objs.get(c).cloned()).collect();
        let names: Vec<String> = children_spec
            .iter()
            .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        let args = ConstructArgs { children, names: Some(names), params, attrs };
        let obj = match reg.construct(&kind, args) {
            Ok(o) => Arc::new(o),
            Err(GeometryError::ModelDoc(ps)) => {
                problems.extend(ps.into_iter().map(|p| format!("nodes.{nid}: {p}")));
                continue;
            }
            Err(e @ (GeometryError::Model(_) | GeometryError::Transpile { .. })) => {
                problems.push(format!("nodes.{nid} ({kind}): {e}"));
                continue;
            }
            Err(e) => return Err(e),
        };
        ctx.ids.insert(node_id(&obj), nid.clone());
        ctx.objs.insert(nid.clone(), Arc::clone(&obj));
        let msg = node_invariant(&obj);
        if !msg.is_empty() {
            problems.push(format!("nodes.{nid} ({kind}): {msg}"));
        }
    }
    for nid in sorted_keys(&nodes) {
        if ctx.objs.contains_key(&nid) {
            continue;
        }
        let kind = nodes[&nid].get("kind").and_then(Value::as_str).unwrap_or("?");
        if reg.contains(kind) && !problems.iter().any(|p| p.contains(nid.as_str())) {
            problems.push(format!("nodes.{nid} could not be constructed"));
        }
    }
    Ok(())
}


pub fn validate(
    doc: &Value,
    base_dir: Option<&Path>,
    reg: Option<&Registry>,
) -> GResult<(Value, Vec<String>)> {
    let owned;
    let reg = if let Some(r) = reg {
        r
    } else {
        owned = registry();
        &owned
    };
    let c = check(doc, base_dir, reg, true)?;
    if !c.problems.is_empty() {
        return Err(GeometryError::ModelDoc(c.problems));
    }
    Ok((Value::Object(c.out), c.warnings))
}


pub fn problems_of(
    doc: &Value,
    base_dir: Option<&Path>,
    reg: Option<&Registry>,
) -> GResult<(Vec<String>, Vec<String>)> {
    let owned;
    let reg = if let Some(r) = reg {
        r
    } else {
        owned = registry();
        &owned
    };
    let c = check(doc, base_dir, reg, true)?;
    Ok((c.problems, c.warnings))
}


pub fn build(doc: &Value, base_dir: Option<&Path>, reg: Option<&Registry>) -> GResult<Model> {
    let reg = reg.cloned().unwrap_or_else(registry);
    let c = check(doc, base_dir, &reg, true)?;
    if !c.problems.is_empty() {
        return Err(GeometryError::ModelDoc(c.problems));
    }
    Ok(Model {
        doc: Value::Object(c.out),
        nodes: c.ctx.objs,
        ids: c.ctx.ids,
        warnings: c.warnings,
        base_dir: base_dir.map(Path::to_path_buf),
        registry: reg,
        bindings: c.ctx.bindings,
        values: c.ctx.values,
        param_units: c.ctx.param_units,
        order: c.ctx.order,
        parents: c.ctx.parents,
    })
}

pub mod model;

pub use model::{
    Model, OutputRef, catalogue, check_roundtrip, encode_attr, fresh_key, from_graph, node_ids, read,
    small_array_json, write,
};

#[must_use]
pub fn entry_dtype(entry: &Value) -> Option<DType> {
    entry.get("dtype").and_then(Value::as_str).and_then(DType::from_name)
}

#[must_use]
pub fn resolve(base: Option<&Path>, rel: &str) -> PathBuf {
    base.map_or_else(|| PathBuf::from(rel), |b| b.join(rel))
}
