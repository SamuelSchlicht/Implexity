// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::error::LinalgError;
use implexity_linalg::krylov::{GcrotOptions, RecyclePair, Truncate, gcrotmk};
use implexity_linalg::operator::{FnOperator, Identity};
use serde_json::{Value, json};

use crate::block_preconditioning::{BlockPreconditionerDiagnostics, ProviderNativePreconditionerFactory};
use crate::certificate::norm2;
use crate::differentiable::{
    Argument, DifferentiableResidual, DifferentiableResponse, gradient, response_value, vjp,
};
use crate::hybrid_linearization::{
    HybridLinearisationPolicy, HybridResidualSolver, LinearBackend, LinearSolveDiagnostics,
};
use crate::pyfmt::fmt_e;
use crate::sparse_block_residual::StateJacobianFn;

pub const COMPATIBILITY_ONLY: bool = true;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecyclingLinearisationPolicy {
    pub base: HybridLinearisationPolicy,
    pub gcrot_inner_dimension: usize,
    pub recycle_dimension: usize,
    pub recycle_design_drift_limit: f64,
    pub reuse_previous_solution: bool,
    pub adaptive_recycling: bool,
    pub recycle_minimum_operator_applications: usize,
}

impl Default for RecyclingLinearisationPolicy {
    fn default() -> Self {
        Self {
            base: HybridLinearisationPolicy { krylov_maxiter: 200, ..HybridLinearisationPolicy::default() },
            gcrot_inner_dimension: 30,
            recycle_dimension: 12,
            recycle_design_drift_limit: 0.35,
            reuse_previous_solution: true,
            adaptive_recycling: true,
            recycle_minimum_operator_applications: 8,
        }
    }
}

impl RecyclingLinearisationPolicy {


    pub fn validated(mut self) -> CaeResult<Self> {
        self.base.validate()?;
        self.gcrot_inner_dimension = self.gcrot_inner_dimension.max(2);
        if !self.recycle_design_drift_limit.is_finite() || self.recycle_design_drift_limit < 0.0 {
            return Err(CaeError::contract("recycle_design_drift_limit must be finite and non-negative"));
        }
        Ok(self)
    }
}

struct RecycleEntry {
    dimension: usize,
    design: Vec<f64>,
    cu: Vec<RecyclePair>,
    previous_solution: Option<Vec<f64>>,
    uses: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecyclePreparation {
    pub key: String,
    pub vectors_in: usize,
    pub reset: bool,
    pub reset_reason: Option<&'static str>,
    pub relative_design_drift: f64,
    pub previous_solution_used: bool,
}

#[derive(Default)]
pub struct KrylovRecyclePool {
    entries: BTreeMap<String, RecycleEntry>,
}

impl std::fmt::Debug for KrylovRecyclePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KrylovRecyclePool").field("snapshot", &self.snapshot()).finish()
    }
}

fn drift(previous: &[f64], current: &[f64]) -> f64 {
    if previous.len() != current.len() {
        return f64::INFINITY;
    }
    let d: Vec<f64> = current.iter().zip(previous).map(|(a, b)| a - b).collect();
    norm2(&d) / norm2(previous).max(norm2(current)).max(1.0)
}

impl KrylovRecyclePool {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn prepare(
        &mut self,
        key: &str,
        dimension: usize,
        design: &[f64],
        drift_limit: f64,
        reuse_previous_solution: bool,
    ) -> (Vec<RecyclePair>, Option<Vec<f64>>, RecyclePreparation) {
        let current = design.to_vec();
        let mut reset = false;
        let mut reason = None;
        let mut d = f64::INFINITY;
        let fresh = |current: &Vec<f64>| RecycleEntry {
            dimension,
            design: current.clone(),
            cu: Vec::new(),
            previous_solution: None,
            uses: 0,
        };
        match self.entries.get_mut(key) {
            None => {
                self.entries.insert(key.to_string(), fresh(&current));
                reset = true;
                reason = Some("new_space");
            }
            Some(e) if e.dimension != dimension => {
                *e = fresh(&current);
                reset = true;
                reason = Some("dimension_changed");
            }
            Some(e) => {
                d = drift(&e.design, &current);
                if d > drift_limit {
                    e.cu.clear();
                    e.previous_solution = None;
                    reset = true;
                    reason = Some("design_drift_exceeded");
                }
            }
        }
        if !d.is_finite() {
            d = if reason == Some("new_space") { 0.0 } else { f64::INFINITY };
        }
        let Some(entry) = self.entries.get_mut(key) else {
            return (
                Vec::new(),
                None,
                RecyclePreparation {
                    key: key.into(),
                    vectors_in: 0,
                    reset,
                    reset_reason: reason,
                    relative_design_drift: d,
                    previous_solution_used: false,
                },
            );
        };
        entry.cu = entry
            .cu
            .iter()
            .filter(|p| p.u.len() == dimension && p.u.iter().all(|v| v.is_finite()))
            .map(|p| RecyclePair { c: None, u: p.u.clone() })
            .collect();
        let x0 = if reuse_previous_solution {
            entry
                .previous_solution
                .clone()
                .filter(|s| s.len() == dimension && s.iter().all(|v| v.is_finite()))
        } else {
            None
        };
        entry.design = current;
        let prep = RecyclePreparation {
            key: key.to_string(),
            vectors_in: entry.cu.len(),
            reset,
            reset_reason: reason,
            relative_design_drift: d,
            previous_solution_used: x0.is_some(),
        };
        (entry.cu.clone(), x0, prep)
    }

