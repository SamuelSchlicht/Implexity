// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::Path;

use serde_json::{Map, Value, json};

use implexity_geometry::document::build;
use implexity_geometry::node::registry;

use crate::error::{AResult, AuthoringError};
use crate::py::{py_str, repr, truthy};

fn problem<T>(msg: String) -> AResult<T> {
    Err(AuthoringError::model_doc(vec![msg]))
}

#[must_use]
pub fn name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

const NAME_PATTERN: &str = "[A-Za-z_][A-Za-z0-9_.:-]{0,63}\\Z";

fn id(key: Option<&Value>, what: &str) -> AResult<String> {
    let key = key.filter(|v| truthy(v)).map_or_else(String::new, py_str);
    if !name_ok(&key) {
        return problem(format!("{what} id {} is invalid; use {NAME_PATTERN}", repr(&Value::from(key))));
    }
    Ok(key)
}

fn sorted_keys(m: Option<&Map<String, Value>>) -> Vec<String> {
    let mut k: Vec<String> = m.map(|m| m.keys().cloned().collect()).unwrap_or_default();
    k.sort();
    k
}

fn list_repr(items: &[String]) -> String {
    implexity_core::pyobj::list_repr(items)
}

fn nodes(doc: &Value) -> Option<&Map<String, Value>> {
    doc.get("nodes").and_then(Value::as_object)
}

fn nodes_mut(doc: &mut Value) -> AResult<&mut Map<String, Value>> {
    doc.get_mut("nodes")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AuthoringError::model_doc(vec!["the document has no node table".into()]))
}

fn node_check(doc: &Value, nid: &str) -> AResult<()> {
    if nodes(doc).is_some_and(|n| n.contains_key(nid)) {
        return Ok(());
    }
    problem(format!(
        "no node {}; the document has {}",
        repr(&Value::from(nid)),
        list_repr(&sorted_keys(nodes(doc)))
    ))
}

fn kind_of(doc: &Value, nid: &str) -> AResult<String> {
    nodes(doc)
        .and_then(|n| n.get(nid))
        .and_then(|e| e.get("kind"))
        .map(py_str)
        .ok_or_else(|| AuthoringError::Key("'kind'".into()))
}

fn fresh(doc: &Value, stem: &str) -> String {
    let mut s = String::new();
    let mut last_bad = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
            s.push(c);
            last_bad = false;
        } else if !last_bad {
            s.push('_');
            last_bad = true;
        }
    }
    let stem = s.trim_matches(|c| matches!(c, '_' | '.' | ':' | '-')).to_string();
    let mut stem = if stem.is_empty() { String::new() } else { stem };
    let first_ok = stem.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
    if stem.is_empty() || !first_ok {
        stem = format!("n_{stem}");
    }
    let stem: String = stem.chars().take(56).collect();
    let taken = nodes(doc);
    if !taken.is_some_and(|t| t.contains_key(&stem)) {
        return stem;
    }
    let mut i = 2;
    while taken.is_some_and(|t| t.contains_key(&format!("{stem}_{i}"))) {
        i += 1;
    }
    format!("{stem}_{i}")
}

fn default_entry(kind: &str) -> AResult<Map<String, Value>> {
    let reg = registry();
    let Some(entry) = reg.get(kind) else {
        return problem(format!(
            "unknown node kind {}; registered kinds are {}",
            repr(&Value::from(kind)),
            list_repr(&reg.names())
        ));
    };
    let mut specs: Vec<_> = entry.info.params.iter().collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    let params: Map<String, Value> = specs
        .iter()
        .map(|p| {
            (p.name.clone(), p.default.as_ref().map_or(Value::Null, implexity_geometry::ParamValue::to_json))
        })
        .collect();
    let units: Map<String, Value> =
        specs.iter().map(|p| (p.name.clone(), Value::from(p.units.clone()))).collect();
    let mut m = Map::new();
    m.insert("kind".into(), Value::from(kind));
    m.insert("children".into(), json!([]));
    m.insert("params".into(), Value::Object(params));
    m.insert("units".into(), Value::Object(units));
    Ok(m)
}

