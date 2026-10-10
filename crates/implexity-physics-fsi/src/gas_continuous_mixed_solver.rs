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

use crate::gas_continuous_layout::ContinuousGasRepresentation;
use crate::gas_continuous_mixed::{ContinuousMixedNetwork, ContinuousMixedResult};
use crate::gas_quadratic_network_solver::{
    CollisionInverse, QuadraticGasNetworkSolvePolicy, QuadraticGasNumericalScaling,
};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::gas_vjp::{scoped_pullback, ReverseScalar};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::nonlinear_graph::NonlinearGraphSystem;
use std::cell::RefCell;
use std::ops::Range;

pub struct ContinuousMixedState<S = f64> {
    pub previous_inventory: Vec<S>,
    pub port_coefficient_density: Vec<Vec<S>>,
    pub gas_rt_m2_s2: S,
    pub molecular_viscosity_pa_s: S,
}
pub struct ContinuousMixedStep {
    pub inventory: Vec<f64>,
    pub evaluated: ContinuousMixedResult<f64>,
    pub scaled_kkt_residual_norm: f64,
    pub physical_residual_norm: f64,
    pub maximum_relative_physical_moment_residual: f64,
    pub newton_iterations: usize,
    pub linear_iterations: usize,
}
pub struct ContinuousMixedSystem {
    network: ContinuousMixedNetwork<f64>,
    tangent: ContinuousMixedNetwork<Dual<1>>,
    reverse: ContinuousMixedNetwork<ReverseScalar>,
    authored: ContinuousMixedState,
    active_cells: Vec<usize>,
    active_ranges: Vec<Option<Range<usize>>>,
    columns: Vec<usize>,
    supports: Vec<Vec<usize>>,
    inverse: RefCell<Option<(Vec<f64>, Vec<(usize, CollisionInverse)>)>>,
    transport_policy: Option<(
        crate::gas_continuous_preconditioner::ContinuousTransportPreconditionerPolicy,
        Vec<f64>,
    )>,
    transport_inverse: RefCell<
        Option<(
            Vec<f64>,
            crate::gas_continuous_preconditioner::ContinuousTransportPreconditioner,
        )>,
    >,
}
pub type ContinuousMixedConeGraph<'a> =
    implexity_solve::partial_constrained_graph::PartialPositiveConeGraph<
        'a,
        ContinuousMixedSystem,
        Box<dyn Fn(usize, &[f64]) -> CaeResult<Vec<f64>> + 'a>,
    >;
