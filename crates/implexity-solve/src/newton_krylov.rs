// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::error::LinalgError;
use implexity_linalg::krylov::{
    GcrotOptions, GmresCallbackType, GmresOptions, RecyclePair, Truncate, gcrotmk, gmres,
};
use implexity_linalg::onenormest::onenormest;
use implexity_linalg::operator::FnOperator;
use serde_json::{Value, json};

use crate::certificate::residual_certificate;
use crate::exact_matrix::{AdmittedExactMatrix, ExactMatrixIdentity, MatrixInput, admit_exact_matrix};
use crate::matrix::{allclose, linspace};
use crate::operation_context::{OperationExecutionContext, is_sha256_hex};
use crate::preconditioner_lease::{
    BoundPreparedPreconditioner, ExactPreconditionerLeaseBudget, ExactPreconditionerReuseTransaction,
    LeaseFactory, Preconditioner, prepare_provider_preconditioner,
};
use crate::trace::{self, Fields, TraceFailure};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KrylovError {
    #[error("{0}")]
    Exhausted(String),
    #[error("{0}")]
    ConditionInconclusive(String),
    #[error("{0}")]
    OperatorMismatch(String),
    #[error("{0}")]
    Contract(#[from] CaeError),
}

impl KrylovError {
    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Exhausted(_) => "ExactKrylovExhausted",
            Self::ConditionInconclusive(_) => "ExactConditionInconclusive",
            Self::OperatorMismatch(_) => "ExactOperatorMismatch",
            Self::Contract(e) => e.python_class(),
        }
    }

    #[must_use]
    pub fn is_fallback(&self) -> bool {
        matches!(self, Self::Exhausted(_) | Self::ConditionInconclusive(_))
    }

    #[must_use]
    pub fn reason(&self) -> String {
        format!("{}: {self}", self.python_class())
    }
}

impl TraceFailure for KrylovError {
    fn trace_fields(&self) -> Fields {
        match self {
            Self::Contract(e) => trace::failure_fields(e),
            other => trace::failure_fields_of_type(other.python_class()),
        }
    }
    fn from_trace(error: CaeError) -> Self {
        Self::Contract(error)
    }
}

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KrylovMethod {
    Gcrotmk,
    Gmres,
}

impl KrylovMethod {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gcrotmk => "gcrotmk",
            Self::Gmres => "gmres",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExactKrylovPolicy {
    pub enabled: bool,
    pub method: KrylovMethod,
    pub rtol: f64,
    pub atol: f64,
    pub residual_limit: f64,
    pub maxiter: usize,
    pub restart: usize,
    pub inner_dimension: usize,
    pub recycle_dimension: usize,
    pub condition_safety_fraction: f64,
    pub maximum_workspace_bytes: usize,
}

impl Default for ExactKrylovPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            method: KrylovMethod::Gcrotmk,
            rtol: 1e-9,
            atol: 0.0,
            residual_limit: 1e-8,
            maxiter: 200,
            restart: 40,
            inner_dimension: 40,
            recycle_dimension: 20,
            condition_safety_fraction: 0.5,
            maximum_workspace_bytes: 512 * 1024 * 1024,
        }
    }
}

impl ExactKrylovPolicy {
    #[must_use]
    pub fn enabled() -> Self {
        Self { enabled: true, ..Self::default() }
    }



