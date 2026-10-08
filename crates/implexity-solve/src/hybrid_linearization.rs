// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
use implexity_linalg::error::LinalgError;
use implexity_linalg::krylov::{GmresCallbackType, GmresOptions, gmres};
use implexity_linalg::lu::SparseLu;
use implexity_linalg::operator::{FnOperator, Identity};
use implexity_linalg::sparse::CsrMatrix;

use crate::certificate::norm2;
use crate::differentiable::{
    Argument, DifferentiableResidual, DifferentiableResponse, gradient, jacobian, jvp_state, response_value,
    vjp,
};
use crate::pyfmt::fmt_e;
use crate::sparse_block_residual::{StateJacobianFn, to_csr_dropping_zeros};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearBackend {
    DenseReference,
    SparseDirect,
    MatrixFree,
}

impl LinearBackend {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DenseReference => "dense_reference",
            Self::SparseDirect => "sparse_direct",
            Self::MatrixFree => "matrix_free",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinearSolveDiagnostics {
    pub requested_backend: &'static str,
    pub used_backend: &'static str,
    pub converged: bool,
    pub relative_residual: f64,
    pub iterations: usize,
    pub fallback_used: bool,
    pub topology_coordinate: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HybridSensitivity {
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
    pub topology_coordinate: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HybridLinearisationPolicy {
    pub dense_max_dofs: usize,
    pub sparse_max_dofs: usize,
    pub krylov_rtol: f64,
    pub krylov_maxiter: usize,
    pub primal_gate: f64,
    pub adjoint_gate: f64,
}

impl Default for HybridLinearisationPolicy {
    fn default() -> Self {
        Self {
            dense_max_dofs: 64,
            sparse_max_dofs: 4096,
            krylov_rtol: 1e-10,
            krylov_maxiter: 400,
            primal_gate: 1e-8,
            adjoint_gate: 1e-8,
        }
    }
}

impl HybridLinearisationPolicy {
    pub fn validate(&self) -> CaeResult<()> {
        for (value, name) in [(self.krylov_rtol, "Krylov tolerance"), (self.primal_gate, "primal gate"), (self.adjoint_gate, "adjoint gate")] {
            if !value.is_finite() || value < 0.0 {
                return Err(CaeError::contract(format!("{name} must be finite and non-negative")));
            }
        }
        if self.krylov_maxiter == 0 {
            return Err(CaeError::contract("Krylov iteration limit must be positive"));
        }
        Ok(())
    }

    #[must_use]
    pub fn choose(&self, n: usize, has_sparse: bool) -> LinearBackend {
        if n <= self.dense_max_dofs {
            LinearBackend::DenseReference
        } else if has_sparse && n <= self.sparse_max_dofs {
            LinearBackend::SparseDirect
        } else {
            LinearBackend::MatrixFree
        }
    }
}

pub type HybridPreconditioner =
    Box<dyn Fn(&[f64], &[f64], &[f64], bool) -> CaeResult<Vec<f64>> + Send + Sync>;

pub struct HybridResidualSolver<R: DifferentiableResidual> {
    residual: R,
    jacobian_state: Option<StateJacobianFn>,
    preconditioner: Option<HybridPreconditioner>,
    policy: HybridLinearisationPolicy,
    nonlinear_tolerance: f64,
    maximum_newton_iterations: usize,
    minimum_step: f64,
}

fn relative(ax: &[f64], rhs: &[f64]) -> f64 {
    let r: Vec<f64> = ax.iter().zip(rhs).map(|(a, b)| a - b).collect();
    norm2(&r) / norm2(rhs).max(1.0)
}

pub(crate) fn validate_newton_parameters(tolerance: f64, minimum_step: f64) -> CaeResult<()> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(CaeError::contract("nonlinear tolerance must be finite and non-negative"));
    }
    if !minimum_step.is_finite() || minimum_step <= 0.0 || minimum_step > 1.0 {
        return Err(CaeError::contract("minimum Newton step must be finite and in (0, 1]"));
    }
    Ok(())
}

impl<R: DifferentiableResidual> HybridResidualSolver<R> {
    pub fn new(
        residual: R,
        jacobian_state: Option<StateJacobianFn>,
        preconditioner: Option<HybridPreconditioner>,
        policy: HybridLinearisationPolicy,
        nonlinear_tolerance: f64,
        maximum_newton_iterations: usize,
        minimum_step: f64,
    ) -> Self {
        Self {
            residual,
            jacobian_state,
            preconditioner,
            policy,
            nonlinear_tolerance,
            maximum_newton_iterations,
            minimum_step,
        }
    }

