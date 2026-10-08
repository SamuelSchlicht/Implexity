// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::{Arc, Mutex, MutexGuard};

use implexity_core::error::{CaeError, CaeResult};
use implexity_core::py_repr::repr_float;
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::{CscMatrix, CsrMatrix};
use serde_json::{Value, json};

use crate::certificate::norm2;
use crate::convergence::{ConvergenceReport, Criterion, ScalarL2Criterion, repr_name_list};
use crate::diagnostics::ResidualPartition;
use crate::exact_matrix::{AdmittedExactMatrix, ExactMatrixIdentity, admit_exact_matrix};
use crate::factorization::Factorization;
use crate::linear_workspace::{
    CertifiedFactorization, ExactFactorizationTransaction, ExactFactorizationWorkspace, TransactionOptions,
};
use crate::matrix::{Jacobian, MatrixAction, checked_matrix, checked_product_block, py_shape};
use crate::newton_krylov::{
    AssembledDispatch, DirectResult, ExactKrylovPolicy, ExactKrylovRecycleStore,
    ExactKrylovRecycleTransaction, ExactLinearization, MatrixFreeDispatch, MatrixFreeLinearization,
    OperatorIdentity, solve_matrix_free_with_assembled_fallback, solve_with_authoritative_fallback,
    verify_matrix_free_equivalence,
};
use crate::nonsmooth::SWITCH_CERTIFICATE;
use crate::operation_context::OperationExecutionContext;
use crate::preconditioner_lease::{
    DEFAULT_REUSE_QUALITY_LIMIT, ExactPreconditionerBinding, ExactPreconditionerLeaseBudget,
    ExactPreconditionerReuseStore, ExactPreconditionerReuseTransaction, ProviderPreconditioner,
    SealedMatrixReadLease,
};
use crate::pyfmt::{fmt_e, fmt_e0, fmt_g};
use crate::trace::{self, Fields};

pub trait BlockCallbacks<C: ?Sized>: Send + Sync {


    fn residual(&self, u: &[f64], design: &[f64], context: &C) -> CaeResult<Vec<f64>>;


    fn state_jacobian(&self, u: &[f64], design: &[f64], context: &C) -> CaeResult<Jacobian>;


    fn design_jacobian(&self, u: &[f64], design: &[f64], context: &C) -> CaeResult<Jacobian>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplicitSolveResult {
    pub state: Vec<f64>,
    pub residual_norm: f64,
    pub iterations: usize,
    pub converged: bool,
    pub condition_number: f64,
    pub convergence: Option<ConvergenceReport>,
    pub relaxed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdjointResult {
    pub adjoint: Vec<f64>,
    pub gradient: Vec<f64>,
    pub residual_norm: f64,
    pub transpose_residual_norm: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BatchedAdjointResult {
    pub adjoints: DenseMatrix,
    pub gradients: DenseMatrix,
    pub residual_norm: f64,
    pub transpose_residual_norms: Vec<f64>,
    pub transpose_relative_residuals: Vec<f64>,
    pub condition_number: f64,
    pub factorization: String,
}

pub type CapabilityFactory<C> = Arc<
    dyn Fn(&ExactMatrixIdentity, &[f64], &[f64], &C) -> CaeResult<Box<dyn ExactLinearization>> + Send + Sync,
>;
pub type MatrixFreeFactory<C> =
    Arc<dyn Fn(&[f64], &[f64], &C) -> CaeResult<Box<dyn MatrixFreeLinearization>> + Send + Sync>;
pub type PreconditionerFactory<C> = Arc<
    dyn Fn(
            &SealedMatrixReadLease,
            &ExactPreconditionerBinding,
            &[f64],
            &[f64],
            &C,
        ) -> CaeResult<Box<dyn ProviderPreconditioner>>
        + Send
        + Sync,
>;

pub struct BlockOptions<C: ?Sized> {
    pub tolerance: f64,
    pub max_iterations: usize,
    pub condition_limit: f64,
    pub krylov_policy: Option<ExactKrylovPolicy>,
    pub capability_factory: Option<CapabilityFactory<C>>,
    pub matrix_free_factory: Option<MatrixFreeFactory<C>>,
    pub matrix_free_bootstrap: bool,
    pub preconditioner_factory: Option<PreconditionerFactory<C>>,
    pub lease_budget: Option<ExactPreconditionerLeaseBudget>,
    pub authority_policy_sha256: Option<String>,
    pub authority_operation_sha256: Option<String>,
    pub authority_provider_profile_sha256: Option<String>,
    pub execution_context: Option<OperationExecutionContext>,
    pub trace_context: Option<OperationExecutionContext>,
    pub criterion: Option<Criterion>,
    pub residual_partition: Option<ResidualPartition>,
    pub relaxed_tolerance: Option<f64>,
    pub local_elimination_partition: Option<Arc<crate::local_condensation::LocalEliminationPartition>>,
    pub rejected_state_capture: Option<crate::rejected_state::RejectedStateCapture>,
    pub rejected_state_context: Option<Arc<dyn Fn(&C) -> Vec<f64> + Send + Sync>>,
}

impl<C: ?Sized> Clone for BlockOptions<C> {
    fn clone(&self) -> Self {
        Self {
            tolerance: self.tolerance,
            max_iterations: self.max_iterations,
            condition_limit: self.condition_limit,
            krylov_policy: self.krylov_policy,
            capability_factory: self.capability_factory.clone(),
            matrix_free_factory: self.matrix_free_factory.clone(),
            matrix_free_bootstrap: self.matrix_free_bootstrap,
            preconditioner_factory: self.preconditioner_factory.clone(),
            lease_budget: self.lease_budget,
            authority_policy_sha256: self.authority_policy_sha256.clone(),
            authority_operation_sha256: self.authority_operation_sha256.clone(),
            authority_provider_profile_sha256: self.authority_provider_profile_sha256.clone(),
            execution_context: self.execution_context.clone(),
            trace_context: self.trace_context.clone(),
            criterion: self.criterion.clone(),
            residual_partition: self.residual_partition.clone(),
            relaxed_tolerance: self.relaxed_tolerance,
            local_elimination_partition: self.local_elimination_partition.clone(),
            rejected_state_capture: self.rejected_state_capture.clone(),
            rejected_state_context: self.rejected_state_context.clone(),
        }
    }
}

impl<C: ?Sized> Default for BlockOptions<C> {
    fn default() -> Self {
        Self {
            tolerance: 1e-10,
            max_iterations: 40,
            condition_limit: 1e12,
            krylov_policy: None,
            capability_factory: None,
            matrix_free_factory: None,
            matrix_free_bootstrap: false,
            preconditioner_factory: None,
            lease_budget: None,
            authority_policy_sha256: None,
            authority_operation_sha256: None,
            authority_provider_profile_sha256: None,
            execution_context: None,
            trace_context: None,
            criterion: None,
            residual_partition: None,
            relaxed_tolerance: None,
            local_elimination_partition: None,
            rejected_state_capture: None,
            rejected_state_context: None,
        }
    }
}

#[derive(Default)]
struct Staging {
    workspace: Option<ExactFactorizationWorkspace>,
    pending: Option<ExactFactorizationTransaction>,
}

pub struct ImplicitBlockSystem<C: ?Sized> {
    callbacks: Arc<dyn BlockCallbacks<C>>,
    options: BlockOptions<C>,
    criterion: Criterion,
    operation_trace: Fields,
    staging: Mutex<Staging>,
}

impl<C: ?Sized> std::fmt::Debug for ImplicitBlockSystem<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImplicitBlockSystem")
            .field("tolerance", &self.options.tolerance)
            .field("max_iterations", &self.options.max_iterations)
            .field("condition_limit", &self.options.condition_limit)
            .finish_non_exhaustive()
    }
}

fn negate(v: &[f64]) -> Vec<f64> {
    v.iter().map(|x| -x).collect()
}

#[allow(clippy::float_cmp)]
fn bits_equal(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y || (x.is_nan() && y.is_nan()))
}