    pub fn validate(self) -> CaeResult<Self> {
        let finite = |v: f64, label: &str, positive: bool| -> CaeResult<f64> {
            if !v.is_finite() || (positive && v <= 0.0) || (!positive && v < 0.0) {
                let q = if positive { "positive " } else { "nonnegative " };
                return Err(contract(format!("{label} must be finite {q}numeric")));
            }
            Ok(v)
        };
        let rtol = finite(self.rtol, "exact Krylov relative tolerance", true)?;
        let atol = finite(self.atol, "exact Krylov absolute tolerance", false)?;
        let residual = finite(self.residual_limit, "exact Krylov residual limit", true)?;
        for (v, label) in [
            (self.maxiter, "exact Krylov maximum iterations"),
            (self.restart, "exact Krylov restart"),
            (self.inner_dimension, "exact Krylov inner dimension"),
            (self.recycle_dimension, "exact Krylov recycle dimension"),
            (self.maximum_workspace_bytes, "exact Krylov maximum workspace bytes"),
        ] {
            if v < 1 {
                return Err(contract(format!("{label} must be a positive integer")));
            }
        }
        let safety = finite(self.condition_safety_fraction, "exact Krylov condition safety fraction", true)?;
        if rtol > residual {
            return Err(contract("exact Krylov solver tolerance cannot exceed residual certification limit"));
        }
        if atol > residual {
            return Err(contract(
                "exact Krylov absolute tolerance cannot exceed residual certification limit",
            ));
        }
        if residual > 1e-8 {
            return Err(contract("exact Krylov residual limit cannot weaken the authoritative 1e-8 gate"));
        }
        if self.recycle_dimension >= self.inner_dimension {
            return Err(contract("exact Krylov recycle dimension must be smaller than inner dimension"));
        }
        if safety > 0.5 {
            return Err(contract("exact Krylov condition safety fraction cannot exceed 0.5"));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspacePlan {
    pub restart: usize,
    pub inner_dimension: usize,
    pub recycle_dimension: usize,
    pub estimated_bytes: usize,
}



pub fn workspace_plan(policy: &ExactKrylovPolicy, size: usize) -> CaeResult<WorkspacePlan> {
    if !policy.enabled {
        return Err(contract("exact Krylov workspace planning requires an enabled policy"));
    }
    if size < 1 {
        return Err(contract("exact Krylov workspace size must be a positive integer"));
    }
    let restart = policy.restart.min(size);
    let inner = policy.inner_dimension.min(size);
    let recycle = policy.recycle_dimension.min(size);
    let (slots, small) = match policy.method {
        KrylovMethod::Gcrotmk => (2 * inner + 14 * recycle + 24, 64 * (inner + recycle + 4).pow(2)),
        KrylovMethod::Gmres => (3 * restart + 8 * recycle + 24, 64 * (restart + 4).pow(2)),
    };
    let estimated = 8 * size * slots + small;
    if estimated > policy.maximum_workspace_bytes {
        return Err(contract(format!(
            "exact Krylov workspace estimate exceeds its operation limit ({estimated} > {} bytes)",
            policy.maximum_workspace_bytes
        )));
    }
    Ok(WorkspacePlan {
        restart,
        inner_dimension: inner,
        recycle_dimension: recycle,
        estimated_bytes: estimated,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExactOperatorIdentity {
    sha256: String,
    size: usize,
}

impl ExactOperatorIdentity {


    pub fn new(sha256: &str, size: usize) -> CaeResult<Self> {
        if !is_sha256_hex(sha256) {
            return Err(contract("exact operator identity must be a lowercase SHA-256 digest"));
        }
        if size < 1 {
            return Err(contract("exact operator identity size must be a positive integer"));
        }
        Ok(Self { sha256: sha256.to_string(), size })
    }
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OperatorIdentity {
    Matrix(ExactMatrixIdentity),
    Opaque(ExactOperatorIdentity),
}

impl OperatorIdentity {
    #[must_use]
    pub fn size(&self) -> usize {
        match self {
            Self::Matrix(m) => m.size,
            Self::Opaque(o) => o.size,
        }
    }
    #[must_use]
    pub fn sha256(&self) -> &str {
        match self {
            Self::Matrix(m) => &m.sha256,
            Self::Opaque(o) => &o.sha256,
        }
    }
}

pub trait ExactLinearization: Send + Sync {
    fn identity(&self) -> ExactMatrixIdentity;
    fn shape(&self) -> (usize, usize);


    fn matvec(&self, value: &[f64]) -> CaeResult<Vec<f64>>;


    fn rmatvec(&self, value: &[f64]) -> CaeResult<Vec<f64>>;


    fn prepare_preconditioner(&self) -> CaeResult<Option<Box<dyn Preconditioner>>>;


    fn release(&self) -> CaeResult<()>;
}

pub trait MatrixFreeLinearization: Send + Sync {
    fn identity(&self) -> ExactOperatorIdentity;
    fn shape(&self) -> (usize, usize);


    fn matvec(&self, value: &[f64]) -> CaeResult<Vec<f64>>;


    fn rmatvec(&self, value: &[f64]) -> CaeResult<Vec<f64>>;
    fn has_preconditioner(&self) -> bool;


    fn prepare_preconditioner(&self) -> CaeResult<Option<Box<dyn Preconditioner>>>;


    fn release(&self) -> CaeResult<()>;
}

type Action = dyn Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync;
type PreFactory = dyn Fn() -> CaeResult<Option<Box<dyn Preconditioner>>> + Send + Sync;
type Hook = dyn Fn() -> CaeResult<()> + Send + Sync;

struct CapabilityCallbacks {
    matvec: Box<Action>,
    rmatvec: Box<Action>,
    preconditioner: Option<Box<PreFactory>>,
    release: Option<Box<Hook>>,
}

pub struct ExactMatrixFreeLinearizationCapability {
    identity: ExactOperatorIdentity,
    shape: (usize, usize),
    callbacks: Mutex<Option<CapabilityCallbacks>>,
}

impl std::fmt::Debug for ExactMatrixFreeLinearizationCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactMatrixFreeLinearizationCapability")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl ExactMatrixFreeLinearizationCapability {


    pub fn new(
        identity: ExactOperatorIdentity,
        shape: (usize, usize),
        matvec: impl Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync + 'static,
        rmatvec: impl Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync + 'static,
        preconditioner_factory: Option<Box<PreFactory>>,
        release: Option<Box<Hook>>,
    ) -> CaeResult<Self> {
        if shape != (identity.size, identity.size) {
            return Err(contract("matrix-free exact capability shape disagrees with its identity"));
        }
        Ok(Self {
            identity,
            shape,
            callbacks: Mutex::new(Some(CapabilityCallbacks {
                matvec: Box::new(matvec),
                rmatvec: Box::new(rmatvec),
                preconditioner: preconditioner_factory,
                release,
            })),
        })
    }

    #[must_use]
    pub fn released(&self) -> bool {
        self.callbacks.lock().map_or(true, |c| c.is_none())
    }

    fn action(&self, transpose: bool, value: &[f64], label: &str) -> CaeResult<Vec<f64>> {
        let guard =
            self.callbacks.lock().map_err(|_| contract("matrix-free exact capability is released"))?;
        let Some(cb) = guard.as_ref() else {
            return Err(contract("matrix-free exact capability is released"));
        };
        let f: &Action = if transpose { cb.rmatvec.as_ref() } else { cb.matvec.as_ref() };
        checked_action(f, value, self.identity.size, label)
    }
}

impl MatrixFreeLinearization for ExactMatrixFreeLinearizationCapability {
    fn identity(&self) -> ExactOperatorIdentity {
        self.identity.clone()
    }
    fn shape(&self) -> (usize, usize) {
        self.shape
    }
    fn matvec(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.action(false, value, "matrix-free exact JVP")
    }
    fn rmatvec(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.action(true, value, "matrix-free exact VJP")
    }
    fn has_preconditioner(&self) -> bool {
        self.callbacks.lock().is_ok_and(|c| c.as_ref().is_some_and(|cb| cb.preconditioner.is_some()))
    }
    fn prepare_preconditioner(&self) -> CaeResult<Option<Box<dyn Preconditioner>>> {
        let guard =
            self.callbacks.lock().map_err(|_| contract("matrix-free exact capability is released"))?;
        let Some(cb) = guard.as_ref() else {
            return Err(contract("matrix-free exact capability is released"));
        };
        match &cb.preconditioner {
            Some(f) => f(),
            None => Ok(None),
        }
    }
    fn release(&self) -> CaeResult<()> {
        let taken =
            self.callbacks.lock().map_err(|_| contract("matrix-free exact capability is released"))?.take();
        if let Some(cb) = taken
            && let Some(hook) = cb.release
        {
            hook().map_err(|e| {
                contract(format!("matrix-free exact capability release failed: {}", e.message()))
            })?;
        }
        Ok(())
    }
}

pub struct ExactLinearizationCapability {
    identity: ExactMatrixIdentity,
    shape: (usize, usize),
    matvec: Box<Action>,
    rmatvec: Box<Action>,
    preconditioner: Option<Box<PreFactory>>,
    release: Option<Box<Hook>>,
}

impl std::fmt::Debug for ExactLinearizationCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactLinearizationCapability")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl ExactLinearizationCapability {
    #[must_use]
    pub fn new(
        identity: ExactMatrixIdentity,
        shape: (usize, usize),
        matvec: Box<Action>,
        rmatvec: Box<Action>,
        preconditioner: Option<Box<PreFactory>>,
        release: Option<Box<Hook>>,
    ) -> Self {
        Self { identity, shape, matvec, rmatvec, preconditioner, release }
    }
}

impl ExactLinearization for ExactLinearizationCapability {
    fn identity(&self) -> ExactMatrixIdentity {
        self.identity.clone()
    }
    fn shape(&self) -> (usize, usize) {
        self.shape
    }
    fn matvec(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        (self.matvec)(value)
    }
    fn rmatvec(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        (self.rmatvec)(value)
    }
    fn prepare_preconditioner(&self) -> CaeResult<Option<Box<dyn Preconditioner>>> {
        match &self.preconditioner {
            Some(f) => f(),
            None => Ok(None),
        }
    }
    fn release(&self) -> CaeResult<()> {
        match &self.release {
            Some(f) => f(),
            None => Ok(()),
        }
    }
}



pub fn assembled_linearization_capability(
    matrix: impl Into<MatrixInput>,
    preconditioner: Option<Box<PreFactory>>,
    release: Option<Box<Hook>>,
) -> CaeResult<ExactLinearizationCapability> {
    let admitted = admit_exact_matrix(matrix, None)?;
    let forward = admitted.shared();
    let reverse = admitted.shared();
    Ok(ExactLinearizationCapability::new(
        admitted.identity().clone(),
        (admitted.size(), admitted.size()),
        Box::new(move |v| forward.apply(v, false)),
        Box::new(move |v| reverse.apply(v, true)),
        preconditioner,
        release,
    ))
}

fn checked_vector(value: &[f64], size: usize, label: &str) -> CaeResult<()> {
    if value.len() != size || value.iter().any(|v| !v.is_finite()) {
        return Err(contract(format!("{label} must have finite shape ({size},)")));
    }
    Ok(())
}



pub fn checked_action(f: &Action, value: &[f64], size: usize, label: &str) -> CaeResult<Vec<f64>> {
    checked_vector(value, size, &format!("{label} input"))?;
    let result = f(value).map_err(|e| contract(format!("{label} failed: {}", e.message())))?;
    checked_vector(&result, size, &format!("{label} result"))?;
    Ok(result)
}

fn checked_dyn(
    value: &[f64],
    size: usize,
    label: &str,
    f: impl FnOnce(&[f64]) -> CaeResult<Vec<f64>>,
) -> CaeResult<Vec<f64>> {
    checked_vector(value, size, &format!("{label} input"))?;
    let result = f(value).map_err(|e| contract(format!("{label} failed: {}", e.message())))?;
    checked_vector(&result, size, &format!("{label} result"))?;
    Ok(result)
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}



pub fn verify_matrix_free_equivalence(
    capability: &dyn MatrixFreeLinearization,
    matrix: impl Into<MatrixInput>,
) -> Result<AdmittedExactMatrix, KrylovError> {
    let identity = capability.identity();
    let admitted = admit_exact_matrix(matrix, Some(identity.size))?;
    if capability.shape() != (admitted.size(), admitted.size()) {
        return Err(contract("matrix-free exact capability shape disagrees with its anchor").into());
    }
    let n = identity.size;
    let p = linspace(0.25, 1.25, n);
    let q = linspace(-0.75, 0.5, n);
    let forward = checked_dyn(&p, n, "matrix-free exact anchor JVP", |v| capability.matvec(v))?;
    let reverse = checked_dyn(&q, n, "matrix-free exact anchor VJP", |v| capability.rmatvec(v))?;
    let ef = admitted.matrix().apply(&p, false)?;
    let er = admitted.matrix().apply(&q, true)?;
    if !allclose(&forward, &ef, 1e-10, 1e-12) || !allclose(&reverse, &er, 1e-10, 1e-12) {
        return Err(KrylovError::OperatorMismatch(
            "matrix-free exact actions disagree with the assembled refresh anchor".into(),
        ));
    }
    let (a, b) = (dot(&q, &forward), dot(&p, &reverse));
    if (a - b).abs() > 1e-10 * 1.0_f64.max(a.abs()).max(b.abs()) {
        return Err(KrylovError::OperatorMismatch(
            "matrix-free exact actions fail transpose duality at the refresh anchor".into(),
        ));
    }
    Ok(admitted)
}

fn copy_cu(items: &[RecyclePair], size: usize) -> CaeResult<Vec<RecyclePair>> {
    items
        .iter()
        .map(|p| {
            if let Some(c) = &p.c {
                checked_vector(c, size, "exact Krylov recycle image")?;
            }
            checked_vector(&p.u, size, "exact Krylov recycle vector")?;
            Ok(p.clone())
        })
        .collect()
}

fn numerical_breakdown(e: &LinalgError) -> bool {
    matches!(e, LinalgError::NoConvergence(_) | LinalgError::NonFinite(_) | LinalgError::Singular(_))
}

fn cap_cu(items: &mut Vec<RecyclePair>, maximum: usize) {
    if items.len() > maximum {
        let excess = items.len() - maximum;
        items.drain(..excess);
    }
}

struct RecycleInner {
    primal: Vec<RecyclePair>,
    transpose: Vec<RecyclePair>,
    closed: bool,
    committed: bool,
}

type Committed = (usize, OperatorIdentity, Vec<RecyclePair>, Vec<RecyclePair>);

#[derive(Default)]
struct RecycleStoreState {
    committed: BTreeMap<String, Committed>,
    active: BTreeMap<String, Arc<Mutex<RecycleInner>>>,
    released: bool,
}

#[derive(Clone, Default)]
pub struct ExactKrylovRecycleStore {
    state: Arc<Mutex<RecycleStoreState>>,
}

impl std::fmt::Debug for ExactKrylovRecycleStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactKrylovRecycleStore").finish_non_exhaustive()
    }
}

impl ExactKrylovRecycleStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, RecycleStoreState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }



    pub fn begin(
        &self,
        namespace: &str,
        size: usize,
        identity: &OperatorIdentity,
    ) -> CaeResult<ExactKrylovRecycleTransaction> {
        let mut state = self.lock();
        if state.released {
            return Err(contract("exact Krylov recycle store is released"));
        }
        if namespace.is_empty() {
            return Err(contract("exact Krylov recycle namespace must be nonempty text"));
        }
        if size < 1 {
            return Err(contract("exact Krylov recycle size must be a positive integer"));
        }
        if identity.size() != size {
            return Err(contract("exact Krylov recycle matrix identity is incompatible"));
        }
        if state.active.contains_key(namespace) {
            return Err(contract("exact Krylov recycle namespace already has an active transaction"));
        }
        let (primal, transpose) = match state.committed.get(namespace) {
            None => (Vec::new(), Vec::new()),
            Some((s, _, _, _)) if *s != size => {
                return Err(contract("exact Krylov recycle namespace size drifted"));
            }
            Some((_, id, p, t)) if id == identity => (copy_cu(p, size)?, copy_cu(t, size)?),
            Some((_, _, p, t)) => (
                p.iter().map(|x| RecyclePair { c: None, u: x.u.clone() }).collect(),
                t.iter().map(|x| RecyclePair { c: None, u: x.u.clone() }).collect(),
            ),
        };
        let inner = Arc::new(Mutex::new(RecycleInner { primal, transpose, closed: false, committed: false }));
        state.active.insert(namespace.to_string(), Arc::clone(&inner));
        Ok(ExactKrylovRecycleTransaction {
            store: self.clone(),
            namespace: namespace.to_string(),
            size,
            identity: identity.clone(),
            inner,
        })
    }

    fn finish(&self, tx: &ExactKrylovRecycleTransaction, commit: bool) -> CaeResult<()> {
        let mut state = self.lock();
        let owns = state.active.get(&tx.namespace).is_some_and(|a| Arc::ptr_eq(a, &tx.inner));
        if !owns {
            return Err(contract("exact Krylov recycle transaction ownership drifted"));
        }
        if commit {
            let inner = tx.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = (
                tx.size,
                tx.identity.clone(),
                copy_cu(&inner.primal, tx.size)?,
                copy_cu(&inner.transpose, tx.size)?,
            );
            drop(inner);
            state.committed.insert(tx.namespace.clone(), entry);
        }
        state.active.remove(&tx.namespace);
        Ok(())
    }

    pub fn clear(&self, namespace: Option<&str>) {
        let mut state = self.lock();
        let close = |inner: &Arc<Mutex<RecycleInner>>| {
            let mut i = inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            i.closed = true;
            i.primal.clear();
            i.transpose.clear();
        };
        match namespace {
            None => {
                for inner in state.active.values() {
                    close(inner);
                }
                state.active.clear();
                state.committed.clear();
            }
            Some(ns) => {
                if let Some(inner) = state.active.remove(ns) {
                    close(&inner);
                }
                state.committed.remove(ns);
            }
        }
    }

    pub fn release(&self) {
        let released = self.lock().released;
        if !released {
            self.clear(None);
            self.lock().released = true;
        }
    }

    #[must_use]
    pub fn committed_counts(&self, namespace: &str) -> Option<(usize, usize)> {
        self.lock().committed.get(namespace).map(|(_, _, p, t)| (p.len(), t.len()))
    }
}

pub struct ExactKrylovRecycleTransaction {
    store: ExactKrylovRecycleStore,
    namespace: String,
    size: usize,
    identity: OperatorIdentity,
    inner: Arc<Mutex<RecycleInner>>,
}

impl std::fmt::Debug for ExactKrylovRecycleTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactKrylovRecycleTransaction")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

pub type RecycleSnapshot = (Vec<RecyclePair>, Vec<RecyclePair>);

impl ExactKrylovRecycleTransaction {
    fn lock(&self) -> MutexGuard<'_, RecycleInner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn open(&self) -> CaeResult<MutexGuard<'_, RecycleInner>> {
        let inner = self.lock();
        if inner.closed {
            return Err(contract("exact Krylov recycle transaction is closed"));
        }
        Ok(inner)
    }

