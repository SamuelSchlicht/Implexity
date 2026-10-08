// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_linalg::ilu::{ColumnOrdering, Ilu, IluOptions};
use implexity_linalg::lu::SparseLu;
use implexity_linalg::{CscMatrix, CsrMatrix, LinalgError, LinearOperator};

use crate::error::{CfdError, CfdResult};
use crate::numerics::system::SaddlePointSystem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorizationStrategy {
    Direct,
    Ilu,
    Jacobi,
}

impl FactorizationStrategy {

    pub fn parse(s: &str) -> CfdResult<Self> {
        match s {
            "direct" => Ok(Self::Direct),
            "ilu" => Ok(Self::Ilu),
            "jacobi" => Ok(Self::Jacobi),
            other => Err(CfdError::Contract(format!(
                "Unsupported factorization strategy: {}",
                implexity_core::py_repr::repr_str(other)
            ))),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Ilu => "ilu",
            Self::Jacobi => "jacobi",
        }
    }
}

enum Factor {
    Direct(SparseLu),
    Ilu(Ilu),
    Jacobi(Vec<f64>),
}


pub struct SparseApproximateInverse {
    n: usize,
    strategy: FactorizationStrategy,
    factor: Factor,
}

impl std::fmt::Debug for SparseApproximateInverse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SparseApproximateInverse")
            .field("n", &self.n)
            .field("strategy", &self.strategy)
            .finish_non_exhaustive()
    }
}

impl SparseApproximateInverse {

    pub fn new(
        matrix: &CsrMatrix,
        strategy: FactorizationStrategy,
        drop_tolerance: f64,
        fill_factor: f64,
        diagonal_floor: f64,
    ) -> CfdResult<Self> {
        if matrix.nrows() != matrix.ncols() {
            return Err(CfdError::Contract("Approximate inverse requires a square matrix.".into()));
        }
        let factor = match strategy {
            FactorizationStrategy::Direct => {
                let csc: CscMatrix = matrix.to_csc();
                Factor::Direct(SparseLu::new(&csc)?)
            }
            FactorizationStrategy::Ilu => {
                if !(drop_tolerance > 0.0 && drop_tolerance < 1.0) {
                    return Err(CfdError::Contract("ILU drop tolerance must lie in (0, 1).".into()));
                }
                if fill_factor < 1.0 || fill_factor.is_nan() {
                    return Err(CfdError::Contract("ILU fill factor must be at least one.".into()));
                }
                let opts = IluOptions {
                    drop_tol: drop_tolerance,
                    fill_factor,
                    ordering: ColumnOrdering::Colamd,
                    ..IluOptions::default()
                };
                Factor::Ilu(Ilu::new(matrix, &opts)?)
            }
            FactorizationStrategy::Jacobi => {
                let diagonal = matrix.diagonal();
                let scale = diagonal.iter().fold(0.0_f64, |m, d| m.max(d.abs())).max(1.0);
                let floor = diagonal_floor * scale;
                if diagonal.iter().any(|d| d.abs() <= floor || d.is_nan()) {
                    return Err(CfdError::Contract(
                        "Jacobi inverse encountered a zero or near-zero diagonal.".into(),
                    ));
                }
                Factor::Jacobi(diagonal.iter().map(|d| 1.0 / d).collect())
            }
        };
        Ok(Self { n: matrix.nrows(), strategy, factor })
    }

    #[must_use]
    pub fn strategy(&self) -> FactorizationStrategy {
        self.strategy
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.n
    }


