// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use rayon::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix, cond};
use implexity_linalg::error::LinalgError;
use implexity_linalg::lu::{LuSymbolic, Parallelism, SparseLu};
use implexity_linalg::onenormest::onenormest;
use implexity_linalg::operator::{FnOperator, LinearOperator};
use crate::local_condensation::{LocalCondensation, LocalEliminationPartition};
use implexity_linalg::sparse::CscMatrix;
use serde_json::json;

struct InverseAction<'a> {
    factors: &'a Factors,
    size: usize,
    transpose: bool,
    counts: &'a std::cell::Cell<(usize, usize)>,
}

impl InverseAction<'_> {
    fn count(&self, columns: usize) {
        let (f, t) = self.counts.get();
        self.counts.set(if self.transpose { (f, t + columns) } else { (f + columns, t) });
    }
}

impl LinearOperator for InverseAction<'_> {
    fn n(&self) -> usize {
        self.size
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        if x.len() != self.size || y.len() != x.len() {
            return Err(LinalgError::Shape("inverse action operand has another length".into()));
        }
        self.count(1);
        y.copy_from_slice(&raw_factor_solve(self.factors, self.size, x, 1, self.transpose)?);
        Ok(())
    }
    fn apply_block(&self, xs: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, LinalgError> {
        self.count(xs.len());
        let mut rhs = vec![0.0; self.size * xs.len()];
        for (column, x) in xs.iter().enumerate() {
            if x.len() != self.size { return Err(LinalgError::Shape("inverse action block has another length".into())); }
            for (row, value) in x.iter().enumerate() { rhs[row * xs.len() + column] = *value; }
        }
        let solved = raw_factor_solve(self.factors, self.size, &rhs, xs.len(), self.transpose)?;
        Ok((0..xs.len()).map(|column| (0..self.size).map(|row| solved[row * xs.len() + column]).collect()).collect())
    }
}

use crate::certificate::{residual_certificate_columns, stable_l2};
use crate::exact_matrix::{AdmittedExactMatrix, CanonicalMatrix, MatrixInput, admit_exact_matrix};
use crate::pyfmt::fmt_e;
use crate::trace::{self, Fields};

pub const SPARSE_KIND: &str = "sparse_lu_condition_1norm_estimate";
pub const DENSE_KIND: &str = "dense_lu_condition_2norm";

pub const SYMBOLIC_CACHE_BYTES: usize = 256 * 1024 * 1024;

struct SymbolicCache {
    entries: VecDeque<(Arc<LuSymbolic>, usize)>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

fn symbolic_cache() -> &'static Mutex<SymbolicCache> {
    static CACHE: OnceLock<Mutex<SymbolicCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SymbolicCache { entries: VecDeque::new(), bytes: 0, hits: 0, misses: 0 }))
}



pub fn symbolic_for(a: &CscMatrix) -> Result<Arc<LuSymbolic>, LinalgError> {
    {
        let mut cache = symbolic_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pos) = cache.entries.iter().position(|(s, _)| s.matches(a)) {
            cache.hits += 1;
            let entry = cache.entries.remove(pos);
            if let Some(entry) = entry {
                let sym = Arc::clone(&entry.0);
                cache.entries.push_back(entry);
                return Ok(sym);
            }
        }
        cache.misses += 1;
    }
    let sym = Arc::new(LuSymbolic::analyze(a)?);

    let bytes = sym.nbytes();
    let mut cache = symbolic_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if bytes <= SYMBOLIC_CACHE_BYTES {
        cache.entries.push_back((Arc::clone(&sym), bytes));
        cache.bytes += bytes;
        while cache.bytes > SYMBOLIC_CACHE_BYTES {
            match cache.entries.pop_front() {
                Some((_, b)) => cache.bytes -= b,
                None => break,
            }
        }
    }
    Ok(sym)
}

#[must_use]
pub fn symbolic_cache_counters() -> (u64, u64) {
    let cache = symbolic_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (cache.hits, cache.misses)
}

enum Factors {
    Sparse(Box<SparseLu>),
    Dense(DenseLu),
    Condensed(Box<LocalCondensation>),
}

