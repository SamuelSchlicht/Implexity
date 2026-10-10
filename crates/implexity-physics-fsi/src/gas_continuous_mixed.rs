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

use crate::gas_continuous_layout::{ContinuousGasLayout, ContinuousGasRepresentation};
use crate::gas_continuum_fv::{ContinuumFvInputs, ContinuumFvPolicy, ContinuumFvResult};
use crate::gas_quadratic_network::{
    PreparedQuadraticGasNetwork, QuadraticGasBoundary, QuadraticGasNetworkPacket,
    QuadraticGasStateAccess,
};
use crate::gas_transient_kinetic::{
    TransientGasBoundary, TransientGasFace, TransientGasPacket, TransientGasScalar,
};
use implexity_core::{CaeError, CaeResult};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KineticResolvedSgsRegion {
    pub cells: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stencil_neighbors: Option<BTreeMap<usize, Vec<ResolvedSgsStencilNeighbor>>>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSgsStencilNeighbor {
    pub slot: usize,
    #[serde(default)]
    pub periodic_image: [i32; 3],
}
struct ResolvedSgsPlan {
    region: KineticResolvedSgsRegion,
    stencils: BTreeMap<usize, Vec<ResolvedSgsStencilNeighbor>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuousTransportOwnership {
    pub resolved_sgs_cells: Vec<usize>,
    pub molecular_kinetic_cells: Vec<usize>,
}
pub struct ContinuousMixedOutputBars {
    pub residual: Vec<f64>,
    pub boundary_moments: std::collections::BTreeMap<usize, [f64; 10]>,
    pub boundary_wall_work_j: std::collections::BTreeMap<usize, f64>,
    pub boundary_wall_heat_j: std::collections::BTreeMap<usize, f64>,
}

pub struct ContinuousMixedResult<S> {
    pub residual: Vec<S>,
    pub boundary_packets: Vec<QuadraticGasNetworkPacket<S>>,
    pub continuum: ContinuumFvResult<S>,
    pub kinetic_collision_heat_j: S,
}
pub struct ContinuousMixedNetwork<S: TransientGasScalar> {
    network: PreparedQuadraticGasNetwork<S>,
    layout: ContinuousGasLayout,
    continuum_policy: ContinuumFvPolicy,
    resolved_sgs: Option<std::sync::Arc<ResolvedSgsPlan>>,
    faces: Vec<TransientGasFace<S>>,
    kinetic_cells: Vec<usize>,
    continuum_cells: Vec<usize>,
}
pub(crate) struct ContinuousMixedMomentCache<'a, S: TransientGasScalar> {
    owner: &'a ContinuousMixedNetwork<S>,
    current: Vec<[S; 4]>,
    previous: Vec<[S; 4]>,
}
impl<S: TransientGasScalar> ContinuousMixedMomentCache<'_, S> {
    pub(crate) fn evaluate_cells(
        &self,
        current: &[S],
        previous: &[S],
        ports: &[Vec<S>],
        rt: S,
        viscosity: S,
        selected: &[usize],
    ) -> CaeResult<ContinuousMixedResult<S>> {
        let mut current_moments = self.current.clone();
        let mut previous_moments = self.previous.clone();
        for cell in self.owner.dependency_cells(selected)? {
            current_moments[cell] = self.owner.layout.macro_inventory(current, cell)?;
            previous_moments[cell] = self.owner.layout.macro_inventory(previous, cell)?;
        }
        self.owner.evaluate_cells_with_moments(
            current,
            previous,
            ports,
            rt,
            viscosity,
            selected,
            current_moments,
            previous_moments,
        )
    }
}
struct MixedAccess<'a, S: TransientGasScalar> {
    owner: &'a ContinuousMixedNetwork<S>,
    current: &'a [S],
    previous: &'a [S],
    current_moments: &'a [[S; 4]],
    previous_moments: &'a [[S; 4]],
    reconstructed: &'a ContinuumFvResult<S>,
    ports: &'a [Vec<S>],
    rt: S,
    viscosity: S,
    cache: Mutex<BTreeMap<(usize, bool), Vec<S>>>,
}
impl<S: TransientGasScalar> QuadraticGasStateAccess<S> for MixedAccess<'_, S> {
    fn coefficient_inventory(&self, cell: usize, previous: bool) -> CaeResult<Option<Vec<S>>> {
        let g = self.owner.network.geometry();
        let volume = if previous {
            g.previous_volume_m3[cell]
        } else {
            g.volume_m3[cell]
        };
        if volume.value() == 0. {
            return Ok(None);
        }
        let values = if previous {
            self.previous
        } else {
            self.current
        };
        if self.owner.layout.representation(cell)? == ContinuousGasRepresentation::Kinetic {
            return Ok(Some(
                self.owner.layout.kinetic_inventory(values, cell)?.to_vec(),
            ));
        }
        if let Some(q) = self
            .cache
            .lock()
            .map_err(|_| CaeError::contract("mixed coefficient cache poisoned"))?
            .get(&(cell, previous))
        {
            return Ok(Some(q.clone()));
        }
        let m = if previous {
            self.previous_moments[cell]
        } else {
            self.current_moments[cell]
        };
        let rho = m[0] / volume;
        let u = std::array::from_fn(|j| m[j + 1] / m[0]);
        let density = if previous {
            self.owner
                .network
                .gaussian()
                .equilibrium(rho, u, self.rt, &self.owner.network.policy().entropy)?
                .coefficient_density
        } else {
            let velocity_gradient: [[S; 3]; 3] = std::array::from_fn(|i| {
                std::array::from_fn(|j| {
                    (self.reconstructed.momentum_gradients[cell][i][j]
                        - u[i] * self.reconstructed.density_gradients[cell][j])
                        / rho
                })
            });
            let mu = self.reconstructed.transport_viscosity_pa_s[cell];
            let mut covariance = [[S::zero(); 3]; 3];
            for i in 0..3 {
                for j in i..3 {
                    let value = (if i == j { self.rt } else { S::zero() })
                        - mu * (velocity_gradient[i][j] + velocity_gradient[j][i]) / rho;
                    covariance[i][j] = value;
                    covariance[j][i] = value;
                }
            }
            self.owner
                .network
                .gaussian()
                .gaussian(rho, u, covariance, &self.owner.network.policy().entropy)?
                .coefficient_density
        };
        let q = density.into_iter().map(|f| f * volume).collect::<Vec<_>>();
        self.cache
            .lock()
            .map_err(|_| CaeError::contract("mixed coefficient cache poisoned"))?
            .insert((cell, previous), q.clone());
        Ok(Some(q))
    }
    fn port_density(&self, port: usize) -> CaeResult<Vec<S>> {
        self.ports
            .get(port)
            .cloned()
            .ok_or_else(|| CaeError::contract("mixed continuous port missing"))
    }
    fn gas_rt_m2_s2(&self) -> S {
        self.rt
    }
    fn molecular_viscosity_pa_s(&self) -> S {
        self.viscosity
    }
    fn transport_viscosity_pa_s(&self, cell: usize) -> CaeResult<S> {
        self.reconstructed
            .transport_viscosity_pa_s
            .get(cell)
            .copied()
            .ok_or_else(|| CaeError::contract("mixed transport viscosity cell missing"))
    }
}
impl<S: TransientGasScalar> ContinuousMixedNetwork<S> {
    pub fn new(
        network: PreparedQuadraticGasNetwork<S>,
        layout: ContinuousGasLayout,
        continuum_policy: ContinuumFvPolicy,
    ) -> CaeResult<Self> {
        let g = network.geometry();
        if layout.slots() != g.volume_m3.len()
            || layout.basis().coefficient_count() != network.basis().coefficient_count()
        {
            return Err(CaeError::contract(
                "continuous mixed geometry/layout mismatch",
            ));
        }
        for axis in 0..3 {
            if layout.basis().knots(axis)? != network.basis().knots(axis)? {
                return Err(CaeError::contract(
                    "continuous mixed velocity basis mismatch",
                ));
            }
        }
        if layout.basis().reference_speed_m_s() != network.basis().reference_speed_m_s() {
            return Err(CaeError::contract(
                "continuous mixed numerical moment scale mismatch",
            ));
        }
        let mut kinetic_cells = Vec::new();
        let mut continuum_cells = Vec::new();
        for c in 0..layout.slots() {
            match layout.representation(c)? {
                ContinuousGasRepresentation::Kinetic => {
                    if g.volume_m3[c].value() > 0. {
                        kinetic_cells.push(c);
                    }
                }
                ContinuousGasRepresentation::Continuum => continuum_cells.push(c),
            }
        }
        let faces = g
            .faces
            .iter()
            .map(|f| TransientGasFace {
                left: f.left,
                boundary: if let Some(right) = f.right {
                    TransientGasBoundary::Interior { right }
                } else {
                    match f.boundary.as_ref().unwrap() {
                        QuadraticGasBoundary::Port { port } => {
                            TransientGasBoundary::Port { port: *port }
                        }
                        QuadraticGasBoundary::DiffuseWall => {
                            TransientGasBoundary::DiffuseWall { wall: 0 }
                        }
                    }
                },
                area_m2: f.area_m2,
                normal: f.outward_unit_normal,
                ale_velocity_m_s: f.ale_velocity_m_s,
                wall_velocity_m_s: f.material_wall_velocity_m_s,
            })
            .collect();
        Ok(Self {
            network,
            layout,
            continuum_policy,
            resolved_sgs: None,
            faces,
            kinetic_cells,
            continuum_cells,
        })
    }
    pub fn with_resolved_sgs(mut self, region: KineticResolvedSgsRegion) -> CaeResult<Self> {
        if self.continuum_policy.wale.is_none() {
            return Err(CaeError::contract(
                "resolved SGS region requires an authored WALE model",
            ));
        }
        let selected = region
            .cells
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if selected.len() != region.cells.len()
            || region.stencil_neighbors.as_ref().is_some_and(|m| {
                m.len() != selected.len() || m.keys().any(|c| !selected.contains(c))
            })
        {
            return Err(CaeError::contract(
                "resolved SGS region has duplicate or missing stencil ownership",
            ));
        }
        let g = self.network.geometry();
        let mut stencils = BTreeMap::new();
        for &cell in &selected {
            if cell >= self.layout.slots()
                || self.layout.representation(cell)? != ContinuousGasRepresentation::Kinetic
                || g.volume_m3[cell].value() <= 0.
                || g.previous_volume_m3[cell].value() <= 0.
            {
                return Err(CaeError::contract(
                    "resolved SGS kinetic cell cannot be a born or closed capacity",
                ));
            }
            let mut authentic = std::collections::BTreeSet::new();
            for face in &g.faces {
                if face.area_m2.value() <= 0. {
                    continue;
                }
                let Some(right) = face.right else {
                    continue;
                };
                let (other, image) = if face.left == cell {
                    (right, face.right_periodic_image)
                } else if right == cell {
                    let mut image = face.right_periodic_image;
                    for v in &mut image {
                        *v = v.checked_neg().ok_or_else(|| {
                            CaeError::contract("resolved SGS periodic image overflow")
                        })?;
                    }
                    (face.left, image)
                } else {
                    continue;
                };
                if self.layout.representation(other)? == ContinuousGasRepresentation::Continuum
                    || selected.contains(&other)
                {
                    authentic.insert(ResolvedSgsStencilNeighbor {
                        slot: other,
                        periodic_image: image,
                    });
                }
            }
            let stencil = region
                .stencil_neighbors
                .as_ref()
                .map(|m| m[&cell].clone())
                .unwrap_or_else(|| authentic.iter().cloned().collect());
            if stencil
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != stencil.len()
                || stencil.iter().any(|v| !authentic.contains(v))
            {
                return Err(CaeError::contract(
                    "resolved SGS stencil is not an authentic resolved neighbor-slot/image",
                ));
            }
            for neighbor in &stencil {
                if g.volume_m3[neighbor.slot].value() <= 0.
                    || g.previous_volume_m3[neighbor.slot].value() <= 0.
                {
                    return Err(CaeError::contract(
                        "resolved SGS stencil requires persistent resolved-fluid capacity",
                    ));
                }
            }
            stencils.insert(cell, stencil);
        }
        self.resolved_sgs = Some(std::sync::Arc::new(ResolvedSgsPlan { region, stencils }));
        Ok(self)
    }
    pub fn resolved_sgs_stencils(
        &self,
    ) -> Option<&BTreeMap<usize, Vec<ResolvedSgsStencilNeighbor>>> {
        self.resolved_sgs.as_ref().map(|p| &p.stencils)
    }
    pub fn transport_ownership(&self) -> ContinuousTransportOwnership {
        let mut resolved = if self.continuum_policy.wale.is_some() {
            self.continuum_cells.clone()
        } else {
            Vec::new()
        };
        if let Some(region) = &self.resolved_sgs {
            resolved.extend(&region.region.cells);
        }
        resolved.sort_unstable();
        let molecular = (0..self.layout.slots())
            .filter(|c| {
                self.layout.representation(*c).unwrap() == ContinuousGasRepresentation::Kinetic
                    && !resolved.contains(c)
            })
            .collect();
        ContinuousTransportOwnership {
            resolved_sgs_cells: resolved,
            molecular_kinetic_cells: molecular,
        }
    }
    pub fn constant_lift<T: TransientGasScalar>(&self) -> CaeResult<ContinuousMixedNetwork<T>> {
        let mut lifted = ContinuousMixedNetwork::new(
            self.network.constant_lift(),
            self.layout.clone(),
            self.continuum_policy.clone(),
        )?;
        lifted.resolved_sgs = self.resolved_sgs.clone();
        Ok(lifted)
    }
    pub fn layout(&self) -> &ContinuousGasLayout {
        &self.layout
    }
    pub fn prepared(&self) -> &PreparedQuadraticGasNetwork<S> {
        &self.network
    }
    fn reconstruct_transport(
        &self,
        input: &ContinuumFvInputs<S>,
        continuum: &[usize],
        support: &[usize],
    ) -> CaeResult<ContinuumFvResult<S>> {
        let mut reconstructed = crate::gas_continuum_fv::reconstruct_cells_scalar(
            input,
            &self.continuum_policy,
            continuum,
        )?;
        if let (Some(wale), Some(region)) = (&self.continuum_policy.wale, &self.resolved_sgs) {
            for &cell in &region.region.cells {
                if input.previous_volume_m3[cell].value() <= 0.
                    || input.volume_m3[cell].value() <= 0.
                    || input.previous_inventory[cell][0].value() <= 0.
                    || input.current_inventory[cell][0].value() <= 0.
                {
                    return Err(CaeError::contract(
                        "resolved SGS cell has no admitted vacuum or birth velocity field",
                    ));
                }
            }
            for stencil in region.stencils.values() {
                for neighbor in stencil {
                    if input.previous_inventory[neighbor.slot][0].value() <= 0.
                        || input.current_inventory[neighbor.slot][0].value() <= 0.
                    {
                        return Err(CaeError::contract(
                            "resolved SGS stencil has no admitted vacuum or birth sample",
                        ));
                    }
                }
            }
            let n = self.layout.slots();
            let mut density = vec![[S::zero(); 4]; n];
            for i in 0..n {
                if input.volume_m3[i].value() > 0. {
                    density[i] = input.current_inventory[i].map(|v| v / input.volume_m3[i]);
                }
            }
            let mut neighbors = vec![Vec::new(); n];
            for (id, face) in input.faces.iter().enumerate() {
                if face.area_m2.value() > 0. {
                    if let TransientGasBoundary::Interior { right } = face.boundary {
                        let r = input.right_centroid_to_face_m[id].ok_or_else(|| {
                            CaeError::contract("transport face requires both centroid offsets")
                        })?;
                        let dx =
                            std::array::from_fn(|j| input.left_centroid_to_face_m[id][j] - r[j]);
                        neighbors[face.left].push((right, dx));
                        neighbors[right].push((face.left, dx.map(|v| -v)));
                    }
                }
            }
            for &cell in &region.region.cells {
                neighbors[cell].clear();
                for other in &region.stencils[&cell] {
                    let g = self.network.geometry();
                    let dx = std::array::from_fn(|axis| {
                        g.cell_centroid_m[other.slot][axis] - g.cell_centroid_m[cell][axis]
                            + g.periodic_basis_m.map_or(S::zero(), |basis| {
                                (0..3).fold(S::zero(), |v, j| {
                                    v + basis[j][axis] * other.periodic_image[j] as f64
                                })
                            })
                    });
                    neighbors[cell].push((other.slot, dx));
                }
            }
            let selected = support
                .iter()
                .copied()
                .filter(|c| {
                    self.layout.representation(*c).unwrap()
                        == ContinuousGasRepresentation::Continuum
                        || region.region.cells.contains(c)
                })
                .collect();
            let transport = crate::gas_continuous_transport::reconstruct(
                &density,
                &neighbors,
                &selected,
                self.continuum_policy.gradient_axes,
                self.continuum_policy.maximum_gradient_condition_number,
                input.viscosity_pa_s,
                input.step_s,
                Some((&wale.model, wale.spacing_m)),
            )?;
            reconstructed.transport_viscosity_pa_s = transport.transport_viscosity_pa_s;
        }
        Ok(reconstructed)
    }
    pub fn dependency_cells(&self, selected: &[usize]) -> CaeResult<Vec<usize>> {
        let first = self.network.dependency_cells(selected)?;
        let mut support = first
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for face in &self.faces {
            if face.area_m2.value() > 0. {
                if let TransientGasBoundary::Interior { right } = face.boundary {
                    if first.contains(&face.left) || first.contains(&right) {
                        support.insert(face.left);
                        support.insert(right);
                    }
                }
            }
        }
        Ok(support.into_iter().collect())
    }
    pub fn evaluate(
        &self,
        current: &[S],
        previous: &[S],
        ports: &[Vec<S>],
        rt: S,
        viscosity: S,
    ) -> CaeResult<ContinuousMixedResult<S>> {
        let selected = self
            .kinetic_cells
            .iter()
            .chain(&self.continuum_cells)
            .copied()
            .collect::<Vec<_>>();
        self.evaluate_cells(current, previous, ports, rt, viscosity, &selected)
    }
    pub fn evaluate_cells(
        &self,
        current: &[S],
        previous: &[S],
        ports: &[Vec<S>],
        rt: S,
        viscosity: S,
        selected: &[usize],
    ) -> CaeResult<ContinuousMixedResult<S>> {
        self.layout
            .validate(current, &self.network.geometry().volume_m3)?;
        self.layout
            .validate(previous, &self.network.geometry().previous_volume_m3)?;
        let current_moments = (0..self.layout.slots())
            .map(|c| self.layout.macro_inventory(current, c))
            .collect::<CaeResult<Vec<_>>>()?;
        let previous_moments = (0..self.layout.slots())
            .map(|c| self.layout.macro_inventory(previous, c))
            .collect::<CaeResult<Vec<_>>>()?;
        self.evaluate_cells_with_moments(
            current,
            previous,
            ports,
            rt,
            viscosity,
            selected,
            current_moments,
            previous_moments,
        )
    }
    pub(crate) fn moment_cache<'a>(
        &'a self,
        current: &[S],
        previous: &[S],
    ) -> CaeResult<ContinuousMixedMomentCache<'a, S>> {
        if current
            .iter()
            .chain(previous)
            .any(|value| value.has_direction())
        {
            return Err(CaeError::contract(
                "mixed moment cache requires constant baseline inventories",
            ));
        }
        self.layout
            .validate(current, &self.network.geometry().volume_m3)?;
        self.layout
            .validate(previous, &self.network.geometry().previous_volume_m3)?;
        Ok(ContinuousMixedMomentCache {
            owner: self,
            current: (0..self.layout.slots())
                .map(|c| self.layout.macro_inventory(current, c))
                .collect::<CaeResult<Vec<_>>>()?,
            previous: (0..self.layout.slots())
                .map(|c| self.layout.macro_inventory(previous, c))
                .collect::<CaeResult<Vec<_>>>()?,
        })
    }
    fn evaluate_cells_with_moments(
        &self,
        current: &[S],
        previous: &[S],
        ports: &[Vec<S>],
        rt: S,
        viscosity: S,
        selected: &[usize],
        current_moments: Vec<[S; 4]>,
        previous_moments: Vec<[S; 4]>,
    ) -> CaeResult<ContinuousMixedResult<S>> {
        self.network.certify_output_cells(selected)?;
        let g = self.network.geometry();
        if selected
            .iter()
            .any(|c| *c >= self.layout.slots() || g.volume_m3[*c].value() <= 0.)
        {
            return Err(CaeError::contract("continuous mixed output slot invalid"));
        }
        let selected_kinetic = selected
            .iter()
            .filter(|c| {
                self.layout.representation(**c).unwrap() == ContinuousGasRepresentation::Kinetic
            })
            .copied()
            .collect::<Vec<_>>();
        let selected_continuum = selected
            .iter()
            .filter(|c| {
                self.layout.representation(**c).unwrap() == ContinuousGasRepresentation::Continuum
            })
            .copied()
            .collect::<Vec<_>>();
        let support = self.network.dependency_cells(selected)?;
        let reconstruct_continuum = support
            .iter()
            .filter(|c| {
                self.layout.representation(**c).unwrap() == ContinuousGasRepresentation::Continuum
            })
            .copied()
            .collect::<Vec<_>>();
        let mut input = ContinuumFvInputs {
            continuum_cells: (0..self.layout.slots())
                .map(|c| {
                    self.layout.representation(c).unwrap() == ContinuousGasRepresentation::Continuum
                })
                .collect(),
            current_inventory: current_moments.clone(),
            previous_inventory: previous_moments.clone(),
            volume_m3: g.volume_m3.clone(),
            previous_volume_m3: g.previous_volume_m3.clone(),
            faces: self.faces.clone(),
            left_centroid_to_face_m: g.faces.iter().map(|f| f.left_centroid_to_face_m).collect(),
            right_centroid_to_face_m: g.faces.iter().map(|f| f.right_centroid_to_face_m).collect(),
            shared_face_packets: vec![None; g.faces.len()],
            gas_rt_m2_s2: rt,
            viscosity_pa_s: viscosity,
            step_s: g.physical_step_s,
        };
        let reconstructed = self.reconstruct_transport(&input, &reconstruct_continuum, &support)?;
        let access = MixedAccess {
            owner: self,
            current,
            previous,
            current_moments: &current_moments,
            previous_moments: &previous_moments,
            reconstructed: &reconstructed,
            ports,
            rt,
            viscosity,
            cache: Mutex::new(BTreeMap::new()),
        };
        let mut boundary_packets = Vec::new();
        for (id, f) in g.faces.iter().enumerate() {
            let shared = f.right.is_none()
                || self.layout.representation(f.left)? == ContinuousGasRepresentation::Kinetic
                || f.right
                    .map(|r| {
                        self.layout
                            .representation(r)
                            .map(|p| p == ContinuousGasRepresentation::Kinetic)
                    })
                    .transpose()?
                    .unwrap_or(false);
            let incident =
                selected.contains(&f.left) || f.right.is_some_and(|r| selected.contains(&r));
            if shared && incident && f.area_m2.value() > 0. {
                let packet = self.network.face_packet(&access, id)?;
                let m = packet.moments;
                input.shared_face_packets[id] = Some(TransientGasPacket {
                    mass_kg: m[0],
                    momentum_kg_m_s: [m[1], m[2], m[3]],
                    kinetic_energy_j: (m[4] + m[5] + m[6]) * S::from_f64(0.5),
                    wall_work_j: packet.wall_work_j,
                    wall_heat_j: packet.wall_heat_j,
                });
                if f.right.is_none() && selected.contains(&f.left) {
                    boundary_packets.push(packet);
                }
            }
        }
        let kinetic = self
            .network
            .evaluate_cells_parallel(&access, &selected_kinetic)?;
        let continuum = crate::gas_continuum_fv::evaluate_cells_scalar(
            &input,
            &self.continuum_policy,
            &selected_continuum,
        )?;
        let mut residual = vec![S::zero(); self.layout.state_size()];
        let mut kinetic_collision_heat_j = S::zero();
        for (cell, value) in kinetic.cells {
            let range = self.layout.range(cell)?;
            residual[range].copy_from_slice(&value.weak_residual);
            kinetic_collision_heat_j =
                kinetic_collision_heat_j + value.collision_thermal_bath_energy_j;
        }
        for &cell in &selected_continuum {
            let range = self.layout.range(cell)?;
            residual[range].copy_from_slice(&continuum.inventory_residual[cell]);
        }
        Ok(ContinuousMixedResult {
            residual,
            boundary_packets,
            continuum,
            kinetic_collision_heat_j,
        })
    }
}

