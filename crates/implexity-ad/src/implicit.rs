// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use crate::error::AdError;
use crate::tape::{Tape, Var};

pub type TransposeSolve = Arc<dyn Fn(&[f64]) -> Result<Vec<f64>, AdError> + Send + Sync>;

pub type ResidualParamVjp = Arc<dyn Fn(&[f64]) -> Result<Vec<f64>, AdError> + Send + Sync>;

pub type VectorMap<'a> = dyn Fn(&[f64]) -> Result<Vec<f64>, AdError> + 'a;

#[derive(Clone, Debug, PartialEq)]
pub struct ImplicitAdjoint {
    pub lambda: Vec<f64>,
    pub grad_params: Vec<f64>,
}



pub fn implicit_adjoint(
    x_bar: &[f64],
    transpose_solve: &VectorMap<'_>,
    residual_param_vjp: &VectorMap<'_>,
) -> Result<ImplicitAdjoint, AdError> {
    let lambda = transpose_solve(x_bar)?;
    if lambda.len() != x_bar.len() {
        return Err(AdError::Shape(format!(
            "transposed solve returned {} values for a system of {}",
            lambda.len(),
            x_bar.len()
        )));
    }
    let mut grad_params = residual_param_vjp(&lambda)?;
    for g in &mut grad_params {
        *g = -*g;
    }
    Ok(ImplicitAdjoint { lambda, grad_params })
}

impl Tape {


    pub fn implicit_solution(
        &mut self,
        params: Var,
        solution: Vec<f64>,
        transpose_solve: TransposeSolve,
        residual_param_vjp: ResidualParamVjp,
    ) -> Result<Var, AdError> {
        let np = params.len();
        self.custom(
            &[params],
            solution,
            Box::new(move |ct| {
                let adj = implicit_adjoint(ct, &*transpose_solve, &*residual_param_vjp)?;
                if adj.grad_params.len() != np {
                    return Err(AdError::Shape(format!(
                        "residual parameter pullback returned {} values for {np} parameters",
                        adj.grad_params.len()
                    )));
                }
                Ok(vec![adj.grad_params])
            }),
        )
    }



    pub fn linear_solve(
        &mut self,
        rhs: Var,
        solution: Vec<f64>,
        transpose_solve: TransposeSolve,
    ) -> Result<Var, AdError> {
        if solution.len() != rhs.len() {
            return Err(AdError::Shape(format!(
                "linear_solve: solution of length {} for right-hand side of {}",
                solution.len(),
                rhs.len()
            )));
        }
        self.custom(&[rhs], solution, Box::new(move |ct| Ok(vec![transpose_solve(ct)?])))
    }
}
