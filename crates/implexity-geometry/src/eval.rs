// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END






use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rayon::prelude::*;

use crate::boxes::SampleBox;
use crate::error::{GResult, GeometryError, model_err};
use crate::fieldclass::{ClassKind, FieldClass, require};
use crate::node::{
    EvalCtx, KernelBox, KernelInputs, KernelScalar, Mode, Node, NodeRef, ParamRef, ParamS, Prepared,
    SmoothKind,
};
use crate::pyfmt;
use crate::scalar::{Dual, RevTape, Rv, Scalar};
use crate::value::ParamValue;

pub const DEFAULT_SMOOTH_MM: f64 = 0.5;

pub const SAFE_EPS: f64 = 1e-24;

pub const POINT_BLOCK: usize = 4096;

pub struct Stats {
    traces: AtomicU64,
    calls: AtomicU64,
}

impl Stats {
    pub fn reset(&self) {
        self.traces.store(0, Ordering::Relaxed);
        self.calls.store(0, Ordering::Relaxed);
    }

    #[must_use]
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({"traces": self.traces.load(Ordering::Relaxed), "calls": self.calls.load(Ordering::Relaxed)})
    }
}

pub static STATS: Stats = Stats { traces: AtomicU64::new(0), calls: AtomicU64::new(0) };

#[derive(Clone, Copy, Debug)]
pub struct EvalOptions {
    pub mode: Mode,
    pub smooth_kind: SmoothKind,
    pub smooth_r_mm: Option<f64>,
    pub validate: bool,
}

impl Default for EvalOptions {
    fn default() -> Self {
        Self { mode: Mode::Exact, smooth_kind: SmoothKind::Poly, smooth_r_mm: None, validate: true }
    }
}

impl EvalOptions {
    #[must_use]
    pub fn exact() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn smooth() -> Self {
        Self { mode: Mode::Smooth, ..Self::default() }
    }

    fn ctx(&self) -> EvalCtx {
        EvalCtx {
            mode: self.mode,
            smooth_kind: self.smooth_kind,
            smooth_r: self.smooth_r_mm.unwrap_or(DEFAULT_SMOOTH_MM),
        }
    }
}


pub fn field_class_of(node: &NodeRef, mode: Mode) -> GResult<FieldClass> {
    let mut memo: HashMap<usize, FieldClass> = HashMap::new();
    fold_class(node, mode, &mut memo, &HashMap::new())
}


pub fn field_class_with_overrides<H: std::hash::BuildHasher>(
    node: &NodeRef,
    mode: Mode,
    overrides: &HashMap<usize, FieldClass, H>,
) -> GResult<FieldClass> {
    let mut memo: HashMap<usize, FieldClass> = HashMap::new();
    fold_class(node, mode, &mut memo, overrides)
}

#[must_use]
pub fn node_id(node: &NodeRef) -> usize {
    Arc::as_ptr(node).cast::<u8>() as usize
}

fn fold_class<H: std::hash::BuildHasher>(
    node: &NodeRef,
    mode: Mode,
    memo: &mut HashMap<usize, FieldClass>,
    overrides: &HashMap<usize, FieldClass, H>,
) -> GResult<FieldClass> {
    let key = node_id(node);
    if let Some(hit) = memo.get(&key) {
        return Ok(hit.clone());
    }
    let fc = if let Some(o) = overrides.get(&key) {
        o.clone()
    } else {
        let mut kids = Vec::with_capacity(node.children().len());
        for c in node.children() {
            kids.push(fold_class(c, mode, memo, overrides)?);
        }
        match mode {
            Mode::Exact => node.op().field_class(node, &kids)?,
            Mode::Smooth => node.op().field_class_smooth(node, &kids)?,
        }
    };
    memo.insert(key, fc.clone());
    Ok(fc)
}

pub type Aabb = ([f64; 3], [f64; 3]);

#[must_use]
pub fn aabb_of(node: &Node) -> Option<Aabb> {
    let (lo, hi) = node.op().aabb(node).ok()??;
    if lo.iter().chain(hi.iter()).any(|v| !v.is_finite()) || (0..3).any(|i| hi[i] < lo[i]) {
        return None;
    }
    Some((lo, hi))
}

#[must_use]
pub fn disjoint(a: &Node, b: &Node, gap: f64) -> bool {
    let (Some(ba), Some(bb)) = (aabb_of(a), aabb_of(b)) else {
        return false;
    };
    (0..3).any(|i| ba.1[i] + gap < bb.0[i] || bb.1[i] + gap < ba.0[i])
}