fn parents(doc: &Value, child: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = nodes(doc) {
        for (pid, ent) in n {
            for r in ent.get("children").and_then(Value::as_array).cloned().unwrap_or_default() {
                if r.get("node").and_then(Value::as_str) == Some(child) {
                    out.push(pid.clone());
                }
            }
        }
    }
    out
}

fn replace_refs(doc: &mut Value, old: &str, new: &str) {
    if let Some(n) = doc.get_mut("nodes").and_then(Value::as_object_mut) {
        for ent in n.values_mut() {
            if let Some(kids) = ent.get_mut("children").and_then(Value::as_array_mut) {
                for r in kids {
                    if r.get("node").and_then(Value::as_str) == Some(old) {
                        r["node"] = Value::from(new);
                    }
                }
            }
        }
    }
    if doc.get("root").and_then(Value::as_str) == Some(old) {
        doc["root"] = Value::from(new);
    }
    if let Some(outs) = doc.get_mut("outputs").and_then(Value::as_object_mut) {
        for v in outs.values_mut() {
            if v.as_str() == Some(old) {
                *v = Value::from(new);
            }
        }
    }
}

fn param_names(kind: &str) -> Vec<String> {
    registry().get(kind).map(|e| e.info.sorted_param_names()).unwrap_or_default()
}

