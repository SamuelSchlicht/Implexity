// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::retention::{RetainedFactorizationOwner, RetentionLedger, global_ledger};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::convergence::{ConvergenceReport, Criterion, ScalarL2Criterion};
use crate::diagnostics::ResidualPartition;
use crate::exact_matrix::ExactMatrixIdentity;
use crate::factorization::Factorization;
use crate::implicit_block::{BlockCallbacks, BlockOptions, ImplicitBlockSystem};
use crate::linear_workspace::{
    ExactFactorizationTransaction, ExactFactorizationWorkspace, TransactionOptions,
};
use crate::matrix::{Jacobian, checked_matrix, checked_product_block};
use crate::newton_krylov::{ExactKrylovPolicy, ExactLinearization, MatrixFreeLinearization};
use crate::operation_context::{
    ContextRequirement, OperationExecutionContext, require_operation_context, trace_fields_of,
};
use crate::preconditioner_lease::{
    ExactPreconditionerBinding, ExactPreconditionerLeaseBudget, ProviderPreconditioner, SealedMatrixReadLease,
};
use crate::trace::{self, Fields};

thread_local! {
    static GUESS_FALLBACK: Cell<bool> = const { Cell::new(true) };
}

#[must_use = "the switch is restored when the guard is dropped"]
pub struct GuessFallbackGuard {
    previous: bool,
}

impl Drop for GuessFallbackGuard {
    fn drop(&mut self) {
        GUESS_FALLBACK.with(|c| c.set(self.previous));
    }
}

pub fn matching_guess_fallback(enabled: bool) -> GuessFallbackGuard {
    let previous = GUESS_FALLBACK.with(|c| c.replace(enabled));
    GuessFallbackGuard { previous }
}

pub trait HistoryProblem: Send + Sync {


    fn residual(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Vec<f64>>;


    fn state_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian>;


    fn previous_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian>;


    fn design_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian>;
}

#[derive(Clone, Copy, Debug)]
pub struct HistoryPoint<'a> {
    pub history_step: usize,
    pub state: &'a [f64],
    pub previous_state: &'a [f64],
    pub design: &'a [f64],
    pub execution_context: Option<&'a OperationExecutionContext>,
}