fn exc_repr(e: &GeometryError) -> String {
    let class = match e {
        GeometryError::Model(_) => "ModelError",
        GeometryError::BoundViolation(_) => "BoundViolation",
        GeometryError::Transpile { .. } => "TranspileError",
        GeometryError::Expr(_) => "ExprError",
        _ => "ValueError",
    };
    format!("{class}({})", pyfmt::str_repr(&e.to_string()))
}

#[must_use]
pub fn check_graph(node: &NodeRef) -> Vec<String> {
    let mut out = Vec::new();
    for (path, n) in node.walk() {
        let msg = match n.op().validate(&n) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(e) => exc_repr(&e),
        };
        if !msg.is_empty() {
            let p = path.join("/");
            out.push(format!("{} ({}): {}", n.kind(), if p.is_empty() { "<root>" } else { &p }, msg));
        }
    }
    out
}

fn validate_or_err(node: &NodeRef) -> GResult<()> {
    let bad = check_graph(node);
    if bad.is_empty() { Ok(()) } else { model_err(bad.join("; ")) }
}


pub fn params_pytree(node: &NodeRef) -> GResult<BTreeMap<String, BTreeMap<String, (Vec<usize>, Vec<f64>)>>> {
    let mut out = BTreeMap::new();
    for (path, n) in node.walk() {
        let mut m = BTreeMap::new();
        for (k, v) in n.params() {
            let arr = v.to_f64_array().map_err(|_| {
                GeometryError::Value(format!(
                    "could not convert parameter {} of {} to float64",
                    pyfmt::str_repr(k),
                    n.kind()
                ))
            })?;
            m.insert(k.clone(), arr);
        }
        out.insert(path.join("/"), m);
    }
    Ok(out)
}


pub fn set_params(node: &NodeRef, updates: &[(ParamRef, ParamValue)]) -> GResult<NodeRef> {
    let mut pt = params_pytree(node)?;
    for (r, v) in updates {
        let k = r.path_key();
        let Some(slot) = pt.get_mut(&k).and_then(|m| m.get_mut(&r.name)) else {
            return model_err(format!("no parameter {} in this graph", pyfmt::str_repr(&r.as_str())));
        };
        *slot = v.to_f64_array().map_err(|_| {
            GeometryError::Value(format!("could not convert {} to float64", pyfmt::str_repr(&r.as_str())))
        })?;
    }
    Ok(bind(node, &pt, &mut Vec::new()))
}

#[must_use]
pub fn bind(
    node: &NodeRef,
    pt: &BTreeMap<String, BTreeMap<String, (Vec<usize>, Vec<f64>)>>,
    path: &mut Vec<String>,
) -> NodeRef {
    let key = path.join("/");
    let params = pt.get(&key).map_or_else(
        || node.params().clone(),
        |m| {
            m.iter()
                .filter_map(|(k, (shape, data))| {
                    ParamValue::array_f64(shape.clone(), data.clone()).map(|v| (k.clone(), v))
                })
                .collect()
        },
    );
    let mut kids = Vec::with_capacity(node.children().len());
    for (name, c) in node.named_children() {
        path.push(name.clone());
        kids.push(bind(c, pt, path));
        path.pop();
    }
    Arc::new(node.with_params(params).with_children(kids))
}

#[must_use]
pub fn discrete_refs(node: &NodeRef) -> Vec<ParamRef> {
    let mut out = Vec::new();
    for (path, n) in node.walk() {
        for name in &n.info().discrete {
            out.push(ParamRef::new(path.clone(), name.clone()));
        }
    }
    out
}

type PreparedMap = HashMap<String, Prepared>;

fn prepare_all(node: &NodeRef, want_vjp: &dyn Fn(&str) -> bool) -> GResult<PreparedMap> {
    let mut out = HashMap::new();
    for (path, n) in node.walk() {
        let key = path.join("/");
        let p = n.op().prepare(&n, want_vjp(&key))?;
        if !p.derived.is_empty() {
            out.insert(key, p);
        }
    }
    Ok(out)
}

trait InputSource<S: KernelScalar> {
    fn inputs(
        &mut self,
        key: &str,
        node: &Node,
        derived: &[(Vec<usize>, Vec<f64>)],
    ) -> GResult<KernelInputs<S>>;
}