impl<C: ?Sized + Sync> ImplicitBlockSystem<C> {


    #[allow(clippy::too_many_lines)]
    pub fn new(callbacks: Arc<dyn BlockCallbacks<C>>, options: BlockOptions<C>) -> CaeResult<Self> {
        if !options.tolerance.is_finite() || options.tolerance <= 0.0 {
            return Err(CaeError::contract("residual tolerance must be finite and positive"));
        }
        if !options.condition_limit.is_finite() || options.condition_limit <= 1.0 {
            return Err(CaeError::contract("condition limit must be finite and greater than one"));
        }
        if let Some(relaxed) = options.relaxed_tolerance
            && (!relaxed.is_finite() || relaxed < options.tolerance)
        {
            return Err(CaeError::contract(
                "relaxed residual tolerance must be finite and at least the strict tolerance",
            ));
        }
        if let Some(capture) = &options.rejected_state_capture { capture.validate()?; }
        let criterion = match &options.criterion {
            Some(c) => Arc::clone(c),
            None => ScalarL2Criterion::shared(options.tolerance)?,
        };
        if let Some(p) = options.krylov_policy {
            p.validate()?;
        }
        if options.capability_factory.is_some() && options.preconditioner_factory.is_some() {
            return Err(CaeError::contract(
                "exact Krylov operation must select one provider integration path",
            ));
        }
        if options.capability_factory.is_some() && options.matrix_free_factory.is_some() {
            return Err(CaeError::contract("exact Krylov operation cannot select two operator capabilities"));
        }
        for (value, label) in [
            (&options.authority_policy_sha256, "policy"),
            (&options.authority_operation_sha256, "operation"),
            (&options.authority_provider_profile_sha256, "provider profile"),
        ] {
            if let Some(v) = value
                && !crate::operation_context::is_sha256_hex(v)
            {
                return Err(CaeError::contract(format!(
                    "exact Krylov authority {label} identity must be a lowercase SHA-256 digest"
                )));
            }
        }
        if let (Some(a), Some(b)) = (&options.execution_context, &options.trace_context)
            && a != b
        {
            return Err(CaeError::contract("numerical and trace operation contexts must match"));
        }
        let trace_context = options.trace_context.clone().or_else(|| options.execution_context.clone());
        let operation_trace = crate::operation_context::trace_fields_of(trace_context.as_ref());
        if options.preconditioner_factory.is_some() && options.execution_context.is_none() {
            return Err(CaeError::contract("sealed exact preconditioner requires an operation context"));
        }
        let enabled = options.krylov_policy.is_some_and(|p| p.enabled);
        if options.matrix_free_factory.is_some() && !enabled {
            return Err(CaeError::contract("matrix-free exact capability requires an enabled Krylov policy"));
        }
        if options.matrix_free_bootstrap && options.matrix_free_factory.is_none() {
            return Err(CaeError::contract(
                "matrix-free exact bootstrap requires an exact action capability",
            ));
        }
        if options.matrix_free_factory.is_some() && options.preconditioner_factory.is_none() {
            return Err(CaeError::contract(
                "matrix-free exact capability requires an assembled-refresh preconditioner",
            ));
        }
        Ok(Self { callbacks, options, criterion, operation_trace, staging: Mutex::new(Staging::default()) })
    }

    #[must_use]
    pub fn tolerance(&self) -> f64 {
        self.options.tolerance
    }
    #[must_use]
    pub fn max_iterations(&self) -> usize {
        self.options.max_iterations
    }
    #[must_use]
    pub fn condition_limit(&self) -> f64 {
        self.options.condition_limit
    }
    #[must_use]
    pub fn criterion(&self) -> &Criterion {
        &self.criterion
    }
    #[must_use]
    pub fn callbacks(&self) -> &Arc<dyn BlockCallbacks<C>> {
        &self.callbacks
    }