pub type HistoryCapabilityFactory = Arc<
    dyn Fn(&ExactMatrixIdentity, HistoryPoint<'_>) -> CaeResult<Box<dyn ExactLinearization>> + Send + Sync,
>;
pub type HistoryMatrixFreeFactory =
    Arc<dyn Fn(HistoryPoint<'_>) -> CaeResult<Box<dyn MatrixFreeLinearization>> + Send + Sync>;
pub type HistoryPreconditionerFactory = Arc<
    dyn Fn(
            &SealedMatrixReadLease,
            &ExactPreconditionerBinding,
            HistoryPoint<'_>,
        ) -> CaeResult<Box<dyn ProviderPreconditioner>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct HistoryOptions {
    pub tolerance: f64,
    pub max_iterations: usize,
    pub condition_limit: f64,
    pub krylov_policy: Option<ExactKrylovPolicy>,
    pub capability_factory: Option<HistoryCapabilityFactory>,
    pub matrix_free_factory: Option<HistoryMatrixFreeFactory>,
    pub matrix_free_bootstrap: bool,
    pub preconditioner_factory: Option<HistoryPreconditionerFactory>,
    pub lease_budget: Option<ExactPreconditionerLeaseBudget>,
    pub authority_policy_sha256: Option<String>,
    pub authority_operation_sha256: Option<String>,
    pub authority_provider_profile_sha256: Option<String>,
    pub criterion: Option<Criterion>,
    pub residual_partition: Option<ResidualPartition>,
    pub relaxed_tolerance: Option<f64>,
    pub local_elimination_partition: Option<Arc<crate::local_condensation::LocalEliminationPartition>>,
    pub rejected_state_capture: Option<crate::rejected_state::RejectedStateCapture>,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            tolerance: 1e-9,
            max_iterations: 40,
            condition_limit: 1e14,
            krylov_policy: None,
            capability_factory: None,
            matrix_free_factory: None,
            matrix_free_bootstrap: false,
            preconditioner_factory: None,
            lease_budget: None,
            authority_policy_sha256: None,
            authority_operation_sha256: None,
            authority_provider_profile_sha256: None,
            criterion: None,
            residual_partition: None,
            relaxed_tolerance: None,
            local_elimination_partition: None,
            rejected_state_capture: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistorySolution {
    pub states: Vec<Vec<f64>>,
    pub residual_norms: Vec<f64>,
    pub newton_iterations: Vec<usize>,
    pub execution_context: Option<OperationExecutionContext>,
    pub convergence_reports: Option<Vec<Option<ConvergenceReport>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryAdjoint {
    pub gradients: DenseMatrix,
    pub adjoints: Vec<DenseMatrix>,
    pub initial_state_covectors: DenseMatrix,
    pub adjoint_factorizations: usize,
    pub adjoint_factorization_builds: usize,
    pub adjoint_factorization_reuses: usize,
    pub maximum_transpose_relative_residual: f64,
    pub history_states_retained: usize,
    pub history_derivative: &'static str,
    pub initial_state_design_derivative: Option<&'static str>,
    pub initial_state_design_gradient_norm: Option<f64>,
    pub residual_error_bounds: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct HistorySolveOptions<'a> {
    pub matching_time_states: Option<&'a [Vec<f64>]>,
    pub execution_context: Option<&'a OperationExecutionContext>,
    pub max_iterations: Option<usize>,
}

type Staged = BTreeMap<usize, (ExactFactorizationWorkspace, ExactFactorizationTransaction)>;

#[derive(Default)]
struct Retention {
    armed: bool,
    context: Option<OperationExecutionContext>,
    staged: Option<Staged>,
    pending: Option<(Staged, String, Option<OperationExecutionContext>)>,
    committed: BTreeMap<usize, ExactFactorizationWorkspace>,
    binding: Option<String>,
    provenance: Option<OperationExecutionContext>,
}

struct RetentionOwner {
    state: Mutex<Retention>,
}

impl RetainedFactorizationOwner for RetentionOwner {
    fn release_retained_factorization(&self, slot: i64) -> bool {
        let Ok(mut state) = self.state.try_lock() else { return false };
        let Ok(step) = usize::try_from(slot) else { return false };
        if let Some(staged) = state.staged.as_mut()
            && let Some((ws, mut tx)) = staged.remove(&step)
        {
            let _ = tx.rollback();
            let _ = ws.clear();
            return true;
        }
        if let Some((staged, _, _)) = state.pending.as_mut()
            && let Some((ws, mut tx)) = staged.remove(&step)
        {
            let _ = tx.rollback();
            let _ = ws.clear();
            return true;
        }
        match state.committed.get(&step) {
            Some(ws) if !ws.transaction_active() => {
                let ws = state.committed.remove(&step);
                if let Some(ws) = ws {
                    let _ = ws.clear();
                }
                true
            }
            _ => false,
        }
    }
}

pub struct NativeHistorySystem {
    problem: Arc<dyn HistoryProblem>,
    options: HistoryOptions,
    criterion: Criterion,
    retention: Arc<RetentionOwner>,
}

impl std::fmt::Debug for NativeHistorySystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHistorySystem")
            .field("tolerance", &self.options.tolerance)
            .field("max_iterations", &self.options.max_iterations)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub fn exact_linearization_binding(design: &[f64], states: &[Vec<f64>]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"implexity-exact-history-linearization/1\0");
    digest.update(((states.len() + 1) as u64).to_be_bytes());
    let arrays = std::iter::once(design).chain(states.iter().map(Vec::as_slice));
    for (index, value) in arrays.enumerate() {
        digest.update((index as u64).to_be_bytes());
        digest.update(1u64.to_be_bytes());
        digest.update((value.len() as u64).to_be_bytes());
        for v in value {
            digest.update(v.to_le_bytes());
        }
    }
    hex::encode(digest.finalize())
}

struct StepCallbacks {
    problem: Arc<dyn HistoryProblem>,
    n: usize,
}

impl BlockCallbacks<[f64]> for StepCallbacks {
    fn residual(&self, z: &[f64], x: &[f64], prev: &[f64]) -> CaeResult<Vec<f64>> {
        self.problem.residual(self.n, z, prev, x)
    }
    fn state_jacobian(&self, z: &[f64], x: &[f64], prev: &[f64]) -> CaeResult<Jacobian> {
        self.problem.state_jacobian(self.n, z, prev, x)
    }
    fn design_jacobian(&self, z: &[f64], x: &[f64], prev: &[f64]) -> CaeResult<Jacobian> {
        self.problem.design_jacobian(self.n, z, prev, x)
    }
}

fn slot(step: usize) -> i64 {
    i64::try_from(step).unwrap_or(i64::MAX)
}

fn ledger() -> CaeResult<&'static RetentionLedger> {
    global_ledger().map_err(|e| CaeError::contract(e.to_string()))
}

impl NativeHistorySystem {


    pub fn new(problem: Arc<dyn HistoryProblem>, options: HistoryOptions) -> CaeResult<Self> {
        if !options.tolerance.is_finite() || options.tolerance <= 0.0 {
            return Err(CaeError::contract("history residual tolerance must be finite and positive"));
        }
        if !options.condition_limit.is_finite() || options.condition_limit <= 1.0 {
            return Err(CaeError::contract("history condition limit must be finite and greater than one"));
        }
        if let Some(p) = options.krylov_policy {
            p.validate()?;
        }
        if options.matrix_free_bootstrap && options.matrix_free_factory.is_none() {
            return Err(CaeError::contract("history exact matrix-free bootstrap requires exact actions"));
        }
        if options.capability_factory.is_some() && options.preconditioner_factory.is_some() {
            return Err(CaeError::contract(
                "history exact Krylov solve must select one provider integration path",
            ));
        }
        if options.capability_factory.is_some() && options.matrix_free_factory.is_some() {
            return Err(CaeError::contract(
                "history exact Krylov solve cannot select two operator capabilities",
            ));
        }
        let criterion = match &options.criterion {
            Some(c) => Arc::clone(c),
            None => ScalarL2Criterion::shared(options.tolerance)?,
        };
        Ok(Self {
            problem,
            options,
            criterion,
            retention: Arc::new(RetentionOwner { state: Mutex::new(Retention::default()) }),
        })
    }

    #[must_use]
    pub fn problem(&self) -> &Arc<dyn HistoryProblem> {
        &self.problem
    }
    #[must_use]
    pub fn options(&self) -> &HistoryOptions {
        &self.options
    }
    #[must_use]
    pub fn criterion(&self) -> &Criterion {
        &self.criterion
    }

    fn state(&self) -> MutexGuard<'_, Retention> {
        self.retention.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn owner(&self) -> Arc<dyn RetainedFactorizationOwner> {
        let owner: Arc<dyn RetainedFactorizationOwner> = self.retention.clone();
        owner
    }

    fn require_frame(&self, size: usize, label: &str) -> CaeResult<()> {
        if let Some(bound) = self.criterion.state_size()
            && bound != size
        {
            return Err(CaeError::contract(format!(
                "{label}: convergence criterion is bound to {bound} unknowns but the state has {size} (solver NativeHistorySystem); author the criterion in this solver's residual frame"
            )));
        }
        Ok(())
    }

    fn criterion_trace(&self) -> Fields {
        if self.criterion.is_scalar() {
            Fields::new()
        } else {
            crate::trace_fields! {"convergence_criterion" => self.criterion.describe()}
        }
    }

    fn block(
        &self,
        n: usize,
        context: Option<&OperationExecutionContext>,
        max_iterations: Option<usize>,
    ) -> CaeResult<ImplicitBlockSystem<[f64]>> {
        let callbacks: Arc<dyn BlockCallbacks<[f64]>> =
            Arc::new(StepCallbacks { problem: Arc::clone(&self.problem), n });
        let ctx = context.cloned();
        let capability_factory = self.options.capability_factory.as_ref().map(|outer| {
            let outer = Arc::clone(outer);
            let ctx = ctx.clone();
            let f: crate::implicit_block::CapabilityFactory<[f64]> =
                Arc::new(move |id: &ExactMatrixIdentity, state: &[f64], design: &[f64], prev: &[f64]| {
                    outer(
                        id,
                        HistoryPoint {
                            history_step: n,
                            state,
                            previous_state: prev,
                            design,
                            execution_context: ctx.as_ref(),
                        },
                    )
                });
            f
        });
        let matrix_free_factory = self.options.matrix_free_factory.as_ref().map(|outer| {
            let outer = Arc::clone(outer);
            let ctx = ctx.clone();
            let f: crate::implicit_block::MatrixFreeFactory<[f64]> =
                Arc::new(move |state: &[f64], design: &[f64], prev: &[f64]| {
                    outer(HistoryPoint {
                        history_step: n,
                        state,
                        previous_state: prev,
                        design,
                        execution_context: ctx.as_ref(),
                    })
                });
            f
        });
        let preconditioner_factory = self.options.preconditioner_factory.as_ref().map(|outer| {
            let outer = Arc::clone(outer);
            let ctx = ctx.clone();
            let f: crate::implicit_block::PreconditionerFactory<[f64]> = Arc::new(
                move |lease: &SealedMatrixReadLease,
                      binding: &ExactPreconditionerBinding,
                      state: &[f64],
                      design: &[f64],
                      prev: &[f64]| {
                    outer(
                        lease,
                        binding,
                        HistoryPoint {
                            history_step: n,
                            state,
                            previous_state: prev,
                            design,
                            execution_context: ctx.as_ref(),
                        },
                    )
                },
            );
            f
        });
        ImplicitBlockSystem::new(
            callbacks,
            BlockOptions {
                tolerance: self.options.tolerance,
                max_iterations: max_iterations
                    .map_or(self.options.max_iterations, |m| m.min(self.options.max_iterations)),
                condition_limit: self.options.condition_limit,
                krylov_policy: self.options.krylov_policy,
                capability_factory,
                matrix_free_factory,
                matrix_free_bootstrap: self.options.matrix_free_bootstrap,
                preconditioner_factory,
                lease_budget: self.options.lease_budget,
                authority_policy_sha256: self.options.authority_policy_sha256.clone(),
                authority_operation_sha256: self.options.authority_operation_sha256.clone(),
                authority_provider_profile_sha256: self.options.authority_provider_profile_sha256.clone(),
                execution_context: ctx,
                trace_context: None,
                criterion: Some(Arc::clone(&self.criterion)),
                residual_partition: self.options.residual_partition.clone(),
                relaxed_tolerance: self.options.relaxed_tolerance,
                local_elimination_partition: self.options.local_elimination_partition.clone(),
                rejected_state_capture: self.options.rejected_state_capture.clone().map(|mut capture| {
                    capture.provenance = serde_json::json!({"caller":capture.provenance,"history_step":n}); capture
                }),
                rejected_state_context: self.options.rejected_state_capture.as_ref().map(|_| Arc::new(|previous: &[f64]| previous.to_vec()) as Arc<dyn Fn(&[f64]) -> Vec<f64> + Send + Sync>),
            },
        )
    }

    fn rollback_staged(&self, staged: Staged) {
        let ledger = ledger().ok();
        let owner = self.owner();
        for (step, (ws, mut tx)) in staged {
            let _ = tx.rollback();
            if let Some(l) = ledger {
                l.release(&owner, Some(slot(step)));
            }
            let _ = ws.clear();
        }
    }

    fn release_committed(&self) {
        let committed = {
            let mut s = self.state();
            s.binding = None;
            s.provenance = None;
            std::mem::take(&mut s.committed)
        };
        let ledger = ledger().ok();
        let owner = self.owner();
        for (step, ws) in committed {
            if let Some(l) = ledger {
                l.release(&owner, Some(slot(step)));
            }
            let _ = ws.clear();
        }
    }

    fn retention_serves(&self, context: Option<&OperationExecutionContext>) -> CaeResult<bool> {
        let provenance = self.state().provenance.clone();
        match provenance {
            None => Ok(context.is_none()),
            Some(p) => {
                if context.is_none() {
                    return Ok(false);
                }
                require_operation_context(
                    context,
                    ContextRequirement {
                        authority_eligible: Some(true),
                        expected: None,
                        provider_profile_digest: Some(p.provider_profile_digest()),
                    },
                    "retained exact linearization operation context",
                )?;
                Ok(true)
            }
        }
    }



    pub fn begin_exact_factorization_reuse(
        &self,
        execution_context: Option<&OperationExecutionContext>,
        retain_for: Option<(&[f64], &HistorySolution)>,
    ) -> CaeResult<Value> {
        if execution_context.is_some() {
            require_operation_context(
                execution_context,
                ContextRequirement { authority_eligible: Some(true), ..Default::default() },
                "exact factorization operation context",
            )?;
        }
        let mut keep = false;
        if let Some((design, solution)) = retain_for {
            let (pending_none, has_committed, binding) = {
                let s = self.state();
                (s.pending.is_none(), !s.committed.is_empty(), s.binding.clone())
            };
            if pending_none && has_committed {
                let states = validated_solution(solution)?;
                keep = binding.as_deref() == Some(exact_linearization_binding(design, &states).as_str())
                    && self.retention_serves(execution_context)?;
            }
        }
        if !keep {
            self.discard_exact_factorization_reuse();
        }
        {
            let mut s = self.state();
            s.armed = true;
            s.context = execution_context.cloned();
        }
        Ok(self.exact_factorization_reuse_report())
    }



    pub fn commit_guarded_exact_linearization(
        &self,
        design: &[f64],
        solution: &HistorySolution,
        execution_context: Option<&OperationExecutionContext>,
    ) -> CaeResult<bool> {
        let pending = self.state().pending.take();
        let Some((mut staged, binding, pending_context)) = pending else { return Ok(false) };
        let checked = (|| -> CaeResult<()> {
            if let Some(pc) = &pending_context {
                require_operation_context(
                    execution_context,
                    ContextRequirement {
                        authority_eligible: Some(true),
                        expected: Some(pc),
                        provider_profile_digest: None,
                    },
                    "guarded exact linearization operation context",
                )?;
                require_operation_context(
                    solution.execution_context.as_ref(),
                    ContextRequirement {
                        authority_eligible: Some(true),
                        expected: Some(pc),
                        provider_profile_digest: None,
                    },
                    "guarded exact linearization solution context",
                )?;
            } else if execution_context.is_some() {
                return Err(CaeError::contract(
                    "legacy exact linearization cannot be committed under a new operation scope",
                ));
            }
            let states = validated_solution(solution)?;
            if exact_linearization_binding(design, &states) != binding {
                return Err(CaeError::contract(
                    "guarded exact linearization design/history identity drifted",
                ));
            }
            Ok(())
        })();
        if let Err(e) = checked {
            self.rollback_staged(staged);
            return Err(e);
        }
        self.release_committed();
        let mut committed = BTreeMap::new();
        let steps: Vec<usize> = staged.keys().copied().collect();
        for step in steps {
            if let Some((ws, mut tx)) = staged.remove(&step) {
                if let Err(e) = tx.commit() {
                    self.rollback_staged(staged);
                    let ledger = ledger().ok();
                    let owner = self.owner();
                    for (s, w) in committed {
                        if let Some(l) = ledger {
                            l.release(&owner, Some(slot(s)));
                        }
                        let _: CaeResult<()> = ExactFactorizationWorkspace::clear(&w);
                    }
                    return Err(e);
                }
                committed.insert(step, ws);
            }
        }
        let mut s = self.state();
        s.committed = committed;
        s.binding = Some(binding);
        s.provenance = pending_context;
        Ok(true)
    }

    pub fn discard_exact_factorization_reuse(&self) {
        let (pending, staged) = {
            let mut s = self.state();
            (s.pending.take(), s.staged.take())
        };
        if let Some((p, _, _)) = pending {
            self.rollback_staged(p);
        }
        if let Some(st) = staged {
            self.rollback_staged(st);
        }
        self.release_committed();
        if let Ok(l) = ledger() {
            l.release(&self.owner(), None);
        }
        let mut s = self.state();
        s.armed = false;
        s.context = None;
    }

    #[must_use]
    pub fn exact_factorization_reuse_report(&self) -> Value {
        let s = self.state();
        let ledger_report =
            ledger().map_or(Value::Null, |l| serde_json::to_value(l.report()).unwrap_or(Value::Null));
        json!({
            "enabled": s.armed,
            "committed": !s.committed.is_empty(),
            "committed_steps": s.committed.keys().collect::<Vec<_>>(),
            "pending_steps": s.pending.as_ref().map(|p| p.0.keys().copied().collect::<Vec<_>>()).unwrap_or_default(),
            "retention_scope": "final_step_guaranteed_earlier_steps_byte_budgeted",
            "retention_ledger": ledger_report,
        })
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial: &[f64],
        steps: usize,
        options: &HistorySolveOptions<'_>,
    ) -> CaeResult<HistorySolution> {
        let context = match options.execution_context {
            Some(c) => Some(require_operation_context(
                Some(c),
                ContextRequirement::default(),
                "history solve operation context",
            )?),
            None => None,
        };
        let operation = trace_fields_of(context);
        if design.is_empty()
            || initial.is_empty()
            || design.iter().any(|v| !v.is_finite())
            || initial.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("history requires finite nonempty design and initial state"));
        }
        self.require_frame(initial.len(), "NativeHistorySystem.solve")?;
        if steps < 1 {
            return Err(CaeError::contract("history requires a positive integer step count"));
        }
        let guesses = match options.matching_time_states {
            None => None,
            Some(raw) => {
                if raw.len() != steps + 1 {
                    return Err(CaeError::contract(
                        "matching-time Newton guesses must contain the initial and every solved time state",
                    ));
                }
                if raw.iter().any(|s| s.len() != initial.len() || s.iter().any(|v| !v.is_finite())) {
                    return Err(CaeError::contract(
                        "matching-time Newton guess has invalid state shape or values",
                    ));
                }
                Some(raw)
            }
        };
        let history_id = trace::new_trace_id("history-solve")?;
        trace::lifecycle(
            "history_solve",
            || {
                let mut f = crate::trace_fields! {
                    "history_id" => history_id, "state_size" => initial.len(), "design_size" => design.len(),
                    "steps" => steps, "matching_time_guesses" => guesses.is_some(),
                    "tolerance" => self.options.tolerance,
                    "maximum_iterations" => options.max_iterations.map_or(self.options.max_iterations, |m| m.min(self.options.max_iterations)),
                };
                f.extend(operation.clone());
                f.extend(self.criterion_trace());
                f
            },
            || {
                self.solve_validated(
                    design,
                    initial,
                    steps,
                    guesses,
                    &history_id,
                    context,
                    options.max_iterations,
                )
            },
            |r| crate::trace_fields! {"total_newton_iterations" => r.newton_iterations.iter().sum::<usize>()},
        )
    }

    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn solve_validated(
        &self,
        x: &[f64],
        initial: &[f64],
        steps: usize,
        guesses: Option<&[Vec<f64>]>,
        history_id: &str,
        context: Option<&OperationExecutionContext>,
        max_iterations: Option<usize>,
    ) -> CaeResult<HistorySolution> {
        let per_field = !self.criterion.is_scalar();
        let (armed, workspace_context) = {
            let s = self.state();
            (s.armed, s.context.clone())
        };
        if armed {
            if let Some(wc) = &workspace_context {
                require_operation_context(
                    context,
                    ContextRequirement {
                        authority_eligible: Some(true),
                        expected: Some(wc),
                        provider_profile_digest: None,
                    },
                    "history solve exact-factorization context",
                )?;
            } else if context.is_some() {
                return Err(CaeError::contract(
                    "legacy exact factorization workspace cannot consume a scoped solve",
                ));
            }
            let pending = self.state().pending.take();
            if let Some((p, _, _)) = pending {
                self.rollback_staged(p);
            }
            self.release_committed();
            self.state().staged = Some(Staged::new());
        }
        let operation = trace_fields_of(context);
        let mut states = vec![initial.to_vec()];
        let mut residuals = Vec::with_capacity(steps);
        let mut iterations = Vec::with_capacity(steps);
        let mut reports = Vec::with_capacity(steps);
        let outcome = (|| -> CaeResult<()> {
            for n in 1..=steps {
                let previous = states[n - 1].clone();
                let guess = guesses.map_or_else(|| previous.clone(), |g| g[n].clone());
                let build = |n: usize| -> CaeResult<(
                    ImplicitBlockSystem<[f64]>,
                    Option<ExactFactorizationWorkspace>,
                )> {
                    let block = self.block(n, context, max_iterations)?;
                    let ws = if armed {
                        let ws = ExactFactorizationWorkspace::new();
                        block.install_exact_factorization_workspace(ws.clone())?;
                        Some(ws)
                    } else {
                        None
                    };
                    Ok((block, ws))
                };
                let (mut block, mut workspace) = build(n)?;
                let solved = trace::span(
                    "history_step_solve",
                    || {
                        let mut f = crate::trace_fields! {"history_id" => history_id, "history_step" => n,
                        "matching_time_guess" => guesses.is_some()};
                        f.extend(operation.clone());
                        f
                    },
                    || match block.solve(x, &guess, &previous) {
                        Ok(r) => Ok(r),
                        Err(e)
                            if e.is_convergence() && guesses.is_some() && GUESS_FALLBACK.with(Cell::get) =>
                        {
                            block.discard_staged_exact_factorization();
                            let text: String = e.message().chars().take(500).collect();
                            trace::point("history_step_guess_fallback", || {
                                let mut f = crate::trace_fields! {"history_id" => history_id, "history_step" => n,
                                "guess_failure" => text};
                                f.extend(operation.clone());
                                f
                            })?;
                            let (b, w) = build(n)?;
                            block = b;
                            workspace = w;
                            block.solve(x, &previous, &previous)
                        }
                        Err(e) => Err(e),
                    },
                );
                let result = match solved {
                    Ok(r) => r,
                    Err(e) => {
                        block.discard_staged_exact_factorization();
                        return Err(e);
                    }
                };
                let step_checks = (|| -> CaeResult<Option<ConvergenceReport>> {
                    if result.state.len() != initial.len() || result.state.iter().any(|v| !v.is_finite()) {
                        return Err(CaeError::contract(format!(
                            "history step {n} returned an invalid state shape or values"
                        )));
                    }
                    if !result.residual_norm.is_finite() || result.residual_norm < 0.0 {
                        return Err(CaeError::contract(format!(
                            "history step {n} returned an invalid residual norm"
                        )));
                    }
                    let report = if per_field {
                        let expected = self.criterion.policy_fields();
                        let ok = result.convergence.as_ref().is_some_and(|r| {
                            r.certified
                                && expected.as_ref().is_none_or(|e| {
                                    r.passed
                                        .iter()
                                        .map(|(k, _)| k.clone())
                                        .collect::<std::collections::BTreeSet<_>>()
                                        == *e
                                })
                        });
                        if !ok {
                            return Err(CaeError::contract(format!(
                                "history step {n} did not satisfy the authored per-field convergence policy"
                            )));
                        }
                        result.convergence.clone()
                    } else {
                        let limit = if result.relaxed {
                            self.options.relaxed_tolerance.unwrap_or(self.options.tolerance)
                        } else {
                            self.options.tolerance
                        };
                        if result.residual_norm > limit {
                            return Err(CaeError::contract(format!(
                                "history step {n} did not satisfy the configured residual tolerance"
                            )));
                        }
                        None
                    };
                    if !result.converged {
                        return Err(CaeError::contract(format!(
                            "history step {n} did not return an exactly converged result"
                        )));
                    }
                    Ok(report)
                })();
                let report = match step_checks {
                    Ok(r) => r,
                    Err(e) => {
                        block.discard_staged_exact_factorization();
                        return Err(e);
                    }
                };
                let normalized = report.as_ref().map(|r| r.normalized.clone());
                states.push(result.state);
                residuals.push(result.residual_norm);
                iterations.push(result.iterations);
                reports.push(report);
                trace::point("history_step_converged", || {
                    let mut f = crate::trace_fields! {"history_id" => history_id, "history_step" => n,
                    "residual_norm" => result.residual_norm, "newton_iterations" => result.iterations};
                    f.extend(operation.clone());
                    if let Some(nm) = &normalized {
                        f.insert(
                            "normalized_residuals".into(),
                            Value::Object(nm.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
                        );
                    }
                    f
                })?;
                if armed {
                    let Some(tx) = block.take_staged_exact_factorization() else {
                        return Err(CaeError::contract(format!(
                            "converged history step {n} did not stage its exact factorization"
                        )));
                    };
                    let ws = workspace.take().unwrap_or_default();
                    self.stage_retained_step(n, ws, tx, n == steps, history_id, &operation)?;
                }
            }
            Ok(())
        })();
        if let Err(e) = outcome {
            let staged = self.state().staged.take();
            if let Some(st) = staged {
                self.rollback_staged(st);
            }
            return Err(e);
        }
        let solution = HistorySolution {
            states,
            residual_norms: residuals,
            newton_iterations: iterations,
            execution_context: context.cloned(),
            convergence_reports: per_field.then_some(reports),
        };
        if armed {
            let binding = exact_linearization_binding(x, &solution.states);
            let mut s = self.state();
            let staged = s.staged.take().unwrap_or_default();
            s.pending = Some((staged, binding, context.cloned()));
        }
        Ok(solution)
    }

    fn stage_retained_step(
        &self,
        n: usize,
        workspace: ExactFactorizationWorkspace,
        mut transaction: ExactFactorizationTransaction,
        is_final: bool,
        history_id: &str,
        operation: &Fields,
    ) -> CaeResult<()> {
        let nbytes = transaction.retained_bytes()?;
        let admitted = ledger()?.admit(&self.owner(), slot(n), nbytes.map(|b| b as u64), is_final);
        if admitted {
            let mut s = self.state();
            if let Some(st) = s.staged.as_mut() {
                st.insert(n, (workspace, transaction));
            } else {
                drop(s);
                transaction.rollback()?;
                workspace.clear()?;
            }
        } else {
            transaction.rollback()?;
            workspace.clear()?;
        }
        trace::point("history_step_factorization_retention", || {
            let mut f = crate::trace_fields! {"history_id" => history_id, "history_step" => n,
            "retained" => admitted, "final_step" => is_final, "retained_bytes" => nbytes};
            f.extend(operation.clone());
            f
        })
    }



    #[allow(clippy::too_many_lines)]
    pub fn adjoint_many(
        &self,
        design: &[f64],
        solution: &HistorySolution,
        gu: &[DenseMatrix],
        grad: &DenseMatrix,
        initial_design_jacobian: Option<Jacobian>,
        execution_context: Option<&OperationExecutionContext>,
    ) -> CaeResult<HistoryAdjoint> {
        match &solution.execution_context {
            None => {
                if execution_context.is_some() {
                    return Err(CaeError::contract(
                        "unscoped history solution cannot enter a scoped adjoint",
                    ));
                }
            }
            Some(sc) => {
                require_operation_context(
                    execution_context,
                    ContextRequirement { expected: Some(sc), ..Default::default() },
                    "history adjoint operation context",
                )?;
            }
        }
        if design.is_empty() || design.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("history adjoint requires a finite nonempty design"));
        }
        let states = validated_solution(solution)?;
        self.require_frame(states[0].len(), "NativeHistorySystem.adjoint_many")?;
        let accepted =
            self.options.relaxed_tolerance.unwrap_or(self.options.tolerance).max(self.options.tolerance);
        if self.criterion.is_scalar() && solution.residual_norms.iter().any(|v| *v > accepted) {
            return Err(CaeError::contract("history solution diagnostics do not establish convergence"));
        }
        let ns = states.len();
        let nz = states[0].len();
        let m = grad.ncols;
        if gu.len() != ns
            || m == 0
            || gu.iter().any(|g| g.nrows != nz || g.ncols != m)
            || grad.nrows != design.len()
        {
            return Err(CaeError::contract("invalid history response derivative dimensions"));
        }
        if gu.iter().any(|g| g.data.iter().any(|v| !v.is_finite()))
            || grad.data.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::convergence("nonfinite history response derivatives"));
        }
        let initial_matrix = match initial_design_jacobian {
            Some(j) => Some(checked_matrix(j, (nz, design.len()), "initial design Jacobian", true)?),
            None => None,
        };
        let adjoint_id = trace::new_trace_id("history-adjoint")?;
        let (armed, workspace_context, pending) = {
            let s = self.state();
            (s.armed, s.context.clone(), s.pending.is_some())
        };
        if armed {
            if let Some(wc) = &workspace_context {
                require_operation_context(
                    execution_context,
                    ContextRequirement {
                        authority_eligible: Some(true),
                        expected: Some(wc),
                        provider_profile_digest: None,
                    },
                    "history adjoint exact-factorization context",
                )?;
            } else if execution_context.is_some() {
                return Err(CaeError::contract(
                    "legacy exact factorization workspace cannot enter a scoped adjoint",
                ));
            }
        }
        if pending {
            return Err(CaeError::contract("history adjoint cannot use an unguarded exact linearization"));
        }
        let operation = trace_fields_of(execution_context);
        let mut retained = false;
        let has_committed = !self.state().committed.is_empty();
        if has_committed {
            let binding = self.state().binding.clone();
            if binding.as_deref() != Some(exact_linearization_binding(design, &states).as_str()) {
                trace::point("history_adjoint_retention_stale", || {
                    let mut f = crate::trace_fields! {"adjoint_id" => adjoint_id};
                    f.extend(operation.clone());
                    f
                })?;
                self.release_committed();
            } else if !self.retention_serves(execution_context)? {
                self.discard_exact_factorization_reuse();
                return Err(CaeError::contract(
                    "retained exact linearization does not serve this adjoint scope",
                ));
            } else {
                retained = true;
            }
        }
        let result = trace::lifecycle(
            "history_adjoint",
            || {
                let mut f = crate::trace_fields! {"adjoint_id" => adjoint_id, "state_size" => nz,
                "design_size" => design.len(), "history_states" => ns, "right_hand_sides" => m};
                f.extend(operation.clone());
                f
            },
            || {
                self.adjoint_validated(
                    design,
                    &states,
                    gu,
                    grad,
                    initial_matrix.as_ref(),
                    &adjoint_id,
                    retained,
                    &operation,
                )
            },
            |r| {
                crate::trace_fields! {
                    "adjoint_right_hand_sides" => m,
                    "adjoint_factorizations" => r.adjoint_factorizations,
                    "adjoint_factorization_builds" => r.adjoint_factorization_builds,
                    "adjoint_factorization_reuses" => r.adjoint_factorization_reuses,
                    "maximum_transpose_relative_residual" => r.maximum_transpose_relative_residual,
                }
            },
        );
        self.discard_exact_factorization_reuse();
        result.map(|mut adjoint| {
            adjoint.residual_error_bounds =
                residual_error_bounds(&adjoint.adjoints, &solution.residual_norms, m);
            adjoint
        })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn adjoint_validated(
        &self,
        x: &[f64],
        states: &[Vec<f64>],
        gu: &[DenseMatrix],
        grad: &DenseMatrix,
        initial_matrix: Option<&Jacobian>,
        adjoint_id: &str,
        retained: bool,
        operation: &Fields,
    ) -> CaeResult<HistoryAdjoint> {
        let ns = states.len();
        let nz = states[0].len();
        let m = grad.ncols;
        let mut lam: Vec<DenseMatrix> = (0..ns).map(|_| DenseMatrix::zeros(nz, m)).collect();
        let mut rel: Vec<f64> = Vec::new();
        let mut reuses = 0usize;
        let mut initial_covector = gu[0].clone();
        let mut grad = grad.clone();
        for n in (1..ns).rev() {
            let z = &states[n];
            let prev = &states[n - 1];
            let residual = self.problem.residual(n, z, prev, x)?;
            if residual.len() != nz || residual.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("history adjoint requires every state to be converged"));
            }
            let report = self.criterion.assess(&residual, None)?;

            let relaxed = self.criterion.is_scalar()
                && self.options.relaxed_tolerance.is_some_and(|limit| {
                    crate::certificate::stable_l2(&residual).is_ok_and(|norm| norm <= limit)
                });
            if !report.certified && !relaxed {
                let extra = if self.criterion.is_scalar() {
                    String::new()
                } else {
                    format!("; step={n}; reason={}", report.reason)
                };
                return Err(CaeError::contract(format!(
                    "history adjoint requires every state to be converged{extra}"
                )));
            }
            let mut rhs = gu[n].clone();
            if n < ns - 1 {
                let b = checked_matrix(
                    self.problem.previous_jacobian(n + 1, &states[n + 1], z, x)?,
                    (nz, nz),
                    "history previous-state Jacobian",
                    true,
                )?;
                let c = checked_product_block(
                    &b,
                    &lam[n + 1],
                    "history previous-state transpose contraction",
                    true,
                )?;
                for (r, v) in rhs.data.iter_mut().zip(&c.data) {
                    *r -= v;
                }
            }
            let (solution, rr) = trace::span(
                "history_adjoint_step",
                || {
                    let mut f = crate::trace_fields! {"adjoint_id" => adjoint_id, "history_step" => n, "right_hand_sides" => m};
                    f.extend(operation.clone());
                    f
                },
                || -> CaeResult<(Vec<f64>, Vec<f64>)> {
                    let workspace = if retained { self.state().committed.remove(&n) } else { None };
                    let ledger = ledger()?;
                    let owner = self.owner();
                    if let Some(ws) = workspace.filter(ExactFactorizationWorkspace::committed) {
                        let fields = {
                            let mut f = crate::trace_fields! {"adjoint_id" => adjoint_id, "history_step" => n,
                            "purpose" => "history_adjoint_exact_reuse"};
                            f.extend(operation.clone());
                            f
                        };
                        let out = (|| {
                            let matrix = trace::span(
                                "state_jacobian_callback",
                                || fields.clone(),
                                || self.problem.state_jacobian(n, z, prev, x),
                            )?;
                            let tx = ws.transaction(
                                matrix,
                                nz,
                                self.options.condition_limit,
                                |_| {
                                    Err(CaeError::contract("strict exact factorization reuse cannot rebuild"))
                                },
                                TransactionOptions {
                                    discard_committed_on_miss: true,
                                    require_committed_match: true,
                                },
                            )?;
                            let solved = tx.solve_block(&rhs.data, m, true)?;
                            drop(tx);
                            Ok::<_, CaeError>((solved.solution, solved.relative))
                        })();
                        let _ = ws.clear();
                        ledger.release(&owner, Some(slot(n)));
                        let out = out?;
                        reuses += 1;
                        Ok(out)
                    } else {
                        if retained {
                            ledger.release(&owner, Some(slot(n)));
                        }
                        let fields = {
                            let mut f = crate::trace_fields! {"adjoint_id" => adjoint_id, "history_step" => n,
                            "purpose" => "history_adjoint"};
                            f.extend(operation.clone());
                            f
                        };
                        let matrix = trace::span(
                            "state_jacobian_callback",
                            || fields.clone(),
                            || self.problem.state_jacobian(n, z, prev, x),
                        )?;
                        let fac =
                            Factorization::new_with_partition(matrix, nz, self.options.condition_limit, Some(&fields), self.options.local_elimination_partition.as_deref())?;
                        let solved = fac.solve_block(&rhs.data, m, true)?;
                        Ok((solved.solution, solved.relative))
                    }
                },
            )?;
            lam[n] = DenseMatrix { nrows: nz, ncols: m, data: solution };
            rel.extend(rr);
            let c = checked_matrix(
                self.problem.design_jacobian(n, z, prev, x)?,
                (nz, x.len()),
                "history design Jacobian",
                true,
            )?;
            let contraction =
                checked_product_block(&c, &lam[n], "history design transpose contraction", true)?;
            for (g, v) in grad.data.iter_mut().zip(&contraction.data) {
                *g -= v;
            }
            if n == 1 {
                let b = checked_matrix(
                    self.problem.previous_jacobian(n, z, prev, x)?,
                    (nz, nz),
                    "initial-state Jacobian",
                    true,
                )?;
                let c = checked_product_block(&b, &lam[n], "initial-state transpose contraction", true)?;
                for (r, v) in initial_covector.data.iter_mut().zip(&c.data) {
                    *r -= v;
                }
            }
        }
        if let Some(im) = initial_matrix {
            let c =
                checked_product_block(im, &initial_covector, "initial design transpose contraction", true)?;
            for (g, v) in grad.data.iter_mut().zip(&c.data) {
                *g += v;
            }
        }
        if grad.data.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("nonfinite history adjoint contraction"));
        }
        Ok(HistoryAdjoint {
            gradients: grad,
            adjoints: lam,
            initial_state_covectors: initial_covector,
            adjoint_factorizations: ns - 1,
            adjoint_factorization_builds: ns - 1 - reuses,
            adjoint_factorization_reuses: reuses,
            maximum_transpose_relative_residual: rel.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            history_states_retained: ns,
            history_derivative: "discrete_implicit_all_steps",
            initial_state_design_derivative: None,
            initial_state_design_gradient_norm: None,
            residual_error_bounds: Vec::new(),
        })
    }
}