fn op_add_node(doc: &mut Value, op: &Value) -> AResult<Value> {
    let kind = op.get("kind").filter(|v| truthy(v)).map_or_else(String::new, py_str);
    let supplied = op.get("id").filter(|v| truthy(v)).cloned();
    let nid = id(Some(&supplied.unwrap_or_else(|| Value::from(fresh(doc, &kind)))), "new node")?;
    if nodes(doc).is_some_and(|n| n.contains_key(&nid)) {
        return problem(format!("node {} already exists", repr(&Value::from(nid))));
    }
    let mut ent = default_entry(&kind)?;
    if let Some(params) = op.get("params").filter(|v| truthy(v)) {
        let Some(pm) = params.as_object() else {
            return Err(AuthoringError::Type(format!(
                "'{}' object has no attribute 'items'",
                crate::py::type_name(params)
            )));
        };
        for (k, v) in pm {
            let known = ent.get("params").and_then(Value::as_object).is_some_and(|p| p.contains_key(k));
            if !known {
                let names = sorted_keys(ent.get("params").and_then(Value::as_object));
                return problem(format!(
                    "{kind} has no parameter {}; it has {}",
                    repr(&Value::from(k.clone())),
                    list_repr(&names)
                ));
            }
            if let Some(p) = ent.get_mut("params").and_then(Value::as_object_mut) {
                p.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(children) = op.get("children").filter(|v| !v.is_null()) {
        let list = match children {
            Value::Array(a) => a.clone(),
            other => {
                return Err(AuthoringError::Type(format!(
                    "'{}' object is not iterable",
                    crate::py::type_name(other)
                )));
            }
        };
        ent.insert("children".into(), Value::Array(list));
    }
    if let Some(attrs) = op.get("attrs").filter(|v| !v.is_null()) {
        ent.insert("attrs".into(), attrs.clone());
    }
    if let Some(d) = op.get("doc").filter(|v| truthy(v)) {
        ent.insert("doc".into(), Value::from(py_str(d)));
    }
    nodes_mut(doc)?.insert(nid.clone(), Value::Object(ent));
    Ok(json!({"op": "add_node", "id": nid, "kind": kind}))
}

fn op_delete_node(doc: &mut Value, op: &Value) -> AResult<Value> {
    let nid = id(op.get("id"), "node")?;
    node_check(doc, &nid)?;
    let refs = parents(doc, &nid);
    if !refs.is_empty() {
        return problem(format!(
            "cannot delete node {} while it is referenced by {}; rewire or disconnect those parents in an earlier operation of the same batch",
            repr(&Value::from(nid)),
            list_repr(&refs)
        ));
    }
    if doc.get("root").and_then(Value::as_str) == Some(nid.as_str()) {
        return problem(format!(
            "cannot delete root node {}; set another root first",
            repr(&Value::from(nid))
        ));
    }
    let outs: Vec<String> = doc
        .get("outputs")
        .and_then(Value::as_object)
        .map(|o| o.iter().filter(|(_, v)| v.as_str() == Some(nid.as_str())).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    if !outs.is_empty() {
        return problem(format!(
            "cannot delete node {}; outputs {} still name it",
            repr(&Value::from(nid)),
            list_repr(&outs)
        ));
    }
    nodes_mut(doc)?.shift_remove(&nid);
    Ok(json!({"op": "delete_node", "id": nid}))
}

fn op_rename_node(doc: &mut Value, op: &Value) -> AResult<Value> {
    let old = id(op.get("id"), "node")?;
    node_check(doc, &old)?;
    let new = id(op.get("new_id"), "new node")?;
    if nodes(doc).is_some_and(|n| n.contains_key(&new)) {
        return problem(format!("node {} already exists", repr(&Value::from(new))));
    }
    let n = nodes_mut(doc)?;
    let ent = n.shift_remove(&old).unwrap_or(Value::Null);
    n.insert(new.clone(), ent);
    replace_refs(doc, &old, &new);
    Ok(json!({"op": "rename_node", "id": old, "new_id": new}))
}

fn op_set_root(doc: &mut Value, op: &Value) -> AResult<Value> {
    let nid = id(op.get("id"), "node")?;
    node_check(doc, &nid)?;
    let was = doc.get("root").cloned().unwrap_or(Value::Null);
    doc["root"] = Value::from(nid.clone());
    Ok(json!({"op": "set_root", "id": nid, "was": was}))
}

fn op_connect(doc: &mut Value, op: &Value) -> AResult<Value> {
    let parent = id(op.get("parent"), "parent")?;
    let child = id(op.get("child"), "child")?;
    node_check(doc, &parent)?;
    node_check(doc, &child)?;
    let name = op.get("name").filter(|v| truthy(v)).map_or_else(String::new, py_str);
    if name.is_empty() {
        return problem("connect needs a non-empty child socket 'name'".into());
    }
    let ent = nodes_mut(doc)?
        .get_mut(&parent)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AuthoringError::Key(repr(&Value::from(parent.clone()))))?;
    let kids = crate::py::setdefault_list(ent, "children")?;
    let hit = kids.iter().position(|r| r.get("name").and_then(Value::as_str) == Some(name.as_str()));
    let rf = json!({"name": name, "node": child});
    let mut report = json!({"op": "connect", "parent": parent, "child": child, "name": name});
    match hit {
        None => {
            kids.push(rf);
            report["action"] = json!("added");
        }
        Some(i) => {
            let was = kids[i].get("node").cloned().unwrap_or(Value::Null);
            kids[i] = rf;
            report["action"] = json!("replaced");
            if !was.is_null() {
                report["was"] = was;
            }
        }
    }
    Ok(report)
}

fn op_disconnect(doc: &mut Value, op: &Value) -> AResult<Value> {
    let parent = id(op.get("parent"), "parent")?;
    node_check(doc, &parent)?;
    let name = op.get("name").filter(|v| truthy(v)).map_or_else(String::new, py_str);
    let ent = nodes_mut(doc)?
        .get_mut(&parent)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AuthoringError::Key(repr(&Value::from(parent.clone()))))?;
    let kids = ent.get("children").and_then(Value::as_array).cloned().unwrap_or_default();
    let kept: Vec<Value> = kids
        .iter()
        .filter(|r| r.get("name").and_then(Value::as_str) != Some(name.as_str()))
        .cloned()
        .collect();
    if kept.len() == kids.len() {
        return problem(format!(
            "node {} has no child socket {}",
            repr(&Value::from(parent)),
            repr(&Value::from(name))
        ));
    }
    ent.insert("children".into(), Value::Array(kept));
    Ok(json!({"op": "disconnect", "parent": parent, "name": name}))
}

fn op_set_node_param(doc: &mut Value, op: &Value) -> AResult<Value> {
    let nid = id(op.get("id"), "node")?;
    node_check(doc, &nid)?;
    let kind = kind_of(doc, &nid)?;
    let param = op.get("param").filter(|v| truthy(v)).map_or_else(String::new, py_str);
    if registry().get(&kind).is_none() {
        return Err(AuthoringError::Key(repr(&Value::from(kind))));
    }
    let names = param_names(&kind);
    if !names.contains(&param) {
        return problem(format!(
            "{kind} has no parameter {}; it has {}",
            repr(&Value::from(param)),
            list_repr(&names)
        ));
    }
    let ent = nodes_mut(doc)?
        .get_mut(&nid)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AuthoringError::Key(repr(&Value::from(nid.clone()))))?;
    let params = crate::py::setdefault_obj(ent, "params")?;
    let was = params.get(&param).cloned().unwrap_or(Value::Null);
    let value = op.get("value").cloned().unwrap_or(Value::Null);
    params.insert(param.clone(), value.clone());
    Ok(json!({"op": "set_node_param", "id": nid, "param": param, "was": was, "now": value}))
}

fn op_set_node_attr(doc: &mut Value, op: &Value) -> AResult<Value> {
    let nid = id(op.get("id"), "node")?;
    node_check(doc, &nid)?;
    let kind = kind_of(doc, &nid)?;
    let attr = op.get("attr").filter(|v| truthy(v)).map_or_else(String::new, py_str);
    if registry().get(&kind).is_none() {
        return Err(AuthoringError::Key(repr(&Value::from(kind))));
    }
    if param_names(&kind).contains(&attr) {
        return problem(format!("{} is a parameter of {kind}; use set_node_param", repr(&Value::from(attr))));
    }
    let Some(init) = crate::kinds::shape(&kind).init else {
        return problem(format!("{kind} does not declare editable constructor attributes"));
    };
    if ["self", "children", "names"].contains(&attr.as_str()) || !init.contains(&attr) {
        return problem(format!("{kind} has no constructor attribute {}", repr(&Value::from(attr))));
    }
    let Some(value) = op.get("value") else {
        return problem("set_node_attr needs an explicit 'value'".into());
    };
    let ent = nodes_mut(doc)?
        .get_mut(&nid)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AuthoringError::Key(repr(&Value::from(nid.clone()))))?;
    let attrs = crate::py::setdefault_obj(ent, "attrs")?;
    let was = attrs.get(&attr).cloned().unwrap_or(Value::Null);
    attrs.insert(attr.clone(), value.clone());
    Ok(json!({"op": "set_node_attr", "id": nid, "attr": attr, "was": was, "now": value}))
}

