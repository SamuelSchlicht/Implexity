// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use faer::dyn_stack::{MemBuffer, MemStack};
use faer::sparse::linalg::LuError;
use faer::sparse::linalg::cholesky::{self as sp_chol, SymbolicCholesky};
use faer::sparse::linalg::lu::{LuRef, LuSymbolicParams, NumericLu, SymbolicLu, factorize_symbolic_lu};
use faer::{Conj, MatMut, Par, Side};

use crate::error::LinalgError;
use crate::multifrontal::{MultifrontalLu, MultifrontalSymbolic, SymmetricOrder};
pub use crate::ordering::NESTED_DISSECTION_MINIMUM_SIZE;
use crate::ordering::SymmetricPattern;
use crate::sparse::CscMatrix;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Parallelism {
    #[default]
    Sequential,
    Rayon,
}

impl Parallelism {
    fn par(self) -> Par {
        match self {
            Self::Sequential => Par::Seq,
            Self::Rayon => Par::rayon(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LuOrdering {
    #[default]
    Automatic,
    Colamd,
    NestedDissection,
    MinimumDegree,
}

fn map_lu_error(e: LuError) -> LinalgError {
    match e {
        LuError::SymbolicSingular { index } => {
            LinalgError::Singular(format!("structurally singular matrix (no pivot at step {index})"))
        }
        LuError::Generic(f) => f.into(),
    }
}

fn buffer(req: faer::dyn_stack::StackReq) -> Result<MemBuffer, LinalgError> {
    MemBuffer::try_new(req).map_err(|_| LinalgError::OutOfMemory("factorization workspace".into()))
}

#[derive(Clone, Debug)]
enum SymbolicBackend {
    Faer { inner: Arc<SymbolicLu<usize>>, storage_bound: usize },
    Multifrontal(Arc<MultifrontalSymbolic>),
}

#[derive(Clone, Debug)]
pub struct LuSymbolic {
    backend: SymbolicBackend,
    n: usize,
    pattern: AnalysedPattern,
}

#[derive(Clone, Debug)]
enum AnalysedPattern {
    Exact { col_ptr: Arc<Vec<usize>>, row_idx: Arc<Vec<usize>> },
    Symmetric(Arc<SymmetricPattern>),
}

impl LuSymbolic {


    pub fn analyze(a: &CscMatrix) -> Result<Self, LinalgError> {
        Self::analyze_with(a, LuOrdering::Automatic)
    }



    pub fn analyze_with(a: &CscMatrix, ordering: LuOrdering) -> Result<Self, LinalgError> {
        let (m, n) = a.shape();
        if m != n {
            return Err(LinalgError::Shape(format!("LU of a non-square {m}×{n} matrix")));
        }
        let ordering = match ordering {
            LuOrdering::Automatic if n >= NESTED_DISSECTION_MINIMUM_SIZE => LuOrdering::NestedDissection,
            LuOrdering::Automatic => LuOrdering::Colamd,
            other => other,
        };
        let (backend, pattern) = match ordering {
            LuOrdering::NestedDissection | LuOrdering::MinimumDegree => {
                let order = if ordering == LuOrdering::NestedDissection {
                    SymmetricOrder::NestedDissection
                } else {
                    SymmetricOrder::MinimumDegree
                };
                let symmetric = SymmetricPattern::of(a)?;
                let analysis = MultifrontalSymbolic::analyze_pattern(&symmetric, order)?;
                (
                    SymbolicBackend::Multifrontal(Arc::new(analysis)),
                    AnalysedPattern::Symmetric(Arc::new(symmetric)),
                )
            }
            LuOrdering::Automatic | LuOrdering::Colamd => {
                let sym = a.as_faer().symbolic();
                let inner = factorize_symbolic_lu(sym, LuSymbolicParams::default())?;

                let qr = faer::sparse::linalg::qr::factorize_symbolic_qr(
                    sym,
                    faer::sparse::linalg::qr::QrSymbolicParams::default(),
                )?;
                let storage_bound = qr.len_val() * size_of::<f64>() + qr.len_idx() * size_of::<usize>();
                (
                    SymbolicBackend::Faer { inner: Arc::new(inner), storage_bound },
                    AnalysedPattern::Exact {
                        col_ptr: Arc::new(a.indptr().to_vec()),
                        row_idx: Arc::new(a.indices().to_vec()),
                    },
                )
            }
        };
        Ok(Self { backend, n, pattern })
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.n
    }

    #[must_use]
    pub fn ordering(&self) -> &'static str {
        match &self.backend {
            SymbolicBackend::Faer { .. } => "COLAMD",
            SymbolicBackend::Multifrontal(s) => s.order().name(),
        }
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        let pattern = match &self.pattern {
            AnalysedPattern::Exact { col_ptr, row_idx } => col_ptr.len() + row_idx.len(),
            AnalysedPattern::Symmetric(p) => p.ptr().len() + p.idx().len(),
        } * size_of::<usize>();
        pattern
            + match &self.backend {
                SymbolicBackend::Faer { .. } => 2 * self.n * size_of::<usize>(),
                SymbolicBackend::Multifrontal(s) => s.nbytes(),
            }
    }

    #[must_use]
    pub fn matches(&self, a: &CscMatrix) -> bool {
        a.shape() == (self.n, self.n)
            && match &self.pattern {
                AnalysedPattern::Exact { col_ptr, row_idx } => {
                    a.indptr() == col_ptr.as_slice() && a.indices() == row_idx.as_slice()
                }
                AnalysedPattern::Symmetric(p) => p.is_pattern_of(a),
            }
    }

    #[must_use]
    pub fn storage_bound_bytes(&self) -> usize {
        match &self.backend {
            SymbolicBackend::Faer { storage_bound, .. } => *storage_bound,
            SymbolicBackend::Multifrontal(s) => s.predicted_entries() * size_of::<f64>(),
        }
    }



    pub fn factor(&self, a: &CscMatrix, parallelism: Parallelism) -> Result<SparseLu, LinalgError> {
        if !self.matches(a) {
            return Err(LinalgError::Shape("matrix pattern differs from the symbolic analysis".into()));
        }
        if !a.is_finite() {
            return Err(LinalgError::NonFinite("matrix entries must be finite".into()));
        }
        let numeric = match &self.backend {
            SymbolicBackend::Faer { inner, .. } => {
                let par = parallelism.par();
                let mut numeric = NumericLu::new();
                let mut mem = buffer(inner.factorize_numeric_lu_scratch::<f64>(par, faer::Spec::default()))?;
                inner
                    .factorize_numeric_lu(
                        &mut numeric,
                        a.as_faer(),
                        par,
                        MemStack::new(&mut mem),
                        faer::Spec::default(),
                    )
                    .map_err(map_lu_error)?;
                NumericBackend::Faer(Box::new(numeric))
            }
            SymbolicBackend::Multifrontal(s) => NumericBackend::Multifrontal(s.factor(a)?),
        };
        let lu = SparseLu { symbolic: self.clone(), numeric, parallelism };
        let mut probe = vec![1.0; self.n];
        lu.solve_in_place(&mut probe)?;
        if probe.iter().any(|v| !v.is_finite()) {
            return Err(LinalgError::Singular("Factor is exactly singular".into()));
        }
        Ok(lu)
    }
}

#[derive(Clone, Debug)]
enum NumericBackend {
    Faer(Box<NumericLu<usize, f64>>),
    Multifrontal(MultifrontalLu),
}

#[derive(Clone, Debug)]
pub struct SparseLu {
    symbolic: LuSymbolic,
    numeric: NumericBackend,
    parallelism: Parallelism,
}

impl SparseLu {


    pub fn new(a: &CscMatrix) -> Result<Self, LinalgError> {
        LuSymbolic::analyze(a)?.factor(a, Parallelism::Sequential)
    }

    #[must_use]
    pub fn symbolic(&self) -> &LuSymbolic {
        &self.symbolic
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.symbolic.n
    }



    pub fn refactor(&mut self, a: &CscMatrix) -> Result<(), LinalgError> {
        *self = self.symbolic.factor(a, self.parallelism)?;
        Ok(())
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        let perms = 2 * self.symbolic.n * size_of::<usize>();
        match &self.numeric {
            NumericBackend::Faer(_) => self.symbolic.storage_bound_bytes() + perms,
            NumericBackend::Multifrontal(m) => m.nbytes() + perms,
        }
    }

    #[must_use]
    pub fn delayed_pivots(&self) -> usize {
        match &self.numeric {
            NumericBackend::Faer(_) => 0,
            NumericBackend::Multifrontal(m) => m.delayed_pivots(),
        }
    }

    fn solve_impl(&self, rhs: &mut [f64], ncols: usize, transpose: bool) -> Result<(), LinalgError> {
        let n = self.symbolic.n;
        if rhs.len() != n * ncols {
            return Err(LinalgError::Shape(format!(
                "right-hand side of length {} for n = {n}, {ncols} columns",
                rhs.len()
            )));
        }
        if n == 0 || ncols == 0 {
            return Ok(());
        }
        match (&self.symbolic.backend, &self.numeric) {
            (SymbolicBackend::Faer { inner, .. }, NumericBackend::Faer(numeric)) => {
                let par = self.parallelism.par();
                let lu = LuRef::new_unchecked(inner, numeric);
                let mat = MatMut::from_column_major_slice_mut(rhs, n, ncols);
                if transpose {
                    let mut mem = buffer(inner.solve_transpose_in_place_scratch::<f64>(ncols, par))?;
                    lu.solve_transpose_in_place_with_conj(Conj::No, mat, par, MemStack::new(&mut mem));
                } else {
                    let mut mem = buffer(inner.solve_in_place_scratch::<f64>(ncols, par))?;
                    lu.solve_in_place_with_conj(Conj::No, mat, par, MemStack::new(&mut mem));
                }
                Ok(())
            }
            (_, NumericBackend::Multifrontal(m)) => m.solve_in_place(rhs, ncols, transpose),
            (SymbolicBackend::Multifrontal(_), NumericBackend::Faer(_)) => {
                Err(LinalgError::Invalid("numeric factors do not belong to the symbolic analysis".into()))
            }
        }
    }



    pub fn solve_in_place(&self, b: &mut [f64]) -> Result<(), LinalgError> {
        self.solve_impl(b, 1, false)
    }



    pub fn solve_transpose_in_place(&self, b: &mut [f64]) -> Result<(), LinalgError> {
        self.solve_impl(b, 1, true)
    }



    pub fn solve(&self, b: &[f64]) -> Result<Vec<f64>, LinalgError> {
        let mut x = b.to_vec();
        self.solve_in_place(&mut x)?;
        Ok(x)
    }



    pub fn solve_transpose(&self, b: &[f64]) -> Result<Vec<f64>, LinalgError> {
        let mut x = b.to_vec();
        self.solve_transpose_in_place(&mut x)?;
        Ok(x)
    }



    pub fn solve_many_in_place(
        &self,
        b: &mut [f64],
        ncols: usize,
        transpose: bool,
    ) -> Result<(), LinalgError> {
        self.solve_impl(b, ncols, transpose)
    }
}



pub fn spsolve(a: &CscMatrix, b: &[f64]) -> Result<Vec<f64>, LinalgError> {
    SparseLu::new(a)?.solve(b)
}

#[derive(Clone, Debug)]
pub struct CholeskySymbolic {
    inner: Arc<SymbolicCholesky<usize>>,
    n: usize,
    col_ptr: Arc<Vec<usize>>,
    row_idx: Arc<Vec<usize>>,
}

impl CholeskySymbolic {


    pub fn analyze(a: &CscMatrix) -> Result<Self, LinalgError> {
        let (m, n) = a.shape();
        if m != n {
            return Err(LinalgError::Shape(format!("Cholesky of a non-square {m}×{n} matrix")));
        }
        let inner = sp_chol::factorize_symbolic_cholesky(
            a.as_faer().symbolic(),
            Side::Lower,
            sp_chol::SymmetricOrdering::default(),
            sp_chol::CholeskySymbolicParams::default(),
        )?;
        Ok(Self {
            inner: Arc::new(inner),
            n,
            col_ptr: Arc::new(a.indptr().to_vec()),
            row_idx: Arc::new(a.indices().to_vec()),
        })
    }



    pub fn factor(&self, a: &CscMatrix, parallelism: Parallelism) -> Result<SparseCholesky, LinalgError> {
        if a.shape() != (self.n, self.n)
            || a.indptr() != self.col_ptr.as_slice()
            || a.indices() != self.row_idx.as_slice()
        {
            return Err(LinalgError::Shape("matrix pattern differs from the symbolic analysis".into()));
        }
        if !a.is_finite() {
            return Err(LinalgError::NonFinite("matrix entries must be finite".into()));
        }
        let par = parallelism.par();
        let mut values = vec![0.0; self.inner.len_val()];
        let mut mem = buffer(self.inner.factorize_numeric_llt_scratch::<f64>(par, faer::Spec::default()))?;
        self.inner
            .factorize_numeric_llt::<f64>(
                &mut values,
                a.as_faer(),
                Side::Lower,
                faer::linalg::cholesky::llt::factor::LltRegularization::default(),
                par,
                MemStack::new(&mut mem),
                faer::Spec::default(),
            )
            .map_err(|e| LinalgError::NotPositiveDefinite(format!("{e:?}")))?;
        Ok(SparseCholesky { symbolic: self.clone(), values, parallelism })
    }
}

#[derive(Clone, Debug)]
pub struct SparseCholesky {
    symbolic: CholeskySymbolic,
    values: Vec<f64>,
    parallelism: Parallelism,
}

impl SparseCholesky {


    pub fn new(a: &CscMatrix) -> Result<Self, LinalgError> {
        CholeskySymbolic::analyze(a)?.factor(a, Parallelism::Sequential)
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        self.values.len() * size_of::<f64>()
    }



    pub fn solve_in_place(&self, b: &mut [f64]) -> Result<(), LinalgError> {
        let n = self.symbolic.n;
        if b.len() != n {
            return Err(LinalgError::Shape(format!("right-hand side of length {} for n = {n}", b.len())));
        }
        if n == 0 {
            return Ok(());
        }
        let par = self.parallelism.par();
        let llt = sp_chol::LltRef::<'_, usize, f64>::new(&self.symbolic.inner, &self.values);
        let mut mem = buffer(self.symbolic.inner.solve_in_place_scratch::<f64>(1, par))?;
        llt.solve_in_place_with_conj(
            Conj::No,
            MatMut::from_column_major_slice_mut(b, n, 1),
            par,
            MemStack::new(&mut mem),
        );
        Ok(())
    }
}

