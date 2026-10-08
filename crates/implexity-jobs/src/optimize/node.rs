// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use implexity_geometry::eval::{EvalOptions, compile_f64};
use implexity_geometry::node::{
    Attr, ConstructArgs, EvalCtx, KernelBox, KernelInputs, KindEntry, KindInfo, Node, NodeOp,
};
use implexity_geometry::scalar::Scalar;
use implexity_geometry::{FieldClass, GResult, GeometryError, NodeRef};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use super::spec::{OptimizeSpec, array_param, rebind, safe};
use crate::error::{JobError, JobResult};

#[derive(Clone, Debug)]
pub struct Solution {
    pub solve_id: String,
    pub values: Vec<(String, ArrayD<f64>)>,
    pub summary: Map<String, Value>,
    pub job_dir: PathBuf,
    pub wall_s: f64,
}

impl Solution {
    #[must_use]
    pub fn describe(&self) -> Value {
        serde_json::json!({
            "solve_id": self.solve_id, "wall_s": self.wall_s, "job_dir": self.job_dir.to_string_lossy(),
            "values": Value::Object(self.values.iter().map(|(k, v)| (k.clone(), safe(v))).collect()),
            "L_first": self.summary.get("L_first"), "L_best": self.summary.get("L_best"),
            "iterations": self.summary.get("iterations"),
        })
    }
}

static SOLUTIONS: LazyLock<Mutex<HashMap<String, Solution>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SOLVE_COUNT: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

pub fn cache_clear() {
    SOLUTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}

#[must_use]
pub fn cache_size() -> usize {
    SOLUTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
}

#[must_use]
pub fn solve_count() -> u64 {
    *SOLVE_COUNT.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cached(sid: &str) -> Option<Solution> {
    SOLUTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(sid).cloned()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Declaration {
    pub free: Vec<Value>,
    pub objective: Vec<Value>,
    pub constraints: Vec<Value>,
    pub case: Value,
    pub settings: Map<String, Value>,
}

pub struct OptimizeOp {
    decl: Declaration,
    spec: Mutex<Option<(usize, Arc<OptimizeSpec>)>>,
}

impl std::fmt::Debug for OptimizeOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptimizeOp").field("decl", &self.decl).finish_non_exhaustive()
    }
}

fn info() -> &'static KindInfo {
    static INFO: LazyLock<KindInfo> = LazyLock::new(|| KindInfo {
        kind: "optimize".into(),
        params: Vec::new(),
        discrete: Vec::new(),
        module: "implexity.implicit.optimize".into(),
        doc: "``Optimize(child, free=[...], objective=[...], constraints=[...])``.".into(),
        field_class_doc: "Optimisation moves values, not the algebra: the child's promise".into(),
        leaf_class: None,
        is_struct: false,
        glsl_refusal: None,
    });
    &INFO
}

fn geo(e: &JobError) -> GeometryError {
    GeometryError::Model(e.message())
}

impl OptimizeOp {
    #[must_use]
    pub fn declaration(&self) -> &Declaration {
        &self.decl
    }


    pub fn spec(&self, node: &Node) -> JobResult<Arc<OptimizeSpec>> {
        let child = node
            .children()
            .first()
            .cloned()
            .ok_or_else(|| JobError::optimize1("an Optimize node has one child"))?;
        let key = Arc::as_ptr(&child) as usize;
        let mut g = self.spec.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((k, s)) = g.as_ref()
            && *k == key
        {
            return Ok(Arc::clone(s));
        }
        let s = Arc::new(OptimizeSpec::new(
            &child,
            &self.decl.free,
            &self.decl.objective,
            &self.decl.constraints,
            &self.decl.case,
            &self.decl.settings,
        )?);
        *g = Some((key, Arc::clone(&s)));
        Ok(s)
    }