fn param_arrays(node: &Node) -> GResult<Vec<(String, Vec<usize>, Vec<f64>)>> {
    let mut out = Vec::with_capacity(node.params().len());
    for (k, v) in node.params() {
        let (shape, data) = v.to_f64_array().map_err(|_| {
            GeometryError::Value(format!(
                "parameter {} of {} is not numeric and cannot be evaluated",
                pyfmt::str_repr(k),
                node.kind()
            ))
        })?;
        out.push((k.clone(), shape, data));
    }
    Ok(out)
}

struct ConstInputs;

impl<S: KernelScalar> InputSource<S> for ConstInputs {
    fn inputs(
        &mut self,
        _key: &str,
        node: &Node,
        derived: &[(Vec<usize>, Vec<f64>)],
    ) -> GResult<KernelInputs<S>> {
        let params = param_arrays(node)?
            .into_iter()
            .map(|(k, shape, data)| (k, ParamS { shape, data: data.into_iter().map(S::cst).collect() }))
            .collect();
        let derived = derived
            .iter()
            .map(|(shape, d)| ParamS { shape: shape.clone(), data: d.iter().map(|x| S::cst(*x)).collect() })
            .collect();
        Ok(KernelInputs { params, derived })
    }
}

fn build_kernel<S: KernelScalar>(
    node: &NodeRef,
    path: &mut Vec<String>,
    ctx: &EvalCtx,
    prepared: &PreparedMap,
    src: &mut dyn InputSource<S>,
) -> GResult<KernelBox<S>> {
    let mut kids = Vec::with_capacity(node.children().len());
    for (name, c) in node.named_children() {
        path.push(name.clone());
        kids.push(build_kernel(c, path, ctx, prepared, src)?);
        path.pop();
    }
    let key = path.join("/");
    let derived: &[(Vec<usize>, Vec<f64>)] = prepared.get(&key).map_or(&[], |p| p.derived.as_slice());
    let inputs = src.inputs(&key, node, derived)?;
    S::build(node.op(), node, &inputs, kids, ctx)
}


pub fn compile_f64(node: &NodeRef, opts: &EvalOptions) -> GResult<KernelBox<f64>> {
    if opts.validate {
        validate_or_err(node)?;
    }
    let prepared = prepare_all(node, &|_| false)?;
    STATS.traces.fetch_add(1, Ordering::Relaxed);
    build_kernel::<f64>(node, &mut Vec::new(), &opts.ctx(), &prepared, &mut ConstInputs)
}


pub fn eval_points(node: &NodeRef, x: &[[f64; 3]], opts: &EvalOptions) -> GResult<Vec<f64>> {
    if x.is_empty() {
        if opts.validate {
            validate_or_err(node)?;
        }
        return Ok(Vec::new());
    }
    let k = compile_f64(node, opts)?;
    STATS.calls.fetch_add(1, Ordering::Relaxed);
    Ok(eval_kernel(&k, x))
}

#[must_use]
pub fn eval_kernel(k: &KernelBox<f64>, x: &[[f64; 3]]) -> Vec<f64> {
    if x.len() <= POINT_BLOCK {
        return x.iter().map(|p| k.eval(*p)).collect();
    }
    let mut out = vec![0.0; x.len()];
    out.par_chunks_mut(POINT_BLOCK).zip(x.par_chunks(POINT_BLOCK)).for_each(|(o, pts)| {
        for (oi, p) in o.iter_mut().zip(pts) {
            *oi = k.eval(*p);
        }
    });
    out
}


pub fn grad_x(node: &NodeRef, x: &[[f64; 3]], opts: &EvalOptions) -> GResult<Vec<[f64; 3]>> {
    let prepared = prepare_all(node, &|_| false)?;
    let k = build_kernel::<Dual<3>>(node, &mut Vec::new(), &opts.ctx(), &prepared, &mut ConstInputs)?;
    STATS.traces.fetch_add(1, Ordering::Relaxed);
    let one = |p: &[f64; 3]| {
        let d = k.eval([Dual::var(p[0], 0), Dual::var(p[1], 1), Dual::var(p[2], 2)]);
        d.d
    };
    let out = if x.len() <= POINT_BLOCK {
        x.iter().map(one).collect()
    } else {
        let mut out = vec![[0.0; 3]; x.len()];
        out.par_chunks_mut(POINT_BLOCK).zip(x.par_chunks(POINT_BLOCK)).for_each(|(o, pts)| {
            for (oi, p) in o.iter_mut().zip(pts) {
                *oi = one(p);
            }
        });
        out
    };
    STATS.calls.fetch_add(1, Ordering::Relaxed);
    Ok(out)
}

