// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
