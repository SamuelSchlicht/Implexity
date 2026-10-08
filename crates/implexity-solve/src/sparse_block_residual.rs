// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::krylov::{GmresOptions, gmres};
use implexity_linalg::lu::SparseLu;
use implexity_linalg::operator::FnOperator;
use implexity_linalg::sparse::CsrMatrix;

use crate::certificate::{norm2, stable_l2};
use crate::differentiable::{
    Argument, DifferentiableResidual, DifferentiableResponse, gradient, jacobian, response_value, vjp,
};
use crate::matrix::Jacobian;
use crate::pyfmt::fmt_g6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockLayout {
    names: Vec<String>,
    sizes: Vec<usize>,
}

impl BlockLayout {


    pub fn new(names: Vec<String>, sizes: Vec<usize>) -> CaeResult<Self> {
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        if names.is_empty() || names.len() != sizes.len() || unique.len() != names.len() || sizes.contains(&0)
        {
            return Err(CaeError::contract("unique block names and positive block sizes are required"));
        }
        Ok(Self { names, sizes })
    }
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }
    #[must_use]
    pub fn sizes(&self) -> &[usize] {
        &self.sizes
    }
    #[must_use]
    pub fn size(&self) -> usize {
        self.sizes.iter().sum()
    }
    #[must_use]
    pub fn slices(&self) -> Vec<(String, usize, usize)> {
        let mut p = 0;
        self.names
            .iter()
            .zip(&self.sizes)
            .map(|(n, &s)| {
                let out = (n.clone(), p, p + s);
                p += s;
                out
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SparseBlockDiagnostics {
    pub converged: bool,
    pub iterations: usize,
    pub residual_norm: f64,
    pub relative_residual: f64,
    pub matrix_nonzeros: usize,
    pub linear_solver: &'static str,
    pub topology_coordinate: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SparseBlockSensitivity {
    pub value: f64,
    pub gradient: Vec<f64>,
    pub state: Vec<f64>,
    pub adjoint: Vec<f64>,
    pub diagnostics: SparseBlockDiagnostics,
    pub adjoint_relative_residual: f64,
    pub dense_design_jacobian_formed: bool,
}

pub type StateJacobianFn = Box<dyn Fn(&[f64], &[f64]) -> CaeResult<Jacobian> + Send + Sync>;

fn finite_norm(v: &[f64]) -> CaeResult<f64> {
    stable_l2(v).map_err(|e| CaeError::convergence(format!("sparse residual certification failed: {e}")))
}



pub fn to_csr_dropping_zeros(j: Jacobian) -> CaeResult<CsrMatrix> {
    match j {
        Jacobian::Dense(m) => {
            let mut r = Vec::new();
            let mut c = Vec::new();
            let mut v = Vec::new();
            for i in 0..m.nrows {
                for k in 0..m.ncols {
                    let x = m.data[i * m.ncols + k];
                    if x != 0.0 {
                        r.push(i);
                        c.push(k);
                        v.push(x);
                    }
                }
            }
            CsrMatrix::from_triplets(m.nrows, m.ncols, &r, &c, &v)
                .map_err(|e| CaeError::contract(e.to_string()))
        }
        other => other.to_csr(),
    }
}

pub struct SparseBlockResidualSolver<R: DifferentiableResidual> {
    residual: R,
    jacobian_state: Option<StateJacobianFn>,
    tolerance: f64,
    maximum_iterations: usize,
    minimum_step: f64,
}

impl<R: DifferentiableResidual> SparseBlockResidualSolver<R> {
    pub fn new(
        residual: R,
        jacobian_state: Option<StateJacobianFn>,
        tolerance: f64,
        maximum_iterations: usize,
        minimum_step: f64,
    ) -> Self {
        Self { residual, jacobian_state, tolerance, maximum_iterations, minimum_step }
    }

    #[must_use]
    pub fn residual(&self) -> &R {
        &self.residual
    }



    pub fn state_jacobian(&self, u: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        let j = match &self.jacobian_state {
            Some(f) => f(u, x)?,
            None => Jacobian::Dense(jacobian(&self.residual, u, x, Argument::State)),
        };
        let m = to_csr_dropping_zeros(j)?;
        if m.shape() != (u.len(), u.len()) {
            return Err(CaeError::contract(format!(
                "state Jacobian must be square, got ({}, {})",
                m.nrows(),
                m.ncols()
            )));
        }
        Ok(m)
    }

    fn eval(&self, u: &[f64], x: &[f64]) -> Vec<f64> {
        self.residual.residual::<f64>(u, x)
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial_state: &[f64],
    ) -> CaeResult<(Vec<f64>, SparseBlockDiagnostics)> {
        let mut u = initial_state.to_vec();
        let mut last = finite_norm(&self.eval(&u, design))?;
        let base = last.max(1.0);
        let mut nnz = 0;
        for it in 0..=self.maximum_iterations {
            if last <= self.tolerance * base {
                return Ok((
                    u,
                    SparseBlockDiagnostics {
                        converged: true,
                        iterations: it,
                        residual_norm: last,
                        relative_residual: last / base,
                        matrix_nonzeros: nnz,
                        linear_solver: "spsolve",
                        topology_coordinate: TOPOLOGY_COORDINATE,
                    },
                ));
            }
            let j = self.state_jacobian(&u, design)?;
            nnz = j.nnz();
            let rhs: Vec<f64> = self.eval(&u, design).iter().map(|v| -v).collect();
            let du = SparseLu::new(&j.to_csc())
                .and_then(|lu| lu.solve(&rhs))
                .map_err(|_| CaeError::convergence("sparse Newton solve failed"))?;
            if du.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::convergence("non-finite sparse Newton step"));
            }
            let mut a = 1.0;
            let mut accepted = false;
            while a >= self.minimum_step {
                let cand: Vec<f64> = u.iter().zip(&du).map(|(p, d)| p + a * d).collect();
                match finite_norm(&self.eval(&cand, design)) {
                    Ok(rn) if rn < last => {
                        u = cand;
                        last = rn;
                        accepted = true;
                        break;
                    }
                    _ => a *= 0.5,
                }
            }
            if !accepted {
                return Err(CaeError::convergence("Newton line search could not reduce residual"));
            }
        }
        Err(CaeError::convergence("sparse block residual did not converge"))
    }



    pub fn response_and_gradient<J: DifferentiableResponse>(
        &self,
        design: &[f64],
        initial_state: &[f64],
        response: &J,
    ) -> CaeResult<SparseBlockSensitivity> {
        let (u, diag) = self.solve(design, initial_state)?;
        let gu = gradient(response, &u, design, Argument::State);
        let gx = gradient(response, &u, design, Argument::Design);
        let j = self.state_jacobian(&u, design)?;
        let lam = SparseLu::new(&j.to_csc())
            .and_then(|lu| lu.solve_transpose(&gu))
            .map_err(|_| CaeError::convergence("sparse adjoint solve failed"))?;
        let jt_lam = j.matvec_transpose(&lam).map_err(|e| CaeError::contract(e.to_string()))?;
        let ar: Vec<f64> = jt_lam.iter().zip(&gu).map(|(a, b)| a - b).collect();
        let arel = norm2(&ar) / norm2(&gu).max(1.0);
        let rv = vjp(&self.residual, &u, design, &lam, Argument::Design)?;
        let grad: Vec<f64> = gx.iter().zip(&rv).map(|(a, b)| a - b).collect();
        Ok(SparseBlockSensitivity {
            value: response_value(response, &u, design),
            gradient: grad,
            state: u,
            adjoint: lam,
            diagnostics: diag,
            adjoint_relative_residual: arel,
            dense_design_jacobian_formed: false,
        })
    }
}

pub struct TransposeKrylovSolver<R: DifferentiableResidual> {
    residual: R,
    tolerance: f64,
    maxiter: usize,
}

pub type PreconditionerFn<'a> = &'a (dyn Fn(&[f64]) -> CaeResult<Vec<f64>> + Sync);

impl<R: DifferentiableResidual> TransposeKrylovSolver<R> {
    pub fn new(residual: R, tolerance: f64, maxiter: usize) -> Self {
        Self { residual, tolerance, maxiter }
    }



    pub fn solve(
        &self,
        state: &[f64],
        design: &[f64],
        rhs: &[f64],
        preconditioner: Option<PreconditionerFn<'_>>,
    ) -> CaeResult<(Vec<f64>, f64)> {
        let n = state.len();
        let a = FnOperator::new(n, |v: &[f64], y: &mut [f64]| {
            let action = vjp(&self.residual, state, design, v, Argument::State)
                .map_err(|e| implexity_linalg::error::LinalgError::Operator(e.to_string()))?;
            y.copy_from_slice(&action);
            Ok(())
        });
        let opts = GmresOptions {
            rtol: self.tolerance,
            atol: 0.0,
            maxiter: Some(self.maxiter),
            ..GmresOptions::default()
        };
        let out = match preconditioner {
            Some(p) => {
                let m = FnOperator::new(n, |v: &[f64], y: &mut [f64]| {
                    let r =
                        p(v).map_err(|e| implexity_linalg::error::LinalgError::Operator(e.to_string()))?;
                    if r.len() != y.len() {
                        return Err(implexity_linalg::error::LinalgError::Shape("preconditioner returned another length".into()));
                    }
                    y.copy_from_slice(&r);
                    Ok(())
                });
                gmres(&a, rhs, None, Some(&m), &opts)
            }
            None => gmres(&a, rhs, None, None::<&implexity_linalg::operator::Identity>, &opts),
        }
        .map_err(|e| CaeError::convergence(format!("matrix-free transpose GMRES failed: {e}")))?;
        if out.info != 0 {
            return Err(CaeError::convergence(format!(
                "matrix-free transpose GMRES did not converge (info={})",
                out.info
            )));
        }
        let ax = vjp(&self.residual, state, design, &out.x, Argument::State)?;
        let r: Vec<f64> = ax.iter().zip(rhs).map(|(p, q)| p - q).collect();
        let rel = norm2(&r) / norm2(rhs).max(1.0);
        Ok((out.x, rel))
    }
}

#[must_use]
pub fn fmt_residual(value: f64) -> String {
    fmt_g6(value)
}

