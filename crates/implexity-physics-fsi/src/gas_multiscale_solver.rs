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

use crate::gas_bgk_preconditioner::BgkCollisionPreconditioner;
use crate::gas_multiscale_network::{
    MultiscaleNetworkInputs, MultiscaleNetworkPolicy, MultiscaleNetworkTopology,
    residual_cells_prepared_scalar,
};
use crate::gas_transient_kinetic::{TransientGasFace, TransientGasInputs, validate};
use crate::gas_vjp::PreparedNetworkVjp;
use implexity_ad::Dual;
use implexity_core::{CaeError, CaeResult};
use implexity_solve::nonlinear_graph::NonlinearGraphSystem;
use std::sync::{Mutex, OnceLock};

pub(crate) fn lift<const N: usize>(
    a: &MultiscaleNetworkInputs<f64>,
) -> MultiscaleNetworkInputs<Dual<N>> {
    let constant = Dual::constant;
    MultiscaleNetworkInputs {
        force_density_n_m3: a
            .force_density_n_m3
            .as_ref()
            .map(|f| f.iter().map(|v| v.map(constant)).collect()),
        gas: TransientGasInputs {
            volume_m3: a.gas.volume_m3.iter().map(|x| constant(*x)).collect(),
            previous_volume_m3: a
                .gas
                .previous_volume_m3
                .iter()
                .map(|x| constant(*x))
                .collect(),
            previous_inventory_kg: a
                .gas
                .previous_inventory_kg
                .iter()
                .map(|x| constant(*x))
                .collect(),
            faces: a
                .gas
                .faces
                .iter()
                .map(|f| TransientGasFace {
                    left: f.left,
                    boundary: f.boundary.clone(),
                    area_m2: constant(f.area_m2),
                    normal: f.normal.map(constant),
                    ale_velocity_m_s: f.ale_velocity_m_s.map(constant),
                    wall_velocity_m_s: f.wall_velocity_m_s.map(constant),
                })
                .collect(),
            port_density_kg_m3: a
                .gas
                .port_density_kg_m3
                .iter()
                .map(|p| p.iter().map(|q| constant(*q)).collect())
                .collect(),
            grid: a.gas.grid.clone(),
            gas_rt_m2_s2: constant(a.gas.gas_rt_m2_s2),
            viscosity_pa_s: constant(a.gas.viscosity_pa_s),
            step_s: constant(a.gas.step_s),
        },
        left_centroid_to_face_m: a
            .left_centroid_to_face_m
            .iter()
            .map(|v| v.map(constant))
            .collect(),
        right_centroid_to_face_m: a
            .right_centroid_to_face_m
            .iter()
            .map(|v| v.map(|v| v.map(constant)))
            .collect(),
    }
}
struct PreconditionerCache {
    state: Vec<f64>,
    blocks: Vec<Option<BgkCollisionPreconditioner>>,
    coarse: Option<crate::gas_moment_coarse::GasMomentCoarse>,
}
pub struct MultiscaleNetworkSystem {
    inputs: MultiscaleNetworkInputs<f64>,
    policy: MultiscaleNetworkPolicy,
    topology: MultiscaleNetworkTopology,
    active_cells: Vec<usize>,
    offsets: Vec<Option<usize>>,
    mass_scale_kg: Vec<f64>,
    populations: usize,
    dimension: usize,
    dual1: OnceLock<MultiscaleNetworkInputs<Dual<1>>>,
    reverse: OnceLock<PreparedNetworkVjp>,
    preconditioner: Mutex<PreconditionerCache>,
}
impl MultiscaleNetworkSystem {
    pub fn new(
        inputs: MultiscaleNetworkInputs<f64>,
        policy: MultiscaleNetworkPolicy,
        mass_scale_kg: Vec<f64>,
    ) -> CaeResult<Self> {
        validate(&inputs.gas, &policy.kinetic)?;
        let n = inputs.gas.volume_m3.len();
        if mass_scale_kg.len() != n || mass_scale_kg.iter().any(|v| !v.is_finite() || *v <= 0.) {
            return Err(CaeError::contract(
                "multiscale solver requires positive cell mass scales",
            ));
        }
        let topology = MultiscaleNetworkTopology::new_with_policy(&inputs, &policy)?;
        let populations = inputs.gas.grid.velocity_m_s.len();
        let active_cells = (0..n)
            .filter(|c| inputs.gas.volume_m3[*c] > 0.)
            .collect::<Vec<_>>();
        let dimension = active_cells
            .len()
            .checked_mul(populations)
            .ok_or_else(|| CaeError::contract("multiscale solver size overflow"))?;
        let mut offsets = vec![None; n];
        for (i, c) in active_cells.iter().enumerate() {
            offsets[*c] = Some(i * populations);
        }
        Ok(Self {
            inputs,
            policy,
            topology,
            active_cells,
            offsets,
            mass_scale_kg,
            populations,
            dimension,
            dual1: OnceLock::new(),
            reverse: OnceLock::new(),
            preconditioner: Mutex::new(PreconditionerCache {
                state: vec![],
                blocks: vec![],
                coarse: None,
            }),
        })
    }
    pub fn pack(&self, inventory_kg: &[f64]) -> CaeResult<Vec<f64>> {
        if inventory_kg.len() != self.inputs.gas.previous_inventory_kg.len()
            || inventory_kg.iter().any(|q| !q.is_finite() || *q < 0.)
        {
            return Err(CaeError::contract(
                "multiscale solver inventory shape or value",
            ));
        }
        let mut x = vec![0.; self.dimension];
        for c in 0..self.offsets.len() {
            for k in 0..self.populations {
                let q = inventory_kg[c * self.populations + k];
                if let Some(offset) = self.offsets[c] {
                    x[offset + k] = q / self.mass_scale_kg[c];
                } else if q != 0. {
                    return Err(CaeError::contract(
                        "closed multiscale solver cell contains inventory",
                    ));
                }
            }
        }
        self.check_state(&x)?;
        Ok(x)
    }
    pub fn unpack(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        let mut q = vec![0.; self.inputs.gas.previous_inventory_kg.len()];
        for c in &self.active_cells {
            let o = self.offsets[*c].unwrap();
            for k in 0..self.populations {
                q[c * self.populations + k] = state[o + k] * self.mass_scale_kg[*c];
            }
        }
        if q.iter().any(|q| !q.is_finite()) {
            return Err(CaeError::contract("multiscale solver inventory overflow"));
        }
        Ok(q)
    }
    pub fn inputs(&self) -> &MultiscaleNetworkInputs<f64> {
        &self.inputs
    }
    pub fn policy(&self) -> &MultiscaleNetworkPolicy {
        &self.policy
    }
}
impl NonlinearGraphSystem for MultiscaleNetworkSystem {
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn check_state(&self, state: &[f64]) -> CaeResult<()> {
        if state.len() != self.dimension || state.iter().any(|q| !q.is_finite() || *q < 0.) {
            return Err(CaeError::contract(
                "multiscale solver state must be finite and nonnegative",
            ));
        }
        Ok(())
    }
    fn admitted_step(&self, state: &[f64], direction: &[f64]) -> CaeResult<f64> {
        self.check_state(state)?;
        if direction.len() != self.dimension || direction.iter().any(|d| !d.is_finite()) {
            return Err(CaeError::contract(
                "multiscale solver Newton direction shape or value",
            ));
        }
        Ok(state
            .iter()
            .zip(direction)
            .filter(|(_, d)| **d < 0.)
            .map(|(q, d)| -q / d)
            .fold(1f64, f64::min))
    }
    fn residual(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        let r = residual_cells_prepared_scalar(
            &self.inputs,
            &self.topology,
            |c, k| self.offsets[c].map_or(0., |o| state[o + k] * self.mass_scale_kg[c]),
            &self.active_cells,
            &self.policy,
        )?;
        let values = r
            .inventory_residual_kg
            .iter()
            .enumerate()
            .map(|(i, r)| r / self.mass_scale_kg[self.active_cells[i / self.populations]])
            .collect::<Vec<_>>();
        if values.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("multiscale solver residual overflow"));
        }
        Ok(values)
    }
    fn jacobian_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        if direction.len() != self.dimension || direction.iter().any(|d| !d.is_finite()) {
            return Err(CaeError::contract(
                "multiscale solver Jacobian direction shape or value",
            ));
        }
        if !transpose {
            let a = self.dual1.get_or_init(|| lift(&self.inputs));
            let r = residual_cells_prepared_scalar(
                a,
                &self.topology,
                |c, k| {
                    self.offsets[c].map_or(Dual::constant(0.), |o| {
                        Dual::new(
                            state[o + k] * self.mass_scale_kg[c],
                            [direction[o + k] * self.mass_scale_kg[c]],
                        )
                    })
                },
                &self.active_cells,
                &self.policy,
            )?;
            let values = r
                .inventory_residual_kg
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    r.eps[0] / self.mass_scale_kg[self.active_cells[i / self.populations]]
                })
                .collect::<Vec<_>>();
            if values.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("multiscale solver JVP overflow"));
            }
            return Ok(values);
        }
        self.reverse
            .get_or_init(|| PreparedNetworkVjp::new(&self.inputs))
            .apply(
                &self.topology,
                state,
                &self.offsets,
                &self.mass_scale_kg,
                self.populations,
                direction,
                &self.policy,
            )
    }
    fn preconditioner_product(
        &self,
        state: &[f64],
        direction: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_state(state)?;
        if direction.len() != self.dimension || direction.iter().any(|d| !d.is_finite()) {
            return Err(CaeError::contract(
                "multiscale solver preconditioner direction shape or value",
            ));
        }
        let mut cache = self
            .preconditioner
            .lock()
            .map_err(|_| CaeError::contract("multiscale solver preconditioner lock failed"))?;
        if cache.state != state {
            let mut blocks = Vec::with_capacity(self.active_cells.len());
            for c in &self.active_cells {
                let o = self.offsets[*c].unwrap();
                let q = state[o..o + self.populations]
                    .iter()
                    .map(|q| q * self.mass_scale_kg[*c])
                    .collect::<Vec<_>>();
                blocks.push(if q.iter().all(|q| *q == 0.) {
                    None
                } else {
                    Some(BgkCollisionPreconditioner::new(
                        &self.inputs.gas.grid,
                        &q,
                        self.inputs.gas.volume_m3[*c],
                        self.inputs.gas.gas_rt_m2_s2,
                        self.inputs.gas.viscosity_pa_s,
                        self.inputs.gas.step_s,
                        &self.policy.kinetic,
                    )?)
                });
            }
            let coarse = crate::gas_moment_coarse::GasMomentCoarse::new(
                &self.inputs,
                &self.policy,
                &self.topology,
                state,
                &self.offsets,
                &self.mass_scale_kg,
            )?;
            cache.coarse = Some(coarse);
            cache.state = state.to_vec();
            cache.blocks = blocks;
        }
        let micro = |direction: &[f64], transpose: bool| -> CaeResult<Vec<f64>> {
            let mut out = vec![0.; self.dimension];
            for (i, c) in self.active_cells.iter().enumerate() {
                let o = self.offsets[*c].unwrap();
                let d = &direction[o..o + self.populations];
                let v = if let Some(p) = &cache.blocks[i] {
                    p.apply(d, transpose)?
                } else {
                    d.to_vec()
                };
                out[o..o + self.populations].copy_from_slice(&v);
            }
            Ok(out)
        };
        let coarse = cache
            .coarse
            .as_ref()
            .ok_or_else(|| CaeError::contract("gas coarse cache absent"))?;
        if transpose {
            let moment = coarse.restrict(direction, true)?;
            let solved = coarse.solve(&moment, true)?;
            let correction = coarse.lift(&solved, true)?;
            let jtc = self.jacobian_product(state, &correction, true)?;
            let remainder = direction
                .iter()
                .zip(jtc)
                .map(|(v, j)| v - j)
                .collect::<Vec<_>>();
            Ok(micro(&remainder, true)?
                .iter()
                .zip(correction)
                .map(|(m, c)| m + c)
                .collect())
        } else {
            let base = micro(direction, false)?;
            let jb = self.jacobian_product(state, &base, false)?;
            let remainder = direction
                .iter()
                .zip(jb)
                .map(|(v, j)| v - j)
                .collect::<Vec<_>>();
            let moment = coarse.restrict(&remainder, false)?;
            let solved = coarse.solve(&moment, false)?;
            let correction = coarse.lift(&solved, false)?;
            Ok(base.iter().zip(correction).map(|(m, c)| m + c).collect())
        }
    }
}
