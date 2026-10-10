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

use crate::gas_continuum_fv::{
    ContinuumFvInputs, ContinuumFvPolicy, ContinuumFvResult,
    evaluate_cells_scalar as continuum_cells, reconstruct_cells_scalar as continuum_reconstruct,
};
use crate::gas_continuum_lift::{ContinuumGasInputs, ContinuumGasPolicy, lift_with_transport_viscosity_scalar};
use crate::gas_multiscale_network::{
    MultiscaleNetworkInputs, MultiscaleNetworkPolicy, MultiscaleNetworkTopology,
    residual_cells_prepared_scalar,
};
use crate::gas_transient_kinetic::{TransientGasBoundary, TransientGasPacket, TransientGasScalar};
use implexity_core::{CaeError, CaeResult};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct MixedGasLayout {
    continuum: Vec<bool>,
    offsets: Vec<Option<usize>>,
    populations: usize,
    dimension: usize,
}
impl MixedGasLayout {
    pub fn new(continuum: Vec<bool>, active: &[bool], populations: usize) -> CaeResult<Self> {
        if continuum.len() != active.len() || populations == 0 {
            return Err(fail("invalid mixed gas state layout"));
        }
        let mut offsets = Vec::with_capacity(active.len());
        let mut dimension = 0usize;
        for i in 0..active.len() {
            if continuum[i] && !active[i] {
                return Err(fail("zero capacity cannot use continuum representation"));
            }
            if active[i] {
                offsets.push(Some(dimension));
                dimension = dimension
                    .checked_add(if continuum[i] { 4 } else { populations })
                    .ok_or_else(|| fail("mixed gas state dimension overflow"))?;
            } else {
                offsets.push(None);
            }
        }
        Ok(Self {
            continuum,
            offsets,
            populations,
            dimension,
        })
    }
    pub fn cell_offset(&self, cell: usize) -> Option<usize> {
        self.offsets.get(cell).copied().flatten()
    }
    pub fn population_count(&self) -> usize {
        self.populations
    }
    pub fn dimension(&self) -> usize {
        self.dimension
    }
    pub fn kinetic_projection<S: TransientGasScalar>(&self, state: &[S]) -> CaeResult<Vec<S>> {
        if state.len() != self.dimension {
            return Err(fail("mixed checkpoint shape mismatch"));
        }
        let mut out = vec![S::zero(); self.offsets.len() * self.populations];
        for (i, o) in self.offsets.iter().enumerate() {
            if !self.continuum[i] {
                if let Some(o) = *o {
                    out[i * self.populations..(i + 1) * self.populations]
                        .copy_from_slice(&state[o..o + self.populations]);
                }
            }
        }
        Ok(out)
    }
    pub fn with_kinetic_projection<S: TransientGasScalar>(
        &self,
        state: &[S],
        inventory: &[S],
    ) -> CaeResult<Vec<S>> {
        if state.len() != self.dimension || inventory.len() != self.offsets.len() * self.populations
        {
            return Err(fail("mixed remapped inventory shape mismatch"));
        }
        let mut out = state.to_vec();
        for (i, o) in self.offsets.iter().enumerate() {
            if !self.continuum[i] {
                if let Some(o) = *o {
                    out[o..o + self.populations].copy_from_slice(
                        &inventory[i * self.populations..(i + 1) * self.populations],
                    );
                }
            }
        }
        Ok(out)
    }
    pub fn continuum_cells(&self) -> &[bool] {
        &self.continuum
    }
    pub fn pack<S: TransientGasScalar>(
        &self,
        inventory: &[S],
        velocity: &[[f64; 3]],
    ) -> CaeResult<Vec<S>> {
        if velocity.len() != self.populations
            || inventory.len() != self.offsets.len() * self.populations
        {
            return Err(fail("mixed gas packing shape mismatch"));
        }
        let mut out = vec![S::zero(); self.dimension];
        for (i, offset) in self.offsets.iter().enumerate() {
            let Some(o) = *offset else {
                if inventory[i * self.populations..(i + 1) * self.populations]
                    .iter()
                    .any(|q| q.value() != 0.0 || q.has_direction())
                {
                    return Err(fail("closed mixed gas slot contains inventory"));
                }
                continue;
            };
            if self.continuum[i] {
                for k in 0..self.populations {
                    let q = inventory[i * self.populations + k];
                    out[o] = out[o] + q;
                    for j in 0..3 {
                        out[o + j + 1] = out[o + j + 1] + q * velocity[k][j];
                    }
                }
            } else {
                out[o..o + self.populations]
                    .copy_from_slice(&inventory[i * self.populations..(i + 1) * self.populations]);
            }
        }
        Ok(out)
    }
    pub(crate) fn moments<S: TransientGasScalar>(
        &self,
        state: &[S],
        velocity: &[[f64; 3]],
    ) -> CaeResult<Vec<[S; 4]>> {
        if state.len() != self.dimension || velocity.len() != self.populations {
            return Err(fail("mixed gas state shape mismatch"));
        }
        let mut out = vec![[S::zero(); 4]; self.offsets.len()];
        for (i, o) in self.offsets.iter().enumerate() {
            let Some(o) = *o else {
                continue;
            };
            if self.continuum[i] {
                out[i].copy_from_slice(&state[o..o + 4]);
            } else {
                for k in 0..self.populations {
                    let q = state[o + k];
                    if !q.finite_first_order() || q.value() < 0.0 {
                        return Err(fail("negative or nonfinite mixed kinetic inventory"));
                    }
                    out[i][0] = out[i][0] + q;
                    for j in 0..3 {
                        out[i][j + 1] = out[i][j + 1] + q * velocity[k][j];
                    }
                }
            }
        }
        Ok(out)
    }
}
#[derive(Clone)]
pub struct MixedGasPolicy {
    pub network: MultiscaleNetworkPolicy,
    pub continuum: ContinuumFvPolicy,
    pub lift: ContinuumGasPolicy,
}
pub struct MixedGasResidual<S = f64> {
    pub inventory_residual: Vec<S>,
    pub face_packets: Vec<TransientGasPacket<S>>,
    pub continuum: ContinuumFvResult<S>,
    pub kinetic_cells: Vec<usize>,
    pub kinetic_collision_bath_energy_j: Vec<S>,
    pub lifted_continuum_cells: usize,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn zero<S: TransientGasScalar>() -> TransientGasPacket<S> {
    TransientGasPacket {
        mass_kg: S::zero(),
        momentum_kg_m_s: [S::zero(); 3],
        kinetic_energy_j: S::zero(),
        wall_work_j: S::zero(),
        wall_heat_j: S::zero(),
    }
}
fn fv<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    layout: &MixedGasLayout,
    moments: Vec<[S; 4]>,
    previous: Vec<[S; 4]>,
) -> ContinuumFvInputs<S> {
    ContinuumFvInputs {
        continuum_cells: layout.continuum.clone(),
        current_inventory: moments,
        previous_inventory: previous,
        volume_m3: a.gas.volume_m3.clone(),
        previous_volume_m3: a.gas.previous_volume_m3.clone(),
        faces: a.gas.faces.clone(),
        left_centroid_to_face_m: a.left_centroid_to_face_m.clone(),
        right_centroid_to_face_m: a.right_centroid_to_face_m.clone(),
        shared_face_packets: vec![Some(zero()); a.gas.faces.len()],
        gas_rt_m2_s2: a.gas.gas_rt_m2_s2,
        viscosity_pa_s: a.gas.viscosity_pa_s,
        step_s: a.gas.step_s,
    }
}
pub fn physical_residual_scalar<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    layout: &MixedGasLayout,
    state: &[S],
    previous_state: &[S],
    policy: &MixedGasPolicy,
) -> CaeResult<MixedGasResidual<S>> {
    residual_inner(a, layout, state, previous_state, policy, None)
}
pub(crate) fn residual_cells_scalar<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    layout: &MixedGasLayout,
    state: &[S],
    previous_state: &[S],
    policy: &MixedGasPolicy,
    cells: &[usize],
) -> CaeResult<MixedGasResidual<S>> {
    residual_inner(a, layout, state, previous_state, policy, Some(cells))
}