    #[must_use]
    pub fn residual(&self) -> &R {
        &self.residual
    }
    #[must_use]
    pub fn policy(&self) -> &HybridLinearisationPolicy {
        &self.policy
    }
    #[must_use]
    pub fn newton_parameters(&self) -> (f64, usize, f64) {
        (self.nonlinear_tolerance, self.maximum_newton_iterations, self.minimum_step)
    }

    #[must_use]
    pub fn has_sparse(&self) -> bool {
        self.jacobian_state.is_some()
    }

    #[must_use]
    pub fn dense_jacobian(&self, u: &[f64], x: &[f64]) -> DenseMatrix {
        jacobian(&self.residual, u, x, Argument::State)
    }



    pub fn sparse_jacobian(&self, u: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        let j = match &self.jacobian_state {
            Some(f) => f(u, x)?,
            None => crate::matrix::Jacobian::Dense(self.dense_jacobian(u, x)),
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

    #[must_use]
    pub fn matrix_free_action(&self, u: &[f64], x: &[f64], v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if transpose {
            vjp(&self.residual, u, x, v, Argument::State)
        } else {
            jvp_state(&self.residual, u, x, v)
        }
    }



    pub fn sparse_solve(
        &self,
        u: &[f64],
        x: &[f64],
        rhs: &[f64],
        transpose: bool,
    ) -> CaeResult<(Vec<f64>, f64)> {
        let a = self.sparse_jacobian(u, x)?;
        let lu = SparseLu::new(&a.to_csc())
            .map_err(|e| CaeError::convergence(format!("sparse direct solve failed: {e}")))?;
        let sol = if transpose { lu.solve_transpose(rhs) } else { lu.solve(rhs) }
            .map_err(|e| CaeError::convergence(format!("sparse direct solve failed: {e}")))?;
        let ax = if transpose { a.matvec_transpose(&sol) } else { a.matvec(&sol) }
            .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok((sol.clone(), relative(&ax, rhs)))
    }



    pub fn linear_solve(
        &self,
        u: &[f64],
        x: &[f64],
        rhs: &[f64],
        transpose: bool,
        backend: Option<LinearBackend>,
        allow_fallback: bool,
    ) -> CaeResult<(Vec<f64>, LinearSolveDiagnostics)> {
        self.policy.validate()?;
        let n = rhs.len();
        let chosen = backend.unwrap_or_else(|| self.policy.choose(n, self.has_sparse()));
        let diag = |used: LinearBackend, converged: bool, rel: f64, iterations: usize, fallback: bool| {
            LinearSolveDiagnostics {
                requested_backend: chosen.as_str(),
                used_backend: used.as_str(),
                converged,
                relative_residual: rel,
                iterations,
                fallback_used: fallback,
                topology_coordinate: TOPOLOGY_COORDINATE,
            }
        };
        match chosen {
            LinearBackend::DenseReference => {
                let a = self.dense_jacobian(u, x);
                let a = if transpose { a.transpose() } else { a };
                let sol = DenseLu::new(&a)
                    .and_then(|lu| lu.solve(rhs, 1, false))
                    .map_err(|e| CaeError::convergence(format!("dense reference solve failed: {e}")))?;
                let ax = a.matvec(&sol).map_err(|e| CaeError::contract(e.to_string()))?;
                let rel = relative(&ax, rhs);
                Ok((sol, diag(chosen, true, rel, 1, false)))
            }
            LinearBackend::SparseDirect => {
                let (sol, rel) = self.sparse_solve(u, x, rhs, transpose)?;
                Ok((sol, diag(chosen, rel.is_finite(), rel, 1, false)))
            }
            LinearBackend::MatrixFree => {
                let a = FnOperator::new(n, |v: &[f64], y: &mut [f64]| {
                    let action = self.matrix_free_action(u, x, v, transpose)
                        .map_err(|e| LinalgError::Operator(e.to_string()))?;
                    if action.len() != y.len() {
                        return Err(LinalgError::Shape("state action returned another length".into()));
                    }
                    y.copy_from_slice(&action);
                    Ok(())
                });
                let opts = GmresOptions {
                    rtol: self.policy.krylov_rtol,
                    atol: 0.0,
                    restart: None,
                    maxiter: Some(self.policy.krylov_maxiter),
                    callback_type: GmresCallbackType::PrNorm,
                };
                let outcome = match &self.preconditioner {
                    Some(p) => {
                        let m = FnOperator::new(n, |v: &[f64], y: &mut [f64]| {
                            let r =
                                p(u, x, v, transpose).map_err(|e| LinalgError::Operator(e.to_string()))?;
                            if r.len() != y.len() {
                                return Err(LinalgError::Shape("preconditioner returned another length".into()));
                            }
                            y.copy_from_slice(&r);
                            Ok(())
                        });
                        gmres(&a, rhs, None, Some(&m), &opts)
                    }
                    None => gmres(&a, rhs, None, None::<&Identity>, &opts),
                };
                let (sol, info, iterations, rel) = match outcome {
                    Ok(r) => {
                        let ax = self.matrix_free_action(u, x, &r.x, transpose)?;
                        let rel = relative(&ax, rhs);
                        (r.x, i64::try_from(r.info).unwrap_or(i64::MAX), r.iterations, rel)
                    }
                    Err(e) => {
                        if !allow_fallback {
                            return Err(CaeError::convergence(format!(
                                "matrix-free Krylov solve failed: {e}"
                            )));
                        }
                        (vec![0.0; n], -999, 0, f64::INFINITY)
                    }
                };
                if info == 0 && rel.is_finite() {
                    return Ok((sol, diag(chosen, true, rel, iterations, false)));
                }
                if !allow_fallback {
                    return Err(CaeError::convergence(format!(
                        "matrix-free Krylov solve did not converge (info={info}, rel={})",
                        fmt_e(rel, 3)
                    )));
                }
                let (sol, rel) = self.sparse_solve(u, x, rhs, transpose)?;
                Ok((sol, diag(LinearBackend::SparseDirect, rel.is_finite(), rel, iterations + 1, true)))
            }
        }
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial_state: &[f64],
        backend: Option<LinearBackend>,
    ) -> CaeResult<(Vec<f64>, f64, Option<LinearSolveDiagnostics>)> {
        self.policy.validate()?;
        validate_newton_parameters(self.nonlinear_tolerance, self.minimum_step)?;
        let mut u = initial_state.to_vec();
        let mut r = self.residual.residual::<f64>(&u, design);
        let base = norm2(&r).max(1.0);
        let mut last = norm2(&r);
        let mut last_diag = None;
        for _ in 0..=self.maximum_newton_iterations {
            if last <= self.nonlinear_tolerance * base {
                return Ok((u, last / base, last_diag));
            }
            let neg: Vec<f64> = r.iter().map(|v| -v).collect();
            let (du, d) = self.linear_solve(&u, design, &neg, false, backend, true)?;
            last_diag = Some(d);
            if du.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::convergence("non-finite Newton step"));
            }
            let mut alpha = 1.0;
            let mut accepted = false;
            while alpha >= self.minimum_step {
                let cand: Vec<f64> = u.iter().zip(&du).map(|(a, d)| a + alpha * d).collect();
                let rc = self.residual.residual::<f64>(&cand, design);
                let rn = norm2(&rc);
                if rn.is_finite() && rn < last {
                    u = cand;
                    r = rc;
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



    pub fn response_and_gradient<J: DifferentiableResponse>(
        &self,
        design: &[f64],
        initial_state: &[f64],
        response: &J,
        backend: Option<LinearBackend>,
    ) -> CaeResult<HybridSensitivity> {
        let (u, prel, pdiag) = self.solve(design, initial_state, backend)?;
        let gu = gradient(response, &u, design, Argument::State);
        let gx = gradient(response, &u, design, Argument::Design);
        let (lam, adiag) = self.linear_solve(&u, design, &gu, true, backend, true)?;
        let rv = vjp(&self.residual, &u, design, &lam, Argument::Design)?;
        let grad: Vec<f64> = gx.iter().zip(&rv).map(|(a, b)| a - b).collect();
        let admissible = prel <= self.policy.primal_gate
            && adiag.relative_residual <= self.policy.adjoint_gate
            && grad.iter().all(|v| v.is_finite());
        let primal_backend = pdiag.as_ref().map_or_else(
            || backend.unwrap_or_else(|| self.policy.choose(u.len(), self.has_sparse())).as_str(),
            |d| d.used_backend,
        );
        Ok(HybridSensitivity {
            value: response_value(response, &u, design),
            gradient: grad,
            fallback_used: pdiag.as_ref().is_some_and(|d| d.fallback_used) || adiag.fallback_used,
            state: u,
            adjoint: lam,
            primal_relative_residual: prel,
            adjoint_relative_residual: adiag.relative_residual,
            primal_backend,
            adjoint_backend: adiag.used_backend,
            admissible_for_optimization: admissible,
            topology_coordinate: TOPOLOGY_COORDINATE,
        })
    }



    pub fn require_optimization_admission(sensitivity: HybridSensitivity) -> CaeResult<HybridSensitivity> {
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

