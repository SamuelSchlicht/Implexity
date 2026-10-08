// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Map, Value, json};

use crate::convergence::{PerFieldCriterion, criterion_from_policy_payload, field_members_from_slices};
use crate::local_assembly::Kind;
use crate::matrix::{Jacobian, eliminate_zeros};
use crate::native_history::{
    HistoryAdjoint, HistoryOptions, HistoryProblem, HistorySolution, HistorySolveOptions, NativeHistorySystem,
};
use crate::operation_context::OperationExecutionContext;

pub trait HistoryBlockCallbacks: Send + Sync {


    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>>;


    fn current_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian>;


    fn previous_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian>;


    fn design_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian>;


    fn check(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<()> {
        Ok(())
    }


    fn current_action(
        &self,
        _n: usize,
        _z: &[f64],
        _old: &[f64],
        _x: &[f64],
        _v: &[f64],
        _transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        None
    }


    fn initial_jacobian(&self, _x: &[f64]) -> Option<CaeResult<Jacobian>> {
        None
    }
}

#[derive(Clone)]
pub struct HistoryBlock {
    pub name: String,
    pub initial: Vec<f64>,
    pub design_indices: Vec<usize>,
    pub callbacks: Arc<dyn HistoryBlockCallbacks>,
    pub field: Option<String>,
}

pub trait HistoryInterface: Send + Sync {


    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>>;


    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian>;


    fn current_action(
        &self,
        _n: usize,
        _z: &[f64],
        _old: &[f64],
        _x: &[f64],
        _v: &[f64],
        _transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        None
    }
}

struct Core {
    blocks: Vec<HistoryBlock>,
    slices: Vec<(usize, usize)>,
    interfaces: RwLock<Vec<Arc<dyn HistoryInterface>>>,
    state_size: usize,
    design_size: usize,
}

fn block_csr(j: &Jacobian) -> CaeResult<std::borrow::Cow<'_, CsrMatrix>> {
    use std::borrow::Cow;
    match j {
        Jacobian::Csr(m) => Ok(Cow::Borrowed(m)),
        Jacobian::Dense(m) => {
            let mut ptr = Vec::with_capacity(m.nrows + 1);
            ptr.push(0);
            let mut idx = Vec::new();
            let mut data = Vec::new();
            for i in 0..m.nrows {
                for k in 0..m.ncols {
                    let x = m.data[i * m.ncols + k];
                    if x != 0.0 {
                        idx.push(k);
                        data.push(x);
                    }
                }
                ptr.push(idx.len());
            }
            CsrMatrix::try_new(m.nrows, m.ncols, ptr, idx, data)
                .map(Cow::Owned)
                .map_err(|e| CaeError::contract(e.to_string()))
        }
        Jacobian::Operator(_) => Err(CaeError::contract("coupled history block partials must be assembled")),
        other @ Jacobian::Csc(_) => other.to_csr().map(Cow::Owned),
    }
}

