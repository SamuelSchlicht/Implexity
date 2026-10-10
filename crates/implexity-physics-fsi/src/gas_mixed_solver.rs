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

use crate::gas_mixed_network::{
    MixedGasLayout, MixedGasLocalPlan, MixedGasPolicy, physical_residual_scalar,
};
use crate::gas_multiscale_network::{MultiscaleNetworkInputs, MultiscaleNetworkTopology};
use crate::gas_transient_kinetic::velocity_equilibrium_density;
use crate::gas_vjp::{ReverseScalar, scoped_pullback};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::nonlinear_graph::NonlinearGraphSystem;

#[derive(Clone)]
pub struct MixedKineticSubdomainPolicy {
    pub maximum_population_count: usize,
    pub maximum_kinetic_unknowns: usize,
    pub freeze_within_solve: bool,
}
struct MixedKineticCache {
    state: Vec<f64>,
    factor: Option<std::sync::Arc<crate::gas_mixed_preconditioner::MixedKineticSubdomain>>,
}
enum MixedBlock {
    Continuum(implexity_linalg::dense::DenseLu),
    Kinetic(Option<crate::gas_bgk_preconditioner::BgkCollisionPreconditioner>),
}
struct MixedBlockCache {
    state: Vec<f64>,
    blocks: Vec<MixedBlock>,
}
struct MixedCoarseCache {
    state: Vec<f64>,
    coarse: Option<std::sync::Arc<crate::gas_mixed_preconditioner::MixedMomentCoarse>>,
}
pub struct MixedGasSystem {
    inputs: MultiscaleNetworkInputs<f64>,
    layout: MixedGasLayout,
    policy: MixedGasPolicy,
    previous: Vec<f64>,
    topology: MultiscaleNetworkTopology,
    active_cells: Vec<usize>,
    columns: Vec<usize>,
    solver_indices: Vec<Option<usize>>,
    scales: Vec<f64>,
    blocks: std::sync::Mutex<MixedBlockCache>,
    coarse: std::sync::Mutex<MixedCoarseCache>,
    kinetic_policy: Option<MixedKineticSubdomainPolicy>,
    kinetic: std::sync::Mutex<MixedKineticCache>,
}
impl MixedGasSystem {
    pub fn new(
        inputs: MultiscaleNetworkInputs<f64>,
        layout: MixedGasLayout,
        policy: MixedGasPolicy,
        previous: Vec<f64>,
        reference_density_kg_m3: f64,
    ) -> CaeResult<Self> {
        if !reference_density_kg_m3.is_finite()
            || reference_density_kg_m3 <= 0.0
            || previous.len() != layout.dimension()
        {
            return Err(CaeError::contract(
                "invalid mixed gas solver reference or checkpoint shape",
            ));
        }
        let topology = MultiscaleNetworkTopology::new_with_policy(&inputs, &policy.network)?;
        let weights = velocity_equilibrium_density(
            &inputs.gas.grid,
            1.,
            [0.; 3],
            inputs.gas.gas_rt_m2_s2,
            &policy.network.kinetic,
        )?;
        let speed = inputs.gas.gas_rt_m2_s2.sqrt();
        let mut columns = vec![];
        let mut scales = vec![];
        let mut active_cells = vec![];
        for (i, &volume) in inputs.gas.volume_m3.iter().enumerate() {
            if volume > 0.0 {
                let o = layout.cell_offset(i).ok_or_else(|| {
                    CaeError::contract("positive mixed capacity has no checkpoint slot")
                })?;
                active_cells.push(i);
                let mass = reference_density_kg_m3 * volume;
                if layout.continuum_cells()[i] {
                    for j in 0..4 {
                        columns.push(o + j);
                        scales.push(mass * if j == 0 { 1. } else { speed });
                    }
                } else {
                    for (k, &w) in weights.iter().enumerate() {
                        columns.push(o + k);
                        scales.push(mass * w);
                    }
                }
            }
        }
        if scales.iter().any(|s| !s.is_finite() || *s <= 0.0) {
            return Err(CaeError::contract(
                "mixed gas numerical scaling is not finite and positive",
            ));
        }
        let mut solver_indices = vec![None; layout.dimension()];
        for (j, &i) in columns.iter().enumerate() {
            solver_indices[i] = Some(j);
        }
        Ok(Self {
            inputs,
            layout,
            policy,
            previous,
            topology,
            active_cells,
            columns,
            solver_indices,
            scales,
            blocks: std::sync::Mutex::new(MixedBlockCache {
                state: vec![],
                blocks: vec![],
            }),
            kinetic_policy: None,
            kinetic: std::sync::Mutex::new(MixedKineticCache {
                state: vec![],
                factor: None,
            }),
            coarse: std::sync::Mutex::new(MixedCoarseCache {
                state: vec![],
                coarse: None,
            }),
        })
    }
    pub fn with_kinetic_subdomain(
        mut self,
        policy: Option<MixedKineticSubdomainPolicy>,
    ) -> CaeResult<Self> {
        if let Some(p) = &policy {
            let count = self
                .active_cells
                .iter()
                .filter(|&&c| !self.layout.continuum_cells()[c])
                .count()
                .checked_mul(self.layout.population_count())
                .ok_or_else(|| CaeError::contract("kinetic subdomain size overflow"))?;
            if p.maximum_population_count == 0
                || p.maximum_kinetic_unknowns == 0
                || self.layout.population_count() > p.maximum_population_count
                || count > p.maximum_kinetic_unknowns
            {
                return Err(CaeError::contract(
                    "mixed kinetic subdomain exceeds its declared numerical domain",
                ));
            }
        }
        self.kinetic_policy = policy;
        Ok(self)
    }
    pub fn pack(&self, physical: &[f64]) -> CaeResult<Vec<f64>> {
        if physical.len() != self.layout.dimension() {
            return Err(CaeError::contract("mixed physical state shape mismatch"));
        }
        Ok(self
            .columns
            .iter()
            .zip(&self.scales)
            .map(|(&i, &s)| physical[i] / s)
            .collect())
    }
    pub fn unpack(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        if state.len() != self.dimension() {
            return Err(CaeError::contract("mixed numerical state shape mismatch"));
        }
        let mut physical = vec![0.; self.layout.dimension()];
        for ((&i, &s), &y) in self.columns.iter().zip(&self.scales).zip(state) {
            physical[i] = s * y;
        }
        Ok(physical)
    }
    pub fn physical_result(
        &self,
        state: &[f64],
    ) -> CaeResult<crate::gas_mixed_network::MixedGasResidual> {
        physical_residual_scalar(
            &self.inputs,
            &self.layout,
            &self.unpack(state)?,
            &self.previous,
            &self.policy,
        )
    }
    pub fn pack_cotangent(&self, physical: &[f64]) -> CaeResult<Vec<f64>> {
        if physical.len() != self.layout.dimension() {
            return Err(CaeError::contract(
                "mixed physical cotangent shape mismatch",
            ));
        }
        Ok(self
            .columns
            .iter()
            .zip(&self.scales)
            .map(|(&i, &s)| physical[i] * s)
            .collect())
    }
    pub fn unpack_cotangent(&self, numerical: &[f64]) -> CaeResult<Vec<f64>> {
        if numerical.len() != self.dimension() {
            return Err(CaeError::contract(
                "mixed numerical cotangent shape mismatch",
            ));
        }
        let mut physical = vec![0.; self.layout.dimension()];
        for ((&i, &s), &q) in self.columns.iter().zip(&self.scales).zip(numerical) {
            physical[i] = q / s;
        }
        Ok(physical)
    }
    pub fn state_scales(&self) -> &[f64] {
        &self.scales
    }
}
impl NonlinearGraphSystem for MixedGasSystem {
    fn dimension(&self) -> usize {
        self.columns.len()
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        if state.len() != self.dimension() || state.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("mixed numerical state shape or value"));
        }
        for &cell in &self.active_cells {
            let o = self
                .layout
                .cell_offset(cell)
                .ok_or_else(|| CaeError::contract("missing mixed checkpoint slot"))?;
            if self.layout.continuum_cells()[cell] {
                let s = self.solver_indices[o]
                    .ok_or_else(|| CaeError::contract("missing mixed solver mass coordinate"))?;
                if state[s] <= 0.0 {
                    return Err(CaeError::contract(
                        "continuum gas mass must remain positive",
                    ));
                }
            } else {
                for k in 0..self.layout.population_count() {
                    let s = self.solver_indices[o + k]
                        .ok_or_else(|| CaeError::contract("missing mixed kinetic coordinate"))?;
                    if state[s] < 0.0 {
                        return Err(CaeError::contract(
                            "kinetic gas inventory must remain nonnegative",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|d| !d.is_finite()) {
            return Err(CaeError::contract("invalid mixed Newton direction"));
        }
        let mut alpha = 1.0f64;
        for &cell in &self.active_cells {
            let o = self
                .layout
                .cell_offset(cell)
                .ok_or_else(|| CaeError::contract("missing mixed cell"))?;
            let count = if self.layout.continuum_cells()[cell] {
                1
            } else {
                self.layout.population_count()
            };
            for k in 0..count {
                let j = self.solver_indices[o + k]
                    .ok_or_else(|| CaeError::contract("missing mixed active coordinate"))?;
                if direction[j] < 0.0 {
                    alpha = alpha.min(0.99 * (-state[j] / direction[j]));
                }
            }
        }
        Ok(alpha)
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        let physical = self.unpack(state)?;
        let mut out = vec![0.; self.dimension()];
        for &cell in &self.active_cells {
            let plan = MixedGasLocalPlan::new_prepared(
                &self.inputs,
                &self.layout,
                &self.policy,
                &self.topology,
                &[cell],
            )?;
            let current = plan
                .state_columns
                .iter()
                .map(|&g| physical[g])
                .collect::<Vec<_>>();
            let old = plan
                .state_columns
                .iter()
                .map(|&g| self.previous[g])
                .collect::<Vec<_>>();
            let result = plan.evaluate(&current, &old)?;
            for &(g, local) in &plan.output_columns {
                if let Some(j) = self.solver_indices[g] {
                    out[j] = result.inventory_residual[local] / self.scales[j];
                }
            }
        }
        Ok(out)
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        if direction.len() != self.dimension() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "mixed Jacobian direction shape or value",
            ));
        }
        let physical = self.unpack(state)?;
        if !transpose {
            let mut out = vec![0.; self.dimension()];
            for &cell in &self.active_cells {
                let plan = MixedGasLocalPlan::new_prepared(
                    &self.inputs,
                    &self.layout,
                    &self.policy,
                    &self.topology,
                    &[cell],
                )?;
                let current =
                    plan.state_columns
                        .iter()
                        .map(|&g| {
                            Dual::<1>::new(
                                physical[g],
                                [self.solver_indices[g]
                                    .map_or(0., |j| self.scales[j] * direction[j])],
                            )
                        })
                        .collect::<Vec<_>>();
                let old = plan
                    .state_columns
                    .iter()
                    .map(|&g| Dual::<1>::constant(self.previous[g]))
                    .collect::<Vec<_>>();
                let result = plan.evaluate(&current, &old)?;
                for &(g, local) in &plan.output_columns {
                    if let Some(j) = self.solver_indices[g] {
                        out[j] = result.inventory_residual[local].eps[0] / self.scales[j];
                    }
                }
            }
            return Ok(out);
        }
        let mut out = vec![0.; self.dimension()];
        for &cell in &self.active_cells {
            let o = self
                .layout
                .cell_offset(cell)
                .ok_or_else(|| CaeError::contract("missing mixed output cell"))?;
            let count = if self.layout.continuum_cells()[cell] {
                4
            } else {
                self.layout.population_count()
            };
            if (0..count).all(|k| direction[self.solver_indices[o + k].unwrap()] == 0.0) {
                continue;
            }
            let plan = MixedGasLocalPlan::new_prepared(
                &self.inputs,
                &self.layout,
                &self.policy,
                &self.topology,
                &[cell],
            )?;
            let values = plan
                .state_columns
                .iter()
                .map(|&i| physical[i])
                .collect::<Vec<_>>();
            let previous = plan
                .state_columns
                .iter()
                .map(|&i| ReverseScalar::from_f64(self.previous[i]))
                .collect::<Vec<_>>();
            let bars = plan
                .output_columns
                .iter()
                .filter_map(|&(global, local)| {
                    self.solver_indices[global].map(|j| (local, direction[j] / self.scales[j]))
                })
                .collect::<Vec<_>>();
            let pulled = scoped_pullback(&values, |variables| {
                let r = plan.evaluate(variables, &previous)?;
                Ok(bars.iter().fold(ReverseScalar::zero(), |s, (i, b)| {
                    s + r.inventory_residual[*i] * *b
                }))
            })?;
            for (&global, bar) in plan.state_columns.iter().zip(pulled) {
                if let Some(j) = self.solver_indices[global] {
                    out[j] += bar * self.scales[j];
                }
            }
        }
        Ok(out)
    }
    fn preconditioner_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        let coarse = {
            let mut cache = self
                .coarse
                .lock()
                .map_err(|_| CaeError::contract("mixed moment preconditioner lock failed"))?;
            if cache.state.is_empty()
                || (cache.state != state
                    && !self
                        .kinetic_policy
                        .as_ref()
                        .is_some_and(|p| p.freeze_within_solve))
            {
                cache.coarse = Some(std::sync::Arc::new(
                    crate::gas_mixed_preconditioner::MixedMomentCoarse::new(
                        &self.inputs,
                        &self.layout,
                        &self.policy,
                        &self.previous,
                        &self.unpack(state)?,
                        &self.columns,
                        &self.scales,
                    )?,
                ));
                cache.state = state.to_vec();
            }
            cache
                .coarse
                .as_ref()
                .ok_or_else(|| CaeError::contract("missing mixed moment preconditioner"))?
                .clone()
        };
        if !transpose {
            let y = self.block_preconditioner_product(state, direction, false)?;
            let jy = self.jacobian_product(state, &y, false)?;
            let r = direction
                .iter()
                .zip(jy)
                .map(|(v, j)| v - j)
                .collect::<Vec<_>>();
            let c = coarse.lift(&coarse.solve(&coarse.restrict(&r, false)?, false)?, false)?;
            Ok(y.iter().zip(c).map(|(y, c)| y + c).collect())
        } else {
            let c = coarse.restrict(&coarse.solve(&coarse.lift(direction, true)?, true)?, true)?;
            let jtc = self.jacobian_product(state, &c, true)?;
            let r = direction
                .iter()
                .zip(jtc)
                .map(|(v, j)| v - j)
                .collect::<Vec<_>>();
            let y = self.block_preconditioner_product(state, &r, true)?;
            Ok(y.iter().zip(c).map(|(y, c)| y + c).collect())
        }
    }
}
impl MixedGasSystem {
    pub(crate) fn block_preconditioner_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        if direction.len() != self.dimension() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("mixed preconditioner direction"));
        }
        if let Some(policy) = &self.kinetic_policy {
            if self
                .active_cells
                .iter()
                .any(|&c| !self.layout.continuum_cells()[c])
            {
                let factor = {
                    let mut cache = self.kinetic.lock().map_err(|_| {
                        CaeError::contract("mixed kinetic preconditioner lock failed")
                    })?;
                    if cache.state.is_empty()
                        || (cache.state != state && !policy.freeze_within_solve)
                    {
                        cache.factor = Some(std::sync::Arc::new(
                            crate::gas_mixed_preconditioner::MixedKineticSubdomain::new(
                                &self.inputs,
                                &self.layout,
                                &self.policy,
                                &self.previous,
                                &self.unpack(state)?,
                                &self.columns,
                                &self.scales,
                                policy.maximum_population_count,
                                policy.maximum_kinetic_unknowns,
                            )?,
                        ));
                        cache.state = state.to_vec();
                    }
                    cache
                        .factor
                        .as_ref()
                        .ok_or_else(|| CaeError::contract("missing kinetic subdomain factor"))?
                        .clone()
                };
                return factor.apply(direction, transpose);
            }
        }
        let mut cache = self
            .blocks
            .lock()
            .map_err(|_| CaeError::contract("mixed preconditioner lock failed"))?;
        if cache.state != state {
            let physical = self.unpack(state)?;
            let mut blocks = vec![];
            for &cell in &self.active_cells {
                let o = self
                    .layout
                    .cell_offset(cell)
                    .ok_or_else(|| CaeError::contract("missing mixed block"))?;
                if self.layout.continuum_cells()[cell] {
                    let plan = MixedGasLocalPlan::new_prepared(
                        &self.inputs,
                        &self.layout,
                        &self.policy,
                        &self.topology,
                        &[cell],
                    )?;
                    let current = plan
                        .state_columns
                        .iter()
                        .map(|&g| {
                            let mut q = Dual::<4>::constant(physical[g]);
                            if g >= o && g < o + 4 {
                                q.eps[g - o] = self.scales[self.solver_indices[g].unwrap()];
                            }
                            q
                        })
                        .collect::<Vec<_>>();
                    let previous = plan
                        .state_columns
                        .iter()
                        .map(|&g| Dual::<4>::constant(self.previous[g]))
                        .collect::<Vec<_>>();
                    let r = plan.evaluate(&current, &previous)?;
                    let mut matrix = vec![0.; 16];
                    for &(global, local) in &plan.output_columns {
                        if global >= o && global < o + 4 {
                            let scale = self.scales[self.solver_indices[global].unwrap()];
                            for j in 0..4 {
                                matrix[(global - o) * 4 + j] =
                                    r.inventory_residual[local].eps[j] / scale;
                            }
                        }
                    }
                    let m = implexity_linalg::dense::DenseMatrix::new(4, 4, matrix)
                        .map_err(|e| CaeError::contract(e.to_string()))?;
                    let lu = implexity_linalg::dense::DenseLu::new(&m)
                        .map_err(|e| CaeError::contract(e.to_string()))?;
                    blocks.push(MixedBlock::Continuum(lu));
                } else {
                    let q = &physical[o..o + self.layout.population_count()];
                    blocks.push(MixedBlock::Kinetic(if q.iter().all(|q| *q == 0.) {
                        None
                    } else {
                        Some(
                            crate::gas_bgk_preconditioner::BgkCollisionPreconditioner::new(
                                &self.inputs.gas.grid,
                                q,
                                self.inputs.gas.volume_m3[cell],
                                self.inputs.gas.gas_rt_m2_s2,
                                self.inputs.gas.viscosity_pa_s,
                                self.inputs.gas.step_s,
                                &self.policy.network.kinetic,
                            )?,
                        )
                    }));
                }
            }
            cache.blocks = blocks;
            cache.state = state.to_vec();
        }
        let mut out = vec![0.; self.dimension()];
        for (&cell, block) in self.active_cells.iter().zip(&cache.blocks) {
            let o = self
                .layout
                .cell_offset(cell)
                .ok_or_else(|| CaeError::contract("missing mixed block output"))?;
            let count = if self.layout.continuum_cells()[cell] {
                4
            } else {
                self.layout.population_count()
            };
            let indices = (0..count)
                .map(|k| self.solver_indices[o + k].unwrap())
                .collect::<Vec<_>>();
            match block {
                MixedBlock::Continuum(lu) => {
                    let b = indices.iter().map(|&j| direction[j]).collect::<Vec<_>>();
                    let x = lu
                        .solve(&b, 1, transpose)
                        .map_err(|e| CaeError::contract(e.to_string()))?;
                    for (&j, x) in indices.iter().zip(x) {
                        out[j] = x;
                    }
                }
                MixedBlock::Kinetic(block) => {
                    let b = indices
                        .iter()
                        .map(|&j| {
                            if transpose {
                                direction[j] / self.scales[j]
                            } else {
                                direction[j] * self.scales[j]
                            }
                        })
                        .collect::<Vec<_>>();
                    let x = if let Some(block) = block {
                        block.apply(&b, transpose)?
                    } else {
                        b
                    };
                    for (&j, x) in indices.iter().zip(x) {
                        out[j] = if transpose {
                            x * self.scales[j]
                        } else {
                            x / self.scales[j]
                        };
                    }
                }
            }
        }
        Ok(out)
    }
}
