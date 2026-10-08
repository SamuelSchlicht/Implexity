// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::arrays::{encode_array, inline_entry, npy_bytes, sidecar_entry};
use super::{
    Binding, DOC_UNITS, SCHEMA, build, canonical_bytes, check, dumps, expr_names, node_invariant, py_repr,
    py_str, sha256_of, unit_info, validate,
};
use crate::error::{GResult, GeometryError};
use crate::eval::node_id;
use crate::fieldclass::FieldClass;
use crate::node::{Attr, Node, NodeRef, Registry};
use crate::pyfmt::str_repr;
use crate::value::{DType, NdArray, ParamValue, json_f64};

pub struct Model {
    pub doc: Value,
    pub(super) nodes: BTreeMap<String, NodeRef>,
    pub(super) ids: HashMap<usize, String>,
    pub warnings: Vec<String>,
    pub(super) base_dir: Option<PathBuf>,
    pub(super) registry: Registry,
    pub(super) bindings: BTreeMap<String, BTreeMap<String, Binding>>,
    pub(super) values: BTreeMap<String, f64>,
    pub(super) param_units: BTreeMap<String, String>,
    pub(super) order: Vec<String>,
    pub(super) parents: BTreeMap<String, Vec<String>>,
}

fn mdoc<T>(p: String) -> GResult<T> {
    Err(GeometryError::ModelDoc(vec![p]))
}

impl Model {
    #[must_use]
    pub fn root(&self) -> Option<NodeRef> {
        self.doc.get("root").and_then(Value::as_str).and_then(|r| self.nodes.get(r)).cloned()
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut s: BTreeSet<String> = self.nodes.keys().cloned().collect();
        if let Some(o) = self.doc.get("outputs").and_then(Value::as_object) {
            s.extend(o.keys().cloned());
        }
        s.into_iter().collect()
    }


    pub fn node(&self, name: &str) -> GResult<NodeRef> {
        let nid = self.doc.get("outputs").and_then(|o| o.get(name)).and_then(Value::as_str).unwrap_or(name);
        self.nodes.get(nid).cloned().ok_or_else(|| {
            GeometryError::ModelDoc(vec![format!(
                "no node or output named {} in this model; it has {}",
                str_repr(name),
                self.names().join(", ")
            )])
        })
    }

    #[must_use]
    pub fn node_table(&self) -> &BTreeMap<String, NodeRef> {
        &self.nodes
    }

    #[must_use]
    pub fn id_of(&self, node: &NodeRef) -> Option<String> {
        self.ids.get(&node_id(node)).cloned()
    }