    #[must_use]
    pub fn closed(&self) -> bool {
        self.lock().closed
    }
    #[must_use]
    pub fn committed(&self) -> bool {
        self.lock().committed
    }
    #[must_use]
    pub fn identity(&self) -> &OperatorIdentity {
        &self.identity
    }
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let i = self.lock();
        (i.primal.len(), i.transpose.len())
    }



    pub fn snapshot(&self) -> CaeResult<RecycleSnapshot> {
        let i = self.open()?;
        Ok((copy_cu(&i.primal, self.size)?, copy_cu(&i.transpose, self.size)?))
    }



    pub fn restore(&self, snapshot: &RecycleSnapshot) -> CaeResult<()> {
        let mut i = self.open()?;
        i.primal = copy_cu(&snapshot.0, self.size)?;
        i.transpose = copy_cu(&snapshot.1, self.size)?;
        Ok(())
    }

    fn with_lists<T>(&self, transpose: bool, f: impl FnOnce(&mut Vec<RecyclePair>) -> T) -> CaeResult<T> {
        let mut i = self.open()?;
        Ok(if transpose { f(&mut i.transpose) } else { f(&mut i.primal) })
    }

    fn recopy_and_cap(&self, maximum: usize) -> CaeResult<()> {
        let mut i = self.open()?;
        i.primal = copy_cu(&i.primal, self.size)?;
        i.transpose = copy_cu(&i.transpose, self.size)?;
        cap_cu(&mut i.primal, maximum);
        cap_cu(&mut i.transpose, maximum);
        Ok(())
    }



    pub fn commit(&self) -> CaeResult<()> {
        drop(self.open()?);
        self.store.finish(self, true)?;
        let mut i = self.lock();
        i.primal.clear();
        i.transpose.clear();
        i.committed = true;
        i.closed = true;
        Ok(())
    }



    pub fn rollback(&self) -> CaeResult<()> {
        drop(self.open()?);
        self.store.finish(self, false)?;
        let mut i = self.lock();
        i.primal.clear();
        i.transpose.clear();
        i.closed = true;
        Ok(())
    }



    pub fn release(&self) -> CaeResult<()> {
        if self.closed() { Ok(()) } else { self.rollback() }
    }



    pub fn abort_store(&self) -> CaeResult<()> {
        if self.closed() {
            let mut i = self.lock();
            i.primal.clear();
            i.transpose.clear();
        } else {
            self.rollback()?;
        }
        self.store.clear(None);
        Ok(())
    }
}

