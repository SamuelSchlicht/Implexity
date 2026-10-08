// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix, cond};

use crate::certificate::norm2;
use crate::differentiable::{
    Argument, DifferentiableResidual, DifferentiableResponse, gradient, jacobian, response_value,
};
use crate::pyfmt::fmt_g6;

#[derive(Clone, Debug, PartialEq)]
pub struct CompositeSolveDiagnostics {
    pub converged: bool,
    pub iterations: usize,
    pub residual_norm: f64,
    pub initial_residual_norm: f64,
    pub condition_number: f64,
    pub row_scale_min: f64,
    pub row_scale_max: f64,
    pub column_scale_min: f64,
    pub column_scale_max: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompositeSensitivity {
    pub value: f64,
    pub gradient: Vec<f64>,
    pub state: Vec<f64>,
    pub adjoint: Vec<f64>,
    pub diagnostics: CompositeSolveDiagnostics,
    pub adjoint_relative_residual: f64,
}

type EquilibratedStep = (Vec<f64>, Vec<f64>, Vec<f64>, f64);

pub struct CompositeResidualSolver<R: DifferentiableResidual> {
    residual: R,
    tolerance: f64,
    maximum_iterations: usize,
    minimum_step: f64,
    scaling_floor: f64,
    condition_limit: f64,
}

fn lin(e: &implexity_linalg::error::LinalgError) -> CaeError {
    CaeError::convergence(e.to_string())
}

impl<R: DifferentiableResidual> CompositeResidualSolver<R> {


    pub fn new(
        residual: R,
        tolerance: f64,
        maximum_iterations: usize,
        minimum_step: f64,
        scaling_floor: f64,
        condition_limit: f64,
    ) -> CaeResult<Self> {
        if tolerance <= 0.0 || maximum_iterations == 0 {
            return Err(CaeError::contract("positive tolerance and iteration count are required"));
        }
        Ok(Self { residual, tolerance, maximum_iterations, minimum_step, scaling_floor, condition_limit })
    }

    fn equilibrated_step(&self, j: &DenseMatrix, r: &[f64]) -> CaeResult<EquilibratedStep> {
        let n = j.nrows;
        let m = j.ncols;
        let row_scale: Vec<f64> = (0..n)
            .map(|i| {
                1.0 / j.data[i * m..(i + 1) * m]
                    .iter()
                    .fold(0.0_f64, |a, v| a.max(v.abs()))
                    .max(self.scaling_floor)
            })
            .collect();
        let mut scaled = j.clone();
        for (row, s) in scaled.data.chunks_mut(m.max(1)).zip(&row_scale) {
            for v in row {
                *v *= s;
            }
        }
        let col_scale: Vec<f64> = (0..m)
            .map(|k| {
                1.0 / (0..n).fold(0.0_f64, |a, i| a.max(scaled.data[i * m + k].abs())).max(self.scaling_floor)
            })
            .collect();
        for row in scaled.data.chunks_mut(m.max(1)) {
            for (v, s) in row.iter_mut().zip(&col_scale) {
                *v *= s;
            }
        }
        let condition = cond(&scaled).map_err(|e| lin(&e))?;
        if !condition.is_finite() || condition > self.condition_limit {
            return Err(CaeError::convergence(format!(
                "equilibrated residual Jacobian is ill-conditioned ({})",
                fmt_g6(condition)
            )));
        }
        let rhs: Vec<f64> = row_scale.iter().zip(r).map(|(s, v)| -s * v).collect();
        let step = DenseLu::new(&scaled).and_then(|lu| lu.solve(&rhs, 1, false)).map_err(|e| lin(&e))?;
        Ok((col_scale.iter().zip(&step).map(|(c, s)| c * s).collect(), row_scale, col_scale, condition))
    }



