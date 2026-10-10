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

use crate::nonlinear_graph::NonlinearGraphSystem;
use implexity_core::{CaeError, CaeResult};
pub struct DiagonalGraph<'a, S: NonlinearGraphSystem + ?Sized> {
    system: &'a S,
    state_scale: Vec<f64>,
    residual_scale: Vec<f64>,
}
impl<'a, S: NonlinearGraphSystem + ?Sized> DiagonalGraph<'a, S> {
    pub fn new(system: &'a S, state_scale: Vec<f64>, residual_scale: Vec<f64>) -> CaeResult<Self> {
        let n = system.dimension();
        if n == 0
            || state_scale.len() != n
            || residual_scale.len() != n
            || state_scale
                .iter()
                .chain(&residual_scale)
                .any(|v| !v.is_finite() || *v <= 0.)
        {
            return Err(CaeError::contract("invalid diagonal graph scales"));
        }
        Ok(Self {
            system,
            state_scale,
            residual_scale,
        })
    }
    fn transform(&self, v: &[f64], scales: &[f64], divide: bool) -> CaeResult<Vec<f64>> {
        if v.len() != self.dimension() || v.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("invalid diagonal graph vector"));
        }
        let out = v
            .iter()
            .zip(scales)
            .map(|(v, s)| if divide { v / s } else { v * s })
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("diagonal graph scaling overflow"));
        }
        Ok(out)
    }
    pub fn to_inner_state(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.transform(state, &self.state_scale, false)
    }
    pub fn from_inner_state(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.transform(state, &self.state_scale, true)
    }
    pub fn inner_system(&self) -> &S {
        self.system
    }
}
impl<S: NonlinearGraphSystem + ?Sized> NonlinearGraphSystem for DiagonalGraph<'_, S> {
    fn dimension(&self) -> usize {
        self.system.dimension()
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        let x = self.to_inner_state(state)?;
        self.transform(&self.system.residual(&x)?, &self.residual_scale, true)
    }
    fn jacobian_product(&self, state: &[f64], v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let x = self.to_inner_state(state)?;
        if transpose {
            let v = self.transform(v, &self.residual_scale, true)?;
            self.transform(
                &self.system.jacobian_product(&x, &v, true)?,
                &self.state_scale,
                false,
            )
        } else {
            let v = self.transform(v, &self.state_scale, false)?;
            self.transform(
                &self.system.jacobian_product(&x, &v, false)?,
                &self.residual_scale,
                true,
            )
        }
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        self.system.check_state(&self.to_inner_state(state)?)
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.system.admitted_step(
            &self.to_inner_state(state)?,
            &self.transform(direction, &self.state_scale, false)?,
        )
    }
    fn newton_fixed_coordinates(
        &self,
        state: &[f64],
        residual: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        let x = self.to_inner_state(state)?;
        let r = self.transform(residual, &self.residual_scale, false)?;
        self.system
            .newton_fixed_coordinates(&x, &r)?
            .into_iter()
            .map(|(i, v)| {
                if i >= state.len() || !v.is_finite() {
                    return Err(CaeError::contract(
                        "diagonal fixed Newton coordinate invalid",
                    ));
                }
                Ok((
                    i,
                    if v == -x[i] {
                        -state[i]
                    } else {
                        v / self.state_scale[i]
                    },
                ))
            })
            .collect()
    }
    fn reconstruct_newton_direction(
        &self,
        state: &[f64],
        residual: &[f64],
        direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        let state = self.to_inner_state(state)?;
        let residual = self.transform(residual, &self.residual_scale, false)?;
        let inner_direction = self.transform(direction, &self.state_scale, false)?;
        let result =
            self.system
                .reconstruct_newton_direction(&state, &residual, &inner_direction)?;
        if result == inner_direction {
            return Ok(direction.to_vec());
        }
        self.transform(&result, &self.state_scale, true)
    }
    fn fallback_direction(&self, state: &[f64], residual: &[f64]) -> CaeResult<Option<Vec<f64>>> {
        let inner_state = self.to_inner_state(state)?;
        let inner_residual = self.transform(residual, &self.residual_scale, false)?;
        self.system
            .fallback_direction(&inner_state, &inner_residual)?
            .map(|direction| self.transform(&direction, &self.state_scale, true))
            .transpose()
    }
    fn preconditioner_product(
        &self,
        state: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        let x = self.to_inner_state(state)?;
        if transpose {
            let v = self.transform(v, &self.state_scale, true)?;
            self.transform(
                &self.system.preconditioner_product(&x, &v, true)?,
                &self.residual_scale,
                false,
            )
        } else {
            let v = self.transform(v, &self.residual_scale, false)?;
            self.transform(
                &self.system.preconditioner_product(&x, &v, false)?,
                &self.state_scale,
                true,
            )
        }
    }
}