pub trait Loss: Sync {
    fn value_and_cotangent(&self, f: &[f64]) -> (f64, Vec<f64>);
}

impl<F: Fn(&[f64]) -> (f64, Vec<f64>) + Sync> Loss for F {
    fn value_and_cotangent(&self, f: &[f64]) -> (f64, Vec<f64>) {
        self(f)
    }
}

pub struct MeanLoss;

impl Loss for MeanLoss {
    fn value_and_cotangent(&self, f: &[f64]) -> (f64, Vec<f64>) {
        #[allow(clippy::cast_precision_loss)]
        let n = f.len() as f64;
        (crate::numpy::mean(f), vec![1.0 / n; f.len()])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamGrad {
    pub r: ParamRef,
    pub shape: Vec<usize>,
    pub data: Vec<f64>,
}

struct RevInputs<'a> {
    tape: &'a RevTape,
    free: &'a BTreeMap<String, BTreeSet<String>>,
    leaves: BTreeMap<String, Vec<(String, Vec<Rv>)>>,
    derived_leaves: BTreeMap<String, Vec<Vec<Rv>>>,
}

impl InputSource<Rv> for RevInputs<'_> {
    fn inputs(
        &mut self,
        key: &str,
        node: &Node,
        derived: &[(Vec<usize>, Vec<f64>)],
    ) -> GResult<KernelInputs<Rv>> {
        let free = self.free.get(key);
        let mut params = Vec::new();
        for (k, shape, data) in param_arrays(node)? {
            let is_free = free.is_some_and(|s| s.contains(&k));
            let vals: Vec<Rv> = if is_free {
                data.iter().map(|x| self.tape.leaf(*x)).collect()
            } else {
                data.iter().map(|x| Rv::cst(*x)).collect()
            };
            if is_free {
                self.leaves.entry(key.to_string()).or_default().push((k.clone(), vals.clone()));
            }
            params.push((k, ParamS { shape, data: vals }));
        }
        let mut dvals = Vec::new();
        if free.is_some() && !derived.is_empty() {
            let mut dl = Vec::new();
            for (shape, d) in derived {
                let v: Vec<Rv> = d.iter().map(|x| self.tape.leaf(*x)).collect();
                dl.push(v.clone());
                dvals.push(ParamS { shape: shape.clone(), data: v });
            }
            self.derived_leaves.insert(key.to_string(), dl);
        } else {
            for (shape, d) in derived {
                dvals.push(ParamS { shape: shape.clone(), data: d.iter().map(|x| Rv::cst(*x)).collect() });
            }
        }
        Ok(KernelInputs { params, derived: dvals })
    }
}

type BlockAdj = (BTreeMap<String, Vec<(String, Vec<f64>)>>, BTreeMap<String, Vec<Vec<f64>>>);

fn reverse_block(
    node: &NodeRef,
    ctx: &EvalCtx,
    prepared: &PreparedMap,
    free: &BTreeMap<String, BTreeSet<String>>,
    pts: &[[f64; 3]],
    cot: &[f64],
) -> GResult<BlockAdj> {
    let mut tape = RevTape::begin()?;
    let mut src = RevInputs { tape: &tape, free, leaves: BTreeMap::new(), derived_leaves: BTreeMap::new() };
    let k = build_kernel::<Rv>(node, &mut Vec::new(), ctx, prepared, &mut src)?;
    let RevInputs { leaves, derived_leaves, .. } = src;
    for (p, c) in pts.iter().zip(cot) {
        let mark = tape.len();
        let out = k.eval([Rv::cst(p[0]), Rv::cst(p[1]), Rv::cst(p[2])]);
        tape.sweep_to(out, *c, mark);
    }
    drop(k);
    tape.finish();
    let pa = leaves
        .into_iter()
        .map(|(key, v)| {
            (
                key,
                v.into_iter()
                    .map(|(name, ls)| (name, ls.iter().map(|l| tape.adjoint(*l)).collect()))
                    .collect(),
            )
        })
        .collect();
    let da = derived_leaves
        .into_iter()
        .map(|(key, v)| {
            (key, v.into_iter().map(|ls| ls.iter().map(|l| tape.adjoint(*l)).collect()).collect())
        })
        .collect();
    Ok((pa, da))
}

fn add_into(acc: &mut [f64], v: &[f64]) {
    for (a, b) in acc.iter_mut().zip(v) {
        *a += b;
    }
}


