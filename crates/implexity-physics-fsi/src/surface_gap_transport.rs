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

use crate::surface_gap::{GapCellGeometry, pressure_loads};
use crate::surface_gap_history::GapFaceConnection;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn norm<S: Scalar>(a: [S; 3]) -> S {
    let q = dot(a, a);
    if q.value() == 0. { S::zero() } else { q.sqrt() }
}
#[derive(Clone, Debug)]
pub struct GapBoundaryTransport<S> {
    pub increments: Vec<[S; 4]>,
    pub cap_force_n: [S; 3],
    pub cap_power_w: S,
    pub advective_momentum_flux_n: [S; 3],
    pub advective_momentum_bound_n: S,
}
pub fn boundary_transport<S: Scalar>(
    geometry: &[GapCellGeometry<S>],
    vertex_velocities: &[[[S; 3]; 6]],
    connections: &[GapFaceConnection],
    port_pressure: &[S],
    face_flux: &[S],
    bulk_port_velocity: &[[S; 3]],
    density: S,
    dt: S,
) -> CaeResult<GapBoundaryTransport<S>> {
    if geometry.is_empty()
        || geometry.len() != vertex_velocities.len()
        || connections.len() != face_flux.len()
        || port_pressure.len() != bulk_port_velocity.len()
        || port_pressure.is_empty()
        || !density.value().is_finite()
        || density.value() <= 0.
        || !dt.value().is_finite()
        || dt.value() <= 0.
        || vertex_velocities
            .iter()
            .flatten()
            .flatten()
            .chain(port_pressure)
            .chain(face_flux)
            .chain(bulk_port_velocity.iter().flatten())
            .any(|v| !v.value().is_finite())
    {
        return Err(fail("invalid surface-gap boundary transport fields"));
    }
    let mut increments = vec![[S::zero(); 4]; port_pressure.len()];
    let mut cap_force = [S::zero(); 3];
    let mut cap_power = S::zero();
    let mut advective = [S::zero(); 3];
    let mut bound = S::zero();
    let mut occupied = vec![[false; 3]; geometry.len()];
    for (connection, &q) in connections.iter().zip(face_flux) {
        if connection.left >= geometry.len()
            || connection.left_edge >= 3
            || connection
                .right
                .is_some_and(|[r, e]| r >= geometry.len() || e >= 3 || r == connection.left)
            || connection.right.is_some() == connection.port.is_some()
        {
            return Err(fail("invalid surface-gap boundary ownership"));
        }
        if connection.right.is_some() {
            continue;
        }
        let p = connection
            .port
            .ok_or_else(|| fail("surface-gap boundary port absent"))?;
        if p >= port_pressure.len() || occupied[connection.left][connection.left_edge] {
            return Err(fail("repeated surface-gap boundary port face"));
        }
        occupied[connection.left][connection.left_edge] = true;
        let g = &geometry[connection.left];
        let v = vertex_velocities[connection.left];
        let edge = connection.left_edge;
        let next = (edge + 1) % 3;
        let unit_load = pressure_loads(g, S::from_f64(1.))?.cap_n[edge];
        let area_vector: [S; 3] = std::array::from_fn(|a| unit_load.iter().map(|f| f[a]).sum());
        let area = norm(area_vector);
        if !area.value().is_finite() || area.value() <= 0. {
            return Err(fail("degenerate surface-gap cap area"));
        }
        let normal = area_vector.map(|a| a / area);
        let mean_velocity: [S; 3] = std::array::from_fn(|a| {
            (v[edge][a] + v[next][a] + v[edge + 3][a] + v[next + 3][a]) * 0.25
        });
        let transported: [S; 3] = if q.value() >= 0. {
            std::array::from_fn(|a| density * q * (mean_velocity[a] + normal[a] * q / area * 1.2))
        } else {
            bulk_port_velocity[p].map(|u| density * q * u)
        };
        increments[p][0] += density * dt * q;
        for node in 0..6 {
            for a in 0..3 {
                let force = unit_load[node][a] * port_pressure[p];
                cap_force[a] += force;
                increments[p][a + 1] += dt * force;
                cap_power += force * v[node][a];
            }
        }
        for a in 0..3 {
            increments[p][a + 1] += dt * transported[a];
            advective[a] += transported[a];
        }
        let absq = if q.value() >= 0. { q } else { -q };
        let wall_jump: [S;3] = std::array::from_fn(|a|
            (v[edge+3][a]+v[next+3][a]-v[edge][a]-v[next][a])*0.5);
        bound += density * absq * (norm(mean_velocity) + norm(bulk_port_velocity[p]) + absq / area * 1.2)
            + density * area * dot(wall_jump,wall_jump) / 12.;
    }
    if !cap_power.value().is_finite()
        || !bound.value().is_finite()
        || increments
            .iter()
            .flatten()
            .chain(&cap_force)
            .chain(&advective)
            .any(|v| !v.value().is_finite())
    {
        return Err(fail("surface-gap boundary transport overflow"));
    }
    Ok(GapBoundaryTransport {
        increments,
        cap_force_n: cap_force,
        cap_power_w: cap_power,
        advective_momentum_flux_n: advective,
        advective_momentum_bound_n: bound,
    })
}