    fn staging(&self) -> MutexGuard<'_, Staging> {
        self.staging.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn require_frame(&self, size: usize, label: &str) -> CaeResult<()> {
        if let Some(bound) = self.criterion.state_size()
            && bound != size
        {
            return Err(CaeError::contract(format!(
                "{label}: convergence criterion is bound to {bound} unknowns but the state has {size} (solver ImplicitBlockSystem); author the criterion in this solver's residual frame"
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

    fn with_op(&self, extra: Fields) -> Fields {
        let mut f = extra;
        f.extend(self.operation_trace.clone());
        f
    }



    pub fn install_exact_factorization_workspace(
        &self,
        workspace: ExactFactorizationWorkspace,
    ) -> CaeResult<()> {
        let mut s = self.staging();
        if s.workspace.is_some() || s.pending.is_some() {
            return Err(CaeError::contract("implicit exact factorization workspace is already installed"));
        }
        s.workspace = Some(workspace);
        Ok(())
    }

    #[must_use]
    pub fn take_staged_exact_factorization(&self) -> Option<ExactFactorizationTransaction> {
        let mut s = self.staging();
        s.workspace = None;
        s.pending.take()
    }

    pub fn discard_staged_exact_factorization(&self) {
        let pending = {
            let mut s = self.staging();
            s.workspace = None;
            s.pending.take()
        };
        if let Some(mut t) = pending {
            let _ = t.rollback();
        }
    }

    fn residual_checked(&self, u: &[f64], design: &[f64], context: &C) -> CaeResult<Vec<f64>> {
        let r = self.callbacks.residual(u, design, context)?;
        if r.len() != u.len() {
            return Err(CaeError::contract(format!(
                "implicit residual must have exact shape ({},), got ({},)",
                u.len(),
                r.len()
            )));
        }
        if r.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("implicit residual has nonfinite values"));
        }
        Ok(r)
    }

    fn state_jacobian_traced(
        &self,
        u: &[f64],
        design: &[f64],
        context: &C,
        fields: &Fields,
    ) -> CaeResult<Jacobian> {
        trace::span(
            "state_jacobian_callback",
            || fields.clone(),
            || self.callbacks.state_jacobian(u, design, context),
        )
    }

    fn converged_gate(&self, matrix: Jacobian, size: usize, fields: &Fields) -> CaeResult<f64> {
        let workspace = self.staging().workspace.clone();
        let Some(workspace) = workspace else {
            return Ok(
                Factorization::new_with_partition(matrix, size, self.options.condition_limit, Some(fields), self.options.local_elimination_partition.as_deref())?.condition()
            );
        };
        if self.staging().pending.is_some() {
            return Err(CaeError::contract("implicit solve already staged an exact factorization"));
        }
        let admitted = admit_exact_matrix(matrix, Some(size))?;
        let limit = self.options.condition_limit;
        let tx = workspace.transaction(
            &admitted,
            size,
            limit,
            |a: &AdmittedExactMatrix| -> CaeResult<Arc<dyn CertifiedFactorization>> {
                Ok(Arc::new(Factorization::new_with_partition(a, size, limit, Some(fields), self.options.local_elimination_partition.as_deref())?))
            },
            TransactionOptions { discard_committed_on_miss: true, require_committed_match: false },
        )?;
        let condition = tx.condition()?;
        self.staging().pending = Some(tx);
        Ok(condition)
    }

    fn captured_gate(&self, matrix: Jacobian, state: &[f64], design: &[f64], context: &C, residual: Option<&[f64]>, fields: &Fields, tier: &str) -> CaeResult<f64> {
        let retained = self.options.rejected_state_capture.as_ref().map(|_| matrix.clone());
        let result = self.converged_gate(matrix, state.len(), fields);
        if let Err(error) = &result
            && matches!(error, CaeError::Convergence(_))
            && let (Some(capture), Some(matrix)) = (&self.options.rejected_state_capture, retained)
        {
            let mut metadata = fields.clone();
            metadata.insert("convergence_tier".into(), Value::String(tier.into()));
            metadata.insert("failure_scope".into(), Value::String("converged_state_factorization_or_condition_gate".into()));
            metadata.insert("condition_limit".into(), Value::from(self.options.condition_limit));
            metadata.insert("strict_tolerance".into(), Value::from(self.options.tolerance));
            metadata.insert("relaxed_tolerance".into(), self.options.relaxed_tolerance.map_or(Value::Null, Value::from));
            let evaluated;
            let residual = if let Some(r) = residual { r } else {
                match self.residual_checked(state, design, context) {
                    Ok(r) => { evaluated = r; &evaluated }
                    Err(failure) => { eprintln!("rejected-state residual capture failed: {failure}; original numerical error retained: {error}"); return result; }
                }
            };
            if let Some(partition) = &self.options.residual_partition
                && let Ok(norms) = partition.norms(residual)
            { metadata.insert("field_residual_norms".into(), Value::Object(norms)); }
            let previous = self.options.rejected_state_context.as_ref().map(|f| f(context));
            let captured = capture.record(state, design, previous.as_deref(), residual, &matrix, &metadata, error);
            if let Err(failure) = &captured {
                eprintln!("rejected-state capture failed: {failure}; original numerical error retained: {error}");
            }
        }
        result
    }

    fn direct_correction(
        &self,
        matrix: Jacobian,
        residual: &[f64],
        size: usize,
        fields: &Fields,
    ) -> CaeResult<Vec<f64>> {
        let fac = Factorization::correction_with_partition(matrix, size, Some(fields), self.options.local_elimination_partition.as_deref())?;
        Ok(fac.solve(&negate(residual), false)?.0)
    }

    fn direct_callback<'a>(
        &'a self,
        matrix: DirectMatrix<'a, C>,
        size: usize,
        fields: &'a Fields,
    ) -> impl FnMut(&[f64], bool) -> CaeResult<DirectResult> + 'a {
        move |rhs: &[f64], transpose: bool| {
            let fac = match &matrix {
                DirectMatrix::Admitted(a) => {
                    Factorization::new_with_partition(*a, size, self.options.condition_limit, Some(fields), self.options.local_elimination_partition.as_deref())?
                }
                DirectMatrix::Assemble { u, design, context } => {
                    let m = self.state_jacobian_traced(u, design, context, fields)?;
                    Factorization::new_with_partition(m, size, self.options.condition_limit, Some(fields), self.options.local_elimination_partition.as_deref())?
                }
            };
            let (x, e, r) = fac.solve(rhs, transpose)?;
            Ok(DirectResult {
                solution: x,
                error_norm: e,
                relative: r,
                condition: fac.condition(),
                kind: fac.kind().into(),
            })
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn krylov_correction(
        &self,
        matrix: Jacobian,
        residual: &[f64],
        u: &[f64],
        design: &[f64],
        context: &C,
        fields: &Fields,
        recycle_store: Option<&ExactKrylovRecycleStore>,
        reuse_store: Option<&ExactPreconditionerReuseStore>,
    ) -> CaeResult<Correction> {
        let size = u.len();
        let policy = match self.options.krylov_policy {
            Some(p) if p.enabled => p,
            _ => {
                return Ok(Correction::direct(
                    self.direct_correction(matrix, residual, size, fields)?,
                    false,
                ));
            }
        };
        if !matrix.is_sparse() {
            return Ok(Correction::direct(self.direct_correction(matrix, residual, size, fields)?, false));
        }
        let Some(recycle_store) = recycle_store else {
            return Err(CaeError::contract(
                "enabled exact Krylov solve requires an operation-scoped recycle store",
            ));
        };
        let admitted = admit_exact_matrix(matrix, Some(size))?;
        let recycle = recycle_store.begin(
            "implicit-newton-primal",
            size,
            &OperatorIdentity::Matrix(admitted.identity().clone()),
        )?;
        let reuse = match (&self.options.preconditioner_factory, reuse_store) {
            (Some(_), None) => {
                let _ = recycle.rollback();
                return Err(CaeError::contract(
                    "enabled sealed preconditioning requires a solve-local reuse store",
                ));
            }
            (Some(_), Some(store)) => match store.begin(&admitted) {
                Ok(t) => Some(t),
                Err(e) => {
                    let _ = recycle.rollback();
                    return Err(e);
                }
            },
            _ => None,
        };
        let mut capability_factory = self.options.capability_factory.as_ref().map(|f| {
            let f = Arc::clone(f);
            move |id: &ExactMatrixIdentity| f(id, u, design, context)
        });
        let mut preconditioner_factory = self.options.preconditioner_factory.as_ref().map(|f| {
            let f = Arc::clone(f);
            move |lease: &SealedMatrixReadLease, binding: &ExactPreconditionerBinding| {
                f(lease, binding, u, design, context)
            }
        });
        let mut direct = self.direct_callback(DirectMatrix::Admitted(&admitted), size, fields);
        let dispatched = solve_with_authoritative_fallback(
            &admitted,
            &negate(residual),
            AssembledDispatch {
                condition_limit: self.options.condition_limit,
                policy,
                direct_solve: &mut direct,
                capability_factory: capability_factory.as_mut().map(|f| {
                    f as &mut dyn FnMut(&ExactMatrixIdentity) -> CaeResult<Box<dyn ExactLinearization>>
                }),
                preconditioner_factory: preconditioner_factory.as_mut().map(|f| {
                    f as &mut dyn FnMut(
                        &SealedMatrixReadLease,
                        &ExactPreconditionerBinding,
                    ) -> CaeResult<Box<dyn ProviderPreconditioner>>
                }),
                execution_context: self.options.execution_context.as_ref(),
                lease_budget: self.options.lease_budget,
                recycle: Some(&recycle),
                reuse: reuse.as_ref(),
                transpose: false,
                trace_fields: Some(fields.clone()),
                defer_condition_to_final: true,
            },
        );
        let dispatched = match dispatched {
            Ok(d) => d,
            Err(e) => {
                let _ = recycle.release();
                if let Some(t) = &reuse
                    && !t.closed()
                {
                    let _ = t.rollback();
                }
                return Err(e);
            }
        };
        let iterative = dispatched.diagnostics.iterative_solve.clone();
        trace::point("exact_krylov_dispatch", || {
            Self::with_dispatch_fields(
                &dispatched.diagnostics,
                dispatched.condition_estimate,
                iterative.as_ref(),
                fields,
                true,
            )
        })?;
        if dispatched.diagnostics.fallback_used {
            let _ = recycle.release();
            if let Some(t) = &reuse
                && !t.closed()
            {
                let _ = t.rollback();
            }
            return Ok(Correction::direct(dispatched.solution, true));
        }
        if recycle.closed() {
            return Err(CaeError::contract(
                "successful exact Krylov correction closed its recycle transaction",
            ));
        }
        if self.options.preconditioner_factory.is_some()
            && reuse.as_ref().is_none_or(ExactPreconditionerReuseTransaction::closed)
        {
            let _ = recycle.rollback();
            return Err(CaeError::contract(
                "successful exact Krylov correction closed its preconditioner reuse transaction",
            ));
        }
        Ok(Correction { step: dispatched.solution, recycle: Some(recycle), reuse, fallback: false })
    }

    fn with_dispatch_fields(
        d: &crate::newton_krylov::ExactKrylovDispatchDiagnostics,
        condition: Option<f64>,
        iterative: Option<&crate::newton_krylov::ExactKrylovSolveDiagnostics>,
        fields: &Fields,
        include_recycled: bool,
    ) -> Fields {
        let mut f = crate::trace_fields! {
            "requested_backend" => d.requested_backend,
            "used_backend" => d.used_backend,
            "fallback_used" => d.fallback_used,
            "fallback_reason" => d.fallback_reason,
            "iterative_iterations" => iterative.map(|i| i.iterations),
            "iterative_relative_residual" => iterative.map(|i| i.relative_residual),
            "iterative_preconditioner_used" => iterative.is_some_and(|i| i.preconditioner_used),
            "admitted_condition_estimate" => condition,
            "condition_estimate_deferred" => condition.is_none(),
        };
        if include_recycled {
            f.insert(
                "iterative_recycled_vector_count".into(),
                json!(iterative.map_or(0, |i| i.recycled_vector_count)),
            );
        }
        f.extend(fields.clone());
        f
    }

    fn matrix_free_probe(
        &self,
        matrix: &Jacobian,
        u: &[f64],
        design: &[f64],
        context: &C,
    ) -> CaeResult<bool> {
        let Some(factory) = &self.options.matrix_free_factory else { return Ok(true) };
        let capability = factory(u, design, context).map_err(|e| {
            CaeError::contract(format!("matrix-free exact capability construction failed: {}", e.message()))
        })?;
        let verified = verify_matrix_free_equivalence(capability.as_ref(), matrix.clone());
        let released = capability.release();
        match (verified, released) {
            (Err(crate::newton_krylov::KrylovError::OperatorMismatch(m)), Err(r)) => {
                Err(CaeError::contract(format!("{m}; capability release also failed: {}", r.message())))
            }
            (_, Err(r)) => Err(r),
            (Err(crate::newton_krylov::KrylovError::OperatorMismatch(_)), Ok(())) => Ok(false),
            (Err(crate::newton_krylov::KrylovError::Contract(e)), Ok(())) => Err(e),
            (Err(other), Ok(())) => Err(CaeError::contract(other.to_string())),
            (Ok(_), Ok(())) => Ok(true),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn matrix_free_correction(
        &self,
        residual: &[f64],
        u: &[f64],
        design: &[f64],
        context: &C,
        fields: &Fields,
        recycle_store: &ExactKrylovRecycleStore,
        reuse_store: &ExactPreconditionerReuseStore,
    ) -> CaeResult<Correction> {
        let size = u.len();
        let policy = self.options.krylov_policy.unwrap_or_default();
        let Some(factory) = self.options.matrix_free_factory.clone() else {
            return Err(CaeError::contract("matrix-free exact capability factory is missing"));
        };
        let capability = factory(u, design, context).map_err(|e| {
            CaeError::contract(format!("matrix-free exact capability construction failed: {}", e.message()))
        })?;
        if !reuse_store.has_committed() && !capability.has_preconditioner() {
            let _ = capability.release();
            return Err(CaeError::contract(
                "matrix-free exact bootstrap was selected but the provider did not supply a preconditioner",
            ));
        }
        let identity = OperatorIdentity::Opaque(capability.identity());
        let Some(execution) = self.options.execution_context.as_ref() else {
            let _ = capability.release();
            return Err(CaeError::contract("matrix-free exact solve requires an operation context"));
        };
        let recycle = match recycle_store.begin("implicit-newton-primal", size, &identity) {
            Ok(t) => t,
            Err(e) => {
                let _ = capability.release();
                return Err(e);
            }
        };
        let reuse = match reuse_store.begin_opaque(&identity, (size, size), execution.scope_digest()) {
            Ok(t) => t,
            Err(e) => {
                let _ = recycle.rollback();
                let _ = capability.release();
                return Err(e);
            }
        };
        let mut refresh = || factory(u, design, context);
        let mut direct = self.direct_callback(DirectMatrix::Assemble { u, design, context }, size, fields);
        let dispatched = solve_matrix_free_with_assembled_fallback(
            capability,
            &negate(residual),
            MatrixFreeDispatch {
                condition_limit: self.options.condition_limit,
                policy,
                direct_solve: &mut direct,
                lagged_preconditioner: None,
                recycle: Some(&recycle),
                before_fallback: None,
                reuse: Some(&reuse),
                refresh_capability: Some(&mut refresh),
                transpose: false,
                trace_fields: Some(fields.clone()),
                defer_condition_to_final: true,
            },
        );
        let dispatched = match dispatched {
            Ok(d) => d,
            Err(e) => {
                let _ = recycle.release();
                if !reuse.closed() {
                    let _ = reuse.rollback();
                }
                return Err(e);
            }
        };
        let iterative = dispatched.diagnostics.iterative_solve.clone();
        trace::point("exact_matrix_free_dispatch", || {
            Self::with_dispatch_fields(
                &dispatched.diagnostics,
                dispatched.condition_estimate,
                iterative.as_ref(),
                fields,
                false,
            )
        })?;
        if dispatched.diagnostics.fallback_used {
            return Ok(Correction::direct(dispatched.solution, true));
        }
        if recycle.closed() {
            return Err(CaeError::contract(
                "successful matrix-free exact correction closed its recycle transaction",
            ));
        }
        if reuse.closed() {
            let _ = recycle.rollback();
            return Err(CaeError::contract(
                "successful matrix-free exact correction closed its preconditioner reuse transaction",
            ));
        }
        Ok(Correction {
            step: dispatched.solution,
            recycle: Some(recycle),
            reuse: Some(reuse),
            fallback: false,
        })
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial_state: &[f64],
        context: &C,
    ) -> CaeResult<ImplicitSolveResult> {
        if initial_state.is_empty()
            || design.is_empty()
            || design.iter().any(|v| !v.is_finite())
            || initial_state.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "implicit block system requires finite nonempty design and state",
            ));
        }
        self.require_frame(initial_state.len(), "ImplicitBlockSystem.solve")?;
        let solve_id = trace::new_trace_id("implicit-solve")?;
        let enabled = self.options.krylov_policy.is_some_and(|p| p.enabled);
        let recycle_store = enabled.then(ExactKrylovRecycleStore::new);
        let reuse_store = if enabled && self.options.preconditioner_factory.is_some() {
            let Some(profile) = self.options.authority_policy_sha256.as_deref() else {
                return Err(CaeError::contract(
                    "sealed preconditioner reuse requires an exact profile identity",
                ));
            };
            Some(ExactPreconditionerReuseStore::new(profile, DEFAULT_REUSE_QUALITY_LIMIT)?)
        } else {
            None
        };
        let result = trace::lifecycle(
            "implicit_solve",
            || {
                let mut f = crate::trace_fields! {
                    "solve_id" => solve_id, "state_size" => initial_state.len(), "design_size" => design.len(),
                    "tolerance" => self.options.tolerance, "maximum_iterations" => self.options.max_iterations,
                    "condition_limit" => self.options.condition_limit,
                };
                f.extend(self.operation_trace.clone());
                f.extend(self.criterion_trace());
                f
            },
            || {
                self.solve_validated(
                    design,
                    initial_state,
                    context,
                    &solve_id,
                    recycle_store.as_ref(),
                    reuse_store.as_ref(),
                )
            },
            |r| {
                crate::trace_fields! {
                    "iterations" => r.iterations, "residual_norm" => r.residual_norm,
                    "condition_estimate" => r.condition_number,
                }
            },
        );
        if let Some(s) = &reuse_store {
            let _ = s.release();
        }
        if let Some(s) = &recycle_store {
            s.release();
        }
        if result.is_err() {
            self.discard_staged_exact_factorization();
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    fn solve_validated(
        &self,
        design: &[f64],
        initial: &[f64],
        context: &C,
        solve_id: &str,
        recycle_store: Option<&ExactKrylovRecycleStore>,
        reuse_store: Option<&ExactPreconditionerReuseStore>,
    ) -> CaeResult<ImplicitSolveResult> {
        let mut u = initial.to_vec();
        let n = u.len();
        let mut minimum_accepted_alpha: Option<f64> = None;
        let mut below_cutoff = 0usize;
        let mut accepted_residual: Option<(Vec<f64>, f64)> = None;
        let mut matrix_free_enabled = self.options.matrix_free_factory.is_some();
        let partition = self.options.residual_partition.as_ref();
        if let Some(p) = partition
            && p.state_size() != n
        {
            return Err(CaeError::contract("residual diagnostic partition frame mismatch"));
        }
        let per_field = !self.criterion.is_scalar();
        let mut last_report: Option<ConvergenceReport> = None;
        let mut direct_only = false;
        let mut rn = 0.0;
        for it in 0..=self.options.max_iterations {
            let r = if let Some((r, norm)) = accepted_residual.take() {
                rn = norm;
                r
            } else {
                let r = trace::span(
                    "nonlinear_residual",
                    || crate::trace_fields! {"solve_id" => solve_id, "newton_iteration" => it},
                    || self.residual_checked(&u, design, context),
                )?;
                rn = norm2(&r);
                r
            };
            let report = self.criterion.assess(&r, Some(rn))?;
            trace::point("newton_iteration", || {
                let mut f = crate::trace_fields! {"solve_id" => solve_id, "newton_iteration" => it, "residual_norm" => rn};
                if let Some(p) = partition
                    && let Ok(norms) = p.norms(&r)
                {
                    f.insert("field_residual_norms".into(), Value::Object(norms));
                }
                if per_field {
                    f.insert("normalized_residuals".into(), pairs_value(&report.normalized));
                    f.insert("fields_passed".into(), pairs_bool(&report.passed));
                }
                f
            })?;
            if report.certified {
                if let Some(s) = reuse_store {
                    s.release()?;
                }
                let fields = self.with_op(crate::trace_fields! {
                    "solve_id" => solve_id, "newton_iteration" => it, "purpose" => "converged_state_condition_gate",
                });
                let matrix = self.state_jacobian_traced(&u, design, context, &fields)?;
                let condition = self.captured_gate(matrix, &u, design, context, Some(&r), &fields, "strict")?;
                return Ok(ImplicitSolveResult {
                    state: u,
                    residual_norm: rn,
                    iterations: it,
                    converged: true,
                    condition_number: condition,
                    convergence: Some(report),
                    relaxed: false,
                });
            }
            last_report = Some(report);
            if it == self.options.max_iterations {
                break;
            }
            let fields = self.with_op(crate::trace_fields! {
                "solve_id" => solve_id, "newton_iteration" => it, "purpose" => "newton_correction",
            });
            let correction = if direct_only {
                let m = self.state_jacobian_traced(&u, design, context, &fields)?;
                Correction::direct(self.direct_correction(m, &r, n, &fields)?, false)
            } else if matrix_free_enabled
                && let (Some(rs), Some(ps)) = (recycle_store, reuse_store)
                && (self.options.matrix_free_bootstrap || ps.has_committed())
            {
                let c = self.matrix_free_correction(&r, &u, design, context, &fields, rs, ps)?;
                direct_only = Self::direct_after_fallback(c.fallback, &fields)?;
                c
            } else {
                let matrix = self.state_jacobian_traced(&u, design, context, &fields)?;
                if matrix_free_enabled {
                    let admitted = self.matrix_free_probe(&matrix, &u, design, context)?;
                    if admitted {
                        trace::point("exact_matrix_free_refresh", || {
                            self.with_op(crate::trace_fields! {"admitted" => true, "reason" => Value::Null,
                                "solve_id" => solve_id, "newton_iteration" => it, "purpose" => "newton_correction"})
                        })?;
                    } else {
                        matrix_free_enabled = false;
                        trace::point("exact_matrix_free_refresh", || {
                            self.with_op(crate::trace_fields! {"admitted" => false, "reason" => "operator_mismatch",
                                "solve_id" => solve_id, "newton_iteration" => it, "purpose" => "newton_correction"})
                        })?;
                    }
                }
                let c = self.krylov_correction(
                    matrix,
                    &r,
                    &u,
                    design,
                    context,
                    &fields,
                    recycle_store,
                    reuse_store,
                )?;
                direct_only = Self::direct_after_fallback(c.fallback, &fields)?;
                c
            };
            let Correction { step: du, mut recycle, mut reuse, .. } = correction;
            let mut alpha = 1.0_f64;
            let mut accepted = false;
            let mut causes: Vec<String> = Vec::new();
            let mut trial_count = 0usize;
            let mut smallest: Option<f64> = None;
            let mut best_norm = f64::INFINITY;
            let mut best_alpha: Option<f64> = None;
            let mut invalid = 0usize;
            let mut last_cause: Option<String> = None;
            let mut termination: Option<&str> = None;
            let mut termination_alpha: Option<f64> = None;
            loop {
                if !alpha.is_finite() || alpha <= 0.0 {
                    termination = Some("alpha_underflow");
                    termination_alpha = Some(alpha);
                    break;
                }
                let trial: Vec<f64> = u.iter().zip(&du).map(|(a, d)| a + alpha * d).collect();
                if bits_equal(&trial, &u) {
                    termination = Some("trial_state_unchanged");
                    termination_alpha = Some(alpha);
                    break;
                }
                let bound = (1.0 - 1e-4 * alpha) * rn;
                if bound >= rn || bound.is_nan() {
                    termination = Some("armijo_decrease_unrepresentable");
                    termination_alpha = Some(alpha);
                    break;
                }
                trial_count += 1;
                smallest = Some(alpha);
                let evaluated = trace::span(
                    "line_search_residual",
                    || {
                        crate::trace_fields! {"solve_id" => solve_id, "newton_iteration" => it,
                        "trial_count" => trial_count, "alpha" => alpha}
                    },
                    || self.residual_checked(&trial, design, context),
                );
                let (tr, trial_norm) = match evaluated {
                    Ok(tr) => {
                        let tn = norm2(&tr);
                        if tn.is_finite() && tn < best_norm {
                            best_norm = tn;
                            best_alpha = Some(alpha);
                        }
                        (Some(tr), tn)
                    }
                    Err(e) if e.is_convergence() => {
                        let cause = e.message().trim().to_string();
                        if !cause.is_empty() && !causes.contains(&cause) && causes.len() < 4 {
                            causes.push(cause.clone());
                        }
                        invalid += 1;
                        last_cause =
                            Some(if cause.is_empty() { e.python_class().to_string() } else { cause });
                        (None, f64::INFINITY)
                    }
                    Err(e) => return Err(e),
                };
                if trial_norm <= bound
                    && let Some(tr) = tr
                {
                    u = trial;
                    accepted_residual = Some((tr, trial_norm));
                    accepted = true;
                    if let Some(t) = reuse.take() {
                        t.commit()?;
                    }
                    if let Some(t) = recycle.take() {
                        t.commit()?;
                    }
                    minimum_accepted_alpha = Some(minimum_accepted_alpha.map_or(alpha, |m| m.min(alpha)));
                    if alpha < 2f64.powi(-16) {
                        below_cutoff += 1;
                    }
                    break;
                }
                alpha *= 0.5;
            }
            if !accepted {
                if let Some(t) = reuse.take() {
                    let _ = t.rollback();
                }
                if let Some(t) = recycle.take() {
                    let _ = t.rollback();
                }
                let best_norm_text = if best_alpha.is_some() { fmt_g(best_norm, 17) } else { "none".into() };
                let best_alpha_text = best_alpha.map_or_else(|| "none".into(), |a| fmt_g(a, 17));
                let smallest_text = smallest.map_or_else(|| "none".into(), |a| fmt_g(a, 17));
                let termination_alpha_text = match termination_alpha {
                    Some(a) if a.is_finite() => fmt_g(a, 17),
                    Some(a) => repr_float(a),
                    None => "None".into(),
                };
                let min_text = minimum_accepted_alpha.map_or_else(|| "none".into(), repr_float);
                let diagnostics = format!(
                    "; newton_iteration={it}; current_residual_norm={}; trial_count={trial_count}; smallest_attempted_alpha={smallest_text}; best_finite_trial_norm={best_norm_text}; best_finite_trial_alpha={best_alpha_text}; invalid_trial_count={invalid}; last_rejected_cause={}; termination_reason={}; termination_alpha={termination_alpha_text}; minimum_accepted_alpha={min_text}; accepted_below_2^-16_count={below_cutoff}",
                    fmt_g(rn, 17),
                    last_cause.as_deref().unwrap_or("none"),
                    termination.unwrap_or("none"),
                );
                let detail = if causes.is_empty() {
                    String::new()
                } else {
                    format!("; rejected trial states: {}", causes.join(" | "))
                };
                let message =
                    format!("implicit Newton line search failed to reduce residual{diagnostics}{detail}");
                return self.relaxed_or(rn, u, it, design, context, solve_id, reuse_store, message);
            }
        }
        let min_text = minimum_accepted_alpha.map_or_else(|| "none".into(), |a| fmt_g(a, 17));
        let criterion_text = match (&last_report, per_field) {
            (Some(report), true) => format!(
                "; convergence_reason={}; fields_failed={}",
                report.reason,
                repr_name_list(&report.failed_fields())
            ),
            _ => String::new(),
        };
        let message = format!(
            "implicit block system did not converge within {} iterations; final_residual_norm={}; minimum_accepted_alpha={min_text}; accepted_below_2^-16_count={below_cutoff}{criterion_text}",
            self.options.max_iterations,
            fmt_g(rn, 17)
        );
        let it = self.options.max_iterations;
        self.relaxed_or(rn, u, it, design, context, solve_id, reuse_store, message)
    }

    #[allow(clippy::too_many_arguments)]
    fn relaxed_or(
        &self,
        rn: f64,
        u: Vec<f64>,
        it: usize,
        design: &[f64],
        context: &C,
        solve_id: &str,
        reuse_store: Option<&ExactPreconditionerReuseStore>,
        message: String,
    ) -> CaeResult<ImplicitSolveResult> {
        let Some(relaxed) = self.options.relaxed_tolerance else { return Err(CaeError::newton(message)) };
        if !self.criterion.is_scalar() || !rn.is_finite() || rn > relaxed {
            return Err(CaeError::newton(message));
        }
        if let Some(s) = reuse_store {
            s.release()?;
        }
        let cause: String = message.chars().take(300).collect();
        trace::point("newton_relaxed_acceptance", || {
            self.with_op(crate::trace_fields! {
                "solve_id" => solve_id, "newton_iteration" => it, "residual_norm" => rn,
                "tolerance" => self.options.tolerance, "relaxed_tolerance" => relaxed, "newton_failure" => cause,
            })
        })?;
        let fields = self.with_op(crate::trace_fields! {
            "solve_id" => solve_id, "newton_iteration" => it, "purpose" => "converged_state_condition_gate",
        });
        let matrix = self.state_jacobian_traced(&u, design, context, &fields)?;
        let condition = self.captured_gate(matrix, &u, design, context, None, &fields, "relaxed")?;
        Ok(ImplicitSolveResult {
            state: u,
            residual_norm: rn,
            iterations: it,
            converged: true,
            condition_number: condition,
            convergence: None,
            relaxed: true,
        })
    }

    fn direct_after_fallback(fallback: bool, fields: &Fields) -> CaeResult<bool> {
        if !fallback {
            return Ok(false);
        }
        trace::point("exact_krylov_direct_for_remaining_solve", || {
            let mut f = crate::trace_fields! {"reason" => "iterative_exhaustion_direct_fallback"};
            f.extend(fields.clone());
            f
        })?;
        Ok(true)
    }



    pub fn adjoint_many(
        &self,
        design: &[f64],
        state: &[f64],
        gu: &DenseMatrix,
        direct: &DenseMatrix,
        context: &C,
    ) -> CaeResult<BatchedAdjointResult> {
        if state.is_empty()
            || design.is_empty()
            || state.iter().any(|v| !v.is_finite())
            || design.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("adjoint requires finite nonempty state and design"));
        }
        if gu.nrows != state.len() || gu.ncols == 0 || gu.data.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("batched response-state gradients have invalid shape or values"));
        }
        if (direct.nrows, direct.ncols) != (design.len(), gu.ncols)
            || direct.data.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "batched direct response-design gradients have invalid shape or values",
            ));
        }
        self.require_frame(state.len(), "ImplicitBlockSystem.adjoint_many")?;
        let rr = self.residual_checked(state, design, context)?;
        let rn = norm2(&rr);
        let report = self.criterion.assess(&rr, Some(rn))?;
        if !report.certified {
            let extra = if self.criterion.is_scalar() {
                String::new()
            } else {
                format!("; reason={}", report.reason)
            };
            return Err(CaeError::contract(format!(
                "adjoint requires a converged state (residual={}){extra}",
                fmt_e(rn, 3)
            )));
        }
        let n = state.len();
        let fac = Factorization::new_with_partition(
            self.callbacks.state_jacobian(state, design, context)?,
            n,
            self.options.condition_limit,
            None,
            self.options.local_elimination_partition.as_deref(),
        )?;
        let solved = fac.solve_block(&gu.data, gu.ncols, true)?;
        let condition = fac.condition();
        let kind = fac.kind().to_string();
        drop(fac);
        let lam = DenseMatrix { nrows: n, ncols: gu.ncols, data: solved.solution };
        let b = checked_matrix(
            self.callbacks.design_jacobian(state, design, context)?,
            (n, design.len()),
            "design Jacobian",
            true,
        )?;
        let contraction = checked_product_block(&b, &lam, "design Jacobian transpose contraction", true)?;
        let data: Vec<f64> = direct.data.iter().zip(&contraction.data).map(|(g, c)| g - c).collect();
        if data.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("adjoint design contraction returned nonfinite values"));
        }
        Ok(BatchedAdjointResult {
            adjoints: lam,
            gradients: DenseMatrix { nrows: design.len(), ncols: gu.ncols, data },
            residual_norm: rn,
            transpose_residual_norms: solved.error_norms,
            transpose_relative_residuals: solved.relative,
            condition_number: condition,
            factorization: kind,
        })
    }



    pub fn adjoint(
        &self,
        design: &[f64],
        state: &[f64],
        response_state_gradient: &[f64],
        response_design_gradient: Option<&[f64]>,
        context: &C,
    ) -> CaeResult<AdjointResult> {
        let gu = DenseMatrix {
            nrows: response_state_gradient.len(),
            ncols: 1,
            data: response_state_gradient.to_vec(),
        };
        let direct = match response_design_gradient {
            Some(g) => g.to_vec(),
            None => vec![0.0; design.len()],
        };
        let direct = DenseMatrix { nrows: direct.len(), ncols: 1, data: direct };
        let out = self.adjoint_many(design, state, &gu, &direct, context)?;
        Ok(AdjointResult {
            adjoint: out.adjoints.data,
            gradient: out.gradients.data,
            residual_norm: out.residual_norm,
            transpose_residual_norm: out.transpose_residual_norms[0],
        })
    }
}