    pub fn solve(&self, rhs: &[f64], transpose: bool) -> CfdResult<Vec<f64>> {
        if rhs.len() != self.n {
            return Err(CfdError::Contract(
                "Approximate-inverse right-hand side has the wrong leading dimension.".into(),
            ));
        }
        if !rhs.iter().all(|v| v.is_finite()) {
            return Err(CfdError::Contract(
                "Approximate-inverse right-hand side contains non-finite values.".into(),
            ));
        }
        Ok(match &self.factor {
            Factor::Direct(lu) => {
                if transpose {
                    lu.solve_transpose(rhs)?
                } else {
                    lu.solve(rhs)?
                }
            }
            Factor::Ilu(ilu) => ilu.solve(rhs, transpose)?,
            Factor::Jacobi(inv) => rhs.iter().zip(inv).map(|(b, d)| d * b).collect(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SchurApproximation {
    pub matrix: CsrMatrix,
    pub inverse_velocity_diagonal: Vec<f64>,
    pub regularization: f64,
}


pub fn build_diagonal_velocity_schur(
    system: &SaddlePointSystem,
    relative_diagonal_floor: f64,
    relative_regularization: f64,
) -> CfdResult<SchurApproximation> {
    let diagonal = system.a.diagonal();
    let scale = diagonal.iter().fold(0.0_f64, |m, d| m.max(d.abs())).max(1.0);
    let floor = relative_diagonal_floor * scale;
    if diagonal.iter().any(|d| *d <= floor || d.is_nan()) {
        return Err(CfdError::Contract(
            "The diagonal-velocity Schur approximation requires a positive velocity-block diagonal.".into(),
        ));
    }
    let inverse: Vec<f64> = diagonal.iter().map(|d| 1.0 / d).collect();
    let ones = vec![1.0; system.n_pressure()];
    let bd = system.b.scaled(&ones, &inverse)?;
    let bdbt = bd.matmul(system.b_transpose())?;
    let mut schur = system.c.add_scaled(1.0, &bdbt, 1.0)?;
    let diag_scale = schur.diagonal().iter().fold(0.0_f64, |m, d| m.max(d.abs())).max(1.0);
    let regularization = relative_regularization * diag_scale;
    if regularization > 0.0 {
        let eye = CsrMatrix::identity(system.n_pressure());
        schur = schur.add_scaled(1.0, &eye, regularization)?;
    }
    Ok(SchurApproximation { matrix: schur, inverse_velocity_diagonal: inverse, regularization })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LduOptions {
    pub velocity_strategy: FactorizationStrategy,
    pub schur_strategy: FactorizationStrategy,
    pub velocity_drop_tolerance: f64,
    pub velocity_fill_factor: f64,
    pub schur_drop_tolerance: f64,
    pub schur_fill_factor: f64,
    pub schur_relative_regularization: f64,
}

impl Default for LduOptions {
    fn default() -> Self {
        Self {
            velocity_strategy: FactorizationStrategy::Ilu,
            schur_strategy: FactorizationStrategy::Ilu,
            velocity_drop_tolerance: 1.0e-4,
            velocity_fill_factor: 12.0,
            schur_drop_tolerance: 1.0e-4,
            schur_fill_factor: 12.0,
            schur_relative_regularization: 1.0e-12,
        }
    }
}

pub struct SaddlePointLduPreconditioner {
    system: Arc<SaddlePointSystem>,
    pub schur: SchurApproximation,
    pub velocity_inverse: SparseApproximateInverse,
    pub schur_inverse: SparseApproximateInverse,
}

impl std::fmt::Debug for SaddlePointLduPreconditioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaddlePointLduPreconditioner")
            .field("size", &self.system.size())
            .field("velocity", &self.velocity_inverse.strategy())
            .field("schur", &self.schur_inverse.strategy())
            .finish_non_exhaustive()
    }
}

impl SaddlePointLduPreconditioner {

    pub fn new(system: Arc<SaddlePointSystem>, opts: &LduOptions) -> CfdResult<Self> {
        let schur = build_diagonal_velocity_schur(&system, 1.0e-12, opts.schur_relative_regularization)?;
        let velocity_inverse = SparseApproximateInverse::new(
            &system.a,
            opts.velocity_strategy,
            opts.velocity_drop_tolerance,
            opts.velocity_fill_factor,
            1.0e-14,
        )?;
        let schur_inverse = SparseApproximateInverse::new(
            &schur.matrix,
            opts.schur_strategy,
            opts.schur_drop_tolerance,
            opts.schur_fill_factor,
            1.0e-14,
        )?;
        Ok(Self { system, schur, velocity_inverse, schur_inverse })
    }

    #[must_use]
    pub fn system(&self) -> &Arc<SaddlePointSystem> {
        &self.system
    }


    pub fn apply(&self, rhs: &[f64]) -> CfdResult<Vec<f64>> {
        let sys = &self.system;
        if rhs.len() != sys.size() {
            return Err(CfdError::Contract("Preconditioner input has the wrong size.".into()));
        }
        let n_u = sys.n_velocity();
        let (r_u, r_p) = rhs.split_at(n_u);
        let y = self.velocity_inverse.solve(r_u, false)?;
        let mut t = sys.b.matvec(&y)?;
        for (ti, rp) in t.iter_mut().zip(r_p) {
            *ti -= rp;
        }
        let p = self.schur_inverse.solve(&t, false)?;
        let btp = sys.b_transpose().matvec(&p)?;
        let v = self.velocity_inverse.solve(&btp, false)?;
        let mut out: Vec<f64> = y.iter().zip(&v).map(|(a, b)| a - b).collect();
        out.extend_from_slice(&p);
        Ok(out)
    }


    pub fn apply_transpose(&self, rhs: &[f64]) -> CfdResult<Vec<f64>> {
        let sys = &self.system;
        if rhs.len() != sys.size() {
            return Err(CfdError::Contract("Transpose-preconditioner input has the wrong size.".into()));
        }
        let n_u = sys.n_velocity();
        let (u_bar, p_bar0) = rhs.split_at(n_u);
        let mut p_bar = p_bar0.to_vec();
        let mut y_bar = u_bar.to_vec();
        let v_bar: Vec<f64> = u_bar.iter().map(|x| -x).collect();
        let q_bar = self.velocity_inverse.solve(&v_bar, true)?;
        let bq = sys.b.matvec(&q_bar)?;
        for (pb, x) in p_bar.iter_mut().zip(&bq) {
            *pb += x;
        }
        let t_bar = self.schur_inverse.solve(&p_bar, true)?;
        let btt = sys.b_transpose().matvec(&t_bar)?;
        for (yb, x) in y_bar.iter_mut().zip(&btt) {
            *yb += x;
        }
        let r_p_bar: Vec<f64> = t_bar.iter().map(|x| -x).collect();
        let mut out = self.velocity_inverse.solve(&y_bar, true)?;
        out.extend_from_slice(&r_p_bar);
        Ok(out)
    }

    #[must_use]
    pub fn as_linear_operator(&self, transpose: bool) -> LduOperator<'_> {
        LduOperator { prec: self, transpose }
    }
}

#[derive(Clone, Copy)]
pub struct LduOperator<'a> {
    prec: &'a SaddlePointLduPreconditioner,
    transpose: bool,
}

impl LinearOperator for LduOperator<'_> {
    fn n(&self) -> usize {
        self.prec.system.size()
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        let r = if self.transpose { self.prec.apply_transpose(x) } else { self.prec.apply(x) };
        match r {
            Ok(v) => {
                y.copy_from_slice(&v);
                Ok(())
            }
            Err(e) => Err(LinalgError::Operator(e.to_string())),
        }
    }
}