impl Core {
    fn inputs(&self, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<()> {
        for (a, size) in [(z, self.state_size), (old, self.state_size), (x, self.design_size)] {
            if a.len() != size || a.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("coupled history input shape/nonfinite error"));
            }
        }
        Ok(())
    }

    fn local(x: &[f64], idx: &[usize]) -> Vec<f64> {
        idx.iter().map(|&i| x[i]).collect()
    }

    fn block_partials(
        &self,
        kind: Kind,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> CaeResult<CsrMatrix> {
        let width = if kind == Kind::Design { self.design_size } else { self.state_size };
        let mut ptr = Vec::with_capacity(self.state_size + 1);
        ptr.push(0);
        let mut idx = Vec::new();
        let mut data = Vec::new();
        let mut pairs: Vec<(usize, f64)> = Vec::new();
        for (b, &(lo, hi)) in self.blocks.iter().zip(&self.slices) {
            let xx = Self::local(x, &b.design_indices);
            let j = match kind {
                Kind::Current => b.callbacks.current_jacobian(n, &z[lo..hi], &old[lo..hi], &xx)?,
                Kind::Previous => b.callbacks.previous_jacobian(n, &z[lo..hi], &old[lo..hi], &xx)?,
                Kind::Design => b.callbacks.design_jacobian(n, &z[lo..hi], &old[lo..hi], &xx)?,
            };
            let shape = (hi - lo, if kind == Kind::Design { b.design_indices.len() } else { hi - lo });
            let m = block_csr(&j)?;
            if j.shape() != shape || m.data().iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract(format!("{}: invalid block partial", b.name)));
            }
            idx.reserve(m.nnz());
            data.reserve(m.nnz());
            let (mp, mi, mv) = (m.indptr(), m.indices(), m.data());
            for r in 0..hi - lo {
                let (cols, vals) = (&mi[mp[r]..mp[r + 1]], &mv[mp[r]..mp[r + 1]]);
                if kind == Kind::Design {
                    pairs.clear();
                    pairs.extend(cols.iter().zip(vals).map(|(&c, &v)| (b.design_indices[c], v)));
                    pairs.sort_by_key(|&(c, _)| c);
                    let mut k = 0;
                    while k < pairs.len() {
                        let c = pairs[k].0;
                        let mut acc = 0.0;
                        while k < pairs.len() && pairs[k].0 == c {
                            acc += pairs[k].1;
                            k += 1;
                        }
                        idx.push(c);
                        data.push(acc);
                    }
                } else {
                    idx.extend(cols.iter().map(|&c| lo + c));

                    data.extend(vals.iter().map(|&v| {
                        let mut acc = 0.0;
                        acc += v;
                        acc
                    }));
                }
                ptr.push(idx.len());
            }
        }
        CsrMatrix::try_new(self.state_size, width, ptr, idx, data)
            .map_err(|e| CaeError::contract(e.to_string()))
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        self.inputs(z, old, x)?;
        let mut out = self.block_partials(kind, n, z, old, x)?;
        for c in self.interfaces.read().map_err(|_| CaeError::contract("coupled interfaces poisoned"))?.iter()
        {
            let a = c.jacobian(kind, n, z, old, x)?;
            let a = a.to_csr()?;
            if a.shape() != out.shape() || !a.is_finite() {
                return Err(CaeError::contract("invalid coupled interface partial"));
            }
            out = out.add_scaled(1.0, &a, 1.0).map_err(|e| CaeError::contract(e.to_string()))?;
        }
        Ok(eliminate_zeros(&out))
    }
}

impl HistoryProblem for Core {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.inputs(z, old, x)?;
        let mut out = vec![0.0; self.state_size];
        for (b, &(lo, hi)) in self.blocks.iter().zip(&self.slices) {
            let xx = Self::local(x, &b.design_indices);
            b.callbacks.check(n, &z[lo..hi], &old[lo..hi], &xx)?;
            let r = b.callbacks.residual(n, &z[lo..hi], &old[lo..hi], &xx)?;
            if r.len() != hi - lo || r.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract(format!("{}: invalid block residual", b.name)));
            }
            for (o, v) in out[lo..hi].iter_mut().zip(r) {
                *o += v;
            }
        }
        for c in self.interfaces.read().map_err(|_| CaeError::contract("coupled interfaces poisoned"))?.iter()
        {
            let r = c.residual(n, z, old, x)?;
            if r.len() != self.state_size || r.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("invalid coupled interface residual"));
            }
            for (o, v) in out.iter_mut().zip(r) {
                *o += v;
            }
        }
        Ok(out)
    }
    fn state_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.jacobian(Kind::Current, n, z, old, x)?))
    }
    fn previous_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.jacobian(Kind::Previous, n, z, old, x)?))
    }
    fn design_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.jacobian(Kind::Design, n, z, old, x)?))
    }
}

pub struct CoupledHistoryAssembly {
    core: Arc<Core>,
    initial: Vec<f64>,
    field_slices: BTreeMap<String, Vec<(usize, usize)>>,
    field_order: Vec<String>,
    criterion: Option<Arc<PerFieldCriterion>>,
    system: NativeHistorySystem,
}

impl std::fmt::Debug for CoupledHistoryAssembly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoupledHistoryAssembly").field("report", &self.report()).finish()
    }
}

impl CoupledHistoryAssembly {