    #[must_use]
    pub fn shared(&self) -> BTreeMap<String, Vec<String>> {
        self.parents.iter().filter(|(_, v)| v.len() > 1).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    #[must_use]
    pub fn parents(&self) -> &BTreeMap<String, Vec<String>> {
        &self.parents
    }

    #[must_use]
    pub fn bindings(&self) -> &BTreeMap<String, BTreeMap<String, Binding>> {
        &self.bindings
    }

    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, f64> {
        &self.values
    }

    #[must_use]
    pub fn param_units(&self) -> &BTreeMap<String, String> {
        &self.param_units
    }

    #[must_use]
    pub fn order(&self) -> &[String] {
        &self.order
    }

    #[must_use]
    pub fn base_dir(&self) -> Option<&Path> {
        self.base_dir.as_deref()
    }

    #[must_use]
    pub fn structure_id(&self) -> Option<String> {
        self.root().map(|r| r.structure_id())
    }

    #[must_use]
    pub fn content_id(&self) -> Option<String> {
        self.root().map(|r| r.content_id())
    }

    fn rebuild(
        &self,
        changed: &BTreeMap<String, BTreeMap<String, ParamValue>>,
    ) -> (BTreeMap<String, NodeRef>, HashMap<usize, String>) {
        let mut new_nodes: BTreeMap<String, NodeRef> = BTreeMap::new();
        for nid in &self.order {
            let Some(old) = self.nodes.get(nid) else { continue };
            let kids: Vec<NodeRef> = old
                .children()
                .iter()
                .map(|c| {
                    self.ids
                        .get(&node_id(c))
                        .and_then(|cid| new_nodes.get(cid))
                        .cloned()
                        .unwrap_or_else(|| Arc::clone(c))
                })
                .collect();
            let mut params = old.params().clone();
            if let Some(ch) = changed.get(nid) {
                for (k, v) in ch {
                    params.insert(k.clone(), v.clone());
                }
            }
            new_nodes.insert(nid.clone(), Arc::new(old.with_params(params).with_children(kids)));
        }
        for (nid, n) in &self.nodes {
            new_nodes.entry(nid.clone()).or_insert_with(|| Arc::clone(n));
        }
        let ids = new_nodes.iter().map(|(k, v)| (node_id(v), k.clone())).collect();
        (new_nodes, ids)
    }


    pub fn parameter_consumers(&self, root: &NodeRef, parameter: &str) -> GResult<Vec<(crate::ParamRef, f64)>> {
        let table = self.doc.get("parameters").and_then(Value::as_object).ok_or_else(|| GeometryError::Expr("document has no parameters".into()))?;
        if !table.contains_key(parameter) || table[parameter].get("expr").is_some() { return Err(GeometryError::Expr("optimization requires an independent document parameter".into())); }
        let mut direction = BTreeMap::new();
        let mut dependencies = BTreeMap::<String, std::collections::BTreeSet<String>>::new();
        let mut pending = BTreeMap::new();
        for (name, entry) in table {
            if let Some(text) = entry.get("expr").and_then(Value::as_str) { pending.insert(name.clone(), super::expr::parse_expr(text)?); }
            else { direction.insert(name.clone(), if name == parameter { 1.0 } else { 0.0 }); dependencies.insert(name.clone(), std::collections::BTreeSet::from([name.clone()])); }
        }
        while !pending.is_empty() {
            let ready: Vec<String> = pending.iter().filter(|(_,tree)| super::expr::expr_names(tree).iter().all(|name| direction.contains_key(name))).map(|(name,_)| name.clone()).collect();
            if ready.is_empty() { return Err(GeometryError::Expr("parameter dependency cycle or unknown parameter".into())); }
            for name in ready {
                let tree = pending.remove(&name).ok_or_else(|| GeometryError::Expr("parameter dependency changed".into()))?;
                let (_, derivative) = super::expr::eval_tree_directional(&tree, &self.values, &direction)?;
                let deps = super::expr::expr_names(&tree).iter().flat_map(|name| dependencies.get(name).into_iter().flatten().cloned()).collect();
                direction.insert(name.clone(), derivative); dependencies.insert(name, deps);
            }
        }
        let mut out = Vec::new();
        for (path, node) in root.walk() {
            let Some(id) = self.id_of(&node) else { continue };
            let Some(bindings) = self.bindings.get(&id) else { continue };
            for (name, binding) in bindings {
                let (affected, derivative, source_units) = match binding {
                    super::Binding::Bind { name: bound, src_unit } => (dependencies.get(bound).is_some_and(|d| d.contains(parameter)), direction.get(bound).copied().unwrap_or(0.0), src_unit),
                    super::Binding::Expr { tree, src_unit } => (super::expr::expr_names(tree).iter().any(|name| dependencies.get(name).is_some_and(|d| d.contains(parameter))), super::expr::eval_tree_directional(tree, &self.values, &direction)?.1, src_unit),
                    _ => continue,
                };
                if !affected { continue; }
                let info = node.info();
                if info.discrete.contains(name) { return Err(GeometryError::Expr(format!("document parameter {parameter} drives discrete parameter {id}:{name}"))); }
                let target_units = info.param(name).map(|p| p.units.as_str()).ok_or_else(|| GeometryError::Expr("bound node parameter is missing".into()))?;
                let coefficient = super::convert(derivative, source_units, target_units)?;
                if !coefficient.is_finite() { return Err(GeometryError::Expr("parameter binding derivative is not finite".into())); }
                out.push((crate::ParamRef::new(path.clone(), name.clone()), coefficient));
            }
        }
        Ok(out)
    }

    pub fn set_parameters(&mut self, values: &BTreeMap<String, Value>) -> GResult<Value> {
        let before_s = self.structure_id();
        let before_c = self.content_id();
        let table = self.doc.get("parameters").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut problems = Vec::new();
        for (name, v) in values {
            match table.get(name) {
                None => problems.push(format!(
                    "no parameter {}; this model declares {}",
                    str_repr(name),
                    if table.is_empty() {
                        "none".to_string()
                    } else {
                        table.keys().cloned().collect::<Vec<_>>().join(", ")
                    }
                )),
                Some(e) if e.get("expr").is_some() => problems.push(format!(
                    "parameter {} is derived ({}); set the parameters it reads instead",
                    str_repr(name),
                    py_str(&e["expr"])
                )),
                Some(_) if !matches!(v, Value::Number(n) if n.as_f64().is_some_and(f64::is_finite)) => {
                    problems.push(format!(
                        "parameter {} must be set to a finite number, got {}",
                        str_repr(name),
                        py_repr(v)
                    ));
                }
                Some(_) => {}
            }
        }
        if !problems.is_empty() {
            return Err(GeometryError::ModelDoc(problems));
        }
        let mut doc = self.doc.clone();
        let mut params = table.clone();
        for (name, v) in values {
            if let Some(e) = params.get_mut(name).and_then(Value::as_object_mut) {
                e.insert("value".into(), json_f64(v.as_f64().unwrap_or(0.0)));
            }
        }
        if let Some(o) = doc.as_object_mut() {
            o.insert("parameters".into(), Value::Object(params));
        }
        let c = check(&doc, self.base_dir.as_deref(), &self.registry, false)?;
        if !c.problems.is_empty() {
            return Err(GeometryError::ModelDoc(c.problems));
        }
        let norm_nodes = c.out.get("nodes").cloned().unwrap_or(Value::Null);
        let mut moved = Vec::new();
        let mut changed: BTreeMap<String, BTreeMap<String, ParamValue>> = BTreeMap::new();
        for (nid, binds) in &self.bindings {
            let Some(node) = self.nodes.get(nid) else { continue };
            for p in binds.keys() {
                let Some(super::Resolved::Value(new)) = c.ctx.resolved.get(nid).and_then(|r| r.get(p)) else {
                    continue;
                };
                let old = node.param(p).cloned();
                let same = match (&old, new) {
                    (Some(o), n) => match (o.scalar_f64(), n.scalar_f64()) {
                        (Some(a), Some(b)) if !o.is_array() => a == b,
                        _ => o == n,
                    },
                    (None, _) => false,
                };
                if !same {
                    changed.entry(nid.clone()).or_default().insert(p.clone(), new.clone());
                    moved.push(serde_json::json!({
                        "node": nid, "param": p,
                        "was": old.map_or(Value::Null, |o| o.to_json()),
                        "now": new.to_json(),
                        "units": norm_nodes.get(nid).and_then(|n| n.get("units")).and_then(|u| u.get(p)).cloned().unwrap_or(Value::Null),
                        "shared": self.parents.get(nid).is_some_and(|v| v.len() > 1),
                    }));
                }
            }
        }
        let (new_nodes, new_ids) = self.rebuild(&changed);
        let mut broken = Vec::new();
        for m in &moved {
            let nid = m["node"].as_str().unwrap_or_default();
            if let Some(n) = new_nodes.get(nid) {
                let msg = node_invariant(n);
                if !msg.is_empty() {
                    broken.push(format!("nodes.{} ({}): {}", nid, n.kind(), msg));
                }
            }
        }
        if !broken.is_empty() {
            return Err(GeometryError::ModelDoc(broken));
        }
        self.nodes = new_nodes;
        self.ids = new_ids;
        self.doc = Value::Object(c.out);
        self.values = c.ctx.values.clone();
        self.warnings = c.warnings;
        let after_s = self.structure_id();
        Ok(serde_json::json!({
            "set": values.iter().map(|(k, v)| (k.clone(), json_f64(v.as_f64().unwrap_or(0.0)))).collect::<Map<String, Value>>(),
            "parameters": self.values.iter().map(|(k, v)| (k.clone(), json_f64(*v))).collect::<Map<String, Value>>(),
            "moved": moved,
            "structure_id": after_s,
            "structure_id_before": before_s,
            "recompiled": after_s != before_s,
            "content_id": self.content_id(),
            "content_id_before": before_c,
            "warnings": self.warnings,
        }))
    }


    pub fn set_node_param(&mut self, name: &str, param: &str, value: &ParamValue) -> GResult<Value> {
        let node = self.node(name)?;
        let nid = self.id_of(&node).unwrap_or_else(|| name.to_string());
        if node.info().param(param).is_none() {
            let have = node.info().sorted_param_names();
            return mdoc(format!(
                "{} has no parameter {}; it has {}",
                node.kind(),
                str_repr(param),
                if have.is_empty() { "none".to_string() } else { have.join(", ") }
            ));
        }
        if let Some(b) = self.bindings.get(&nid).and_then(|m| m.get(param)) {
            return mdoc(format!(
                "{}.{} is bound to {}; setting it directly would be undone by the next parameter edit",
                nid,
                param,
                b.spec_repr()
            ));
        }
        let mut changed = BTreeMap::new();
        changed.entry(nid.clone()).or_insert_with(BTreeMap::new).insert(param.to_string(), value.clone());
        let (nodes, ids) = self.rebuild(&changed);
        self.nodes = nodes;
        self.ids = ids;
        Ok(serde_json::json!({
            "node": nid, "param": param, "now": value.to_json(),
            "parents": self.parents.get(&nid).cloned().unwrap_or_default(),
            "shared": self.parents.get(&nid).is_some_and(|v| v.len() > 1),
            "content_id": self.content_id(),
        }))
    }


    pub fn to_doc(&self) -> GResult<Value> {
        let mut out = self.doc.as_object().cloned().unwrap_or_default();
        let mut arrays: Map<String, Value> =
            self.doc.get("arrays").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut nodes = Map::new();
        for (nid, node) in &self.nodes {
            let stored = self.doc.get("nodes").and_then(|n| n.get(nid)).cloned().unwrap_or(Value::Null);
            let mut kids = Vec::new();
            for (name, child) in node.named_children() {
                let Some(cid) = self.ids.get(&node_id(child)) else {
                    return mdoc(format!(
                        "node {} has a child ({}) that is not in this document's table -- the graph was rewired outside the document, which the document cannot describe",
                        str_repr(nid),
                        child.kind()
                    ));
                };
                kids.push(serde_json::json!({"name": name, "node": cid}));
            }
            let binds = self.bindings.get(nid);
            let mut params = Map::new();
            for p in node.info().sorted_param_names() {
                if binds.is_some_and(|b| b.contains_key(&p)) {
                    params.insert(
                        p.clone(),
                        stored.get("params").and_then(|x| x.get(&p)).cloned().unwrap_or(Value::Null),
                    );
                    continue;
                }
                params.insert(p.clone(), param_to_json(node.param(&p), &mut arrays, &stored, nid, &p)?);
            }
            let mut entry = Map::new();
            entry.insert("kind".into(), Value::from(node.kind()));
            entry.insert("children".into(), Value::Array(kids));
            entry.insert("params".into(), Value::Object(params));
            let mut units = Map::new();
            for p in node.info().sorted_param_names() {
                units.insert(
                    p.clone(),
                    Value::from(node.info().param(&p).map(|s| s.units.clone()).unwrap_or_default()),
                );
            }
            entry.insert("units".into(), Value::Object(units));
            if let Some(d) = stored.get("doc") {
                entry.insert("doc".into(), d.clone());
            }
            let attrs = attrs_of(node, &mut arrays)?;
            if !attrs.is_empty() {
                entry.insert("attrs".into(), Value::Object(attrs));
            }
            nodes.insert(nid.clone(), Value::Object(entry));
        }
        out.insert("nodes".into(), Value::Object(nodes));
        if arrays.is_empty() {
            out.remove("arrays");
        } else {
            out.insert("arrays".into(), Value::Object(arrays));
        }
        Ok(Value::Object(out))
    }


    pub fn dumps(&self) -> GResult<String> {
        Ok(dumps(&self.to_doc()?))
    }


    pub fn sha256(&self) -> GResult<String> {
        Ok(sha256_of(&self.to_doc()?))
    }

    pub fn field_classes(&mut self) -> BTreeMap<String, FieldClass> {
        let mut out: BTreeMap<String, FieldClass> = BTreeMap::new();
        for nid in self.order.clone() {
            let Some(node) = self.nodes.get(&nid).cloned() else { continue };
            let kids: Vec<FieldClass> = node
                .children()
                .iter()
                .filter_map(|c| self.ids.get(&node_id(c)).and_then(|cid| out.get(cid)).cloned())
                .collect();
            match node.op().field_class(&node, &kids) {
                Ok(fc) => {
                    out.insert(nid, fc);
                }
                Err(e) => {
                    self.warnings.push(format!("nodes.{}.field_class raised {}: {}", nid, err_class(&e), e));
                }
            }
        }
        out
    }

    pub fn describe(&mut self) -> Value {
        let classes = self.field_classes();
        let mut alias: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(o) = self.doc.get("outputs").and_then(Value::as_object) {
            for (a, nid) in o {
                alias.entry(py_str(nid)).or_default().push(a.clone());
            }
        }
        let mut rows = Vec::new();
        for (nid, node) in &self.nodes {
            let children: Vec<Value> = node
                .named_children()
                .map(|(n, c)| serde_json::json!({"name": n, "node": self.ids.get(&node_id(c)).cloned().unwrap_or_default()}))
                .collect();
            let mut parents = self.parents.get(nid).cloned().unwrap_or_default();
            parents.sort();
            let mut outputs = alias.get(nid).cloned().unwrap_or_default();
            outputs.sort();
            let units: Map<String, Value> = node
                .info()
                .sorted_param_names()
                .into_iter()
                .map(|p| {
                    (
                        p.clone(),
                        Value::from(node.info().param(&p).map(|s| s.units.clone()).unwrap_or_default()),
                    )
                })
                .collect();
            let bound: Map<String, Value> = self
                .bindings
                .get(nid)
                .map(|b| b.iter().map(|(p, v)| (p.clone(), Value::from(v.kind()))).collect())
                .unwrap_or_default();
            rows.push(serde_json::json!({
                "id": nid, "kind": node.kind(), "children": children, "parents": parents,
                "shared": self.parents.get(nid).is_some_and(|v| v.len() > 1),
                "outputs": outputs,
                "params": node.describe()["params"].clone(),
                "units": units, "bound": bound,
                "structure_id": node.structure_id(), "content_id": node.content_id(),
                "field_class": classes.get(nid).map_or(Value::Null, FieldClass::as_json),
            }));
        }
        Value::Array(rows)
    }

    #[must_use]
    pub fn parameter_table(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        let Some(params) = self.doc.get("parameters").and_then(Value::as_object) else { return rows };
        for (name, e) in params {
            let mut row = Map::new();
            row.insert("name".into(), Value::from(name.clone()));
            row.insert("units".into(), e.get("units").cloned().unwrap_or(Value::Null));
            row.insert("value".into(), self.values.get(name).map_or(Value::Null, |v| json_f64(*v)));
            row.insert("derived".into(), Value::from(e.get("expr").is_some()));
            for k in ["expr", "doc", "min", "max", "free"] {
                if let Some(v) = e.get(k) {
                    row.insert(k.into(), v.clone());
                }
            }
            let mut binds: Vec<String> = Vec::new();
            for (nid, b) in &self.bindings {
                for (p, bind) in b {
                    let hit = match bind {
                        Binding::Bind { name: n, .. } => n == name,
                        Binding::Expr { tree, .. } => expr_names(tree).contains(name),
                        Binding::Array { .. } => false,
                    };
                    if hit {
                        binds.push(format!("{nid}:{p}"));
                    }
                }
            }
            binds.sort();
            row.insert("binds".into(), serde_json::json!(binds));
            rows.push(Value::Object(row));
        }
        rows
    }


    pub fn provenance_block(&mut self) -> GResult<Value> {
        let d = self.to_doc()?;
        let canon = canonical_bytes(&d);
        let shared = self.shared();
        let params: Map<String, Value> = self
            .parameter_table()
            .into_iter()
            .map(|r| {
                (
                    py_str(&r["name"]),
                    serde_json::json!({"value": r["value"].clone(), "units": r["units"].clone(), "derived": r["derived"].clone()}),
                )
            })
            .collect();
        let arrays: Map<String, Value> = d
            .get("arrays")
            .and_then(Value::as_object)
            .map(|a| {
                a.iter()
                    .map(|(k, v)| {
                        let stored = if v.get("b64").is_some() { Value::from("inline") } else { v.get("file").cloned().unwrap_or(Value::Null) };
                        (k.clone(), serde_json::json!({"dtype": v["dtype"].clone(), "shape": v["shape"].clone(), "sha256": v["sha256"].clone(), "stored": stored}))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let kinds: BTreeSet<String> = self.nodes.values().map(|n| n.kind().to_string()).collect();
        let root = d.get("root").and_then(Value::as_str).map(str::to_string);
        let fc =
            root.and_then(|r| self.field_classes().get(&r).map(FieldClass::as_json)).unwrap_or(Value::Null);
        Ok(serde_json::json!({
            "schema": d["schema"].clone(), "name": d["name"].clone(), "document": d,
            "canonical_sha256": hex::encode(Sha256::digest(&canon)),
            "canonical_bytes": canon.len(),
            "structure_id": self.structure_id(), "content_id": self.content_id(),
            "nodes": self.nodes.len(), "shared_nodes": shared.len(), "shared": shared,
            "kinds": kinds, "parameters": params, "arrays": arrays, "field_class": fc,
            "warnings": self.warnings,
        }))
    }
}

fn err_class(e: &GeometryError) -> &'static str {
    match e {
        GeometryError::Model(_) => "ModelError",
        GeometryError::BoundViolation(_) => "BoundViolation",
        _ => "ValueError",
    }
}

#[must_use]
pub fn fresh_key(arrays: &Map<String, Value>, hint: &str) -> String {
    let mut base: String = hint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') { c } else { '_' })
        .collect();
    if base.is_empty() {
        base = "array".into();
    }
    if !base.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        base = format!("a{base}");
    }
    let mut key: String = base.chars().take(56).collect();
    let mut i = 1;
    while arrays.contains_key(&key) {
        key = format!("{}_{}", base.chars().take(52).collect::<String>(), i);
        i += 1;
    }
    key
}

#[must_use]
pub fn small_array_json(a: &NdArray) -> Option<Value> {
    if a.size() == 0 || a.size() > 16 {
        return None;
    }
    matches!(a.dtype(), DType::F64 | DType::I64).then(|| a.to_json_list())
}

fn param_to_json(
    v: Option<&ParamValue>,
    arrays: &mut Map<String, Value>,
    stored: &Value,
    nid: &str,
    p: &str,
) -> GResult<Value> {
    let Some(v) = v else {
        return mdoc(format!("nodes.{nid}.params.{p} is None and cannot be written"));
    };
    match v {
        ParamValue::Array(a) => {
            if a.ndim() == 0 {
                return Ok(match a.dtype() {
                    DType::F64 | DType::F32 => json_f64(a.to_f64_vec()[0]),
                    DType::Bool => Value::from(a.to_f64_vec()[0] != 0.0),
                    #[allow(clippy::cast_possible_truncation)]
                    _ => Value::from(a.to_f64_vec()[0] as i64),
                });
            }
            if let Some(small) = small_array_json(a) {
                return Ok(small);
            }
            let (entry, raw) = encode_array(a);
            let was = stored.get("params").and_then(|x| x.get(p)).and_then(|x| x.get("array")).map(py_str);
            if let Some(k) = &was
                && arrays.get(k).and_then(|e| e.get("sha256")) == entry.get("sha256")
            {
                return Ok(serde_json::json!({"array": k}));
            }
            let key = was.unwrap_or_else(|| fresh_key(arrays, &format!("{nid}_{p}")));
            let old_file = arrays.get(&key).and_then(|e| e.get("file")).map(py_str);
            let e = match old_file {
                Some(f) => sidecar_entry(&entry, &f),
                None => inline_entry(&entry, &raw),
            };
            arrays.insert(key.clone(), e);
            Ok(serde_json::json!({"array": key}))
        }
        ParamValue::Float(f) if !f.is_finite() => {
            mdoc(format!("nodes.{nid}.params.{p} holds a float, which a model document cannot carry"))
        }
        other => Ok(other.to_json()),
    }
}


pub fn encode_attr(
    v: &Attr,
    collect: &mut dyn FnMut(&NdArray, &str) -> String,
    key_hint: &str,
) -> GResult<Value> {
    Ok(match v {
        Attr::Ref(r) => serde_json::json!({"$ref": r.as_str()}),
        Attr::FieldClass(fc) => serde_json::json!({"$fieldclass": fc.as_json()}),
        Attr::Dict(m) => {
            let mut out = Map::new();
            for (k, x) in m {
                out.insert(k.clone(), encode_attr(x, collect, &format!("{key_hint}.{k}"))?);
            }
            Value::Object(out)
        }
        Attr::List(items) => Value::Array(
            items.iter().map(|x| encode_attr(x, collect, key_hint)).collect::<GResult<Vec<_>>>()?,
        ),
        Attr::Null => Value::Null,
        Attr::Bool(b) => Value::from(*b),
        Attr::Str(s) => Value::from(s.clone()),
        Attr::Int(i) => Value::from(*i),
        Attr::Float(f) if f.is_finite() => json_f64(*f),
        Attr::Float(_) => {
            return mdoc(format!(
                "attribute {} is a float, which a model document cannot carry",
                str_repr(key_hint)
            ));
        }
        Attr::Array(a) => serde_json::json!({"$array": collect(a, key_hint)}),
    })
}

fn attrs_of(node: &Node, arrays: &mut Map<String, Value>) -> GResult<Map<String, Value>> {
    let mut collect = |a: &NdArray, hint: &str| -> String {
        let (entry, raw) = encode_array(a);
        for (k, v) in arrays.iter() {
            if v.get("sha256") == entry.get("sha256") {
                return k.clone();
            }
        }
        let key = fresh_key(arrays, hint);
        arrays.insert(key.clone(), inline_entry(&entry, &raw));
        key
    };
    let mut attrs = node.op().doc_attrs();
    attrs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Map::new();
    for (k, v) in &attrs {
        out.insert(k.clone(), encode_attr(v, &mut collect, k)?);
    }
    Ok(out)
}

fn sanitise(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') { c } else { '_' })
        .collect();
    if s.is_empty() || !s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        s = format!("n_{s}");
    }
    s.chars().take(64).collect()
}

#[must_use]
pub fn node_ids(root: &NodeRef, extra: &[NodeRef]) -> (HashMap<usize, String>, Vec<String>) {
    let mut ids = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    fn walk(
        node: &NodeRef,
        path: &mut Vec<String>,
        ids: &mut HashMap<usize, String>,
        order: &mut Vec<String>,
    ) {
        if ids.contains_key(&node_id(node)) {
            return;
        }
        let base = sanitise(&if path.is_empty() { "root".to_string() } else { path.join(".") });
        let mut key = base.clone();
        let mut i = 1;
        while order.contains(&key) {
            key = format!("{}_{}", base.chars().take(60).collect::<String>(), i);
            i += 1;
        }
        ids.insert(node_id(node), key.clone());
        order.push(key);
        for (n, c) in node.named_children() {
            path.push(n.clone());
            walk(c, path, ids, order);
            path.pop();
        }
    }
    for r in std::iter::once(root).chain(extra.iter()) {
        walk(r, &mut Vec::new(), &mut ids, &mut order);
    }
    (ids, order)
}

pub enum OutputRef {
    Node(NodeRef),
    Id(String),
}


#[allow(clippy::too_many_arguments)]
pub fn from_graph(
    root: &NodeRef,
    name: &str,
    parameters: Option<Value>,
    outputs: &[(String, OutputRef)],
    bindings: &BTreeMap<String, BTreeMap<String, Value>>,
    doc: Option<&str>,
    meta: Option<Value>,
    inline_max: Option<usize>,
) -> GResult<(Value, BTreeMap<String, Vec<u8>>)> {
    let extra: Vec<NodeRef> = outputs
        .iter()
        .filter_map(|(_, o)| if let OutputRef::Node(n) = o { Some(Arc::clone(n)) } else { None })
        .collect();
    let (ids, order) = node_ids(root, &extra);
    let mut objs: BTreeMap<String, NodeRef> = BTreeMap::new();
    fn collect(n: &NodeRef, ids: &HashMap<usize, String>, objs: &mut BTreeMap<String, NodeRef>) {
        let id = ids[&node_id(n)].clone();
        objs.insert(id, Arc::clone(n));
        for c in n.children() {
            if !objs.contains_key(&ids[&node_id(c)]) {
                collect(c, ids, objs);
            }
        }
    }
    for r in std::iter::once(root).chain(extra.iter()) {
        collect(r, &ids, &mut objs);
    }
    let mut arrays = Map::new();
    let mut sidecars = BTreeMap::new();
    let mut put_array = |a: &NdArray, hint: &str, arrays: &mut Map<String, Value>| -> String {
        let (entry, raw) = encode_array(a);
        for (k, v) in arrays.iter() {
            if v.get("sha256") == entry.get("sha256") {
                return k.clone();
            }
        }
        let key = fresh_key(arrays, hint);
        if inline_max.is_some_and(|m| raw.len() > m) {
            let fname = format!("{key}.npy");
            arrays.insert(key.clone(), sidecar_entry(&entry, &fname));
            sidecars.insert(fname, npy_bytes(a));
        } else {
            arrays.insert(key.clone(), inline_entry(&entry, &raw));
        }
        key
    };
    let mut nodes = Map::new();
    for nid in &order {
        let node = &objs[nid];
        let binds = bindings.get(nid);
        let mut params = Map::new();
        for p in node.info().sorted_param_names() {
            if let Some(b) = binds.and_then(|b| b.get(&p)) {
                params.insert(p.clone(), b.clone());
                continue;
            }
            let v = match node.param(&p) {
                None => {
                    return mdoc(format!(
                        "{nid}.{p} is None on the live node; a document cannot write a parameter that has no value"
                    ));
                }
                Some(ParamValue::Array(a)) => match small_array_json(a) {
                    Some(s) => s,
                    None => serde_json::json!({"array": put_array(a, &format!("{nid}_{p}"), &mut arrays)}),
                },
                Some(other) => other.to_json(),
            };
            params.insert(p.clone(), v);
        }
        let mut entry = Map::new();
        entry.insert("kind".into(), Value::from(node.kind()));
        entry.insert("params".into(), Value::Object(params));
        let units: Map<String, Value> = node
            .info()
            .sorted_param_names()
            .into_iter()
            .map(|p| {
                (p.clone(), Value::from(node.info().param(&p).map(|s| s.units.clone()).unwrap_or_default()))
            })
            .collect();
        entry.insert("units".into(), Value::Object(units));
        entry.insert(
            "children".into(),
            Value::Array(
                node.named_children()
                    .map(|(n, c)| serde_json::json!({"name": n, "node": ids[&node_id(c)]}))
                    .collect(),
            ),
        );
        let attrs = node.op().doc_attrs();
        if !attrs.is_empty() {
            let mut a = Map::new();
            for (k, v) in &attrs {
                let mut col = |arr: &NdArray, hint: &str| put_array(arr, hint, &mut arrays);
                a.insert(k.clone(), encode_attr(v, &mut col, k)?);
            }
            entry.insert("attrs".into(), Value::Object(a));
        }
        nodes.insert(nid.clone(), Value::Object(entry));
    }
    let mut out = Map::new();
    out.insert("schema".into(), Value::from(SCHEMA));
    out.insert("name".into(), Value::from(name));
    out.insert("units".into(), Value::from(DOC_UNITS));
    out.insert("nodes".into(), Value::Object(nodes));
    out.insert("root".into(), Value::from(ids[&node_id(root)].clone()));
    if let Some(p) = parameters.filter(|p| super::truthy(Some(p))) {
        out.insert("parameters".into(), p);
    }
    if !outputs.is_empty() {
        let o: Map<String, Value> = outputs
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Value::from(match v {
                        OutputRef::Node(n) => ids[&node_id(n)].clone(),
                        OutputRef::Id(s) => s.clone(),
                    }),
                )
            })
            .collect();
        out.insert("outputs".into(), Value::Object(o));
    }
    if !arrays.is_empty() {
        out.insert("arrays".into(), Value::Object(arrays));
    }
    if let Some(m) = meta.filter(|m| super::truthy(Some(m))) {
        out.insert("meta".into(), m);
    }
    if let Some(d) = doc.filter(|d| !d.is_empty()) {
        out.insert("doc".into(), Value::from(d));
    }
    Ok((Value::Object(out), sidecars))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> GResult<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, bytes).map_err(|e| GeometryError::Io(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| GeometryError::Io(format!("{}: {e}", path.display())))
}


