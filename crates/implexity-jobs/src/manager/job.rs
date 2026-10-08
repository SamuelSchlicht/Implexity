// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use implexity_core::pyobj::truthy;
use implexity_geometry::document::{Binding, Model};
use implexity_geometry::{NodeRef, ParamRef};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::error::{JobError, JobResult};
use crate::managed_evaluation::ManagedEvaluationControl;
use crate::private::epoch_seconds;

pub const TERMINAL: [&str; 9] = [
    "stopped",
    "completed",
    "error",
    "superseded",
    "discarded",
    "accepted",
    "intervening",
    "branched",
    "attention",
];

#[must_use]
pub fn is_terminal(status: &str) -> bool {
    TERMINAL.contains(&status)
}

#[derive(Debug, Default)]
pub struct Event {
    flag: Mutex<bool>,
    cond: Condvar,
}

impl Event {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set(&self) {
        let mut flag = self.flag.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *flag = true;
        self.cond.notify_all();
    }

    pub fn clear(&self) {
        *self.flag.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        *self.flag.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn wait(&self, timeout_s: Option<f64>) -> bool {
        let mut flag = self.flag.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = timeout_s
            .and_then(|t| std::time::Instant::now().checked_add(crate::worker_cli::duration_from_secs(t)));
        while !*flag {
            match deadline {
                None => flag = self.cond.wait(flag).unwrap_or_else(std::sync::PoisonError::into_inner),
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        return false;
                    }
                    flag = self
                        .cond
                        .wait_timeout(flag, d - now)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            }
        }
        true
    }
}

#[derive(Debug, Default)]
pub struct Gate {
    held: Mutex<bool>,
    cond: Condvar,
}

impl Gate {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn acquire(&self, blocking: bool) -> bool {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !blocking && *held {
            return false;
        }
        while *held {
            held = self.cond.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *held = true;
        true
    }

    pub fn release(&self) {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = false;
        self.cond.notify_one();
    }

    #[must_use]
    pub fn hold(self: &Arc<Self>) -> GateGuard {
        self.acquire(true);
        GateGuard(Arc::clone(self))
    }

    #[must_use]
    pub fn try_hold(self: &Arc<Self>) -> Option<GateGuard> {
        self.acquire(false).then(|| GateGuard(Arc::clone(self)))
    }
}

#[derive(Debug)]
pub struct GateGuard(Arc<Gate>);

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[must_use]
pub fn num(value: Option<&Value>) -> Value {
    let Some(v) = value else { return Value::Null };
    match crate::provider_worker::json_array(v) {
        Some(a) if a.ndim() == 0 => {
            implexity_optim::numeric::float_value(a.iter().next().copied().unwrap_or(f64::NAN))
        }
        Some(a) => Value::Array(a.iter().map(|x| implexity_optim::numeric::float_value(*x)).collect()),
        None => Value::Null,
    }
}

#[must_use]
pub fn num_array(value: &ArrayD<f64>) -> Value {
    if value.ndim() == 0 {
        implexity_optim::numeric::float_value(value.iter().next().copied().unwrap_or(f64::NAN))
    } else {
        Value::Array(value.iter().map(|x| implexity_optim::numeric::float_value(*x)).collect())
    }
}

#[must_use]
pub fn free_entry(reference: &str, lo: Option<f64>, hi: Option<f64>, scale: Option<f64>) -> Value {
    let mut e = Map::new();
    e.insert("ref".into(), Value::String(reference.into()));
    for (k, v) in [("lo", lo), ("hi", hi), ("scale", scale)] {
        if let Some(v) = v {
            e.insert(k.into(), implexity_optim::numeric::float_value(v));
        }
    }
    Value::Object(e)
}

fn repr(v: &Value) -> String {
    implexity_core::pyobj::repr(v)
}

fn param_value_array(node: &NodeRef, name: &str) -> Option<ArrayD<f64>> {
    node.param(name).and_then(crate::manager::param_to_array)
}

fn float_of(v: Option<&Value>) -> Result<Option<f64>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            n.as_f64().map(Some).ok_or_else(|| "could not convert to float".to_string())
        }
        Some(Value::Bool(b)) => Ok(Some(f64::from(u8::from(*b)))),
        Some(Value::String(s)) => s.trim().parse::<f64>().map(Some).map_err(|_| {
            format!("could not convert string to float: {}", implexity_core::py_repr::repr_str(s))
        }),
        Some(other) => Err(format!(
            "float() argument must be a string or a real number, not {}",
            implexity_core::pyobj::type_name(other)
        )),
    }
}

pub type FreePlan = (Vec<Value>, Vec<Value>, Vec<String>);