pub struct ContinuousMixedConstrained<'a> {
    graph: ContinuousMixedConeGraph<'a>,
    scales: Vec<f64>,
}
impl<'a> ContinuousMixedConstrained<'a> {
    pub fn with_selected_kkt_newton(mut self) -> Self {
        self.graph = self.graph.with_selected_kkt_newton();
        self
    }
    pub fn physical_graph(&self) -> &ContinuousMixedConeGraph<'a> {
        &self.graph
    }
    pub fn scales(&self) -> &[f64] {
        &self.scales
    }
    pub fn scaled_graph(
        &self,
    ) -> CaeResult<implexity_solve::diagonal_graph::DiagonalGraph<'_, ContinuousMixedConeGraph<'a>>>
    {
        implexity_solve::diagonal_graph::DiagonalGraph::new(
            &self.graph,
            self.scales.clone(),
            self.scales.clone(),
        )
    }
}
impl ContinuousMixedSystem {
    pub fn new(
        network: ContinuousMixedNetwork<f64>,
        authored: ContinuousMixedState,
    ) -> CaeResult<Self> {
        if !authored.gas_rt_m2_s2.is_finite() || authored.gas_rt_m2_s2 <= 0.
            || !authored.molecular_viscosity_pa_s.is_finite() || authored.molecular_viscosity_pa_s <= 0.
            || authored.port_coefficient_density.iter().any(|a| a.len() != network.layout().basis().coefficient_count() || a.iter().any(|v| !v.is_finite() || *v < 0.))
            || network.prepared().geometry().faces.iter().any(|f| matches!(f.boundary, Some(crate::gas_quadratic_network::QuadraticGasBoundary::Port { port }) if port >= authored.port_coefficient_density.len())) {
            return Err(CaeError::contract("mixed continuous authored gas properties or port density invalid"));
        }
        network.layout().validate(
            &authored.previous_inventory,
            &network.prepared().geometry().previous_volume_m3,
        )?;
        let mut active_ranges = vec![None; network.layout().slots()];
        let mut active_cells = Vec::new();
        let mut columns = Vec::new();
        for (cell, volume) in network.prepared().geometry().volume_m3.iter().enumerate() {
            if *volume > 0. {
                let start = columns.len();
                columns.extend(network.layout().range(cell)?);
                active_ranges[cell] = Some(start..columns.len());
                active_cells.push(cell);
            }
        }
        let supports = active_cells
            .iter()
            .map(|c| network.dependency_cells(&[*c]))
            .collect::<CaeResult<Vec<_>>>()?;
        Ok(Self {
            tangent: network.constant_lift()?,
            reverse: network.constant_lift()?,
            network,
            authored,
            active_cells,
            active_ranges,
            columns,
            supports,
            inverse: RefCell::new(None),
            transport_policy: None,
            transport_inverse: RefCell::new(None),
        })
    }
    pub fn with_transport_preconditioner(
        mut self,
        policy: crate::gas_continuous_preconditioner::ContinuousTransportPreconditionerPolicy,
        scales: Vec<f64>,
    ) -> CaeResult<Self> {
        if scales.len() != self.columns.len() || scales.iter().any(|v| !v.is_finite() || *v <= 0.) {
            return Err(CaeError::contract(
                "continuous transport numerical scales invalid",
            ));
        }
        policy.validate()?;
        self.transport_policy = Some((policy, scales));
        Ok(self)
    }
    pub fn network(&self) -> &ContinuousMixedNetwork<f64> {
        &self.network
    }
    pub fn active_cells(&self) -> &[usize] {
        &self.active_cells
    }
    pub fn active_columns(&self) -> &[usize] {
        &self.columns
    }
    pub fn pack(&self, inventory: &[f64]) -> CaeResult<Vec<f64>> {
        self.network
            .layout()
            .validate(inventory, &self.network.prepared().geometry().volume_m3)?;
        Ok(self.columns.iter().map(|c| inventory[*c]).collect())
    }
    pub fn unpack<S: TransientGasScalar>(&self, state: &[S]) -> CaeResult<Vec<S>> {
        if state.len() != self.columns.len() || state.iter().any(|v| !v.finite_first_order()) {
            return Err(CaeError::contract(
                "mixed continuous active state shape or finite domain",
            ));
        }
        let mut out = vec![S::zero(); self.network.layout().state_size()];
        for (c, v) in self.columns.iter().zip(state) {
            out[*c] = *v;
        }
        Ok(out)
    }
    pub fn evaluate(&self, state: &[f64]) -> CaeResult<ContinuousMixedResult<f64>> {
        self.network.evaluate(
            &self.unpack(state)?,
            &self.authored.previous_inventory,
            &self.authored.port_coefficient_density,
            self.authored.gas_rt_m2_s2,
            self.authored.molecular_viscosity_pa_s,
        )
    }
    pub fn build_constrained_graph(
        &self,
        policy: &QuadraticGasNetworkSolvePolicy,
    ) -> CaeResult<ContinuousMixedConstrained<'_>> {
        use implexity_solve::{
            cone_residual::ConeResidualProblem, partial_constrained_graph::PartialPositiveConeGraph,
        };
        policy.nonlinear.validate()?;
        if !policy.reference_density_kg_m3.is_finite()
            || policy.reference_density_kg_m3 <= 0.
            || !policy.physical_moment_relative_tolerance.is_finite()
            || policy.physical_moment_relative_tolerance <= 0.
            || policy
                .physical_moment_absolute_tolerances
                .iter()
                .any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(CaeError::contract(
                "mixed continuous solver physical/numerical policy invalid",
            ));
        }
        let basis = self.network.layout().basis();
        let speed = basis.reference_speed_m_s();
        let units = [
            1.,
            speed,
            speed,
            speed,
            speed * speed,
            speed * speed,
            speed * speed,
            speed * speed,
            speed * speed,
            speed * speed,
        ];
        if units.iter().any(|v| !v.is_finite() || *v <= 0.) {
            return Err(CaeError::contract("mixed continuous moment scale invalid"));
        }
        let equality = (0..10)
            .map(|j| {
                basis
                    .integrated_moment_rows()
                    .iter()
                    .map(|r| r[j] / units[j])
                    .collect()
            })
            .collect();
        let has_kinetic = self.active_cells.iter().any(|c| {
            self.network.layout().representation(*c).unwrap()
                == ContinuousGasRepresentation::Kinetic
        });
        let cone = if has_kinetic {
            Some(ConeResidualProblem::new(
                basis.mass_rows()?,
                equality,
                policy.cone.clone(),
                policy.numerical_projection_step,
            )?)
        } else {
            None
        };
        let blocks = self
            .active_cells
            .iter()
            .filter(|c| {
                self.network.layout().representation(**c).unwrap()
                    == ContinuousGasRepresentation::Kinetic
            })
            .map(|c| {
                (
                    self.active_ranges[*c].clone().unwrap(),
                    cone.as_ref().unwrap().clone(),
                )
            })
            .collect();
        let initializer: Box<dyn Fn(usize, &[f64]) -> CaeResult<Vec<f64>> + '_> =
            Box::new(move |_: usize, target: &[f64]| {
                let physical = std::array::from_fn(|j| target[j] * units[j]);
                if physical.iter().all(|v| *v == 0.) {
                    return Ok(vec![0.; basis.coefficient_count()]);
                }
                Ok(basis
                    .moment_entropy(physical, &self.network.prepared().policy().entropy)?
                    .coefficient_density)
            });
        let graph =
            PartialPositiveConeGraph::new(self, blocks, initializer)?.with_inner_preconditioner();
        let equilibrium = if has_kinetic {
            self.network
                .prepared()
                .gaussian()
                .equilibrium(
                    policy.reference_density_kg_m3,
                    [0.; 3],
                    self.authored.gas_rt_m2_s2,
                    &self.network.prepared().policy().entropy,
                )?
                .coefficient_density
        } else {
            Vec::new()
        };
        let weights = match policy.numerical_scaling {
            QuadraticGasNumericalScaling::EquilibriumCoefficient => equilibrium,
            QuadraticGasNumericalScaling::UniformCoefficient => {
                vec![equilibrium.iter().copied().fold(0f64, f64::max); equilibrium.len()]
            }
        };
        let mut scales = Vec::with_capacity(self.columns.len());
        let thermal_speed = self.authored.gas_rt_m2_s2.sqrt();
        for c in &self.active_cells {
            let v = self.network.prepared().geometry().volume_m3[*c];
            match self.network.layout().representation(*c)? {
                ContinuousGasRepresentation::Kinetic => {
                    scales.extend(weights.iter().map(|w| w * v))
                }
                ContinuousGasRepresentation::Continuum => scales.extend([
                    policy.reference_density_kg_m3 * v,
                    policy.reference_density_kg_m3 * v * thermal_speed,
                    policy.reference_density_kg_m3 * v * thermal_speed,
                    policy.reference_density_kg_m3 * v * thermal_speed,
                ]),
            }
        }
        Ok(ContinuousMixedConstrained { graph, scales })
    }

    pub fn solve(
        &self,
        numerical_trial_inventory: &[f64],
        policy: &QuadraticGasNetworkSolvePolicy,
    ) -> CaeResult<ContinuousMixedStep> {
        self.solve_internal(numerical_trial_inventory, policy, false)
    }
    pub fn solve_with_selected_kkt_newton(
        &self,
        numerical_trial_inventory: &[f64],
        policy: &QuadraticGasNetworkSolvePolicy,
    ) -> CaeResult<ContinuousMixedStep> {
        self.solve_internal(numerical_trial_inventory, policy, true)
    }
    fn solve_internal(
        &self,
        numerical_trial_inventory: &[f64],
        policy: &QuadraticGasNetworkSolvePolicy,
        selected_kkt_newton: bool,
    ) -> CaeResult<ContinuousMixedStep> {
        if self.dimension() == 0 {
            policy.nonlinear.validate()?;
            let active = self.pack(numerical_trial_inventory)?;
            let evaluated = self.evaluate(&active)?;
            return Ok(ContinuousMixedStep {
                inventory: self.unpack(&active)?,
                evaluated,
                scaled_kkt_residual_norm: 0.,
                physical_residual_norm: 0.,
                maximum_relative_physical_moment_residual: 0.,
                newton_iterations: 0,
                linear_iterations: 0,
            });
        }
        let constrained = self.build_constrained_graph(policy)?;
        let constrained = if selected_kkt_newton {
            constrained.with_selected_kkt_newton()
        } else {
            constrained
        };
        let scaled = constrained.scaled_graph()?;
        let basis = self.network.layout().basis();
        let thermal_speed = self.authored.gas_rt_m2_s2.sqrt();
        let initial = scaled.from_inner_state(&self.pack(numerical_trial_inventory)?)?;
        let solution =
            implexity_solve::nonlinear_graph::solve(&scaled, &initial, &policy.nonlinear)?;
        let current = scaled.to_inner_state(&solution.state)?;
        let inventory = self.unpack(&current)?;
        let evaluated = self.evaluate(&current)?;
        let mut maximum_relative_physical_moment_residual = 0f64;
        let units = [
            1.,
            thermal_speed,
            thermal_speed,
            thermal_speed,
            self.authored.gas_rt_m2_s2,
            self.authored.gas_rt_m2_s2,
            self.authored.gas_rt_m2_s2,
            self.authored.gas_rt_m2_s2,
            self.authored.gas_rt_m2_s2,
            self.authored.gas_rt_m2_s2,
        ];
        for c in &self.active_cells {
            let range = self.network.layout().range(*c)?;
            let moments = match self.network.layout().representation(*c)? {
                ContinuousGasRepresentation::Kinetic => {
                    basis.weak_moments(&evaluated.residual[range])?.to_vec()
                }
                ContinuousGasRepresentation::Continuum => evaluated.residual[range].to_vec(),
            };
            let mass = self.network.layout().macro_inventory(&inventory, *c)?[0]
                + self
                    .network
                    .layout()
                    .macro_inventory(&self.authored.previous_inventory, *c)?[0];
            for (j, r) in moments.iter().enumerate() {
                let scale = mass * units[j];
                let tolerance = policy.physical_moment_absolute_tolerances[j]
                    + policy.physical_moment_relative_tolerance * scale;
                if !scale.is_finite() || !tolerance.is_finite() || (mass > 0. && scale <= 0.) {
                    return Err(CaeError::contract("mixed physical balance scale overflow"));
                }
                if scale == 0. {
                    if *r != 0. {
                        return Err(CaeError::convergence(
                            "mixed true vacuum physical balance nonzero",
                        ));
                    }
                } else {
                    maximum_relative_physical_moment_residual =
                        maximum_relative_physical_moment_residual.max(r.abs() / scale);
                    if r.abs() > tolerance {
                        return Err(CaeError::convergence(format!(
                            "mixed continuous physical moment did not converge at cell {c}, moment {j}"
                        )));
                    }
                }
            }
        }
        let physical_residual_norm = self
            .columns
            .iter()
            .map(|i| evaluated.residual[*i])
            .fold(0f64, |s, v| s.hypot(v));
        Ok(ContinuousMixedStep {
            inventory,
            evaluated,
            scaled_kkt_residual_norm: solution.residual_norm,
            physical_residual_norm,
            maximum_relative_physical_moment_residual,
            newton_iterations: solution.newton_iterations,
            linear_iterations: solution.linear_iterations,
        })
    }
}
impl NonlinearGraphSystem for ContinuousMixedSystem {
    fn dimension(&self) -> usize {
        self.columns.len()
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        self.network.layout().validate(
            &self.unpack(state)?,
            &self.network.prepared().geometry().volume_m3,
        )
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("mixed continuous direction invalid"));
        }
        let mut step = 1f64;
        for c in &self.active_cells {
            let r = self.active_ranges[*c].clone().unwrap();
            let indices = if self.network.layout().representation(*c)?
                == ContinuousGasRepresentation::Kinetic
            {
                r.clone()
            } else {
                r.start..r.start + 1
            };
            for i in indices {
                if direction[i] < 0. {
                    step = step.min(-state[i] / direction[i]);
                }
            }
        }
        Ok(step)
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        let e = self.evaluate(state)?;
        Ok(self.columns.iter().map(|i| e.residual[*i]).collect())
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "mixed continuous Jacobian direction invalid",
            ));
        }
        if !transpose {
            let x = state
                .iter()
                .zip(direction)
                .map(|(x, d)| Dual::new(*x, [*d]))
                .collect::<Vec<_>>();
            let old = self
                .authored
                .previous_inventory
                .iter()
                .map(|v| Dual::constant(*v))
                .collect::<Vec<_>>();
            let ports = self
                .authored
                .port_coefficient_density
                .iter()
                .map(|a| a.iter().map(|v| Dual::constant(*v)).collect())
                .collect::<Vec<_>>();
            let out = self.tangent.evaluate(
                &self.unpack(&x)?,
                &old,
                &ports,
                Dual::constant(self.authored.gas_rt_m2_s2),
                Dual::constant(self.authored.molecular_viscosity_pa_s),
            )?;
            return Ok(self
                .columns
                .iter()
                .map(|i| out.residual[*i].eps[0])
                .collect());
        }
        let mut current = self
            .unpack(state)?
            .iter()
            .map(|v| ReverseScalar::from_f64(*v))
            .collect::<Vec<_>>();
        let old = self
            .authored
            .previous_inventory
            .iter()
            .map(|v| ReverseScalar::from_f64(*v))
            .collect::<Vec<_>>();
        let ports = self
            .authored
            .port_coefficient_density
            .iter()
            .map(|a| a.iter().map(|v| ReverseScalar::from_f64(*v)).collect())
            .collect::<Vec<_>>();
        let moment_cache = self.reverse.moment_cache(&current, &old)?;
        let mut out = vec![0.; self.dimension()];
        for (output, c) in self.active_cells.iter().enumerate() {
            let support = self.supports[output]
                .iter()
                .filter(|c| self.active_ranges[**c].is_some())
                .copied()
                .collect::<Vec<_>>();
            let indices = support
                .iter()
                .flat_map(|c| self.network.layout().range(*c).unwrap())
                .collect::<Vec<_>>();
            let values = indices
                .iter()
                .map(|i| current[*i].value())
                .collect::<Vec<_>>();
            let range = self.network.layout().range(*c)?;
            let bars = &direction[self.active_ranges[*c].clone().unwrap()];
            let gradient = scoped_pullback(&values, |leaves| {
                for (i, v) in indices.iter().zip(leaves) {
                    current[*i] = *v;
                }
                let e = moment_cache.evaluate_cells(
                    &current,
                    &old,
                    &ports,
                    ReverseScalar::from_f64(self.authored.gas_rt_m2_s2),
                    ReverseScalar::from_f64(self.authored.molecular_viscosity_pa_s),
                    &[*c],
                )?;
                Ok(e.residual[range.clone()]
                    .iter()
                    .zip(bars)
                    .fold(ReverseScalar::zero(), |s, (v, b)| s + *v * *b))
            })?;
            let mut position = 0;
            for cell in support {
                let full = self.network.layout().range(cell)?;
                let active = self.active_ranges[cell].clone().unwrap();
                for (i, j) in full.zip(active) {
                    out[j] += gradient[position];
                    current[i] = ReverseScalar::from_f64(state[j]);
                    position += 1;
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
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "mixed continuous preconditioner direction invalid",
            ));
        }
        if let Some((policy, scales)) = &self.transport_policy {
            if !self
                .transport_inverse
                .borrow()
                .as_ref()
                .is_some_and(|(x, _)| policy.freeze_within_solve || x == state)
            {
                let factor =
                    crate::gas_continuous_preconditioner::ContinuousTransportPreconditioner::new(
                        &self.network,
                        &self.authored,
                        &self.unpack(state)?,
                        &self.columns,
                        scales,
                        policy,
                    )?;
                *self.transport_inverse.borrow_mut() = Some((state.to_vec(), factor));
            }
            let cache = self.transport_inverse.borrow();
            return cache
                .as_ref()
                .unwrap()
                .1
                .apply(direction, transpose, |v, t| {
                    self.jacobian_product(state, v, t)
                });
        }
        if !self
            .inverse
            .borrow()
            .as_ref()
            .is_some_and(|(x, _)| x == state)
        {
            let mut blocks = Vec::new();
            for c in &self.active_cells {
                if self.network.layout().representation(*c)? == ContinuousGasRepresentation::Kinetic
                {
                    let r = self.active_ranges[*c].clone().unwrap();
                    blocks.push((
                        *c,
                        CollisionInverse::new_for(
                            self.network.prepared(),
                            &state[r],
                            *c,
                            self.authored.gas_rt_m2_s2,
                            self.authored.molecular_viscosity_pa_s,
                        )?,
                    ));
                }
            }
            *self.inverse.borrow_mut() = Some((state.to_vec(), blocks));
        }
        let mut out = direction.to_vec();
        let cache = self.inverse.borrow();
        let basis = self.network.layout().basis();
        for (c, block) in &cache.as_ref().unwrap().1 {
            let r = self.active_ranges[*c].clone().unwrap();
            let d = &direction[r.clone()];
            let v = if transpose {
                basis.mass_solve(&block.apply(basis, d, true)?)?
            } else {
                block.apply(basis, &basis.mass_solve(d)?, false)?
            };
            out[r].copy_from_slice(&v);
        }
        Ok(out)
    }
}

