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

use crate::gas_quadratic_balance::{QuadraticGasBalance, coefficient_balance};
use crate::gas_quadratic_diffuse_wall::{QuadraticDiffuseWallPolicy, QuadraticDiffuseWallState};
use crate::gas_quadratic_gaussian::QuadraticGaussianBasis;
use crate::gas_quadratic_multiscale_flux::{
    PreparedQuadraticMultiscaleFace, QuadraticMultiscaleFaceState, evaluate_prepared_cached,
    prepare_with_gaussian,
};
use crate::gas_quadratic_velocity::{QuadraticEntropyPolicy, QuadraticVelocityBasis};
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum QuadraticGasBoundary {
    Port { port: usize },
    DiffuseWall,
}
#[derive(Clone)]
pub struct QuadraticGasNetworkFace<S> {
    pub left: usize,
    pub right: Option<usize>,
    pub boundary: Option<QuadraticGasBoundary>,
    pub area_m2: S,
    pub outward_unit_normal: [S; 3],
    pub centroid_m: [S; 3],
    pub local_centroid_m: Option<([f64; 3], [S; 3])>,
    pub left_centroid_to_face_m: [S; 3],
    pub right_centroid_to_face_m: Option<[S; 3]>,
    pub right_periodic_image: [i32; 3],
    pub ale_velocity_m_s: [S; 3],
    pub material_wall_velocity_m_s: [S; 3],
    pub averaging_window_s: S,
}
#[derive(Clone)]
pub struct QuadraticGasNetworkGeometry<S> {
    pub previous_volume_m3: Vec<S>,
    pub volume_m3: Vec<S>,
    pub cell_centroid_m: Vec<[S; 3]>,
    pub cell_local_centroid_m: Option<Vec<Option<([f64; 3], [S; 3])>>>,
    pub cell_central_covariance_m2: Vec<Option<[[S; 3]; 3]>>,
    pub faces: Vec<QuadraticGasNetworkFace<S>>,
    pub physical_step_s: S,
    pub periodic_basis_m: Option<[[S; 3]; 3]>,
}
impl<S: Scalar> QuadraticGasNetworkGeometry<S> {
    pub fn cell_centroid_in_frame(&self, cell: usize, frame: [f64; 3]) -> CaeResult<[S; 3]> {
        let world = self.cell_centroid_m.get(cell).ok_or_else(|| fail("continuous centroid cell is absent"))?;
        if frame.iter().any(|v| !v.is_finite()) || self.cell_local_centroid_m.as_ref().is_some_and(|v| v.len() != self.cell_centroid_m.len()) {
            return Err(fail("continuous centroid frame layout differs"));
        }
        Ok(match self.cell_local_centroid_m.as_ref().and_then(|v| v[cell]) {
            Some((anchor, point)) => std::array::from_fn(|k| point[k] + S::from_f64(anchor[k] - frame[k])),
            None => std::array::from_fn(|k| world[k] - S::from_f64(frame[k])),
        })
    }
    fn cell_frame(&self, cell: usize) -> [f64; 3] {
        self.cell_local_centroid_m.as_ref().and_then(|v| v[cell]).map_or([0.; 3], |v| v.0)
    }
}
impl<S: Scalar> QuadraticGasNetworkFace<S> {
    fn centroid_in_frame(&self, frame: [f64; 3]) -> [S; 3] {
        match self.local_centroid_m {
            Some((anchor, point)) => std::array::from_fn(|k| point[k] + S::from_f64(anchor[k] - frame[k])),
            None => std::array::from_fn(|k| self.centroid_m[k] - S::from_f64(frame[k])),
        }
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    from = "QuadraticGasReconstructionDocument"
)]
pub enum QuadraticGasReconstruction {
    Constant,
    Linear {
        axes: [bool; 3],
        maximum_condition_number: f64,
    },
    Quadratic {
        axes: [bool; 3],
        maximum_condition_number: f64,
    },
}
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum QuadraticGasReconstructionDocument {
    Constant {},
    Linear {
        axes: [bool; 3],
        maximum_condition_number: f64,
    },
    Quadratic {
        axes: [bool; 3],
        maximum_condition_number: f64,
    },
}
impl From<QuadraticGasReconstructionDocument> for QuadraticGasReconstruction {
    fn from(value: QuadraticGasReconstructionDocument) -> Self {
        match value {
            QuadraticGasReconstructionDocument::Constant {} => Self::Constant,
            QuadraticGasReconstructionDocument::Linear {
                axes,
                maximum_condition_number,
            } => Self::Linear {
                axes,
                maximum_condition_number,
            },
            QuadraticGasReconstructionDocument::Quadratic {
                axes,
                maximum_condition_number,
            } => Self::Quadratic {
                axes,
                maximum_condition_number,
            },
        }
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuadraticGasNetworkPolicy {
    pub reconstruction: QuadraticGasReconstruction,
    pub entropy: QuadraticEntropyPolicy,
    pub diffuse_wall: QuadraticDiffuseWallPolicy,
    pub maximum_operator_nonzeros: usize,
    pub geometric_relative_tolerance: f64,
    pub geometric_absolute_volume_tolerance_m3: f64,
}
#[derive(Clone)]
pub struct QuadraticGasNetworkState<S> {
    pub previous_coefficient_inventory: Vec<Option<Vec<S>>>,
    pub current_coefficient_inventory: Vec<Option<Vec<S>>>,
    pub port_coefficient_density: Vec<Vec<S>>,
    pub gas_rt_m2_s2: S,
    pub molecular_viscosity_pa_s: S,
}
pub struct PreparedQuadraticGasNetwork<S> {
    wall_interior_anchors:
        Option<Vec<Option<crate::gas_quadratic_diffuse_wall::QuadraticWallInteriorAnchor<S>>>>,
    kinetic_ownership: Option<Vec<bool>>,
    output_scope: Option<Vec<bool>>,
    basis: Arc<QuadraticVelocityBasis>,
    gaussian: Arc<QuadraticGaussianBasis>,
    geometry: QuadraticGasNetworkGeometry<S>,
    policy: QuadraticGasNetworkPolicy,
    operators: Vec<Option<Arc<PreparedQuadraticMultiscaleFace<S>>>>,
    density_certificates:
        Vec<Option<Arc<[crate::gas_quadratic_reconstruction_admission::MinimumDensityCache; 2]>>>,
    incident_faces: Vec<Vec<(usize, f64)>>,
    reconstruction_neighbors: Vec<Vec<(usize, [i32; 3])>>,
}
#[derive(Clone)]
pub struct QuadraticGasNetworkPacket<S> {
    pub face: usize,
    pub weak_inventory: Vec<S>,
    pub moments: [S; 10],
    pub wall_work_j: S,
    pub wall_heat_j: S,
}
pub struct QuadraticGasNetworkSolution<S> {
    pub weak_residual: Vec<Option<Vec<S>>>,
    pub physical_moment_residual: Vec<Option<[S; 10]>>,
    pub collision_thermal_bath_energy_j: Vec<S>,
    pub face_packets: Vec<QuadraticGasNetworkPacket<S>>,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn geometry_zero<S: TransientGasScalar>(
    value: S,
    terms: &[S],
    absolute: f64,
    relative: f64,
) -> CaeResult<()> {
    geometry_zero_conditioned(value,terms,absolute,relative,None)
}
fn geometry_zero_conditioned<S:TransientGasScalar>(value:S,terms:&[S],absolute:f64,relative:f64,direction_bounds:Option<&[Vec<f64>]>)->CaeResult<()>{
    if direction_bounds.is_some_and(|rows|rows.len()!=terms.len()||rows.iter().flatten().any(|v|!v.is_finite()||*v<0.)){
        return Err(fail("continuous gas geometry conditioning bounds are invalid"));
    }
    let scale = terms.iter().map(|v| v.value().abs()).sum::<f64>();
    if !value.finite_first_order()
        || !scale.is_finite()
        || value.value().abs() > absolute + relative * scale
    {
        return Err(fail(
            "continuous gas geometry does not satisfy its conservation identity",
        ));
    }
    if let Some(directions) = value.geometry_first_order_directions() {
        if direction_bounds.is_some_and(|rows|rows.iter().zip(terms).any(|(row,term)|row.len()!=directions.len()&&!(row.is_empty()&&!term.has_direction()))){
            return Err(fail("continuous gas geometry conditioning dimensions differ"));
        }
        for (k, d) in directions.iter().enumerate() {
            let mut bound = 0.;
            for (index,term) in terms.iter().enumerate() {
                let td = match direction_bounds {
                    Some(rows)=>std::borrow::Cow::Borrowed(rows[index].as_slice()),
                    None=>std::borrow::Cow::Owned(term.geometry_first_order_directions()
                        .ok_or_else(|| fail("continuous gas geometry direction ownership differs"))?),
                };
                if td.is_empty() && !term.has_direction() {
                    continue;
                }
                if td.len() != directions.len() {
                    return Err(fail("continuous gas geometry direction dimension differs"));
                }
                bound += td[k].abs();
            }
            if !bound.is_finite() || d.abs() > relative * bound {
                return Err(fail(
                    "continuous gas geometry violates first-order conservation",
                ));
            }
        }
    } else if value.has_direction() {
        return Err(fail(
            "continuous gas shape reverse requires prior conserved-geometry jet certification",
        ));
    }
    Ok(())
}
impl<S: TransientGasScalar> PreparedQuadraticGasNetwork<S> {
    pub fn new(
        basis: Arc<QuadraticVelocityBasis>,
        geometry: QuadraticGasNetworkGeometry<S>,
        policy: QuadraticGasNetworkPolicy,
    ) -> CaeResult<Self> {
        Self::new_with_ownership(basis, geometry, policy, None, None, None)
    }
    pub fn new_mixed(
        basis: Arc<QuadraticVelocityBasis>,
        geometry: QuadraticGasNetworkGeometry<S>,
        policy: QuadraticGasNetworkPolicy,
        kinetic_ownership: Vec<bool>,
    ) -> CaeResult<Self> {
        Self::new_mixed_with_exact_primal_cache(basis, geometry, policy, kinetic_ownership, None)
    }
    pub fn new_mixed_with_exact_primal_cache(
        basis: Arc<QuadraticVelocityBasis>,
        geometry: QuadraticGasNetworkGeometry<S>,
        policy: QuadraticGasNetworkPolicy,
        kinetic_ownership: Vec<bool>,
        gaussian_primal_cache_entries: Option<usize>,
    ) -> CaeResult<Self> {
        if kinetic_ownership.len() != geometry.volume_m3.len() {
            return Err(fail("continuous kinetic ownership shape mismatch"));
        }
        Self::new_with_ownership(
            basis,
            geometry,
            policy,
            Some(kinetic_ownership),
            None,
            gaussian_primal_cache_entries,
        )
    }
    pub fn new_mixed_selected(
        basis: Arc<QuadraticVelocityBasis>,
        geometry: QuadraticGasNetworkGeometry<S>,
        policy: QuadraticGasNetworkPolicy,
        kinetic_ownership: Vec<bool>,
        selected_output_cells: &[usize],
    ) -> CaeResult<Self> {
        Self::new_mixed_selected_with_exact_primal_cache(
            basis,
            geometry,
            policy,
            kinetic_ownership,
            selected_output_cells,
            None,
        )
    }
    pub fn new_mixed_selected_with_exact_primal_cache(
        basis: Arc<QuadraticVelocityBasis>,
        geometry: QuadraticGasNetworkGeometry<S>,
        policy: QuadraticGasNetworkPolicy,
        kinetic_ownership: Vec<bool>,
        selected_output_cells: &[usize],
        gaussian_primal_cache_entries: Option<usize>,
    ) -> CaeResult<Self> {
        let cells = geometry.volume_m3.len();
        if kinetic_ownership.len() != cells {
            return Err(fail("continuous kinetic ownership shape mismatch"));
        }
        let mut scope = vec![false; cells];
        for &cell in selected_output_cells {
            if cell >= cells || scope[cell] {
                return Err(fail(
                    "continuous prepared output scope invalid or duplicated",
                ));
            }
            scope[cell] = true;
        }
        Self::new_with_ownership(
            basis,
            geometry,
            policy,
            Some(kinetic_ownership),
            Some(scope),
            gaussian_primal_cache_entries,
        )
    }
    pub fn certify_output_cells(&self, selected: &[usize]) -> CaeResult<()> {
        let mut seen = std::collections::BTreeSet::new();
        for &cell in selected {
            if cell >= self.geometry.volume_m3.len()
                || !seen.insert(cell)
                || self.output_scope.as_ref().is_some_and(|scope| !scope[cell])
            {
                return Err(fail(
                    "continuous output exceeds prepared scope or is invalid",
                ));
            }
        }
        Ok(())
    }
    fn new_with_ownership(
        basis: Arc<QuadraticVelocityBasis>,
        geometry: QuadraticGasNetworkGeometry<S>,
        policy: QuadraticGasNetworkPolicy,
        kinetic_ownership: Option<Vec<bool>>,
        output_scope: Option<Vec<bool>>,
        gaussian_primal_cache_entries: Option<usize>,
    ) -> CaeResult<Self> {
        let cells = geometry.volume_m3.len();
        if cells == 0
            || geometry.previous_volume_m3.len() != cells
            || geometry.cell_centroid_m.len() != cells
            || geometry.cell_local_centroid_m.as_ref().is_some_and(|v| v.len() != cells || v.iter().flatten().any(|(a,p)| a.iter().any(|v| !v.is_finite()) || p.iter().any(|v| !v.finite_first_order())))
            || geometry.cell_central_covariance_m2.len() != cells
            || !geometry.physical_step_s.finite_first_order()
            || geometry.physical_step_s.value() <= 0.
            || geometry
                .volume_m3
                .iter()
                .chain(&geometry.previous_volume_m3)
                .any(|v| !v.finite_first_order() || v.value() < 0.)
            || geometry
                .cell_centroid_m
                .iter()
                .flatten()
                .any(|v| !v.finite_first_order())
            || !policy.geometric_relative_tolerance.is_finite()
            || policy.geometric_relative_tolerance <= 0.
            || !policy.geometric_absolute_volume_tolerance_m3.is_finite()
            || policy.geometric_absolute_volume_tolerance_m3 < 0.
            || policy.maximum_operator_nonzeros == 0
        {
            return Err(fail("invalid continuous gas network geometry or policy"));
        }
        if let Some(local) = &geometry.cell_local_centroid_m {
            for (cell, point) in local.iter().enumerate() {
                if let Some((anchor, position)) = point {
                    for axis in 0..3 {
                        let origin = S::from_f64(anchor[axis]);
                        let projected = position[axis] + origin;
                        geometry_zero(geometry.cell_centroid_m[cell][axis] - projected, &[geometry.cell_centroid_m[cell][axis], -position[axis], -origin], 0., policy.geometric_relative_tolerance)?;
                    }
                }
            }
        }
        if matches!(
            policy.reconstruction,
            QuadraticGasReconstruction::Quadratic { .. }
        ) && geometry
            .cell_central_covariance_m2
            .iter()
            .enumerate()
            .any(|(i, v)| geometry.volume_m3[i].value() > 0. && v.is_none())
        {
            return Err(fail(
                "quadratic finite-volume reconstruction requires actual cell covariance",
            ));
        }
        {
            for covariance in geometry.cell_central_covariance_m2.iter().flatten() {
                if covariance.iter().flatten().any(|v| !v.finite_first_order()) {
                    return Err(fail("continuous gas cell covariance is nonfinite"));
                }
                for i in 0..3 {
                    if covariance[i][i].value() < 0. {
                        return Err(fail("continuous gas cell variance is negative"));
                    }
                    for j in 0..3 {
                        geometry_zero(
                            covariance[i][j] - covariance[j][i],
                            &[covariance[i][j], -covariance[j][i]],
                            0.,
                            policy.geometric_relative_tolerance,
                        )?;
                    }
                }
            }
        }
        if geometry
            .periodic_basis_m
            .as_ref()
            .is_some_and(|b| b.iter().flatten().any(|v| !v.finite_first_order()))
        {
            return Err(fail("continuous periodic basis is nonfinite"));
        }
        let gaussian = QuadraticGaussianBasis::new(basis.clone(), basis.reference_speed_m_s())?;
        let gaussian = Arc::new(match gaussian_primal_cache_entries {
            Some(entries) => gaussian.with_exact_primal_cache(entries)?,
            None => gaussian,
        });
        let mut operator_cache = std::collections::BTreeMap::new();
        let mut operators = Vec::with_capacity(geometry.faces.len());
        let mut incident_faces = vec![vec![]; cells];
        for (id, face) in geometry.faces.iter().enumerate() {
            if face.left >= cells
                || face.right.is_some_and(|r| r >= cells)
                || face.right.is_some() == face.boundary.is_some()
                || face.right.is_some() != face.right_centroid_to_face_m.is_some()
                || face.local_centroid_m.is_some_and(|(a,p)| a.iter().any(|v| !v.is_finite()) || p.iter().any(|v| !v.finite_first_order()))
                || !face.area_m2.finite_first_order()
                || face.area_m2.value() < 0.
                || face
                    .outward_unit_normal
                    .iter()
                    .chain(&face.centroid_m)
                    .chain(&face.left_centroid_to_face_m)
                    .chain(face.right_centroid_to_face_m.iter().flatten())
                    .chain(&face.ale_velocity_m_s)
                    .chain(&face.material_wall_velocity_m_s)
                    .any(|v| !v.finite_first_order())
                || !face.averaging_window_s.finite_first_order()
                || face.averaging_window_s.value() <= 0.
            {
                return Err(fail("invalid continuous gas network face"));
            }
            if let Some((anchor, position)) = face.local_centroid_m {
                for axis in 0..3 {
                    let origin = S::from_f64(anchor[axis]);
                    let projected = position[axis] + origin;
                    geometry_zero(face.centroid_m[axis] - projected, &[face.centroid_m[axis], -position[axis], -origin], 0., policy.geometric_relative_tolerance)?;
                }
            }
            let frame = geometry.cell_frame(face.left);
            let left_center = geometry.cell_centroid_in_frame(face.left, frame)?;
            let face_center = face.centroid_in_frame(frame);
            for axis in 0..3 {
                geometry_zero(
                    face_center[axis]
                        - left_center[axis]
                        - face.left_centroid_to_face_m[axis],
                    &[
                        face_center[axis],
                        -left_center[axis],
                        -face.left_centroid_to_face_m[axis],
                    ],
                    0.,
                    policy.geometric_relative_tolerance,
                )?;
            }
            if let Some(right) = face.right {
                if geometry.periodic_basis_m.is_none() && face.right_periodic_image != [0; 3] {
                    return Err(fail("continuous periodic face has no translation basis"));
                }
                let right_center = geometry.cell_centroid_in_frame(right, frame)?;
                for axis in 0..3 {
                    let image = geometry.periodic_basis_m.map_or(S::zero(), |b| {
                        (0..3).fold(S::zero(), |v, j| {
                            v + b[j][axis] * face.right_periodic_image[j] as f64
                        })
                    });
                    let left = left_center[axis]
                        + face.left_centroid_to_face_m[axis];
                    let right = right_center[axis]
                        + image
                        + face.right_centroid_to_face_m.unwrap()[axis];
                    geometry_zero(
                        left - right,
                        &[
                            left_center[axis],
                            face.left_centroid_to_face_m[axis],
                            -right_center[axis],
                            -image,
                            -face.right_centroid_to_face_m.unwrap()[axis],
                        ],
                        0.,
                        policy.geometric_relative_tolerance,
                    )?;
                }
            } else if face.right_periodic_image != [0; 3] {
                return Err(fail(
                    "continuous physical boundary carries a periodic neighbor image",
                ));
            }
            incident_faces[face.left].push((id, 1.));
            if let Some(right) = face.right {
                incident_faces[right].push((id, -1.));
            }
            if face.area_m2.value() == 0. {
                if face.area_m2.has_direction() {
                    return Err(fail(
                        "zero-area continuous gas face has an unresolved event direction",
                    ));
                }
                operators.push(None);
                continue;
            }
            if geometry.volume_m3[face.left].value() == 0.
                || face
                    .right
                    .is_some_and(|r| geometry.volume_m3[r].value() == 0.)
            {
                return Err(fail("positive continuous gas face touches a closed cell"));
            }
            let squares = face.outward_unit_normal.map(|v| v * v);
            let norm = squares.iter().copied().fold(S::zero(), |a, b| a + b);
            geometry_zero(
                norm - S::one(),
                &squares,
                0.,
                policy.geometric_relative_tolerance,
            )?;
            if output_scope.as_ref().is_some_and(|scope| {
                !scope[face.left] && face.right.is_none_or(|right| !scope[right])
            }) {
                operators.push(None);
                continue;
            }
            if kinetic_ownership.as_ref().is_some_and(|owned| {
                face.right
                    .is_some_and(|right| !owned[face.left] && !owned[right])
            }) {
                operators.push(None);
                continue;
            }
            let key = face
                .outward_unit_normal
                .iter()
                .chain(&face.ale_velocity_m_s)
                .all(|v| !v.has_direction())
                .then(|| {
                    std::array::from_fn::<_, 6, _>(|i| {
                        if i < 3 {
                            face.outward_unit_normal[i].value().to_bits()
                        } else {
                            face.ale_velocity_m_s[i - 3].value().to_bits()
                        }
                    })
                });
            let operator = if let Some(found) = key.and_then(|key| operator_cache.get(&key)) {
                Arc::clone(found)
            } else {
                let made = Arc::new(prepare_with_gaussian(
                    gaussian.clone(),
                    face.outward_unit_normal,
                    face.ale_velocity_m_s,
                    policy.maximum_operator_nonzeros,
                )?);
                if let Some(key) = key {
                    operator_cache.insert(key, made.clone());
                }
                made
            };
            operators.push(Some(operator));
        }
        for cell in 0..cells {
            for axis in 0..3 {
                let terms = incident_faces[cell]
                    .iter()
                    .map(|(id, sign)| {
                        let face = &geometry.faces[*id];
                        face.area_m2 * face.outward_unit_normal[axis] * *sign
                    })
                    .collect::<Vec<_>>();
                let sum = terms.iter().copied().fold(S::zero(), |a, b| a + b);
                let direction_bounds=incident_faces[cell].iter().map(|(id,sign)|{
                    let face=&geometry.faces[*id];
                    face.area_m2.geometry_product_direction_bounds(face.outward_unit_normal[axis])
                        .map(|row|row.into_iter().map(|v|v*sign.abs()).collect::<Vec<_>>())
                }).collect::<Option<Vec<_>>>();
                geometry_zero_conditioned(sum,&terms,0.,policy.geometric_relative_tolerance,direction_bounds.as_deref())?;
            }
            let mut terms = vec![geometry.volume_m3[cell], -geometry.previous_volume_m3[cell]];
            for &(id, sign) in &incident_faces[cell] {
                let face = &geometry.faces[id];
                let speed = (0..3).fold(S::zero(), |v, j| {
                    v + face.outward_unit_normal[j] * face.ale_velocity_m_s[j]
                });
                terms.push(-geometry.physical_step_s * face.area_m2 * speed * sign);
            }
            let direction_bounds=(||->CaeResult<Option<Vec<Vec<f64>>>>{
                let mut rows=Vec::new();
                for value in terms.iter().take(2).copied(){
                    let Some(row)=value.geometry_first_order_directions()else{return Ok(None);};
                    if row.iter().any(|v|!v.is_finite()){return Err(fail("swept volume direction is nonfinite"));}
                    rows.push(row.into_iter().map(f64::abs).collect::<Vec<_>>());
                }
                for &(id,sign) in &incident_faces[cell]{
                    let face=&geometry.faces[id];let mut bounds=Vec::<f64>::new();
                    for axis in 0..3{
                        let factors=[geometry.physical_step_s,face.area_m2,face.outward_unit_normal[axis],face.ale_velocity_m_s[axis]];
                        let row=match S::geometry_product_factors_direction_bounds(&factors){
                            Some(row)=>row,
                            None=>{
                                if factors.iter().all(|v|v.geometry_first_order_directions().is_some()){return Err(fail("swept volume product conditioning is invalid or unsupported"));}
                                return Ok(None);
                            }
                        };
                        if row.iter().any(|v|!v.is_finite()||*v<0.){return Err(fail("swept volume product conditioning is nonfinite or negative"));}
                        if row.is_empty(){if factors.iter().any(|v|v.has_direction()){return Err(fail("swept volume product conditioning omits active directions"));}continue;}
                        if bounds.is_empty(){bounds.resize(row.len(),0.);}else if bounds.len()!=row.len(){return Err(fail("swept volume product conditioning dimensions differ"));}
                        for(k,value)in row.into_iter().enumerate(){bounds[k]+=value*sign.abs();if !bounds[k].is_finite(){return Err(fail("swept volume conditioning sum is nonfinite"));}}
                    }
                    rows.push(bounds);
                }
                Ok(Some(rows))
            })()?;
            geometry_zero_conditioned(
                terms.iter().copied().fold(S::zero(), |a, b| a + b),
                &terms,
                policy.geometric_absolute_volume_tolerance_m3,
                policy.geometric_relative_tolerance,
                direction_bounds.as_deref(),
            )?;
        }
        let density_certificates = operators
            .iter()
            .map(|operator| {
                operator.as_ref().map(|_| {
                    Arc::new(std::array::from_fn(|_| {
                        crate::gas_quadratic_reconstruction_admission::MinimumDensityCache::default(
                        )
                    }))
                })
            })
            .collect();
        let mut result = Self {
            wall_interior_anchors: None,
            kinetic_ownership,
            output_scope,
            basis,
            gaussian,
            geometry,
            policy,
            operators,
            density_certificates,
            incident_faces,
            reconstruction_neighbors: Vec::with_capacity(cells),
        };
        for cell in 0..cells {
            let neighbors = result.build_reconstruction_neighbors(cell)?;
            result.reconstruction_neighbors.push(neighbors);
        }
        Ok(result)
    }
    pub fn with_wall_interior_anchors(
        mut self,
        anchors: Vec<Option<crate::gas_quadratic_diffuse_wall::QuadraticWallInteriorAnchor<S>>>,
    ) -> CaeResult<Self> {
        if anchors.len() != self.geometry.faces.len() {
            return Err(fail("continuous wall anchor face layout differs"));
        }
        for (id, anchor) in anchors.iter().enumerate() {
            if let Some(anchor) = anchor {
                let face = &self.geometry.faces[id];
                if !matches!(face.boundary, Some(QuadraticGasBoundary::DiffuseWall))
                    || face.area_m2.value() <= 0.
                {
                    return Err(fail(
                        "continuous wall anchor requires an actual positive wall face",
                    ));
                }
                anchor.validate_face(face.left, face.centroid_m, face.outward_unit_normal)?;
            }
        }
        self.wall_interior_anchors = Some(anchors);
        Ok(self)
    }
    pub fn constant_lift<T: TransientGasScalar>(&self) -> PreparedQuadraticGasNetwork<T> {
        let lift = |v: S| T::from_f64(v.value());
        let mut lifted_operators =
            std::collections::BTreeMap::<usize, Arc<PreparedQuadraticMultiscaleFace<T>>>::new();
        PreparedQuadraticGasNetwork {
            wall_interior_anchors: self.wall_interior_anchors.as_ref().map(|v| {
                v.iter()
                    .map(|a| a.as_ref().map(|a| a.constant_lift()))
                    .collect()
            }),
            kinetic_ownership: self.kinetic_ownership.clone(),
            output_scope: self.output_scope.clone(),
            basis: self.basis.clone(),
            gaussian: self.gaussian.clone(),
            geometry: QuadraticGasNetworkGeometry {
                previous_volume_m3: self
                    .geometry
                    .previous_volume_m3
                    .iter()
                    .copied()
                    .map(lift)
                    .collect(),
                volume_m3: self.geometry.volume_m3.iter().copied().map(lift).collect(),
                cell_centroid_m: self
                    .geometry
                    .cell_centroid_m
                    .iter()
                    .map(|v| v.map(lift))
                    .collect(),
                cell_local_centroid_m: self.geometry.cell_local_centroid_m.as_ref().map(|v| v.iter().map(|p| p.map(|(a,c)| (a,c.map(lift)))).collect()),
                cell_central_covariance_m2: self
                    .geometry
                    .cell_central_covariance_m2
                    .iter()
                    .map(|v| v.map(|c| c.map(|r| r.map(lift))))
                    .collect(),
                faces: self
                    .geometry
                    .faces
                    .iter()
                    .map(|f| QuadraticGasNetworkFace {
                        left: f.left,
                        right: f.right,
                        boundary: f.boundary.clone(),
                        area_m2: lift(f.area_m2),
                        outward_unit_normal: f.outward_unit_normal.map(lift),
                        centroid_m: f.centroid_m.map(lift),
                        local_centroid_m: f.local_centroid_m.map(|(a,c)| (a,c.map(lift))),
                        left_centroid_to_face_m: f.left_centroid_to_face_m.map(lift),
                        right_centroid_to_face_m: f.right_centroid_to_face_m.map(|v| v.map(lift)),
                        right_periodic_image: f.right_periodic_image,
                        ale_velocity_m_s: f.ale_velocity_m_s.map(lift),
                        material_wall_velocity_m_s: f.material_wall_velocity_m_s.map(lift),
                        averaging_window_s: lift(f.averaging_window_s),
                    })
                    .collect(),
                physical_step_s: lift(self.geometry.physical_step_s),
                periodic_basis_m: self
                    .geometry
                    .periodic_basis_m
                    .map(|b| b.map(|r| r.map(lift))),
            },
            policy: self.policy.clone(),
            operators: self
                .operators
                .iter()
                .map(|o| {
                    o.as_ref().map(|o| {
                        let key = Arc::as_ptr(o) as usize;
                        lifted_operators
                            .entry(key)
                            .or_insert_with(|| Arc::new(o.constant_lift()))
                            .clone()
                    })
                })
                .collect(),
            density_certificates: self.density_certificates.clone(),
            incident_faces: self.incident_faces.clone(),
            reconstruction_neighbors: self.reconstruction_neighbors.clone(),
        }
    }
    pub fn prepared_operator_count(&self) -> usize {
        self.operators
            .iter()
            .flatten()
            .map(|o| Arc::as_ptr(o) as usize)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
    pub fn geometry(&self) -> &QuadraticGasNetworkGeometry<S> {
        &self.geometry
    }
    pub fn basis(&self) -> &QuadraticVelocityBasis {
        &self.basis
    }
    pub(crate) fn gaussian(&self) -> &QuadraticGaussianBasis {
        &self.gaussian
    }
    pub fn policy(&self) -> &QuadraticGasNetworkPolicy {
        &self.policy
    }
}
pub trait QuadraticGasStateAccess<S: TransientGasScalar> {
    fn coefficient_inventory(&self, cell: usize, previous: bool) -> CaeResult<Option<Vec<S>>>;
    fn port_density(&self, port: usize) -> CaeResult<Vec<S>>;
    fn gas_rt_m2_s2(&self) -> S;
    fn molecular_viscosity_pa_s(&self) -> S;
    fn transport_viscosity_pa_s(&self, _cell: usize) -> CaeResult<S> {
        Ok(self.molecular_viscosity_pa_s())
    }
}
impl<S: TransientGasScalar> QuadraticGasStateAccess<S> for QuadraticGasNetworkState<S> {
    fn coefficient_inventory(&self, cell: usize, previous: bool) -> CaeResult<Option<Vec<S>>> {
        (if previous {
            &self.previous_coefficient_inventory
        } else {
            &self.current_coefficient_inventory
        })
        .get(cell)
        .cloned()
        .ok_or_else(|| fail("continuous gas state cell is absent"))
    }
    fn port_density(&self, port: usize) -> CaeResult<Vec<S>> {
        self.port_coefficient_density
            .get(port)
            .cloned()
            .ok_or_else(|| fail("continuous gas pressure port is absent"))
    }
    fn gas_rt_m2_s2(&self) -> S {
        self.gas_rt_m2_s2
    }
    fn molecular_viscosity_pa_s(&self) -> S {
        self.molecular_viscosity_pa_s
    }
}
impl<S: TransientGasScalar> PreparedQuadraticGasNetwork<S> {
    fn inventory<A: QuadraticGasStateAccess<S>>(
        &self,
        state: &A,
        cell: usize,
        previous: bool,
    ) -> CaeResult<Option<Vec<S>>> {
        let volume = if previous {
            self.geometry.previous_volume_m3[cell]
        } else {
            self.geometry.volume_m3[cell]
        };
        let q = state.coefficient_inventory(cell, previous)?;
        if volume.value() == 0. {
            if volume.has_direction()
                || q.as_ref().is_some_and(|q| {
                    q.len() != self.basis.coefficient_count()
                        || q.iter().any(|v| {
                            v.value() != 0. || v.has_direction() || !v.finite_first_order()
                        })
                })
            {
                return Err(fail(
                    "closed continuous gas slot retains inventory or an event direction",
                ));
            }
            return Ok(None);
        }
        let q = q.ok_or_else(|| fail("open continuous gas slot has no coefficient inventory"))?;
        if q.len() != self.basis.coefficient_count()
            || q.iter().any(|v| !v.finite_first_order() || v.value() < 0.)
        {
            return Err(fail("continuous gas coefficient inventory is invalid"));
        }
        Ok(Some(q))
    }
    fn build_reconstruction_neighbors(&self, cell: usize) -> CaeResult<Vec<(usize, [i32; 3])>> {
        use std::collections::BTreeSet;
        let depth = if matches!(
            self.policy.reconstruction,
            QuadraticGasReconstruction::Quadratic { .. }
        ) {
            2
        } else {
            1
        };
        let initial = (cell, [0i32; 3]);
        let mut seen = BTreeSet::from([initial]);
        let mut frontier = vec![initial];
        for _ in 0..depth {
            let mut next = vec![];
            for (current, image) in frontier {
                for &(id, sign) in &self.incident_faces[current] {
                    let face = &self.geometry.faces[id];
                    if face.area_m2.value() == 0. {
                        continue;
                    }
                    let Some(right) = face.right else {
                        continue;
                    };
                    let other = if sign > 0. { right } else { face.left };
                    let mut target = image;
                    for j in 0..3 {
                        let shift = if sign > 0. {
                            face.right_periodic_image[j]
                        } else {
                            face.right_periodic_image[j]
                                .checked_neg()
                                .ok_or_else(|| fail("continuous periodic image overflow"))?
                        };
                        target[j] = target[j]
                            .checked_add(shift)
                            .ok_or_else(|| fail("continuous periodic image overflow"))?;
                    }
                    if seen.insert((other, target)) {
                        next.push((other, target));
                    }
                }
            }
            frontier = next;
        }
        seen.remove(&initial);
        Ok(seen.into_iter().collect())
    }
    fn reconstruction_policy(
        &self,
    ) -> crate::gas_quadratic_coefficient_reconstruction::CoefficientReconstructionPolicy {
        use crate::gas_quadratic_coefficient_reconstruction::{
            CoefficientReconstructionOrder as Order, CoefficientReconstructionPolicy as Policy,
        };
        match self.policy.reconstruction {
            QuadraticGasReconstruction::Constant => Policy {
                order: Order::Constant,
                axes: [false; 3],
                maximum_condition_number: 1e12,
            },
            QuadraticGasReconstruction::Linear {
                axes,
                maximum_condition_number,
            } => Policy {
                order: Order::Linear,
                axes,
                maximum_condition_number,
            },
            QuadraticGasReconstruction::Quadratic {
                axes,
                maximum_condition_number,
            } => Policy {
                order: Order::Quadratic,
                axes,
                maximum_condition_number,
            },
        }
    }
    fn reconstruct<A: QuadraticGasStateAccess<S>>(
        &self,
        state: &A,
        cell: usize,
        offset: [S; 3],
    ) -> CaeResult<crate::gas_quadratic_coefficient_reconstruction::CoefficientFaceReconstruction<S>>
    {
        use crate::gas_quadratic_coefficient_reconstruction::{
            CoefficientNeighbor, reconstruct_face,
        };
        let q = self
            .inventory(state, cell, false)?
            .ok_or_else(|| fail("continuous face reconstruction touches closed cell"))?;
        let average = q
            .iter()
            .map(|v| *v / self.geometry.volume_m3[cell])
            .collect::<Vec<_>>();
        let previous = self
            .inventory(state, cell, true)?
            .unwrap_or_else(|| vec![S::zero(); self.basis.coefficient_count()]);
        let mut neighbors = vec![];
        for &(other, image) in &self.reconstruction_neighbors[cell] {
            let Some(q) = self.inventory(state, other, false)? else {
                continue;
            };
            let frame = self.geometry.cell_frame(cell);
            let center = self.geometry.cell_centroid_in_frame(cell, frame)?;
            let neighbor = self.geometry.cell_centroid_in_frame(other, frame)?;
            let offset = std::array::from_fn(|axis| {
                neighbor[axis] - center[axis]
                    + self.geometry.periodic_basis_m.map_or(S::zero(), |basis| {
                        (0..3).fold(S::zero(), |v, j| v + basis[j][axis] * image[j] as f64)
                    })
            });
            neighbors.push(CoefficientNeighbor {
                centroid_offset_m: offset,
                central_covariance_m2: self.geometry.cell_central_covariance_m2[other],
                average_density: q
                    .iter()
                    .map(|v| *v / self.geometry.volume_m3[other])
                    .collect(),
            });
        }
        reconstruct_face(
            &self.basis,
            &previous,
            &average,
            self.geometry.cell_central_covariance_m2[cell],
            &neighbors,
            offset,
            &self.reconstruction_policy(),
        )
    }
    pub(crate) fn face_packet<A: QuadraticGasStateAccess<S>>(
        &self,
        state: &A,
        id: usize,
    ) -> CaeResult<QuadraticGasNetworkPacket<S>> {
        let geometry = &self.geometry.faces[id];
        let operator = self.operators[id]
            .as_ref()
            .ok_or_else(|| fail("continuous zero-area face has no transport operator"))?;
        let left = self.reconstruct(state, geometry.left, geometry.left_centroid_to_face_m)?;
        let n = self.basis.coefficient_count();
        let zero = S::zero();
        let dt_area = self.geometry.physical_step_s * geometry.area_m2;
        if matches!(geometry.boundary, Some(QuadraticGasBoundary::DiffuseWall)) {
            let rho = left.equilibrium_mass_gradient;
            let j = left.equilibrium_momentum_gradient;
            let inventory = self
                .inventory(state, geometry.left, false)?
                .ok_or_else(|| fail("wall-adjacent open inventory is absent"))?;
            let volume = self.geometry.volume_m3[geometry.left];
            let moments = self.basis.coefficient_moments(&inventory)?;
            let density = moments[0] / volume;
            let momentum = std::array::from_fn(|axis| moments[axis + 1] / volume);
            let (density, momentum, offset, rho, j) = if let Some(anchor) = self
                .wall_interior_anchors
                .as_ref()
                .and_then(|a| a[id].as_ref())
            {
                anchor.validate_face(
                    geometry.left,
                    geometry.centroid_m,
                    geometry.outward_unit_normal,
                )?;
                let reconstructed = self.reconstruct(
                    state,
                    geometry.left,
                    anchor.offset_from_frame_m(self.geometry.cell_frame(geometry.left), self.geometry.cell_centroid_in_frame(geometry.left, self.geometry.cell_frame(geometry.left))?),
                )?;
                let moments = self.basis.coefficient_moments(&reconstructed.density)?;
                (
                    moments[0],
                    std::array::from_fn(|k| moments[k + 1]),
                    anchor.centroid_to_face_m(),
                    reconstructed.equilibrium_mass_gradient,
                    reconstructed.equilibrium_momentum_gradient,
                )
            } else {
                (density, momentum, geometry.left_centroid_to_face_m, rho, j)
            };
            let wall_moments = crate::gas_quadratic_diffuse_wall::adjacent_wall_moments(
                operator,
                density,
                momentum,
                offset,
                geometry.material_wall_velocity_m_s,
                rho,
                j,
            )?;
            let pressure = wall_moments.wall_density_kg_m3 * state.gas_rt_m2_s2();
            let wall = crate::gas_quadratic_diffuse_wall::evaluate_multiscale_cached(
                operator,
                &QuadraticDiffuseWallState {
                    outgoing_density: left.density,
                    outgoing_density_gradient: left.density_gradient,
                    equilibrium_mass_gradient: wall_moments.density_gradient,
                    equilibrium_momentum_gradient: wall_moments.momentum_gradient,
                    gas_rt_m2_s2: state.gas_rt_m2_s2(),
                    molecular_viscosity_pa_s: state.transport_viscosity_pa_s(geometry.left)?,
                    step_s: geometry.averaging_window_s,
                    material_wall_velocity_m_s: geometry.material_wall_velocity_m_s,
                },
                pressure,
                &self.policy.entropy,
                &self.policy.diffuse_wall,
                self.density_certificates[id].as_deref(),
            )?;
            return Ok(QuadraticGasNetworkPacket {
                face: id,
                weak_inventory: wall.weak_flux.iter().map(|v| *v * dt_area).collect(),
                moments: wall.flux_moments.map(|v| v * dt_area),
                wall_work_j: wall.wall_work_flux_w_m2 * dt_area,
                wall_heat_j: wall.wall_heat_flux_w_m2 * dt_area,
            });
        }
        let (right, gradient) = if let Some(right) = geometry.right {
            let value =
                self.reconstruct(state, right, geometry.right_centroid_to_face_m.unwrap())?;
            (value.density, value.density_gradient)
        } else {
            let Some(QuadraticGasBoundary::Port { port }) = geometry.boundary else {
                return Err(fail("continuous gas boundary kind is absent"));
            };
            let density = state.port_density(port)?;
            if density.len() != n
                || density
                    .iter()
                    .any(|v| !v.finite_first_order() || v.value() < 0.)
            {
                return Err(fail("continuous gas port coefficient density is invalid"));
            }
            (density, std::array::from_fn(|_| vec![zero; n]))
        };
        let mut rho = [zero; 3];
        let mut j = [[zero; 3]; 3];
        for axis in 0..3 {
            let common =
                operator.interface_density_action(&left.density_gradient[axis], &gradient[axis])?;
            let moments = self.basis.weak_moments(&common)?;
            rho[axis] = moments[0];
            for component in 0..3 {
                j[component][axis] = moments[component + 1];
            }
        }
        let value = evaluate_prepared_cached(
            operator,
            &QuadraticMultiscaleFaceState {
                left_density: left.density,
                right_density: right,
                left_density_gradient: left.density_gradient,
                right_density_gradient: gradient,
                equilibrium_mass_gradient: rho,
                equilibrium_momentum_gradient: j,
                gas_rt_m2_s2: state.gas_rt_m2_s2(),
                molecular_viscosity_pa_s: if let Some(right) = geometry.right {
                    (state.transport_viscosity_pa_s(geometry.left)?
                        + state.transport_viscosity_pa_s(right)?)
                        * 0.5
                } else {
                    state.transport_viscosity_pa_s(geometry.left)?
                },
                step_s: geometry.averaging_window_s,
            },
            &self.policy.entropy,
            self.density_certificates[id].as_deref(),
        )?;
        Ok(QuadraticGasNetworkPacket {
            face: id,
            weak_inventory: value
                .average_weak_flux
                .iter()
                .map(|v| *v * dt_area)
                .collect(),
            moments: value.average_moments.map(|v| v * dt_area),
            wall_work_j: zero,
            wall_heat_j: zero,
        })
    }
}
pub struct QuadraticGasNetworkLocalSolution<S> {
    pub cells: Vec<(usize, QuadraticGasBalance<S>)>,
    pub face_packets: Vec<QuadraticGasNetworkPacket<S>>,
}
impl<S: TransientGasScalar> PreparedQuadraticGasNetwork<S> {
    fn validate_material<A: QuadraticGasStateAccess<S>>(&self, state: &A) -> CaeResult<()> {
        if [state.gas_rt_m2_s2(), state.molecular_viscosity_pa_s()]
            .iter()
            .any(|v| !v.finite_first_order() || v.value() <= 0.)
        {
            return Err(fail("continuous gas material data are invalid"));
        }
        Ok(())
    }
    fn base_balance<A: QuadraticGasStateAccess<S>>(
        &self,
        state: &A,
        cell: usize,
        packets: &[Vec<S>],
    ) -> CaeResult<Option<QuadraticGasBalance<S>>> {
        let current = self.inventory(state, cell, false)?;
        let previous = self.inventory(state, cell, true)?;
        let Some(current) = current else {
            if previous
                .as_ref()
                .is_some_and(|p| p.iter().any(|v| v.value() != 0. || v.has_direction()))
            {
                return Err(fail("closing continuous gas slot traps positive inventory"));
            }
            return Ok(None);
        };
        let previous = previous.unwrap_or_else(|| vec![S::zero(); self.basis.coefficient_count()]);
        Ok(Some(coefficient_balance(
            &self.gaussian,
            &previous,
            &current,
            self.geometry.volume_m3[cell],
            state.gas_rt_m2_s2(),
            state.transport_viscosity_pa_s(cell)?,
            self.geometry.physical_step_s,
            packets,
            &self.policy.entropy,
        )?))
    }
    fn ordered_face_packets<A: QuadraticGasStateAccess<S> + Sync>(
        &self,
        state: &A,
        ids: Vec<usize>,
    ) -> Vec<(usize, CaeResult<QuadraticGasNetworkPacket<S>>)> {
        if S::supports_parallel_face_evaluation() {
            use rayon::prelude::*;
            ids.into_par_iter()
                .map(|id| (id, self.face_packet(state, id)))
                .collect()
        } else {
            ids.into_iter()
                .map(|id| (id, self.face_packet(state, id)))
                .collect()
        }
    }
    pub fn evaluate<A: QuadraticGasStateAccess<S>>(
        &self,
        state: &A,
    ) -> CaeResult<QuadraticGasNetworkSolution<S>> {
        self.evaluate_with_packets(state, |ids| {
            ids.into_iter().map(|id| (id, self.face_packet(state, id)))
        })
    }
    pub fn evaluate_parallel<A: QuadraticGasStateAccess<S> + Sync>(
        &self,
        state: &A,
    ) -> CaeResult<QuadraticGasNetworkSolution<S>> {
        if !S::supports_parallel_face_evaluation() {
            return self.evaluate(state);
        }
        self.evaluate_with_packets(state, |ids| self.ordered_face_packets(state, ids))
    }
    fn evaluate_with_packets<
        A: QuadraticGasStateAccess<S>,
        I: IntoIterator<Item = (usize, CaeResult<QuadraticGasNetworkPacket<S>>)>,
    >(
        &self,
        state: &A,
        packets_for: impl FnOnce(Vec<usize>) -> I,
    ) -> CaeResult<QuadraticGasNetworkSolution<S>> {
        if self.kinetic_ownership.is_some() {
            return Err(fail(
                "mixed continuous network requires explicitly owned local coefficient equations",
            ));
        }
        self.validate_material(state)?;
        let cells = self.geometry.volume_m3.len();
        let mut residual = Vec::with_capacity(cells);
        let mut heat = Vec::with_capacity(cells);
        for cell in 0..cells {
            if let Some(balance) = self.base_balance(state, cell, &[])? {
                residual.push(Some(balance.weak_residual));
                heat.push(balance.collision_thermal_bath_energy_j);
            } else {
                residual.push(None);
                heat.push(S::zero());
            }
        }
        let mut packets = vec![];
        let ids = (0..self.geometry.faces.len())
            .filter(|&id| self.operators[id].is_some())
            .collect::<Vec<_>>();
        for (id, packet) in packets_for(ids) {
            let packet = packet?;
            let geometry = &self.geometry.faces[id];
            let left = residual[geometry.left]
                .as_mut()
                .ok_or_else(|| fail("continuous packet enters closed left cell"))?;
            for (r, p) in left.iter_mut().zip(&packet.weak_inventory) {
                *r += *p;
            }
            if let Some(right) = geometry.right {
                let right = residual[right]
                    .as_mut()
                    .ok_or_else(|| fail("continuous packet enters closed right cell"))?;
                for (r, p) in right.iter_mut().zip(&packet.weak_inventory) {
                    *r -= *p;
                }
            } else {
                packets.push(packet);
            }
        }
        let physical = residual
            .iter()
            .map(|r| r.as_ref().map(|r| self.basis.weak_moments(r)).transpose())
            .collect::<CaeResult<Vec<_>>>()?;
        if residual
            .iter()
            .flatten()
            .flatten()
            .chain(heat.iter())
            .any(|v| !v.finite_first_order())
        {
            return Err(fail("continuous gas network balance is nonfinite"));
        }
        Ok(QuadraticGasNetworkSolution {
            weak_residual: residual,
            physical_moment_residual: physical,
            collision_thermal_bath_energy_j: heat,
            face_packets: packets,
        })
    }
    pub fn evaluate_cells<A: QuadraticGasStateAccess<S>>(
        &self,
        state: &A,
        selected: &[usize],
    ) -> CaeResult<QuadraticGasNetworkLocalSolution<S>> {
        self.evaluate_cells_with_packets(state, selected, |ids| {
            ids.into_iter().map(|id| (id, self.face_packet(state, id)))
        })
    }
    pub fn evaluate_cells_parallel<A: QuadraticGasStateAccess<S> + Sync>(
        &self,
        state: &A,
        selected: &[usize],
    ) -> CaeResult<QuadraticGasNetworkLocalSolution<S>> {
        if !S::supports_parallel_face_evaluation() {
            return self.evaluate_cells(state, selected);
        }
        self.evaluate_cells_with_packets(state, selected, |ids| {
            self.ordered_face_packets(state, ids)
        })
    }
    fn evaluate_cells_with_packets<
        A: QuadraticGasStateAccess<S>,
        I: IntoIterator<Item = (usize, CaeResult<QuadraticGasNetworkPacket<S>>)>,
    >(
        &self,
        state: &A,
        selected: &[usize],
        packets_for: impl FnOnce(Vec<usize>) -> I,
    ) -> CaeResult<QuadraticGasNetworkLocalSolution<S>> {
        use std::collections::{BTreeMap, BTreeSet};
        self.certify_output_cells(selected)?;
        self.validate_material(state)?;
        let mut seen = BTreeSet::new();
        let mut required = BTreeSet::new();
        for &cell in selected {
            if cell >= self.geometry.volume_m3.len() || !seen.insert(cell) {
                return Err(fail(
                    "continuous gas local output cell is invalid or duplicated",
                ));
            }
            if self
                .kinetic_ownership
                .as_ref()
                .is_some_and(|owned| !owned[cell])
            {
                return Err(fail("continuum slot has no kinetic coefficient equation"));
            }
            if self.inventory(state, cell, false)?.is_some() {
                for &(id, _) in &self.incident_faces[cell] {
                    if self.operators[id].is_some() {
                        required.insert(id);
                    }
                }
            }
        }
        let mut packets = BTreeMap::new();
        let ids = required.into_iter().collect::<Vec<_>>();
        for (id, packet) in packets_for(ids) {
            packets.insert(id, packet?);
        }
        let mut cells = vec![];
        for &cell in selected {
            let mut local = vec![];
            for &(id, sign) in &self.incident_faces[cell] {
                if let Some(packet) = packets.get(&id) {
                    local.push(
                        packet
                            .weak_inventory
                            .iter()
                            .map(|v| *v * sign)
                            .collect::<Vec<_>>(),
                    );
                }
            }
            if let Some(balance) = self.base_balance(state, cell, &local)? {
                cells.push((cell, balance));
            }
        }
        Ok(QuadraticGasNetworkLocalSolution {
            cells,
            face_packets: packets.into_values().collect(),
        })
    }
    pub fn dependency_cells(&self, selected: &[usize]) -> CaeResult<Vec<usize>> {
        use std::collections::BTreeSet;
        self.certify_output_cells(selected)?;
        let mut support = BTreeSet::new();
        for &cell in selected {
            if cell >= self.geometry.volume_m3.len() {
                return Err(fail("continuous gas output dependency cell is invalid"));
            }
            support.insert(cell);
            for &(face, _) in &self.incident_faces[cell] {
                let face = &self.geometry.faces[face];
                support.insert(face.left);
                if let Some(right) = face.right {
                    support.insert(right);
                }
            }
        }
        let first = support.iter().copied().collect::<Vec<_>>();
        for cell in first {
            support.extend(
                self.reconstruction_neighbors[cell]
                    .clone()
                    .into_iter()
                    .map(|(c, _)| c),
            );
        }
        Ok(support.into_iter().collect())
    }
}

pub(crate) struct ConstantWeakTransportFace {
    pub face: usize,
    pub weight: f64,
    pub density: crate::gas_quadratic_halfspace::QuadraticHalfspaceOperator<f64>,
    pub speed: crate::gas_quadratic_halfspace::QuadraticHalfspaceOperator<f64>,
}
pub(crate) struct ConstantWeakTransportCell {
    pub storage_weight: f64,
    pub faces: Vec<ConstantWeakTransportFace>,
}
impl PreparedQuadraticGasNetwork<f64> {
    pub(crate) fn constant_weak_transport<A: QuadraticGasStateAccess<f64>>(
        &self,
        state: &A,
        cell: usize,
    ) -> CaeResult<ConstantWeakTransportCell> {
        self.weak_transport_descriptor(state, cell, false)
    }
    pub(crate) fn constant_surrogate_weak_transport<A: QuadraticGasStateAccess<f64>>(
        &self,
        state: &A,
        cell: usize,
    ) -> CaeResult<ConstantWeakTransportCell> {
        self.weak_transport_descriptor(state, cell, true)
    }
    fn weak_transport_descriptor<A: QuadraticGasStateAccess<f64>>(
        &self,
        state: &A,
        cell: usize,
        numerical_surrogate: bool,
    ) -> CaeResult<ConstantWeakTransportCell> {
        if !numerical_surrogate
            && !matches!(
                self.policy.reconstruction,
                QuadraticGasReconstruction::Constant
            )
        {
            return Err(fail(
                "sparse weak transport descriptor requires constant reconstruction",
            ));
        }
        self.certify_output_cells(&[cell])?;
        self.validate_material(state)?;
        let volume = *self
            .geometry
            .volume_m3
            .get(cell)
            .ok_or_else(|| fail("weak transport cell index"))?;
        if volume <= 0.
            || self
                .kinetic_ownership
                .as_ref()
                .is_some_and(|owned| !owned[cell])
        {
            return Err(fail(
                "weak transport descriptor requires an active kinetic cell",
            ));
        }
        let inventory = self
            .inventory(state, cell, false)?
            .ok_or_else(|| fail("weak transport inventory absent"))?;
        let rho = self.basis.coefficient_moments(&inventory)?[0] / volume;
        let rt = state.gas_rt_m2_s2();
        let mu = state.transport_viscosity_pa_s(cell)?;
        let storage_weight = 1. + self.geometry.physical_step_s * rho * rt / mu;
        let mut faces = Vec::new();
        for &(id, sign) in &self.incident_faces[cell] {
            let Some(operator) = self.operators[id].as_ref() else {
                continue;
            };
            let face = &self.geometry.faces[id];
            let common_density = if matches!(face.boundary, Some(QuadraticGasBoundary::DiffuseWall))
            {
                if sign != 1. {
                    return Err(fail("wall weak transport orientation"));
                }
                let adjacent = self.reconstruct(state, face.left, face.left_centroid_to_face_m)?;
                let (density, momentum, offset, mass_gradient, momentum_gradient) =
                    if let Some(anchor) = self
                        .wall_interior_anchors
                        .as_ref()
                        .and_then(|a| a[id].as_ref())
                    {
                        let reconstructed = self.reconstruct(
                            state,
                            face.left,
                            anchor.offset_from_frame_m(self.geometry.cell_frame(face.left), self.geometry.cell_centroid_in_frame(face.left, self.geometry.cell_frame(face.left))?),
                        )?;
                        let moments = self.basis.coefficient_moments(&reconstructed.density)?;
                        (
                            moments[0],
                            std::array::from_fn(|k| moments[k + 1]),
                            anchor.centroid_to_face_m(),
                            reconstructed.equilibrium_mass_gradient,
                            reconstructed.equilibrium_momentum_gradient,
                        )
                    } else {
                        let moments = self.basis.coefficient_moments(&inventory)?;
                        (
                            rho,
                            std::array::from_fn(|k| moments[k + 1] / volume),
                            face.left_centroid_to_face_m,
                            adjacent.equilibrium_mass_gradient,
                            adjacent.equilibrium_momentum_gradient,
                        )
                    };
                crate::gas_quadratic_diffuse_wall::adjacent_wall_moments(
                    operator,
                    density,
                    momentum,
                    offset,
                    face.material_wall_velocity_m_s,
                    mass_gradient,
                    momentum_gradient,
                )?
                .wall_density_kg_m3
            } else {
                let left = self
                    .reconstruct(state, face.left, face.left_centroid_to_face_m)?
                    .density;
                let right = if let Some(right) = face.right {
                    self.reconstruct(state, right, face.right_centroid_to_face_m.unwrap())?
                        .density
                } else {
                    let Some(QuadraticGasBoundary::Port { port }) = face.boundary else {
                        return Err(fail("weak transport boundary owner"));
                    };
                    state.port_density(port)?
                };
                self.basis
                    .weak_moments(&operator.interface_density_action(&left, &right)?)?[0]
            };
            let face_mu = if let Some(right) = face.right {
                (state.transport_viscosity_pa_s(face.left)?
                    + state.transport_viscosity_pa_s(right)?)
                    * 0.5
            } else {
                state.transport_viscosity_pa_s(face.left)?
            };
            if !face_mu.is_finite() || face_mu <= 0. {
                return Err(fail("weak transport face viscosity invalid"));
            }
            let cf0 = crate::gas_multiscale_flux::coefficients(
                face.averaging_window_s * common_density * rt / face_mu,
                face.averaging_window_s,
            )[0];
            let weight = self.geometry.physical_step_s * face.area_m2 * cf0 / volume;
            let (density, speed) =
                operator.numerical_halfspace_operators(if sign == 1. { 0 } else { 1 })?;
            if !weight.is_finite()
                || weight < 0.
                || !common_density.is_finite()
                || common_density < 0.
            {
                return Err(fail(
                    "weak transport descriptor nonfinite material coefficient",
                ));
            }
            faces.push(ConstantWeakTransportFace {
                face: id,
                weight,
                density: density.clone(),
                speed: speed.clone(),
            });
        }
        if !storage_weight.is_finite() || storage_weight < 1. {
            return Err(fail("weak transport storage coefficient invalid"));
        }
        Ok(ConstantWeakTransportCell {
            storage_weight,
            faces,
        })
    }
}
