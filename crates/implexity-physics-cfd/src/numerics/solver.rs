// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_linalg::krylov::{GmresCallbackType, GmresOptions, LgmresOptions, gmres, lgmres};

use crate::error::{CfdError, CfdResult};
use crate::numerics::preconditioner::{FactorizationStrategy, LduOptions, SaddlePointLduPreconditioner};
use crate::numerics::system::SaddlePointSystem;
use crate::pyfmt::fmt_e;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KrylovMethod {
    Gmres,
    Lgmres,
}

impl KrylovMethod {

    pub fn parse(s: &str) -> CfdResult<Self> {
        match s {
            "gmres" => Ok(Self::Gmres),
            "lgmres" => Ok(Self::Lgmres),
            other => Err(CfdError::Contract(format!(
                "Unsupported Krylov method: {}",
                implexity_core::py_repr::repr_str(other)
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaddlePointSolverConfig {
    pub method: KrylovMethod,
    pub relative_tolerance: f64,
    pub absolute_tolerance: f64,
    pub restart: usize,
    pub maximum_iterations: usize,
    pub velocity_preconditioner: FactorizationStrategy,
    pub schur_preconditioner: FactorizationStrategy,
    pub velocity_drop_tolerance: f64,
    pub velocity_fill_factor: f64,
    pub schur_drop_tolerance: f64,
    pub schur_fill_factor: f64,
    pub schur_relative_regularization: f64,
    pub require_convergence: bool,
}

impl Default for SaddlePointSolverConfig {
    fn default() -> Self {
        Self {
            method: KrylovMethod::Gmres,
            relative_tolerance: 1.0e-9,
            absolute_tolerance: 1.0e-12,
            restart: 80,
            maximum_iterations: 500,
            velocity_preconditioner: FactorizationStrategy::Ilu,
            schur_preconditioner: FactorizationStrategy::Ilu,
            velocity_drop_tolerance: 1.0e-4,
            velocity_fill_factor: 12.0,
            schur_drop_tolerance: 1.0e-4,
            schur_fill_factor: 12.0,
            schur_relative_regularization: 1.0e-12,
            require_convergence: true,
        }
    }
}

impl SaddlePointSolverConfig {

    pub fn validate(&self) -> CfdResult<()> {
        if !(self.relative_tolerance > 0.0 && self.relative_tolerance < 1.0) {
            return Err(CfdError::Contract("relative_tolerance must lie in (0, 1).".into()));
        }
        if !self.absolute_tolerance.is_finite() || self.absolute_tolerance < 0.0 {
            return Err(CfdError::Contract("absolute_tolerance must be finite and nonnegative.".into()));
        }
        if self.restart < 2 || self.maximum_iterations < 1 {
            return Err(CfdError::Contract("Invalid Krylov restart or iteration limit.".into()));
        }
        for value in [self.velocity_drop_tolerance, self.schur_drop_tolerance] {
            if !(value > 0.0 && value < 1.0) {
                return Err(CfdError::Contract("ILU drop tolerances must lie in (0, 1).".into()));
            }
        }
        for value in [self.velocity_fill_factor, self.schur_fill_factor] {
            if !value.is_finite() || value < 1.0 {
                return Err(CfdError::Contract("ILU fill factors must be finite and at least one.".into()));
            }
        }
        if !self.schur_relative_regularization.is_finite() || self.schur_relative_regularization < 0.0 {
            return Err(CfdError::Contract("Schur regularization must be finite and nonnegative.".into()));
        }
        Ok(())
    }

    fn ldu_options(&self) -> LduOptions {
        LduOptions {
            velocity_strategy: self.velocity_preconditioner,
            schur_strategy: self.schur_preconditioner,
            velocity_drop_tolerance: self.velocity_drop_tolerance,
            velocity_fill_factor: self.velocity_fill_factor,
            schur_drop_tolerance: self.schur_drop_tolerance,
            schur_fill_factor: self.schur_fill_factor,
            schur_relative_regularization: self.schur_relative_regularization,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SaddlePointSolveResult {
    pub state: Vec<f64>,
    pub velocity: Vec<f64>,
    pub pressure: Vec<f64>,
    pub converged: bool,
    pub solver_info: usize,
    pub transpose: bool,
    pub iterations_observed: usize,
    pub residual_history: Vec<f64>,
    pub residual_norm: f64,
    pub relative_residual: f64,
    pub velocity_block_residual_norm: f64,
    pub continuity_residual_norm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaddlePointKrylovSolver {
    pub config: SaddlePointSolverConfig,
}

fn norm2(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum::<f64>().sqrt()
}

impl SaddlePointKrylovSolver {

    pub fn new(config: SaddlePointSolverConfig) -> CfdResult<Self> {
        config.validate()?;
        Ok(Self { config })
    }


    pub fn prepare(&self, system: &Arc<SaddlePointSystem>) -> CfdResult<SaddlePointLduPreconditioner> {
        SaddlePointLduPreconditioner::new(Arc::clone(system), &self.config.ldu_options())
    }


    pub fn solve(
        &self,
        system: &Arc<SaddlePointSystem>,
        rhs: Option<&[f64]>,
        transpose: bool,
        initial_guess: Option<&[f64]>,
        preconditioner: Option<&SaddlePointLduPreconditioner>,
    ) -> CfdResult<SaddlePointSolveResult> {
        let c = &self.config;
        let b = match rhs {
            None => system.rhs(),
            Some(r) => r.to_vec(),
        };
        if b.len() != system.size() || !b.iter().all(|v| v.is_finite()) {
            return Err(CfdError::Contract("The saddle-point right-hand side is invalid.".into()));
        }
        if let Some(x0) = initial_guess
            && (x0.len() != system.size() || !x0.iter().all(|v| v.is_finite()))
        {
            return Err(CfdError::Contract("The initial guess is invalid.".into()));
        }
        let owned;
        let block_prec = if let Some(p) = preconditioner {
            p
        } else {
            owned = self.prepare(system)?;
            &owned
        };
        if !Arc::ptr_eq(block_prec.system(), system) {
            return Err(CfdError::Contract(
                "A prepared preconditioner cannot be reused with another system instance.".into(),
            ));
        }
        let operator = system.operator(transpose);
        let m = block_prec.as_linear_operator(transpose);
        let (state, info, history) = match c.method {
            KrylovMethod::Gmres => {
                let opts = GmresOptions {
                    rtol: c.relative_tolerance,
                    atol: c.absolute_tolerance,
                    restart: Some(c.restart),
                    maxiter: Some(c.maximum_iterations),
                    callback_type: GmresCallbackType::PrNorm,
                };
                let r = gmres(&operator, &b, initial_guess, Some(&m), &opts)?;
                let history = r.residuals.iter().map(|v| v.abs()).collect();
                (r.x, r.info, history)
            }
            KrylovMethod::Lgmres => {
                let opts = LgmresOptions {
                    rtol: c.relative_tolerance,
                    atol: c.absolute_tolerance,
                    maxiter: c.maximum_iterations,
                    ..LgmresOptions::default()
                };
                let mut outer_v = Vec::new();
                let r = lgmres(&operator, &b, initial_guess, Some(&m), &opts, &mut outer_v)?;
                (r.x, r.info, r.residuals)
            }
        };
        let mut residual = vec![0.0; system.size()];
        system.apply(&state, &mut residual, transpose)?;
        for (ri, bi) in residual.iter_mut().zip(&b) {
            *ri -= bi;
        }
        let residual_norm = norm2(&residual);
        let rhs_norm = norm2(&b);
        let relative = residual_norm / rhs_norm.max(f64::MIN_POSITIVE);
        let n_u = system.n_velocity();
        let target = c.absolute_tolerance.max(c.relative_tolerance * rhs_norm);
        let converged = info == 0 && residual_norm <= (10.0 * target).max(100.0 * f64::EPSILON);
        let result = SaddlePointSolveResult {
            velocity: state[..n_u].to_vec(),
            pressure: state[n_u..].to_vec(),
            converged,
            solver_info: info,
            transpose,
            iterations_observed: history.len(),
            residual_history: history,
            residual_norm,
            relative_residual: relative,
            velocity_block_residual_norm: norm2(&residual[..n_u]),
            continuity_residual_norm: norm2(&residual[n_u..]),
            state,
        };
        if c.require_convergence && !converged {
            return Err(CfdError::Convergence(format!(
                "{} saddle-point solve did not converge: info={info}, residual={}, relative={}.",
                if transpose { "Transpose" } else { "Primal" },
                fmt_e(residual_norm, 6),
                fmt_e(relative, 6)
            )));
        }
        Ok(result)
    }
}