pub fn write(
    path: &Path,
    doc: &Value,
    sidecars: &BTreeMap<String, Vec<u8>>,
    base_dir: Option<&Path>,
) -> GResult<(Value, Vec<String>)> {
    let abs = std::path::absolute(path).map_err(|e| GeometryError::Io(e.to_string()))?;
    let d = abs.parent().map(Path::to_path_buf).unwrap_or_default();
    for (fname, blob) in sidecars {
        atomic_write(&d.join(fname), blob)?;
    }
    let (norm, warnings) = validate(doc, Some(base_dir.unwrap_or(&d)), None)?;
    atomic_write(path, dumps(&norm).as_bytes())?;
    Ok((norm, warnings))
}


pub fn read(path: &Path, reg: Option<&Registry>) -> GResult<Model> {
    let text =
        std::fs::read_to_string(path).map_err(|e| GeometryError::Io(format!("{}: {e}", path.display())))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| GeometryError::Value(e.to_string()))?;
    let abs = std::path::absolute(path).map_err(|e| GeometryError::Io(e.to_string()))?;
    build(&doc, abs.parent(), reg)
}


pub fn check_roundtrip(doc: &Value, base_dir: Option<&Path>, reg: Option<&Registry>) -> GResult<Value> {
    let m1 = build(doc, base_dir, reg)?;
    let t1 = m1.dumps()?;
    let d2: Value = serde_json::from_str(&t1).map_err(|e| GeometryError::Value(e.to_string()))?;
    let m2 = build(&d2, base_dir, reg)?;
    let t2 = m2.dumps()?;
    let first = first_difference(&t1, &t2);
    Ok(serde_json::json!({
        "bytes_equal": t1 == t2, "bytes": t1.len(),
        "content_id": m1.content_id(), "content_id_2": m2.content_id(),
        "content_id_equal": m1.content_id() == m2.content_id(),
        "structure_id": m1.structure_id(),
        "structure_id_equal": m1.structure_id() == m2.structure_id(),
        "sha256": m1.sha256()?, "sha256_equal": m1.sha256()? == m2.sha256()?,
        "nodes": m1.nodes.len(), "shared": m1.shared().len(),
        "first_difference": first,
    }))
}