    pub fn new(
        blocks: Vec<HistoryBlock>,
        design_size: usize,
        tolerance: f64,
        max_iterations: usize,
        coupled_convergence_policy: Option<&Value>,
    ) -> CaeResult<Self> {
        if design_size < 1 {
            return Err(CaeError::contract("positive integer coupled design size required"));
        }
        if blocks.is_empty() {
            return Err(CaeError::contract("coupled history needs at least one block"));
        }
        let mut size = 0;
        let mut slices = Vec::new();
        let mut initial = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        let mut field_slices: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
        let mut field_order: Vec<String> = Vec::new();
        for b in &blocks {
            if b.name.is_empty() || !names.insert(b.name.clone()) {
                return Err(CaeError::contract("history block identities must be unique"));
            }
            if b.initial.is_empty() || b.initial.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract(format!("{}: invalid initial state", b.name)));
            }
            if b.design_indices.is_empty() || b.design_indices.iter().any(|&i| i >= design_size) {
                return Err(CaeError::contract(format!("{}: invalid design incidence", b.name)));
            }
            let slice = (size, size + b.initial.len());
            slices.push(slice);
            if let Some(label) = &b.field {
                if label.is_empty() {
                    return Err(CaeError::contract(format!(
                        "{}: history block field label must be a non-empty string",
                        b.name
                    )));
                }
                if !field_slices.contains_key(label) {
                    field_order.push(label.clone());
                }
                field_slices.entry(label.clone()).or_default().push(slice);
            }
            size += b.initial.len();
            initial.extend_from_slice(&b.initial);
        }
        let criterion = match coupled_convergence_policy {
            None => None,
            Some(policy) => {
                if field_slices.is_empty() {
                    return Err(CaeError::contract(
                        "coupled convergence policy requires at least one field-labelled HistoryBlock",
                    ));
                }
                let unlabelled: Vec<String> =
                    blocks.iter().filter(|b| b.field.is_none()).map(|b| b.name.clone()).collect();
                if !unlabelled.is_empty() {
                    return Err(CaeError::contract(format!(
                        "coupled convergence policy requires every HistoryBlock to carry a field label; unlabelled={}",
                        crate::convergence::repr_name_list(&unlabelled)
                    )));
                }
                let ordered: Vec<(String, Vec<(usize, usize)>)> =
                    field_order.iter().map(|l| (l.clone(), field_slices[l].clone())).collect();
                let members = field_members_from_slices(&ordered, size)?;
                Some(Arc::new(criterion_from_policy_payload(policy, &members, size)?))
            }
        };
        let core = Arc::new(Core {
            blocks,
            slices,
            interfaces: RwLock::new(Vec::new()),
            state_size: size,
            design_size,
        });
        let problem: Arc<dyn HistoryProblem> = core.clone();
        let system = NativeHistorySystem::new(
            problem,
            HistoryOptions {
                tolerance,
                max_iterations,
                criterion: criterion.clone().map(|c| c as crate::convergence::Criterion),
                ..HistoryOptions::default()
            },
        )?;
        Ok(Self { core, initial, field_slices, field_order, criterion, system })
    }

    #[must_use]
    pub fn system(&self) -> &NativeHistorySystem {
        &self.system
    }
    #[must_use]
    pub fn initial(&self) -> &[f64] {
        &self.initial
    }
    #[must_use]
    pub fn state_size(&self) -> usize {
        self.core.state_size
    }
    #[must_use]
    pub fn design_size(&self) -> usize {
        self.core.design_size
    }
    #[must_use]
    pub fn slice(&self, name: &str) -> Option<(usize, usize)> {
        self.core.blocks.iter().position(|b| b.name == name).map(|i| self.core.slices[i])
    }



    pub fn add_interface(&self, component: Arc<dyn HistoryInterface>) -> CaeResult<()> {
        self.core
            .interfaces
            .write()
            .map_err(|_| CaeError::contract("coupled interfaces poisoned"))?
            .push(component);
        Ok(())
    }



    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.residual(n, z, old, x)
    }



    pub fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        self.core.jacobian(kind, n, z, old, x)
    }



    pub fn initial_design_jacobian(&self, design: &[f64]) -> CaeResult<CsrMatrix> {
        if design.len() != self.core.design_size || design.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("initial Jacobian requires finite exact-shaped real design"));
        }
        let mut rr = Vec::new();
        let mut cc = Vec::new();
        let mut vv = Vec::new();
        for (b, &(lo, hi)) in self.core.blocks.iter().zip(&self.core.slices) {
            let Some(j) = b.callbacks.initial_jacobian(&Core::local(design, &b.design_indices)) else {
                continue;
            };
            let j = j?;
            let m = block_csr(&j)?;
            if j.shape() != (hi - lo, b.design_indices.len()) || m.data().iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract(format!("{}: invalid initial design Jacobian", b.name)));
            }
            for r in 0..m.nrows() {
                let range = m.indptr()[r]..m.indptr()[r + 1];
                rr.extend(std::iter::repeat_n(lo + r, range.len()));
                cc.extend(m.indices()[range.clone()].iter().map(|&k| b.design_indices[k]));
                vv.extend_from_slice(&m.data()[range]);
            }
        }
        let m = CsrMatrix::from_triplets(self.core.state_size, self.core.design_size, &rr, &cc, &vv)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(eliminate_zeros(&m))
    }



    pub fn solve(
        &self,
        design: &[f64],
        steps: usize,
        options: &HistorySolveOptions<'_>,
    ) -> CaeResult<HistorySolution> {
        self.system.solve(design, &self.initial, steps, options)
    }



    pub fn adjoint_many(
        &self,
        design: &[f64],
        solution: &HistorySolution,
        gu: &[DenseMatrix],
        grad: &DenseMatrix,
        execution_context: Option<&OperationExecutionContext>,
    ) -> CaeResult<HistoryAdjoint> {
        let initial = self.initial_design_jacobian(design)?;
        self.system.adjoint_many(design, solution, gu, grad, Some(Jacobian::Csr(initial)), execution_context)
    }



    pub fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        if v.len() != self.core.state_size || v.iter().any(|x| !x.is_finite()) {
            return Err(CaeError::contract("coupled current-action operand is invalid"));
        }
        self.core.inputs(z, old, x)?;
        let mut out = vec![0.0; self.core.state_size];
        for (b, &(lo, hi)) in self.core.blocks.iter().zip(&self.core.slices) {
            let xx = Core::local(x, &b.design_indices);
            b.callbacks.check(n, &z[lo..hi], &old[lo..hi], &xx)?;
            let action =
                match b.callbacks.current_action(n, &z[lo..hi], &old[lo..hi], &xx, &v[lo..hi], transpose) {
                    Some(a) => a?,
                    None => b
                        .callbacks
                        .current_jacobian(n, &z[lo..hi], &old[lo..hi], &xx)?
                        .apply(&v[lo..hi], transpose)?,
                };
            if action.len() != hi - lo || action.iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract(format!("{}: invalid current derivative action", b.name)));
            }
            for (o, a) in out[lo..hi].iter_mut().zip(action) {
                *o += a;
            }
        }
        for c in
            self.core.interfaces.read().map_err(|_| CaeError::contract("coupled interfaces poisoned"))?.iter()
        {
            let action = match c.current_action(n, z, old, x, v, transpose) {
                Some(a) => a?,
                None => c.jacobian(Kind::Current, n, z, old, x)?.apply(v, transpose)?,
            };
            if action.len() != out.len() || action.iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract("invalid coupled interface derivative action"));
            }
            for (o, a) in out.iter_mut().zip(action) {
                *o += a;
            }
        }
        Ok(out)
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let blocks: Map<String, Value> = self
            .core
            .blocks
            .iter()
            .zip(&self.core.slices)
            .map(|(b, (lo, hi))| (b.name.clone(), json!([lo, hi])))
            .collect();
        let interfaces = self.core.interfaces.read().map_or(0, |i| i.len());
        let mut out = json!({
            "state_unknowns": self.core.state_size,
            "design_entries": self.core.design_size,
            "state_blocks": blocks,
            "interface_count": interfaces,
            "coupling": "same_newton_state_and_all_history_adjoint",
            "block_solvers_called_independently": false,
            "global_dense_jacobian_allocated": false,
        });
        if let (Some(c), Value::Object(map)) = (&self.criterion, &mut out) {
            use crate::convergence::ConvergenceCriterion as _;
            map.insert("convergence_criterion".into(), c.describe());
            let fields: Map<String, Value> = self
                .field_order
                .iter()
                .map(|l| {
                    (l.clone(), json!(self.field_slices[l].iter().map(|(a, b)| [a, b]).collect::<Vec<_>>()))
                })
                .collect();
            map.insert("field_slices".into(), Value::Object(fields));
        }
        out
    }
}