fn raw_factor_solve(factors: &Factors, n: usize, b: &[f64], m: usize, transpose: bool) -> Result<Vec<f64>, LinalgError> {
    match factors {
        Factors::Dense(lu) => lu.solve(b, m, transpose),
        Factors::Condensed(lu) => lu.solve(b, m, transpose),
        Factors::Sparse(lu) => {
            let mut col = vec![0.0; n * m];
            for i in 0..n { for j in 0..m { col[j * n + i] = b[i * m + j]; } }
            lu.solve_many_in_place(&mut col, m, transpose)?;
            let mut out = vec![0.0; n * m];
            for i in 0..n { for j in 0..m { out[i * m + j] = col[j * n + i]; } }
            Ok(out)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CertifiedSolve {
    pub solution: Vec<f64>,
    pub error_norms: Vec<f64>,
    pub relative: Vec<f64>,
}

type Refined = (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>);

pub struct Factorization {
    matrix: AdmittedExactMatrix,
    factors: Factors,
    condition: f64,
    kind: &'static str,
    retained_bytes: usize,
    trace: Fields,
}

impl std::fmt::Debug for Factorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Factorization")
            .field("size", &self.matrix.size())
            .field("kind", &self.kind)
            .field("condition", &self.condition)
            .finish_non_exhaustive()
    }
}

fn backend(e: &LinalgError) -> CaeError {
    match e {
        LinalgError::Singular(_) | LinalgError::NonFinite(_) | LinalgError::NoConvergence(_) => {
            CaeError::convergence(e.to_string())
        }
        _ => CaeError::contract(e.to_string()),
    }
}

fn prefixed(prefix: &str, e: &CaeError) -> CaeError {
    let message = format!("{prefix}: {}", e.message());
    if e.is_convergence() { CaeError::convergence(message) } else { CaeError::contract(message) }
}

impl Factorization {


    pub fn new(
        matrix: impl Into<MatrixInput>,
        size: usize,
        condition_limit: f64,
        trace_fields: Option<&Fields>,
    ) -> CaeResult<Self> {
        Self::build(matrix.into(), size, Some(condition_limit), trace_fields, None)
    }



    pub fn for_correction(
        matrix: impl Into<MatrixInput>,
        size: usize,
        trace_fields: Option<&Fields>,
    ) -> CaeResult<Self> {
        Self::build(matrix.into(), size, None, trace_fields, None)
    }

    pub fn new_with_partition(matrix: impl Into<MatrixInput>, size: usize, condition_limit: f64, trace_fields: Option<&Fields>, partition: Option<&LocalEliminationPartition>) -> CaeResult<Self> {
        Self::build(matrix.into(), size, Some(condition_limit), trace_fields, partition)
    }

    pub fn correction_with_partition(matrix: impl Into<MatrixInput>, size: usize, trace_fields: Option<&Fields>, partition: Option<&LocalEliminationPartition>) -> CaeResult<Self> {
        Self::build(matrix.into(), size, None, trace_fields, partition)
    }

    fn build(
        input: MatrixInput,
        size: usize,
        condition_limit: Option<f64>,
        trace_fields: Option<&Fields>,
        partition: Option<&LocalEliminationPartition>,
    ) -> CaeResult<Self> {
        if partition.is_some_and(|p| p.size() != size) { return Err(CaeError::contract("local elimination partition has another state size")); }
        let trace_fields = trace_fields.cloned().unwrap_or_default();
        let with = |extra: Fields| {
            let mut f = extra;
            f.extend(trace_fields.clone());
            f
        };
        let estimate = condition_limit.is_some();
        let admitted = trace::span(
            "linear_matrix_validation",
            || with(crate::trace_fields! {"size" => size}),
            || match input {
                MatrixInput::Admitted(a) => admit_exact_matrix(a, Some(size)),
                MatrixInput::Jacobian(j) => {
                    let j = crate::matrix::checked_matrix(j, (size, size), "state Jacobian", false)?;
                    admit_exact_matrix(j, Some(size))
                }
            },
        )?;
        let factored = match admitted.matrix() {
            CanonicalMatrix::Sparse(a) => Self::factor_sparse(a, size, estimate, &trace_fields, partition),
            CanonicalMatrix::Dense(a) => Self::factor_dense(a, size, estimate, &trace_fields),
        };
        let (factors, condition, kind, retained) = factored.map_err(|e| {
            if e.message().starts_with("implicit Jacobian factorization failed") {
                e
            } else {
                prefixed("implicit Jacobian factorization failed", &e)
            }
        })?;

        if let Some(limit) = condition_limit
            && (!condition.is_finite() || condition > limit)
        {
            return Err(CaeError::convergence(format!(
                "implicit state Jacobian is singular/ill-conditioned (cond={})",
                fmt_e(condition, 3)
            )));
        }
        let nnz = admitted.matrix().entries();
        trace::point("linear_factorization_ready", || {
            let estimate = if condition.is_finite() { json!(condition) } else { serde_json::Value::Null };
            with(crate::trace_fields! {
                "size" => size, "nnz" => nnz, "factorization" => kind,
                "condition_estimate" => estimate, "condition_limit" => condition_limit,
            })
        })?;
        Ok(Self { matrix: admitted, factors, condition, kind, retained_bytes: retained, trace: trace_fields })
    }

    fn factor_sparse(
        a: &CscMatrix,
        size: usize,
        estimate: bool,
        tf: &Fields,
        partition: Option<&LocalEliminationPartition>,
    ) -> CaeResult<(Factors, f64, &'static str, usize)> {
        let with = |extra: Fields| {
            let mut f = extra;
            f.extend(tf.clone());
            f
        };
        let nnz = a.nnz();
        let condensed = if let Some(partition) = partition {
            match trace::span("linear_local_elimination_factorization", || with(crate::trace_fields! {"size" => size, "eliminated_size" => partition.eliminated_size()}), || LocalCondensation::new(a, partition, symbolic_for).map_err(|e| backend(&e))) {
                Ok(factor) => Some(factor),
                Err(e) => {
                    trace::point("local_elimination_fallback", || with(crate::trace_fields! {"size" => size, "reason" => e.message(), "fallback" => "full_sparse_lu"}))?;
                    None
                }
            }
        } else { None };
        let (factors, kind, retained) = if let Some(factor) = condensed {
            let retained = factor.nbytes() + a.nbytes();
            trace::point("local_elimination_ready", || with(crate::trace_fields! {"size" => size, "retained_size" => factor.retained_size(), "condition_scope" => "full_original_matrix"}))?;
            (Factors::Condensed(Box::new(factor)), "exact_local_elimination_full_condition_1norm_estimate", retained)
        } else {
            let symbolic = trace::span("linear_fill_reducing_ordering", || with(crate::trace_fields! {"size" => size}), || symbolic_for(a).map_err(|e| backend(&e)))?;
            let ordering = symbolic.ordering();
            let lu = trace::span("linear_sparse_factorization", || with(crate::trace_fields! {"size" => size, "nnz" => nnz, "ordering" => ordering}), || symbolic.factor(a, Parallelism::Sequential).map_err(|e| backend(&e)))?;
            let retained = lu.nbytes() + a.nbytes();
            (Factors::Sparse(Box::new(lu)), SPARSE_KIND, retained)
        };
        if !estimate { return Ok((factors, f64::NAN, kind, retained)); }
        let counts = std::cell::Cell::new((0usize, 0usize));
        let condition = trace::lifecycle(
            "linear_condition_estimate",
            || with(crate::trace_fields! {"size" => size, "nnz" => nnz}),
            || {
                let op = FnOperator::new(size, |x: &[f64], y: &mut [f64]| {
                    y.copy_from_slice(&a.matvec(x)?);
                    Ok(())
                });
                let op_t = FnOperator::new(size, |x: &[f64], y: &mut [f64]| {
                    y.copy_from_slice(&a.matvec_transpose(x)?);
                    Ok(())
                });
                let inv = InverseAction { factors: &factors, size, transpose: false, counts: &counts };
                let inv_t = InverseAction { factors: &factors, size, transpose: true, counts: &counts };
                let na = onenormest(&op, &op_t).map_err(|e| backend(&e))?;
                let ni = onenormest(&inv, &inv_t).map_err(|e| backend(&e))?;
                Ok::<f64, CaeError>(na.estimate * ni.estimate)
            },
            |value| {
                let (f, t) = counts.get();
                crate::trace_fields! {
                    "condition_estimate" => value,
                    "forward_inverse_action_count" => f,
                    "transpose_inverse_action_count" => t,
                    "total_inverse_action_count" => f + t,
                }
            },
        )?;
        Ok((factors, condition, kind, retained))
    }

    fn factor_dense(
        a: &DenseMatrix,
        size: usize,
        estimate: bool,
        tf: &Fields,
    ) -> CaeResult<(Factors, f64, &'static str, usize)> {
        let with = |extra: Fields| {
            let mut f = extra;
            f.extend(tf.clone());
            f
        };
        let condition = if estimate {
            trace::span(
                "linear_condition_estimate",
                || with(crate::trace_fields! {"size" => size}),
                || cond(a).map_err(|e| backend(&e).context(&format!("dense condition estimation state_size={size}"))),
            )?
        } else {
            f64::NAN
        };
        let lu = trace::span(
            "linear_dense_factorization",
            || with(crate::trace_fields! {"size" => size}),
            || DenseLu::new(a).map_err(|e| backend(&e)),
        )?;
        let retained = a.data.len() * 8 * 2 + size * 4;
        Ok((Factors::Dense(lu), condition, DENSE_KIND, retained))
    }

    #[must_use]
    pub fn matrix(&self) -> &AdmittedExactMatrix {
        &self.matrix
    }
    #[must_use]
    pub fn condition(&self) -> f64 {
        self.condition
    }
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    #[must_use]
    pub fn size(&self) -> usize {
        self.matrix.size()
    }

    fn raw_solve(&self, b: &[f64], m: usize, transpose: bool) -> CaeResult<Vec<f64>> {
        raw_factor_solve(&self.factors, self.size(), b, m, transpose).map_err(|e| backend(&e))
    }

    fn residual(&self, x: &[f64], b: &[f64], m: usize, transpose: bool) -> CaeResult<Vec<f64>> {
        let n = self.size();
        if m == 1 {
            let mut error = self.matrix.matrix().apply(x, transpose)?;
            for (value, rhs) in error.iter_mut().zip(b) { *value -= rhs; }
            return Ok(error);
        }
        if transpose && m >= 4 && let CanonicalMatrix::Sparse(matrix) = self.matrix.matrix() {
            let mut error = vec![0.0; n * m];
            error.par_chunks_mut(m).enumerate().for_each(|(i, output)| {
                let (rows, values) = matrix.col(i);
                for (&row, &value) in rows.iter().zip(values) {
                    let input = &x[row * m..(row + 1) * m];
                    for j in 0..m { output[j] += value * input[j]; }
                }
            });
            for (value, rhs) in error.iter_mut().zip(b) { *value -= rhs; }
            return Ok(error);
        }
        let mut error = vec![0.0; n * m];
        let mut col = vec![0.0; n];
        for j in 0..m {
            for i in 0..n {
                col[i] = x[i * m + j];
            }
            let y = self.matrix.matrix().apply(&col, transpose)?;
            for i in 0..n {
                error[i * m + j] = y[i] - b[i * m + j];
            }
        }
        Ok(error)
    }



    pub fn solve(&self, b: &[f64], transpose: bool) -> CaeResult<(Vec<f64>, f64, f64)> {
        let r = self.solve_block(b, 1, transpose)?;
        Ok((r.solution, r.error_norms[0], r.relative[0]))
    }



    #[allow(clippy::too_many_lines)]
    pub fn solve_block(&self, b: &[f64], m: usize, transpose: bool) -> CaeResult<CertifiedSolve> {
        let n = self.size();
        if m == 0 || b.len() != n * m {
            return Err(CaeError::contract("linear right-hand side has invalid shape or nonfinite values"));
        }
        if b.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence(
                "linear right-hand side has invalid shape or nonfinite values",
            ));
        }
        let t = if transpose { "transpose " } else { "" };
        let base = |extra: Fields| {
            let mut f = extra;
            f.extend(self.trace.clone());
            f
        };
        let mut x = trace::span(
            "linear_solve",
            || base(crate::trace_fields! {"size" => n, "right_hand_sides" => m, "transpose" => transpose}),
            || self.raw_solve(b, m, transpose),
        )
        .map_err(|e| prefixed(&format!("linear {t}solve failed"), &e))?;
        if x.len() != n * m {
            return Err(CaeError::contract(format!("linear {t}solve returned invalid shape or values")));
        }
        if x.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence(format!("linear {t}solve returned invalid shape or values")));
        }
        let mut error = trace::span(
            "linear_residual_certification",
            || base(crate::trace_fields! {"size" => n, "right_hand_sides" => m, "transpose" => transpose}),
            || self.residual(&x, b, m, transpose),
        )
        .map_err(|e| prefixed(&format!("linear {t}solve residual check failed"), &e))?;
        let certify = |error: &[f64]| -> CaeResult<(Vec<f64>, Vec<f64>)> {
            if m == 1 {
                let e = stable_l2(error).map_err(|s| {
                    CaeError::convergence(format!("linear solve residual certification failed: {s}"))
                })?;
                let bn = stable_l2(b).map_err(|s| {
                    CaeError::convergence(format!("linear solve residual certification failed: {s}"))
                })?;
                let rel = e / 1.0_f64.max(bn);
                if !rel.is_finite() {
                    return Err(CaeError::convergence(
                        "linear solve residual certification failed: residual certification is nonfinite",
                    ));
                }
                Ok((vec![e], vec![rel]))
            } else {
                residual_certificate_columns(error, b, n, m).map_err(|s| {
                    CaeError::convergence(format!("linear solve residual certification failed: {s}"))
                })
            }
        };
        let (mut error_norm, mut relative) = certify(&error)?;
        for refinement in 0..3 {
            if !relative.iter().any(|r| *r > 1e-8) {
                break;
            }
            let neg: Vec<f64> = error.iter().map(|v| -v).collect();
            let step = (|| -> CaeResult<Refined> {
                let correction = self.raw_solve(&neg, m, transpose)?;
                if correction.len() != n * m {
                    return Err(CaeError::contract("invalid residual correction shape or values"));
                }
                if correction.iter().any(|v| !v.is_finite()) {
                    return Err(CaeError::convergence("invalid residual correction shape or values"));
                }
                let candidate: Vec<f64> = x.iter().zip(&correction).map(|(a, c)| a + c).collect();
                if candidate.iter().any(|v| !v.is_finite()) {
                    return Err(CaeError::convergence("nonfinite corrected solution"));
                }
                let err = self.residual(&candidate, b, m, transpose)?;
                let (en, rel) = certify(&err)?;
                Ok((candidate, err, en, rel))
            })()
            .map_err(|e| prefixed("linear residual correction failed", &e))?;
            let (candidate, err, en, rel) = step;
            error = err;
            error_norm = en;
            relative = rel;
            let max_rel = relative.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            trace::point("linear_residual_correction", || {
                base(crate::trace_fields! {
                    "refinement" => refinement + 1, "transpose" => transpose,
                    "right_hand_sides" => m, "maximum_relative_residual" => max_rel,
                })
            })?;
            let unchanged = candidate.iter().zip(&x).all(|(a, b)| a.to_bits() == b.to_bits());
            x = candidate;
            if unchanged {
                break;
            }
        }

        if x.iter().any(|v| !v.is_finite()) || relative.iter().any(|r| *r > 1e-8) {
            return Err(CaeError::convergence(format!("linear {t}solve failed residual check")));
        }
        Ok(CertifiedSolve { solution: x, error_norms: error_norm, relative })
    }

    #[must_use]
    pub fn describe(&self) -> serde_json::Value {
        let condition =
            if self.condition.is_finite() { json!(self.condition) } else { serde_json::Value::Null };
        json!({"kind": self.kind, "condition": condition, "size": self.size(), "retained_bytes": self.retained_bytes})
    }
}

