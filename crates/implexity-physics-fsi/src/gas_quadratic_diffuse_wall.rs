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

use crate::gas_gap_mass::GasMassScalar;
use crate::gas_quadratic_multiscale_flux::{
    evaluate_prepared, PreparedQuadraticMultiscaleFace, QuadraticMultiscaleFaceState,
};
use crate::gas_quadratic_velocity::QuadraticEntropyPolicy;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Dual;
use implexity_core::{CaeError, CaeResult};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone)]
pub struct QuadraticDiffuseWallState<S> {
    pub outgoing_density: Vec<S>,
    pub material_wall_velocity_m_s: [S; 3],
    pub outgoing_density_gradient: [Vec<S>; 3],
    pub equilibrium_mass_gradient: [S; 3],
    pub equilibrium_momentum_gradient: [[S; 3]; 3],
    pub gas_rt_m2_s2: S,
    pub molecular_viscosity_pa_s: S,
    pub step_s: S,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuadraticDiffuseWallPolicy {
    pub mass_flux_absolute_tolerance_kg_m2_s: f64,
    pub mass_flux_relative_tolerance: f64,
    pub maximum_iterations: usize,
}
#[derive(Clone)]
pub struct QuadraticDiffuseWallSolution<S> {
    pub weak_flux: Vec<S>,
    pub flux_moments: [S; 10],
    pub wall_density_kg_m3: S,
    pub wall_momentum_flux_n_m2: [S; 3],
    pub wall_work_flux_w_m2: S,
    pub wall_heat_flux_w_m2: S,
    pub mass_flux_defect_kg_m2_s: S,
    pub iterations: usize,
}
fn support_direction<S: TransientGasScalar>(
    value: S,
    column: usize,
    dimension: usize,
) -> CaeResult<f64> {
    let directions = value
        .geometry_first_order_directions()
        .ok_or_else(|| fail("wall support jets are not authenticated"))?;
    if directions.is_empty() && !value.has_direction() {
        return Ok(0.);
    }
    if directions.len() != dimension {
        return Err(fail("wall support jet dimension differs"));
    }
    Ok(directions[column])
}
fn certify_support_plane<S: TransientGasScalar>(
    normal: &[S; 3],
    origin: &[S; 3],
    point: &[S; 3],
    offset: S,
    residual: S,
) -> CaeResult<()> {
    let directions = residual
        .geometry_first_order_directions()
        .ok_or_else(|| fail("wall support jets are not authenticated"))?;
    for (j, d) in directions.iter().enumerate() {
        let mut bound = support_direction(offset, j, directions.len())?.abs();
        for k in 0..3 {
            for point in [point[k],origin[k]] {
                let terms = normal[k].geometry_product_direction_bounds(point)
                    .ok_or_else(||fail("wall support product jets are not authenticated"))?;
                if terms.len()!=directions.len() {
                    return Err(fail("wall support product jet dimension differs"));
                }
                bound += terms[j];
            }
        }
        if !bound.is_finite() || d.abs() > 256. * f64::EPSILON * bound {
            return Err(fail("wall support boundary directions differ"));
        }
    }
    Ok(())
}
#[derive(Clone)]
pub struct QuadraticWallInteriorAnchor<S> {
    left_slot: usize,
    position_m: [S; 3],
    face_position_m: [S; 3],
    face_normal: [S; 3],
}
impl<S: TransientGasScalar> QuadraticWallInteriorAnchor<S> {
    pub(crate) fn from_measured_convex_support(
        left_slot: usize,
        face_position_m: [S; 3],
        face_normal: [S; 3],
        position_m: [S; 3],
        support_volume_m3: S,
        vertices_m: &[[S; 3]],
        planes: &[([S; 3], [S; 3], S)],
    ) -> CaeResult<Self> {
        if !support_volume_m3.finite_first_order()
            || support_volume_m3.value() <= 0.
            || position_m.iter().any(|v| !v.finite_first_order())
            || vertices_m.len() < 4
            || planes.len() < 4
            || vertices_m
                .iter()
                .flatten()
                .chain(&face_position_m)
                .chain(&face_normal)
                .chain(
                    planes
                        .iter()
                        .flat_map(|(n, o, d)| n.iter().chain(o).chain(std::iter::once(d))),
                )
                .any(|v| !v.finite_first_order())
        {
            return Err(fail("invalid continuous wall convex support"));
        }
        let mut wall_support = false;
        for (normal, origin, offset) in planes {
            let norm = (0..3)
                .map(|k| normal[k].value() * normal[k].value())
                .sum::<f64>();
            if !norm.is_finite() || norm == 0. {
                return Err(fail("wall support plane is degenerate"));
            }
            let interior = (0..3).fold(*offset, |v, k| v + normal[k] * (position_m[k] - origin[k]));
            if !interior.finite_first_order() || interior.value() >= 0. {
                return Err(fail(
                    "continuous wall anchor is not strictly inside its convex support",
                ));
            }
            for point in vertices_m
                .iter()
                .chain(std::iter::once(&position_m))
                .chain(std::iter::once(&face_position_m))
            {
                let terms: [S; 3] = std::array::from_fn(|k| normal[k] * (point[k] - origin[k]));
                let value = terms.iter().fold(*offset, |a, v| a + *v);
                let scale = (0..3)
                    .map(|k| {
                        normal[k].value().abs() * (point[k].value().abs() + origin[k].value().abs())
                    })
                    .sum::<f64>()
                    + offset.value().abs();
                if !value.finite_first_order() || value.value() > 256. * f64::EPSILON * scale {
                    return Err(fail("continuous wall anchor support is not convex"));
                }
                if value.value().abs() <= 256. * f64::EPSILON * scale {
                    certify_support_plane(normal, origin, point, *offset, value)?;
                }
            }
            let cross = [
                normal[1] * face_normal[2] - normal[2] * face_normal[1],
                normal[2] * face_normal[0] - normal[0] * face_normal[2],
                normal[0] * face_normal[1] - normal[1] * face_normal[0],
            ];
            let aligned = cross
                .iter()
                .all(|v| v.value().abs() <= 256. * f64::EPSILON * norm.sqrt())
                && (0..3)
                    .map(|k| normal[k].value() * face_normal[k].value())
                    .sum::<f64>()
                    > 0.;
            let residual = (0..3).fold(*offset, |v, k| {
                v + normal[k] * (face_position_m[k] - origin[k])
            });
            let scale = (0..3)
                .map(|k| {
                    normal[k].value().abs()
                        * (face_position_m[k].value().abs() + origin[k].value().abs())
                })
                .sum::<f64>()
                + offset.value().abs();
            if aligned && residual.value().abs() <= 256. * f64::EPSILON * scale {
                certify_support_plane(normal, origin, &face_position_m, *offset, residual)?;
                for k in 0..3 {
                    let dirs = cross[k]
                        .geometry_first_order_directions()
                        .ok_or_else(|| fail("wall support normal jets are not authenticated"))?;
                    let a = (k + 1) % 3;
                    let b = (k + 2) % 3;
                    let terms = [(normal[a],face_normal[b]), (normal[b],face_normal[a])];
                    for (j, d) in dirs.iter().enumerate() {
                        let bound = terms.iter().try_fold(0., |sum, (a,b)| {
                            let terms=a.geometry_product_direction_bounds(*b)
                                .ok_or_else(||fail("wall support normal product jets are not authenticated"))?;
                            if terms.len()!=dirs.len() {return Err(fail("wall support normal product dimension differs"));}
                            Ok::<_, CaeError>(sum+terms[j])
                        })?;
                        if d.abs() > 256. * f64::EPSILON * bound {
                            return Err(fail("wall support normal directions differ"));
                        }
                    }
                }
                wall_support = true;
            }
        }
        let distance = (0..3).fold(S::zero(), |v, k| {
            v + face_normal[k] * (face_position_m[k] - position_m[k])
        });
        if !wall_support || !distance.finite_first_order() || distance.value() <= 0. {
            return Err(fail(
                "continuous wall has no authentic positive-distance interior support",
            ));
        }
        Ok(Self {
            left_slot,
            position_m,
            face_position_m,
            face_normal,
        })
    }
    pub(crate) fn from_certified_measured_convex_support(
        left_slot: usize,
        face_position_m: [S; 3],
        face_normal: [S; 3],
        position_m: [S; 3],
        support_volume_m3: S,
        vertices_m: &[[S; 3]],
        planes: &[([S; 3], [S; 3], S)],
        proof: &crate::native_geometry_construction_error::ConvexSupportConstructionProof,
    ) -> CaeResult<Self> {
        proof.validate_bindings(
            support_volume_m3,
            position_m,
            vertices_m,
            planes,
            face_position_m,
            face_normal,
        )?;
        if !support_volume_m3.finite_first_order()
            || support_volume_m3.value() <= 0.
            || !proof.certifies_positive_volume()
            || position_m.iter().any(|v| !v.finite_first_order())
            || vertices_m.len() < 4
            || planes.len() < 4
            || vertices_m
                .iter()
                .flatten()
                .chain(&face_position_m)
                .chain(&face_normal)
                .chain(
                    planes
                        .iter()
                        .flat_map(|(n, o, d)| n.iter().chain(o).chain(std::iter::once(d))),
                )
                .any(|v| !v.finite_first_order())
        {
            return Err(fail("invalid certified continuous wall convex support"));
        }
        let mut wall_support = false;
        for (index, (normal, origin, offset)) in planes.iter().enumerate() {
            let norm = (0..3)
                .map(|k| normal[k].value() * normal[k].value())
                .sum::<f64>();
            if !norm.is_finite() || norm == 0. {
                return Err(fail("wall support plane is degenerate"));
            }
            let interior = (0..3).fold(*offset, |v, k| v + normal[k] * (position_m[k] - origin[k]));
            if !interior.finite_first_order()
                || interior.value() >= 0.
                || !proof.certifies_centroid_membership(index)
            {
                return Err(fail("certified wall anchor is not strictly inside its convex support"));
            }
            for (vertex, point) in vertices_m.iter().enumerate() {
                let value = (0..3).fold(*offset, |v, k| v + normal[k] * (point[k] - origin[k]));
                let scale = (0..3)
                    .map(|k| normal[k].value().abs() * (point[k].value().abs() + origin[k].value().abs()))
                    .sum::<f64>() + offset.value().abs();
                if !value.finite_first_order()
                    || value.value() > 256. * f64::EPSILON * scale
                    || !proof.certifies_vertex_membership(vertex, index)
                {
                    return Err(fail("certified wall support vertex is not convex"));
                }
            }
            let residual = (0..3).fold(*offset, |v, k| {
                v + normal[k] * (face_position_m[k] - origin[k])
            });
            let scale = (0..3)
                .map(|k| normal[k].value().abs()
                    * (face_position_m[k].value().abs() + origin[k].value().abs()))
                .sum::<f64>() + offset.value().abs();
            if !residual.finite_first_order()
                || residual.value() > 256. * f64::EPSILON * scale
                || !proof.certifies_face_membership(index)
            {
                return Err(fail("certified wall face is outside its convex support"));
            }
            let cross = [
                normal[1] * face_normal[2] - normal[2] * face_normal[1],
                normal[2] * face_normal[0] - normal[0] * face_normal[2],
                normal[0] * face_normal[1] - normal[1] * face_normal[0],
            ];
            let aligned = cross.iter()
                .all(|v| v.finite_first_order() && v.value().abs() <= 256. * f64::EPSILON * norm.sqrt())
                && (0..3).map(|k| normal[k].value() * face_normal[k].value()).sum::<f64>() > 0.;
            if proof.owns_native_wall_plane(index) || (aligned && residual.value().abs() <= 256. * f64::EPSILON * scale) {
                if !proof.certifies_face_plane(index) || !proof.certifies_parallel_normal(index) {
                    return Err(fail("certified wall face or normal directions differ"));
                }
                wall_support = true;
            }
        }
        let distance = (0..3).fold(S::zero(), |v, k| {
            v + face_normal[k] * (face_position_m[k] - position_m[k])
        });
        if !wall_support
            || !distance.finite_first_order()
            || distance.value() <= 0.
            || !proof.certifies_positive_wall_distance()
        {
            return Err(fail("certified continuous wall has no positive-distance interior support"));
        }
        Ok(Self { left_slot, position_m, face_position_m, face_normal })
    }
    pub fn position_m(&self) -> [S; 3] {
        self.position_m
    }
    pub fn left_slot(&self) -> usize {
        self.left_slot
    }
    pub(crate) fn validate_face(
        &self,
        left: usize,
        position: [S; 3],
        normal: [S; 3],
    ) -> CaeResult<()> {
        if left != self.left_slot {
            return Err(fail("continuous wall anchor slot ownership differs"));
        }
        for (a, b) in self
            .face_position_m
            .iter()
            .chain(&self.face_normal)
            .zip(position.iter().chain(&normal))
        {
            if a.value().to_bits() != b.value().to_bits() {
                return Err(fail("continuous wall anchor face ownership differs"));
            }
            let d = *a - *b;
            let dirs = d.geometry_first_order_directions().ok_or_else(|| {
                fail("continuous wall anchor requires authenticated geometry directions")
            })?;
            if dirs.iter().any(|v| *v != 0.) {
                return Err(fail(
                    "continuous wall anchor face direction ownership differs",
                ));
            }
        }
        Ok(())
    }
    pub fn constant_lift<T: TransientGasScalar>(&self) -> QuadraticWallInteriorAnchor<T> {
        let lift = |v: S| T::from_f64(v.value());
        QuadraticWallInteriorAnchor {
            left_slot: self.left_slot,
            position_m: self.position_m.map(lift),
            face_position_m: self.face_position_m.map(lift),
            face_normal: self.face_normal.map(lift),
        }
    }
}
#[derive(Clone)]
pub struct AdjacentWallMoments<S> {
    pub wall_density_kg_m3: S,
    pub density_gradient: [S; 3],
    pub momentum_gradient: [[S; 3]; 3],
}
pub fn adjacent_wall_gradients<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    adjacent_density: S,
    adjacent_momentum: [S; 3],
    centroid_to_face_m: [S; 3],
    material_wall_velocity_m_s: [S; 3],
    interior_density_gradient: [S; 3],
    interior_momentum_gradient: [[S; 3]; 3],
) -> CaeResult<([S; 3], [[S; 3]; 3])> {
    let value = adjacent_wall_moments(
        geometry,
        adjacent_density,
        adjacent_momentum,
        centroid_to_face_m,
        material_wall_velocity_m_s,
        interior_density_gradient,
        interior_momentum_gradient,
    )?;
    Ok((value.density_gradient, value.momentum_gradient))
}
pub fn adjacent_wall_moments<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    adjacent_density: S,
    adjacent_momentum: [S; 3],
    centroid_to_face_m: [S; 3],
    material_wall_velocity_m_s: [S; 3],
    interior_density_gradient: [S; 3],
    interior_momentum_gradient: [[S; 3]; 3],
) -> CaeResult<AdjacentWallMoments<S>> {
    if !adjacent_density.finite_first_order()
        || adjacent_density.value() < 0.
        || adjacent_momentum
            .iter()
            .chain(&centroid_to_face_m)
            .chain(&material_wall_velocity_m_s)
            .chain(&interior_density_gradient)
            .chain(interior_momentum_gradient.iter().flatten())
            .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid adjacent continuous wall state"));
    }
    if adjacent_density.value() == 0. && adjacent_momentum.iter().any(|v| v.value() != 0.) {
        return Err(fail("vacuum wall-adjacent state has momentum"));
    }
    let normal = geometry.unit_normal();
    let distance = (0..3).fold(S::zero(), |v, k| v + normal[k] * centroid_to_face_m[k]);
    if !distance.finite_first_order() || distance.value() <= 0. {
        return Err(fail(
            "continuous wall needs positive normal centroid distance",
        ));
    }
    let density_normal = (0..3).fold(S::zero(), |v, k| {
        v + normal[k] * interior_density_gradient[k]
    });
    let density_gradient =
        std::array::from_fn(|k| interior_density_gradient[k] - normal[k] * density_normal);
    let tangent_offset: [S; 3] =
        std::array::from_fn(|k| centroid_to_face_m[k] - distance * normal[k]);
    let wall_density = adjacent_density
        + (0..3).fold(S::zero(), |v, k| {
            v + density_gradient[k] * tangent_offset[k]
        });
    if !wall_density.finite_first_order() || wall_density.value() < 0. {
        return Err(fail(
            "continuous wall tangential density is negative or nonfinite",
        ));
    }
    let momentum_gradient = std::array::from_fn(|component| {
        let old_normal = (0..3).fold(S::zero(), |v, k| {
            v + normal[k] * interior_momentum_gradient[component][k]
        });
        let tangent_momentum = (0..3).fold(S::zero(), |v, k| {
            v + (interior_momentum_gradient[component][k] - normal[k] * old_normal)
                * tangent_offset[k]
        });
        let wall_normal = (wall_density * material_wall_velocity_m_s[component]
            - adjacent_momentum[component]
            - tangent_momentum)
            / distance;
        std::array::from_fn(|k| {
            interior_momentum_gradient[component][k] + normal[k] * (wall_normal - old_normal)
        })
    });
    if density_gradient
        .iter()
        .chain(momentum_gradient.iter().flatten())
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("continuous wall gradient is nonfinite"));
    }
    Ok(AdjacentWallMoments {
        wall_density_kg_m3: wall_density,
        density_gradient,
        momentum_gradient,
    })
}
fn state<S: TransientGasScalar>(
    a: &QuadraticDiffuseWallState<S>,
    unit: &[S],
    rho: S,
) -> QuadraticMultiscaleFaceState<S> {
    QuadraticMultiscaleFaceState {
        left_density: a.outgoing_density.clone(),
        right_density: unit.iter().map(|x| *x * rho).collect(),
        left_density_gradient: a.outgoing_density_gradient.clone(),
        right_density_gradient: std::array::from_fn(|_| vec![S::zero(); unit.len()]),
        equilibrium_mass_gradient: a.equilibrium_mass_gradient,
        equilibrium_momentum_gradient: a.equilibrium_momentum_gradient,
        gas_rt_m2_s2: a.gas_rt_m2_s2,
        molecular_viscosity_pa_s: a.molecular_viscosity_pa_s,
        step_s: a.step_s,
    }
}
fn lift<S: TransientGasScalar, T: TransientGasScalar>(
    a: &QuadraticDiffuseWallState<S>,
) -> QuadraticDiffuseWallState<T> {
    QuadraticDiffuseWallState {
        material_wall_velocity_m_s: a.material_wall_velocity_m_s.map(|v| T::from_f64(v.value())),
        outgoing_density: a
            .outgoing_density
            .iter()
            .map(|v| T::from_f64(v.value()))
            .collect(),
        outgoing_density_gradient: std::array::from_fn(|j| {
            a.outgoing_density_gradient[j]
                .iter()
                .map(|v| T::from_f64(v.value()))
                .collect()
        }),
        equilibrium_mass_gradient: a.equilibrium_mass_gradient.map(|v| T::from_f64(v.value())),
        equilibrium_momentum_gradient: a
            .equilibrium_momentum_gradient
            .map(|row| row.map(|v| T::from_f64(v.value()))),
        gas_rt_m2_s2: T::from_f64(a.gas_rt_m2_s2.value()),
        molecular_viscosity_pa_s: T::from_f64(a.molecular_viscosity_pa_s.value()),
        step_s: T::from_f64(a.step_s.value()),
    }
}
pub fn evaluate<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticDiffuseWallState<S>,
    entropy: &QuadraticEntropyPolicy,
    p: &QuadraticDiffuseWallPolicy,
) -> CaeResult<QuadraticDiffuseWallSolution<S>> {
    if !p.mass_flux_absolute_tolerance_kg_m2_s.is_finite()
        || p.mass_flux_absolute_tolerance_kg_m2_s < 0.
        || !p.mass_flux_relative_tolerance.is_finite()
        || p.mass_flux_relative_tolerance <= 0.
        || p.maximum_iterations == 0
    {
        return Err(fail("invalid continuous diffuse-wall solver policy"));
    }
    if a.material_wall_velocity_m_s
        .iter()
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid continuous material wall velocity"));
    }
    let relative = std::array::from_fn::<_, 3, _>(|j| {
        a.material_wall_velocity_m_s[j] - geometry.ale_velocity()[j]
    });
    let normal = geometry.unit_normal();
    let normal_speed = (0..3).fold(S::zero(), |s, j| s + relative[j] * normal[j]);
    let scale = a
        .material_wall_velocity_m_s
        .iter()
        .chain(&geometry.ale_velocity())
        .map(|v| v.value().abs())
        .fold(1., f64::max);
    let direction_scale = a
        .material_wall_velocity_m_s
        .iter()
        .chain(&geometry.ale_velocity())
        .chain(&normal)
        .map(|v| v.maximum_direction())
        .fold(1., f64::max)
        * (1. + scale);
    if normal_speed.value().abs() > 256. * f64::EPSILON * scale
        || normal_speed.maximum_direction() > 256. * f64::EPSILON * direction_scale
    {
        return Err(fail(
            "continuous material wall must match the geometric normal velocity",
        ));
    }
    let basis = geometry.basis();
    let unit = geometry
        .gaussian()
        .equilibrium(
            S::one(),
            a.material_wall_velocity_m_s,
            a.gas_rt_m2_s2,
            entropy,
        )?
        .coefficient_density;
    let incoming_unit = geometry.incoming_speed_action(&unit)?;
    let denominator = basis.weak_moments(&incoming_unit)?[0];
    if !denominator.finite_first_order() || denominator.value() <= 0. {
        return Err(fail(
            "continuous diffuse wall lacks a resolved incoming halfspace",
        ));
    }
    let initial = basis.weak_moments(&geometry.outgoing_speed_action(&a.outgoing_density)?)?[0];
    if !initial.finite_first_order() || initial.value() < 0. {
        return Err(fail(
            "continuous diffuse-wall outgoing mass flux is invalid",
        ));
    }
    let gd = geometry.constant_lift::<Dual<1>>();
    let ad = lift::<S, Dual<1>>(a);
    let ud = unit
        .iter()
        .map(|v| Dual::<1>::constant(v.value()))
        .collect::<Vec<_>>();
    let denominator_primal = denominator.value();
    let mut rho = initial.value() / denominator_primal;
    let mut qualified = None;
    for iteration in 0..p.maximum_iterations {
        let r = Dual::new(rho, [1.]);
        let result = evaluate_prepared(&gd, &state(&ad, &ud, r), entropy)?;
        let outgoing = basis.weak_moments(&result.positive_weak_flux)?[0];
        let residual = r * denominator_primal - outgoing;
        let tolerance = p.mass_flux_absolute_tolerance_kg_m2_s
            + p.mass_flux_relative_tolerance
                * (rho * denominator_primal).abs().max(outgoing.re.abs());
        if !tolerance.is_finite()
            || !residual.finite_first_order()
            || residual.eps[0] <= 0.
            || outgoing.re < 0.
        {
            return Err(fail(
                "continuous diffuse-wall density equation is inadmissible",
            ));
        }
        if residual.re.abs() <= tolerance {
            qualified = Some((rho, residual.eps[0], iteration));
            break;
        }
        let direction = -residual.re / residual.eps[0];
        let mut fraction = 1.;
        let mut admitted = false;
        for _ in 0..40 {
            let trial = rho + fraction * direction;
            if trial >= 0. && trial.is_finite() {
                if let Ok(value) =
                    evaluate_prepared(&gd, &state(&ad, &ud, Dual::constant(trial)), entropy)
                {
                    let out = basis.weak_moments(&value.positive_weak_flux)?[0].re;
                    if out >= 0. && (trial * denominator_primal - out).abs() < residual.re.abs() {
                        rho = trial;
                        admitted = true;
                        break;
                    }
                }
            }
            fraction *= 0.5;
        }
        if !admitted {
            return Err(fail(
                "continuous diffuse-wall density solve failed admission",
            ));
        }
    }
    let (value, slope, iterations) =
        qualified.ok_or_else(|| fail("continuous diffuse-wall density solve did not converge"))?;
    let fixed = evaluate_prepared(geometry, &state(a, &unit, S::from_f64(value)), entropy)?;
    let fixed_out = basis.weak_moments(&fixed.positive_weak_flux)?[0];
    let residual = S::from_f64(value) * denominator - fixed_out;
    let wall_density = S::from_f64(value) - (residual - S::from_f64(residual.value())) / slope;
    let final_out =
        evaluate_prepared(geometry, &state(a, &unit, wall_density), entropy)?.positive_weak_flux;
    let flux = final_out
        .into_iter()
        .zip(incoming_unit)
        .map(|(out, incoming)| out - wall_density * incoming)
        .collect::<Vec<_>>();
    let moments = basis.weak_moments(&flux)?;
    let mass_defect = moments[0];
    let tolerance = p.mass_flux_absolute_tolerance_kg_m2_s
        + p.mass_flux_relative_tolerance * (wall_density.value() * denominator_primal).abs();
    if !tolerance.is_finite()
        || !wall_density.finite_first_order()
        || wall_density.value() < 0.
        || !mass_defect.finite_first_order()
        || mass_defect.value().abs() > tolerance
    {
        return Err(fail(
            "continuous diffuse-wall conservation certificate failed",
        ));
    }
    let force = [moments[1], moments[2], moments[3]];
    let work = (0..3).fold(S::zero(), |s, j| {
        s + force[j] * a.material_wall_velocity_m_s[j]
    });
    let energy = (moments[4] + moments[5] + moments[6]) * 0.5;
    if [work, energy].iter().any(|v| !v.finite_first_order()) {
        return Err(fail("continuous diffuse-wall energy ledger is nonfinite"));
    }
    Ok(QuadraticDiffuseWallSolution {
        weak_flux: flux,
        flux_moments: moments,
        wall_density_kg_m3: wall_density,
        wall_momentum_flux_n_m2: force,
        wall_work_flux_w_m2: work,
        wall_heat_flux_w_m2: energy - work,
        mass_flux_defect_kg_m2_s: mass_defect,
        iterations,
    })
}