impl Drop for ExactKrylovRecycleTransaction {
    fn drop(&mut self) {
        if !self.closed() {
            let _ = self.rollback();
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactKrylovSolveDiagnostics {
    pub backend: &'static str,
    pub transpose: bool,
    pub iterations: usize,
    pub relative_residual: f64,
    pub preconditioner_used: bool,
    pub recycled_vector_count: usize,
}

impl ExactKrylovSolveDiagnostics {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"backend": self.backend, "transpose": self.transpose, "iterations": self.iterations,
               "relative_residual": self.relative_residual, "preconditioner_used": self.preconditioner_used,
               "recycled_vector_count": self.recycled_vector_count})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactKrylovSolveResult {
    pub solution: Vec<f64>,
    pub error_norm: f64,
    pub relative_residual: f64,
    pub diagnostics: ExactKrylovSolveDiagnostics,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactConditionEstimate {
    pub value: f64,
    pub condition_limit: f64,
    pub safety_fraction: f64,
    pub method: &'static str,
    pub admitted: bool,
}

pub enum SessionOperator {
    Assembled(AdmittedExactMatrix),
    MatrixFree(Box<dyn MatrixFreeLinearization>),
}

pub enum SessionPreconditioner {
    Bound(BoundPreparedPreconditioner),
    Opaque(Box<dyn Preconditioner>),
}

impl SessionPreconditioner {
    fn get(&self) -> &dyn Preconditioner {
        match self {
            Self::Bound(b) => b,
            Self::Opaque(p) => p.as_ref(),
        }
    }
}


pub struct ExactKrylovSession<'r> {
    operator: Option<SessionOperator>,
    identity: OperatorIdentity,
    size: usize,
    policy: ExactKrylovPolicy,
    plan: WorkspacePlan,
    capability: Option<Box<dyn ExactLinearization>>,
    preconditioner: Option<SessionPreconditioner>,
    recycle: Option<&'r ExactKrylovRecycleTransaction>,
    trace_fields: Fields,
    released: bool,
    entered: bool,
}

const ESTIMATOR_FIELDS: [&str; 8] = [
    "duration_s",
    "error_type",
    "condition_limit",
    "condition_estimate",
    "admitted",
    "forward_inverse_action_count",
    "transpose_inverse_action_count",
    "total_inverse_action_count",
];

impl<'r> ExactKrylovSession<'r> {


    pub fn new(
        operator: SessionOperator,
        policy: ExactKrylovPolicy,
        capability: Option<Box<dyn ExactLinearization>>,
        preconditioner: Option<SessionPreconditioner>,
        recycle: Option<&'r ExactKrylovRecycleTransaction>,
        trace_fields: Option<Fields>,
    ) -> CaeResult<Self> {
        if !policy.enabled {
            return Err(contract("exact Krylov session requires an enabled policy"));
        }
        let (identity, size) = match &operator {
            SessionOperator::Assembled(a) => (OperatorIdentity::Matrix(a.identity().clone()), a.size()),
            SessionOperator::MatrixFree(c) => {
                let id = c.identity();
                if c.shape() != (id.size, id.size) {
                    return Err(contract("matrix-free exact capability metadata is incomplete"));
                }
                if capability.is_some() {
                    return Err(contract("exact Krylov session cannot select two operator capabilities"));
                }
                let n = id.size;
                (OperatorIdentity::Opaque(id), n)
            }
        };
        let plan = workspace_plan(&policy, size)?;
        let trace_fields = trace_fields.unwrap_or_default();
        let mut collision: Vec<&str> = ESTIMATOR_FIELDS
            .iter()
            .copied()
            .chain(trace::PROTECTED_FIELDS)
            .chain(trace::LIFECYCLE_FIELDS)
            .filter(|k| trace_fields.contains_key(*k))
            .collect();
        if !collision.is_empty() {
            collision.sort_unstable();
            collision.dedup();
            let list: Vec<String> = collision.iter().map(|k| format!("'{k}'")).collect();
            return Err(contract(format!(
                "exact Krylov trace fields collide with estimator fields: [{}]",
                list.join(", ")
            )));
        }
        match (&operator, &preconditioner) {
            (SessionOperator::Assembled(_), Some(SessionPreconditioner::Opaque(_))) => {
                return Err(contract("exact Krylov prepared preconditioner must come from the sealed lease"));
            }
            (SessionOperator::Assembled(a), Some(SessionPreconditioner::Bound(b)))
                if (b.binding().matrix_identity != *a.identity()
                    || b.binding().shape != (a.size(), a.size())) =>
            {
                return Err(contract(
                    "sealed prepared preconditioner does not bind the authoritative matrix",
                ));
            }
            (SessionOperator::MatrixFree(_), Some(p)) if p.get().shape() != (size, size) => {
                return Err(contract("lagged exact preconditioner shape disagrees with the operator"));
            }
            _ => {}
        }
        if let Some(tx) = recycle {
            if tx.closed() || tx.size() != size || *tx.identity() != identity {
                return Err(contract("exact Krylov recycle transaction is incompatible"));
            }
            tx.recopy_and_cap(plan.recycle_dimension)?;
        }
        Ok(Self {
            operator: Some(operator),
            identity,
            size,
            policy,
            plan,
            capability,
            preconditioner,
            recycle,
            trace_fields,
            released: false,
            entered: false,
        })
    }

    #[must_use]
    pub fn plan(&self) -> WorkspacePlan {
        self.plan
    }

    #[must_use]
    pub fn identity(&self) -> &OperatorIdentity {
        &self.identity
    }

    fn apply_operator(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        match self.operator.as_ref() {
            Some(SessionOperator::Assembled(a)) => a.matrix().apply(x, transpose),
            Some(SessionOperator::MatrixFree(c)) => {
                let label =
                    if transpose { "matrix-free exact transpose action" } else { "matrix-free exact action" };
                checked_dyn(x, self.size, label, |v| if transpose { c.rmatvec(v) } else { c.matvec(v) })
            }
            None => Err(contract("exact Krylov session is not active")),
        }
    }

    fn validate(&mut self) -> CaeResult<()> {
        let n = self.size;
        let p = linspace(0.25, 1.25, n);
        let q = linspace(-0.75, 0.5, n);
        if let Some(SessionOperator::MatrixFree(c)) = self.operator.as_ref() {
            let forward = checked_dyn(&p, n, "matrix-free exact capability JVP", |v| c.matvec(v))?;
            let reverse = checked_dyn(&q, n, "matrix-free exact capability VJP", |v| c.rmatvec(v))?;
            let (a, b) = (dot(&q, &forward), dot(&p, &reverse));
            if (a - b).abs() > 1e-10 * 1.0_f64.max(a.abs()).max(b.abs()) {
                return Err(contract("matrix-free exact capability transpose is inconsistent"));
            }
            if self.preconditioner.is_none() {
                let declared = c.has_preconditioner();
                let prepared = c.prepare_preconditioner().map_err(|e| {
                    contract(format!(
                        "matrix-free exact bootstrap preconditioner preparation failed: {}",
                        e.message()
                    ))
                })?;
                if declared && prepared.is_none() {
                    return Err(contract(
                        "matrix-free exact capability declared an empty bootstrap preconditioner",
                    ));
                }
                self.preconditioner = prepared.map(SessionPreconditioner::Opaque);
            }
        }
        if let Some(cap) = self.capability.as_ref() {
            let Some(SessionOperator::Assembled(a)) = self.operator.as_ref() else {
                return Err(contract("exact linearization capability is incomplete"));
            };
            if cap.identity() != *a.identity() || cap.shape() != (n, n) {
                return Err(contract(
                    "exact linearization capability does not bind the authoritative matrix",
                ));
            }
            let forward = checked_dyn(&p, n, "capability matvec", |v| cap.matvec(v))?;
            let reverse = checked_dyn(&q, n, "capability rmatvec", |v| cap.rmatvec(v))?;
            let ef = a.matrix().apply(&p, false)?;
            let er = a.matrix().apply(&q, true)?;
            if !allclose(&forward, &ef, 1e-12, 1e-14) || !allclose(&reverse, &er, 1e-12, 1e-14) {
                return Err(contract(
                    "exact linearization capability actions disagree with authoritative matrix",
                ));
            }
            let (x, y) = (dot(&q, &forward), dot(&p, &reverse));
            if (x - y).abs() > 1e-11 * 1.0_f64.max(x.abs()).max(y.abs()) {
                return Err(contract("exact linearization capability transpose is inconsistent"));
            }
            let cap_pre = cap
                .prepare_preconditioner()
                .map_err(|e| contract(format!("exact preconditioner preparation failed: {}", e.message())))?;
            if let Some(extra) = cap_pre {
                if self.preconditioner.is_some() {
                    extra.release().map_err(|e| {
                        contract(format!(
                            "duplicate exact preconditioner paths and capability cleanup failed: {}",
                            e.message()
                        ))
                    })?;
                    return Err(contract("exact Krylov operation supplied two preconditioner paths"));
                }
                self.preconditioner = Some(SessionPreconditioner::Opaque(extra));
            }
        }
        if let Some(pre) = self.preconditioner.as_ref() {
            let pre = pre.get();
            let fp = checked_dyn(&p, n, "preconditioner apply", |v| pre.apply(v))?;
            let rq = checked_dyn(&q, n, "preconditioner transpose apply", |v| pre.apply_transpose(v))?;
            let (a, b) = (dot(&q, &fp), dot(&p, &rq));
            if (a - b).abs() > 1e-10 * 1.0_f64.max(a.abs()).max(b.abs()) {
                return Err(contract("exact preconditioner transpose is inconsistent"));
            }
        }
        Ok(())
    }



    pub fn enter(&mut self) -> CaeResult<()> {
        if self.entered || self.released {
            return Err(contract("exact Krylov session cannot be entered twice"));
        }
        self.entered = true;
        if let Err(e) = self.validate() {
            let _ = self.release();
            return Err(e);
        }
        Ok(())
    }



    pub fn release(&mut self) -> CaeResult<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let pre = self.preconditioner.take();
        let cap = self.capability.take();
        let op = self.operator.take();
        self.recycle = None;
        let mut failures = Vec::new();
        if let Some(p) = pre
            && let Err(e) = p.get().release()
        {
            failures.push(format!("preconditioner release failed: {}", e.message()));
        }
        if let Some(c) = cap
            && let Err(e) = c.release()
        {
            failures.push(format!("capability release failed: {}", e.message()));
        }
        if let Some(SessionOperator::MatrixFree(c)) = op
            && let Err(e) = c.release()
        {
            failures.push(format!("capability release failed: {}", e.message()));
        }
        if failures.is_empty() { Ok(()) } else { Err(contract(failures.join("; "))) }
    }



    #[allow(clippy::too_many_lines)]
    pub fn solve(&self, rhs: &[f64], transpose: bool) -> Result<ExactKrylovSolveResult, KrylovError> {
        if !self.entered || self.released {
            return Err(contract("exact Krylov session is not active").into());
        }
        checked_vector(rhs, self.size, "exact Krylov right-hand side")?;
        let n = self.size;
        let side_cell: RefCell<Option<CaeError>> = RefCell::new(None);
        let side = &side_cell;
        let op = FnOperator::new(n, |x: &[f64], y: &mut [f64]| match self.apply_operator(x, transpose) {
            Ok(v) => {
                y.copy_from_slice(&v);
                Ok(())
            }
            Err(e) => {
                let msg = e.message().to_string();
                side.borrow_mut().get_or_insert(e);
                Err(LinalgError::Operator(msg))
            }
        });
        let pre = self.preconditioner.as_ref().map(SessionPreconditioner::get);
        let pre_op = pre.map(|p| {
            FnOperator::new(n, move |x: &[f64], y: &mut [f64]| {
                let r = checked_dyn(x, n, "exact preconditioner action", |v| {
                    if transpose { p.apply_transpose(v) } else { p.apply(v) }
                });
                match r {
                    Ok(v) => {
                        y.copy_from_slice(&v);
                        Ok(())
                    }
                    Err(e) => {
                        let msg = e.message().to_string();
                        side.borrow_mut().get_or_insert(e);
                        Err(LinalgError::Operator(msg))
                    }
                }
            })
        });
        let mut recycled = 0;
        let outcome = match self.policy.method {
            KrylovMethod::Gcrotmk => {
                let opts = GcrotOptions {
                    rtol: self.policy.rtol,
                    atol: self.policy.atol,
                    maxiter: self.policy.maxiter,
                    m: self.plan.inner_dimension,
                    k: Some(self.plan.recycle_dimension),
                    discard_c: false,
                    truncate: Truncate::Oldest,
                };
                if let Some(tx) = self.recycle {
                    let first = tx.with_lists(transpose, |cu| {
                        recycled = cu.len();
                        let out = gcrotmk(&op, rhs, None, pre_op.as_ref(), &opts, cu);
                        cap_cu(cu, self.plan.recycle_dimension);
                        out
                    })?;
                    match first {

                        Err(e) if recycled > 0 && side.borrow().is_none() && numerical_breakdown(&e) => {
                            let cause: String = e.to_string().chars().take(500).collect();
                            trace::point("exact_krylov_recycle_discarded", || {
                                crate::trace_fields! {"transpose" => transpose, "recycled" => recycled,
                                "cause" => cause}
                            })?;
                            tx.with_lists(transpose, |cu| {
                                cu.clear();
                                recycled = 0;
                                let out = gcrotmk(&op, rhs, None, pre_op.as_ref(), &opts, cu);
                                cap_cu(cu, self.plan.recycle_dimension);
                                out
                            })?
                        }
                        other => other,
                    }
                } else {
                    let mut cu = Vec::new();
                    gcrotmk(&op, rhs, None, pre_op.as_ref(), &opts, &mut cu)
                }
            }
            KrylovMethod::Gmres => {
                let opts = GmresOptions {
                    rtol: self.policy.rtol,
                    atol: self.policy.atol,
                    restart: Some(self.plan.restart),
                    maxiter: Some(self.policy.maxiter),
                    callback_type: GmresCallbackType::Legacy,
                };
                gmres(&op, rhs, None, pre_op.as_ref(), &opts)
            }
        };
        let result = match outcome {
            Ok(r) => r,
            Err(e) => {
                if let Some(inner) = side.borrow_mut().take() {
                    return Err(inner.into());
                }


                if numerical_breakdown(&e) {
                    return Err(KrylovError::Exhausted(format!("exact Krylov backend breakdown: {e}")));
                }
                return Err(contract(format!("exact Krylov backend failed: {e}")).into());
            }
        };
        let status = result.info;
        let x = result.x;
        if x.len() == n && x.iter().any(|v| !v.is_finite()) {
            return Err(KrylovError::Exhausted("exact Krylov backend breakdown: non-finite solution".into()));
        }
        checked_vector(&x, n, "exact Krylov solution")?;
        let ax = self
            .apply_operator(&x, transpose)
            .map_err(|e| contract(format!("exact Krylov residual certification failed: {}", e.message())))?;
        let error: Vec<f64> = ax.iter().zip(rhs).map(|(a, b)| a - b).collect();
        if error.len() == n && error.iter().any(|v| !v.is_finite()) {
            return Err(KrylovError::Exhausted("exact Krylov backend breakdown: non-finite residual".into()));
        }
        checked_vector(&error, n, "exact Krylov residual")?;
        let (error_norm, relative) = residual_certificate(&error, rhs)
            .map_err(|s| contract(format!("exact Krylov residual certification failed: {s}")))?;
        if status > 0 {
            if status != self.policy.maxiter {

                if matches!(self.policy.method, KrylovMethod::Gcrotmk)
                    && relative > self.policy.residual_limit
                {
                    return Err(KrylovError::Exhausted(format!(
                        "exact Krylov backend breakdown: gcrotmk stopped at outer iteration {status}"
                    )));
                }
                return Err(contract("exact Krylov backend returned a non-exhaustion positive status").into());
            }
            if relative > self.policy.residual_limit {
                return Err(KrylovError::Exhausted(format!(
                    "exact Krylov {} exhausted its iteration bound",
                    self.policy.method.as_str()
                )));
            }
        }
        if relative > self.policy.residual_limit {
            return Err(KrylovError::Exhausted(
                "exact Krylov solution did not satisfy authoritative residual certification".into(),
            ));
        }
        Ok(ExactKrylovSolveResult {
            solution: x,
            error_norm,
            relative_residual: relative,
            diagnostics: ExactKrylovSolveDiagnostics {
                backend: self.policy.method.as_str(),
                transpose,
                iterations: result.iterations,
                relative_residual: relative,
                preconditioner_used: pre.is_some(),
                recycled_vector_count: recycled,
            },
        })
    }



    pub fn estimate_condition(&self, condition_limit: f64) -> Result<ExactConditionEstimate, KrylovError> {
        if !condition_limit.is_finite() || condition_limit <= 0.0 {
            return Err(contract("exact Krylov condition limit must be finite positive numeric").into());
        }
        if condition_limit <= 1.0 {
            return Err(contract("exact Krylov condition limit must exceed one").into());
        }
        let n = self.size;
        let counts = std::cell::Cell::new((0usize, 0usize));
        let side: RefCell<Option<KrylovError>> = RefCell::new(None);
        let op_side_cell: RefCell<Option<CaeError>> = RefCell::new(None);
        let op_side = &op_side_cell;
        let run = || -> Result<ExactConditionEstimate, KrylovError> {
            let make = |transpose: bool| {
                FnOperator::new(n, move |x: &[f64], y: &mut [f64]| match self.apply_operator(x, transpose) {
                    Ok(v) => {
                        y.copy_from_slice(&v);
                        Ok(())
                    }
                    Err(e) => {
                        let msg = e.message().to_string();
                        op_side.borrow_mut().get_or_insert(e);
                        Err(LinalgError::Operator(msg))
                    }
                })
            };
            let inverse = |transpose: bool| {
                let side = &side;
                let counts = &counts;
                FnOperator::new(n, move |x: &[f64], y: &mut [f64]| match self.solve(x, transpose) {
                    Ok(s) => {
                        let (f, t) = counts.get();
                        counts.set(if transpose { (f, t + 1) } else { (f + 1, t) });
                        y.copy_from_slice(&s.solution);
                        Ok(())
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        side.borrow_mut().get_or_insert(e);
                        Err(LinalgError::Operator(msg))
                    }
                })
            };
            let (a, at) = (make(false), make(true));
            let (inv, inv_t) = (inverse(false), inverse(true));
            let lift = |e: LinalgError| -> KrylovError {
                if let Some(inner) = side.borrow_mut().take() {
                    return inner;
                }
                if let Some(inner) = op_side.borrow_mut().take() {
                    return inner.into();
                }
                contract(format!("exact Krylov condition estimation failed: {e}")).into()
            };
            let na = onenormest(&a, &at).map_err(lift)?;
            let ni = onenormest(&inv, &inv_t).map_err(lift)?;
            let estimate = na.estimate * ni.estimate;
            if !estimate.is_finite() || estimate <= 0.0 {
                return Err(contract("exact Krylov condition estimate is nonfinite or nonpositive").into());
            }
            Ok(ExactConditionEstimate {
                value: estimate,
                condition_limit,
                safety_fraction: self.policy.condition_safety_fraction,
                method: "one_norm_iterative_inverse_estimate",
                admitted: estimate <= condition_limit * self.policy.condition_safety_fraction,
            })
        };
        let result = trace::lifecycle(
            "exact_krylov_condition_estimate",
            || {
                let mut f = crate::trace_fields! {"condition_limit" => condition_limit};
                f.extend(self.trace_fields.clone());
                f
            },
            run,
            |r| {
                let (f, t) = counts.get();
                crate::trace_fields! {
                    "condition_estimate" => r.value, "admitted" => r.admitted,
                    "forward_inverse_action_count" => f, "transpose_inverse_action_count" => t,
                    "total_inverse_action_count" => f + t,
                }
            },
        )?;
        if !result.admitted {
            return Err(KrylovError::ConditionInconclusive(
                "iterative condition estimate requires authoritative direct admission".into(),
            ));
        }
        Ok(result)
    }
}

impl Drop for ExactKrylovSession<'_> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectResult {
    pub solution: Vec<f64>,
    pub error_norm: f64,
    pub relative: f64,
    pub condition: f64,
    pub kind: String,
}