    pub fn solve_id(&self, node: &Node) -> JobResult<String> {
        let spec = self.spec(node)?;
        let subtree = Arc::new(node.with_params(node.params().clone())).content_id();
        let mut h = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut h, subtree.as_bytes());
        sha2::Digest::update(&mut h, [0x1f]);
        sha2::Digest::update(&mut h, spec.digest().as_bytes());
        Ok(hex::encode(sha2::Digest::finalize(h))[..16].to_string())
    }


    pub fn solve(
        &self,
        node: &Node,
        out_dir: Option<PathBuf>,
        resume: bool,
        emit: super::solve::Emit<'_>,
        log: super::solve::LogFn<'_>,
        replay: Option<&[Value]>,
        force: bool,
    ) -> JobResult<Solution> {
        let sid = self.solve_id(node)?;
        if !force && let Some(s) = cached(&sid) {
            return Ok(s);
        }
        let out_dir = out_dir.unwrap_or_else(|| {
            let base = std::env::var("IMPLEXITY_CASE_DIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map_or_else(|| home().join(".implexity"), PathBuf::from);
            base.join("implicit_opt").join(&sid)
        });
        let spec = self.spec(node)?;
        let t0 = std::time::Instant::now();
        let outcome = super::solve::solve(&spec, &out_dir, resume, emit, log, replay)?;
        *SOLVE_COUNT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        let values = spec
            .free
            .iter()
            .map(|f| (f.ref_str(), outcome.best.get(&f.slot).cloned().unwrap_or_else(|| f.start.clone())))
            .collect();
        let sol = Solution {
            solve_id: sid.clone(),
            values,
            summary: outcome.summary,
            job_dir: out_dir,
            wall_s: implexity_mesh::numeric::py_round_digits(t0.elapsed().as_secs_f64(), 3),
        };
        SOLUTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(sid, sol.clone());
        Ok(sol)
    }


    pub fn resolved(&self, node: &Node, sol: &Solution) -> JobResult<NodeRef> {
        let spec = self.spec(node)?;
        let mut vals = Vec::new();
        for fr in &spec.free {
            if let Some((_, v)) = sol.values.iter().find(|(k, _)| *k == fr.ref_str()) {
                vals.push((fr.child_ref(), array_param(v)));
            }
        }
        rebind(&spec.model, &vals)
    }


    pub fn describe(&self, node: &Node) -> JobResult<Map<String, Value>> {
        let spec = self.spec(node)?;
        let sid = self.solve_id(node)?;
        let mut d = Map::new();
        d.insert("optimize".into(), spec.describe());
        d.insert("solve_id".into(), Value::from(sid.clone()));
        let sol = cached(&sid);
        d.insert("solved".into(), Value::Bool(sol.is_some()));
        if let Some(s) = sol {
            d.insert("solution".into(), s.describe());
        }
        Ok(d)
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn free_to_doc(f: &Value) -> Value {
    match f {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let v = match v.as_object().and_then(|o| o.get("$ref")) {
                        Some(r) => r.clone(),
                        None => v.clone(),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::String(_) => f.clone(),
        other => Value::from(implexity_core::pyobj::py_str(other)),
    }
}

fn attr_json(a: &Attr) -> Value {
    match a {
        Attr::Ref(r) => Value::from(r.as_str()),
        Attr::List(v) => Value::Array(v.iter().map(attr_json).collect()),
        Attr::Dict(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), attr_json(v))).collect()),
        other => other.to_json(),
    }
}

fn list_attr(a: Option<Attr>) -> Vec<Value> {
    match a.map(|a| attr_json(&a)) {
        Some(Value::Array(v)) => v,
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other],
    }
}


pub fn construct(mut args: ConstructArgs) -> GResult<Node> {
    let free = list_attr(args.take_attr("free"));
    let objective = list_attr(args.take_attr("objective"));
    let constraints = list_attr(args.take_attr("constraints"));
    let case = args.take_attr("case").map_or(Value::Null, |a| attr_json(&a));
    let mut settings = Map::new();
    for (k, v) in std::mem::take(&mut args.attrs) {
        settings.insert(k, attr_json(&v));
    }
    for (k, v) in std::mem::take(&mut args.params) {
        settings.insert(k, v.to_json());
    }
    if args.children.len() != 1 {
        return Err(GeometryError::Model(format!(
            "optimize takes 1 child(ren), got {}",
            args.children.len()
        )));
    }
    for k in ["free", "objective", "constraints", "case"] {
        if settings.contains_key(k) {
            return Err(GeometryError::Model(format!(
                "optimiser setting {} collides with a declaration slot of the same name; rename the setting",
                implexity_core::py_repr::repr_str(k)
            )));
        }
    }
    let op = Arc::new(OptimizeOp {
        decl: Declaration { free, objective, constraints, case, settings },
        spec: Mutex::new(None),
    });
    let node =
        Node::new(op.clone(), args.children, Some(vec!["model".into()]), std::collections::BTreeMap::new())?;
    op.spec(&node).map_err(|e| geo(&e))?;
    Ok(node)
}


