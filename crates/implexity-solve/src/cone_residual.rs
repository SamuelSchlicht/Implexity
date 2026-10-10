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

use crate::positive_cone::{ConeProjection, ConeProjectionPolicy, ConeProjectionProblem};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{CscMatrix, lu::SparseCholesky};
use std::sync::Arc;
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone)]
pub struct ConeResidualProblem {
    projection: ConeProjectionProblem,
    inverse: Arc<SparseCholesky>,
    equality: Arc<Vec<Vec<f64>>>,
    step: f64,
}
pub struct ConeResidual {
    pub residual: Vec<f64>,
    pub projection: ConeProjection,
    problem: ConeResidualProblem,
}
impl ConeResidualProblem {
    pub fn new(
        metric: Vec<Vec<(usize, f64)>>,
        equality: Vec<Vec<f64>>,
        policy: ConeProjectionPolicy,
        step: f64,
    ) -> CaeResult<Self> {
        if !step.is_finite() || step <= 0. {
            return Err(fail("cone residual numerical step must be positive"));
        }
        let n = metric.len();
        let mut rows = vec![];
        let mut cols = vec![];
        let mut vals = vec![];
        for (i, row) in metric.iter().enumerate() {
            for &(j, v) in row {
                rows.push(i);
                cols.push(j);
                vals.push(v);
            }
        }
        let projection = ConeProjectionProblem::new(metric, equality.clone(), policy)?;
        let matrix =
            CscMatrix::from_triplets(n, n, &rows, &cols, &vals).map_err(|e| fail(e.to_string()))?;
        let inverse = Arc::new(SparseCholesky::new(&matrix).map_err(|e| fail(e.to_string()))?);
        Ok(Self {
            projection,
            inverse,
            equality: Arc::new(equality),
            step,
        })
    }
    pub fn dimension(&self) -> usize {
        self.projection.dimension()
    }
    pub fn physical_preconditioner_rhs(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        let out = self
            .projection
            .metric_action(value)?
            .into_iter()
            .map(|v| v / self.step)
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("cone preconditioner scaling overflow"));
        }
        Ok(out)
    }
    fn inverse_action(&self, v: &[f64]) -> CaeResult<Vec<f64>> {
        if v.len() != self.dimension() || v.iter().any(|x| !x.is_finite()) {
            return Err(fail("invalid cone residual vector"));
        }
        let mut out = v.to_vec();
        self.inverse
            .solve_in_place(&mut out)
            .map_err(|e| fail(e.to_string()))?;
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("cone residual inverse is nonfinite"));
        }
        Ok(out)
    }
    pub fn predictor(
        &self,
        state: &[f64],
        physical_residual: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        if state.len() != self.dimension() || state.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(fail("invalid cone residual state"));
        }
        let flow = self.inverse_action(physical_residual)?;
        let predictor = state
            .iter()
            .zip(flow)
            .map(|(x, r)| x - self.step * r)
            .collect::<Vec<_>>();
        let target = self.projection.equality_action(&predictor)?;
        Ok((predictor, target))
    }
    pub fn try_evaluate_cached(
        &self,
        state: &[f64],
        physical_residual: &[f64],
        previous: &ConeProjection,
    ) -> CaeResult<Option<ConeResidual>> {
        let (predictor, target) = self.predictor(state, physical_residual)?;
        Ok(self
            .projection
            .try_project_cached(&predictor, &target, previous)?
            .map(|projection| self.with_projection(state, projection)))
    }
    fn with_projection(&self, state: &[f64], projection: ConeProjection) -> ConeResidual {
        let residual = state
            .iter()
            .zip(&projection.state)
            .map(|(x, p)| x - p)
            .collect();
        ConeResidual {
            residual,
            projection,
            problem: self.clone(),
        }
    }
    pub fn evaluate(
        &self,
        state: &[f64],
        physical_residual: &[f64],
        feasible_initializer: &[f64],
    ) -> CaeResult<ConeResidual> {
        let (predictor, target) = self.predictor(state, physical_residual)?;
        let projection = self
            .projection
            .project(&predictor, &target, feasible_initializer)?;
        Ok(self.with_projection(state, projection))
    }
}
impl ConeResidual {
    pub fn jvp(
        &self,
        state_direction: &[f64],
        physical_residual_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.jvp_mode(state_direction, physical_residual_direction, false)
    }
    pub fn selected_kkt_jvp(
        &self,
        state_direction: &[f64],
        physical_residual_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.jvp_mode(state_direction, physical_residual_direction, true)
    }
    fn jvp_mode(
        &self,
        state_direction: &[f64],
        physical_residual_direction: &[f64],
        selected: bool,
    ) -> CaeResult<Vec<f64>> {
        if state_direction.len() != self.problem.dimension()
            || state_direction.iter().any(|v| !v.is_finite())
        {
            return Err(fail("invalid cone residual state direction"));
        }
        let flow = self.problem.inverse_action(physical_residual_direction)?;
        let dp = state_direction
            .iter()
            .zip(flow)
            .map(|(x, r)| x - self.problem.step * r)
            .collect::<Vec<_>>();
        let target = self.problem.projection.equality_action(&dp)?;
        let projected = if selected {
            self.projection.selected_kkt_jvp(&dp, &target)?
        } else {
            self.projection.jvp(&dp, &target)?
        };
        Ok(state_direction
            .iter()
            .zip(projected)
            .map(|(x, p)| x - p)
            .collect())
    }
    pub fn vjp(&self, bar: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.vjp_mode(bar, false)
    }
    pub fn selected_kkt_vjp(&self, bar: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.vjp_mode(bar, true)
    }
    fn vjp_mode(&self, bar: &[f64], selected: bool) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        if bar.len() != self.problem.dimension() || bar.iter().any(|v| !v.is_finite()) {
            return Err(fail("invalid cone residual cotangent"));
        }
        let projected_bar = bar.iter().map(|v| -v).collect::<Vec<_>>();
        let (mut predictor, target) = if selected {
            self.projection.selected_kkt_vjp(&projected_bar)?
        } else {
            self.projection.vjp(&projected_bar)?
        };
        for (row, &weight) in self.problem.equality.iter().zip(&target) {
            for (j, v) in row.iter().enumerate() {
                predictor[j] += v * weight;
            }
        }
        let physical = self
            .problem
            .inverse_action(&predictor)?
            .into_iter()
            .map(|v| -self.problem.step * v)
            .collect();
        let state = bar.iter().zip(predictor).map(|(b, p)| b + p).collect();
        Ok((state, physical))
    }
}