pub fn value_and_grad_params(
    node: &NodeRef,
    refs: &[ParamRef],
    loss: &dyn Loss,
    x: &[[f64; 3]],
    opts: &EvalOptions,
) -> GResult<(f64, Vec<ParamGrad>)> {
    if opts.validate {
        validate_or_err(node)?;
    }
    let discrete: BTreeSet<String> = discrete_refs(node).iter().map(ParamRef::as_str).collect();
    let bad: BTreeSet<String> = refs.iter().map(ParamRef::as_str).filter(|s| discrete.contains(s)).collect();
    if !bad.is_empty() {
        return model_err(format!(
            "no derivative exists with respect to {}: the field is piecewise constant in a pattern's copy count, so a \
             gradient there is zero everywhere it is defined and undefined at every integer.  Move it as a parameter -- \
             which is free, it does not recompile -- and leave it out of the free set.",
            bad.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let pt = params_pytree(node)?;
    let missing: Vec<String> = refs
        .iter()
        .filter(|r| !pt.get(&r.path_key()).is_some_and(|m| m.contains_key(&r.name)))
        .map(ParamRef::as_str)
        .collect();
    if !missing.is_empty() {
        return model_err(format!("no such parameter(s) in this graph: {}", missing.join(", ")));
    }
    let mut free: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in refs {
        free.entry(r.path_key()).or_default().insert(r.name.clone());
    }
    let ctx = opts.ctx();
    let prepared = prepare_all(node, &|key| free.contains_key(key))?;

    let kf = build_kernel::<f64>(node, &mut Vec::new(), &ctx, &prepared, &mut ConstInputs)?;
    let f = eval_kernel(&kf, x);
    drop(kf);
    let (value, cot) = loss.value_and_cotangent(&f);
    STATS.traces.fetch_add(1, Ordering::Relaxed);
    let blocks: Vec<GResult<BlockAdj>> = x
        .par_chunks(POINT_BLOCK)
        .zip(cot.par_chunks(POINT_BLOCK))
        .map(|(p, c)| reverse_block(node, &ctx, &prepared, &free, p, c))
        .collect();

    let mut param_adj: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    let mut derived_adj: BTreeMap<String, Vec<Vec<f64>>> = BTreeMap::new();
    for b in blocks {
        let (pa, da) = b?;
        for (key, list) in pa {
            for (name, g) in list {
                match param_adj.get_mut(&(key.clone(), name.clone())) {
                    Some(acc) => add_into(acc, &g),
                    None => {
                        param_adj.insert((key.clone(), name), g);
                    }
                }
            }
        }
        for (key, list) in da {
            match derived_adj.get_mut(&key) {
                Some(acc) => {
                    for (a, g) in acc.iter_mut().zip(list) {
                        add_into(a, &g);
                    }
                }
                None => {
                    derived_adj.insert(key, list);
                }
            }
        }
    }

    for (key, adj) in &derived_adj {
        let Some(p) = prepared.get(key) else { continue };
        let Some(vjp) = &p.vjp else {
            return model_err(format!("node at {} has derived arrays but no pullback", pyfmt::str_repr(key)));
        };
        for (name, g) in vjp(adj)? {
            if !free.get(key).is_some_and(|s| s.contains(&name)) {
                continue;
            }
            match param_adj.get_mut(&(key.clone(), name.clone())) {
                Some(acc) => add_into(acc, &g),
                None => {
                    param_adj.insert((key.clone(), name), g);
                }
            }
        }
    }
    STATS.calls.fetch_add(1, Ordering::Relaxed);
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let (shape, vals) = &pt[&r.path_key()][&r.name];
        let data =
            param_adj.get(&(r.path_key(), r.name.clone())).cloned().unwrap_or_else(|| vec![0.0; vals.len()]);
        out.push(ParamGrad { r: r.clone(), shape: shape.clone(), data });
    }
    Ok((value, out))
}


pub fn grad_params(
    node: &NodeRef,
    refs: &[ParamRef],
    loss: &dyn Loss,
    x: &[[f64; 3]],
    opts: &EvalOptions,
) -> GResult<Vec<ParamGrad>> {
    Ok(value_and_grad_params(node, refs, loss, x, opts)?.1)
}


pub fn jacobian_params_forward(
    node: &NodeRef,
    refs: &[ParamRef],
    x: &[[f64; 3]],
    opts: &EvalOptions,
) -> GResult<Vec<Vec<f64>>> {
    let pt = params_pytree(node)?;
    let mut columns: Vec<(String, String, usize)> = Vec::new();
    for r in refs {
        let Some((_, data)) = pt.get(&r.path_key()).and_then(|m| m.get(&r.name)) else {
            return model_err(format!("no such parameter(s) in this graph: {}", r.as_str()));
        };
        for i in 0..data.len() {
            columns.push((r.path_key(), r.name.clone(), i));
        }
    }
    let ctx = opts.ctx();
    let free: BTreeSet<String> = refs.iter().map(ParamRef::path_key).collect();
    let prepared = prepare_all(node, &|key| free.contains(key))?;
    let mut jac = vec![vec![0.0; columns.len()]; x.len()];
    for (c0, chunk) in columns.chunks(4).enumerate() {
        struct Seeded<'a> {
            chunk: &'a [(String, String, usize)],
            prepared: &'a PreparedMap,
        }
        impl InputSource<Dual<4>> for Seeded<'_> {
            fn inputs(
                &mut self,
                key: &str,
                node: &Node,
                derived: &[(Vec<usize>, Vec<f64>)],
            ) -> GResult<KernelInputs<Dual<4>>> {
                let mut params = Vec::new();
                for (k, shape, data) in param_arrays(node)? {
                    let mut vals: Vec<Dual<4>> = data.iter().map(|v| Dual::cst(*v)).collect();
                    for (slot, (ck, cn, ci)) in self.chunk.iter().enumerate() {
                        if ck == key && *cn == k {
                            vals[*ci].d[slot] = 1.0;
                        }
                    }
                    params.push((k, ParamS { shape, data: vals }));
                }

                let mut dvals = Vec::new();
                if let Some(p) = self.prepared.get(key) {
                    let seeded: Vec<usize> =
                        (0..self.chunk.len()).filter(|s| self.chunk[*s].0 == key).collect();
                    let mut tangents: Vec<Vec<Vec<f64>>> =
                        derived.iter().map(|(_, d)| vec![vec![0.0; 4]; d.len()]).collect();
                    if !seeded.is_empty() {
                        let Some(vjp) = &p.vjp else {
                            return model_err("derived arrays without a pullback".to_string());
                        };

                        for (a, (_, d)) in derived.iter().enumerate() {
                            for e in 0..d.len() {
                                let mut adj: Vec<Vec<f64>> =
                                    derived.iter().map(|(_, dd)| vec![0.0; dd.len()]).collect();
                                adj[a][e] = 1.0;
                                let pulled = vjp(&adj)?;
                                for &s in &seeded {
                                    let (_, name, ci) = &self.chunk[s];
                                    if let Some((_, g)) = pulled.iter().find(|(n, _)| n == name) {
                                        tangents[a][e][s] = g.get(*ci).copied().unwrap_or(0.0);
                                    }
                                }
                            }
                        }
                    }
                    for ((shape, d), t) in derived.iter().zip(tangents) {
                        dvals.push(ParamS {
                            shape: shape.clone(),
                            data: d
                                .iter()
                                .zip(t)
                                .map(|(v, tt)| Dual { v: *v, d: [tt[0], tt[1], tt[2], tt[3]] })
                                .collect(),
                        });
                    }
                }
                Ok(KernelInputs { params, derived: dvals })
            }
        }
        let mut src = Seeded { chunk, prepared: &prepared };
        let k = build_kernel::<Dual<4>>(node, &mut Vec::new(), &ctx, &prepared, &mut src)?;
        for (row, p) in jac.iter_mut().zip(x) {
            let d = k.eval([Dual::cst(p[0]), Dual::cst(p[1]), Dual::cst(p[2])]);
            for s in 0..chunk.len() {
                row[c0 * 4 + s] = d.d[s];
            }
        }
    }
    Ok(jac)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridStats {
    pub shape: [usize; 3],
    pub dense: usize,
    pub field_class: String,
    pub mode: String,
    pub blocks: usize,
    pub pruned_blocks: usize,
    pub sampled: usize,
    pub dead_samples: Option<usize>,
    pub sample_fraction: f64,
    pub seconds: f64,
}

impl GridStats {
    #[must_use]
    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::json!({
            "shape": self.shape, "dense": self.dense, "field_class": self.field_class, "mode": self.mode,
            "blocks": self.blocks, "pruned_blocks": self.pruned_blocks, "sampled": self.sampled,
            "sample_fraction": self.sample_fraction, "seconds": self.seconds,
        });
        if let (Some(d), Some(o)) = (self.dead_samples, m.as_object_mut()) {
            o.insert("dead_samples".into(), serde_json::Value::from(d));
        }
        m
    }
}


