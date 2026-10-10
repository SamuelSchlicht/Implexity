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

use crate::gas_transient_kinetic::{
    TransientGasBoundary, TransientGasFace, TransientGasPacket, TransientGasScalar,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuumWalePolicy {
    pub model: implexity_physics_lbm::moving::wale::Wale,
    pub spacing_m: f64,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuumFvPolicy {
    pub wale: Option<ContinuumWalePolicy>,
    pub gradient_axes: [bool; 3],
    pub maximum_gradient_condition_number: f64,
    pub characteristic_length_m: Vec<f64>,
    pub forcing_timescale_s: Vec<f64>,
    pub maximum_knudsen_number: f64,
    pub maximum_collision_time_ratio: f64,
    pub maximum_mach_number: f64,
    pub metric_absolute_tolerance_m2: f64,
    pub geometric_absolute_tolerance_m3: f64,
    pub geometric_relative_tolerance: f64,
}
#[derive(Clone)]
pub struct ContinuumFvInputs<S = f64> {
    pub continuum_cells: Vec<bool>,
    pub current_inventory: Vec<[S; 4]>,
    pub previous_inventory: Vec<[S; 4]>,
    pub volume_m3: Vec<S>,
    pub previous_volume_m3: Vec<S>,
    pub faces: Vec<TransientGasFace<S>>,
    pub left_centroid_to_face_m: Vec<[S; 3]>,
    pub right_centroid_to_face_m: Vec<Option<[S; 3]>>,
    pub shared_face_packets: Vec<Option<TransientGasPacket<S>>>,
    pub gas_rt_m2_s2: S,
    pub viscosity_pa_s: S,
    pub step_s: S,
}
pub struct ContinuumFvResult<S = f64> {
    pub inventory_residual: Vec<[S; 4]>,
    pub density_gradients: Vec<[S; 3]>,
    pub momentum_gradients: Vec<[[S; 3]; 3]>,
    pub transport_viscosity_pa_s: Vec<S>,
    pub face_packets: Vec<TransientGasPacket<S>>,
    pub maximum_knudsen_number: f64,
    pub maximum_collision_time_ratio: f64,
    pub equilibrium_energy_balance_residual_j: S,
    pub ce_energy_correction_j: Vec<S>,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).fold(S::zero(), |v, i| v + a[i] * b[i])
}
pub fn evaluate_scalar<S: TransientGasScalar>(
    a: &ContinuumFvInputs<S>,
    p: &ContinuumFvPolicy,
) -> CaeResult<ContinuumFvResult<S>> {
    evaluate_inner(a, p, None, false)
}
pub(crate) fn evaluate_cells_scalar<S: TransientGasScalar>(
    a: &ContinuumFvInputs<S>,
    p: &ContinuumFvPolicy,
    cells: &[usize],
) -> CaeResult<ContinuumFvResult<S>> {
    evaluate_inner(a, p, Some(cells), false)
}
pub(crate) fn reconstruct_cells_scalar<S: TransientGasScalar>(
    a: &ContinuumFvInputs<S>,
    p: &ContinuumFvPolicy,
    cells: &[usize],
) -> CaeResult<ContinuumFvResult<S>> {
    evaluate_inner(a, p, Some(cells), true)
}
fn evaluate_inner<S: TransientGasScalar>(
    a: &ContinuumFvInputs<S>,
    p: &ContinuumFvPolicy,
    cells: Option<&[usize]>,
    reconstruction_only: bool,
) -> CaeResult<ContinuumFvResult<S>> {
    let opaque_geometry_direction = a
        .volume_m3
        .iter()
        .chain(&a.previous_volume_m3)
        .chain(a.faces.iter().flat_map(|f| {
            f.normal
                .iter()
                .chain(&f.ale_velocity_m_s)
                .chain(&f.wall_velocity_m_s)
                .chain(std::iter::once(&f.area_m2))
        }))
        .chain(a.left_centroid_to_face_m.iter().flatten())
        .chain(a.right_centroid_to_face_m.iter().flatten().flatten())
        .any(|v| v.geometry_first_order_directions().is_none() && v.has_direction());
    if opaque_geometry_direction {
        return Err(fail(
            "continuum reverse geometry requires a separately qualified metric tangent owner",
        ));
    }
    if let Some(wale) = &p.wale {
        wale.model.validate()?;
        if !wale.spacing_m.is_finite() || wale.spacing_m <= 0.0 {
            return Err(fail("continuum WALE spacing must be finite and positive"));
        }
    }
    let n = a.volume_m3.len();
    let nf = a.faces.len();
    if n == 0 || a.continuum_cells.len() != n {
        return Err(fail("continuum ownership shape mismatch"));
    }
    let selected = cells
        .map(|v| v.iter().copied().collect::<std::collections::BTreeSet<_>>())
        .unwrap_or_else(|| (0..n).filter(|&i| a.continuum_cells[i]).collect());
    if selected.iter().any(|&i| i >= n || !a.continuum_cells[i]) {
        return Err(fail("invalid continuum output cells"));
    }
    let mut gradient_cells = selected.clone();
    if !reconstruction_only {
        for f in &a.faces {
            if let TransientGasBoundary::Interior { right } = f.boundary {
                if f.left < n
                    && right < n
                    && a.continuum_cells[f.left]
                    && a.continuum_cells[right]
                    && (selected.contains(&f.left) || selected.contains(&right))
                {
                    gradient_cells.insert(f.left);
                    gradient_cells.insert(right);
                }
            }
        }
    }

    if n == 0
        || [
            a.continuum_cells.len(),
            a.current_inventory.len(),
            a.previous_inventory.len(),
            a.previous_volume_m3.len(),
            p.characteristic_length_m.len(),
            p.forcing_timescale_s.len(),
        ]
        .iter()
        .any(|&v| v != n)
        || [
            a.left_centroid_to_face_m.len(),
            a.right_centroid_to_face_m.len(),
            a.shared_face_packets.len(),
        ]
        .iter()
        .any(|&v| v != nf)
        || [a.gas_rt_m2_s2, a.viscosity_pa_s, a.step_s]
            .iter()
            .any(|x| !x.finite_first_order() || x.value() <= 0.0)
        || [
            p.maximum_gradient_condition_number,
            p.maximum_knudsen_number,
            p.maximum_collision_time_ratio,
            p.maximum_mach_number,
            p.geometric_relative_tolerance,
        ]
        .iter()
        .any(|x| !x.is_finite() || *x <= 0.0)
        || [
            p.metric_absolute_tolerance_m2,
            p.geometric_absolute_tolerance_m3,
        ]
        .iter()
        .any(|x| !x.is_finite() || *x < 0.0)
    {
        return Err(fail("invalid continuum finite volume configuration"));
    }
    let mut density = vec![[S::zero(); 4]; n];
    let mut maxkn = 0.0_f64;
    let mut maxtime = 0.0_f64;
    for i in 0..n {
        if [a.volume_m3[i], a.previous_volume_m3[i]]
            .iter()
            .any(|v| !v.finite_first_order() || v.value() < 0.0)
            || a.current_inventory[i]
                .iter()
                .chain(&a.previous_inventory[i])
                .any(|v| !v.finite_first_order())
            || a.current_inventory[i][0].value() < 0.0
            || a.previous_inventory[i][0].value() < 0.0
        {
            return Err(fail("invalid continuum cell inventory or capacity"));
        }
        if a.volume_m3[i].value() > 0.0 {
            density[i] = a.current_inventory[i].map(|v| v / a.volume_m3[i]);
        } else if a.current_inventory[i]
            .iter()
            .any(|v| v.value() != 0.0 || v.maximum_direction() != 0.0)
        {
            return Err(fail(
                "zero capacity cannot hold gas inventory or an ordinary inventory direction",
            ));
        }
        if a.continuum_cells[i] {
            let rho = density[i][0];
            if rho.value() <= 0.0 || a.previous_volume_m3[i].value() <= 0.0 {
                return Err(fail(
                    "continuum cells require positive current gas and an existing capacity",
                ));
            }
            let l = p.characteristic_length_m[i];
            let t = p.forcing_timescale_s[i];
            if !l.is_finite() || l <= 0.0 || !t.is_finite() || t <= 0.0 {
                return Err(fail(
                    "invalid declared continuum length or forcing time scale",
                ));
            }
            let tau = a.viscosity_pa_s / (rho * a.gas_rt_m2_s2);
            let kn = tau * a.gas_rt_m2_s2.sqrt() / l;
            let ratio = tau / t;
            maxkn = maxkn.max(kn.value());
            maxtime = maxtime.max(ratio.value());
            let u = std::array::from_fn(|j| density[i][j + 1] / rho);
            let mach_squared = dot(u, u) / a.gas_rt_m2_s2;
            if kn.value() > p.maximum_knudsen_number
                || ratio.value() > p.maximum_collision_time_ratio
                || mach_squared.value() > p.maximum_mach_number * p.maximum_mach_number
            {
                return Err(fail(
                    "continuum cell requires kinetic representation at the declared physical scales",
                ));
            }
        }
    }
    let mut neighbors = vec![Vec::<(usize, [S; 3])>::new(); n];
    let mut metric = vec![[S::zero(); 3]; n];
    let mut swept = vec![S::zero(); n];
    for (fi, f) in a.faces.iter().enumerate() {
        if f.left >= n
            || !f.area_m2.finite_first_order()
            || f.area_m2.value() < 0.0
            || f.normal
                .iter()
                .chain(&f.ale_velocity_m_s)
                .chain(&f.wall_velocity_m_s)
                .chain(&a.left_centroid_to_face_m[fi])
                .any(|v| !v.finite_first_order())
        {
            return Err(fail("invalid continuum face geometry"));
        }
        let normal_defect = dot(f.normal, f.normal) - S::one();
        if f.area_m2.value() > 0.0
            && (normal_defect.value().abs() > 1e-10
                || geometry_direction_norm(normal_defect)? > 1e-10)
        {
            return Err(fail("continuum face normal is not a unit vector"));
        }
        let av = f.normal.map(|v| v * f.area_m2);
        for j in 0..3 {
            metric[f.left][j] = metric[f.left][j] + av[j];
        }
        let g = dot(av, f.ale_velocity_m_s) * a.step_s;
        swept[f.left] = swept[f.left] + g;
        if let TransientGasBoundary::Interior { right } = f.boundary {
            if right >= n || right == f.left {
                return Err(fail("invalid continuum face adjacency"));
            }
            for j in 0..3 {
                metric[right][j] = metric[right][j] - av[j];
            }
            swept[right] = swept[right] - g;
            let r = a.right_centroid_to_face_m[fi]
                .ok_or_else(|| fail("interior continuum face requires both centroid offsets"))?;
            if r.iter().any(|v| !v.finite_first_order()) {
                return Err(fail("nonfinite continuum centroid offset"));
            }
            let dx = std::array::from_fn(|j| a.left_centroid_to_face_m[fi][j] - r[j]);
            if f.area_m2.value() > 0.0 {
                neighbors[f.left].push((right, dx));
                neighbors[right].push((f.left, dx.map(|v| -v)));
            }
        }
    }
    for i in 0..n {
        if gradient_cells.contains(&i) {
            for v in metric[i] {
                if v.value().abs() > p.metric_absolute_tolerance_m2
                    || geometry_direction_norm(v)? > p.metric_absolute_tolerance_m2
                {
                    return Err(fail("continuum face vectors do not close the cell metric"));
                }
            }
            let defect = a.volume_m3[i] - a.previous_volume_m3[i] - swept[i];
            let tol = p.geometric_absolute_tolerance_m3
                + p.geometric_relative_tolerance
                    * a.volume_m3[i].value().max(a.previous_volume_m3[i].value());
            if defect.value().abs() > tol || geometry_direction_norm(defect)? > tol {
                return Err(fail(
                    "continuum moving cell violates geometric conservation",
                ));
            }
        }
    }
    let transport = crate::gas_continuous_transport::reconstruct(
        &density,
        &neighbors,
        &gradient_cells,
        p.gradient_axes,
        p.maximum_gradient_condition_number,
        a.viscosity_pa_s,
        a.step_s,
        p.wale.as_ref().map(|w| (&w.model, w.spacing_m)),
    )?;
    let grad = transport.gradients;
    let transport_viscosity = transport.transport_viscosity_pa_s;
    let mut residual = vec![[S::zero(); 4]; n];
    for i in 0..n {
        if selected.contains(&i) && !reconstruction_only {
            residual[i] =
                std::array::from_fn(|j| a.current_inventory[i][j] - a.previous_inventory[i][j]);
        }
    }
    let mut packets = Vec::with_capacity(nf);
    for (fi, f) in a.faces.iter().enumerate() {
        let touches = selected.contains(&f.left)
            || matches!(f.boundary,TransientGasBoundary::Interior{right} if selected.contains(&right));
        if reconstruction_only || !touches {
            packets.push(TransientGasPacket {
                mass_kg: S::zero(),
                momentum_kg_m_s: [S::zero(); 3],
                kinetic_energy_j: S::zero(),
                wall_work_j: S::zero(),
                wall_heat_j: S::zero(),
            });
            continue;
        }
        let right = match f.boundary {
            TransientGasBoundary::Interior { right } => Some(right),
            _ => None,
        };
        let analytic = right.is_some_and(|r| a.continuum_cells[f.left] && a.continuum_cells[r]);
        let packet = if analytic && f.area_m2.value() > 0.0 {
            if a.shared_face_packets[fi].is_some() {
                return Err(fail(
                    "continuum interior face has duplicate molecular ownership",
                ));
            }
            let r = right.unwrap();
            let l = f.left;
            let right_offset = a.right_centroid_to_face_m[fi]
                .ok_or_else(|| fail("missing continuum right centroid"))?;
            let face_left = std::array::from_fn::<_, 4, _>(|q| {
                density[l][q] + dot(grad[l][q], a.left_centroid_to_face_m[fi])
            });
            let face_right =
                std::array::from_fn::<_, 4, _>(|q| density[r][q] + dot(grad[r][q], right_offset));
            if face_left[0].value() <= 0.0 || face_right[0].value() <= 0.0 {
                return Err(fail(
                    "continuum face reconstruction requires kinetic representation or spatial refinement",
                ));
            }
            let ul = std::array::from_fn(|j| face_left[j + 1] / face_left[0]);
            let ur = std::array::from_fn(|j| face_right[j + 1] / face_right[0]);
            let cell_ul: [S; 3] = std::array::from_fn(|j| density[l][j + 1] / density[l][0]);
            let cell_ur: [S; 3] = std::array::from_fn(|j| density[r][j + 1] / density[r][0]);
            let vl = dot(
                std::array::from_fn(|j| ul[j] - f.ale_velocity_m_s[j]),
                f.normal,
            );
            let vr = dot(
                std::array::from_fn(|j| ur[j] - f.ale_velocity_m_s[j]),
                f.normal,
            );
            let speed = a.gas_rt_m2_s2.sqrt() + (vl * vl + vr * vr + a.gas_rt_m2_s2).sqrt();
            let rho = (face_left[0] + face_right[0]) * 0.5;
            let mass = (face_left[0] * vl + face_right[0] * vr
                - speed * (face_right[0] - face_left[0]))
                * 0.5;
            let right_offset = a.right_centroid_to_face_m[fi]
                .ok_or_else(|| fail("missing continuum right centroid"))?;
            let dx = std::array::from_fn(|j| a.left_centroid_to_face_m[fi][j] - right_offset[j]);
            let dn = dot(dx, f.normal);
            if dn.value() <= 0.0 {
                return Err(fail(
                    "continuum face has nonpositive normal centroid separation",
                ));
            }
            let mut gu = [[S::zero(); 3]; 3];
            for q in 0..3 {
                for j in 0..3 {
                    gu[q][j] = ((grad[l][q + 1][j] - cell_ul[q] * grad[l][0][j]) / density[l][0]
                        + (grad[r][q + 1][j] - cell_ur[q] * grad[r][0][j]) / density[r][0])
                        * 0.5;
                }
                let discrepancy = (cell_ur[q] - cell_ul[q] - dot(gu[q], dx)) / dn;
                for j in 0..3 {
                    gu[q][j] = gu[q][j] + discrepancy * f.normal[j];
                }
            }
            let div = (0..3).fold(S::zero(), |s, j| s + gu[j][j]);
            let face_viscosity = if p.wale.is_some() {
                transport_viscosity[l] * 0.5 + transport_viscosity[r] * 0.5
            } else {
                a.viscosity_pa_s
            };
            let stress = std::array::from_fn::<_, 3, _>(|i| {
                std::array::from_fn::<_, 3, _>(|j| face_viscosity * (gu[i][j] + gu[j][i]))
            });
            let momentum = std::array::from_fn(|j| {
                (face_left[j + 1] * vl + face_right[j + 1] * vr
                    - speed * (face_right[j + 1] - face_left[j + 1]))
                    * 0.5
                    + rho * a.gas_rt_m2_s2 * f.normal[j]
                    - dot(stress[j], f.normal)
            });
            let scale = a.step_s * f.area_m2;
            let uf = std::array::from_fn(|j| (ul[j] + ur[j]) * 0.5);
            let stresswork = dot(std::array::from_fn(|j| dot(stress[j], f.normal)), uf);
            let el =
                face_left[0] * (dot(ul, ul) + a.gas_rt_m2_s2 * 3.0) * 0.5 - face_viscosity * div;
            let er =
                face_right[0] * (dot(ur, ur) + a.gas_rt_m2_s2 * 3.0) * 0.5 - face_viscosity * div;
            let energy = (el * vl + er * vr - speed * (er - el)) * 0.5
                + rho * a.gas_rt_m2_s2 * dot(uf, f.normal)
                - stresswork;
            TransientGasPacket {
                mass_kg: scale * mass,
                momentum_kg_m_s: momentum.map(|v| v * scale),
                kinetic_energy_j: energy * scale,
                wall_work_j: S::zero(),
                wall_heat_j: S::zero(),
            }
        } else if f.area_m2.value() == 0.0 {
            TransientGasPacket {
                mass_kg: S::zero(),
                momentum_kg_m_s: [S::zero(); 3],
                kinetic_energy_j: S::zero(),
                wall_work_j: S::zero(),
                wall_heat_j: S::zero(),
            }
        } else {
            a.shared_face_packets[fi].clone().ok_or_else(||fail("continuum boundary or representation interface requires one authentic molecular packet"))?
        };
        if [
            packet.mass_kg,
            packet.kinetic_energy_j,
            packet.wall_work_j,
            packet.wall_heat_j,
        ]
        .iter()
        .chain(&packet.momentum_kg_m_s)
        .any(|v| !v.finite_first_order())
        {
            return Err(fail("nonfinite continuum face packet"));
        }
        let v = [
            packet.mass_kg,
            packet.momentum_kg_m_s[0],
            packet.momentum_kg_m_s[1],
            packet.momentum_kg_m_s[2],
        ];
        if selected.contains(&f.left) {
            for j in 0..4 {
                residual[f.left][j] = residual[f.left][j] + v[j];
            }
        }
        if let Some(r) = right {
            if selected.contains(&r) {
                for j in 0..4 {
                    residual[r][j] = residual[r][j] - v[j];
                }
            }
        }
        packets.push(packet);
    }
    let mut energy = S::zero();
    for i in 0..n {
        if selected.contains(&i) {
            let current = a.current_inventory[i];
            let old = a.previous_inventory[i];
            if old[0].value() <= 0.0 {
                return Err(fail("continuum cells require positive previous gas mass"));
            }
            let e = |q: [S; 4]| {
                (q[1] * q[1] + q[2] * q[2] + q[3] * q[3]) / q[0] * 0.5 + q[0] * a.gas_rt_m2_s2 * 1.5
            };
            energy = energy + e(current) - e(old);
        }
    }
    for (fi, f) in a.faces.iter().enumerate() {
        let left = selected.contains(&f.left);
        let right = match f.boundary {
            TransientGasBoundary::Interior { right } => selected.contains(&right),
            _ => false,
        };
        if left != right {
            energy = energy
                + if left {
                    packets[fi].kinetic_energy_j
                } else {
                    -packets[fi].kinetic_energy_j
                };
        }
    }
    Ok(ContinuumFvResult {
        inventory_residual: residual,
        density_gradients: grad.iter().map(|g| g[0]).collect(),
        momentum_gradients: grad.iter().map(|g| [g[1], g[2], g[3]]).collect(),
        face_packets: packets,
        maximum_knudsen_number: maxkn,
        maximum_collision_time_ratio: maxtime,
        equilibrium_energy_balance_residual_j: energy,
        ce_energy_correction_j: (0..n)
            .map(|i| {
                if !a.continuum_cells[i] {
                    S::zero()
                } else {
                    let rho = density[i][0];
                    let div = (0..3).fold(S::zero(), |v, j| {
                        v + (grad[i][j + 1][j] - density[i][j + 1] / rho * grad[i][0][j]) / rho
                    });
                    -transport_viscosity[i] * div * a.volume_m3[i]
                }
            })
            .collect(),
        transport_viscosity_pa_s: transport_viscosity,
    })
}

fn geometry_direction_norm<S: TransientGasScalar>(value: S) -> CaeResult<f64> {
    if let Some(directions) = value.geometry_first_order_directions() {
        if directions.iter().any(|v| !v.is_finite()) {
            return Err(fail("continuum geometry partial is not finite"));
        }
        return Ok(directions.iter().map(|v| v.abs()).fold(0., f64::max));
    }
    if !value.has_direction() {
        Ok(0.)
    } else {
        Err(fail(
            "continuum geometry partials require a bounded local owner",
        ))
    }
}