fn parameter_definition(op: &Value, previous: Option<&Map<String, Value>>) -> AResult<Map<String, Value>> {
    let value = op.get("value");
    let expr = op.get("expr");
    if value.is_some() && expr.is_some() {
        return problem("a parameter definition needs exactly one of value and expr".into());
    }
    if previous.is_some() && value.is_none() && expr.is_none() {
        return problem("set_parameter_definition needs an explicit value or expr".into());
    }
    let mut ent = previous.cloned().unwrap_or_default();
    ent.remove("value");
    ent.remove("expr");
    if let Some(expr) = expr {
        ent.insert("expr".into(), expr.clone());
    } else {
        ent.insert("value".into(), value.cloned().unwrap_or(json!(0.0)));
    }
    if let Some(units) = op.get("units") {
        if !units.is_string() {
            return problem("parameter units must be a string".into());
        }
        if let Some(previous) = previous {
            let from = previous.get("units").and_then(Value::as_str).unwrap_or("-");
            let to = units.as_str().unwrap_or("-");
            if from != to {
                for key in ["min", "max"] {
                    if op.get(key).is_none() && let Some(bound) = previous.get(key).and_then(Value::as_f64) {
                        ent.insert(key.into(), json!(implexity_geometry::document::convert(bound, from, to)?));
                    }
                }
            }
        }
        ent.insert("units".into(), units.clone());
    } else {
        ent.entry("units").or_insert(json!("-"));
    }
    for key in ["doc", "min", "max", "free"] {
        if let Some(value) = op.get(key) {
            if value.is_null() {
                ent.remove(key);
            } else {
                ent.insert(key.into(), value.clone());
            }
        }
    }
    Ok(ent)
}