fn residual_error_bounds(adjoints: &[DenseMatrix], residual_norms: &[f64], m: usize) -> Vec<f64> {
    let mut bounds = vec![0.0; m];
    for (lam, r) in adjoints.iter().skip(1).zip(residual_norms) {
        let mut squares = vec![0.0; m];
        for row in lam.data.chunks_exact(m.max(1)) {
            for (s, v) in squares.iter_mut().zip(row) {
                *s += v * v;
            }
        }
        for (b, s) in bounds.iter_mut().zip(squares) {
            *b += s.sqrt() * r;
        }
    }
    bounds
}

impl Drop for NativeHistorySystem {
    fn drop(&mut self) {
        self.discard_exact_factorization_reuse();
    }
}



pub fn validated_solution(solution: &HistorySolution) -> CaeResult<Vec<Vec<f64>>> {
    let states = &solution.states;
    if states.len() < 2 {
        return Err(CaeError::contract(
            "history adjoint requires an initial state and at least one solved step",
        ));
    }
    let size = states[0].len();
    for (i, s) in states.iter().enumerate() {
        if s.is_empty() || s.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(format!("history state {i} has invalid shape or values")));
        }
        if s.len() != size {
            return Err(CaeError::contract("history states must have one identical finite shape"));
        }
    }
    if solution.residual_norms.len() != states.len() - 1
        || solution.newton_iterations.len() != states.len() - 1
    {
        return Err(CaeError::contract("history solution diagnostics do not match its time states"));
    }
    if solution.residual_norms.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err(CaeError::contract("history solution contains an invalid residual norm"));
    }
    Ok(states.clone())
}

