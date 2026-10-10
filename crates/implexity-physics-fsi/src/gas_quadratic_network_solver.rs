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

use crate::gas_quadratic_network::{
    PreparedQuadraticGasNetwork, QuadraticGasNetworkSolution, QuadraticGasNetworkState,
    QuadraticGasStateAccess,
};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::gas_vjp::{ReverseScalar, scoped_pullback};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::nonlinear_graph::NonlinearGraphSystem;
use std::collections::BTreeMap;

pub struct QuadraticGasNetworkSystem {
    prepared: PreparedQuadraticGasNetwork<f64>,
    tangent: PreparedQuadraticGasNetwork<Dual<1>>,
    reverse: PreparedQuadraticGasNetwork<ReverseScalar>,
    authored: QuadraticGasNetworkState<f64>,
    active_cells: Vec<usize>,
    offsets: Vec<Option<usize>>,
    supports: Vec<Vec<usize>>,
    boundary_ports: Vec<Vec<usize>>,
    dimension: usize,
    collision_inverse: std::cell::RefCell<Option<(Vec<f64>, Vec<CollisionInverse>)>>,
}
struct Access<'a, S> {
    authored: &'a QuadraticGasNetworkState<f64>,
    offsets: &'a [Option<usize>],
    state: &'a [S],
    local: Option<&'a BTreeMap<usize, Vec<S>>>,
    coefficients: usize,
}
impl<S: TransientGasScalar> QuadraticGasStateAccess<S> for Access<'_, S> {
    fn coefficient_inventory(&self, cell: usize, previous: bool) -> CaeResult<Option<Vec<S>>> {
        if previous {
            return self
                .authored
                .previous_coefficient_inventory
                .get(cell)
                .map(|a| {
                    a.as_ref()
                        .map(|a| a.iter().map(|v| S::from_f64(*v)).collect())
                })
                .ok_or_else(|| CaeError::contract("continuous graph history cell is absent"));
        }
        let offset = self
            .offsets
            .get(cell)
            .ok_or_else(|| CaeError::contract("continuous graph cell is absent"))?;
        match offset {
            None => Ok(None),
            Some(offset) => {
                if let Some(local) = self.local {
                    return local.get(&cell).cloned().map(Some).ok_or_else(|| {
                        CaeError::contract("continuous graph reverse support is incomplete")
                    });
                }
                Ok(Some(
                    self.state[*offset..*offset + self.coefficients].to_vec(),
                ))
            }
        }
    }
    fn port_density(&self, port: usize) -> CaeResult<Vec<S>> {
        self.authored
            .port_coefficient_density
            .get(port)
            .map(|a| a.iter().map(|v| S::from_f64(*v)).collect())
            .ok_or_else(|| CaeError::contract("continuous graph port is absent"))
    }
    fn gas_rt_m2_s2(&self) -> S {
        S::from_f64(self.authored.gas_rt_m2_s2)
    }
    fn molecular_viscosity_pa_s(&self) -> S {
        S::from_f64(self.authored.molecular_viscosity_pa_s)
    }
}
impl QuadraticGasNetworkSystem {
    pub fn new(
        prepared: PreparedQuadraticGasNetwork<f64>,
        authored: QuadraticGasNetworkState<f64>,
    ) -> CaeResult<Self> {
        prepared.evaluate(&authored)?;
        let n = prepared.basis().coefficient_count();
        let mut active_cells = Vec::new();
        let mut offsets = vec![None; prepared.geometry().volume_m3.len()];
        let mut dimension = 0usize;
        for (cell, volume) in prepared.geometry().volume_m3.iter().enumerate() {
            if *volume > 0. {
                offsets[cell] = Some(dimension);
                dimension = dimension
                    .checked_add(n)
                    .ok_or_else(|| CaeError::contract("continuous graph dimension overflow"))?;
                active_cells.push(cell);
            }
        }
        let supports = active_cells
            .iter()
            .map(|cell| prepared.dependency_cells(&[*cell]))
            .collect::<CaeResult<Vec<_>>>()?;
        let mut boundary_ports = vec![std::collections::BTreeSet::new(); offsets.len()];
        for face in &prepared.geometry().faces {
            if face.area_m2 > 0. {
                if let Some(crate::gas_quadratic_network::QuadraticGasBoundary::Port { port }) =
                    face.boundary
                {
                    boundary_ports[face.left].insert(port);
                }
            }
        }
        let boundary_ports = active_cells
            .iter()
            .map(|cell| boundary_ports[*cell].iter().copied().collect())
            .collect();
        Ok(Self {
            tangent: prepared.constant_lift(),
            reverse: prepared.constant_lift(),
            prepared,
            authored,
            active_cells,
            offsets,
            supports,
            boundary_ports,
            dimension,
            collision_inverse: std::cell::RefCell::new(None),
        })
    }
    pub fn prepared(&self) -> &PreparedQuadraticGasNetwork<f64> {
        &self.prepared
    }
    pub fn active_cells(&self) -> &[usize] {
        &self.active_cells
    }
    pub fn pack(&self, inventory: &[Option<Vec<f64>>]) -> CaeResult<Vec<f64>> {
        if inventory.len() != self.offsets.len() {
            return Err(CaeError::contract(
                "continuous graph inventory layout mismatch",
            ));
        }
        let n = self.prepared.basis().coefficient_count();
        let mut out = Vec::with_capacity(self.dimension);
        for (cell, offset) in self.offsets.iter().enumerate() {
            if offset.is_some() {
                let a = inventory[cell].as_ref().ok_or_else(|| {
                    CaeError::contract("open continuous graph inventory is absent")
                })?;
                if a.len() != n {
                    return Err(CaeError::contract(
                        "continuous graph coefficient count mismatch",
                    ));
                }
                out.extend_from_slice(a);
            } else if inventory[cell]
                .as_ref()
                .is_some_and(|a| a.iter().any(|v| *v != 0.))
            {
                return Err(CaeError::contract(
                    "closed continuous graph inventory is nonzero",
                ));
            }
        }
        self.check_state(&out)?;
        Ok(out)
    }
    pub fn unpack(&self, state: &[f64]) -> CaeResult<Vec<Option<Vec<f64>>>> {
        self.check_state(state)?;
        let n = self.prepared.basis().coefficient_count();
        Ok(self
            .offsets
            .iter()
            .map(|o| o.map(|o| state[o..o + n].to_vec()))
            .collect())
    }
    pub fn evaluate(&self, state: &[f64]) -> CaeResult<QuadraticGasNetworkSolution<f64>> {
        self.check_state(state)?;
        self.prepared.evaluate_parallel(&Access {
            authored: &self.authored,
            offsets: &self.offsets,
            state,
            local: None,
            coefficients: self.prepared.basis().coefficient_count(),
        })
    }
}
impl NonlinearGraphSystem for QuadraticGasNetworkSystem {
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        if state.len() != self.dimension || state.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(CaeError::contract(
                "continuous graph state must be finite and nonnegative",
            ));
        }
        Ok(())
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != state.len() || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("continuous graph direction is invalid"));
        }
        let mut step = 1f64;
        for (x, d) in state.iter().zip(direction) {
            if *d < 0. {
                step = step.min(-x / d);
            }
        }
        Ok(step)
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(self
            .evaluate(state)?
            .weak_residual
            .into_iter()
            .flatten()
            .flatten()
            .collect())
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        if direction.len() != self.dimension || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "continuous graph Jacobian direction is invalid",
            ));
        }
        let n = self.prepared.basis().coefficient_count();
        if !transpose {
            let values = state
                .iter()
                .zip(direction)
                .map(|(v, d)| Dual::new(*v, [*d]))
                .collect::<Vec<_>>();
            let result = self.tangent.evaluate_parallel(&Access {
                authored: &self.authored,
                offsets: &self.offsets,
                state: &values,
                local: None,
                coefficients: n,
            })?;
            return Ok(result
                .weak_residual
                .into_iter()
                .flatten()
                .flatten()
                .map(|v| v.eps[0])
                .collect());
        }
        let mut out = vec![0.; self.dimension];
        for (i, cell) in self.active_cells.iter().enumerate() {
            let support = self.supports[i]
                .iter()
                .filter(|c| self.offsets[**c].is_some())
                .copied()
                .collect::<Vec<_>>();
            let values = support
                .iter()
                .flat_map(|c| {
                    let o = self.offsets[*c].unwrap();
                    state[o..o + n].iter().copied()
                })
                .collect::<Vec<_>>();
            let bars = &direction[self.offsets[*cell].unwrap()..self.offsets[*cell].unwrap() + n];
            let gradient = scoped_pullback(&values, |leaves| {
                let local = support
                    .iter()
                    .enumerate()
                    .map(|(j, c)| (*c, leaves[j * n..(j + 1) * n].to_vec()))
                    .collect::<BTreeMap<_, _>>();
                let access = Access {
                    authored: &self.authored,
                    offsets: &self.offsets,
                    state: &[],
                    local: Some(&local),
                    coefficients: n,
                };
                let result = self.reverse.evaluate_cells(&access, &[*cell])?;
                let residual = &result
                    .cells
                    .first()
                    .ok_or_else(|| CaeError::contract("continuous graph reverse cell is absent"))?
                    .1
                    .weak_residual;
                Ok(residual
                    .iter()
                    .zip(bars)
                    .fold(ReverseScalar::zero(), |sum, (v, b)| sum + *v * *b))
            })?;
            for (j, c) in support.iter().enumerate() {
                let o = self.offsets[*c].unwrap();
                for k in 0..n {
                    out[o + k] += gradient[j * n + k];
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
        if direction.len() != self.dimension || direction.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "continuous collision inverse direction invalid",
            ));
        }
        if !self
            .collision_inverse
            .borrow()
            .as_ref()
            .is_some_and(|(x, _)| x.as_slice() == state)
        {
            let n = self.prepared.basis().coefficient_count();
            let blocks = self
                .active_cells
                .iter()
                .map(|cell| {
                    let o = self.offsets[*cell].unwrap();
                    CollisionInverse::new(self, &state[o..o + n], *cell)
                })
                .collect::<CaeResult<Vec<_>>>()?;
            *self.collision_inverse.borrow_mut() = Some((state.to_vec(), blocks));
        }
        let cache = self.collision_inverse.borrow();
        let n = self.prepared.basis().coefficient_count();
        let mut out = Vec::with_capacity(self.dimension);
        for (i, block) in cache.as_ref().unwrap().1.iter().enumerate() {
            let d = &direction[i * n..(i + 1) * n];
            if transpose {
                out.extend(self.prepared.basis().mass_solve(&block.apply(
                    self.prepared.basis(),
                    d,
                    true,
                )?)?);
            } else {
                out.extend(block.apply(
                    self.prepared.basis(),
                    &self.prepared.basis().mass_solve(d)?,
                    false,
                )?);
            }
        }
        Ok(out)
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuadraticGasNumericalScaling {
    EquilibriumCoefficient,
    UniformCoefficient,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuadraticGasNetworkSolvePolicy {
    pub numerical_scaling: QuadraticGasNumericalScaling,
    pub nonlinear: implexity_solve::nonlinear_graph::NonlinearGraphPolicy,
    pub cone: implexity_solve::positive_cone::ConeProjectionPolicy,
    pub numerical_projection_step: f64,
    pub reference_density_kg_m3: f64,
    pub physical_moment_relative_tolerance: f64,
    pub physical_moment_absolute_tolerances: [f64; 10],
}
pub struct QuadraticGasNetworkStep {
    pub coefficient_inventory: Vec<Option<Vec<f64>>>,
    pub evaluated: QuadraticGasNetworkSolution<f64>,
    pub scaled_kkt_residual_norm: f64,
    pub physical_weak_residual_norm: f64,
    pub maximum_relative_weak_balance_error: f64,
    pub maximum_normalized_moment_residual: f64,
    pub maximum_relative_moment_balance_residual: f64,
    pub newton_iterations: usize,
    pub linear_iterations: usize,
}
impl QuadraticGasNetworkSystem {
    pub fn solve(
        &self,
        numerical_trial_inventory: &[Option<Vec<f64>>],
        policy: &QuadraticGasNetworkSolvePolicy,
    ) -> CaeResult<QuadraticGasNetworkStep> {
        use implexity_solve::{
            block_constrained_graph::BlockPositiveConeGraph, cone_residual::ConeResidualProblem,
            diagonal_graph::DiagonalGraph,
        };
        if !policy.reference_density_kg_m3.is_finite() || policy.reference_density_kg_m3 <= 0. {
            return Err(CaeError::contract(
                "continuous graph numerical reference density is invalid",
            ));
        }
        if !policy.physical_moment_relative_tolerance.is_finite()
            || policy.physical_moment_relative_tolerance <= 0.
            || policy
                .physical_moment_absolute_tolerances
                .iter()
                .any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(CaeError::contract(
                "continuous graph physical moment convergence policy is invalid",
            ));
        }
        let basis = self.prepared.basis();
        let speed = basis.reference_speed_m_s();
        let normalization = [
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
        if normalization.iter().any(|s| !s.is_finite() || *s <= 0.) {
            return Err(CaeError::contract(
                "continuous graph moment normalization overflow",
            ));
        }
        let equality = (0..10)
            .map(|j| {
                basis
                    .integrated_moment_rows()
                    .iter()
                    .map(|r| r[j] / normalization[j])
                    .collect()
            })
            .collect();
        let cone = ConeResidualProblem::new(
            basis.mass_rows()?,
            equality,
            policy.cone.clone(),
            policy.numerical_projection_step,
        )?;
        let graph = BlockPositiveConeGraph::new(
            self,
            vec![cone; self.active_cells.len()],
            |_: usize, target: &[f64]| {
                let physical = std::array::from_fn(|j| target[j] * normalization[j]);
                if physical[0] == 0. && physical.iter().all(|v| *v == 0.) {
                    return Ok(vec![0.; basis.coefficient_count()]);
                }
                Ok(basis
                    .moment_entropy(physical, &self.prepared.policy().entropy)?
                    .coefficient_density)
            },
        )?
        .with_inner_preconditioner();
        let weights = self
            .prepared
            .gaussian()
            .equilibrium(
                policy.reference_density_kg_m3,
                [0.; 3],
                self.authored.gas_rt_m2_s2,
                &self.prepared.policy().entropy,
            )?
            .coefficient_density;
        let weights = match policy.numerical_scaling {
            QuadraticGasNumericalScaling::EquilibriumCoefficient => weights,
            QuadraticGasNumericalScaling::UniformCoefficient => {
                vec![weights.iter().copied().fold(0f64, f64::max); weights.len()]
            }
        };
        let scales = self
            .active_cells
            .iter()
            .flat_map(|cell| {
                weights
                    .iter()
                    .map(move |w| *w * self.prepared.geometry().volume_m3[*cell])
            })
            .collect::<Vec<_>>();
        let scaled = DiagonalGraph::new(&graph, scales.clone(), scales)?;
        let initial = scaled.from_inner_state(&self.pack(numerical_trial_inventory)?)?;
        let result = implexity_solve::nonlinear_graph::solve(&scaled, &initial, &policy.nonlinear)?;
        let state = scaled.to_inner_state(&result.state)?;
        let evaluated = self.evaluate(&state)?;
        let physical_weak_residual_norm = evaluated
            .weak_residual
            .iter()
            .flatten()
            .flatten()
            .fold(0f64, |n, v| n.hypot(*v));
        let local = self.prepared.evaluate_cells_parallel(
            &Access {
                authored: &self.authored,
                offsets: &self.offsets,
                state: &state,
                local: None,
                coefficients: basis.coefficient_count(),
            },
            &self.active_cells,
        )?;
        let norm = |values: &[f64]| values.iter().fold(0f64, |sum, v| sum.hypot(*v));
        let mut weak_scales = vec![0.; self.offsets.len()];
        let mut weak_defects = vec![0.; self.offsets.len()];
        for (cell, value) in &local.cells {
            let offset = self.offsets[*cell].unwrap();
            weak_scales[*cell] =
                norm(&basis.mass_apply(&state[offset..offset + basis.coefficient_count()])?)
                    + self.authored.previous_coefficient_inventory[*cell]
                        .as_ref()
                        .map(|a| basis.mass_apply(a).map(|v| norm(&v)))
                        .transpose()?
                        .unwrap_or(0.)
                    + norm(&basis.mass_apply(&value.collision_coefficient_increment)?);
            weak_defects[*cell] = norm(&value.weak_residual);
        }
        for packet in &local.face_packets {
            let scale = norm(&packet.weak_inventory);
            let face = &self.prepared.geometry().faces[packet.face];
            weak_scales[face.left] += scale;
            if let Some(right) = face.right {
                weak_scales[right] += scale;
            }
        }
        let maximum_relative_weak_balance_error = self
            .active_cells
            .iter()
            .map(|cell| {
                if weak_scales[*cell] == 0. {
                    if weak_defects[*cell] == 0. {
                        0.
                    } else {
                        f64::INFINITY
                    }
                } else {
                    weak_defects[*cell] / weak_scales[*cell]
                }
            })
            .fold(0f64, f64::max);
        if !maximum_relative_weak_balance_error.is_finite() {
            return Err(CaeError::contract(
                "continuous graph weak balance diagnostic nonfinite",
            ));
        }
        let maximum_normalized_moment_residual = evaluated
            .physical_moment_residual
            .iter()
            .flatten()
            .flat_map(|a| a.iter().zip(normalization).map(|(v, s)| v.abs() / s))
            .fold(0., f64::max);
        let thermal_speed = self.authored.gas_rt_m2_s2.sqrt();
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
        let mut maximum_relative_moment_balance_residual = 0f64;
        for cell in &self.active_cells {
            let offset = self.offsets[*cell].unwrap();
            let mass =
                basis.coefficient_moments(&state[offset..offset + basis.coefficient_count()])?[0];
            let previous = self.authored.previous_coefficient_inventory[*cell]
                .as_ref()
                .map(|a| basis.coefficient_moments(a))
                .transpose()?
                .map_or(0., |m| m[0]);
            let reference_mass = mass + previous;
            if !reference_mass.is_finite() {
                return Err(CaeError::contract(
                    "continuous physical moment reference overflow",
                ));
            }
            let moments = evaluated.physical_moment_residual[*cell].as_ref().unwrap();
            for j in 0..10 {
                let scale = reference_mass * units[j];
                if !scale.is_finite() || (reference_mass > 0. && scale <= 0.) {
                    return Err(CaeError::contract(
                        "continuous physical moment scale is outside finite range",
                    ));
                }
                if scale == 0. {
                    if moments[j] != 0. {
                        return Err(CaeError::convergence(
                            "true vacuum has a nonzero physical moment balance",
                        ));
                    }
                } else {
                    maximum_relative_moment_balance_residual =
                        maximum_relative_moment_balance_residual.max(moments[j].abs() / scale);
                    let tolerance = policy.physical_moment_absolute_tolerances[j]
                        + policy.physical_moment_relative_tolerance * scale;
                    if !tolerance.is_finite() {
                        return Err(CaeError::contract(
                            "continuous physical moment tolerance overflow",
                        ));
                    }
                    if moments[j].abs() > tolerance {
                        return Err(CaeError::convergence(format!(
                            "continuous physical moment balance did not converge at cell {cell}, moment {j}"
                        )));
                    }
                }
            }
        }
        Ok(QuadraticGasNetworkStep {
            coefficient_inventory: self.unpack(&state)?,
            evaluated,
            scaled_kkt_residual_norm: result.residual_norm,
            physical_weak_residual_norm,
            maximum_relative_weak_balance_error,
            maximum_normalized_moment_residual,
            maximum_relative_moment_balance_residual,
            newton_iterations: result.newton_iterations,
            linear_iterations: result.linear_iterations,
        })
    }
}

pub struct QuadraticGasNetworkOutputBars {
    pub weak_residual: Vec<f64>,
    pub boundary_moment_packets: BTreeMap<usize, [f64; 10]>,
    pub boundary_wall_work_j: BTreeMap<usize, f64>,
    pub boundary_wall_heat_j: BTreeMap<usize, f64>,
}
pub struct QuadraticGasNetworkInputBars {
    pub current_coefficient_inventory: Vec<f64>,
    pub previous_coefficient_inventory: Vec<Option<Vec<f64>>>,
    pub port_coefficient_density: Vec<Vec<f64>>,
    pub gas_rt_m2_s2: f64,
    pub molecular_viscosity_pa_s: f64,
    pub fixed_previous_vacuum_cells: Vec<usize>,
}
struct ParameterAccess<'a> {
    current: Access<'a, ReverseScalar>,
    previous: Option<(usize, &'a [ReverseScalar])>,
    ports: &'a BTreeMap<usize, Vec<ReverseScalar>>,
    rt: ReverseScalar,
    viscosity: ReverseScalar,
}
impl QuadraticGasStateAccess<ReverseScalar> for ParameterAccess<'_> {
    fn coefficient_inventory(
        &self,
        cell: usize,
        previous: bool,
    ) -> CaeResult<Option<Vec<ReverseScalar>>> {
        if previous && self.previous.is_some_and(|(c, _)| c == cell) {
            return Ok(Some(self.previous.unwrap().1.to_vec()));
        }
        self.current.coefficient_inventory(cell, previous)
    }
    fn port_density(&self, port: usize) -> CaeResult<Vec<ReverseScalar>> {
        self.ports
            .get(&port)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| self.current.port_density(port))
    }
    fn gas_rt_m2_s2(&self) -> ReverseScalar {
        self.rt
    }
    fn molecular_viscosity_pa_s(&self) -> ReverseScalar {
        self.viscosity
    }
}
impl QuadraticGasNetworkSystem {
    pub fn output_vjp(
        &self,
        state: &[f64],
        bars: &QuadraticGasNetworkOutputBars,
    ) -> CaeResult<QuadraticGasNetworkInputBars> {
        self.check_state(state)?;
        if bars.weak_residual.len() != self.dimension
            || bars
                .weak_residual
                .iter()
                .chain(bars.boundary_moment_packets.values().flatten())
                .chain(bars.boundary_wall_work_j.values())
                .chain(bars.boundary_wall_heat_j.values())
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "continuous graph output cotangent is invalid",
            ));
        }
        let faces = &self.prepared.geometry().faces;
        if bars
            .boundary_moment_packets
            .keys()
            .chain(bars.boundary_wall_work_j.keys())
            .chain(bars.boundary_wall_heat_j.keys())
            .any(|f| *f >= faces.len() || faces[*f].right.is_some() || faces[*f].area_m2 == 0.)
        {
            return Err(CaeError::contract(
                "continuous graph cotangent must address an active boundary packet",
            ));
        }
        let n = self.prepared.basis().coefficient_count();
        let mut out = QuadraticGasNetworkInputBars {
            current_coefficient_inventory: vec![0.; self.dimension],
            previous_coefficient_inventory: self
                .authored
                .previous_coefficient_inventory
                .iter()
                .map(|a| a.as_ref().map(|a| vec![0.; a.len()]))
                .collect(),
            port_coefficient_density: self
                .authored
                .port_coefficient_density
                .iter()
                .map(|a| vec![0.; a.len()])
                .collect(),
            gas_rt_m2_s2: 0.,
            molecular_viscosity_pa_s: 0.,
            fixed_previous_vacuum_cells: Vec::new(),
        };
        for (index, cell) in self.active_cells.iter().enumerate() {
            let support = self.supports[index]
                .iter()
                .filter(|c| self.offsets[**c].is_some())
                .copied()
                .collect::<Vec<_>>();
            let ports = &self.boundary_ports[index];
            let mut values = support
                .iter()
                .flat_map(|c| {
                    let o = self.offsets[*c].unwrap();
                    state[o..o + n].iter().copied()
                })
                .collect::<Vec<_>>();
            let current_end = values.len();
            let old = self.authored.previous_coefficient_inventory[*cell].as_ref();
            let old_positive = old.is_some_and(|a| a.iter().any(|v| *v > 0.));
            if old_positive {
                values.extend_from_slice(old.unwrap());
            } else {
                out.fixed_previous_vacuum_cells.push(*cell);
            }
            let previous_end = values.len();
            for p in ports {
                values.extend_from_slice(&self.authored.port_coefficient_density[*p]);
            }
            let port_end = values.len();
            values.extend([
                self.authored.gas_rt_m2_s2,
                self.authored.molecular_viscosity_pa_s,
            ]);
            let offset = self.offsets[*cell].unwrap();
            let gradient = scoped_pullback(&values, |leaves| {
                let local = support
                    .iter()
                    .enumerate()
                    .map(|(j, c)| (*c, leaves[j * n..(j + 1) * n].to_vec()))
                    .collect::<BTreeMap<_, _>>();
                let ports = ports
                    .iter()
                    .enumerate()
                    .map(|(j, p)| {
                        (
                            *p,
                            leaves[previous_end + j * n..previous_end + (j + 1) * n].to_vec(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                let access = ParameterAccess {
                    current: Access {
                        authored: &self.authored,
                        offsets: &self.offsets,
                        state: &[],
                        local: Some(&local),
                        coefficients: n,
                    },
                    previous: old_positive.then_some((*cell, &leaves[current_end..previous_end])),
                    ports: &ports,
                    rt: leaves[port_end],
                    viscosity: leaves[port_end + 1],
                };
                let result = self.reverse.evaluate_cells(&access, &[*cell])?;
                let mut objective = result.cells[0]
                    .1
                    .weak_residual
                    .iter()
                    .zip(&bars.weak_residual[offset..offset + n])
                    .fold(ReverseScalar::zero(), |sum, (v, b)| sum + *v * *b);
                for packet in result.face_packets {
                    if let Some(b) = bars.boundary_moment_packets.get(&packet.face) {
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
            for (j, c) in support.iter().enumerate() {
                let o = self.offsets[*c].unwrap();
                for k in 0..n {
                    out.current_coefficient_inventory[o + k] += gradient[j * n + k];
                }
            }
            if old_positive {
                for k in 0..n {
                    out.previous_coefficient_inventory[*cell].as_mut().unwrap()[k] +=
                        gradient[current_end + k];
                }
            }
            for (j, p) in ports.iter().enumerate() {
                for k in 0..n {
                    out.port_coefficient_density[*p][k] += gradient[previous_end + j * n + k];
                }
            }
            out.gas_rt_m2_s2 += gradient[port_end];
            out.molecular_viscosity_pa_s += gradient[port_end + 1];
        }
        Ok(out)
    }
}

pub(crate) struct CollisionInverse {
    alpha: f64,
    columns: Vec<[f64; 4]>,
    schur: implexity_linalg::dense::DenseLu,
}
impl CollisionInverse {
    fn new(system: &QuadraticGasNetworkSystem, current: &[f64], cell: usize) -> CaeResult<Self> {
        Self::new_for(
            &system.prepared,
            current,
            cell,
            system.authored.gas_rt_m2_s2,
            system.authored.molecular_viscosity_pa_s,
        )
    }
    pub(crate) fn new_for(
        prepared: &PreparedQuadraticGasNetwork<f64>,
        current: &[f64],
        cell: usize,
        rt: f64,
        viscosity: f64,
    ) -> CaeResult<Self> {
        use implexity_linalg::dense::{DenseLu, DenseMatrix};
        let basis = prepared.basis();
        let m = basis.coefficient_moments(current)?;
        let volume = prepared.geometry().volume_m3[cell];
        let step = prepared.geometry().physical_step_s;

        let gamma = rt / (volume * viscosity);
        let delta = step * gamma * m[0];
        let alpha = 1. + delta;
        let mut columns = vec![[0.; 4]; current.len()];
        if m[0] > 0. {
            let moments = std::array::from_fn::<_, 4, _>(|j| {
                let mut eps = [0.; 4];
                eps[j] = 1.;
                Dual::new(m[j], eps)
            });
            let eq = prepared
                .gaussian()
                .equilibrium(
                    moments[0] / volume,
                    std::array::from_fn(|j| moments[j + 1] / moments[0]),
                    Dual::constant(rt),
                    &prepared.policy().entropy,
                )?
                .coefficient_density;
            for k in 0..current.len() {
                for j in 0..4 {
                    columns[k][j] = delta * volume * eq[k].eps[j];
                }
                columns[k][0] += step * gamma * (volume * eq[k].re - current[k]);
            }
        }
        let mut schur = vec![0.; 16];
        for i in 0..4 {
            for j in 0..4 {
                schur[i * 4 + j] = if i == j { 1. } else { 0. };
                for (k, r) in basis.integrated_moment_rows().iter().enumerate() {
                    schur[i * 4 + j] -= r[i] * columns[k][j] / alpha;
                }
            }
        }
        if !alpha.is_finite()
            || alpha <= 0.
            || columns
                .iter()
                .flatten()
                .chain(&schur)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "continuous collision preconditioner overflow",
            ));
        }
        let schur = DenseLu::new(
            &DenseMatrix::new(4, 4, schur).map_err(|e| CaeError::contract(e.to_string()))?,
        )
        .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(Self {
            alpha,
            columns,
            schur,
        })
    }
    pub(crate) fn apply(
        &self,
        basis: &crate::gas_quadratic_velocity::QuadraticVelocityBasis,
        d: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        let rhs = std::array::from_fn::<_, 4, _>(|j| {
            self.columns
                .iter()
                .zip(basis.integrated_moment_rows())
                .zip(d)
                .map(|((u, c), d)| if transpose { u[j] * d } else { c[j] * d })
                .sum()
        });
        let value = self
            .schur
            .solve(&rhs, 1, transpose)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let out = self
            .columns
            .iter()
            .zip(basis.integrated_moment_rows())
            .zip(d)
            .map(|((u, c), d)| {
                d / self.alpha
                    + (0..4)
                        .map(|j| {
                            if transpose {
                                c[j] * value[j]
                            } else {
                                u[j] * value[j]
                            }
                        })
                        .sum::<f64>()
                        / (self.alpha * self.alpha)
            })
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("continuous collision inverse overflow"));
        }
        Ok(out)
    }
}