#[allow(clippy::too_many_lines)]
#[must_use]
pub fn plan_free(m: &Model, child: &NodeRef, entries: &[Value]) -> FreePlan {
    let mut paths: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for (path, n) in child.walk() {
        if let Some(nid) = m.id_of(&n) {
            let mut p = vec!["model".to_string()];
            p.extend(path);
            paths.entry(nid).or_default().push(p);
        }
    }
    let table = m.doc.get("parameters").and_then(Value::as_object).cloned().unwrap_or_default();
    let (mut free, mut plan, mut problems) = (Vec::new(), Vec::new(), Vec::new());
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let parents_of = |nid: &str| -> Vec<String> {
        let mut p = m.parents().get(nid).cloned().unwrap_or_default();
        p.sort();
        p
    };
    for raw in entries {
        let ent: Map<String, Value> = match raw {
            Value::Object(o) => o.clone(),
            other => {
                let mut o = Map::new();
                o.insert("ref".into(), other.clone());
                o
            }
        };
        if !matches!(ent.get("ref"), None | Some(Value::String(_) | Value::Null)) {
            problems.push(format!("free entry {}: 'ref' must be 'a/b:name'", repr(raw)));
            continue;
        }
        let name = ent
            .get("parameter")
            .filter(|v| truthy(v))
            .or_else(|| ent.get("name").filter(|v| truthy(v)))
            .map(implexity_core::pyobj::py_str);
        let lo_raw = ent.get("lo").or_else(|| ent.get("min"));
        let hi_raw = ent.get("hi").or_else(|| ent.get("max"));
        let scale_raw = ent.get("scale");
        let ref_raw = ent.get("ref").filter(|v| truthy(v));
        if let (Some(n), Some(r)) = (&name, ref_raw) {
            problems.push(format!(
                "free entry {} names BOTH a document parameter ({}) and a node reference ({}); it is one or the other",
                repr(raw),
                implexity_core::py_repr::repr_str(n),
                repr(r)
            ));
            continue;
        }
        let Some(name) = name else {
            let Some(reference) = ref_raw.and_then(Value::as_str) else {
                problems.push(format!(
                    "free entry {} has neither 'ref' (a node parameter, 'model/<path>:<name>') nor 'parameter' (a named parameter of the document)",
                    repr(raw)
                ));
                continue;
            };
            let pr = match ParamRef::parse(reference) {
                Ok(p) => p,
                Err(e) => {
                    problems.push(format!("free {reference}: {e}"));
                    continue;
                }
            };
            if pr.path.first().map(String::as_str) != Some("model") {
                let mut p = vec!["model".to_string()];
                p.extend(pr.path.iter().cloned());
                problems.push(format!(
                    "free {reference}: a node reference is rooted at the OPTIMIZE node, so the model is reached through 'model/' -- write it as {}",
                    ParamRef::new(p, pr.name.clone()).as_str()
                ));
                continue;
            }
            let node = match child.at(&pr.path[1..]) {
                Ok(n) => n,
                Err(e) => {
                    problems.push(format!("free {reference} does not resolve: {e}"));
                    continue;
                }
            };
            let nid = m.id_of(&node);
            if node.info().param(&pr.name).is_none() {
                let names = node.info().sorted_param_names();
                problems.push(format!(
                    "free {reference}: {} has no parameter {}; it has {}",
                    node.kind(),
                    implexity_core::py_repr::repr_str(&pr.name),
                    if names.is_empty() { "none".to_string() } else { names.join(", ") }
                ));
                continue;
            }
            let bound =
                nid.as_ref().and_then(|id| m.bindings().get(id)).and_then(|b| b.get(&pr.name)).cloned();
            if let Some(b @ (Binding::Bind { .. } | Binding::Expr { .. })) = &bound {
                let (what, fix) = match b {
                    Binding::Bind { name, .. } => (
                        implexity_core::py_repr::repr_str(name),
                        format!("the named parameter {}", implexity_core::py_repr::repr_str(name)),
                    ),
                    _ => ("an expression".to_string(), "what the expression reads".to_string()),
                };
                problems.push(format!(
                    "free {reference} names a node parameter that is BOUND to {what}: the optimiser would move it and the next parameter edit would re-resolve the binding and silently undo it.  Mark {fix} free instead -- it is what drives it."
                ));
                continue;
            }
            let nid_s = nid.clone().unwrap_or_default();
            if paths.get(&nid_s).is_some_and(|p| p.len() > 1) {
                let parents = parents_of(&nid_s);
                problems.push(format!(
                    "free {reference} reaches node {}, which is SHARED ({} parents: {}).  A free reference is a PATH, so the optimiser would move this path's copy only, while writing the result back into the document moves every parent at once -- the document has one node, not two.  Optimise a copy, or make the shared value a named parameter.",
                    implexity_core::py_repr::repr_str(&nid_s),
                    parents.len(),
                    parents.join(", ")
                ));
                continue;
            }
            let key = pr.as_str();
            if !seen.insert(key.clone()) {
                problems.push(format!("free {key} appears twice"));
                continue;
            }
            let units = node.info().param(&pr.name).map(|p| p.units.clone()).unwrap_or_default();
            let (spatial, array_key) = match &bound {
                Some(Binding::Array { key, .. }) => (true, Some(key.clone())),
                _ => (false, None),
            };
            let array_file = array_key.as_ref().and_then(|k| {
                m.doc.get("arrays").and_then(|a| a.get(k)).and_then(|e| e.get("file")).cloned()
            });
            let (lo, hi, scale) = match (float_of(lo_raw), float_of(hi_raw), float_of(scale_raw)) {
                (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    problems.push(format!("free {key}: {e}"));
                    continue;
                }
            };
            free.push(free_entry(&key, lo, hi, scale));
            let start = param_value_array(&node, &pr.name).map_or(Value::Null, |a| num_array(&a));
            plan.push(json!({
                "kind": if spatial { "spatial_array" } else { "node_param" },
                "ref": key, "node": nid.map_or(Value::Null, Value::String),
                "param": pr.name, "units": units, "node_units": units,
                "parameter": Value::Null,
                "lo": lo_raw.cloned().unwrap_or(Value::Null), "hi": hi_raw.cloned().unwrap_or(Value::Null),
                "start": start, "start_document": Value::Null,
                "array_key": array_key.map_or(Value::Null, Value::String),
                "array_file": array_file.unwrap_or(Value::Null),
            }));
            continue;
        };
        let Some(entry) = table.get(&name).and_then(Value::as_object) else {
            let mut names: Vec<&String> = table.keys().collect();
            names.sort();
            problems.push(format!(
                "free parameter {}: this model declares no such parameter; it declares {}",
                implexity_core::py_repr::repr_str(&name),
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                }
            ));
            continue;
        };
        if let Some(expr) = entry.get("expr") {
            problems.push(format!(
                "free parameter {} is DERIVED ({}): its value is computed from the parameters it reads, so an optimiser could not write a value back into it.  Mark what it reads free.",
                implexity_core::py_repr::repr_str(&name),
                implexity_core::pyobj::py_str(expr)
            ));
            continue;
        }
        let consumers = match m.parameter_consumers(child, &name) { Ok(v) => v, Err(e) => { problems.push(e.to_string()); continue; } };
        if consumers.is_empty() { problems.push(format!("parameter {name} has no continuous consumers beneath this node")); continue; }
        let complex = consumers.len() != 1 || consumers.iter().any(|(r,_)| { let n = child.at(&r.path).ok(); let id = n.as_ref().and_then(|n| m.id_of(n)); id.and_then(|id| m.bindings().get(&id)).and_then(|b| b.get(&r.name)).is_some_and(|b| !matches!(b, Binding::Bind { name: bound, .. } if *bound == name)) });
        if complex {
            let units = entry.get("units").map_or_else(|| "-".into(), implexity_core::pyobj::py_str);
            if ent.get("units").filter(|v| !v.is_null()).is_some_and(|v| implexity_core::pyobj::py_str(v) != units) { problems.push(format!("parameter {name} units differ from the document")); continue; }
            let (lo, hi, scale) = match (float_of(lo_raw.or_else(|| entry.get("min"))), float_of(hi_raw.or_else(|| entry.get("max"))), float_of(scale_raw)) {
                (Ok(lo), Ok(hi), Ok(scale)) => (lo,hi,scale),
                (Err(e),_,_) | (_,Err(e),_) | (_,_,Err(e)) => { problems.push(format!("parameter {name}: {e}")); continue; }
            };
            let start = m.values().get(&name).copied().unwrap_or(f64::NAN);
            let key = format!("parameter:{name}");
            if !seen.insert(key.clone()) { problems.push(format!("parameter {name} is already free")); continue; }
            let representative = &consumers[0].0;
            let mut path = vec!["model".into()]; path.extend(representative.path.clone());
            let reference = ParamRef::new(path, representative.name.clone()).as_str();
            let mut coordinate = free_entry(&reference, lo, hi, scale).as_object().cloned().unwrap_or_default();
            coordinate.insert("parameter".into(), json!(name)); coordinate.insert("units".into(), json!(units)); coordinate.insert("start".into(), json!(start));
            free.push(Value::Object(coordinate));
            plan.push(json!({"kind":"parameter","ref":key,"parameter":name,"node":Value::Null,"param":representative.name,"units":units,"node_units":units,"lo":lo,"hi":hi,"doc_min":entry.get("min"),"doc_max":entry.get("max"),"start":start,"start_document":start}));
            continue;
        }
        let mut direct: Vec<(String, String)> = Vec::new();
        let mut via_expr: Vec<(String, String)> = Vec::new();
        for (nid, binds) in m.bindings() {
            for (p, b) in binds {
                match b {
                    Binding::Bind { name: bound, .. } if *bound == name => {
                        direct.push((nid.clone(), p.clone()));
                    }
                    Binding::Expr { tree, .. }
                        if implexity_geometry::document::expr::expr_names(tree).contains(&name) =>
                    {
                        via_expr.push((nid.clone(), p.clone()));
                    }
                    _ => {}
                }
            }
        }
        let mut drives: Vec<String> =
            direct.iter().chain(&via_expr).map(|(a, b)| format!("{a}:{b}")).collect();
        drives.sort();
        if drives.is_empty() {
            problems.push(format!(
                "free parameter {} drives nothing in this graph: no node parameter binds to it, so moving it would move no geometry and every gradient would be exactly zero",
                implexity_core::py_repr::repr_str(&name)
            ));
            continue;
        }
        if direct.len() != 1 || !via_expr.is_empty() {
            let refs: Vec<String> = direct
                .iter()
                .filter(|(a, _)| paths.get(a).is_some_and(|p| p.len() == 1))
                .map(|(a, b)| ParamRef::new(paths[a][0].clone(), b.clone()).as_str())
                .collect();
            problems.push(format!(
                "free parameter {} drives {} node parameter(s) ({}){}.  A design variable here is ONE node parameter -- the free set is a list of parameter references and each one moves independently -- so a named parameter that moves several at once cannot be expressed as one.  Name them individually with 'ref' ({}), which makes them INDEPENDENT, or give each its own named parameter.",
                implexity_core::py_repr::repr_str(&name),
                drives.len(),
                drives.join(", "),
                if via_expr.is_empty() {
                    ""
                } else {
                    " -- and one of them through an EXPRESSION, which has no inverse to write a value back through"
                },
                if refs.is_empty() { "none of them is beneath the node being optimised".to_string() } else { refs.join(", ") }
            ));
            continue;
        }
        let (nid, pname) = direct[0].clone();
        let Some(node_paths) = paths.get(&nid) else {
            problems.push(format!(
                "free parameter {} drives {nid}:{pname}, which is not beneath the node being optimised: nothing under it reads that parameter",
                implexity_core::py_repr::repr_str(&name)
            ));
            continue;
        };
        if node_paths.len() > 1 {
            let parents = parents_of(&nid);
            problems.push(format!(
                "free parameter {} drives {nid}:{pname}, and node {} is SHARED ({} parents: {}).  A free reference is a PATH, so the optimiser would move one copy of a node the document holds once.  Optimise a copy of it, or mark the parents' own parameters free.",
                implexity_core::py_repr::repr_str(&name),
                implexity_core::py_repr::repr_str(&nid),
                parents.len(),
                parents.join(", ")
            ));
            continue;
        }
        let Ok(node) = m.node(&nid) else { continue };
        let node_units = node.info().param(&pname).map(|p| p.units.clone()).unwrap_or_default();
        let p_units = entry.get("units").map_or_else(|| "-".to_string(), implexity_core::pyobj::py_str);
        if let Some(want) = ent.get("units").filter(|v| !v.is_null()) {
            let want = implexity_core::pyobj::py_str(want);
            if want != p_units {
                problems.push(format!(
                    "free parameter {}: the request declares it in {} and the document declares it in {} -- a unit is not a matter of opinion, and converting silently between two declarations is how a millimetre becomes a metre",
                    implexity_core::py_repr::repr_str(&name),
                    implexity_core::py_repr::repr_str(&want),
                    implexity_core::py_repr::repr_str(&p_units)
                ));
                continue;
            }
        }
        let convert = |v: Option<&Value>| -> Result<Option<f64>, String> {
            match float_of(v)? {
                None => Ok(None),
                Some(x) => implexity_geometry::document::convert(x, &p_units, &node_units)
                    .map(Some)
                    .map_err(|e| e.to_string()),
            }
        };
        let (mut lo_n, mut hi_n, sc_n) = match (convert(lo_raw), convert(hi_raw), convert(scale_raw)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                problems.push(format!("free parameter {}: {e}", implexity_core::py_repr::repr_str(&name)));
                continue;
            }
        };
        if lo_n.is_none()
            && let Some(min) = entry.get("min")
        {
            lo_n = convert(Some(min)).ok().flatten();
        }
        if hi_n.is_none()
            && let Some(max) = entry.get("max")
        {
            hi_n = convert(Some(max)).ok().flatten();
        }
        let reference = ParamRef::new(node_paths[0].clone(), pname.clone()).as_str();
        if !seen.insert(reference.clone()) {
            problems.push(format!(
                "free parameter {} resolves to {reference}, which is already free",
                implexity_core::py_repr::repr_str(&name)
            ));
            continue;
        }
        free.push(free_entry(&reference, lo_n, hi_n, sc_n));
        let start_doc = m.values().get(&name).copied();
        let start = start_doc.map_or(Value::Null, |v| {
            implexity_geometry::document::convert(v, &p_units, &node_units)
                .map_or(Value::Null, implexity_optim::numeric::float_value)
        });
        plan.push(json!({
            "kind": "parameter", "ref": reference, "parameter": name,
            "node": nid, "param": pname, "units": p_units, "node_units": node_units,
            "lo": lo_n.map_or(Value::Null, implexity_optim::numeric::float_value),
            "hi": hi_n.map_or(Value::Null, implexity_optim::numeric::float_value),
            "doc_min": entry.get("min").cloned().unwrap_or(Value::Null),
            "doc_max": entry.get("max").cloned().unwrap_or(Value::Null),
            "start": start,
            "start_document": start_doc.map_or(Value::Null, implexity_optim::numeric::float_value),
        }));
    }
    (free, plan, problems)
}