pub fn boundary_transport_derivative_domain<S: Scalar>(
    vertex_velocities: &[[[S; 3]; 6]],
    connections: &[GapFaceConnection],
    face_flux: &[S],
    bulk_port_velocity: &[[S; 3]],
) -> CaeResult<()> {
    if connections.len() != face_flux.len()
        || vertex_velocities
            .iter()
            .flatten()
            .flatten()
            .chain(face_flux)
            .chain(bulk_port_velocity.iter().flatten())
            .any(|v| !v.value().is_finite())
    {
        return Err(fail("invalid boundary transport derivative fields"));
    }
    for (c, q) in connections.iter().zip(face_flux) {
        if c.left >= vertex_velocities.len()
            || c.left_edge >= 3
            || c.right.is_some() == c.port.is_some()
        {
            return Err(fail("invalid boundary transport derivative ownership"));
        }
        if c.right.is_some() {
            continue;
        }
        let p = c
            .port
            .ok_or_else(|| fail("boundary derivative port absent"))?;
        if p >= bulk_port_velocity.len() {
            return Err(fail("boundary derivative port index"));
        }
        if q.value() == 0. {
            let v = vertex_velocities[c.left];
            let e = c.left_edge;
            let j = (e + 1) % 3;
            for a in 0..3 {
                let mean = (v[e][a] + v[j][a] + v[e + 3][a] + v[j + 3][a]) * 0.25;
                if mean.value() != bulk_port_velocity[p][a].value() {
                    return Err(fail(
                        "advective momentum derivative is undefined at flow reversal",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn pressure_profile<S: Scalar>(
    g: &GapCellGeometry<S>,
    edge: usize,
    q: S,
    gradient: [S; 3],
    viscosity: S,
) -> CaeResult<([S; 3], S, [S; 3])> {
    if edge >= 3
        || !viscosity.value().is_finite()
        || viscosity.value() <= 0.
        || gradient.iter().any(|v| !v.value().is_finite())
    {
        return Err(fail("invalid material-ALE pressure profile"));
    }
    let cap = pressure_loads(g, S::from_f64(1.))?.cap_n[edge];
    let av: [S; 3] = std::array::from_fn(|a| cap.iter().map(|f| f[a]).sum());
    let area = norm(av);
    if area.value() <= 0. || !area.value().is_finite() {
        return Err(fail("degenerate pressure profile side area"));
    }
    let normal = av.map(|v| v / area);
    let tangent_gradient: [S; 3] =
        std::array::from_fn(|a| gradient[a] - g.normal[a] * dot(gradient, g.normal));
    let raw =
        tangent_gradient.map(|v| -v * g.hydraulic_gap_m * g.hydraulic_gap_m / (viscosity * 12.));
    let transverse: [S; 3] = std::array::from_fn(|a| raw[a] - normal[a] * dot(raw, normal));
    let full = std::array::from_fn(|a| transverse[a] + normal[a] * q / area);
    Ok((full, area, normal))
}

pub fn boundary_transport_pressure_profile<S: Scalar>(
    geometry: &[GapCellGeometry<S>],
    vertex_velocities: &[[[S; 3]; 6]],
    connections: &[GapFaceConnection],
    port_pressure: &[S],
    face_flux: &[S],
    bulk_port_velocity: &[[S; 3]],
    density: S,
    dt: S,
    pressure_gradient_pa_m: &[[S; 3]],
    viscosity_pa_s: S,
) -> CaeResult<GapBoundaryTransport<S>> {
    if pressure_gradient_pa_m.len() != geometry.len() || pressure_gradient_pa_m.iter().flatten().any(|v|!v.value().is_finite()) {
        return Err(fail("pressure profile gradient shape"));
    }
    let mut out = boundary_transport(
        geometry,
        vertex_velocities,
        connections,
        port_pressure,
        face_flux,
        bulk_port_velocity,
        density,
        dt,
    )?;
    let points = [
        0.5 - (3_f64 / 5.).sqrt() * 0.5,
        0.5,
        0.5 + (3_f64 / 5.).sqrt() * 0.5,
    ];
    let weights = [5. / 18., 4. / 9., 5. / 18.];
    for (c, &q) in connections.iter().zip(face_flux) {
        if c.right.is_some() {
            continue;
        }
        let port = c.port.ok_or_else(|| fail("pressure profile port absent"))?;
        let g = &geometry[c.left];
        let v = vertex_velocities[c.left];
        let e = c.left_edge;
        let j = (e + 1) % 3;
        let (full, area, normal) =
            pressure_profile(g, e, q, pressure_gradient_pa_m[c.left], viscosity_pa_s)?;
        if q.value() < 0. {
            continue;
        }
        let lower: [S; 3] = std::array::from_fn(|a| (v[e][a] + v[j][a]) * 0.5);
        let upper: [S; 3] = std::array::from_fn(|a| (v[e + 3][a] + v[j + 3][a]) * 0.5);
        let mean: [S; 3] = std::array::from_fn(|a| (lower[a] + upper[a]) * 0.5);
        let mut exact = [S::zero(); 3];
        for (&t, &w) in points.iter().zip(&weights) {
            let shape = 6. * t * (1. - t);
            let flux = q / area * shape;
            for a in 0..3 {
                let u = lower[a] * (1. - t) + upper[a] * t + full[a] * shape;
                exact[a] += density * area * flux * u * w;
            }
        }
        let old: [S; 3] =
            std::array::from_fn(|a| density * q * (mean[a] + normal[a] * q / area * 1.2));
        for a in 0..3 {
            let delta = exact[a] - old[a];
            out.increments[port][a + 1] += dt * delta;
            out.advective_momentum_flux_n[a] += delta;
        }
        let transverse: [S; 3] = std::array::from_fn(|a| full[a] - normal[a] * q / area);
        out.advective_momentum_bound_n += density * q * norm(transverse) * 1.2;
    }
    if out
        .increments
        .iter()
        .flatten()
        .chain(&out.advective_momentum_flux_n)
        .any(|v| !v.value().is_finite())
        || !out.advective_momentum_bound_n.value().is_finite()
    {
        return Err(fail("pressure profile transport overflow"));
    }
    Ok(out)
}

pub fn pressure_profile_derivative_domain<S: Scalar>(
    geometry: &[GapCellGeometry<S>],
    vertex_velocities: &[[[S; 3]; 6]],
    connections: &[GapFaceConnection],
    face_flux: &[S],
    bulk_port_velocity: &[[S; 3]],
    pressure_gradient_pa_m: &[[S; 3]],
    viscosity_pa_s: S,
) -> CaeResult<()> {
    if geometry.len() != vertex_velocities.len()
        || geometry.len() != pressure_gradient_pa_m.len()
        || connections.len() != face_flux.len()
    {
        return Err(fail("pressure profile derivative shape"));
    }
    for (c, &q) in connections.iter().zip(face_flux) {
        if c.left >= geometry.len()
            || c.left_edge >= 3
            || !q.value().is_finite()
            || c.right.is_some() == c.port.is_some()
        {
            return Err(fail("pressure profile derivative ownership"));
        }
        if c.right.is_some() {
            continue;
        }
        let p = c
            .port
            .ok_or_else(|| fail("pressure profile derivative port absent"))?;
        if p >= bulk_port_velocity.len() {
            return Err(fail("pressure profile derivative port index"));
        }
        let (full, _, _) = pressure_profile(
            &geometry[c.left],
            c.left_edge,
            q,
            pressure_gradient_pa_m[c.left],
            viscosity_pa_s,
        )?;
        if q.value() == 0. {
            let v = vertex_velocities[c.left];
            let e = c.left_edge;
            let j = (e + 1) % 3;
            for a in 0..3 {
                let outgoing =
                    (v[e][a] + v[j][a] + v[e + 3][a] + v[j + 3][a]) * 0.25 + full[a] * 1.2;
                if outgoing.value() != bulk_port_velocity[p][a].value() {
                    return Err(fail(
                        "pressure profile momentum derivative is undefined at flow reversal",
                    ));
                }
            }
        }
    }
    Ok(())
}