fn residual_inner<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    layout: &MixedGasLayout,
    state: &[S],
    previous_state: &[S],
    policy: &MixedGasPolicy,
    selected_cells: Option<&[usize]>,
) -> CaeResult<MixedGasResidual<S>> {
    let n = a.gas.volume_m3.len();
    let nv = a.gas.grid.velocity_m_s.len();
    if n != layout.offsets.len()
        || nv != layout.populations
        || policy.network.gradient_axes != policy.continuum.gradient_axes
    {
        return Err(fail("inconsistent mixed gas domain or gradient axes"));
    }
    if a.force_density_n_m3.is_some() {
        return Err(fail("mixed gas force coupling is not yet qualified"));
    }
    for i in 0..n {
        if layout.continuum[i] {
            let dv = a.gas.volume_m3[i] - a.gas.previous_volume_m3[i];
            if dv.value() != 0.0 || dv.has_direction() {
                return Err(fail(
                    "initial mixed continuum route requires a fixed continuum capacity",
                ));
            }
        }
    }
    for f in &a.gas.faces {
        let right = match f.boundary {
            TransientGasBoundary::Interior { right } => Some(right),
            _ => None,
        };
        if layout.continuum[f.left] || right.is_some_and(|r| layout.continuum[r]) {
            if f.ale_velocity_m_s
                .iter()
                .any(|v| v.value() != 0.0 || v.has_direction())
            {
                return Err(fail(
                    "initial mixed continuum chart requires stationary continuum interfaces",
                ));
            }
        }
    }
    let selected = selected_cells
        .map(|v| v.iter().copied().collect::<BTreeSet<_>>())
        .unwrap_or_else(|| (0..n).filter(|&i| layout.offsets[i].is_some()).collect());
    if selected
        .iter()
        .any(|&i| i >= n || layout.offsets[i].is_none())
    {
        return Err(fail("invalid mixed residual output cells"));
    }
    let continuum_output = selected
        .iter()
        .copied()
        .filter(|&i| layout.continuum[i])
        .collect::<Vec<_>>();
    let current_moments = layout.moments(state, &a.gas.grid.velocity_m_s)?;
    let previous_moments = layout.moments(previous_state, &a.gas.grid.velocity_m_s)?;
    let mut c = fv(a, layout, current_moments.clone(), previous_moments.clone());
    for (i, f) in c.faces.iter().enumerate() {
        if let TransientGasBoundary::Interior { right } = f.boundary {
            if layout.continuum[f.left] && layout.continuum[right] {
                c.shared_face_packets[i] = None;
            }
        }
    }
    let topology = MultiscaleNetworkTopology::new_with_policy(a, &policy.network)?;
    let kinetic_cells = (0..n)
        .filter(|&i| selected.contains(&i) && !layout.continuum[i] && layout.offsets[i].is_some())
        .collect::<Vec<_>>();
    let mut output = kinetic_cells.iter().copied().collect::<BTreeSet<_>>();
    for f in &a.gas.faces {
        if !matches!(f.boundary, TransientGasBoundary::Interior { .. })
            && layout.continuum[f.left]
            && selected.contains(&f.left)
        {
            output.insert(f.left);
        }
    }
    for f in &a.gas.faces {
        if let TransientGasBoundary::Interior { right } = f.boundary {
            if selected.contains(&f.left) && layout.continuum[f.left] && !layout.continuum[right] {
                output.insert(right);
            }
            if selected.contains(&right) && layout.continuum[right] && !layout.continuum[f.left] {
                output.insert(f.left);
            }
        }
    }
    let output = output.into_iter().collect::<Vec<_>>();
    if output.is_empty() {
        let continuum = continuum_cells(&c, &policy.continuum, &continuum_output)?;
        let mut residual = vec![S::zero(); layout.dimension];
        for &i in &continuum_output {
            let o = layout.offsets[i].ok_or_else(|| fail("missing continuum state offset"))?;
            residual[o..o + 4].copy_from_slice(&continuum.inventory_residual[i]);
        }
        let face_packets = continuum.face_packets.clone();
        return Ok(MixedGasResidual {
            inventory_residual: residual,
            face_packets,
            continuum,
            kinetic_cells: vec![],
            kinetic_collision_bath_energy_j: vec![],
            lifted_continuum_cells: 0,
        });
    }
    let mut support = BTreeSet::new();
    for &i in &output {
        support.extend(topology.residual_support_cells[i].iter().copied());
    }
    let continuum_support = support
        .iter()
        .copied()
        .filter(|&i| layout.continuum[i])
        .collect::<Vec<_>>();
    let reconstruction = continuum_reconstruct(&c, &policy.continuum, &continuum_support)?;
    let mut oldc = c.clone();
    oldc.current_inventory = previous_moments.clone();
    oldc.previous_inventory = previous_moments.clone();
    oldc.volume_m3 = a.gas.previous_volume_m3.clone();
    oldc.previous_volume_m3 = a.gas.previous_volume_m3.clone();
    let previous_reconstruction =
        continuum_reconstruct(&oldc, &policy.continuum, &continuum_support)?;
    let mut network = a.clone();
    network.gas.previous_inventory_kg = vec![S::zero(); n * nv];
    let mut virtual_current = vec![None; n];
    let mut count = 0;
    for i in 0..n {
        if !layout.continuum[i] {
            if let Some(o) = layout.offsets[i] {
                network.gas.previous_inventory_kg[i * nv..(i + 1) * nv]
                    .copy_from_slice(&previous_state[o..o + nv]);
            }
            continue;
        }
        if !support.contains(&i) {
            continue;
        }
        let closure =
            |moments: &[[S; 4]], gradient: &ContinuumFvResult<S>, volume: S| -> CaeResult<Vec<S>> {
                let mut lp = policy.lift.clone();
                lp.characteristic_length_m = policy.continuum.characteristic_length_m[i];
                lp.forcing_timescale_s = policy.continuum.forcing_timescale_s[i];
                let rho = moments[i][0] / volume;
                let input = ContinuumGasInputs {
                    density_kg_m3: rho,
                    momentum_kg_m2_s: std::array::from_fn(|j| moments[i][j + 1] / volume),
                    density_gradient_kg_m4: gradient.density_gradients[i],
                    momentum_gradient_kg_m3_s: gradient.momentum_gradients[i],
                    ale_velocity_m_s: [S::zero(); 3],
                    gas_rt_m2_s2: a.gas.gas_rt_m2_s2,
                    viscosity_pa_s: a.gas.viscosity_pa_s,
                };
                let lift = lift_with_transport_viscosity_scalar(&input, &a.gas.grid, &lp, gradient.transport_viscosity_pa_s[i])?;
                Ok(lift.density_kg_m3.iter().map(|q| *q * volume).collect())
            };
        virtual_current[i] = Some(closure(
            &current_moments,
            &reconstruction,
            a.gas.volume_m3[i],
        )?);
        let old = closure(
            &previous_moments,
            &previous_reconstruction,
            a.gas.previous_volume_m3[i],
        )?;
        network.gas.previous_inventory_kg[i * nv..(i + 1) * nv].copy_from_slice(&old);
        count += 1;
    }
    let physical = residual_cells_prepared_scalar(
        &network,
        &topology,
        |i, k| {
            if layout.continuum[i] {
                virtual_current[i]
                    .as_ref()
                    .expect("mixed kinetic support includes every continuum dependency")[k]
            } else {
                layout.offsets[i].map_or(S::zero(), |o| state[o + k])
            }
        },
        &output,
        &policy.network,
    )?;
    let mut shared = vec![None; a.gas.faces.len()];
    for (fi, packet) in &physical.face_packets {
        shared[*fi] = Some(packet.clone());
    }
    for (fi, f) in c.faces.iter().enumerate() {
        let analytic = match f.boundary {
            TransientGasBoundary::Interior { right } => {
                layout.continuum[f.left] && layout.continuum[right]
            }
            _ => false,
        };
        if !analytic {
            c.shared_face_packets[fi] = shared[fi].clone();
        }
    }
    let continuum = continuum_cells(&c, &policy.continuum, &continuum_output)?;
    let mut residual = vec![S::zero(); layout.dimension];
    let mut bath = vec![];
    for (i, o) in layout.offsets.iter().enumerate() {
        if let Some(o) = *o {
            if layout.continuum[i] && selected.contains(&i) {
                residual[o..o + 4].copy_from_slice(&continuum.inventory_residual[i]);
            }
        }
    }
    for (j, &i) in output.iter().enumerate() {
        if !layout.continuum[i] && selected.contains(&i) {
            let o = layout.offsets[i].ok_or_else(|| fail("inactive kinetic residual cell"))?;
            residual[o..o + nv]
                .copy_from_slice(&physical.inventory_residual_kg[j * nv..(j + 1) * nv]);
            bath.push(physical.collision_heat_bath_energy_j[j]);
        }
    }
    let mut packets = continuum.face_packets.clone();
    for (fi, packet) in &physical.face_packets {
        let f = &a.gas.faces[*fi];
        let analytic = matches!(f.boundary,TransientGasBoundary::Interior{right} if layout.continuum[f.left] && layout.continuum[right]);
        if !analytic {
            packets[*fi] = packet.clone();
        }
    }

    Ok(MixedGasResidual {
        inventory_residual: residual,
        face_packets: packets,
        continuum,
        kinetic_cells,
        kinetic_collision_bath_energy_j: bath,
        lifted_continuum_cells: count,
    })
}

