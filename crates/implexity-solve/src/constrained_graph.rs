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

use crate::cone_residual::{ConeResidual, ConeResidualProblem};
use crate::nonlinear_graph::NonlinearGraphSystem;
use implexity_core::{CaeError, CaeResult};
use std::cell::RefCell;

pub struct PositiveConeGraph<'a, S, F> {
    inner: &'a S,
    cone: ConeResidualProblem,
    initializer: F,
    cached: RefCell<Option<(Vec<f64>, ConeResidual)>>,
}
impl<'a, S: NonlinearGraphSystem, F: Fn(&[f64]) -> CaeResult<Vec<f64>>>
    PositiveConeGraph<'a, S, F>
{
    pub fn new(inner: &'a S, cone: ConeResidualProblem, initializer: F) -> CaeResult<Self> {
        if inner.dimension() != cone.dimension() {
            return Err(CaeError::contract("positive-cone graph dimension mismatch"));
        }
        Ok(Self {
            inner,
            cone,
            initializer,
            cached: RefCell::new(None),
        })
    }
    pub fn physical_residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.inner.residual(state)
    }
    fn evaluate(&self, state: &[f64]) -> CaeResult<()> {
        if self
            .cached
            .borrow()
            .as_ref()
            .is_some_and(|(x, _)| x.as_slice() == state)
        {
            return Ok(());
        }
        self.check_state(state)?;
        let residual = self.inner.residual(state)?;
        let (_, target) = self.cone.predictor(state, &residual)?;
        let seed = (self.initializer)(&target)?;
        let value = self.cone.evaluate(state, &residual, &seed)?;
        *self.cached.borrow_mut() = Some((state.to_vec(), value));
        Ok(())
    }
}
impl<S: NonlinearGraphSystem, F: Fn(&[f64]) -> CaeResult<Vec<f64>>> NonlinearGraphSystem
    for PositiveConeGraph<'_, S, F>
{
    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        Ok(self.cached.borrow().as_ref().unwrap().1.residual.clone())
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        let cache = self.cached.borrow();
        let value = &cache.as_ref().unwrap().1;
        if transpose {
            let (state_bar, physical_bar) = value.vjp(direction)?;
            let physical = self.inner.jacobian_product(state, &physical_bar, true)?;
            Ok(state_bar
                .into_iter()
                .zip(physical)
                .map(|(a, b)| a + b)
                .collect())
        } else {
            let physical = self.inner.jacobian_product(state, direction, false)?;
            value.jvp(direction, &physical)
        }
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        if state.len() != self.dimension() || state.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(CaeError::contract("positive-cone graph state is invalid"));
        }
        self.inner.check_state(state)
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "positive-cone graph direction is invalid",
            ));
        }
        let mut step = self.inner.admitted_step(state, direction)?;
        for (&x, &d) in state.iter().zip(direction) {
            if d < 0. {
                step = step.min(-x / d);
            }
        }
        Ok(step)
    }
}