fn op_set_parameter_definition(doc: &mut Value, op: &Value) -> AResult<Value> {
    let name = id(op.get("name"), "parameter")?;
    let table = crate::py::setdefault_obj(crate::py::obj_mut(doc)?, "parameters")?;
    let previous = table.get(&name).and_then(Value::as_object).ok_or_else(|| {
        AuthoringError::model_doc(vec![format!("no parameter {}", repr(&Value::from(name.clone())))])
    })?;
    let definition = parameter_definition(op, Some(previous))?;
    let was = Value::Object(previous.clone());
    let now = Value::Object(definition);
    table.insert(name.clone(), now.clone());
    Ok(json!({"op":"set_parameter_definition", "name":name, "was":was, "now":now}))
}

fn op_add_parameter(doc: &mut Value, op: &Value) -> AResult<Value> {
    let name = id(op.get("name"), "parameter")?;
    let root = crate::py::obj_mut(doc)?;
    let table = crate::py::setdefault_obj(root, "parameters")?;
    if table.contains_key(&name) {
        return problem(format!("parameter {} already exists", repr(&Value::from(name))));
    }
    let ent = parameter_definition(op, None)?;
    table.insert(name.clone(), Value::Object(ent));
    Ok(json!({"op": "add_parameter", "name": name}))
}

fn op_delete_parameter(doc: &mut Value, op: &Value) -> AResult<Value> {
    let name = id(op.get("name"), "parameter")?;
    let exists = doc
        .get("parameters")
        .filter(|v| truthy(v))
        .and_then(Value::as_object)
        .is_some_and(|t| t.contains_key(&name));
    if !exists {
        return problem(format!("no parameter {}", repr(&Value::from(name))));
    }
    let mut direct = Vec::new();
    if let Some(n) = nodes(doc) {
        for (nid, ent) in n {
            if let Some(ps) = ent.get("params").and_then(Value::as_object) {
                for (p, v) in ps {
                    if v.is_object() && v.get("bind").and_then(Value::as_str) == Some(name.as_str()) {
                        direct.push(format!("{nid}:{p}"));
                    }
                }
            }
        }
    }
    if !direct.is_empty() {
        return problem(format!(
            "cannot delete parameter {}; it is bound by {}",
            repr(&Value::from(name)),
            list_repr(&direct)
        ));
    }
    if let Some(t) = doc.get_mut("parameters").and_then(Value::as_object_mut) {
        t.shift_remove(&name);
    }
    Ok(json!({"op": "delete_parameter", "name": name}))
}