pub type DirectSolve<'a> = dyn FnMut(&[f64], bool) -> CaeResult<DirectResult> + 'a;

#[derive(Clone, Debug, PartialEq)]
pub struct ExactKrylovDispatchDiagnostics {
    pub requested_backend: String,
    pub used_backend: String,
    pub fallback_used: bool,
    pub fallback_reason: Option<String>,
    pub iterative_resources_released_before_fallback: bool,
    pub iterative_solve: Option<ExactKrylovSolveDiagnostics>,
    pub iterative_condition: Option<ExactConditionEstimate>,
    pub direct_factorization: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactKrylovDispatchResult {
    pub solution: Vec<f64>,
    pub error_norm: f64,
    pub relative_residual: f64,
    pub condition_estimate: Option<f64>,
    pub diagnostics: ExactKrylovDispatchDiagnostics,
}



pub fn direct_result(
    direct: &mut DirectSolve<'_>,
    rhs: &[f64],
    transpose: bool,
    matrix: Option<&AdmittedExactMatrix>,
    condition_limit: Option<f64>,
) -> CaeResult<DirectResult> {
    if rhs.is_empty() {
        return Err(contract("authoritative direct right-hand side must be nonempty"));
    }
    checked_vector(rhs, rhs.len(), "authoritative direct right-hand side")?;
    let mut value = direct(rhs, transpose)?;
    checked_vector(&value.solution, rhs.len(), "authoritative direct solution")?;
    for (v, label) in [
        (value.error_norm, "authoritative direct residual error norm"),
        (value.relative, "authoritative direct relative residual"),
    ] {
        if !v.is_finite() || v < 0.0 {
            return Err(contract(format!("{label} must be finite nonnegative numeric")));
        }
    }
    if !value.condition.is_finite() || value.condition <= 0.0 {
        return Err(contract("authoritative direct condition estimate must be finite positive numeric"));
    }
    if let Some(limit) = condition_limit {
        if !limit.is_finite() || limit <= 0.0 {
            return Err(contract("authoritative direct condition limit must be finite positive numeric"));
        }
        if limit <= 1.0 {
            return Err(contract("authoritative direct condition limit must exceed one"));
        }
        if value.condition > limit {
            return Err(contract("authoritative direct result exceeds its condition limit"));
        }
    }
    if value.kind.is_empty() {
        return Err(contract("authoritative direct factorization identity is invalid"));
    }
    if let Some(a) = matrix {
        let ax = a.matrix().apply(&value.solution, transpose).map_err(|e| {
            contract(format!("authoritative direct residual recertification failed: {}", e.message()))
        })?;
        let err: Vec<f64> = ax.iter().zip(rhs).map(|(p, q)| p - q).collect();
        checked_vector(&err, rhs.len(), "authoritative direct recertification residual")?;
        let (e, rel) = residual_certificate(&err, rhs)
            .map_err(|s| contract(format!("authoritative direct residual recertification failed: {s}")))?;
        if !rel.is_finite() || rel > 1e-8 {
            return Err(contract(
                "authoritative direct fallback failed independent residual recertification",
            ));
        }
        value.error_norm = e;
        value.relative = rel;
    }
    Ok(value)
}

fn rollback_all(
    recycle: Option<&ExactKrylovRecycleTransaction>,
    reuse: Option<&ExactPreconditionerReuseTransaction>,
    failures: &mut Vec<String>,
) {
    if let Some(r) = recycle
        && !r.closed()
        && let Err(e) = r.rollback()
    {
        failures.push(format!("recycle rollback failed: {}", e.message()));
    }
    if let Some(t) = reuse
        && !t.closed()
        && let Err(e) = t.rollback()
    {
        failures.push(format!("preconditioner reuse rollback failed: {}", e.message()));
    }
}

fn with_cleanup(primary: CaeError, label: &str, failures: &[String]) -> CaeError {
    if failures.is_empty() {
        primary
    } else {
        contract(format!("{label}; cleanup also failed: {}", failures.join("; ")))
    }
}

pub type LinearizationFactory<'f> =
    dyn FnMut(&ExactMatrixIdentity) -> CaeResult<Box<dyn ExactLinearization>> + 'f;

pub struct AssembledDispatch<'a, 'f> {
    pub condition_limit: f64,
    pub policy: ExactKrylovPolicy,
    pub direct_solve: &'a mut DirectSolve<'f>,
    pub capability_factory: Option<&'a mut LinearizationFactory<'f>>,
    pub preconditioner_factory: Option<&'a mut LeaseFactory<'f>>,
    pub execution_context: Option<&'a OperationExecutionContext>,
    pub lease_budget: Option<ExactPreconditionerLeaseBudget>,
    pub recycle: Option<&'a ExactKrylovRecycleTransaction>,
    pub reuse: Option<&'a ExactPreconditionerReuseTransaction>,
    pub transpose: bool,
    pub trace_fields: Option<Fields>,
    pub defer_condition_to_final: bool,
}



#[allow(clippy::too_many_lines)]
pub fn solve_with_authoritative_fallback(
    matrix: &AdmittedExactMatrix,
    rhs: &[f64],
    options: AssembledDispatch<'_, '_>,
) -> CaeResult<ExactKrylovDispatchResult> {
    let AssembledDispatch {
        condition_limit,
        policy,
        direct_solve,
        mut capability_factory,
        mut preconditioner_factory,
        execution_context,
        lease_budget,
        recycle,
        reuse,
        transpose,
        trace_fields,
        defer_condition_to_final,
    } = options;
    if !condition_limit.is_finite() || condition_limit <= 0.0 {
        return Err(contract("exact Krylov condition limit must be finite positive numeric"));
    }
    if condition_limit <= 1.0 {
        return Err(contract("exact Krylov condition limit must exceed one"));
    }
    if !policy.enabled {
        if reuse.is_some() {
            return Err(contract("disabled exact Krylov dispatch cannot reuse a preconditioner"));
        }
        let d = direct_result(direct_solve, rhs, transpose, None, Some(condition_limit))?;
        return Ok(ExactKrylovDispatchResult {
            solution: d.solution,
            error_norm: d.error_norm,
            relative_residual: d.relative,
            condition_estimate: Some(d.condition),
            diagnostics: ExactKrylovDispatchDiagnostics {
                requested_backend: "direct".into(),
                used_backend: "direct".into(),
                fallback_used: false,
                fallback_reason: None,
                iterative_resources_released_before_fallback: false,
                iterative_solve: None,
                iterative_condition: None,
                direct_factorization: Some(d.kind),
            },
        });
    }
    let identity = matrix.identity().clone();
    if let Some(t) = reuse {
        if t.closed() || *t.matrix_identity() != OperatorIdentity::Matrix(identity.clone()) {
            let mut f = Vec::new();
            rollback_all(recycle, None, &mut f);
            return Err(contract("exact preconditioner reuse transaction is incompatible"));
        }
        if preconditioner_factory.is_none() || capability_factory.is_some() {
            let mut f = Vec::new();
            rollback_all(recycle, reuse, &mut f);
            return Err(contract("exact preconditioner reuse requires one sealed provider path"));
        }
    }
    if let Err(e) = workspace_plan(&policy, identity.size) {
        let mut f = Vec::new();
        rollback_all(recycle, reuse, &mut f);
        return Err(with_cleanup(e, "exact Krylov workspace planning failed", &f));
    }
    let mut capability: Option<Box<dyn ExactLinearization>> = None;
    if let Some(factory) = capability_factory.as_mut() {
        match factory(&identity) {
            Ok(c) => capability = Some(c),
            Err(e) => {
                let mut f = Vec::new();
                rollback_all(recycle, reuse, &mut f);
                if !f.is_empty() {
                    return Err(contract(format!(
                        "exact Krylov capability construction failed; {}",
                        f.join("; ")
                    )));
                }
                return Err(contract(format!(
                    "exact Krylov capability construction failed: {}",
                    e.message()
                )));
            }
        }
    }
    let mut fallback_reason: Option<String> = None;
    let mut iterative: Option<ExactKrylovSolveResult> = None;
    let mut condition: Option<ExactConditionEstimate> = None;
    let mut last_diagnostics: Option<ExactKrylovSolveDiagnostics> = None;
    loop {
        let prepared = match preconditioner_factory.as_mut() {
            None => None,
            Some(factory) => {
                let prepared = match reuse {
                    Some(t) => {
                        let mut wrapped = |lease: &crate::preconditioner_lease::SealedMatrixReadLease,
                                           binding: &crate::preconditioner_lease::ExactPreconditionerBinding| {
                            t.prepare(lease, binding, &mut **factory)
                        };
                        prepare_provider_preconditioner(
                            matrix,
                            None,
                            execution_context,
                            &mut wrapped,
                            lease_budget,
                            1e-10,
                        )
                    }
                    None => prepare_provider_preconditioner(
                        matrix,
                        None,
                        execution_context,
                        &mut **factory,
                        lease_budget,
                        1e-10,
                    ),
                };
                match prepared {
                    Ok(p) => Some(p),
                    Err(e) if e.is_convergence() => {

                        if let Some(t) = reuse
                            && t.reused()
                            && !t.rebuild_attempted()
                        {
                            if let Err(err) = t.force_fresh_rebuild() {
                                let mut f = Vec::new();
                                rollback_all(recycle, reuse, &mut f);
                                return Err(with_cleanup(err, "fresh preconditioner rebuild failed", &f));
                            }
                            continue;
                        }
                        if let Some(c) = capability.take()
                            && let Err(r) = c.release()
                        {
                            let mut f = vec![format!("capability release failed: {}", r.message())];
                            rollback_all(recycle, reuse, &mut f);
                            return Err(with_cleanup(e, "sealed preconditioner preparation failed", &f));
                        }
                        fallback_reason = Some(format!("sealed preconditioner preparation: {}", e.message()));
                        break;
                    }
                    Err(e) => {
                        let mut f = Vec::new();
                        if let Some(c) = capability.take()
                            && let Err(r) = c.release()
                        {
                            f.push(format!("capability release failed: {}", r.message()));
                        }
                        rollback_all(recycle, reuse, &mut f);
                        return Err(with_cleanup(e, "sealed preconditioner preparation failed", &f));
                    }
                }
            }
        };
        let retry_snapshot = match (reuse, recycle) {
            (Some(t), Some(r)) if t.reused() => Some(r.snapshot()?),
            _ => None,
        };
        let session = ExactKrylovSession::new(
            SessionOperator::Assembled(matrix.clone()),
            policy,
            capability.take(),
            prepared.map(SessionPreconditioner::Bound),
            recycle,
            trace_fields.clone(),
        );
        let mut session = match session {
            Ok(s) => s,
            Err(e) => {
                let mut f = Vec::new();
                rollback_all(recycle, reuse, &mut f);
                return Err(with_cleanup(e, "exact Krylov session construction failed", &f));
            }
        };
        let attempt =
            (|| -> Result<(ExactKrylovSolveResult, Option<ExactConditionEstimate>), KrylovError> {
                session.enter()?;
                let solved = session.solve(rhs, transpose)?;
                let cond = if defer_condition_to_final {
                    None
                } else {
                    Some(session.estimate_condition(condition_limit)?)
                };
                Ok((solved, cond))
            })();
        let released = session.release();
        drop(session);
        match attempt {
            Ok((solved, cond)) => {
                if let Err(e) = released {
                    let mut f = Vec::new();
                    rollback_all(recycle, reuse, &mut f);
                    return Err(with_cleanup(e, "exact Krylov correction failed", &f));
                }
                iterative = Some(solved);
                condition = cond;
                break;
            }
            Err(e) if e.is_fallback() => {
                if let Err(r) = released {
                    let mut f = Vec::new();
                    rollback_all(recycle, reuse, &mut f);
                    return Err(with_cleanup(r, "exact Krylov correction failed", &f));
                }
                if let Some(t) = reuse
                    && t.reused()
                    && !t.rebuild_attempted()
                {
                    let rebuild = t.force_fresh_rebuild().and_then(|()| match (recycle, &retry_snapshot) {
                        (Some(r), Some(s)) => r.restore(s),
                        _ => Ok(()),
                    });
                    if let Err(err) = rebuild {
                        let mut f = Vec::new();
                        rollback_all(recycle, reuse, &mut f);
                        return Err(with_cleanup(err, "fresh preconditioner rebuild failed", &f));
                    }
                    continue;
                }
                fallback_reason = Some(e.reason());
                break;
            }
            Err(e) => {
                let err = match e {
                    KrylovError::Contract(c) => c,
                    other => contract(other.to_string()),
                };
                let mut f = Vec::new();
                rollback_all(recycle, reuse, &mut f);
                return Err(with_cleanup(err, "exact Krylov correction failed", &f));
            }
        }
    }
    if let Some(reason) = fallback_reason {
        let mut failures = Vec::new();
        if let Some(t) = reuse
            && !t.closed()
            && let Err(e) = t.abort_store()
        {
            failures.push(format!("preconditioner reuse abort failed: {}", e.message()));
        }
        if let Some(r) = recycle
            && let Err(e) = r.abort_store()
        {
            failures.push(format!("Krylov recycle abort failed: {}", e.message()));
        }
        if !failures.is_empty() {
            return Err(contract(failures.join("; ")));
        }
        let iterative_diagnostics = iterative.take().map(|i| i.diagnostics).or(last_diagnostics.take());
        let d = direct_result(direct_solve, rhs, transpose, Some(matrix), Some(condition_limit))?;
        return Ok(ExactKrylovDispatchResult {
            solution: d.solution,
            error_norm: d.error_norm,
            relative_residual: d.relative,
            condition_estimate: Some(d.condition),
            diagnostics: ExactKrylovDispatchDiagnostics {
                requested_backend: policy.method.as_str().into(),
                used_backend: "direct".into(),
                fallback_used: true,
                fallback_reason: Some(reason),
                iterative_resources_released_before_fallback: true,
                iterative_solve: iterative_diagnostics,
                iterative_condition: None,
                direct_factorization: Some(d.kind),
            },
        });
    }
    let solved = iterative.ok_or_else(|| contract("exact Krylov correction produced no result"))?;
    Ok(ExactKrylovDispatchResult {
        solution: solved.solution,
        error_norm: solved.error_norm,
        relative_residual: solved.relative_residual,
        condition_estimate: condition.as_ref().map(|c| c.value),
        diagnostics: ExactKrylovDispatchDiagnostics {
            requested_backend: policy.method.as_str().into(),
            used_backend: policy.method.as_str().into(),
            fallback_used: false,
            fallback_reason: None,
            iterative_resources_released_before_fallback: false,
            iterative_solve: Some(solved.diagnostics),
            iterative_condition: condition,
            direct_factorization: None,
        },
    })
}

pub type RefreshFactory<'a> = dyn FnMut() -> CaeResult<Box<dyn MatrixFreeLinearization>> + 'a;

pub struct MatrixFreeDispatch<'a, 'f> {
    pub condition_limit: f64,
    pub policy: ExactKrylovPolicy,
    pub direct_solve: &'a mut DirectSolve<'f>,
    pub lagged_preconditioner: Option<Box<dyn Preconditioner>>,
    pub recycle: Option<&'a ExactKrylovRecycleTransaction>,
    pub before_fallback: Option<&'a mut (dyn FnMut() -> CaeResult<()> + 'f)>,
    pub reuse: Option<&'a ExactPreconditionerReuseTransaction>,
    pub refresh_capability: Option<&'a mut RefreshFactory<'f>>,
    pub transpose: bool,
    pub trace_fields: Option<Fields>,
    pub defer_condition_to_final: bool,
}