pub(crate) fn constant_inputs<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<f64>,
) -> MultiscaleNetworkInputs<S> {
    let c = S::from_f64;
    MultiscaleNetworkInputs {
        gas: crate::gas_transient_kinetic::TransientGasInputs {
            volume_m3: a.gas.volume_m3.iter().map(|&v| c(v)).collect(),
            previous_volume_m3: a.gas.previous_volume_m3.iter().map(|&v| c(v)).collect(),
            previous_inventory_kg: a.gas.previous_inventory_kg.iter().map(|&v| c(v)).collect(),
            faces: a
                .gas
                .faces
                .iter()
                .map(|f| crate::gas_transient_kinetic::TransientGasFace {
                    left: f.left,
                    boundary: f.boundary.clone(),
                    area_m2: c(f.area_m2),
                    normal: f.normal.map(c),
                    ale_velocity_m_s: f.ale_velocity_m_s.map(c),
                    wall_velocity_m_s: f.wall_velocity_m_s.map(c),
                })
                .collect(),
            port_density_kg_m3: a
                .gas
                .port_density_kg_m3
                .iter()
                .map(|p| p.iter().map(|&v| c(v)).collect())
                .collect(),
            grid: a.gas.grid.clone(),
            gas_rt_m2_s2: c(a.gas.gas_rt_m2_s2),
            viscosity_pa_s: c(a.gas.viscosity_pa_s),
            step_s: c(a.gas.step_s),
        },
        force_density_n_m3: a
            .force_density_n_m3
            .as_ref()
            .map(|f| f.iter().map(|v| v.map(c)).collect()),
        left_centroid_to_face_m: a.left_centroid_to_face_m.iter().map(|v| v.map(c)).collect(),
        right_centroid_to_face_m: a
            .right_centroid_to_face_m
            .iter()
            .map(|v| v.map(|v| v.map(c)))
            .collect(),
    }
}
#[derive(Clone)]
pub(crate) struct MixedGasLocalPlan {
    inputs: MultiscaleNetworkInputs<f64>,
    layout: MixedGasLayout,
    policy: MixedGasPolicy,
    pub state_columns: Vec<usize>,
    pub output_columns: Vec<(usize, usize)>,
    pub face_columns: Vec<(usize, usize)>,
    pub port_columns: Vec<usize>,
    output_cells: Vec<usize>,
}
impl MixedGasLocalPlan {
    pub(crate) fn new(
        a: &MultiscaleNetworkInputs<f64>,
        layout: &MixedGasLayout,
        policy: &MixedGasPolicy,
        cells: &[usize],
    ) -> CaeResult<Self> {
        let top = MultiscaleNetworkTopology::new_with_policy(a, &policy.network)?;
        Self::new_prepared(a, layout, policy, &top, cells)
    }
    pub(crate) fn new_prepared(
        a: &MultiscaleNetworkInputs<f64>,
        layout: &MixedGasLayout,
        policy: &MixedGasPolicy,
        top: &MultiscaleNetworkTopology,
        cells: &[usize],
    ) -> CaeResult<Self> {
        let n = layout.offsets.len();
        if cells.iter().any(|&i| i >= n || layout.offsets[i].is_none()) {
            return Err(fail("invalid local mixed residual scope"));
        }
        let selected = cells.iter().copied().collect::<BTreeSet<_>>();
        let mut network_outputs = selected
            .iter()
            .copied()
            .filter(|&i| !layout.continuum[i])
            .collect::<BTreeSet<_>>();
        let mut gradients = BTreeSet::new();
        for &i in &selected {
            if layout.continuum[i] {
                gradients.insert(i);
                for &fi in &top.incident_faces[i] {
                    let f = &a.gas.faces[fi];
                    match f.boundary {
                        TransientGasBoundary::Interior { right } => {
                            let j = if f.left == i { right } else { f.left };
                            if layout.continuum[j] {
                                gradients.insert(j);
                            } else {
                                network_outputs.insert(j);
                            }
                        }
                        _ => {
                            network_outputs.insert(i);
                        }
                    }
                }
            }
        }
        let mut support = selected.clone();
        for i in &network_outputs {
            support.extend(top.residual_support_cells[*i].iter().copied());
        }
        gradients.extend(support.iter().copied().filter(|&i| layout.continuum[i]));
        let mut faces = BTreeSet::new();
        for &i in support.iter().chain(&gradients) {
            faces.extend(top.incident_faces[i].iter().copied());
        }
        let mut keep = support;
        for &fi in &faces {
            let f = &a.gas.faces[fi];
            keep.insert(f.left);
            if let TransientGasBoundary::Interior { right } = f.boundary {
                keep.insert(right);
            }
        }
        let indices = keep.into_iter().collect::<Vec<_>>();
        let cell_map = indices
            .iter()
            .enumerate()
            .map(|(new, &old)| (old, new))
            .collect::<std::collections::BTreeMap<_, _>>();
        let face_ids = faces.into_iter().collect::<Vec<_>>();
        let mut port_ids = BTreeSet::new();
        for &fi in &face_ids {
            if let TransientGasBoundary::Port { port } = a.gas.faces[fi].boundary {
                port_ids.insert(port);
            }
        }
        let ports = port_ids.into_iter().collect::<Vec<_>>();
        let port_map = ports
            .iter()
            .enumerate()
            .map(|(new, &old)| (old, new))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut inputs = MultiscaleNetworkInputs {
            gas: crate::gas_transient_kinetic::TransientGasInputs {
                volume_m3: vec![],
                previous_volume_m3: vec![],
                previous_inventory_kg: vec![],
                faces: vec![],
                port_density_kg_m3: vec![],
                grid: a.gas.grid.clone(),
                gas_rt_m2_s2: a.gas.gas_rt_m2_s2,
                viscosity_pa_s: a.gas.viscosity_pa_s,
                step_s: a.gas.step_s,
            },
            force_density_n_m3: None,
            left_centroid_to_face_m: vec![],
            right_centroid_to_face_m: vec![],
        };
        inputs.gas.volume_m3 = indices.iter().map(|&i| a.gas.volume_m3[i]).collect();
        inputs.gas.previous_volume_m3 = indices
            .iter()
            .map(|&i| a.gas.previous_volume_m3[i])
            .collect();
        inputs.gas.previous_inventory_kg = vec![0.; indices.len() * layout.populations];
        inputs.gas.port_density_kg_m3 = ports
            .iter()
            .map(|&i| a.gas.port_density_kg_m3[i].clone())
            .collect();
        inputs.gas.faces = face_ids
            .iter()
            .map(|&fi| {
                let mut f = a.gas.faces[fi].clone();
                f.left = cell_map[&f.left];
                f.boundary = match f.boundary {
                    TransientGasBoundary::Interior { right } => TransientGasBoundary::Interior {
                        right: cell_map[&right],
                    },
                    TransientGasBoundary::Port { port } => TransientGasBoundary::Port {
                        port: port_map[&port],
                    },
                    TransientGasBoundary::DiffuseWall { wall } => {
                        TransientGasBoundary::DiffuseWall { wall }
                    }
                };
                f
            })
            .collect();
        inputs.left_centroid_to_face_m = face_ids
            .iter()
            .map(|&i| a.left_centroid_to_face_m[i])
            .collect();
        inputs.right_centroid_to_face_m = face_ids
            .iter()
            .map(|&i| a.right_centroid_to_face_m[i])
            .collect();
        inputs.force_density_n_m3 = a
            .force_density_n_m3
            .as_ref()
            .map(|f| indices.iter().map(|&i| f[i]).collect());
        let local_layout = MixedGasLayout::new(
            indices.iter().map(|&i| layout.continuum[i]).collect(),
            &indices
                .iter()
                .map(|&i| layout.offsets[i].is_some())
                .collect::<Vec<_>>(),
            layout.populations,
        )?;
        let mut state_columns = vec![];
        let mut output_columns = vec![];
        for (local, &i) in indices.iter().enumerate() {
            if let Some(o) = layout.offsets[i] {
                let count = if layout.continuum[i] {
                    4
                } else {
                    layout.populations
                };
                let local_o = local_layout.offsets[local]
                    .ok_or_else(|| fail("inconsistent local mixed layout"))?;
                for k in 0..count {
                    state_columns.push(o + k);
                    if selected.contains(&i) {
                        output_columns.push((o + k, local_o + k));
                    }
                }
            }
        }
        let mut local_policy = policy.clone();
        local_policy.continuum.characteristic_length_m = indices
            .iter()
            .map(|&i| policy.continuum.characteristic_length_m[i])
            .collect();
        local_policy.continuum.forcing_timescale_s = indices
            .iter()
            .map(|&i| policy.continuum.forcing_timescale_s[i])
            .collect();
        local_policy.network.wall_grazing_pilots =
            policy.network.wall_grazing_pilots.as_ref().map(|p| {
                face_ids
                    .iter()
                    .enumerate()
                    .filter_map(|(new, old)| p.get(old).map(|pilot| (new, pilot.clone())))
                    .collect()
            });
        Ok(Self {
            inputs,
            layout: local_layout,
            policy: local_policy,
            state_columns,
            output_columns,
            face_columns: face_ids
                .iter()
                .enumerate()
                .map(|(new, &old)| (old, new))
                .collect(),
            port_columns: ports,
            output_cells: cells.iter().map(|i| cell_map[i]).collect(),
        })
    }
    pub(crate) fn evaluate<S: TransientGasScalar>(
        &self,
        current: &[S],
        previous: &[S],
    ) -> CaeResult<MixedGasResidual<S>> {
        if current.len() != self.layout.dimension || previous.len() != self.layout.dimension {
            return Err(fail("local mixed state dimension mismatch"));
        }
        residual_cells_scalar(
            &constant_inputs::<S>(&self.inputs),
            &self.layout,
            current,
            previous,
            &self.policy,
            &self.output_cells,
        )
    }
    pub(crate) fn evaluate_with_ports<S: TransientGasScalar>(
        &self,
        current: &[S],
        previous: &[S],
        ports: Vec<Vec<S>>,
    ) -> CaeResult<MixedGasResidual<S>> {
        let mut inputs = constant_inputs::<S>(&self.inputs);
        if ports.len() != inputs.gas.port_density_kg_m3.len()
            || ports.iter().any(|v| v.len() != self.layout.populations)
        {
            return Err(fail("local mixed molecular port shape mismatch"));
        }
        inputs.gas.port_density_kg_m3 = ports;
        residual_cells_scalar(
            &inputs,
            &self.layout,
            current,
            previous,
            &self.policy,
            &self.output_cells,
        )
    }
}