pub fn evaluate_multiscale<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticDiffuseWallState<S>,
    adjacent_gas_pressure_pa: S,
    entropy: &QuadraticEntropyPolicy,
    p: &QuadraticDiffuseWallPolicy,
) -> CaeResult<QuadraticDiffuseWallSolution<S>> {
    evaluate_multiscale_cached(geometry, a, adjacent_gas_pressure_pa, entropy, p, None)
}
pub fn evaluate_multiscale_cached<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticDiffuseWallState<S>,
    adjacent_gas_pressure_pa: S,
    entropy: &QuadraticEntropyPolicy,
    p: &QuadraticDiffuseWallPolicy,
    cache: Option<&[crate::gas_quadratic_reconstruction_admission::MinimumDensityCache; 2]>,
) -> CaeResult<QuadraticDiffuseWallSolution<S>> {
    if !p.mass_flux_absolute_tolerance_kg_m2_s.is_finite()
        || p.mass_flux_absolute_tolerance_kg_m2_s < 0.
        || !p.mass_flux_relative_tolerance.is_finite()
        || p.mass_flux_relative_tolerance <= 0.
        || p.maximum_iterations == 0
    {
        return Err(fail("invalid continuous diffuse-wall solver policy"));
    }
    if a.material_wall_velocity_m_s
        .iter()
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid continuous material wall velocity"));
    }
    let relative = std::array::from_fn::<_, 3, _>(|j| {
        a.material_wall_velocity_m_s[j] - geometry.ale_velocity()[j]
    });
    let normal = geometry.unit_normal();
    let normal_speed = (0..3).fold(S::zero(), |s, j| s + relative[j] * normal[j]);
    let scale = a
        .material_wall_velocity_m_s
        .iter()
        .chain(&geometry.ale_velocity())
        .map(|v| v.value().abs())
        .fold(1., f64::max);
    let direction_scale = a
        .material_wall_velocity_m_s
        .iter()
        .chain(&geometry.ale_velocity())
        .chain(&normal)
        .map(|v| v.maximum_direction())
        .fold(1., f64::max)
        * (1. + scale);
    if normal_speed.value().abs() > 256. * f64::EPSILON * scale
        || normal_speed.maximum_direction() > 256. * f64::EPSILON * direction_scale
    {
        return Err(fail(
            "continuous material wall must match the geometric normal velocity",
        ));
    }

    if !adjacent_gas_pressure_pa.finite_first_order()
        || adjacent_gas_pressure_pa.value() < 0.
        || !a.gas_rt_m2_s2.finite_first_order()
        || a.gas_rt_m2_s2.value() <= 0.
    {
        return Err(fail("invalid multiscale wall adjacent gas pressure"));
    }
    let basis = geometry.basis();
    let unit = geometry
        .gaussian()
        .equilibrium(
            S::one(),
            a.material_wall_velocity_m_s,
            a.gas_rt_m2_s2,
            entropy,
        )?
        .coefficient_density;
    let incoming = geometry.incoming_speed_action(&unit)?;
    let denominator = basis.weak_moments(&incoming)?[0];
    if !denominator.finite_first_order() || denominator.value() <= 0. {
        return Err(fail("multiscale wall lacks resolved incoming velocities"));
    }
    let result = crate::gas_quadratic_multiscale_flux::evaluate_prepared_with_equilibrium_cached(
        geometry,
        &state(a, &unit, S::zero()),
        entropy,
        adjacent_gas_pressure_pa / a.gas_rt_m2_s2,
        a.material_wall_velocity_m_s,
        cache,
    )?;
    let outgoing = basis.weak_moments(&result.positive_weak_flux)?[0];
    let wall_density = outgoing / denominator;
    if !wall_density.finite_first_order() || wall_density.value() < 0. {
        return Err(fail("invalid multiscale reflected density"));
    }
    let flux = result
        .positive_weak_flux
        .iter()
        .zip(incoming)
        .map(|(a, b)| *a - b * wall_density)
        .collect::<Vec<_>>();
    let moments = basis.weak_moments(&flux)?;
    let tolerance = p.mass_flux_absolute_tolerance_kg_m2_s
        + p.mass_flux_relative_tolerance
            * (outgoing.value().abs() + (denominator * wall_density).value().abs());
    if !tolerance.is_finite() || moments[0].value().abs() > tolerance {
        return Err(fail("multiscale wall diffuse mass balance failed"));
    }
    let momentum = [moments[1], moments[2], moments[3]];
    let work = (0..3).fold(S::zero(), |v, j| {
        v + momentum[j] * a.material_wall_velocity_m_s[j]
    });
    let heat = (moments[4] + moments[5] + moments[6]) * 0.5 - work;
    if [work, heat]
        .iter()
        .chain(&moments)
        .any(|x| !x.finite_first_order())
    {
        return Err(fail("nonfinite multiscale wall transfer"));
    }
    Ok(QuadraticDiffuseWallSolution {
        weak_flux: flux,
        flux_moments: moments,
        wall_density_kg_m3: wall_density,
        wall_momentum_flux_n_m2: momentum,
        wall_work_flux_w_m2: work,
        wall_heat_flux_w_m2: heat,
        mass_flux_defect_kg_m2_s: moments[0],
        iterations: 0,
    })
}