pub fn eval_grid(
    node: &NodeRef,
    bx: &SampleBox,
    opts: &EvalOptions,
    prune: bool,
    block: usize,
    allow_measured: bool,
) -> GResult<(Vec<f64>, GridStats)> {
    let mut opts = *opts;
    if opts.validate {
        validate_or_err(node)?;
        opts.validate = false;
    }
    let shape = bx.shape;
    let mut rec = GridStats { shape, dense: bx.n(), ..GridStats::default() };
    let fc = field_class_of(node, opts.mode)?;
    rec.field_class = fc.repr();
    let dense = |why: String, rec: &mut GridStats| -> GResult<Vec<f64>> {
        rec.mode = why;
        rec.blocks = 0;
        rec.pruned_blocks = 0;
        rec.sampled = rec.dense;
        rec.sample_fraction = 1.0;
        let t = Instant::now();
        let out = eval_points(node, &bx.points_mm(), &opts)?;
        rec.seconds = t.elapsed().as_secs_f64();
        Ok(out)
    };
    if !prune {
        let v = dense("dense (asked)".into(), &mut rec)?;
        return Ok((v, rec));
    }
    let Some(factor) = fc.safe_step_factor() else {
        let v = dense(format!("dense ({} carries no bound)", fc.kind().name()), &mut rec)?;
        return Ok((v, rec));
    };
    if fc.measured() && !allow_measured {
        let v = dense(
            format!(
                "dense (Lipschitz {} was MEASURED over {} samples; pass allow_measured=True to prune on it)",
                pyfmt::g(fc.k()),
                fc.samples()
            ),
            &mut rec,
        )?;
        return Ok((v, rec));
    }
    require(&fc, ClassKind::Lipschitz, "pruning a grid cell", true)?;
    let t0 = Instant::now();
    let b = block.max(2);
    let mut lo_i: [Vec<usize>; 3] = Default::default();
    let mut hi_i: [Vec<usize>; 3] = Default::default();
    for c in 0..3 {
        let n_ax = shape[c];
        let stop = (n_ax.saturating_sub(1)).max(1);
        let mut st: Vec<usize> = (0..stop).step_by(b).collect();
        if st.is_empty() {
            st.push(0);
        }
        hi_i[c] = st.iter().map(|l| (l + b).min(n_ax.saturating_sub(1))).collect();
        lo_i[c] = st;
    }
    let nb = [lo_i[0].len(), lo_i[1].len(), lo_i[2].len()];
    #[allow(clippy::cast_precision_loss)]
    let mid: [Vec<f64>; 3] = std::array::from_fn(|c| {
        lo_i[c].iter().zip(&hi_i[c]).map(|(l, h)| 0.5 * (*l as f64 + *h as f64)).collect()
    });
    #[allow(clippy::cast_precision_loss)]
    let span: [Vec<f64>; 3] = std::array::from_fn(|c| {
        lo_i[c].iter().zip(&hi_i[c]).map(|(l, h)| (*h as f64) - (*l as f64)).collect()
    });
    let mut idx = Vec::with_capacity(nb[0] * nb[1] * nb[2]);
    let mut diags = Vec::with_capacity(idx.capacity());
    let h_mm = bx.h * 1000.0;
    for i in 0..nb[0] {
        for j in 0..nb[1] {
            for k in 0..nb[2] {
                idx.push([mid[0][i], mid[1][j], mid[2][k]]);
                let (si, sj, sk) = (span[0][i], span[1][j], span[2][k]);
                diags.push(h_mm * (si.powi(2) + sj.powi(2) + sk.powi(2)).sqrt());
            }
        }
    }
    let centres = bx.points_at_mm(&idx);
    let fc_centre = eval_points(node, &centres, &opts)?;
    let free: Vec<bool> = fc_centre.iter().zip(&diags).map(|(f, d)| f.abs() * factor > 0.5 * d).collect();
    let fidx = |i: usize, j: usize, k: usize| (i * nb[1] + j) * nb[2] + k;
    let mut ca: [Vec<usize>; 3] = Default::default();
    let mut cb: [Vec<usize>; 3] = Default::default();
    for c in 0..3 {
        for a in 0..shape[c] {
            let first = (a / b).min(nb[c] - 1);
            let prev = first.saturating_sub(1);
            let inside_prev = a <= hi_i[c][prev];
            ca[c].push(first);
            cb[c].push(if inside_prev { prev } else { first });
        }
    }
    let n = bx.n();
    let mut out = vec![0.0; n];
    let mut live = Vec::new();
    let mut dead_count = 0usize;
    let mut flat = 0usize;
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let mut dead = true;
                'combo: for ai in [ca[0][i], cb[0][i]] {
                    for aj in [ca[1][j], cb[1][j]] {
                        for ak in [ca[2][k], cb[2][k]] {
                            if !free[fidx(ai, aj, ak)] {
                                dead = false;
                                break 'combo;
                            }
                        }
                    }
                }
                out[flat] = fc_centre[fidx(ca[0][i], ca[1][j], ca[2][k])];
                if dead {
                    dead_count += 1;
                } else {
                    #[allow(clippy::cast_precision_loss)]
                    live.push((flat, [i as f64, j as f64, k as f64]));
                }
                flat += 1;
            }
        }
    }
    if !live.is_empty() {
        let pts = bx.points_at_mm(&live.iter().map(|(_, p)| *p).collect::<Vec<_>>());
        let vals = eval_points(node, &pts, &opts)?;
        for ((f, _), v) in live.iter().zip(vals) {
            out[*f] = v;
        }
    }
    rec.mode = "pruned".into();
    rec.blocks = free.len();
    rec.pruned_blocks = free.iter().filter(|f| **f).count();
    rec.sampled = live.len() + free.len();
    rec.dead_samples = Some(dead_count);
    #[allow(clippy::cast_precision_loss)]
    {
        rec.sample_fraction = rec.sampled as f64 / rec.dense.max(1) as f64;
    }
    rec.seconds = t0.elapsed().as_secs_f64();
    Ok((out, rec))
}