#[derive(Clone)]
pub struct DerivedHook(
    pub Arc<dyn Fn(&BTreeMap<String, ArrayD<f64>>) -> JobResult<BTreeMap<String, ArrayD<f64>>> + Send + Sync>,
);

impl std::fmt::Debug for DerivedHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<derive_model_updates>")
    }
}

#[derive(Clone, Default)]
pub struct JobMeta {
    pub fields: Map<String, Value>,
    pub before_values: BTreeMap<String, ArrayD<f64>>,
    pub coordinate_initial: BTreeMap<String, ArrayD<f64>>,
    pub topology_initial: Option<ArrayD<f64>>,
    pub provider_derived_design_base: BTreeMap<String, ArrayD<f64>>,
    pub provider_derived_before_values: BTreeMap<String, ArrayD<f64>>,
    pub provider_derived_hook: Option<DerivedHook>,
    pub spec: Option<Arc<crate::optimize::OptimizeSpec>>,
}

impl std::fmt::Debug for JobMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobMeta").field("fields", &self.fields).finish_non_exhaustive()
    }
}

impl JobMeta {
    #[must_use]
    pub fn get(&self, key: &str) -> &Value {
        self.fields.get(key).unwrap_or(&Value::Null)
    }

    #[must_use]
    pub fn s(&self, key: &str) -> String {
        match self.get(key) {
            Value::Null => String::new(),
            v => implexity_core::pyobj::py_str(v),
        }
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct ModelOptJob {
    pub id: String,
    pub channel: Value,
    pub seq: i64,
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub rows: Vec<Value>,
    pub previews: Vec<Value>,
    pub summary: Option<Map<String, Value>>,
    pub error: Option<Value>,
    pub stderr_tail: Option<String>,
    pub t_submit: f64,
    pub t_start: Option<f64>,
    pub t_end: Option<f64>,
    pub job_dir: Option<String>,
    pub spec_file: Option<String>,
    pub settings: Map<String, Value>,
    pub grid: Value,
    pub iters: i64,
    pub lr: f64,
    pub live_every: i64,
    pub steerable: bool,
    pub momentum: Value,
    pub node: String,
    pub model_kind: String,
    pub case_name: Value,
    pub case_norm: Value,
    pub free: Vec<Value>,
    pub plan: Vec<Value>,
    pub drive: Value,
    pub spec: Option<Arc<crate::optimize::OptimizeSpec>>,
    pub solve_id: String,
    pub objective_terms: Value,
    pub objective_block: Value,
    pub case_source: String,
    pub physics_provider: String,
    pub provider_execution: String,
    pub provider_problem: Value,
    pub provider_responses: Vec<Value>,
    pub design_coordinate_selection: Value,
    pub provider_derived_plan: Vec<Value>,
    pub provider_derived_design_base: BTreeMap<String, ArrayD<f64>>,
    pub provider_derived_before_values: BTreeMap<String, ArrayD<f64>>,
    pub provider_derived_hook: Option<DerivedHook>,
    pub computation_effort: Option<Value>,
    pub managed_control: Option<ManagedEvaluationControl>,
    pub managed_operation_id: Option<String>,
    pub managed_active_dir: Option<String>,
    pub managed_requested_control: Option<String>,
    pub managed_budget_wall_s: Option<f64>,
    pub managed_budget_memory_bytes: Option<i64>,
    pub managed_budget_initialized: bool,
    pub managed_consumed_wall_s: f64,
    pub managed_stdout_offset: u64,
    pub managed_stderr_offset: u64,
    pub managed_stdout_pending: Vec<u8>,
    pub managed_supervisor_state: String,
    pub managed_supervisor_reason: Option<String>,
    pub managed_observation: Option<Value>,
    pub managed_terminal_manifest: Option<Value>,
    pub managed_spec_fingerprint: Option<Value>,
    pub managed_peak_owned_rss_bytes: i64,
    pub managed_peak_owned_memory_bytes: i64,
    pub request: Map<String, Value>,
    pub parent_job_id: Option<String>,
    pub intervention: Option<Value>,
    pub intervention_branch_token: Option<String>,
    pub numerical_attention: Option<Value>,
    pub numerical_attention_branch_token: Option<String>,
    pub numerical_progress: Option<Value>,
    pub solver_recovery: Option<Value>,
    pub result_authority: String,
    pub matching_time_guess_consumed: Option<Value>,
    pub warnings: Vec<Value>,
    pub ignored: Map<String, Value>,
    pub declared_by: String,
    pub structure_id: Value,
    pub content_id_start: Value,
    pub model_content_id_owned: Value,
    pub model_rollback_resolved: bool,
    pub before_doc: Value,
    pub before_values: BTreeMap<String, ArrayD<f64>>,
    pub before_sha256: String,
    pub steers: Vec<Map<String, Value>>,
    pub timeline: Vec<(String, f64)>,
    pub regimes: i64,
    pub accepted: Option<Value>,
    pub working_design: Option<Value>,
    pub record: Option<Value>,
    pub apply_stats: Vec<Map<String, Value>>,
    pub apply_skipped: i64,
    pub last_apply_t: Option<f64>,
    pub live: Option<Map<String, Value>>,
    pub managed_terminal_event: Arc<Event>,
}

fn copy_arrays(v: &BTreeMap<String, ArrayD<f64>>) -> BTreeMap<String, ArrayD<f64>> {
    v.clone()
}

impl ModelOptJob {

    #[allow(clippy::too_many_lines)]
    pub fn new(channel: Value, seq: i64, meta: &JobMeta) -> JobResult<Self> {
        let t_submit = epoch_seconds();
        let settings = meta.get("settings").as_object().cloned().unwrap_or_default();
        #[allow(clippy::cast_possible_truncation)]
        let int = |k: &str| settings.get(k).and_then(Value::as_f64).map_or(0, |v| v as i64);
        let result_authority = match meta.get("result_authority") {
            Value::Null => "authoritative".to_string(),
            v => implexity_core::pyobj::py_str(v),
        };
        if result_authority != "authoritative" && result_authority != "exploratory_non_authoritative" {
            return Err(JobError::value("optimization result authority is invalid"));
        }
        let list = |k: &str| meta.get(k).as_array().cloned().unwrap_or_default();
        let content = meta.get("content_id").clone();
        Ok(Self {
            id: crate::private::token_hex(6).map_err(JobError::from)?,
            channel,
            seq,
            status: "queued".into(),
            progress: 0.0,
            message: "queued".into(),
            rows: Vec::new(),
            previews: Vec::new(),
            summary: None,
            error: None,
            stderr_tail: None,
            t_submit,
            t_start: None,
            t_end: None,
            job_dir: None,
            spec_file: None,
            iters: int("iters"),
            lr: settings.get("lr").and_then(Value::as_f64).unwrap_or(0.0),
            live_every: int("live_every"),
            steerable: settings.get("steerable").is_some_and(truthy),
            momentum: settings.get("momentum").cloned().unwrap_or(Value::Null),
            settings,
            grid: meta.get("grid").clone(),
            node: meta.s("node"),
            model_kind: meta.s("model_kind"),
            case_name: meta.get("case_name").clone(),
            case_norm: meta.get("case_norm").clone(),
            free: list("free"),
            plan: list("plan"),
            drive: meta.get("drive").clone(),
            spec: meta.spec.clone(),
            solve_id: meta.s("solve_id"),
            objective_terms: meta.get("objective_terms").clone(),
            objective_block: meta.get("objective_block").clone(),
            case_source: meta.s("case_source"),
            physics_provider: meta.s("physics_provider"),
            provider_execution: match meta.get("provider_execution") {
                v if truthy(v) => implexity_core::pyobj::py_str(v),
                _ => "implicit_job".into(),
            },
            provider_problem: meta.get("provider_problem").clone(),
            provider_responses: list("provider_responses"),
            design_coordinate_selection: match meta.get("design_coordinate_selection") {
                v if truthy(v) => v.clone(),
                _ => Value::Object(Map::new()),
            },
            provider_derived_plan: list("provider_derived_plan"),
            provider_derived_design_base: copy_arrays(&meta.provider_derived_design_base),
            provider_derived_before_values: copy_arrays(&meta.provider_derived_before_values),
            provider_derived_hook: meta.provider_derived_hook.clone(),
            computation_effort: Some(meta.get("computation_effort").clone()).filter(|v| !v.is_null()),
            managed_control: None,
            managed_operation_id: None,
            managed_active_dir: None,
            managed_requested_control: None,
            managed_budget_wall_s: None,
            managed_budget_memory_bytes: None,
            managed_budget_initialized: false,
            managed_consumed_wall_s: 0.0,
            managed_stdout_offset: 0,
            managed_stderr_offset: 0,
            managed_stdout_pending: Vec::new(),
            managed_supervisor_state: "not_started".into(),
            managed_supervisor_reason: None,
            managed_observation: None,
            managed_terminal_manifest: None,
            managed_spec_fingerprint: None,
            managed_peak_owned_rss_bytes: 0,
            managed_peak_owned_memory_bytes: 0,
            request: meta.get("request").as_object().cloned().unwrap_or_default(),
            parent_job_id: Some(meta.s("parent_job_id")).filter(|s| !s.is_empty()),
            intervention: None,
            intervention_branch_token: Some(meta.s("intervention_branch_token")).filter(|s| !s.is_empty()),
            numerical_attention: None,
            numerical_attention_branch_token: Some(meta.s("numerical_attention_branch_token"))
                .filter(|s| !s.is_empty()),
            numerical_progress: None,
            solver_recovery: None,
            result_authority,
            matching_time_guess_consumed: None,
            warnings: list("warnings"),
            ignored: meta.get("ignored").as_object().cloned().unwrap_or_default(),
            declared_by: meta.s("declared_by"),
            structure_id: meta.get("structure_id").clone(),
            content_id_start: content.clone(),
            model_content_id_owned: content,
            model_rollback_resolved: false,
            before_doc: meta.get("before_doc").clone(),
            before_values: meta.before_values.clone(),
            before_sha256: meta.s("before_sha256"),
            steers: Vec::new(),
            timeline: vec![("submitted".into(), t_submit)],
            regimes: 1,
            accepted: None,
            working_design: None,
            record: None,
            apply_stats: Vec::new(),
            apply_skipped: 0,
            last_apply_t: None,
            live: None,
            managed_terminal_event: Event::new(),
        })
    }

    pub fn mark(&mut self, what: &str) {
        self.timeline.push((what.into(), epoch_seconds()));
    }

    #[must_use]
    pub fn record_pointer(&self) -> Value {
        let r = self.record.as_ref();
        json!({
            "schema": implexity_io::provenance::SCHEMA,
            "url": format!("/v1/implicit/optimize/jobs/{}/record", self.id),
            "record_id": r.and_then(|r| r.get("record_id")).cloned().unwrap_or(Value::Null),
            "built": r.is_some_and(truthy),
            "steered": self.steers.iter().any(|e| e.get("applied").is_some_and(truthy)),
            "warnings": r.and_then(|r| r.get("warnings")).filter(|v| truthy(v)).cloned().unwrap_or(json!([])),
        })
    }

    #[must_use]
    pub fn design_variables(&self) -> Vec<Value> {
        self.free
            .iter()
            .map(|f| {
                let mut row = f.as_object().cloned().unwrap_or_default();
                let reference = f.get("ref").cloned().unwrap_or(Value::Null);
                if let Some(p) = self.plan.iter().find(|e| e.get("ref") == Some(&reference)) {
                    for (k, src) in [
                        ("kind", "kind"),
                        ("parameter", "parameter"),
                        ("node", "node"),
                        ("param", "param"),
                        ("start", "start"),
                        ("start_document", "start_document"),
                        ("document_units", "units"),
                    ] {
                        row.insert(k.into(), p.get(src).cloned().unwrap_or(Value::Null));
                    }
                }
                let live = reference
                    .as_str()
                    .and_then(|r| self.live.as_ref().and_then(|l| l.get(r)))
                    .cloned()
                    .unwrap_or(Value::Null);
                row.insert("value".into(), live);
                Value::Object(row)
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn as_dict(&self, with_rows: bool) -> Value {
        let round = implexity_mesh::numeric::py_round_digits;
        let fv = implexity_optim::numeric::float_value;
        let mut d = Map::new();
        let id = &self.id;
        let steered = self.steers.iter().any(|e| e.get("applied").is_some_and(truthy));
        for (k, v) in [
            ("kind", json!("implicit_optimize")),
            ("job_id", json!(id)),
            ("channel", self.channel.clone()),
            ("seq", json!(self.seq)),
            ("status", json!(self.status)),
            ("progress", fv(round(self.progress, 3))),
            ("message", json!(self.message)),
            ("node", json!(self.node)),
            ("model_kind", json!(self.model_kind)),
            ("declared_by", json!(self.declared_by)),
            ("case", self.case_name.clone()),
            ("grid", self.grid.clone()),
            ("iters", json!(self.iters)),
            ("total_iters", json!(self.iters)),
            ("stages", json!(1)),
            ("lr", fv(self.lr)),
            ("live_every", json!(self.live_every)),
            ("design", json!("the model's own values")),
            ("objective_terms", self.objective_terms.clone()),
            ("physics_provider", json!(self.physics_provider)),
            ("provider_execution", json!(self.provider_execution)),
            ("design_coordinate_selection", self.design_coordinate_selection.clone()),
            ("parent_job_id", self.parent_job_id.clone().map_or(Value::Null, Value::String)),
            ("intervention", self.intervention.clone().unwrap_or(Value::Null)),
            ("numerical_attention", self.numerical_attention.clone().unwrap_or(Value::Null)),
            ("numerical_progress", self.numerical_progress.clone().unwrap_or(Value::Null)),
            ("solver_recovery", self.solver_recovery.clone().unwrap_or(Value::Null)),
            ("result_authority", json!(self.result_authority)),
            ("canonical_eligible", json!(self.result_authority == "authoritative")),
            ("steerable", json!(self.steerable)),
            ("momentum", self.momentum.clone()),
            ("regimes", json!(self.regimes)),
            ("solve_id", json!(self.solve_id)),
            ("structure_id", self.structure_id.clone()),
            ("steers", Value::Array(self.steers.iter().cloned().map(Value::Object).collect())),
            ("steered", json!(steered)),
            ("iterations", json!(self.rows.len())),
            ("free", Value::Array(self.design_variables())),
            ("warnings", Value::Array(self.warnings.clone())),
            ("ignored", Value::Object(self.ignored.clone())),
            (
                "timeline",
                Value::Array(
                    self.timeline
                        .iter()
                        .map(|(e, t)| json!({"event": e, "t": fv(round(t - self.t_submit, 2))}))
                        .collect(),
                ),
            ),
            ("poll", json!(format!("/v1/implicit/optimize/jobs/{id}"))),
            ("ops", json!(format!("/v1/implicit/optimize/jobs/{id}"))),
            (
                "steer",
                if self.steerable {
                    json!(format!("/v1/implicit/optimize/jobs/{id}/steer"))
                } else {
                    Value::Null
                },
            ),
            ("before", json!(format!("/v1/implicit/optimize/jobs/{id}/before"))),
        ] {
            d.insert(k.into(), v);
        }
        if let Some(t_start) = self.t_start.filter(|t| *t != 0.0) {
            d.insert("elapsed_s".into(), fv(round(self.t_end.unwrap_or_else(epoch_seconds) - t_start, 1)));
        }
        if let (Some(first), Some(last)) = (self.rows.first(), self.rows.last()) {
            let l_first =
                self.summary.as_ref().and_then(|s| s.get("L_first")).cloned().unwrap_or_else(|| {
                    first.get("L_first").or_else(|| first.get("L")).cloned().unwrap_or(Value::Null)
                });
            d.insert("L_first".into(), l_first);
            d.insert("L_last".into(), last.get("L").cloned().unwrap_or(Value::Null));
            let best = match last.get("L_best") {
                Some(b) => b.clone(),
                None => self
                    .rows
                    .iter()
                    .filter_map(|r| r.get("L"))
                    .min_by(|a, b| {
                        a.as_f64()
                            .unwrap_or(f64::NAN)
                            .partial_cmp(&b.as_f64().unwrap_or(f64::NAN))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .cloned()
                    .unwrap_or(Value::Null),
            };
            d.insert("L_best".into(), best);
            d.insert("last_row".into(), last.clone());
        }
        if with_rows {
            let tail = |v: &[Value]| Value::Array(v[v.len().saturating_sub(400)..].to_vec());
            d.insert("history".into(), tail(&self.rows));
            d.insert("previews".into(), tail(&self.previews));
        }
        if !self.apply_stats.is_empty() {
            let ms: Vec<f64> =
                self.apply_stats.iter().filter_map(|a| a.get("apply_ms").and_then(Value::as_f64)).collect();
            #[allow(clippy::cast_precision_loss)]
            let mean = implexity_mesh::numeric::mean(&ms);
            let max = ms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            d.insert(
                "live_overhead".into(),
                json!({
                    "pushes": ms.len(), "apply_ms_mean": fv(round(mean, 2)),
                    "apply_ms_max": fv(round(max, 2)),
                    "applies_skipped_rate_cap": self.apply_skipped,
                    "apply_cap_hz": fv(crate::manager::live_apply_hz()),
                    "what": "while this job runs the DOCUMENT's free parameters are the optimiser's latest iterate, in memory; the stored file still holds the pre-run values until accept, and any other end puts the pre-run values back",
                }),
            );
        }
        if let Some(summary) = &self.summary {
            let mut s = summary.clone();
            s.shift_remove("history");
            d.insert("summary".into(), Value::Object(s));
        }
        let effort_view = self.computation_effort.as_ref().map(|e| {
            implexity_runtime::provider_job_authority::public_effort_view(e)
                .map_or(Value::Null, Value::Object)
        });
        if let Some(view) = &effort_view {
            d.insert("computation_effort".into(), view.clone());
        }
        if !self.provider_derived_plan.is_empty() {
            d.insert(
                "provider_derived_model_outputs".into(),
                Value::Array(
                    self.provider_derived_plan
                        .iter()
                        .map(|r| {
                            Value::String(r.get("ref").map(implexity_core::pyobj::py_str).unwrap_or_default())
                        })
                        .collect(),
                ),
            );
        }
        if let Some(c) = &self.matching_time_guess_consumed {
            d.insert("matching_time_guess_consumed".into(), c.clone());
        }
        if self.managed_budget_initialized || self.managed_budget_wall_s.is_some() {
            let remaining = self.managed_budget_wall_s.map(|w| (w - self.managed_consumed_wall_s).max(0.0));
            let mut effective = Map::new();
            effective.insert("wall_time_s".into(), self.managed_budget_wall_s.map_or(Value::Null, fv));
            if self.managed_budget_wall_s.is_none() {
                effective.insert("wall_time_mode".into(), json!("unlimited"));
            }
            effective.insert("memory_bytes".into(), json!(self.managed_budget_memory_bytes.unwrap_or(0)));
            let mut observed = Map::new();
            observed.insert("consumed_wall_time_s".into(), fv(self.managed_consumed_wall_s));
            observed.insert("remaining_wall_time_s".into(), remaining.map_or(Value::Null, fv));
            if let Some(Value::Object(o)) = &self.managed_observation {
                for (k, v) in o {
                    observed.insert(k.clone(), v.clone());
                }
            }
            d.insert(
                "supervisor".into(),
                json!({
                    "state": self.managed_supervisor_state,
                    "reason": self.managed_supervisor_reason,
                    "requested": effort_view.unwrap_or(Value::Null),
                    "effective": effective,
                    "observed": observed,
                }),
            );
        }
        if let Some(a) = self.accepted.as_ref().filter(|a| truthy(a)) {
            d.insert("accepted".into(), a.clone());
        }
        if let Some(w) = self.working_design.as_ref().filter(|w| truthy(w)) {
            d.insert("working_design".into(), w.clone());
        }
        if let Some(e) = &self.error {
            d.insert("error".into(), e.clone());
            if let Some(t) = self.stderr_tail.as_ref().filter(|t| !t.is_empty()) {
                d.insert("stderr_tail".into(), json!(t));
            }
        }
        d.insert("record".into(), self.record_pointer());
        Value::Object(d)
    }
}

pub const MANAGED_OPTIMIZATION_RESTART_SCHEMA: &str = "implexity-private-managed-optimization-restart/1";

#[derive(Clone, Debug)]
pub struct RecoveredModelOptJob {
    pub id: String,
    pub t_submit: f64,
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub managed_control: ManagedEvaluationControl,
    pub managed_operation_id: String,
    pub managed_requested_control: Option<String>,
    pub managed_supervisor_state: String,
    pub managed_supervisor_reason: Option<String>,
    pub managed_terminal_event: Arc<Event>,
    pub model_rollback_resolved: bool,
    pub physics_provider: String,
    pub job_dir: String,
    pub request_sha256: String,
    snapshot: Map<String, Value>,
    timeline: Vec<(String, f64)>,
}

impl RecoveredModelOptJob {
    #[must_use]
    pub fn new(
        descriptor: &Map<String, Value>,
        root_live: bool,
        state: &str,
        control: ManagedEvaluationControl,
        job_dir: &std::path::Path,
    ) -> Self {
        let snapshot = descriptor.get("snapshot").and_then(Value::as_object).cloned().unwrap_or_default();
        Self {
            id: descriptor.get("job_id").and_then(Value::as_str).unwrap_or("").to_string(),
            t_submit: descriptor.get("t_submit").and_then(Value::as_f64).unwrap_or(0.0),
            status: if root_live { "running" } else { "finalizing" }.into(),
            progress: snapshot.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
            message: "reattached after service restart; inspection and supervised stop remain available"
                .into(),
            managed_operation_id: control.operation_id().to_string(),
            managed_control: control,
            managed_requested_control: None,
            managed_supervisor_state: state.into(),
            managed_supervisor_reason: None,
            managed_terminal_event: Event::new(),
            model_rollback_resolved: true,
            physics_provider: snapshot
                .get("physics_provider")
                .filter(|v| truthy(v))
                .map(implexity_core::pyobj::py_str)
                .unwrap_or_default(),
            job_dir: job_dir.to_string_lossy().into_owned(),
            request_sha256: descriptor
                .get("request_sha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            snapshot,
            timeline: vec![("reattached".into(), epoch_seconds())],
        }
    }

    pub fn mark(&mut self, event: &str) {
        self.timeline.push((event.into(), epoch_seconds()));
    }

    #[must_use]
    pub fn as_dict(&self, with_rows: bool) -> Value {
        let fv = implexity_optim::numeric::float_value;
        let round = implexity_mesh::numeric::py_round_digits;
        let mut result = self.snapshot.clone();
        let history = if with_rows { json!([]) } else { result.get("history").cloned().unwrap_or(json!([])) };
        for (k, v) in [
            ("kind", json!("implicit_optimize")),
            ("job_id", json!(self.id)),
            ("status", json!(self.status)),
            ("progress", fv(round(self.progress, 3))),
            ("message", json!(self.message)),
            ("iterations", json!(0)),
            ("free", json!([])),
            ("history", history),
            (
                "recovery",
                json!({
                    "schema": "implexity-managed-optimization-recovery/1",
                    "mode": "inspect_stop_only",
                    "operation_id": self.managed_operation_id,
                    "request_sha256": self.request_sha256,
                    "duplicate_submission": false,
                    "model_mutation_authority": false,
                }),
            ),
            (
                "supervisor",
                json!({"state": self.managed_supervisor_state, "reason": self.managed_supervisor_reason}),
            ),
            (
                "timeline",
                Value::Array(
                    self.timeline
                        .iter()
                        .map(|(e, t)| json!({"event": e, "t": fv(round(t - self.t_submit, 2))}))
                        .collect(),
                ),
            ),
        ] {
            result.insert(k.into(), v);
        }
        if !with_rows {
            result.shift_remove("history");
        }
        Value::Object(result)
    }
}