pub fn optimize_node(child: NodeRef, decl: &Declaration) -> JobResult<NodeRef> {
    let mut attrs = std::collections::BTreeMap::new();
    attrs.insert("free".to_string(), Attr::from_json(&Value::Array(decl.free.clone())));
    attrs.insert("objective".to_string(), Attr::from_json(&Value::Array(decl.objective.clone())));
    attrs.insert("constraints".to_string(), Attr::from_json(&Value::Array(decl.constraints.clone())));
    attrs.insert("case".to_string(), Attr::from_json(&decl.case));
    for (k, v) in &decl.settings {
        attrs.insert(k.clone(), Attr::from_json(v));
    }
    let n = construct(ConstructArgs {
        children: vec![child],
        names: None,
        params: std::collections::BTreeMap::new(),
        attrs,
    })?;
    Ok(Arc::new(n))
}

#[must_use]
pub fn op_of(node: &Node) -> Option<&OptimizeOp> {
    node.op().as_any().downcast_ref::<OptimizeOp>()
}

pub fn register_kind() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if !implexity_geometry::node::registry().contains("optimize") {
            let _ = implexity_geometry::node::register_kind(KindEntry {
                info: Arc::new(info().clone()),
                construct: Arc::new(construct),
            });
        }
    });
}

impl NodeOp for OptimizeOp {
    fn info(&self) -> &KindInfo {
        info()
    }

    fn doc_attrs(&self) -> Vec<(String, Attr)> {
        let d = &self.decl;
        let mut out = vec![
            ("free".to_string(), Attr::from_json(&Value::Array(d.free.iter().map(free_to_doc).collect()))),
            ("objective".to_string(), Attr::from_json(&Value::Array(d.objective.clone()))),
            ("constraints".to_string(), Attr::from_json(&Value::Array(d.constraints.clone()))),
            ("case".to_string(), Attr::from_json(&d.case)),
        ];
        for (k, v) in &d.settings {
            out.push((k.clone(), Attr::from_json(v)));
        }
        out
    }

    fn field_class(&self, _node: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        kids.first().cloned().ok_or_else(|| GeometryError::Value("tuple index out of range".into()))
    }

    fn kernel<S: Scalar>(
        &self,
        node: &Node,
        _inputs: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let sid = self.solve_id(node).map_err(|e| geo(&e))?;
        let solved = cached(&sid);
        if TypeId::of::<S>() == TypeId::of::<f64>() {

            let sol = match solved {
                Some(s) => s,
                None => {
                    self.solve(node, None, false, &|_, _| {}, &|_| {}, None, false).map_err(|e| geo(&e))?
                }
            };
            let child = self.resolved(node, &sol).map_err(|e| geo(&e))?;
            let opts = EvalOptions {
                mode: ctx.mode,
                smooth_kind: ctx.smooth_kind,
                smooth_r_mm: Some(ctx.smooth_r),
                validate: false,
            };
            let k: KernelBox<f64> = compile_f64(&child, &opts)?;
            let boxed: Box<dyn Any> = Box::new(k);
            return boxed
                .downcast::<KernelBox<S>>()
                .map(|b| *b)
                .map_err(|_| GeometryError::Model("optimize kernel type mismatch".into()));
        }
        if solved.is_none() {
            return Err(GeometryError::Model(format!(
                "this Optimize node has not been solved and f() was called under a jax trace: running a \
                 coupled-physics optimisation inside a traced kernel is not something this node will do silently.  \
                 Call .solve() first (it is memoised on content_id {sid}), then trace."
            )));
        }

        let _ = kids;
        Err(GeometryError::Model(format!(
            "differentiate the resolved child of Optimize node {sid} (resolved()), not the Optimize node itself"
        )))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