pub use crate::gas_continuous_mixed::ContinuousMixedOutputBars;
pub struct ContinuousMixedInputBars {
    pub current_inventory: Vec<f64>,
    pub previous_inventory: Vec<f64>,
    pub port_coefficient_density: Vec<Vec<f64>>,
    pub gas_rt_m2_s2: f64,
    pub molecular_viscosity_pa_s: f64,
    pub fixed_previous_vacuum_cells: Vec<usize>,
}
impl ContinuousMixedSystem {
    pub fn authored(&self) -> &ContinuousMixedState {
        &self.authored
    }
    pub fn output_vjp(
        &self,
        state: &[f64],
        bars: &ContinuousMixedOutputBars,
    ) -> CaeResult<ContinuousMixedInputBars> {
        self.check_state(state)?;
        let geometry = self.network.prepared().geometry();
        if bars.residual.len() != self.network.layout().state_size()
            || bars
                .residual
                .iter()
                .chain(bars.boundary_moments.values().flatten())
                .chain(bars.boundary_wall_work_j.values())
                .chain(bars.boundary_wall_heat_j.values())
                .any(|v| !v.is_finite())
            || bars
                .boundary_moments
                .keys()
                .chain(bars.boundary_wall_work_j.keys())
                .chain(bars.boundary_wall_heat_j.keys())
                .any(|f| {
                    *f >= geometry.faces.len()
                        || geometry.faces[*f].right.is_some()
                        || geometry.faces[*f].area_m2 <= 0.
                })
        {
            return Err(CaeError::contract(
                "mixed continuous output cotangent invalid",
            ));
        }
        for c in 0..self.network.layout().slots() {
            if self.active_ranges[c].is_none()
                && bars.residual[self.network.layout().range(c)?]
                    .iter()
                    .any(|v| *v != 0.)
            {
                return Err(CaeError::contract(
                    "closed mixed residual cotangent has no ordinary event derivative",
                ));
            }
        }
        let physical = self.unpack(state)?;
        let mut current = physical
            .iter()
            .map(|v| ReverseScalar::from_f64(*v))
            .collect::<Vec<_>>();
        let mut old = self
            .authored
            .previous_inventory
            .iter()
            .map(|v| ReverseScalar::from_f64(*v))
            .collect::<Vec<_>>();
        let mut ports = self
            .authored
            .port_coefficient_density
            .iter()
            .map(|a| {
                a.iter()
                    .map(|v| ReverseScalar::from_f64(*v))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let moment_cache = self.reverse.moment_cache(&current, &old)?;
        let mut out = ContinuousMixedInputBars {
            current_inventory: vec![0.; physical.len()],
            previous_inventory: vec![0.; old.len()],
            port_coefficient_density: ports.iter().map(|a| vec![0.; a.len()]).collect(),
            gas_rt_m2_s2: 0.,
            molecular_viscosity_pa_s: 0.,
            fixed_previous_vacuum_cells: Vec::new(),
        };
        for c in 0..self.network.layout().slots() {
            if self
                .network
                .layout()
                .macro_inventory(&self.authored.previous_inventory, c)?[0]
                == 0.
            {
                out.fixed_previous_vacuum_cells.push(c);
            }
        }
        for (index, cell) in self.active_cells.iter().enumerate() {
            let current_indices = self.supports[index]
                .iter()
                .filter(|c| self.active_ranges[**c].is_some())
                .flat_map(|c| self.network.layout().range(*c).unwrap())
                .collect::<Vec<_>>();
            let previous_indices = self.supports[index]
                .iter()
                .filter(|c| {
                    geometry.previous_volume_m3[**c] > 0.
                        && self
                            .network
                            .layout()
                            .macro_inventory(&self.authored.previous_inventory, **c)
                            .unwrap()[0]
                            > 0.
                })
                .flat_map(|c| self.network.layout().range(*c).unwrap())
                .collect::<Vec<_>>();
            let mut selected_ports = std::collections::BTreeSet::new();
            for f in &geometry.faces {
                if f.left == *cell && f.area_m2 > 0. {
                    if let Some(crate::gas_quadratic_network::QuadraticGasBoundary::Port { port }) =
                        f.boundary
                    {
                        selected_ports.insert(port);
                    }
                }
            }
            let port_indices = selected_ports
                .into_iter()
                .flat_map(|p| (0..ports[p].len()).map(move |k| (p, k)))
                .collect::<Vec<_>>();
            let mut values = current_indices
                .iter()
                .map(|i| physical[*i])
                .collect::<Vec<_>>();
            let old_start = values.len();
            values.extend(
                previous_indices
                    .iter()
                    .map(|i| self.authored.previous_inventory[*i]),
            );
            let ports_start = values.len();
            values.extend(
                port_indices
                    .iter()
                    .map(|(p, k)| self.authored.port_coefficient_density[*p][*k]),
            );
            let scalar_start = values.len();
            values.extend([
                self.authored.gas_rt_m2_s2,
                self.authored.molecular_viscosity_pa_s,
            ]);
            let gradient = scoped_pullback(&values, |leaves| {
                for (i, v) in current_indices.iter().zip(&leaves[..old_start]) {
                    current[*i] = *v;
                }
                for (i, v) in previous_indices.iter().zip(&leaves[old_start..ports_start]) {
                    old[*i] = *v;
                }
                for ((p, k), v) in port_indices.iter().zip(&leaves[ports_start..scalar_start]) {
                    ports[*p][*k] = *v;
                }
                let result = moment_cache.evaluate_cells(
                    &current,
                    &old,
                    &ports,
                    leaves[scalar_start],
                    leaves[scalar_start + 1],
                    &[*cell],
                )?;
                let range = self.network.layout().range(*cell)?;
                let mut objective = result.residual[range.clone()]
                    .iter()
                    .zip(&bars.residual[range])
                    .fold(ReverseScalar::zero(), |s, (r, b)| s + *r * *b);
                for packet in result.boundary_packets {
                    if let Some(b) = bars.boundary_moments.get(&packet.face) {
                        for j in 0..10 {
                            objective += packet.moments[j] * b[j];
                        }
                    }
                    if let Some(b) = bars.boundary_wall_work_j.get(&packet.face) {
                        objective += packet.wall_work_j * *b;
                    }
                    if let Some(b) = bars.boundary_wall_heat_j.get(&packet.face) {
                        objective += packet.wall_heat_j * *b;
                    }
                }
                Ok(objective)
            })?;
            for (i, g) in current_indices.iter().zip(&gradient[..old_start]) {
                out.current_inventory[*i] += *g;
                current[*i] = ReverseScalar::from_f64(physical[*i]);
            }
            for (i, g) in previous_indices
                .iter()
                .zip(&gradient[old_start..ports_start])
            {
                out.previous_inventory[*i] += *g;
                old[*i] = ReverseScalar::from_f64(self.authored.previous_inventory[*i]);
            }
            for ((p, k), g) in port_indices
                .iter()
                .zip(&gradient[ports_start..scalar_start])
            {
                out.port_coefficient_density[*p][*k] += *g;
                ports[*p][*k] =
                    ReverseScalar::from_f64(self.authored.port_coefficient_density[*p][*k]);
            }
            out.gas_rt_m2_s2 += gradient[scalar_start];
            out.molecular_viscosity_pa_s += gradient[scalar_start + 1];
        }
        Ok(out)
    }
}