#[allow(clippy::too_many_lines)]
pub fn solve_matrix_free_with_assembled_fallback(
    capability: Box<dyn MatrixFreeLinearization>,
    rhs: &[f64],
    options: MatrixFreeDispatch<'_, '_>,
) -> CaeResult<ExactKrylovDispatchResult> {
    let MatrixFreeDispatch {
        condition_limit,
        policy,
        direct_solve,
        lagged_preconditioner,
        recycle,
        mut before_fallback,
        reuse,
        mut refresh_capability,
        transpose,
        trace_fields,
        defer_condition_to_final,
    } = options;
    let release_cap = |c: Box<dyn MatrixFreeLinearization>| {
        let _ = c.release();
    };
    if !policy.enabled {
        release_cap(capability);
        return Err(contract("matrix-free exact dispatch requires an enabled Krylov policy"));
    }
    if reuse.is_some() && (lagged_preconditioner.is_some() || before_fallback.is_some()) {
        release_cap(capability);
        return Err(contract("matrix-free exact reuse cannot select a second preconditioner lifecycle"));
    }
    if !condition_limit.is_finite() || condition_limit <= 0.0 {
        release_cap(capability);
        return Err(contract("matrix-free exact condition limit must be finite positive numeric"));
    }
    if condition_limit <= 1.0 {
        release_cap(capability);
        return Err(contract("matrix-free exact condition limit must exceed one"));
    }
    let identity = capability.identity();
    if capability.shape() != (identity.size, identity.size) {
        release_cap(capability);
        return Err(contract("matrix-free exact capability metadata is incomplete"));
    }
    let op_identity = OperatorIdentity::Opaque(identity.clone());
    if let Some(r) = recycle
        && (r.closed() || *r.identity() != op_identity)
    {
        release_cap(capability);
        return Err(contract("matrix-free exact recycle transaction is incompatible"));
    }
    if let Some(t) = reuse {
        if t.closed() || *t.matrix_identity() != op_identity || t.shape() != (identity.size, identity.size) {
            let mut f = Vec::new();
            rollback_all(recycle, None, &mut f);
            release_cap(capability);
            return Err(contract("matrix-free exact preconditioner reuse transaction is incompatible"));
        }
        if refresh_capability.is_none() {
            let mut f = Vec::new();
            rollback_all(recycle, reuse, &mut f);
            release_cap(capability);
            return Err(contract("matrix-free exact preconditioner reuse requires a refresh factory"));
        }
    }
    let cleanup_error = |primary: CaeError, extra: Vec<String>| -> CaeError {
        let mut f = extra;
        rollback_all(recycle, reuse, &mut f);
        if f.is_empty() {
            primary
        } else {
            contract(format!("matrix-free exact correction cleanup failed: {}", f.join("; ")))
        }
    };
    let mut current = Some(capability);
    let mut current_pre: Option<Box<dyn Preconditioner>> = lagged_preconditioner;
    let fallback_reason: String;
    let mut iterative_diagnostics: Option<ExactKrylovSolveDiagnostics> = None;
    loop {
        let mut retry_snapshot = None;
        if let Some(t) = reuse {
            let cap = current.as_ref().ok_or_else(|| contract("matrix-free exact capability is released"))?;
            let mut factory = || cap.prepare_preconditioner();
            match t.prepare_opaque(&mut factory) {
                Ok(p) => current_pre = Some(p),
                Err(e) => {
                    let mut f = Vec::new();
                    if let Some(c) = current.take()
                        && let Err(r) = c.release()
                    {
                        f.push(format!("capability release failed: {}", r.message()));
                    }
                    return Err(cleanup_error(e, f));
                }
            }
            if t.reused()
                && let Some(r) = recycle
            {
                retry_snapshot = Some(r.snapshot()?);
            }
        }
        let cap = current.take().ok_or_else(|| contract("matrix-free exact capability is released"))?;
        let session = ExactKrylovSession::new(
            SessionOperator::MatrixFree(cap),
            policy,
            None,
            current_pre.take().map(SessionPreconditioner::Opaque),
            recycle,
            trace_fields.clone(),
        );
        let mut session = match session {
            Ok(s) => s,
            Err(e) => return Err(cleanup_error(e, Vec::new())),
        };
        let attempt =
            (|| -> Result<(ExactKrylovSolveResult, Option<ExactConditionEstimate>), KrylovError> {
                session.enter()?;
                let solved = session.solve(rhs, transpose)?;
                let cond = if defer_condition_to_final {
                    None
                } else {
                    Some(session.estimate_condition(condition_limit)?)
                };
                Ok((solved, cond))
            })();
        let released = session.release();
        drop(session);
        match attempt {
            Ok((solved, cond)) => {
                if let Err(e) = released {
                    return Err(cleanup_error(e, Vec::new()));
                }
                return Ok(ExactKrylovDispatchResult {
                    solution: solved.solution,
                    error_norm: solved.error_norm,
                    relative_residual: solved.relative_residual,
                    condition_estimate: cond.as_ref().map(|c| c.value),
                    diagnostics: ExactKrylovDispatchDiagnostics {
                        requested_backend: format!("matrix_free_{}", policy.method.as_str()),
                        used_backend: format!("matrix_free_{}", policy.method.as_str()),
                        fallback_used: false,
                        fallback_reason: None,
                        iterative_resources_released_before_fallback: false,
                        iterative_solve: Some(solved.diagnostics),
                        iterative_condition: cond,
                        direct_factorization: None,
                    },
                });
            }
            Err(e) if e.is_fallback() => {
                if let Err(r) = released {
                    return Err(cleanup_error(r, Vec::new()));
                }
                if let Some(t) = reuse
                    && t.reused()
                    && !t.rebuild_attempted()
                {
                    let rebuilt = (|| -> CaeResult<Box<dyn MatrixFreeLinearization>> {
                        t.force_fresh_rebuild()?;
                        if let (Some(r), Some(s)) = (recycle, &retry_snapshot) {
                            r.restore(s)?;
                        }
                        let factory = refresh_capability.as_mut().ok_or_else(|| {
                            contract("matrix-free exact preconditioner reuse requires a refresh factory")
                        })?;
                        let refreshed = factory().map_err(|e| {
                            contract(format!("matrix-free exact capability refresh failed: {}", e.message()))
                        })?;
                        if refreshed.identity() != identity
                            || refreshed.shape() != (identity.size, identity.size)
                        {
                            let detail = match refreshed.release() {
                                Ok(()) => String::new(),
                                Err(r) => format!(
                                    "; cleanup also failed: capability release failed: {}",
                                    r.message()
                                ),
                            };
                            return Err(contract(format!(
                                "matrix-free exact refreshed capability identity drifted{detail}"
                            )));
                        }
                        Ok(refreshed)
                    })();
                    match rebuilt {
                        Ok(c) => {
                            current = Some(c);
                            current_pre = None;
                            continue;
                        }
                        Err(err) => return Err(cleanup_error(err, Vec::new())),
                    }
                }
                fallback_reason = e.reason();
                break;
            }
            Err(e) => {
                let err = match e {
                    KrylovError::Contract(c) => c,
                    other => contract(other.to_string()),
                };
                return Err(cleanup_error(err, Vec::new()));
            }
        }
    }
    if let Some(t) = reuse
        && !t.closed()
    {
        t.abort_store().map_err(|e| {
            contract(format!("matrix-free exact preconditioner abort failed: {}", e.message()))
        })?;
    }
    if let Some(r) = recycle {
        r.abort_store()?;
    }
    if let Some(hook) = before_fallback.as_mut() {
        hook().map_err(|e| {
            contract(format!("matrix-free exact fallback preparation failed: {}", e.message()))
        })?;
    }
    iterative_diagnostics.take();
    let d = direct_result(direct_solve, rhs, transpose, None, Some(condition_limit))?;
    Ok(ExactKrylovDispatchResult {
        solution: d.solution,
        error_norm: d.error_norm,
        relative_residual: d.relative,
        condition_estimate: Some(d.condition),
        diagnostics: ExactKrylovDispatchDiagnostics {
            requested_backend: format!("matrix_free_{}", policy.method.as_str()),
            used_backend: "direct".into(),
            fallback_used: true,
            fallback_reason: Some(fallback_reason),
            iterative_resources_released_before_fallback: true,
            iterative_solve: None,
            iterative_condition: None,
            direct_factorization: Some(d.kind),
        },
    })
}

