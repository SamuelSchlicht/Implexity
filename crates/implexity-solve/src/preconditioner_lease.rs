// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::{Arc, Mutex, MutexGuard};

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::{CscMatrix, CsrMatrix};
use sha2::{Digest, Sha256};

use crate::certificate::norm2;
use crate::exact_matrix::{
    AdmittedExactMatrix, CanonicalMatrix, ExactMatrixIdentity, MatrixInput, admit_exact_matrix,
};
use crate::matrix::linspace;
use crate::newton_krylov::OperatorIdentity;
use crate::operation_context::{
    ContextRequirement, OperationExecutionContext, is_sha256_hex, require_operation_context,
};

const SPARSE_PATTERN_SCHEMA: &[u8] = b"implexity-exact-sparse-pattern/1\0";
pub const DEFAULT_REUSE_QUALITY_LIMIT: f64 = 0.95;

fn lease_error(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn checked_digest(value: &str, label: &str) -> CaeResult<String> {
    if is_sha256_hex(value) {
        Ok(value.to_string())
    } else {
        Err(lease_error(format!("{label} must be a lowercase SHA-256 digest")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactPreconditionerLeaseBudget {
    pub maximum_entries: usize,
    pub maximum_bytes: usize,
}

impl ExactPreconditionerLeaseBudget {


    pub fn new(maximum_entries: usize, maximum_bytes: usize) -> CaeResult<Self> {
        if maximum_entries < 1 {
            return Err(lease_error("lease maximum entries must be a positive integer"));
        }
        if maximum_bytes < 1 {
            return Err(lease_error("lease maximum bytes must be a positive integer"));
        }
        Ok(Self { maximum_entries, maximum_bytes })
    }



    pub fn conservative_vector_access(size: usize) -> CaeResult<Self> {
        if size < 1 {
            return Err(lease_error("lease default budget requires a positive matrix size"));
        }
        let entries = 16 * size;
        let index = size_of::<usize>();
        Ok(Self { maximum_entries: entries, maximum_bytes: entries * (8 + 2 * index) + (size + 1) * index })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExactSparsePatternIdentity {
    pub sha256: String,
    pub shape: (usize, usize),
    pub entries: usize,
}

fn index_le(values: &[usize], width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * width);
    for &v in values {
        if width == 4 {
            out.extend_from_slice(&u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes());
        } else {
            out.extend_from_slice(&(v as u64).to_le_bytes());
        }
    }
    out
}



pub fn exact_sparse_pattern_identity(
    matrix: impl Into<MatrixInput>,
) -> CaeResult<ExactSparsePatternIdentity> {
    let admitted = admit_exact_matrix(matrix, None)?;
    let CanonicalMatrix::Sparse(m) = admitted.matrix() else {
        return Err(lease_error("preconditioner reuse requires a canonical sparse matrix"));
    };
    let mut digest = Sha256::new();
    digest.update(SPARSE_PATTERN_SCHEMA);
    digest.update((m.nrows() as u64).to_be_bytes());
    digest.update((m.ncols() as u64).to_be_bytes());
    digest.update((m.nnz() as u64).to_be_bytes());
    let width = if i32::try_from(m.nnz().max(m.nrows()).max(m.ncols())).is_ok() { 4 } else { 8 };
    let dtype: &[u8] = if width == 4 { b"<i4" } else { b"<i8" };
    for (label, values) in [(&b"indptr"[..], m.indptr()), (&b"indices"[..], m.indices())] {
        digest.update((label.len() as u64).to_be_bytes());
        digest.update(label);
        digest.update((dtype.len() as u64).to_be_bytes());
        digest.update(dtype);
        digest.update((values.len() as u64).to_be_bytes());
        digest.update(index_le(values, width));
    }
    Ok(ExactSparsePatternIdentity {
        sha256: hex::encode(digest.finalize()),
        shape: m.shape(),
        entries: m.nnz(),
    })
}

fn checked_vector(value: Vec<f64>, size: usize, label: &str) -> CaeResult<Vec<f64>> {
    if value.len() != size || value.iter().any(|v| !v.is_finite()) {
        return Err(lease_error(format!("{label} must be a finite vector with shape ({size},)")));
    }
    Ok(value)
}

fn checked_indices(value: &[usize], size: usize, label: &str) -> CaeResult<Vec<usize>> {
    if value.is_empty() || value.iter().any(|&i| i >= size) {
        return Err(lease_error(format!(
            "{label} must be a nonempty one-dimensional in-range integer index set"
        )));
    }
    let mut sorted = value.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(lease_error(format!("{label} must not contain duplicates")));
    }
    Ok(value.to_vec())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactPreconditionerBinding {
    pub matrix_identity: ExactMatrixIdentity,
    pub scope_digest: String,
    pub shape: (usize, usize),
}

impl ExactPreconditionerBinding {


    pub fn new(
        matrix_identity: ExactMatrixIdentity,
        scope_digest: &str,
        shape: (usize, usize),
    ) -> CaeResult<Self> {
        let scope_digest = checked_digest(scope_digest, "preconditioner scope digest")?;
        if shape.0 < 1 || shape.1 < 1 {
            return Err(lease_error(
                "preconditioner binding shape must be a nonempty two-dimensional integer shape",
            ));
        }
        if shape.0 != shape.1 || shape.0 != matrix_identity.size {
            return Err(lease_error("preconditioner binding shape disagrees with its exact matrix identity"));
        }
        Ok(Self { matrix_identity, scope_digest, shape })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MatrixCopy {
    Sparse(CscMatrix),
    Dense(DenseMatrix),
}

struct LeaseState {
    matrix: Option<Arc<CanonicalMatrix>>,
    used_entries: usize,
    used_bytes: usize,
}

pub struct SealedMatrixReadLease {
    identity: ExactMatrixIdentity,
    shape: (usize, usize),
    storage: &'static str,
    budget: ExactPreconditionerLeaseBudget,
    sparse_copy_entries: usize,
    sparse_copy_bytes: usize,
    state: Mutex<LeaseState>,
}

impl std::fmt::Debug for SealedMatrixReadLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedMatrixReadLease")
            .field("shape", &self.shape)
            .field("closed", &self.closed())
            .finish_non_exhaustive()
    }
}

impl SealedMatrixReadLease {


    pub fn new(
        matrix: impl Into<MatrixInput>,
        budget: Option<ExactPreconditionerLeaseBudget>,
    ) -> CaeResult<Self> {
        let admitted = admit_exact_matrix(matrix, None)?;
        let identity = admitted.identity().clone();
        let budget = match budget {
            Some(b) => b,
            None => ExactPreconditionerLeaseBudget::conservative_vector_access(identity.size)?,
        };
        let shared = admitted.shared();
        let (storage, sparse_copy_entries, sparse_copy_bytes) = match shared.as_ref() {
            CanonicalMatrix::Sparse(m) => ("csc", m.nnz() * 2 + m.indptr().len(), shared.nbytes()),
            CanonicalMatrix::Dense(_) => ("dense", 0, 0),
        };
        Ok(Self {
            shape: (identity.size, identity.size),
            identity,
            storage,
            budget,
            sparse_copy_entries,
            sparse_copy_bytes,
            state: Mutex::new(LeaseState { matrix: Some(shared), used_entries: 0, used_bytes: 0 }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, LeaseState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn open(&self) -> CaeResult<Arc<CanonicalMatrix>> {
        self.lock().matrix.clone().ok_or_else(|| lease_error("provider matrix read lease is closed"))
    }

    fn reserve(&self, entries: usize, bytes: usize, label: &str) -> CaeResult<Arc<CanonicalMatrix>> {
        let mut state = self.lock();
        let Some(matrix) = state.matrix.clone() else {
            return Err(lease_error("provider matrix read lease is closed"));
        };
        if state.used_entries + entries > self.budget.maximum_entries
            || state.used_bytes + bytes > self.budget.maximum_bytes
        {
            return Err(lease_error(format!("{label} exceeds the operation-scoped matrix read budget")));
        }
        state.used_entries += entries;
        state.used_bytes += bytes;
        Ok(matrix)
    }

    #[must_use]
    pub fn closed(&self) -> bool {
        self.lock().matrix.is_none()
    }



    pub fn identity(&self) -> CaeResult<&ExactMatrixIdentity> {
        self.open()?;
        Ok(&self.identity)
    }



    pub fn shape(&self) -> CaeResult<(usize, usize)> {
        self.open()?;
        Ok(self.shape)
    }



    pub fn storage(&self) -> CaeResult<&'static str> {
        self.open()?;
        Ok(self.storage)
    }



    pub fn budget(&self) -> CaeResult<ExactPreconditionerLeaseBudget> {
        self.open()?;
        Ok(self.budget)
    }



    pub fn remaining(&self) -> CaeResult<(usize, usize)> {
        let state = self.lock();
        if state.matrix.is_none() {
            return Err(lease_error("provider matrix read lease is closed"));
        }
        Ok((self.budget.maximum_entries - state.used_entries, self.budget.maximum_bytes - state.used_bytes))
    }



    pub fn diagonal_copy(&self) -> CaeResult<Vec<f64>> {
        let n = self.shape.0;
        let m = self.reserve(n, n * 8, "matrix diagonal copy")?;
        let d = match m.as_ref() {
            CanonicalMatrix::Sparse(a) => (0..n).map(|i| a.get(i, i)).collect(),
            CanonicalMatrix::Dense(a) => (0..n).map(|i| a.get(i, i)).collect(),
        };
        checked_vector(d, n, "authoritative matrix diagonal copy")
            .map_err(|_| lease_error("authoritative matrix produced an invalid diagonal copy"))
    }



    pub fn indexed_submatrix_copy(&self, rows: &[usize], columns: &[usize]) -> CaeResult<MatrixCopy> {
        self.open()?;
        let r = checked_indices(rows, self.shape.0, "row indices")?;
        let c = checked_indices(columns, self.shape.1, "column indices")?;
        let values = r.len() * c.len();
        let copied = r.len() + c.len();
        let idx = size_of::<usize>();
        let bytes = values * (8 + 2 * idx) + (c.len() + 1 + copied) * idx;
        let m = self.reserve(values + copied, bytes, "indexed matrix copy")?;
        let get = |i: usize, j: usize| match m.as_ref() {
            CanonicalMatrix::Sparse(a) => a.get(i, j),
            CanonicalMatrix::Dense(a) => a.get(i, j),
        };
        let dense: Vec<f64> =
            r.iter().flat_map(|&i| c.iter().map(move |&j| (i, j))).map(|(i, j)| get(i, j)).collect();
        if dense.iter().any(|v| !v.is_finite()) {
            return Err(lease_error("authoritative matrix produced an invalid indexed submatrix copy"));
        }
        match m.as_ref() {
            CanonicalMatrix::Dense(_) => {
                Ok(MatrixCopy::Dense(DenseMatrix { nrows: r.len(), ncols: c.len(), data: dense }))
            }
            CanonicalMatrix::Sparse(a) => {

                let mut ri = Vec::new();
                let mut ci = Vec::new();
                let mut vals = Vec::new();
                for (jj, &j) in c.iter().enumerate() {
                    let (rows_j, vals_j) = a.col(j);
                    for (ii, &i) in r.iter().enumerate() {
                        if let Ok(pos) = rows_j.binary_search(&i) {
                            ri.push(ii);
                            ci.push(jj);
                            vals.push(vals_j[pos]);
                        }
                    }
                }
                let block = CscMatrix::from_triplets(r.len(), c.len(), &ri, &ci, &vals)
                    .map_err(|e| lease_error(e.to_string()))?;
                Ok(MatrixCopy::Sparse(block))
            }
        }
    }



    pub fn sparse_csc_copy(&self) -> CaeResult<CscMatrix> {
        self.open()?;
        if self.storage != "csc" {
            return Err(lease_error("canonical sparse copy requires a sparse authoritative matrix"));
        }
        let m =
            self.reserve(self.sparse_copy_entries, self.sparse_copy_bytes, "canonical sparse matrix copy")?;
        match m.as_ref() {
            CanonicalMatrix::Sparse(a) => Ok(a.clone()),
            CanonicalMatrix::Dense(_) => {
                Err(lease_error("canonical sparse copy requires a sparse authoritative matrix"))
            }
        }
    }

    fn action(&self, value: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let entries = 2 * (self.shape.0 + self.shape.1);
        let label = if transpose { "matrix-lease rmatvec" } else { "matrix-lease matvec" };
        let m = self.reserve(entries, entries * 8, label)?;
        let operand = checked_vector(value.to_vec(), self.shape.1, &format!("{label} operand"))?;
        checked_vector(m.apply(&operand, transpose)?, self.shape.0, &format!("{label} result"))
    }



    pub fn matvec(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.action(value, false)
    }



    pub fn rmatvec(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.action(value, true)
    }

    pub fn close(&self) {
        self.lock().matrix = None;
    }
}


pub trait Preconditioner: Send + Sync {
    fn shape(&self) -> (usize, usize);


    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>>;


    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>>;


    fn release(&self) -> CaeResult<()>;
}

pub trait ProviderPreconditioner: Preconditioner {
    fn matrix_identity(&self) -> ExactMatrixIdentity;
    fn scope_digest(&self) -> String;
}

pub type LeaseFactory<'a> = dyn FnMut(&SealedMatrixReadLease, &ExactPreconditionerBinding) -> CaeResult<Box<dyn ProviderPreconditioner>>
    + 'a;

pub struct BoundPreparedPreconditioner {
    binding: ExactPreconditionerBinding,
    provider: Mutex<Option<Box<dyn ProviderPreconditioner>>>,
}

impl std::fmt::Debug for BoundPreparedPreconditioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundPreparedPreconditioner").field("binding", &self.binding).finish_non_exhaustive()
    }
}

impl BoundPreparedPreconditioner {
    fn new(provider: Box<dyn ProviderPreconditioner>, binding: ExactPreconditionerBinding) -> Self {
        Self { binding, provider: Mutex::new(Some(provider)) }
    }

    #[must_use]
    pub fn binding(&self) -> &ExactPreconditionerBinding {
        &self.binding
    }

    #[must_use]
    pub fn released(&self) -> bool {
        self.provider.lock().map_or(true, |p| p.is_none())
    }

    fn action(&self, name: &str, value: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let guard = self.provider.lock().map_err(|_| lease_error("prepared preconditioner is released"))?;
        let Some(provider) = guard.as_ref() else {
            return Err(lease_error("prepared preconditioner is released"));
        };
        let operand = checked_vector(
            value.to_vec(),
            self.binding.shape.1,
            &format!("prepared preconditioner {name} operand"),
        )?;
        let raw = if transpose { provider.apply_transpose(&operand) } else { provider.apply(&operand) }
            .map_err(|e| {
                lease_error(format!("provider prepared preconditioner {name} failed: {}", e.message()))
            })?;
        checked_vector(raw, self.binding.shape.0, &format!("prepared preconditioner {name} result"))
    }
}

impl Preconditioner for BoundPreparedPreconditioner {
    fn shape(&self) -> (usize, usize) {
        self.binding.shape
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.action("apply", value, false)
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.action("apply_transpose", value, true)
    }
    fn release(&self) -> CaeResult<()> {
        let provider =
            self.provider.lock().map_err(|_| lease_error("prepared preconditioner is released"))?.take();
        match provider {
            None => Ok(()),
            Some(p) => p.release().map_err(|e| {
                lease_error(format!("provider prepared preconditioner release failed: {}", e.message()))
            }),
        }
    }
}

fn provider_metadata(
    provider: &dyn ProviderPreconditioner,
    binding: &ExactPreconditionerBinding,
) -> CaeResult<()> {
    if provider.matrix_identity() != binding.matrix_identity {
        return Err(lease_error("prepared preconditioner has a stale exact matrix identity"));
    }
    if provider.scope_digest() != binding.scope_digest {
        return Err(lease_error("prepared preconditioner has a stale opaque scope digest"));
    }
    let shape = provider.shape();
    if shape.0 < 1 || shape.1 < 1 {
        return Err(lease_error(
            "prepared preconditioner shape must be a nonempty two-dimensional integer shape",
        ));
    }
    if shape != binding.shape {
        return Err(lease_error("prepared preconditioner shape disagrees with the authoritative matrix"));
    }
    Ok(())
}

fn release_failed_preparation(provider: &dyn Preconditioner, primary: CaeError) -> CaeError {
    match provider.release() {
        Ok(()) => primary,
        Err(release) => lease_error(format!(
            "{}; provider release after failed preparation also failed: {}",
            primary.message(),
            release.message()
        )),
    }
}

fn duality_scale(p: &[f64], q: &[f64], forward: &[f64], reverse: &[f64], a: f64, b: f64) -> f64 {
    let ta: f64 = q.iter().zip(forward).map(|(x, y)| (x * y).abs()).sum();
    let tb: f64 = p.iter().zip(reverse).map(|(x, y)| (x * y).abs()).sum();
    1.0_f64.max(a.abs()).max(b.abs()).max(ta).max(tb)
}



pub fn duality_check(pre: &dyn Preconditioner, size: usize, tolerance: f64, message: &str) -> CaeResult<()> {
    let p = linspace(0.25, 1.25, size);
    let q = linspace(-0.75, 0.5, size);
    let forward = pre.apply(&p)?;
    let reverse = pre.apply_transpose(&q)?;
    let a: f64 = q.iter().zip(&forward).map(|(x, y)| x * y).sum();
    let b: f64 = p.iter().zip(&reverse).map(|(x, y)| x * y).sum();
    let scale = duality_scale(&p, &q, &forward, &reverse, a, b);
    if !(a.is_finite() && b.is_finite() && scale.is_finite()) || (a - b).abs() > tolerance * scale {
        return Err(CaeError::convergence(format!(
            "{message} (q.Mp = {a:e}, p.MTq = {b:e}, rounding scale {scale:e}, tolerance {tolerance:e})"
        )));
    }
    Ok(())
}

fn validate_opaque(provider: &dyn Preconditioner, shape: (usize, usize)) -> CaeResult<()> {
    if provider.shape() != shape {
        return Err(lease_error("opaque prepared preconditioner is incomplete or has the wrong shape"));
    }
    let size = shape.0;
    let p = linspace(0.25, 1.25, size);
    let q = linspace(-0.75, 0.5, size);
    let forward = provider
        .apply(&p)
        .and_then(|v| checked_vector(v, size, "opaque prepared preconditioner forward result"))
        .map_err(|e| lease_error(format!("opaque prepared preconditioner action failed: {}", e.message())))?;
    let reverse = provider
        .apply_transpose(&q)
        .and_then(|v| checked_vector(v, size, "opaque prepared preconditioner transpose result"))
        .map_err(|e| lease_error(format!("opaque prepared preconditioner action failed: {}", e.message())))?;
    let a: f64 = q.iter().zip(&forward).map(|(x, y)| x * y).sum();
    let b: f64 = p.iter().zip(&reverse).map(|(x, y)| x * y).sum();
    if (a - b).abs() > 1e-10 * duality_scale(&p, &q, &forward, &reverse, a, b) {
        return Err(lease_error("opaque prepared preconditioner forward/transpose actions fail duality"));
    }
    Ok(())
}



pub fn prepare_provider_preconditioner(
    matrix: impl Into<MatrixInput>,
    scope_digest: Option<&str>,
    execution_context: Option<&OperationExecutionContext>,
    factory: &mut LeaseFactory<'_>,
    lease_budget: Option<ExactPreconditionerLeaseBudget>,
    transpose_tolerance: f64,
) -> CaeResult<BoundPreparedPreconditioner> {
    let mut scope = scope_digest.map(str::to_string);
    if let Some(ctx) = execution_context {
        let ctx = require_operation_context(
            Some(ctx),
            ContextRequirement::default(),
            "preconditioner operation context",
        )?;
        if scope.as_deref().is_some_and(|s| s != ctx.scope_digest()) {
            return Err(lease_error("preconditioner scope digest disagrees with its operation context"));
        }
        scope = Some(ctx.scope_digest().to_string());
    }
    let scope = checked_digest(scope.as_deref().unwrap_or(""), "preconditioner scope digest")?;
    if !transpose_tolerance.is_finite() || transpose_tolerance <= 0.0 || transpose_tolerance > 1e-8 {
        return Err(lease_error("preconditioner transpose tolerance must lie in (0, 1e-8]"));
    }
    let admitted = admit_exact_matrix(matrix, None)?;
    let binding = ExactPreconditionerBinding::new(
        admitted.identity().clone(),
        &scope,
        (admitted.size(), admitted.size()),
    )?;
    let lease = SealedMatrixReadLease::new(&admitted, lease_budget)?;
    let provider = factory(&lease, &binding);
    lease.close();
    let provider = provider
        .map_err(|e| lease_error(format!("provider preconditioner preparation failed: {}", e.message())))?;
    if let Err(e) = provider_metadata(provider.as_ref(), &binding) {
        return Err(release_failed_preparation(provider.as_ref(), e));
    }
    let bound = BoundPreparedPreconditioner::new(provider, binding.clone());
    if let Err(e) = duality_check(
        &bound,
        binding.shape.0,
        transpose_tolerance,
        "prepared preconditioner forward/transpose actions fail duality",
    ) {
        return Err(match bound.release() {
            Ok(()) => e,
            Err(r) => lease_error(format!(
                "{}; provider release after failed preparation also failed: {}",
                e.message(),
                r.message()
            )),
        });
    }
    Ok(bound)
}

struct Retained {
    provider: Mutex<Option<Box<dyn Preconditioner>>>,
    source_identity: OperatorIdentity,
    pattern: Option<ExactSparsePatternIdentity>,
    profile: String,
    scope_digest: String,
    shape: (usize, usize),
}

impl Retained {
    fn released(&self) -> bool {
        self.provider.lock().map_or(true, |p| p.is_none())
    }
    fn with<T>(&self, f: impl FnOnce(&dyn Preconditioner) -> CaeResult<T>) -> CaeResult<T> {
        let guard = self.provider.lock().map_err(|_| lease_error("retained preconditioner is released"))?;
        match guard.as_ref() {
            Some(p) => f(p.as_ref()),
            None => Err(lease_error("retained preconditioner is released")),
        }
    }
    fn release(&self) -> CaeResult<()> {
        let provider =
            self.provider.lock().map_err(|_| lease_error("retained preconditioner is released"))?.take();
        match provider {
            None => Ok(()),
            Some(p) => p.release().map_err(|e| {
                lease_error(format!("retained provider preconditioner release failed: {}", e.message()))
            }),
        }
    }
}

fn reuse_quality(lease: &SealedMatrixReadLease, retained: &Retained) -> CaeResult<f64> {
    let size = retained.shape.0;
    let fp = linspace(0.25, 1.25, size);
    let tp = linspace(-0.75, 0.5, size);
    let forward = checked_vector(
        retained.with(|p| p.apply(&fp))?,
        size,
        "retained preconditioner forward probe result",
    )?;
    let reverse = checked_vector(
        retained.with(|p| p.apply_transpose(&tp))?,
        size,
        "retained preconditioner transpose probe result",
    )?;
    let fe: Vec<f64> = lease.matvec(&forward)?.iter().zip(&fp).map(|(a, b)| a - b).collect();
    let re: Vec<f64> = lease.rmatvec(&reverse)?.iter().zip(&tp).map(|(a, b)| a - b).collect();
    let quality = (norm2(&fe) / 1.0_f64.max(norm2(&fp))).max(norm2(&re) / 1.0_f64.max(norm2(&tp)));
    if !quality.is_finite() || quality < 0.0 {
        return Err(lease_error("retained preconditioner quality probe is invalid"));
    }
    Ok(quality)
}

pub struct BorrowedPreconditioner {
    matrix_identity: Option<ExactMatrixIdentity>,
    scope_digest: String,
    shape: (usize, usize),
    retained: Mutex<Option<Arc<Retained>>>,
    transaction: Option<Arc<Mutex<TxState>>>,
    borrow_id: u64,
}

impl std::fmt::Debug for BorrowedPreconditioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BorrowedPreconditioner").field("shape", &self.shape).finish_non_exhaustive()
    }
}

impl BorrowedPreconditioner {
    fn retained(&self) -> CaeResult<Arc<Retained>> {
        self.retained
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .ok_or_else(|| lease_error("borrowed preconditioner is released"))
    }
}

impl Preconditioner for BorrowedPreconditioner {
    fn shape(&self) -> (usize, usize) {
        self.shape
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.retained()?.with(|p| p.apply(value))
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.retained()?.with(|p| p.apply_transpose(value))
    }
    fn release(&self) -> CaeResult<()> {
        let taken =
            self.retained.lock().map_err(|_| lease_error("borrowed preconditioner is released"))?.take();
        if taken.is_none() {
            return Ok(());
        }
        if let Some(tx) = &self.transaction {
            let mut state =
                tx.lock().map_err(|_| lease_error("preconditioner reuse borrow ownership drifted"))?;
            if state.borrow != Some(self.borrow_id) {
                return Err(lease_error("preconditioner reuse borrow ownership drifted"));
            }
            state.borrow = None;
        }
        Ok(())
    }
}

impl ProviderPreconditioner for BorrowedPreconditioner {
    fn matrix_identity(&self) -> ExactMatrixIdentity {
        self.matrix_identity.clone().unwrap_or(ExactMatrixIdentity {
            sha256: String::new(),
            size: self.shape.0,
            storage: "opaque",
            entries: 0,
        })
    }
    fn scope_digest(&self) -> String {
        self.scope_digest.clone()
    }
}

#[allow(clippy::struct_excessive_bools)]
struct TxState {
    candidate: Option<Arc<Retained>>,
    borrow: Option<u64>,
    prepared: bool,
    quality: Option<f64>,
    reused: bool,
    rebuild_attempted: bool,
    closed: bool,
    committed: bool,
}

struct StoreState {
    committed: Option<Arc<Retained>>,
    active: Option<u64>,
    released: bool,
}

#[derive(Clone)]
pub struct ExactPreconditionerReuseStore {
    profile: String,
    quality_limit: f64,
    state: Arc<Mutex<StoreState>>,
}

impl std::fmt::Debug for ExactPreconditionerReuseStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactPreconditionerReuseStore")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

static IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_id() -> u64 {
    IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl ExactPreconditionerReuseStore {


    pub fn new(profile_identity: &str, quality_limit: f64) -> CaeResult<Self> {
        let profile = checked_digest(profile_identity, "preconditioner reuse profile identity")?;
        if !quality_limit.is_finite() || quality_limit <= 0.0 || quality_limit >= 1.0 {
            return Err(lease_error("preconditioner reuse quality limit must lie in (0, 1)"));
        }
        Ok(Self {
            profile,
            quality_limit,
            state: Arc::new(Mutex::new(StoreState { committed: None, active: None, released: false })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn profile_identity(&self) -> &str {
        &self.profile
    }

    fn begin_inner(
        &self,
        identity: OperatorIdentity,
        pattern: Option<ExactSparsePatternIdentity>,
        shape: (usize, usize),
        opaque_scope: Option<String>,
    ) -> CaeResult<ExactPreconditionerReuseTransaction> {
        let mut state = self.lock();
        if state.released {
            return Err(lease_error("preconditioner reuse store is released"));
        }
        if state.active.is_some() {
            return Err(lease_error("preconditioner reuse store already has an active transaction"));
        }
        let mut candidate = state.committed.clone();
        let compatible = candidate.as_ref().is_some_and(|c| {
            c.profile == self.profile
                && c.shape == shape
                && match &opaque_scope {
                    Some(scope) => &c.scope_digest == scope,
                    None => c.pattern == pattern,
                }
        });
        if candidate.is_some() && !compatible {
            state.committed = None;
            if let Some(c) = candidate.take() {
                c.release()?;
            }
        }
        let id = next_id();
        state.active = Some(id);
        Ok(ExactPreconditionerReuseTransaction {
            store: self.clone(),
            id,
            identity,
            pattern,
            shape,
            opaque_scope,
            state: Arc::new(Mutex::new(TxState {
                candidate,
                borrow: None,
                prepared: false,
                quality: None,
                reused: false,
                rebuild_attempted: false,
                closed: false,
                committed: false,
            })),
        })
    }



    pub fn begin(&self, matrix: &AdmittedExactMatrix) -> CaeResult<ExactPreconditionerReuseTransaction> {
        {
            let state = self.lock();
            if state.released {
                return Err(lease_error("preconditioner reuse store is released"));
            }
            if state.active.is_some() {
                return Err(lease_error("preconditioner reuse store already has an active transaction"));
            }
        }
        let pattern = exact_sparse_pattern_identity(matrix)?;
        self.begin_inner(
            OperatorIdentity::Matrix(matrix.identity().clone()),
            Some(pattern),
            (matrix.size(), matrix.size()),
            None,
        )
    }



    pub fn begin_opaque(
        &self,
        operator_identity: &OperatorIdentity,
        shape: (usize, usize),
        scope_digest: &str,
    ) -> CaeResult<ExactPreconditionerReuseTransaction> {
        if shape.0 < 1 || shape.1 < 1 {
            return Err(lease_error(
                "opaque preconditioner reuse shape must be a nonempty two-dimensional integer shape",
            ));
        }
        if shape.0 != shape.1 || shape.0 != operator_identity.size() {
            return Err(lease_error("opaque preconditioner reuse identity and shape disagree"));
        }
        checked_digest(operator_identity.sha256(), "opaque preconditioner reuse identity digest")?;
        let scope = checked_digest(scope_digest, "opaque preconditioner reuse scope digest")?;
        self.begin_inner(operator_identity.clone(), None, shape, Some(scope))
    }

    #[must_use]
    pub fn has_committed(&self) -> bool {
        let state = self.lock();
        !state.released && state.active.is_none() && state.committed.as_ref().is_some_and(|c| !c.released())
    }



    pub fn borrow_lagged(
        &self,
        shape: (usize, usize),
        scope_digest: &str,
    ) -> CaeResult<Option<BorrowedPreconditioner>> {
        let retained = {
            let state = self.lock();
            if state.released {
                return Err(lease_error("preconditioner reuse store is released"));
            }
            if state.active.is_some() {
                return Err(lease_error("cannot borrow a lagged preconditioner during a transaction"));
            }
            state.committed.clone()
        };
        let Some(retained) = retained else { return Ok(None) };
        if retained.shape != shape {
            self.discard_committed()?;
            return Ok(None);
        }
        if retained.scope_digest != scope_digest {
            return Err(lease_error("lagged preconditioner operation scope drifted"));
        }
        if retained.released() {
            return Err(lease_error("retained preconditioner is released"));
        }
        Ok(Some(BorrowedPreconditioner {
            matrix_identity: None,
            scope_digest: retained.scope_digest.clone(),
            shape: retained.shape,
            retained: Mutex::new(Some(retained)),
            transaction: None,
            borrow_id: 0,
        }))
    }



    pub fn discard_committed(&self) -> CaeResult<()> {
        let retained = {
            let mut state = self.lock();
            if state.active.is_some() {
                return Err(lease_error("cannot discard a committed preconditioner during a transaction"));
            }
            state.committed.take()
        };
        match retained {
            Some(r) => r.release(),
            None => Ok(()),
        }
    }

    fn finish(&self, id: u64, candidate: Option<&Arc<Retained>>, commit: bool) -> CaeResult<()> {
        let prior = {
            let mut state = self.lock();
            if state.active != Some(id) {
                return Err(lease_error("preconditioner reuse transaction ownership drifted"));
            }
            state.active = None;
            if commit {
                let prior = state.committed.take();
                state.committed = candidate.cloned();
                prior.filter(|p| !candidate.is_some_and(|c| Arc::ptr_eq(p, c)))
            } else {
                None
            }
        };
        match prior {
            Some(p) => p.release(),
            None => Ok(()),
        }
    }

    fn committed_is(&self, candidate: &Arc<Retained>) -> bool {
        self.lock().committed.as_ref().is_some_and(|c| Arc::ptr_eq(c, candidate))
    }



    pub fn release(&self) -> CaeResult<()> {
        let committed = {
            let mut state = self.lock();
            if state.released {
                return Ok(());
            }
            state.active = None;
            state.released = true;
            state.committed.take()
        };
        match committed {
            Some(c) => c.release(),
            None => Ok(()),
        }
    }
}

pub struct ExactPreconditionerReuseTransaction {
    store: ExactPreconditionerReuseStore,
    id: u64,
    identity: OperatorIdentity,
    pattern: Option<ExactSparsePatternIdentity>,
    shape: (usize, usize),
    opaque_scope: Option<String>,
    state: Arc<Mutex<TxState>>,
}

impl std::fmt::Debug for ExactPreconditionerReuseTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactPreconditionerReuseTransaction")
            .field("closed", &self.closed())
            .finish_non_exhaustive()
    }
}

impl ExactPreconditionerReuseTransaction {
    fn lock(&self) -> MutexGuard<'_, TxState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn require_open(&self) -> CaeResult<MutexGuard<'_, TxState>> {
        let state = self.lock();
        if state.closed {
            return Err(lease_error("preconditioner reuse transaction is closed"));
        }
        Ok(state)
    }

    #[must_use]
    pub fn matrix_identity(&self) -> &OperatorIdentity {
        &self.identity
    }
    #[must_use]
    pub fn shape(&self) -> (usize, usize) {
        self.shape
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
    pub fn reused(&self) -> bool {
        self.lock().reused
    }
    #[must_use]
    pub fn rebuild_attempted(&self) -> bool {
        self.lock().rebuild_attempted
    }
    #[must_use]
    pub fn quality(&self) -> Option<f64> {
        self.lock().quality
    }

    fn borrow(
        &self,
        state: &mut TxState,
        candidate: Arc<Retained>,
        identity: Option<ExactMatrixIdentity>,
        scope: String,
    ) -> BorrowedPreconditioner {
        let id = next_id();
        state.borrow = Some(id);
        state.prepared = true;
        BorrowedPreconditioner {
            matrix_identity: identity,
            scope_digest: scope,
            shape: self.shape,
            retained: Mutex::new(Some(candidate)),
            transaction: Some(Arc::clone(&self.state)),
            borrow_id: id,
        }
    }



    pub fn prepare(
        &self,
        lease: &SealedMatrixReadLease,
        binding: &ExactPreconditionerBinding,
        factory: &mut LeaseFactory<'_>,
    ) -> CaeResult<Box<dyn ProviderPreconditioner>> {
        let mut state = self.require_open()?;
        if state.prepared || state.borrow.is_some() {
            return Err(lease_error("preconditioner reuse transaction is already prepared"));
        }
        let OperatorIdentity::Matrix(identity) = &self.identity else {
            return Err(lease_error("preconditioner reuse binding disagrees with the current matrix"));
        };
        if &binding.matrix_identity != identity
            || binding.shape != self.shape
            || lease.identity()? != identity
            || lease.shape()? != self.shape
        {
            return Err(lease_error("preconditioner reuse binding disagrees with the current matrix"));
        }
        if let Some(candidate) = state.candidate.clone() {
            if candidate.scope_digest != binding.scope_digest {
                return Err(lease_error("preconditioner reuse operation scope drifted"));
            }
            let quality = reuse_quality(lease, &candidate)?;
            state.quality = Some(quality);
            if quality <= self.store.quality_limit {
                state.reused = true;
            } else {
                {
                    let mut store = self.store.lock();
                    if store.committed.as_ref().is_some_and(|c| Arc::ptr_eq(c, &candidate)) {
                        store.committed = None;
                    }
                }
                candidate.release()?;
                state.candidate = None;
            }
        }
        let candidate = if let Some(c) = state.candidate.clone() {
            c
        } else {
            let provider = factory(lease, binding)?;
            if let Err(e) = provider_metadata(provider.as_ref(), binding) {
                return Err(release_failed_preparation(provider.as_ref(), e));
            }
            let boxed: Box<dyn Preconditioner> = Box::new(ProviderAsPreconditioner(provider));
            let c = Arc::new(Retained {
                provider: Mutex::new(Some(boxed)),
                source_identity: self.identity.clone(),
                pattern: self.pattern.clone(),
                profile: self.store.profile.clone(),
                scope_digest: binding.scope_digest.clone(),
                shape: binding.shape,
            });
            state.candidate = Some(Arc::clone(&c));
            state.reused = false;
            c
        };
        let borrow = self.borrow(&mut state, candidate, Some(identity.clone()), binding.scope_digest.clone());
        Ok(Box::new(borrow))
    }



    pub fn prepare_opaque(
        &self,
        factory: &mut dyn FnMut() -> CaeResult<Option<Box<dyn Preconditioner>>>,
    ) -> CaeResult<Box<dyn Preconditioner>> {
        let mut state = self.require_open()?;
        let Some(scope) = self.opaque_scope.clone() else {
            return Err(lease_error("assembled preconditioner transaction cannot prepare an opaque action"));
        };
        if state.prepared || state.borrow.is_some() {
            return Err(lease_error("preconditioner reuse transaction is already prepared"));
        }
        let candidate = if let Some(c) = state.candidate.clone() {
            if c.scope_digest != scope {
                return Err(lease_error("opaque preconditioner reuse operation scope drifted"));
            }
            state.reused = true;
            c
        } else {
            let provider = factory()
                .map_err(|e| {
                    lease_error(format!("opaque preconditioner preparation failed: {}", e.message()))
                })?
                .ok_or_else(|| {
                    lease_error("opaque prepared preconditioner is incomplete or has the wrong shape")
                })?;
            if let Err(e) = validate_opaque(provider.as_ref(), self.shape) {
                return Err(release_failed_preparation(provider.as_ref(), e));
            }
            let c = Arc::new(Retained {
                provider: Mutex::new(Some(provider)),
                source_identity: self.identity.clone(),
                pattern: None,
                profile: self.store.profile.clone(),
                scope_digest: scope.clone(),
                shape: self.shape,
            });
            state.candidate = Some(Arc::clone(&c));
            state.reused = false;
            c
        };
        let borrow = self.borrow(&mut state, candidate, None, scope);
        Ok(Box::new(borrow))
    }



    pub fn force_fresh_rebuild(&self) -> CaeResult<()> {
        let mut state = self.require_open()?;
        if !state.reused || state.rebuild_attempted || state.borrow.is_some() {
            return Err(lease_error("fresh preconditioner rebuild is not eligible"));
        }
        let candidate = state.candidate.take();
        state.prepared = false;
        state.reused = false;
        state.rebuild_attempted = true;
        state.quality = None;
        drop(state);
        if let Some(c) = candidate {
            {
                let mut store = self.store.lock();
                if store.committed.as_ref().is_some_and(|x| Arc::ptr_eq(x, &c)) {
                    store.committed = None;
                }
            }
            c.release()?;
        }
        Ok(())
    }



    pub fn commit(&self) -> CaeResult<()> {
        let mut state = self.require_open()?;
        if !state.prepared || state.candidate.is_none() || state.borrow.is_some() {
            return Err(lease_error("preconditioner reuse transaction cannot commit an incomplete use"));
        }
        self.store.finish(self.id, state.candidate.as_ref(), true)?;
        state.committed = true;
        state.closed = true;
        Ok(())
    }



    pub fn rollback(&self) -> CaeResult<()> {
        let mut state = self.require_open()?;
        if state.borrow.is_some() {
            return Err(lease_error("preconditioner reuse transaction still has an active borrow"));
        }
        let candidate = state.candidate.take();
        state.closed = true;
        drop(state);
        if let Some(c) = &candidate
            && !self.store.committed_is(c)
        {
            c.release()?;
        }
        self.store.finish(self.id, None, false)
    }



    pub fn abort_store(&self) -> CaeResult<()> {
        let mut state = self.require_open()?;
        if state.borrow.is_some() {
            return Err(lease_error("preconditioner reuse transaction still has an active borrow"));
        }
        let candidate = state.candidate.take();
        state.closed = true;
        drop(state);
        let committed = self.store.lock().committed.take();
        let mut result = Ok(());
        if let Some(c) = &candidate {
            result = c.release();
        }
        if let Some(c) = committed
            && !candidate.as_ref().is_some_and(|x| Arc::ptr_eq(x, &c))
        {
            let r = c.release();
            if result.is_ok() {
                result = r;
            }
        }
        self.store.finish(self.id, None, false)?;
        result
    }

    #[must_use]
    pub fn source_identity(&self) -> Option<OperatorIdentity> {
        self.lock().candidate.as_ref().map(|c| c.source_identity.clone())
    }
}

impl Drop for ExactPreconditionerReuseTransaction {
    fn drop(&mut self) {
        let open = !self.closed();
        if open {
            self.state.lock().map(|mut s| s.borrow = None).ok();
            let _ = self.rollback();
        }
    }
}

struct ProviderAsPreconditioner(Box<dyn ProviderPreconditioner>);

impl Preconditioner for ProviderAsPreconditioner {
    fn shape(&self) -> (usize, usize) {
        self.0.shape()
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.0.apply(value)
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.0.apply_transpose(value)
    }
    fn release(&self) -> CaeResult<()> {
        self.0.release()
    }
}

#[derive(Debug)]
pub struct JacobiPreconditioner {
    inverse_diagonal: Vec<f64>,
    identity: ExactMatrixIdentity,
    scope: String,
}

impl JacobiPreconditioner {


    pub fn from_lease(
        lease: &SealedMatrixReadLease,
        binding: &ExactPreconditionerBinding,
    ) -> CaeResult<Self> {
        let d = lease.diagonal_copy()?;
        if d.contains(&0.0) {
            return Err(lease_error("Jacobi preconditioner requires a nonzero diagonal"));
        }
        Ok(Self {
            inverse_diagonal: d.iter().map(|v| 1.0 / v).collect(),
            identity: binding.matrix_identity.clone(),
            scope: binding.scope_digest.clone(),
        })
    }
}

impl Preconditioner for JacobiPreconditioner {
    fn shape(&self) -> (usize, usize) {
        (self.inverse_diagonal.len(), self.inverse_diagonal.len())
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(value.iter().zip(&self.inverse_diagonal).map(|(v, d)| v * d).collect())
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.apply(value)
    }
    fn release(&self) -> CaeResult<()> {
        Ok(())
    }
}

impl ProviderPreconditioner for JacobiPreconditioner {
    fn matrix_identity(&self) -> ExactMatrixIdentity {
        self.identity.clone()
    }
    fn scope_digest(&self) -> String {
        self.scope.clone()
    }
}



pub fn admit_csr(matrix: CsrMatrix) -> CaeResult<AdmittedExactMatrix> {
    admit_exact_matrix(crate::matrix::Jacobian::Csr(matrix), None)
}

