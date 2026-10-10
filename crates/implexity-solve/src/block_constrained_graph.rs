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

pub struct BlockPositiveConeGraph<'a, S, F> {
    inner: &'a S,
    blocks: Vec<ConeResidualProblem>,
    offsets: Vec<usize>,
    initializer: F,
    use_inner_preconditioner: bool,
    cached: RefCell<Option<(Vec<f64>, Vec<ConeResidual>)>>,
}
impl<'a, S: NonlinearGraphSystem, F: Fn(usize, &[f64]) -> CaeResult<Vec<f64>>>
    BlockPositiveConeGraph<'a, S, F>
{
    pub fn new(inner: &'a S, blocks: Vec<ConeResidualProblem>, initializer: F) -> CaeResult<Self> {
        let mut offsets = vec![0usize];
        for block in &blocks {
            let next = offsets
                .last()
                .unwrap()
                .checked_add(block.dimension())
                .ok_or_else(|| CaeError::contract("positive-cone block dimension overflow"))?;
            offsets.push(next);
        }
        if blocks.is_empty() || offsets.last().copied() != Some(inner.dimension()) {
            return Err(CaeError::contract(
                "positive-cone block graph dimension mismatch",
            ));
        }
        Ok(Self {
            inner,
            blocks,
            offsets,
            initializer,
            use_inner_preconditioner: false,
            cached: RefCell::new(None),
        })
    }
    pub fn with_inner_preconditioner(mut self) -> Self {
        self.use_inner_preconditioner = true;
        self
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
        if residual.len() != state.len() || residual.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "positive-cone block physical residual invalid",
            ));
        }
        let previous = self.cached.borrow();
        let mut values = Vec::with_capacity(self.blocks.len());
        for (i, block) in self.blocks.iter().enumerate() {
            let range = self.offsets[i]..self.offsets[i + 1];
            if let Some((_, previous)) = previous.as_ref() {
                if let Some(value) = block.try_evaluate_cached(
                    &state[range.clone()],
                    &residual[range.clone()],
                    &previous[i].projection,
                )? {
                    values.push(value);
                    continue;
                }
            }
            let (_, target) = block.predictor(&state[range.clone()], &residual[range.clone()])?;
            let seed = (self.initializer)(i, &target)?;
            values.push(block.evaluate(&state[range.clone()], &residual[range], &seed)?);
        }
        drop(previous);
        *self.cached.borrow_mut() = Some((state.to_vec(), values));
        Ok(())
    }
}
impl<S: NonlinearGraphSystem, F: Fn(usize, &[f64]) -> CaeResult<Vec<f64>>> NonlinearGraphSystem
    for BlockPositiveConeGraph<'_, S, F>
{
    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        Ok(self
            .cached
            .borrow()
            .as_ref()
            .unwrap()
            .1
            .iter()
            .flat_map(|v| v.residual.iter().copied())
            .collect())
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("positive-cone block direction invalid"));
        }
        let cache = self.cached.borrow();
        let values = &cache.as_ref().unwrap().1;
        if transpose {
            let mut state_bar = Vec::with_capacity(state.len());
            let mut physical_bar = Vec::with_capacity(state.len());
            for (i, value) in values.iter().enumerate() {
                let range = self.offsets[i]..self.offsets[i + 1];
                let (a, b) = value.vjp(&direction[range])?;
                state_bar.extend(a);
                physical_bar.extend(b);
            }
            let physical = self.inner.jacobian_product(state, &physical_bar, true)?;
            if physical.len() != state.len() {
                return Err(CaeError::contract("positive-cone block transpose shape"));
            }
            Ok(state_bar
                .into_iter()
                .zip(physical)
                .map(|(a, b)| a + b)
                .collect())
        } else {
            let physical = self.inner.jacobian_product(state, direction, false)?;
            if physical.len() != state.len() {
                return Err(CaeError::contract("positive-cone block tangent shape"));
            }
            let mut out = Vec::with_capacity(state.len());
            for (i, value) in values.iter().enumerate() {
                let range = self.offsets[i]..self.offsets[i + 1];
                out.extend(value.jvp(&direction[range.clone()], &physical[range])?);
            }
            Ok(out)
        }
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        if state.len() != self.dimension() || state.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(CaeError::contract("positive-cone block state invalid"));
        }
        self.inner.check_state(state)
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("positive-cone block step invalid"));
        }
        let mut step = self.inner.admitted_step(state, direction)?;
        for (&x, &d) in state.iter().zip(direction) {
            if d < 0. {
                step = step.min(-x / d);
            }
        }
        Ok(step)
    }
    fn reconstruct_newton_direction(
        &self,
        state: &[f64],
        residual: &[f64],
        direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        if residual.len() != state.len()
            || direction.len() != state.len()
            || residual.iter().chain(direction).any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "positive-cone exact Newton input invalid",
            ));
        }
        let cache = self.cached.borrow();
        let mut result = direction.to_vec();
        for (block, value) in cache.as_ref().unwrap().1.iter().enumerate() {
            for index in value.projection.strict_active_indices()? {
                let index = self.offsets[block] + index;
                result[index] = -state[index];
            }
        }
        Ok(result)
    }
    fn fallback_direction(&self, state: &[f64], residual: &[f64]) -> CaeResult<Option<Vec<f64>>> {
        if residual.len() != self.dimension() || residual.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "positive-cone fallback residual invalid",
            ));
        }
        let actual = self.residual(state)?;
        if actual
            .iter()
            .zip(residual)
            .any(|(a, b)| (a - b).abs() > 16. * f64::EPSILON * a.abs().max(b.abs()))
        {
            return Err(CaeError::contract(
                "positive-cone fallback residual mismatch",
            ));
        }
        Ok(Some(actual.into_iter().map(|v| -v).collect()))
    }
    fn preconditioner_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        if !self.use_inner_preconditioner {
            return Ok(direction.to_vec());
        }
        if direction.len() != self.dimension() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "cone block preconditioner direction invalid",
            ));
        }
        let input = if transpose {
            self.inner.preconditioner_product(state, direction, true)?
        } else {
            direction.to_vec()
        };
        let mut lifted = Vec::with_capacity(input.len());
        for (i, block) in self.blocks.iter().enumerate() {
            lifted.extend(
                block.physical_preconditioner_rhs(&input[self.offsets[i]..self.offsets[i + 1]])?,
            );
        }
        if transpose {
            Ok(lifted)
        } else {
            self.inner.preconditioner_product(state, &lifted, false)
        }
    }
}