impl<S: TransientGasScalar> ContinuousMixedNetwork<S> {
    pub fn weighted_selected_output(
        &self,
        current: &[S],
        previous: &[S],
        ports: &[Vec<S>],
        rt: S,
        viscosity: S,
        selected: &[usize],
        bars: &ContinuousMixedOutputBars,
    ) -> CaeResult<S> {
        let geometry = self.network.geometry();
        if bars.residual.len() != self.layout.state_size()
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
                .any(|f| *f >= geometry.faces.len() || geometry.faces[*f].right.is_some())
        {
            return Err(CaeError::contract(
                "continuous mixed weighted output cotangent invalid",
            ));
        }
        let unique = selected
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != selected.len() {
            return Err(CaeError::contract(
                "continuous mixed weighted output cells duplicate",
            ));
        }
        let result = self.evaluate_cells(current, previous, ports, rt, viscosity, selected)?;
        let mut objective = S::zero();
        for cell in selected {
            let range = self.layout.range(*cell)?;
            for (value, bar) in result.residual[range.clone()]
                .iter()
                .zip(&bars.residual[range])
            {
                objective = objective + *value * S::from_f64(*bar);
            }
        }
        for packet in result.boundary_packets {
            if let Some(bar) = bars.boundary_moments.get(&packet.face) {
                for j in 0..10 {
                    objective = objective + packet.moments[j] * S::from_f64(bar[j]);
                }
            }
            if let Some(bar) = bars.boundary_wall_work_j.get(&packet.face) {
                objective = objective + packet.wall_work_j * S::from_f64(*bar);
            }
            if let Some(bar) = bars.boundary_wall_heat_j.get(&packet.face) {
                objective = objective + packet.wall_heat_j * S::from_f64(*bar);
            }
        }
        if !objective.finite_first_order() {
            return Err(CaeError::contract(
                "continuous mixed weighted output nonfinite",
            ));
        }
        Ok(objective)
    }
}