fn op_bind(doc: &mut Value, op: &Value) -> AResult<Value> {
    let nid = id(op.get("id"), "node")?;
    node_check(doc, &nid)?;
    let kind = kind_of(doc, &nid)?;
    let param = op.get("param").filter(|v| truthy(v)).map_or_else(String::new, py_str);
    if registry().get(&kind).is_none() {
        return Err(AuthoringError::Key(repr(&Value::from(kind))));
    }
    if !param_names(&kind).contains(&param) {
        return problem(format!("{kind} has no parameter {}", repr(&Value::from(param))));
    }
    let val = if op.get("bind").is_some() {
        let name = id(op.get("bind"), "parameter")?;
        let known = doc
            .get("parameters")
            .filter(|v| truthy(v))
            .and_then(Value::as_object)
            .is_some_and(|t| t.contains_key(&name));
        if !known {
            return problem(format!("no named parameter {}", repr(&Value::from(name))));
        }
        json!({"bind": name})
    } else if let Some(e) = op.get("expr") {
        json!({"expr": py_str(e)})
    } else if let Some(a) = op.get("array") {
        json!({"array": py_str(a)})
    } else {
        return problem("bind operation needs one of 'bind', 'expr' or 'array'".into());
    };
    let ent = nodes_mut(doc)?
        .get_mut(&nid)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AuthoringError::Key(repr(&Value::from(nid.clone()))))?;
    crate::py::setdefault_obj(ent, "params")?.insert(param.clone(), val.clone());
    Ok(json!({"op": "bind", "id": nid, "param": param, "value": val}))
}

pub const OPS: [&str; 12] = [
    "add_node",
    "add_parameter",
    "bind",
    "connect",
    "delete_node",
    "delete_parameter",
    "disconnect",
    "rename_node",
    "set_node_attr",
    "set_node_param",
    "set_parameter_definition",
    "set_root",
];

fn run_op(name: &str, doc: &mut Value, op: &Value) -> AResult<Value> {
    match name {
        "add_node" => op_add_node(doc, op),
        "delete_node" => op_delete_node(doc, op),
        "rename_node" => op_rename_node(doc, op),
        "set_root" => op_set_root(doc, op),
        "connect" => op_connect(doc, op),
        "disconnect" => op_disconnect(doc, op),
        "set_node_param" => op_set_node_param(doc, op),
        "set_node_attr" => op_set_node_attr(doc, op),
        "add_parameter" => op_add_parameter(doc, op),
        "set_parameter_definition" => op_set_parameter_definition(doc, op),
        "delete_parameter" => op_delete_parameter(doc, op),
        _ => op_bind(doc, op),
    }
}

fn python_type_name(e: &AuthoringError) -> String {
    match e {
        AuthoringError::Key(_) => "KeyError".into(),
        AuthoringError::Type(_) => "TypeError".into(),
        other => other.class().to_string(),
    }
}


pub fn apply(
    document: &Value,
    operations: Option<&Value>,
    base_dir: Option<&Path>,
) -> AResult<(Value, Value)> {
    if !document.is_object() {
        return problem("the stored model document is not a JSON object".into());
    }
    let ops: Vec<Value> = match operations {
        Some(v @ Value::Object(_)) => vec![v.clone()],
        Some(Value::Array(a)) if !a.is_empty() => a.clone(),
        _ => return problem("send {'operations': [{...}, ...]} with at least one edit".into()),
    };
    let mut out = document.clone();
    if let Some(m) = out.as_object_mut() {
        m.entry("nodes").or_insert_with(|| json!({}));
    }
    let mut applied = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if !op.is_object() {
            return problem(format!("operations[{i}] must be an object"));
        }
        let name = op.get("op").filter(|v| truthy(v)).map_or_else(String::new, py_str);
        if !OPS.contains(&name.as_str()) {
            return problem(format!(
                "operations[{i}].op {} is unknown; use {}",
                repr(&Value::from(name)),
                list_repr(&OPS.iter().map(ToString::to_string).collect::<Vec<_>>())
            ));
        }
        match run_op(&name, &mut out, op) {
            Ok(r) => applied.push(r),
            Err(e) if e.is_model_doc() => return Err(e),
            Err(e) => return problem(format!("operations[{i}] {name}: {}: {e}", python_type_name(&e))),
        }
    }
    let model = build(&out, base_dir, None)?;
    let norm = model.to_doc()?;
    let root = norm.get("root").cloned().unwrap_or(Value::Null);
    Ok((
        norm,
        json!({"kind": "implicit_graph_edit", "operations": applied, "count": applied.len(),
            "structure_id": model.structure_id(), "content_id": model.content_id(), "root": root}),
    ))
}