impl<C: ?Sized> Drop for ImplicitBlockSystem<C> {
    fn drop(&mut self) {
        let pending = self.staging.get_mut().ok().and_then(|s| s.pending.take());
        if let Some(mut t) = pending {
            let _ = t.rollback();
        }
    }
}

enum DirectMatrix<'a, C: ?Sized> {
    Admitted(&'a AdmittedExactMatrix),
    Assemble { u: &'a [f64], design: &'a [f64], context: &'a C },
}

struct Correction {
    step: Vec<f64>,
    recycle: Option<ExactKrylovRecycleTransaction>,
    reuse: Option<ExactPreconditionerReuseTransaction>,
    fallback: bool,
}

impl Correction {
    fn direct(step: Vec<f64>, fallback: bool) -> Self {
        Self { step, recycle: None, reuse: None, fallback }
    }
}

fn pairs_value(pairs: &[(String, f64)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

fn pairs_bool(pairs: &[(String, bool)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

pub const EQUALITY_RANK_CERTIFICATE: f64 = SWITCH_CERTIFICATE;

#[derive(Clone, Debug, PartialEq)]
pub struct EqualityConstraint {
    pub source: Vec<usize>,
    pub target: Vec<usize>,
    pub label: String,
    pub scale: f64,
}

impl EqualityConstraint {


    pub fn new(source: Vec<usize>, target: Vec<usize>, label: &str, scale: f64) -> CaeResult<Self> {
        let q = implexity_core::py_repr::repr_str(label);
        for (name, idx) in [("source", &source), ("target", &target)] {
            if idx.is_empty() {
                return Err(CaeError::contract(format!(
                    "equality constraint {q} {name} must be a nonempty tuple of nonnegative state indices"
                )));
            }
        }
        if source.len() != target.len() {
            return Err(CaeError::contract(format!(
                "equality constraint {q} ties ports of different width ({} vs {})",
                source.len(),
                target.len()
            )));
        }
        if source.iter().zip(&target).any(|(a, b)| a == b) {
            return Err(CaeError::contract(format!("equality constraint {q} ties a state entry to itself")));
        }
        if !scale.is_finite() || scale == 0.0 {
            return Err(CaeError::contract(format!(
                "equality constraint {q} scale must be finite and nonzero"
            )));
        }
        Ok(Self { source, target, label: label.to_string(), scale })
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.source.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EqualityConstraintSet {
    constraints: Vec<EqualityConstraint>,
    state_size: usize,
    certificate: f64,
    rows: usize,
    row_index: Vec<usize>,
    col_index: Vec<usize>,
    values: Vec<f64>,
}

impl EqualityConstraintSet {


    pub fn new(constraints: Vec<EqualityConstraint>, state_size: usize, certificate: f64) -> CaeResult<Self> {
        if constraints.is_empty() {
            return Err(CaeError::contract("equality constraint set requires at least one constraint"));
        }
        if state_size < 1 {
            return Err(CaeError::contract("equality constraint set requires a positive state size"));
        }
        for c in &constraints {
            let top = c.source.iter().chain(&c.target).copied().max().unwrap_or(0);
            if top >= state_size {
                return Err(CaeError::contract(format!(
                    "equality constraint {} references state index {top} outside a state of size {state_size}",
                    implexity_core::py_repr::repr_str(&c.label)
                )));
            }
        }
        if !certificate.is_finite() || certificate < 0.0 {
            return Err(CaeError::contract("equality rank certificate must be finite and nonnegative"));
        }
        let mut ri = Vec::new();
        let mut ci = Vec::new();
        let mut vals = Vec::new();
        let mut r = 0;
        for c in &constraints {
            for (&a, &b) in c.source.iter().zip(&c.target) {
                ri.extend([r, r]);
                ci.extend([a, b]);
                vals.extend([c.scale, -c.scale]);
                r += 1;
            }
        }
        Ok(Self { constraints, state_size, certificate, rows: r, row_index: ri, col_index: ci, values: vals })
    }

    #[must_use]
    pub fn state_size(&self) -> usize {
        self.state_size
    }
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }
    #[must_use]
    pub fn constraints(&self) -> &[EqualityConstraint] {
        &self.constraints
    }

    #[must_use]
    pub fn matrix_dense(&self) -> DenseMatrix {
        let mut c = DenseMatrix::zeros(self.rows, self.state_size);
        for ((&r, &col), &v) in self.row_index.iter().zip(&self.col_index).zip(&self.values) {
            c.data[r * self.state_size + col] += v;
        }
        c
    }



    pub fn matrix_sparse(&self) -> CaeResult<CsrMatrix> {
        CsrMatrix::from_triplets(self.rows, self.state_size, &self.row_index, &self.col_index, &self.values)
            .map_err(|e| CaeError::contract(e.to_string()))
    }



    pub fn residual(&self, u: &[f64]) -> CaeResult<Vec<f64>> {
        if u.len() != self.state_size {
            return Err(CaeError::contract(format!(
                "equality constraint state must have shape ({},), got ({},)",
                self.state_size,
                u.len()
            )));
        }
        let mut out = vec![0.0; self.rows];
        for ((&r, &c), &v) in self.row_index.iter().zip(&self.col_index).zip(&self.values) {
            out[r] += v * u[c];
        }
        Ok(out)
    }



    pub fn apply_transpose(&self, lam: &[f64]) -> CaeResult<Vec<f64>> {
        if lam.len() != self.rows {
            return Err(CaeError::contract(format!(
                "equality multipliers must have shape ({},), got ({},)",
                self.rows,
                lam.len()
            )));
        }
        let mut out = vec![0.0; self.state_size];
        for ((&r, &c), &v) in self.row_index.iter().zip(&self.col_index).zip(&self.values) {
            out[c] += v * lam[r];
        }
        Ok(out)
    }

    #[must_use]
    pub fn switch_distance(&self) -> f64 {
        if self.rows > self.state_size {
            return 0.0;
        }
        implexity_linalg::dense::singular_values(&self.matrix_dense())
            .ok()
            .and_then(|s| s.last().copied())
            .unwrap_or(0.0)
    }

    #[must_use]
    pub fn diagnostics(&self) -> Value {
        let d = self.switch_distance();
        json!({
            "equality_rows": self.rows,
            "distance_to_rank_deficiency": d,
            "switch_certificate": self.certificate,
            "sensitivity_admissible": d.is_finite() && d > self.certificate,
            "derivative_scope": "implicit-function-theorem through the KKT system; no derivative when the constraint Jacobian is rank deficient",
        })
    }



    pub fn certify_sensitivity(&self) -> CaeResult<f64> {
        let d = self.switch_distance();
        if !d.is_finite() {
            return Err(CaeError::contract(
                "equality constraint Jacobian rank distance is not finite; sensitivity admission refused",
            ));
        }
        if d <= self.certificate {
            let labels: Vec<&str> =
                self.constraints.iter().map(|c| c.label.as_str()).filter(|l| !l.is_empty()).collect();
            let suffix = if labels.is_empty() { String::new() } else { format!(" [{}]", labels.join(", ")) };
            return Err(CaeError::contract(format!(
                "equality constraint Jacobian is rank deficient; sensitivity admission refused: sigma_min(C) = {} <= {} over {} constraint row(s){suffix}",
                fmt_e(d, 3),
                fmt_e0(self.certificate),
                self.rows
            )));
        }
        Ok(d)
    }



    pub fn split<'z>(&self, z: &'z [f64]) -> CaeResult<(&'z [f64], &'z [f64])> {
        if z.len() != self.state_size + self.rows {
            return Err(CaeError::contract(format!(
                "augmented state must have shape ({},), got ({},)",
                self.state_size + self.rows,
                z.len()
            )));
        }
        Ok(z.split_at(self.state_size))
    }



    pub fn augmented_initial_state(
        &self,
        initial: &[f64],
        multipliers: Option<&[f64]>,
    ) -> CaeResult<Vec<f64>> {
        if initial.len() != self.state_size {
            return Err(CaeError::contract(format!(
                "initial state must have shape ({},), got ({},)",
                self.state_size,
                initial.len()
            )));
        }
        let lam = multipliers.map_or_else(|| vec![0.0; self.rows], <[f64]>::to_vec);
        if lam.len() != self.rows {
            return Err(CaeError::contract(format!("initial multipliers must have shape ({},)", self.rows)));
        }
        Ok(initial.iter().copied().chain(lam).collect())
    }
}

pub struct AugmentedCallbacks<C: ?Sized> {
    physics: Arc<dyn BlockCallbacks<C>>,
    constraints: EqualityConstraintSet,
}

impl<C: ?Sized> AugmentedCallbacks<C> {
    pub fn new(physics: Arc<dyn BlockCallbacks<C>>, constraints: EqualityConstraintSet) -> Self {
        Self { physics, constraints }
    }
}

struct PaddedAction {
    inner: Arc<dyn MatrixAction>,
    extra_rows: usize,
}

impl MatrixAction for PaddedAction {
    fn shape(&self) -> (usize, usize) {
        let (r, c) = self.inner.shape();
        (r + self.extra_rows, c)
    }
    fn matvec(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        let mut y = self.inner.matvec(x)?;
        y.extend(std::iter::repeat_n(0.0, self.extra_rows));
        Ok(y)
    }
    fn rmatvec(&self, y: &[f64]) -> CaeResult<Vec<f64>> {
        let n = self.inner.shape().0;
        if y.len() < n {
            return Err(CaeError::contract("padded transpose operand has the wrong length"));
        }
        self.inner.rmatvec(&y[..n])
    }
}

impl<C: ?Sized + Sync> BlockCallbacks<C> for AugmentedCallbacks<C> {
    fn residual(&self, z: &[f64], design: &[f64], context: &C) -> CaeResult<Vec<f64>> {
        let (u, lam) = self.constraints.split(z)?;
        let n = self.constraints.state_size;
        let r = self.physics.residual(u, design, context)?;
        if r.len() != n {
            return Err(CaeError::contract(format!(
                "implicit residual must have exact shape ({n},), got ({},)",
                r.len()
            )));
        }
        let ct = self.constraints.apply_transpose(lam)?;
        let mut out: Vec<f64> = r.iter().zip(&ct).map(|(a, b)| a + b).collect();
        out.extend(self.constraints.residual(u)?);
        Ok(out)
    }

    fn state_jacobian(&self, z: &[f64], design: &[f64], context: &C) -> CaeResult<Jacobian> {
        let (u, _) = self.constraints.split(z)?;
        let n = self.constraints.state_size;
        let m = self.constraints.rows;
        let j = checked_matrix(
            self.physics.state_jacobian(u, design, context)?,
            (n, n),
            "state Jacobian",
            false,
        )?;
        if j.is_sparse() {
            let jc = j.to_csr()?;
            let c = self.constraints.matrix_sparse()?;
            let mut rows = Vec::new();
            let mut cols = Vec::new();
            let mut vals = Vec::new();
            for i in 0..n {
                let (ci, vi) = jc.row(i);
                for (&cc, &vv) in ci.iter().zip(vi) {
                    rows.push(i);
                    cols.push(cc);
                    vals.push(vv);
                }
            }
            for i in 0..m {
                let (ci, vi) = c.row(i);
                for (&cc, &vv) in ci.iter().zip(vi) {
                    rows.push(n + i);
                    cols.push(cc);
                    vals.push(vv);
                    rows.push(cc);
                    cols.push(n + i);
                    vals.push(vv);
                }
            }
            let kkt = CscMatrix::from_triplets(n + m, n + m, &rows, &cols, &vals)
                .map_err(|e| CaeError::contract(e.to_string()))?;
            return Ok(Jacobian::Csc(kkt));
        }
        let Jacobian::Dense(jd) = j else {
            return Err(CaeError::contract(
                "state Jacobian: state solve requires an assembled dense/sparse Jacobian",
            ));
        };
        let c = self.constraints.matrix_dense();
        let size = n + m;
        let mut out = DenseMatrix::zeros(size, size);
        for i in 0..n {
            out.data[i * size..i * size + n].copy_from_slice(&jd.data[i * n..(i + 1) * n]);
        }
        for r in 0..m {
            for col in 0..n {
                let v = c.data[r * n + col];
                out.data[(n + r) * size + col] = v;
                out.data[col * size + n + r] = v;
            }
        }
        Ok(Jacobian::Dense(out))
    }

    fn design_jacobian(&self, z: &[f64], design: &[f64], context: &C) -> CaeResult<Jacobian> {
        let (u, _) = self.constraints.split(z)?;
        let n = self.constraints.state_size;
        let m = self.constraints.rows;
        let d = design.len();
        let b = checked_matrix(
            self.physics.design_jacobian(u, design, context)?,
            (n, d),
            "design Jacobian",
            true,
        )?;
        Ok(match b {
            Jacobian::Operator(op) => Jacobian::Operator(Arc::new(PaddedAction { inner: op, extra_rows: m })),
            Jacobian::Dense(bd) => {
                let mut data = bd.data;
                data.extend(std::iter::repeat_n(0.0, m * d));
                Jacobian::Dense(DenseMatrix { nrows: n + m, ncols: d, data })
            }
            sparse => {
                let bc = sparse.to_csr()?;
                let (indptr, indices, data) = bc.into_parts();
                let mut indptr = indptr;
                let last = *indptr.last().unwrap_or(&0);
                indptr.extend(std::iter::repeat_n(last, m));
                Jacobian::Csr(
                    CsrMatrix::try_new(n + m, d, indptr, indices, data)
                        .map_err(|e| CaeError::contract(e.to_string()))?,
                )
            }
        })
    }
}

pub struct EqualityCoupledBlockSystem<C: ?Sized> {
    constraints: EqualityConstraintSet,
    system: ImplicitBlockSystem<C>,
}

impl<C: ?Sized + Sync + 'static> EqualityCoupledBlockSystem<C> {


    pub fn new(
        physics: Arc<dyn BlockCallbacks<C>>,
        constraints: EqualityConstraintSet,
        options: BlockOptions<C>,
    ) -> CaeResult<Self> {
        let augmented: Arc<dyn BlockCallbacks<C>> =
            Arc::new(AugmentedCallbacks::new(physics, constraints.clone()));
        Ok(Self { constraints, system: ImplicitBlockSystem::new(augmented, options)? })
    }

    #[must_use]
    pub fn augmented_size(&self) -> usize {
        self.constraints.state_size + self.constraints.rows
    }

    #[must_use]
    pub fn constraints(&self) -> &EqualityConstraintSet {
        &self.constraints
    }

    #[must_use]
    pub fn system(&self) -> &ImplicitBlockSystem<C> {
        &self.system
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial: &[f64],
        context: &C,
        multipliers: Option<&[f64]>,
    ) -> CaeResult<ImplicitSolveResult> {
        let z = if initial.len() == self.constraints.state_size {
            self.constraints.augmented_initial_state(initial, multipliers)?
        } else if multipliers.is_some() {
            return Err(CaeError::contract("multipliers may only accompany a physics-sized initial state"));
        } else {
            initial.to_vec()
        };
        self.system.solve(design, &z, context)
    }



    pub fn adjoint_many(
        &self,
        design: &[f64],
        state: &[f64],
        gu: &DenseMatrix,
        direct: &DenseMatrix,
        context: &C,
    ) -> CaeResult<BatchedAdjointResult> {
        self.constraints.certify_sensitivity()?;
        self.system.adjoint_many(design, state, gu, direct, context)
    }
}

#[must_use]
pub fn shape_text(rows: usize, cols: usize) -> String {
    py_shape(rows, cols)
}