impl ContinuousMixedNetwork<f64> {
    pub(crate) fn constant_weak_transport_cells(
        &self,
        current: &[f64],
        previous: &[f64],
        ports: &[Vec<f64>],
        rt: f64,
        viscosity: f64,
        selected: &[usize],
    ) -> CaeResult<
        Vec<(
            usize,
            crate::gas_quadratic_network::ConstantWeakTransportCell,
        )>,
    > {
        self.weak_transport_cells(current, previous, ports, rt, viscosity, selected, false)
    }
    pub(crate) fn constant_surrogate_weak_transport_cells(
        &self,
        current: &[f64],
        previous: &[f64],
        ports: &[Vec<f64>],
        rt: f64,
        viscosity: f64,
        selected: &[usize],
    ) -> CaeResult<
        Vec<(
            usize,
            crate::gas_quadratic_network::ConstantWeakTransportCell,
        )>,
    > {
        self.weak_transport_cells(current, previous, ports, rt, viscosity, selected, true)
    }
    fn weak_transport_cells(
        &self,
        current: &[f64],
        previous: &[f64],
        ports: &[Vec<f64>],
        rt: f64,
        viscosity: f64,
        selected: &[usize],
        numerical_surrogate: bool,
    ) -> CaeResult<
        Vec<(
            usize,
            crate::gas_quadratic_network::ConstantWeakTransportCell,
        )>,
    > {
        self.network.certify_output_cells(selected)?;
        self.layout
            .validate(current, &self.network.geometry().volume_m3)?;
        self.layout
            .validate(previous, &self.network.geometry().previous_volume_m3)?;
        if selected.is_empty()
            || (!numerical_surrogate
                && !matches!(
                    self.network.policy().reconstruction,
                    crate::gas_quadratic_network::QuadraticGasReconstruction::Constant
                ))
        {
            return Err(CaeError::contract(
                "constant weak transport numerical scope invalid",
            ));
        }
        let g = self.network.geometry();
        let current_moments = (0..self.layout.slots())
            .map(|c| self.layout.macro_inventory(current, c))
            .collect::<CaeResult<Vec<_>>>()?;
        let previous_moments = (0..self.layout.slots())
            .map(|c| self.layout.macro_inventory(previous, c))
            .collect::<CaeResult<Vec<_>>>()?;
        let support = self.network.dependency_cells(selected)?;
        let reconstruct_continuum = support
            .iter()
            .copied()
            .filter(|c| {
                self.layout.representation(*c).unwrap() == ContinuousGasRepresentation::Continuum
            })
            .collect::<Vec<_>>();
        let input = ContinuumFvInputs {
            continuum_cells: (0..self.layout.slots())
                .map(|c| {
                    self.layout.representation(c).unwrap() == ContinuousGasRepresentation::Continuum
                })
                .collect(),
            current_inventory: current_moments.clone(),
            previous_inventory: previous_moments.clone(),
            volume_m3: g.volume_m3.clone(),
            previous_volume_m3: g.previous_volume_m3.clone(),
            faces: self.faces.clone(),
            left_centroid_to_face_m: g.faces.iter().map(|f| f.left_centroid_to_face_m).collect(),
            right_centroid_to_face_m: g.faces.iter().map(|f| f.right_centroid_to_face_m).collect(),
            shared_face_packets: vec![None; g.faces.len()],
            gas_rt_m2_s2: rt,
            viscosity_pa_s: viscosity,
            step_s: g.physical_step_s,
        };
        let reconstructed = self.reconstruct_transport(&input, &reconstruct_continuum, &support)?;
        let access = MixedAccess {
            owner: self,
            current,
            previous,
            current_moments: &current_moments,
            previous_moments: &previous_moments,
            reconstructed: &reconstructed,
            ports,
            rt,
            viscosity,
            cache: Mutex::new(BTreeMap::new()),
        };
        selected
            .iter()
            .map(|&cell| {
                let descriptor = if numerical_surrogate {
                    self.network
                        .constant_surrogate_weak_transport(&access, cell)
                } else {
                    self.network.constant_weak_transport(&access, cell)
                };
                descriptor.map(|v| (cell, v))
            })
            .collect()
    }
}
