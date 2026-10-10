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

use crate::nonlinear_graph::{
    solve_linear, NonlinearGraphLinearSolution, NonlinearGraphPolicy, NonlinearGraphSystem,
};
use implexity_core::{CaeError, CaeResult};
pub struct FixedCoordinateLinearSystem<'a, S: ?Sized> {
    inner: &'a S,
    mask: Vec<bool>,
}
impl<'a, S: NonlinearGraphSystem + ?Sized> FixedCoordinateLinearSystem<'a, S> {
    pub fn new(inner: &'a S, indices: &[usize]) -> CaeResult<Self> {
        let mut mask = vec![false; inner.dimension()];
        for &i in indices {
            if i >= mask.len() || mask[i] {
                return Err(CaeError::contract(
                    "invalid fixed Newton coordinate ownership",
                ));
            }
            mask[i] = true;
        }
        Ok(Self { inner, mask })
    }
    fn product(
        &self,
        state: &[f64],
        v: &[f64],
        transpose: bool,
        preconditioner: bool,
    ) -> CaeResult<Vec<f64>> {
        if v.len() != self.mask.len() || v.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("fixed Newton product input invalid"));
        }
        let mut input = v.to_vec();
        for (i, m) in self.mask.iter().enumerate() {
            if *m {
                input[i] = 0.;
            }
        }
        let mut result = if preconditioner {
            self.inner
                .preconditioner_product(state, &input, transpose)?
        } else {
            self.inner.jacobian_product(state, &input, transpose)?
        };
        if result.len() != v.len() || result.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("fixed Newton product output invalid"));
        }
        for (i, m) in self.mask.iter().enumerate() {
            if *m {
                result[i] = v[i];
            }
        }
        Ok(result)
    }
}
impl<S: NonlinearGraphSystem + ?Sized> NonlinearGraphSystem for FixedCoordinateLinearSystem<'_, S> {
    fn dimension(&self) -> usize {
        self.mask.len()
    }
    fn residual(&self, _: &[f64]) -> CaeResult<Vec<f64>> {
        Err(CaeError::contract(
            "fixed coordinate linear system has no nonlinear residual",
        ))
    }
    fn jacobian_product(&self, x: &[f64], v: &[f64], t: bool) -> CaeResult<Vec<f64>> {
        self.product(x, v, t, false)
    }
    fn preconditioner_product(&self, x: &[f64], v: &[f64], t: bool) -> CaeResult<Vec<f64>> {
        self.product(x, v, t, true)
    }
    fn check_state(&self, x: &[f64]) -> CaeResult<()> {
        self.inner.check_state(x)
    }
    fn admitted_step(&self, _: &[f64], _: &[f64]) -> CaeResult<f64> {
        Err(CaeError::contract(
            "fixed coordinate linear system has no nonlinear step",
        ))
    }
}
pub fn solve_newton_with_fixed_coordinates<S: NonlinearGraphSystem + ?Sized>(
    system: &S,
    state: &[f64],
    rhs: &[f64],
    coordinates: &[(usize, f64)],
    policy: &NonlinearGraphPolicy,
) -> CaeResult<NonlinearGraphLinearSolution> {
    if coordinates.is_empty() {
        return solve_linear(system, state, rhs, false, policy);
    }
    let n = system.dimension();
    if state.len() != n || rhs.len() != n || state.iter().chain(rhs).any(|v| !v.is_finite()) {
        return Err(CaeError::contract("fixed Newton solve input invalid"));
    }
    let restricted = FixedCoordinateLinearSystem::new(
        system,
        &coordinates.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
    )?;
    let mut prescribed = vec![0.; n];
    for &(i, v) in coordinates {
        if !v.is_finite() {
            return Err(CaeError::contract("nonfinite fixed Newton direction"));
        }
        prescribed[i] = v;
    }
    let action = system.jacobian_product(state, &prescribed, false)?;
    if action.len() != n || action.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("fixed Newton prescribed action invalid"));
    }
    let mut reduced_rhs = rhs
        .iter()
        .zip(&action)
        .map(|(r, a)| r - a)
        .collect::<Vec<_>>();
    for &(i, _) in coordinates {
        reduced_rhs[i] = 0.;
    }
    let solved = solve_linear(&restricted, state, &reduced_rhs, false, policy)?;
    let mut direction = prescribed
        .iter()
        .zip(&solved.direction)
        .map(|(p, d)| p + d)
        .collect::<Vec<_>>();
    for &(i, v) in coordinates {
        direction[i] = v;
    }
    let actual = system.jacobian_product(state, &direction, false)?;
    if actual.len() != n || actual.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("fixed Newton actual action invalid"));
    }
    let norm = |v: &[f64]| v.iter().fold(0f64, |a, v| a.hypot(*v));
    let defect = norm(
        &actual
            .iter()
            .zip(rhs)
            .map(|(a, b)| a - b)
            .collect::<Vec<_>>(),
    );
    let bound = policy
        .linear_absolute_tolerance
        .max(policy.linear_relative_tolerance * norm(rhs));
    if !defect.is_finite() || !bound.is_finite() || defect > bound {
        return Err(CaeError::convergence(format!(
            "fixed Newton original linear residual {defect:e} exceeds {bound:e}"
        )));
    }
    Ok(NonlinearGraphLinearSolution {
        direction,
        residual_norm: defect,
        iterations: solved.iterations,
    })
}
