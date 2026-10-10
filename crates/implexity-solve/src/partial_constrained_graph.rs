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
use std::{cell::RefCell, ops::Range};

pub struct PartialPositiveConeGraph<'a, S, F> {
    inner: &'a S,
    blocks: Vec<(Range<usize>, ConeResidualProblem)>,
    initializer: F,
    use_inner_preconditioner: bool,
    selected_kkt_newton: bool,
    cached: RefCell<Option<(Vec<f64>, Vec<f64>, Vec<ConeResidual>)>>,
}
impl<'a, S: NonlinearGraphSystem, F: Fn(usize, &[f64]) -> CaeResult<Vec<f64>>>
    PartialPositiveConeGraph<'a, S, F>
{
    pub fn new(
        inner: &'a S,
        blocks: Vec<(Range<usize>, ConeResidualProblem)>,
        initializer: F,
    ) -> CaeResult<Self> {
        let mut ranges = Vec::with_capacity(blocks.len());
        for (range, block) in &blocks {
            if range.start >= range.end
                || range.end > inner.dimension()
                || range.end - range.start != block.dimension()
            {
                return Err(CaeError::contract("partial positive-cone range invalid"));
            }
            ranges.push(range.clone());
        }
        ranges.sort_by_key(|range| range.start);
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(CaeError::contract("partial positive-cone ranges overlap"));
        }
        Ok(Self {
            inner,
            blocks,
            initializer,
            use_inner_preconditioner: false,
            selected_kkt_newton: false,
            cached: RefCell::new(None),
        })
    }
    pub fn with_inner_preconditioner(mut self) -> Self {
        self.use_inner_preconditioner = true;
        self
    }
    pub fn with_selected_kkt_newton(mut self) -> Self {
        self.selected_kkt_newton = true;
        self
    }
    pub fn physical_residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.inner.residual(state)
    }
    pub fn physical_residual_jvp(
        &self,
        state: &[f64],
        physical_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        if physical_direction.len() != self.dimension()
            || physical_direction.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "partial cone physical direction invalid",
            ));
        }
        let mut result = physical_direction.to_vec();
        let cache = self.cached.borrow();
        for ((range, _), value) in self.blocks.iter().zip(&cache.as_ref().unwrap().2) {
            result[range.clone()].copy_from_slice(&value.jvp(
                &vec![0.; range.end - range.start],
                &physical_direction[range.clone()],
            )?);
        }
        Ok(result)
    }
    pub fn physical_residual_vjp(&self, state: &[f64], graph_bar: &[f64]) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        if graph_bar.len() != self.dimension() || graph_bar.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial cone physical cotangent invalid",
            ));
        }
        let mut result = graph_bar.to_vec();
        let cache = self.cached.borrow();
        for ((range, _), value) in self.blocks.iter().zip(&cache.as_ref().unwrap().2) {
            let (_, physical_bar) = value.vjp(&graph_bar[range.clone()])?;
            result[range.clone()].copy_from_slice(&physical_bar);
        }
        Ok(result)
    }
    fn evaluate(&self, state: &[f64]) -> CaeResult<()> {
        if self
            .cached
            .borrow()
            .as_ref()
            .is_some_and(|(x, _, _)| x.as_slice() == state)
        {
            return Ok(());
        }
        self.check_state(state)?;
        let residual = self.inner.residual(state)?;
        if residual.len() != state.len() || residual.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial positive-cone physical residual invalid",
            ));
        }
        let previous = self.cached.borrow();
        let mut values = Vec::with_capacity(self.blocks.len());
        for (i, (range, block)) in self.blocks.iter().enumerate() {
            let range = range.clone();
            if let Some((_, _, previous)) = previous.as_ref() {
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
        *self.cached.borrow_mut() = Some((state.to_vec(), residual, values));
        Ok(())
    }
}
impl<S: NonlinearGraphSystem, F: Fn(usize, &[f64]) -> CaeResult<Vec<f64>>> NonlinearGraphSystem
    for PartialPositiveConeGraph<'_, S, F>
{
    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        let cache = self.cached.borrow();
        let (_, physical, values) = cache.as_ref().unwrap();
        let mut result = physical.clone();
        for ((range, _), value) in self.blocks.iter().zip(values) {
            result[range.clone()].copy_from_slice(&value.residual);
        }
        Ok(result)
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.evaluate(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial positive-cone direction invalid",
            ));
        }
        let cache = self.cached.borrow();
        let values = &cache.as_ref().unwrap().2;
        if transpose {
            let mut state_bar = vec![0.; state.len()];
            let mut physical_bar = direction.to_vec();
            for ((range, _), value) in self.blocks.iter().zip(values) {
                let (a, b) = if self.selected_kkt_newton {
                    value.selected_kkt_vjp(&direction[range.clone()])?
                } else {
                    value.vjp(&direction[range.clone()])?
                };
                state_bar[range.clone()].copy_from_slice(&a);
                physical_bar[range.clone()].copy_from_slice(&b);
            }
            let physical = self.inner.jacobian_product(state, &physical_bar, true)?;
            if physical.len() != state.len() || physical.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract(
                    "partial positive-cone transpose invalid",
                ));
            }
            Ok(state_bar
                .into_iter()
                .zip(physical)
                .map(|(a, b)| a + b)
                .collect())
        } else {
            let mut physical = self.inner.jacobian_product(state, direction, false)?;
            if physical.len() != state.len() || physical.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("partial positive-cone tangent invalid"));
            }
            for ((range, _), value) in self.blocks.iter().zip(values) {
                let block = if self.selected_kkt_newton {
                    value.selected_kkt_jvp(&direction[range.clone()], &physical[range.clone()])?
                } else {
                    value.jvp(&direction[range.clone()], &physical[range.clone()])?
                };
                physical[range.clone()].copy_from_slice(&block);
            }
            Ok(physical)
        }
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        if state.len() != self.dimension()
            || state.iter().any(|v| !v.is_finite())
            || self
                .blocks
                .iter()
                .any(|(range, _)| state[range.clone()].iter().any(|v| *v < 0.))
        {
            return Err(CaeError::contract("partial positive-cone state invalid"));
        }
        self.inner.check_state(state)
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("partial positive-cone step invalid"));
        }
        let mut step = self.inner.admitted_step(state, direction)?;
        for (range, _) in &self.blocks {
            for (&x, &d) in state[range.clone()].iter().zip(&direction[range.clone()]) {
                if d < 0. {
                    step = step.min(-x / d);
                }
            }
        }
        Ok(step)
    }
    fn newton_fixed_coordinates(
        &self,
        state: &[f64],
        residual: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        self.evaluate(state)?;
        if residual.len() != state.len() || residual.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial cone fixed Newton input invalid",
            ));
        }
        let cache = self.cached.borrow();
        let mut result = self
            .inner
            .newton_fixed_coordinates(state, &cache.as_ref().unwrap().1)?;
        for ((range, _), value) in self.blocks.iter().zip(&cache.as_ref().unwrap().2) {
            let active = if self.selected_kkt_newton {
                value.projection.selected_kkt_active_indices()
            } else {
                value.projection.strict_active_indices()?
            };
            for i in active {
                let i = range.start + i;
                result.push((i, -state[i]));
            }
        }
        Ok(result)
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
        let mut result = self.inner.reconstruct_newton_direction(
            state,
            &cache.as_ref().unwrap().1,
            direction,
        )?;
        if result.len() != state.len() || result.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial positive-cone reconstructed direction invalid",
            ));
        }
        for ((range, _), value) in self.blocks.iter().zip(&cache.as_ref().unwrap().2) {
            let active = if self.selected_kkt_newton {
                value.projection.selected_kkt_active_indices()
            } else {
                value.projection.strict_active_indices()?
            };
            for index in active {
                let index = range.start + index;
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
        if direction.len() != self.dimension() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial cone preconditioner direction invalid",
            ));
        }
        if !self.use_inner_preconditioner {
            return Ok(direction.to_vec());
        }
        let input = if transpose {
            self.inner.preconditioner_product(state, direction, true)?
        } else {
            direction.to_vec()
        };
        if input.len() != self.dimension() || input.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial positive-cone inner preconditioner invalid",
            ));
        }
        let mut lifted = input.clone();
        for (range, block) in &self.blocks {
            lifted[range.clone()]
                .copy_from_slice(&block.physical_preconditioner_rhs(&input[range.clone()])?);
        }
        let result = if transpose {
            lifted
        } else {
            self.inner.preconditioner_product(state, &lifted, false)?
        };
        if result.len() != self.dimension() || result.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "partial positive-cone preconditioner output invalid",
            ));
        }
        Ok(result)
    }
}