fn first_difference(a: &str, b: &str) -> Value {
    if a == b {
        return Value::Null;
    }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let slice =
        |v: &[char], lo: usize, hi: usize| -> String { v[lo.min(v.len())..hi.min(v.len())].iter().collect() };
    for (i, (x, y)) in ac.iter().zip(bc.iter()).enumerate() {
        if x != y {
            let lo = i.saturating_sub(40);
            return serde_json::json!({"at": i, "a": slice(&ac, lo, i + 40), "b": slice(&bc, lo, i + 40)});
        }
    }
    let n = ac.len().min(bc.len());
    serde_json::json!({"at": n, "a": slice(&ac, ac.len().saturating_sub(80), ac.len()), "b": slice(&bc, bc.len().saturating_sub(80), bc.len())})
}

fn default_json(v: Option<&ParamValue>) -> Value {
    match v {
        None => Value::Null,
        Some(ParamValue::Array(a)) => serde_json::json!({"shape": a.shape(), "dtype": a.dtype().name()}),
        Some(other) => other.to_json(),
    }
}

#[must_use]
pub fn catalogue(reg: Option<&Registry>) -> Value {
    let owned;
    let reg = if let Some(r) = reg {
        r
    } else {
        owned = crate::node::registry();
        &owned
    };
    let mut out = Vec::new();
    for (kind, entry) in reg.iter() {
        let info = &entry.info;
        let mut params: Vec<&crate::node::ParamSpec> = info.params.iter().collect();
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let prows: Vec<Value> = params
            .iter()
            .map(|p| {
                let (dim, _) = unit_info(&p.units);
                serde_json::json!({"name": p.name, "units": p.units, "dimension": dim, "default": default_json(p.default.as_ref()),
                    "doc": p.doc, "is_length": dim == "length"})
            })
            .collect();
        out.push(serde_json::json!({
            "kind": kind, "module": info.module, "doc": info.doc, "params": prows,
            "field_class": {"doc": info.field_class_doc, "leaf": info.leaf_class.as_ref().map_or(Value::Null, FieldClass::as_json)},
        }));
    }
    Value::Array(out)
}