    pub fn finalise(
        &mut self,
        key: &str,
        design: &[f64],
        solution: &[f64],
        cu: Vec<RecyclePair>,
        maximum_vectors: usize,
    ) -> usize {
        let Some(entry) = self.entries.get_mut(key) else { return 0 };
        entry.design = design.to_vec();
        entry.previous_solution = Some(solution.to_vec());
        let mut cu = cu;
        if maximum_vectors == 0 {
            cu.clear();
        } else if cu.len() > maximum_vectors {
            let excess = cu.len() - maximum_vectors;
            cu.drain(..excess);
        }
        entry.cu = cu.into_iter().map(|p| RecyclePair { c: None, u: p.u }).collect();
        entry.uses += 1;
        entry.cu.len()
    }

    pub fn clear(&mut self, key: Option<&str>) {
        match key {
            None => self.entries.clear(),
            Some(k) => {
                self.entries.remove(k);
            }
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Value {
        Value::Object(
            self.entries
                .iter()
                .map(|(k, e)| {
                    (
                        k.clone(),
                        json!({"dimension": e.dimension, "recycled_vectors": e.cu.len(), "uses": e.uses}),
                    )
                })
                .collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Stage40LinearSolveDiagnostics {
    pub requested_backend: &'static str,
    pub used_backend: &'static str,
    pub converged: bool,
    pub relative_residual: f64,
    pub iterations: usize,
    pub fallback_used: bool,
    pub krylov_method: &'static str,
    pub operator_applications: usize,
    pub preconditioner_applications: usize,
    pub recycled_vectors_in: usize,
    pub recycled_vectors_out: usize,
    pub recycle_reset: bool,
    pub recycle_reset_reason: Option<&'static str>,
    pub relative_design_drift: f64,
    pub previous_solution_used: bool,
    pub preconditioner_provider: Option<String>,
    pub preconditioner_strategy: Option<String>,
    pub preconditioner_transpose_duality_error: Option<f64>,
    pub recycling_suppressed: bool,
    pub recycling_suppression_reason: Option<&'static str>,
    pub initial_guess_relative_residual: Option<f64>,
    pub initial_guess_accepted_without_iteration: bool,
    pub recycle_initial_guess_rejected: bool,
    pub topology_coordinate: &'static str,
}

impl Stage40LinearSolveDiagnostics {
    fn direct(base: &LinearSolveDiagnostics, requested: LinearBackend) -> Self {
        Self {
            requested_backend: requested.as_str(),
            used_backend: base.used_backend,
            converged: base.converged,
            relative_residual: base.relative_residual,
            iterations: base.iterations,
            fallback_used: base.fallback_used,
            krylov_method: "none",
            operator_applications: 1,
            preconditioner_applications: 0,
            recycled_vectors_in: 0,
            recycled_vectors_out: 0,
            recycle_reset: false,
            recycle_reset_reason: None,
            relative_design_drift: 0.0,
            previous_solution_used: false,
            preconditioner_provider: None,
            preconditioner_strategy: None,
            preconditioner_transpose_duality_error: None,
            recycling_suppressed: false,
            recycling_suppression_reason: None,
            initial_guess_relative_residual: None,
            initial_guess_accepted_without_iteration: false,
            recycle_initial_guess_rejected: false,
            topology_coordinate: TOPOLOGY_COORDINATE,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecyclingSensitivity {
    pub value: f64,
    pub gradient: Vec<f64>,
    pub state: Vec<f64>,
    pub adjoint: Vec<f64>,
    pub primal_relative_residual: f64,
    pub adjoint_relative_residual: f64,
    pub primal_backend: &'static str,
    pub adjoint_backend: &'static str,
    pub fallback_used: bool,
    pub admissible_for_optimization: bool,
    pub primal_linear_diagnostics: Option<Stage40LinearSolveDiagnostics>,
    pub adjoint_linear_diagnostics: Stage40LinearSolveDiagnostics,
    pub topology_coordinate: &'static str,
}

pub type StateAction = Box<dyn Fn(&[f64], &[f64], &[f64]) -> CaeResult<Vec<f64>> + Send + Sync>;

pub struct RecyclingHybridResidualSolver<R: DifferentiableResidual> {
    hybrid: HybridResidualSolver<R>,
    policy: RecyclingLinearisationPolicy,
    preconditioner_factory: Option<ProviderNativePreconditionerFactory>,
    state_jvp: Option<StateAction>,
    state_vjp: Option<StateAction>,
    pool: std::sync::Mutex<KrylovRecyclePool>,
    namespace: String,
}

impl<R: DifferentiableResidual> RecyclingHybridResidualSolver<R> {


    #[allow(clippy::too_many_arguments)]
    pub fn new(
        residual: R,
        jacobian_state: Option<StateJacobianFn>,
        preconditioner_factory: Option<ProviderNativePreconditionerFactory>,
        state_jvp: Option<StateAction>,
        state_vjp: Option<StateAction>,
        policy: RecyclingLinearisationPolicy,
        recycle_pool: Option<KrylovRecyclePool>,
        recycle_namespace: &str,
        nonlinear_tolerance: f64,
        maximum_newton_iterations: usize,
        minimum_step: f64,
    ) -> CaeResult<Self> {
        let policy = policy.validated()?;
        crate::hybrid_linearization::validate_newton_parameters(nonlinear_tolerance, minimum_step)?;
        Ok(Self {
            hybrid: HybridResidualSolver::new(
                residual,
                jacobian_state,
                None,
                policy.base,
                nonlinear_tolerance,
                maximum_newton_iterations,
                minimum_step,
            ),
            policy,
            preconditioner_factory,
            state_jvp,
            state_vjp,
            pool: std::sync::Mutex::new(recycle_pool.unwrap_or_default()),
            namespace: recycle_namespace.to_string(),
        })
    }

    #[must_use]
    pub fn pool_snapshot(&self) -> Value {
        self.pool.lock().map_or(Value::Null, |p| p.snapshot())
    }

    fn action(&self, u: &[f64], x: &[f64], v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        match (transpose, &self.state_vjp, &self.state_jvp) {
            (true, Some(f), _) | (false, _, Some(f)) => f(u, x, v),
            _ => self.hybrid.matrix_free_action(u, x, v, transpose),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sparse_fallback(
        &self,
        u: &[f64],
        x: &[f64],
        rhs: &[f64],
        transpose: bool,
        requested: LinearBackend,
        ops: usize,
        pre_ops: usize,
        prep: &RecyclePreparation,
        vectors_out: usize,
        pre_diag: Option<&BlockPreconditionerDiagnostics>,
    ) -> CaeResult<(Vec<f64>, Stage40LinearSolveDiagnostics)> {
        let (sol, rel) = self
            .hybrid
            .sparse_solve(u, x, rhs, transpose)
            .map_err(|_| CaeError::convergence("Stage-40 sparse fallback failed"))?;
        Ok((
            sol,
            Stage40LinearSolveDiagnostics {
                requested_backend: requested.as_str(),
                used_backend: LinearBackend::SparseDirect.as_str(),
                converged: rel.is_finite(),
                relative_residual: rel,
                iterations: 1,
                fallback_used: true,
                krylov_method: "gcrotmk_to_sparse_direct",
                operator_applications: ops + 1,
                preconditioner_applications: pre_ops,
                recycled_vectors_in: prep.vectors_in,
                recycled_vectors_out: vectors_out,
                recycle_reset: prep.reset,
                recycle_reset_reason: prep.reset_reason,
                relative_design_drift: prep.relative_design_drift,
                previous_solution_used: prep.previous_solution_used,
                preconditioner_provider: pre_diag.map(|d| d.provider_name.clone()),
                preconditioner_strategy: pre_diag.map(|d| d.strategy.clone()),
                preconditioner_transpose_duality_error: pre_diag.map(|d| d.transpose_duality_error),
                recycling_suppressed: false,
                recycling_suppression_reason: None,
                initial_guess_relative_residual: None,
                initial_guess_accepted_without_iteration: false,
                recycle_initial_guess_rejected: false,
                topology_coordinate: TOPOLOGY_COORDINATE,
            },
        ))
    }



    #[allow(clippy::too_many_lines)]
    pub fn linear_solve(
        &self,
        u: &[f64],
        x: &[f64],
        rhs: &[f64],
        transpose: bool,
        backend: Option<LinearBackend>,
        allow_fallback: bool,
    ) -> CaeResult<(Vec<f64>, Stage40LinearSolveDiagnostics)> {
        let n = rhs.len();
        let chosen = backend.unwrap_or_else(|| self.policy.base.choose(n, self.hybrid.has_sparse()));
        if chosen != LinearBackend::MatrixFree {
            let (sol, base) = self.hybrid.linear_solve(u, x, rhs, transpose, Some(chosen), allow_fallback)?;
            return Ok((sol, Stage40LinearSolveDiagnostics::direct(&base, chosen)));
        }
        let role = if transpose { "adjoint" } else { "primal" };
        let key = format!("{}:{role}:{n}", self.namespace);
        let (mut cu, mut x0, prep) = {
            let mut pool = self.pool.lock().map_err(|_| CaeError::contract("recycle pool poisoned"))?;
            pool.prepare(
                &key,
                n,
                x,
                self.policy.recycle_design_drift_limit,
                self.policy.reuse_previous_solution,
            )
        };
        let ops = std::cell::Cell::new(0usize);
        let pre_ops = std::cell::Cell::new(0usize);
        let mut initial_relative = None;
        let mut initial_rejected = false;
        if let Some(guess) = x0.clone() {
            let ag = self.action(u, x, &guess, transpose)?;
            let res: Vec<f64> = ag.iter().zip(rhs).map(|(a, b)| a - b).collect();
            let rhs_norm = norm2(rhs);
            let init_norm = norm2(&res);
            let rel = init_norm / rhs_norm.max(1.0);
            initial_relative = Some(rel);
            let threshold =
                (self.policy.base.krylov_rtol * rhs_norm).max(10.0 * f64::EPSILON * rhs_norm.max(1.0));
            if init_norm.is_finite() && init_norm <= threshold {
                let out = {
                    let mut pool =
                        self.pool.lock().map_err(|_| CaeError::contract("recycle pool poisoned"))?;
                    pool.finalise(&key, x, &guess, cu, self.policy.recycle_dimension)
                };
                return Ok((
                    guess,
                    Stage40LinearSolveDiagnostics {
                        krylov_method: "recycled_solution_residual_acceptance",
                        converged: true,
                        relative_residual: rel,
                        iterations: 0,
                        operator_applications: 1,
                        recycled_vectors_in: prep.vectors_in,
                        recycled_vectors_out: out,
                        recycle_reset: prep.reset,
                        recycle_reset_reason: prep.reset_reason,
                        relative_design_drift: prep.relative_design_drift,
                        previous_solution_used: true,
                        initial_guess_relative_residual: Some(rel),
                        initial_guess_accepted_without_iteration: true,
                        ..Stage40LinearSolveDiagnostics::direct(
                            &LinearSolveDiagnostics {
                                requested_backend: chosen.as_str(),
                                used_backend: chosen.as_str(),
                                converged: true,
                                relative_residual: rel,
                                iterations: 0,
                                fallback_used: false,
                                topology_coordinate: TOPOLOGY_COORDINATE,
                            },
                            chosen,
                        )
                    },
                ));
            }
            if !init_norm.is_finite() || init_norm > rhs_norm.max(f64::MIN_POSITIVE) {
                x0 = None;
                initial_rejected = true;
            }
        }
        let built = match &self.preconditioner_factory {
            None => None,
            Some(f) => match f.build(u, x).and_then(|b| {
                if b.size() == n {
                    Ok(b)
                } else {
                    Err(CaeError::contract(format!(
                        "provider preconditioner has size {}; system has {n}",
                        b.size()
                    )))
                }
            }) {
                Ok(b) => Some(b),
                Err(e) => {
                    if !allow_fallback {
                        return Err(CaeError::convergence(format!(
                            "provider preconditioner construction failed: {}",
                            e.message()
                        )));
                    }
                    return self.sparse_fallback(
                        u,
                        x,
                        rhs,
                        transpose,
                        chosen,
                        ops.get(),
                        pre_ops.get(),
                        &prep,
                        cu.len(),
                        None,
                    );
                }
            },
        };
        let pre_diag =
            built.as_ref().map(crate::block_preconditioning::BlockSchurPreconditioner::diagnostics);
        let op = FnOperator::new(n, |v: &[f64], y: &mut [f64]| {
            ops.set(ops.get() + 1);
            let r = self.action(u, x, v, transpose).map_err(|e| LinalgError::Operator(e.to_string()))?;
            if r.len() != y.len() {
                return Err(LinalgError::Shape("state action returned another length".into()));
            }
            y.copy_from_slice(&r);
            Ok(())
        });
        let inner = self.policy.gcrot_inner_dimension.min(n.max(2));
        let recycle = self.policy.recycle_dimension.min(inner).min(n.saturating_sub(1));
        let opts = GcrotOptions {
            rtol: self.policy.base.krylov_rtol,
            atol: 0.0,
            maxiter: self.policy.base.krylov_maxiter,
            m: inner,
            k: Some(recycle),
            discard_c: true,
            truncate: Truncate::Oldest,
        };
        let outcome = match &built {
            Some(p) => {
                let m = FnOperator::new(n, |v: &[f64], y: &mut [f64]| {
                    pre_ops.set(pre_ops.get() + 1);
                    let r = p.apply(v, transpose).map_err(|e| LinalgError::Operator(e.to_string()))?;
                    if r.len() != y.len() {
                        return Err(LinalgError::Shape("preconditioner returned another length".into()));
                    }
                    y.copy_from_slice(&r);
                    Ok(())
                });
                gcrotmk(&op, rhs, x0.as_deref(), Some(&m), &opts, &mut cu)
            }
            None => gcrotmk(&op, rhs, x0.as_deref(), None::<&Identity>, &opts, &mut cu),
        };
        let (solution, info, iterations, rel) = match outcome {
            Ok(r) => {
                let ax = self.action(u, x, &r.x, transpose)?;
                let res: Vec<f64> = ax.iter().zip(rhs).map(|(a, b)| a - b).collect();
                let rel = norm2(&res) / norm2(rhs).max(1.0);
                (r.x, i64::try_from(r.info).unwrap_or(i64::MAX), r.iterations, rel)
            }
            Err(e) => {
                if !allow_fallback {
                    return Err(CaeError::convergence(format!(
                        "Stage-40 matrix-free GCROT solve failed: {e}"
                    )));
                }
                (vec![0.0; n], -999, 0, f64::INFINITY)
            }
        };
        let converged = info == 0 && rel.is_finite() && solution.iter().all(|v| v.is_finite());
        let suppress = converged
            && self.policy.adaptive_recycling
            && ops.get() <= self.policy.recycle_minimum_operator_applications;
        let vectors_out = if converged {
            let mut pool = self.pool.lock().map_err(|_| CaeError::contract("recycle pool poisoned"))?;
            pool.finalise(&key, x, &solution, cu, if suppress { 0 } else { self.policy.recycle_dimension })
        } else {
            cu.len()
        };
        if !converged {
            if !allow_fallback {
                return Err(CaeError::convergence(format!(
                    "matrix-free GCROT did not converge (info={info}, rel={})",
                    fmt_e(rel, 3)
                )));
            }
            return self.sparse_fallback(
                u,
                x,
                rhs,
                transpose,
                chosen,
                ops.get(),
                pre_ops.get(),
                &prep,
                vectors_out,
                pre_diag.as_ref(),
            );
        }
        Ok((
            solution,
            Stage40LinearSolveDiagnostics {
                requested_backend: chosen.as_str(),
                used_backend: chosen.as_str(),
                converged: true,
                relative_residual: rel,
                iterations,
                fallback_used: false,
                krylov_method: "gcrotmk",
                operator_applications: ops.get(),
                preconditioner_applications: pre_ops.get(),
                recycled_vectors_in: prep.vectors_in,
                recycled_vectors_out: vectors_out,
                recycle_reset: prep.reset,
                recycle_reset_reason: prep.reset_reason,
                relative_design_drift: prep.relative_design_drift,
                previous_solution_used: prep.previous_solution_used,
                preconditioner_provider: pre_diag.as_ref().map(|d| d.provider_name.clone()),
                preconditioner_strategy: pre_diag.as_ref().map(|d| d.strategy.clone()),
                preconditioner_transpose_duality_error: pre_diag.as_ref().map(|d| d.transpose_duality_error),
                recycling_suppressed: suppress,
                recycling_suppression_reason: suppress.then_some("preconditioner_already_effective"),
                initial_guess_relative_residual: initial_relative,
                initial_guess_accepted_without_iteration: false,
                recycle_initial_guess_rejected: initial_rejected,
                topology_coordinate: TOPOLOGY_COORDINATE,
            },
        ))
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial_state: &[f64],
        backend: Option<LinearBackend>,
    ) -> CaeResult<(Vec<f64>, f64, Option<Stage40LinearSolveDiagnostics>)> {
        let r = self.hybrid.residual();
        let mut u = initial_state.to_vec();
        let mut res = r.residual::<f64>(&u, design);
        let base = norm2(&res).max(1.0);
        let mut last = norm2(&res);
        let mut last_diag = None;
        for _ in 0..=self.maximum_newton_iterations() {
            if last <= self.nonlinear_tolerance() * base {
                return Ok((u, last / base, last_diag));
            }
            let neg: Vec<f64> = res.iter().map(|v| -v).collect();
            let (du, d) = self.linear_solve(&u, design, &neg, false, backend, true)?;
            last_diag = Some(d);
            if du.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::convergence("non-finite Newton step"));
            }
            let mut alpha = 1.0;
            let mut accepted = false;
            while alpha >= self.minimum_step() {
                let cand: Vec<f64> = u.iter().zip(&du).map(|(a, d)| a + alpha * d).collect();
                let rc = r.residual::<f64>(&cand, design);
                let rn = norm2(&rc);
                if rn.is_finite() && rn < last {
                    u = cand;
                    res = rc;
                    last = rn;
                    accepted = true;
                    break;
                }
                alpha *= 0.5;
            }
            if !accepted {
                return Err(CaeError::convergence("Newton line search could not reduce residual"));
            }
        }
        Err(CaeError::convergence("hybrid residual solve did not converge"))
    }

    fn nonlinear_tolerance(&self) -> f64 {
        self.hybrid_params().0
    }
    fn maximum_newton_iterations(&self) -> usize {
        self.hybrid_params().1
    }
    fn minimum_step(&self) -> f64 {
        self.hybrid_params().2
    }
    fn hybrid_params(&self) -> (f64, usize, f64) {
        self.hybrid.newton_parameters()
    }



    pub fn response_and_gradient<J: DifferentiableResponse>(
        &self,
        design: &[f64],
        initial_state: &[f64],
        response: &J,
        backend: Option<LinearBackend>,
    ) -> CaeResult<RecyclingSensitivity> {
        let r = self.hybrid.residual();
        let (u, prel, pdiag) = self.solve(design, initial_state, backend)?;
        let gu = gradient(response, &u, design, Argument::State);
        let gx = gradient(response, &u, design, Argument::Design);
        let (lam, adiag) = self.linear_solve(&u, design, &gu, true, backend, true)?;
        let rv = vjp(r, &u, design, &lam, Argument::Design)?;
        let grad: Vec<f64> = gx.iter().zip(&rv).map(|(a, b)| a - b).collect();
        let admissible = prel <= self.policy.base.primal_gate
            && adiag.relative_residual <= self.policy.base.adjoint_gate
            && adiag.converged
            && grad.iter().all(|v| v.is_finite());
        let selected = backend.unwrap_or_else(|| self.policy.base.choose(u.len(), self.hybrid.has_sparse()));
        Ok(RecyclingSensitivity {
            value: response_value(response, &u, design),
            gradient: grad,
            primal_backend: pdiag.as_ref().map_or(selected.as_str(), |d| d.used_backend),
            fallback_used: pdiag.as_ref().is_some_and(|d| d.fallback_used) || adiag.fallback_used,
            state: u,
            adjoint: lam,
            primal_relative_residual: prel,
            adjoint_relative_residual: adiag.relative_residual,
            adjoint_backend: adiag.used_backend,
            admissible_for_optimization: admissible,
            primal_linear_diagnostics: pdiag,
            adjoint_linear_diagnostics: adiag,
            topology_coordinate: TOPOLOGY_COORDINATE,
        })
    }



    pub fn require_optimization_admission(
        sensitivity: RecyclingSensitivity,
    ) -> CaeResult<RecyclingSensitivity> {
        if !sensitivity.admissible_for_optimization {
            return Err(CaeError::convergence(format!(
                "optimization gradient rejected: primal residual={}, adjoint residual={}",
                fmt_e(sensitivity.primal_relative_residual, 3),
                fmt_e(sensitivity.adjoint_relative_residual, 3)
            )));
        }
        Ok(sensitivity)
    }
}