    pub fn solve(
        &self,
        design: &[f64],
        initial_state: &[f64],
    ) -> CaeResult<(Vec<f64>, CompositeSolveDiagnostics)> {
        let mut state = initial_state.to_vec();
        let initial = norm2(&self.residual.residual::<f64>(&state, design));
        let mut last = initial;
        let mut row_scale = vec![1.0; state.len()];
        let mut col_scale = vec![1.0; state.len()];
        let mut condition = 1.0;
        let mut converged = last <= self.tolerance;
        let mut iterations = 0;
        for iteration in 1..=self.maximum_iterations {
            if converged {
                break;
            }
            let r = self.residual.residual::<f64>(&state, design);
            let j = jacobian(&self.residual, &state, design, Argument::State);
            if (j.nrows, j.ncols) != (state.len(), state.len()) {
                return Err(CaeError::convergence(format!(
                    "residual Jacobian must be square, got ({}, {})",
                    j.nrows, j.ncols
                )));
            }
            let (step, rs, cs, c) = self.equilibrated_step(&j, &r)?;
            row_scale = rs;
            col_scale = cs;
            condition = c;
            let mut accepted = false;
            let mut alpha = 1.0;
            while alpha >= self.minimum_step {
                let cand: Vec<f64> = state.iter().zip(&step).map(|(a, s)| a + alpha * s).collect();
                let norm = norm2(&self.residual.residual::<f64>(&cand, design));
                if norm.is_finite() && norm < last {
                    state = cand;
                    last = norm;
                    accepted = true;
                    break;
                }
                alpha *= 0.5;
            }
            if !accepted {
                return Err(CaeError::convergence(
                    "Newton line search could not reduce the physical residual",
                ));
            }
            iterations = iteration;
            converged = last <= self.tolerance * initial.max(1.0);
        }
        let fold = |v: &[f64], f: fn(f64, f64) -> f64, init: f64| v.iter().copied().fold(init, f);
        let diagnostics = CompositeSolveDiagnostics {
            converged,
            iterations,
            residual_norm: last,
            initial_residual_norm: initial,
            condition_number: condition,
            row_scale_min: fold(&row_scale, f64::min, f64::INFINITY),
            row_scale_max: fold(&row_scale, f64::max, f64::NEG_INFINITY),
            column_scale_min: fold(&col_scale, f64::min, f64::INFINITY),
            column_scale_max: fold(&col_scale, f64::max, f64::NEG_INFINITY),
        };
        if !converged {
            return Err(CaeError::convergence(format!(
                "composite residual did not converge in {} iterations; norm={}",
                self.maximum_iterations,
                fmt_g6(last)
            )));
        }
        Ok((state, diagnostics))
    }



    pub fn response_and_gradient<J: DifferentiableResponse>(
        &self,
        design: &[f64],
        initial_state: &[f64],
        response: &J,
    ) -> CaeResult<CompositeSensitivity> {
        let (state, diagnostics) = self.solve(design, initial_state)?;
        let value = response_value(response, &state, design);
        let js = jacobian(&self.residual, &state, design, Argument::State);
        let jd = jacobian(&self.residual, &state, design, Argument::Design);
        let gu = gradient(response, &state, design, Argument::State);
        let gx = gradient(response, &state, design, Argument::Design);
        let condition = cond(&js).map_err(|e| lin(&e))?;
        if !condition.is_finite() || condition > self.condition_limit {
            return Err(CaeError::convergence(format!(
                "adjoint Jacobian is ill-conditioned ({})",
                fmt_g6(condition)
            )));
        }
        let adjoint = DenseLu::new(&js).and_then(|lu| lu.solve(&gu, 1, true)).map_err(|e| lin(&e))?;
        let jd_t_lam = crate::matrix::Jacobian::Dense(jd).apply(&adjoint, true)?;
        let grad: Vec<f64> = gx.iter().zip(&jd_t_lam).map(|(a, b)| a - b).collect();
        let jt_lam = crate::matrix::Jacobian::Dense(js).apply(&adjoint, true)?;
        let res: Vec<f64> = jt_lam.iter().zip(&gu).map(|(a, b)| a - b).collect();
        let rel = norm2(&res) / norm2(&gu).max(1e-30);
        Ok(CompositeSensitivity {
            value,
            gradient: grad,
            state,
            adjoint,
            diagnostics,
            adjoint_relative_residual: rel,
        })
    }
}

