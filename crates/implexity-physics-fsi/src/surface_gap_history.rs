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

use crate::surface_gap::GapCellGeometry;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn add<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn scale<S: Scalar>(a: [S; 3], s: S) -> [S; 3] {
    std::array::from_fn(|i| a[i] * s)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm<S: Scalar>(v: [S; 3]) -> S {
    dot(v, v).sqrt()
}
fn midpoint_vertex<S: Scalar>(g: &GapCellGeometry<S>, i: usize) -> [S; 3] {
    scale(add(g.vertices_m[i], g.vertices_m[i + 3]), S::from_f64(0.5))
}
fn centroid<S: Scalar>(g: &GapCellGeometry<S>) -> [S; 3] {
    scale(
        add(
            add(midpoint_vertex(g, 0), midpoint_vertex(g, 1)),
            midpoint_vertex(g, 2),
        ),
        S::from_f64(1. / 3.),
    )
}

#[derive(Clone, Debug)]
pub struct GapVolumeFace<S> {
    pub left: usize,
    pub right: Option<usize>,
    pub port: Option<usize>,
    pub conductance_m3_pa_s: S,
    pub full_liquid_advection_m3_s: S,
    pub boundary_liquid_fraction: S,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrthogonalFacePolicy {
    pub maximum_skew_ratio: f64,
    pub maximum_dihedral_angle_rad: f64,
    pub position_tolerance_m: f64,
}

pub fn orthogonal_face<S: Scalar>(
    left: usize,
    left_geometry: &GapCellGeometry<S>,
    left_edge: usize,
    right: Option<(usize, &GapCellGeometry<S>, usize)>,
    port: Option<usize>,
    viscosity_pa_s: S,
    relative_velocity_m_s: [S; 3],
    boundary_liquid_fraction: S,
    policy: &OrthogonalFacePolicy,
) -> CaeResult<GapVolumeFace<S>> {
    if left_edge >= 3
        || right.is_some_and(|(_, _, edge)| edge >= 3)
        || right.is_some() == port.is_some()
        || !viscosity_pa_s.value().is_finite()
        || viscosity_pa_s.value() <= 0.
        || relative_velocity_m_s.iter().any(|v| !v.value().is_finite())
        || !boundary_liquid_fraction.value().is_finite()
        || !(0. ..=1.).contains(&boundary_liquid_fraction.value())
        || ![
            policy.maximum_skew_ratio,
            policy.maximum_dihedral_angle_rad,
            policy.position_tolerance_m,
        ]
        .iter()
        .all(|v| v.is_finite() && *v > 0.)
        || policy.maximum_dihedral_angle_rad >= std::f64::consts::FRAC_PI_2
    {
        return Err(fail("invalid orthogonal gap-face specification"));
    }
    let i = left_edge;
    let j = (i + 1) % 3;
    let a = midpoint_vertex(left_geometry, i);
    let b = midpoint_vertex(left_geometry, j);
    let length = norm(sub(b, a));
    let tangent = scale(sub(b, a), S::from_f64(1.) / length);
    let normal = cross(tangent, left_geometry.normal);
    let centre = scale(add(a, b), S::from_f64(0.5));
    let offset = sub(centre, centroid(left_geometry));
    let dl = dot(offset, normal);
    if !length.value().is_finite()
        || length.value() <= 0.
        || dl.value() <= 0.
        || (right.is_none()
            && dot(offset, tangent).value().abs() / dl.value() > policy.maximum_skew_ratio)
    {
        return Err(fail(
            "gap face requires an orthogonal positive control-volume metric",
        ));
    }
    let hleft = left_geometry.hydraulic_gap_m;
    let conductivity = hleft * hleft * hleft;
    let mut resistance = dl / conductivity;
    if let Some((_, g, edge)) = right {
        let ar = midpoint_vertex(g, edge);
        let br = midpoint_vertex(g, (edge + 1) % 3);
        if norm(sub(a, br)).value() > policy.position_tolerance_m
            || norm(sub(b, ar)).value() > policy.position_tolerance_m
            || dot(g.normal, left_geometry.normal).value() < policy.maximum_dihedral_angle_rad.cos()
        {
            return Err(fail("gap cells do not share a coherent face"));
        }
        let tr = scale(sub(br, ar), S::from_f64(1.) / norm(sub(br, ar)));
        let nr = cross(tr, g.normal);
        let or = sub(centre, centroid(g));
        let dr = dot(or, nr);
        if dr.value() <= 0. || dot(sub(centroid(g),centroid(left_geometry)), tangent).value().abs() / (dl+dr).value() > policy.maximum_skew_ratio {
            return Err(fail(
                "neighbour gap metric requires orthogonal reconstruction",
            ));
        }
        let hr = g.hydraulic_gap_m;
        let kr = hr * hr * hr;
        resistance += dr / kr;
    }
    let conductance = length / (viscosity_pa_s * 12. * resistance);
    let edge_gap = (dot(
        sub(left_geometry.vertices_m[i + 3], left_geometry.vertices_m[i]),
        left_geometry.normal,
    ) + dot(
        sub(left_geometry.vertices_m[j + 3], left_geometry.vertices_m[j]),
        left_geometry.normal,
    )) * 0.5;
    let full_advection = dot(relative_velocity_m_s, normal) * edge_gap * length;
    if !conductance.value().is_finite()
        || conductance.value() <= 0.
        || !full_advection.value().is_finite()
    {
        return Err(fail("nonfinite surface-gap conductance"));
    }
    Ok(GapVolumeFace {
        left,
        right: right.map(|(i, _, _)| i),
        port,
        conductance_m3_pa_s: conductance,
        full_liquid_advection_m3_s: full_advection,
        boundary_liquid_fraction,
    })
}

pub fn volume_residual<S: Scalar>(
    capacity: &[S],
    pressure: &[S],
    volume: &[S],
    previous_volume: &[S],
    ports: &[S],
    faces: &[GapVolumeFace<S>],
    time_step_s: S,
) -> CaeResult<Vec<S>> {
    let n = capacity.len();
    if n == 0
        || [pressure.len(), volume.len(), previous_volume.len()]
            .iter()
            .any(|m| *m != n)
        || !time_step_s.value().is_finite()
        || time_step_s.value() <= 0.
        || capacity
            .iter()
            .any(|v| !v.value().is_finite() || v.value() <= 0.)
        || pressure
            .iter()
            .chain(volume)
            .chain(previous_volume)
            .chain(ports)
            .any(|v| !v.value().is_finite())
        || previous_volume.iter().any(|v| v.value() < 0.)
    {
        return Err(fail("invalid moving-volume gap fields"));
    }
    let mut residual = volume
        .iter()
        .zip(previous_volume)
        .map(|(v, o)| (*v - *o) / time_step_s)
        .collect::<Vec<_>>();
    for face in faces {
        if face.left >= n
            || face.right.is_some_and(|i| i >= n || i == face.left)
            || face.right.is_some() == face.port.is_some()
            || face.port.is_some_and(|i| i >= ports.len())
            || !face.conductance_m3_pa_s.value().is_finite()
            || face.conductance_m3_pa_s.value() <= 0.
            || !face.full_liquid_advection_m3_s.value().is_finite()
            || !face.boundary_liquid_fraction.value().is_finite()
            || !(0. ..=1.).contains(&face.boundary_liquid_fraction.value())
        {
            return Err(fail("invalid moving-volume gap face"));
        }
        let left = face.left;
        let pr = if let Some(right) = face.right {
            pressure[right]
        } else {
            ports[face.port.ok_or_else(|| fail("gap pressure port absent"))?]
        };
        let theta = if face.full_liquid_advection_m3_s.value() >= 0. {
            volume[left] / capacity[left]
        } else if let Some(right) = face.right {
            volume[right] / capacity[right]
        } else {
            face.boundary_liquid_fraction
        };
        let q = face.conductance_m3_pa_s * (pressure[left] - pr)
            + face.full_liquid_advection_m3_s * theta;
        residual[left] += q;
        if let Some(right) = face.right {
            residual[right] -= q;
        }
    }
    if residual.iter().any(|v| !v.value().is_finite()) {
        return Err(fail("moving-volume gap residual overflow"));
    }
    Ok(residual)
}

pub fn complementarity_residual<S: Scalar>(
    capacity: &[S],
    pressure: &[S],
    volume: &[S],
    pressure_scale_pa: f64,
) -> CaeResult<Vec<S>> {
    if capacity.len() != pressure.len()
        || capacity.len() != volume.len()
        || !pressure_scale_pa.is_finite()
        || pressure_scale_pa <= 0.
        || capacity
            .iter()
            .any(|v| !v.value().is_finite() || v.value() <= 0.)
        || pressure
            .iter()
            .chain(volume)
            .any(|v| !v.value().is_finite())
    {
        return Err(fail("invalid volume complementarity fields"));
    }
    capacity
        .iter()
        .zip(pressure)
        .zip(volume)
        .map(|((c, p), m)| {
            let a = *p / pressure_scale_pa;
            let b = S::from_f64(1.) - *m / *c;
            if a.value() == 0. && b.value() == 0. {
                return Err(fail(
                    "volume complementarity derivative is undefined at cavitation transition",
                ));
            }
            Ok((a * a + b * b).sqrt() - a - b)
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct GapVolumeSolution {
    pub pressure_pa: Vec<f64>,
    pub liquid_volume_m3: Vec<f64>,
    pub iterations: usize,
    pub residual_ratio: f64,
}

pub fn solve_liquid_volume(
    capacity: &[f64],
    previous_volume: &[f64],
    ports: &[f64],
    faces: &[GapVolumeFace<f64>],
    time_step_s: f64,
    tolerance: f64,
    max_iterations: usize,
) -> CaeResult<GapVolumeSolution> {
    let n = capacity.len();
    let _ = volume_residual(
        capacity,
        &vec![0.; n],
        previous_volume,
        previous_volume,
        ports,
        faces,
        time_step_s,
    )?;
    if ports.iter().any(|p| *p < 0.)
        || !tolerance.is_finite()
        || tolerance <= 0.
        || max_iterations == 0
    {
        return Err(fail("invalid liquid volume solve options"));
    }
    let mut pressure = vec![0.; n];
    let mut volume = previous_volume
        .iter()
        .zip(capacity)
        .map(|(v, c)| v.min(*c))
        .collect::<Vec<_>>();
    let scale = previous_volume
        .iter()
        .chain(capacity)
        .fold(0f64, |a, b| a.max(*b))
        / time_step_s;
    for iteration in 1..=max_iterations {
        for i in 0..n {
            let mut rhs = previous_volume[i] / time_step_s;
            let mut pd = 0.;
            let mut md = 1. / time_step_s;
            for face in faces {
                let (other, port, k, a) = if face.left == i {
                    (
                        face.right,
                        face.port,
                        face.conductance_m3_pa_s,
                        face.full_liquid_advection_m3_s,
                    )
                } else if face.right == Some(i) {
                    (
                        Some(face.left),
                        None,
                        face.conductance_m3_pa_s,
                        -face.full_liquid_advection_m3_s,
                    )
                } else {
                    continue;
                };
                pd += k;
                rhs += k * other.map_or_else(|| ports[port.unwrap_or(0)], |j| pressure[j]);
                if a >= 0. {
                    md += a / capacity[i];
                } else {
                    rhs -= a * other
                        .map_or(face.boundary_liquid_fraction, |j| volume[j] / capacity[j]);
                }
            }
            let saturated_rhs = md * capacity[i];
            if rhs > saturated_rhs {
                if pd <= 0. {
                    return Err(CaeError::convergence(
                        "sealed liquid volume exceeds moving capacity",
                    ));
                }
                pressure[i] = (rhs - saturated_rhs) / pd;
                volume[i] = capacity[i];
            } else {
                pressure[i] = 0.;
                volume[i] = rhs / md;
            }
        }
        let residual = volume_residual(
            capacity,
            &pressure,
            &volume,
            previous_volume,
            ports,
            faces,
            time_step_s,
        )?;
        let ratio = residual.iter().fold(0f64, |a, b| a.max(b.abs())) / scale;
        if ratio <= tolerance {
            return Ok(GapVolumeSolution {
                pressure_pa: pressure,
                liquid_volume_m3: volume,
                iterations: iteration,
                residual_ratio: ratio,
            });
        }
    }
    Err(CaeError::convergence(
        "moving liquid-volume solve did not converge",
    ))
}

pub fn solve_gas_volume(
    capacity: &[f64],
    previous_capacity: &[f64],
    ports: &[f64],
    faces: &[GapVolumeFace<f64>],
    time_step_s: f64,
    tolerance: f64,
    max_iterations: usize,
) -> CaeResult<GapVolumeSolution> {
    let n = capacity.len();
    let _ = volume_residual(
        capacity,
        &vec![0.; n],
        capacity,
        previous_capacity,
        ports,
        faces,
        time_step_s,
    )?;
    if !tolerance.is_finite()
        || tolerance <= 0.
        || max_iterations == 0
        || faces.iter().any(|f| f.boundary_liquid_fraction != 1.)
    {
        return Err(fail("invalid saturated gas-volume solve options"));
    }
    let mut visited = vec![false; n];
    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut has_port = false;
        while let Some(i) = stack.pop() {
            for face in faces {
                if face.left == i || face.right == Some(i) {
                    has_port |= face.port.is_some();
                    let neighbour = if face.left == i {
                        face.right
                    } else {
                        Some(face.left)
                    };
                    if let Some(j) = neighbour {
                        if !visited[j] {
                            visited[j] = true;
                            stack.push(j);
                        }
                    }
                }
            }
        }
        if !has_port {
            return Err(fail("gas-volume component has no pressure reference port"));
        }
    }
    let mut pressure = vec![0.; n];
    let scale = capacity
        .iter()
        .chain(previous_capacity)
        .fold(0f64, |a, b| a.max(*b))
        / time_step_s;
    for iteration in 1..=max_iterations {
        for i in 0..n {
            let mut rhs = (previous_capacity[i] - capacity[i]) / time_step_s;
            let mut diagonal = 0.;
            for face in faces {
                let (other, port, sign) = if face.left == i {
                    (face.right, face.port, 1.)
                } else if face.right == Some(i) {
                    (Some(face.left), None, -1.)
                } else {
                    continue;
                };
                let k = face.conductance_m3_pa_s;
                diagonal += k;
                rhs += k * other.map_or_else(|| ports[port.unwrap_or(0)], |j| pressure[j])
                    - sign * face.full_liquid_advection_m3_s;
            }
            if diagonal <= 0. {
                return Err(fail("gas-volume pressure row is disconnected"));
            }
            pressure[i] = rhs / diagonal;
        }
        let residual = volume_residual(
            capacity,
            &pressure,
            capacity,
            previous_capacity,
            ports,
            faces,
            time_step_s,
        )?;
        let ratio = residual.iter().fold(0f64, |a, b| a.max(b.abs())) / scale;
        if ratio <= tolerance {
            return Ok(GapVolumeSolution {
                pressure_pa: pressure,
                liquid_volume_m3: capacity.to_vec(),
                iterations: iteration,
                residual_ratio: ratio,
            });
        }
    }
    Err(CaeError::convergence(
        "moving gas-volume solve did not converge",
    ))
}

pub fn volume_face_fluxes<S: Scalar>(
    capacity: &[S],
    pressure: &[S],
    volume: &[S],
    ports: &[S],
    faces: &[GapVolumeFace<S>],
) -> CaeResult<Vec<S>> {
    let _ = volume_residual(
        capacity,
        pressure,
        volume,
        volume,
        ports,
        faces,
        S::from_f64(1.),
    )?;
    Ok(faces
        .iter()
        .map(|face| {
            let right_pressure = face
                .right
                .map_or_else(|| ports[face.port.unwrap_or(0)], |j| pressure[j]);
            let theta = if face.full_liquid_advection_m3_s.value() >= 0. {
                volume[face.left] / capacity[face.left]
            } else {
                face.right
                    .map_or(face.boundary_liquid_fraction, |j| volume[j] / capacity[j])
            };
            face.conductance_m3_pa_s * (pressure[face.left] - right_pressure)
                + face.full_liquid_advection_m3_s * theta
        })
        .collect())
}

#[derive(Clone, Debug)]
pub struct GasVolumeFeedback<S> {
    pub cell_pressure_pa: Vec<S>,
    pub port_pressure_pa: Vec<S>,
    pub face_flux_m3_s: Vec<S>,
}

pub fn solve_gas_volume_feedback<S: Scalar>(
    capacity: &[S],
    previous_capacity: &[S],
    port_base_pressure_pa: &[S],
    port_pressure_per_flux_pa_s_m3: &[S],
    faces: &[GapVolumeFace<S>],
    time_step_s: S,
) -> CaeResult<GasVolumeFeedback<S>> {
    let n = capacity.len();
    let m = port_base_pressure_pa.len();
    let count = n
        .checked_add(m)
        .ok_or_else(|| fail("gas feedback shape overflow"))?;
    if count > 256
        || m == 0
        || port_pressure_per_flux_pa_s_m3.len() != m
        || port_pressure_per_flux_pa_s_m3
            .iter()
            .any(|v| !v.value().is_finite() || v.value() < 0.)
        || faces
            .iter()
            .any(|f| f.boundary_liquid_fraction.value() != 1.)
    {
        return Err(fail("invalid gas feedback layout or dense cell-port limit"));
    }
    let _ = volume_residual(
        capacity,
        &vec![S::zero(); n],
        capacity,
        previous_capacity,
        port_base_pressure_pa,
        faces,
        time_step_s,
    )?;
    let mut a = vec![vec![S::zero(); count]; count];
    let mut b = vec![S::zero(); count];
    for i in 0..n {
        b[i] = (previous_capacity[i] - capacity[i]) / time_step_s;
    }
    for j in 0..m {
        a[n + j][n + j] = S::from_f64(1.);
        b[n + j] = port_base_pressure_pa[j];
    }
    for face in faces {
        let l = face.left;
        let k = face.conductance_m3_pa_s;
        let q = face.full_liquid_advection_m3_s;
        let r = face.right.unwrap_or(n + face.port.unwrap_or(0));
        a[l][l] += k;
        a[l][r] -= k;
        b[l] -= q;
        if let Some(r) = face.right {
            a[r][r] += k;
            a[r][l] -= k;
            b[r] += q;
        } else {
            let port = face
                .port
                .ok_or_else(|| fail("gas feedback face port absent"))?;
            let gain = port_pressure_per_flux_pa_s_m3[port];
            a[n + port][l] -= gain * k;
            a[n + port][n + port] += gain * k;
            b[n + port] += gain * q;
        }
    }
    for row in 0..count {
        let magnitude = a[row].iter().fold(0f64, |x, v| x.max(v.value().abs()));
        if magnitude == 0. || !magnitude.is_finite() {
            return Err(fail("gas feedback disconnected pressure row"));
        }
        for value in &mut a[row] {
            *value = *value / magnitude;
        }
        b[row] = b[row] / magnitude;
    }
    for pivot in 0..count {
        let row = (pivot..count)
            .max_by(|i, j| {
                a[*i][pivot]
                    .value()
                    .abs()
                    .total_cmp(&a[*j][pivot].value().abs())
            })
            .unwrap_or(pivot);
        a.swap(row, pivot);
        b.swap(row, pivot);
        let d = a[pivot][pivot];
        if !d.value().is_finite() || d.value().abs() < 1e-14 {
            return Err(fail("gas feedback pressure reference is singular"));
        }
        for i in pivot + 1..count {
            let factor = a[i][pivot] / d;
            for j in pivot + 1..count {
                let value = a[pivot][j];
                a[i][j] -= factor * value;
            }
            let value = b[pivot];
            b[i] -= factor * value;
            a[i][pivot] = S::zero();
        }
    }
    let mut x = vec![S::zero(); count];
    for i in (0..count).rev() {
        let mut rhs = b[i];
        for j in i + 1..count {
            rhs -= a[i][j] * x[j];
        }
        x[i] = rhs / a[i][i];
        if !x[i].value().is_finite() {
            return Err(fail("gas feedback pressure overflow"));
        }
    }
    let cell_pressure = x[..n].to_vec();
    let port_pressure = x[n..].to_vec();
    let flux = volume_face_fluxes(capacity, &cell_pressure, capacity, &port_pressure, faces)?;
    Ok(GasVolumeFeedback {
        cell_pressure_pa: cell_pressure,
        port_pressure_pa: port_pressure,
        face_flux_m3_s: flux,
    })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapFaceConnection {
    pub left: usize,
    pub left_edge: usize,
    pub right: Option<[usize; 2]>,
    pub port: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct GapPoiseuilleExchange<S> {
    pub pressure_gradient_pa_m: Vec<[S; 3]>,
    pub cover_force_n: Vec<[[S; 3]; 6]>,
    pub hydraulic_dissipation_w: S,
}

fn face_metric<S: Scalar>(g: &GapCellGeometry<S>, edge: usize) -> ([S; 3], S, S) {
    let a = midpoint_vertex(g, edge);
    let b = midpoint_vertex(g, (edge + 1) % 3);
    let length = norm(sub(b, a));
    let outward = cross(scale(sub(b, a), S::from_f64(1.) / length), g.normal);
    let distance = dot(
        sub(scale(add(a, b), S::from_f64(0.5)), centroid(g)),
        outward,
    );
    (outward, length, distance)
}

pub fn pressure_driven_skin_exchange<S: Scalar>(
    geometry: &[GapCellGeometry<S>],
    pressure: &[S],
    ports: &[S],
    connections: &[GapFaceConnection],
    viscosity_pa_s: S,
    policy: &OrthogonalFacePolicy,
) -> CaeResult<GapPoiseuilleExchange<S>> {
    let n = geometry.len();
    if n == 0 || pressure.len() != n || pressure.iter().chain(ports).any(|p| !p.value().is_finite())
    {
        return Err(fail("invalid pressure-driven skin fields"));
    }
    let mut occupied = vec![[false; 3]; n];
    let mut gradient = vec![[S::zero(); 3]; n];
    let mut normal_matrix = vec![[S::zero(); 3]; n];
    let mut normal_rhs = vec![[S::zero(); 2]; n];
    let mut dissipation = S::zero();
    for connection in connections {
        let l = connection.left;
        let e = connection.left_edge;
        if l >= n
            || e >= 3
            || occupied[l][e]
            || connection
                .right
                .is_some_and(|[r, re]| r >= n || re >= 3 || r == l)
            || connection.right.is_some() == connection.port.is_some()
            || connection.port.is_some_and(|p| p >= ports.len())
        {
            return Err(fail("invalid pressure-driven skin face ownership"));
        }
        occupied[l][e] = true;
        let neighbour = connection.right.map(|[r, re]| (r, &geometry[r], re));
        let face = orthogonal_face(
            l,
            &geometry[l],
            e,
            neighbour,
            connection.port,
            viscosity_pa_s,
            [S::zero(); 3],
            S::from_f64(1.),
            policy,
        )?;
        let (normal, length, dl) = face_metric(&geometry[l], e);
        let h = geometry[l].hydraulic_gap_m;
        let rl = dl / (h * h * h);
        let (pr, pface) = if let Some([r, re]) = connection.right {
            if occupied[r][re] {
                return Err(fail("repeated pressure-driven neighbour face"));
            }
            occupied[r][re] = true;
            let (_, _, dr) = face_metric(&geometry[r], re);
            let hr = geometry[r].hydraulic_gap_m;
            let rr = dr / (hr * hr * hr);
            (
                pressure[r],
                (rr * pressure[l] + rl * pressure[r]) / (rl + rr),
            )
        } else {
            let p = ports[connection
                .port
                .ok_or_else(|| fail("skin pressure port absent"))?];
            (p, p)
        };
        let mut accumulate =
            |cell: usize, edge_normal: [S; 3], edge_length: S, distance: S, face_pressure: S| {
                let g = &geometry[cell];
                let u = dot(edge_normal, g.tangent[0]);
                let v = dot(edge_normal, g.tangent[1]);
                let weight = edge_length * distance;
                let slope = (face_pressure - pressure[cell]) / distance;
                normal_matrix[cell][0] += weight * u * u;
                normal_matrix[cell][1] += weight * u * v;
                normal_matrix[cell][2] += weight * v * v;
                normal_rhs[cell][0] += weight * u * slope;
                normal_rhs[cell][1] += weight * v * slope;
            };
        accumulate(l, normal, length, dl, pface);
        if let Some([r, re]) = connection.right {
            let (nr, lr, dr) = face_metric(&geometry[r], re);
            accumulate(r, nr, lr, dr, pface);
        }
        let difference = pressure[l] - pr;
        dissipation += face.conductance_m3_pa_s * difference * difference;
    }
    if occupied.iter().flatten().any(|v| !*v) {
        return Err(fail(
            "pressure-driven skin requires complete face ownership",
        ));
    }
    for i in 0..n {
        let a = normal_matrix[i];
        let b = normal_rhs[i];
        let determinant = a[0] * a[2] - a[1] * a[1];
        if !determinant.value().is_finite() || determinant.value() <= 0. {
            return Err(fail("pressure-gradient normal reconstruction is singular"));
        }
        let u = (a[2] * b[0] - a[1] * b[1]) / determinant;
        let v = (a[0] * b[1] - a[1] * b[0]) / determinant;
        gradient[i] = add(
            scale(geometry[i].tangent[0], u),
            scale(geometry[i].tangent[1], v),
        );
    }
    let cover = geometry
        .iter()
        .zip(&gradient)
        .map(|(g, grad)| {
            let heights: [S; 3] =
                std::array::from_fn(|i| dot(sub(g.vertices_m[i + 3], g.vertices_m[i]), g.normal));
            let sum: S = heights.into_iter().sum();
            let mut forces = [[S::zero(); 3]; 6];
            for body in 0..2 {
                let v = &g.vertices_m[body * 3..body * 3 + 3];
                let area = norm(cross(sub(v[1], v[0]), sub(v[2], v[0]))) * 0.5;
                for vertex in 0..3 {
                    forces[body * 3 + vertex] = scale(*grad, -area * (sum + heights[vertex]) / 24.);
                }
            }
            forces
        })
        .collect::<Vec<_>>();
    if !dissipation.value().is_finite()
        || gradient
            .iter()
            .flatten()
            .chain(cover.iter().flatten().flatten())
            .any(|v| !v.value().is_finite())
    {
        return Err(fail("pressure-driven skin exchange overflow"));
    }
    Ok(GapPoiseuilleExchange {
        pressure_gradient_pa_m: gradient,
        cover_force_n: cover,
        hydraulic_dissipation_w: dissipation,
    })
}