#[allow(clippy::too_many_arguments)]
pub fn sphere_trace(
    node: &NodeRef,
    origins: &[[f64; 3]],
    dirs: &[[f64; 3]],
    tmax: f64,
    iters: usize,
    eps: f64,
    mode: Mode,
    allow_measured: bool,
) -> GResult<(Vec<f64>, Vec<bool>)> {
    let fc = field_class_of(node, mode)?;
    let Some(fac) = fc.safe_step_factor() else {
        return Err(GeometryError::BoundViolation(
            "sphere tracing needs a field that carries a distance bound; this one is IMPLICIT".into(),
        ));
    };
    if fc.measured() && !allow_measured {
        require(&fc, ClassKind::Lipschitz, "sphere tracing", false)?;
    }
    let d: Vec<[f64; 3]> = dirs
        .iter()
        .map(|v| {
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-30);
            [v[0] / n, v[1] / n, v[2] / n]
        })
        .collect();
    let mut t = vec![0.0; origins.len()];
    let mut hit = vec![false; origins.len()];
    let opts = EvalOptions { mode, ..EvalOptions::default() };
    let k = compile_f64(node, &opts)?;
    for _ in 0..iters {
        let p: Vec<[f64; 3]> = origins
            .iter()
            .zip(&d)
            .zip(&t)
            .map(|((o, dd), tt)| [o[0] + tt * dd[0], o[1] + tt * dd[1], o[2] + tt * dd[2]])
            .collect();
        let f = eval_kernel(&k, &p);
        STATS.calls.fetch_add(1, Ordering::Relaxed);
        for i in 0..t.len() {
            let step = f[i].abs() * fac;
            let new_hit = f[i].abs() < eps && !hit[i];
            hit[i] |= new_hit;
            if !hit[i] {
                t[i] = (t[i] + step.max(1e-9)).min(tmax);
            }
        }
        if hit.iter().zip(&t).all(|(h, tt)| *h || *tt >= tmax) {
            break;
        }
    }
    Ok((t, hit))
}

#[derive(Clone, Debug, PartialEq)]
pub struct GradStats {
    pub max: f64,
    pub mean: f64,
    pub p99: f64,
    pub p50: f64,
    pub samples: usize,
}


pub fn gradient_statistics(node: &NodeRef, pts: &[[f64; 3]], mode: Mode) -> GResult<GradStats> {
    let g = grad_x(node, pts, &EvalOptions { mode, ..EvalOptions::default() })?;
    let mags: Vec<f64> = g.iter().map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()).collect();
    Ok(GradStats {
        max: crate::numpy::max(&mags).unwrap_or(f64::NAN),
        mean: crate::numpy::mean(&mags),
        p99: crate::numpy::percentile(&mags, 99.0).unwrap_or(f64::NAN),
        p50: crate::numpy::percentile(&mags, 50.0).unwrap_or(f64::NAN),
        samples: mags.len(),
    })
}
