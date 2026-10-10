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

use crate::gas_multiscale_flux::{
    MultiscaleGasFaceInputs, WallGrazingPilot, face_exchange_scalar_with_wall_pilot,
};
use crate::gas_transient_kinetic::{
    TransientGasBoundary, TransientGasInputs, TransientGasPacket, TransientGasPolicy,
    TransientGasScalar, validate, velocity_equilibrium_density,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct MultiscaleNetworkInputs<S = f64> {
    pub gas: TransientGasInputs<S>,
    pub force_density_n_m3: Option<Vec<[S; 3]>>,
    pub left_centroid_to_face_m: Vec<[S; 3]>,
    pub right_centroid_to_face_m: Vec<Option<[S; 3]>>,
}
#[derive(Clone)]
pub struct MultiscaleNetworkPolicy {
    pub kinetic: TransientGasPolicy,
    pub gradient_axes: [bool; 3],
    pub local_transport_cfl: f64,
    pub microscopic_linear_reconstruction: bool,
    pub quadratic_reconstruction_condition_limit: Option<f64>,
    pub wall_grazing_pilots: Option<BTreeMap<usize, WallGrazingPilot>>,
}
pub struct MultiscaleNetworkResidual<S = f64> {
    pub cells: Vec<usize>,
    pub inventory_residual_kg: Vec<S>,
    pub face_packets: Vec<(usize, TransientGasPacket<S>)>,
    pub collision_heat_bath_energy_j: Vec<S>,
    pub forcing_work_j: Vec<S>,
    pub forcing_thermal_bath_energy_j: Vec<S>,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).fold(S::zero(), |a, b| a + b)
}
fn insert_quadratic_sample<S: TransientGasScalar>(
    samples: &mut Vec<(usize, [S; 3])>,
    cell: usize,
    offset: [S; 3],
) -> CaeResult<()> {
    if let Some((_, old)) = samples.iter().find(|(i, _)| *i == cell) {
        let scale = old
            .iter()
            .chain(&offset)
            .map(|v| v.value().abs())
            .fold(0., f64::max);
        let direction_scale = old
            .iter()
            .chain(&offset)
            .map(|v| v.maximum_direction())
            .fold(0., f64::max);
        if (0..3).any(|i| {
            let d = old[i] - offset[i];
            !d.finite_first_order()
                || d.value().abs() > 128. * f64::EPSILON * scale
                || d.maximum_direction() > 128. * f64::EPSILON * direction_scale
        }) {
            return Err(fail(
                "quadratic gas stencil has inconsistent centroid paths or periodic images",
            ));
        }
    } else {
        samples.push((cell, offset));
    }
    Ok(())
}
fn invert_active<S: TransientGasScalar>(a: [[S; 3]; 3], axes: [bool; 3]) -> CaeResult<[[S; 3]; 3]> {
    let ids = (0..3).filter(|i| axes[*i]).collect::<Vec<_>>();
    let n = ids.len();
    let mut m = vec![vec![S::zero(); 2 * n]; n];
    for i in 0..n {
        for j in 0..n {
            m[i][j] = a[ids[i]][ids[j]];
        }
        m[i][n + i] = S::one();
    }
    for k in 0..n {
        let pivot = (k..n)
            .max_by(|i, j| m[*i][k].value().abs().total_cmp(&m[*j][k].value().abs()))
            .unwrap();
        if !m[pivot][k].finite_first_order() || m[pivot][k].value() == 0. {
            return Err(fail("multiscale reconstruction stencil is rank deficient"));
        }
        m.swap(k, pivot);
        let p = m[k][k];
        for j in 0..2 * n {
            m[k][j] = m[k][j] / p;
        }
        for i in 0..n {
            if i != k {
                let t = m[i][k];
                for j in 0..2 * n {
                    m[i][j] = m[i][j] - t * m[k][j];
                }
            }
        }
    }
    let mut out = [[S::zero(); 3]; 3];
    for i in 0..n {
        for j in 0..n {
            out[ids[i]][ids[j]] = m[i][n + j];
        }
    }
    if out.iter().flatten().any(|v| !v.finite_first_order()) {
        return Err(fail("multiscale reconstruction inverse is not finite"));
    }
    Ok(out)
}
#[derive(Clone, Debug)]
pub struct MultiscaleNetworkTopology {
    pub incident_faces: Vec<Vec<usize>>,
    pub neighbor_cells: Vec<Vec<usize>>,
    pub residual_support_cells: Vec<Vec<usize>>,
}
impl MultiscaleNetworkTopology {
    pub fn new<S>(a: &MultiscaleNetworkInputs<S>) -> CaeResult<Self> {
        Self::with_radius(a, 2)
    }
    pub fn new_with_policy<S>(
        a: &MultiscaleNetworkInputs<S>,
        policy: &MultiscaleNetworkPolicy,
    ) -> CaeResult<Self> {
        Self::with_radius(
            a,
            if policy.quadratic_reconstruction_condition_limit.is_some() {
                3
            } else {
                2
            },
        )
    }
    fn with_radius<S>(a: &MultiscaleNetworkInputs<S>, radius: usize) -> CaeResult<Self> {
        let n = a.gas.volume_m3.len();
        let mut incident = vec![vec![]; n];
        let mut neighbors = vec![BTreeSet::new(); n];
        for (i, f) in a.gas.faces.iter().enumerate() {
            if f.left >= n {
                return Err(fail("multiscale face cell is out of range"));
            }
            incident[f.left].push(i);
            if let TransientGasBoundary::Interior { right } = f.boundary {
                if right >= n || right == f.left {
                    return Err(fail("invalid multiscale interior face"));
                }
                incident[right].push(i);
                neighbors[f.left].insert(right);
                neighbors[right].insert(f.left);
            }
        }
        let mut support = vec![vec![]; n];
        for c in 0..n {
            let mut p = BTreeSet::from([c]);
            for _ in 0..radius {
                let current = p.iter().copied().collect::<Vec<_>>();
                for j in current {
                    p.extend(neighbors[j].iter().copied());
                }
            }
            support[c] = p.into_iter().collect();
        }
        Ok(Self {
            incident_faces: incident,
            neighbor_cells: neighbors
                .into_iter()
                .map(|s| s.into_iter().collect())
                .collect(),
            residual_support_cells: support,
        })
    }
}
pub fn conservative_collision_increment_scalar<S: TransientGasScalar>(
    inventory: &[S],
    equilibrium_inventory: &[S],
    grid: &crate::gas_transient_kinetic::TransientVelocityGrid,
    rt: S,
    collision_step_ratio: S,
) -> CaeResult<Vec<S>> {
    let nv = grid.velocity_m_s.len();
    if inventory.len() != nv
        || equilibrium_inventory.len() != nv
        || !rt.finite_first_order()
        || rt.value() <= 0.
        || !collision_step_ratio.finite_first_order()
        || collision_step_ratio.value() < 0.
        || inventory
            .iter()
            .chain(equilibrium_inventory)
            .any(|v| !v.finite_first_order() || v.value() < 0.)
    {
        return Err(fail("invalid conservative BGK collision data"));
    }
    let mass = equilibrium_inventory
        .iter()
        .copied()
        .fold(S::zero(), |a, b| a + b);
    if mass.value() == 0. {
        if inventory
            .iter()
            .chain(equilibrium_inventory)
            .any(|v| v.value() != 0. || v.has_direction())
        {
            return Err(fail("vacuum BGK collision changes its active stratum"));
        }
        return Ok(vec![S::zero(); nv]);
    }
    let u = std::array::from_fn::<_, 3, _>(|i| {
        equilibrium_inventory
            .iter()
            .zip(&grid.velocity_m_s)
            .map(|(q, c)| *q * c[i])
            .fold(S::zero(), |a, b| a + b)
            / mass
    });
    let root = rt.sqrt();
    let centered = grid
        .velocity_m_s
        .iter()
        .map(|c| std::array::from_fn::<_, 3, _>(|i| (S::from_f64(c[i]) - u[i]) / root))
        .collect::<Vec<_>>();
    let cov = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            equilibrium_inventory
                .iter()
                .zip(&centered)
                .map(|(g, c)| *g * c[i] * c[j] / mass)
                .fold(S::zero(), |a, b| a + b)
        })
    });
    let inv = invert_active(cov, [true; 3])?;
    let mut increment = equilibrium_inventory
        .iter()
        .zip(inventory)
        .map(|(g, q)| *g - *q)
        .collect::<Vec<_>>();
    for _ in 0..2 {
        let defect = increment.iter().copied().fold(S::zero(), |a, b| a + b);
        let centered_defect = std::array::from_fn(|i| {
            increment
                .iter()
                .zip(&centered)
                .map(|(q, c)| *q * c[i] / mass)
                .fold(S::zero(), |a, b| a + b)
        });
        let coeff = std::array::from_fn::<_, 3, _>(|i| dot(inv[i], centered_defect));
        for k in 0..nv {
            increment[k] =
                increment[k] - equilibrium_inventory[k] * (defect / mass + dot(coeff, centered[k]));
        }
    }
    for q in &mut increment {
        *q = *q * collision_step_ratio;
    }
    if increment.iter().any(|v| !v.finite_first_order()) {
        return Err(fail("conservative BGK increment is not finite"));
    }
    Ok(increment)
}
pub fn dependency_cells<S>(a: &MultiscaleNetworkInputs<S>, cell: usize) -> CaeResult<Vec<usize>> {
    MultiscaleNetworkTopology::new(a)?
        .residual_support_cells
        .get(cell)
        .cloned()
        .ok_or_else(|| fail("multiscale dependency cell is out of range"))
}
pub fn dependency_cells_with_policy<S>(
    a: &MultiscaleNetworkInputs<S>,
    cell: usize,
    policy: &MultiscaleNetworkPolicy,
) -> CaeResult<Vec<usize>> {
    MultiscaleNetworkTopology::new_with_policy(a, policy)?
        .residual_support_cells
        .get(cell)
        .cloned()
        .ok_or_else(|| fail("multiscale dependency cell is out of range"))
}
pub fn residual_cells_scalar<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    state: impl Fn(usize, usize) -> S,
    output_cells: &[usize],
    policy: &MultiscaleNetworkPolicy,
) -> CaeResult<MultiscaleNetworkResidual<S>> {
    validate(&a.gas, &policy.kinetic)?;
    let topology = MultiscaleNetworkTopology::new_with_policy(a, policy)?;
    residual_cells_prepared_scalar(a, &topology, state, output_cells, policy)
}
pub(crate) fn residual_cells_prepared_scalar<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    topology: &MultiscaleNetworkTopology,
    state: impl Fn(usize, usize) -> S,
    output_cells: &[usize],
    policy: &MultiscaleNetworkPolicy,
) -> CaeResult<MultiscaleNetworkResidual<S>> {
    let gas = &a.gas;
    if policy
        .quadratic_reconstruction_condition_limit
        .is_some_and(|v| !v.is_finite() || v < 1.)
    {
        return Err(fail("invalid quadratic gas reconstruction condition limit"));
    }
    let n = gas.volume_m3.len();
    let nv = gas.grid.velocity_m_s.len();
    if let Some(pilots) = &policy.wall_grazing_pilots {
        for (fi, pilot) in pilots {
            let face = gas
                .faces
                .get(*fi)
                .ok_or_else(|| fail("wall grazing pilot face index is invalid"))?;
            pilot.validate(face)?;
        }
    }

    if topology.incident_faces.len() != n
        || topology.neighbor_cells.len() != n
        || topology.residual_support_cells.len() != n
    {
        return Err(fail("multiscale prepared topology shape mismatch"));
    }
    if !policy.local_transport_cfl.is_finite()
        || policy.local_transport_cfl <= 0.
        || policy.local_transport_cfl > 1.
        || a.left_centroid_to_face_m.len() != gas.faces.len()
        || a.right_centroid_to_face_m.len() != gas.faces.len()
        || a.left_centroid_to_face_m
            .iter()
            .flatten()
            .chain(a.right_centroid_to_face_m.iter().flatten().flatten())
            .any(|v| !v.finite_first_order())
    {
        return Err(fail(
            "invalid multiscale network reconstruction or local CFL policy",
        ));
    }
    let output = output_cells.iter().copied().collect::<BTreeSet<_>>();
    if output.len() != output_cells.len() || output.iter().any(|i| *i >= n) {
        return Err(fail("invalid multiscale residual output cells"));
    }
    let mut needed = BTreeSet::new();
    for c in &output {
        needed.extend(topology.residual_support_cells[*c].iter().copied());
    }
    let mut density = BTreeMap::new();
    for c in &needed {
        let q = (0..nv).map(|k| state(*c, k)).collect::<Vec<_>>();
        if q.iter().any(|v| !v.finite_first_order() || v.value() < 0.) {
            return Err(fail(
                "multiscale current inventory must be finite and nonnegative",
            ));
        }
        if gas.volume_m3[*c].value() == 0. {
            if q.iter().any(|v| v.value() != 0. || v.has_direction()) {
                return Err(fail("closed multiscale cells require fixed zero inventory"));
            }
            density.insert(*c, vec![S::zero(); nv]);
        } else {
            density.insert(
                *c,
                q.iter().map(|q| *q / gas.volume_m3[*c]).collect::<Vec<_>>(),
            );
        }
    }
    if a.force_density_n_m3
        .as_ref()
        .is_some_and(|f| f.len() != n || f.iter().flatten().any(|v| !v.finite_first_order()))
    {
        return Err(fail("invalid multiscale gas force density"));
    }
    let mut quadratic = BTreeMap::new();
    let mut gradients = BTreeMap::new();
    let mut local_time = BTreeMap::new();
    let cmax = gas
        .grid
        .velocity_m_s
        .iter()
        .map(|c| c.iter().map(|v| v * v).sum::<f64>().sqrt())
        .fold(0., f64::max);
    let mut prepare = output.clone();
    let mut active_faces = BTreeSet::new();
    for c in &output {
        prepare.extend(topology.neighbor_cells[*c].iter().copied());
        active_faces.extend(topology.incident_faces[*c].iter().copied());
    }
    for c in &prepare {
        let mut mat = [[S::zero(); 3]; 3];
        let mut rhs = vec![[S::zero(); 3]; nv];
        let mut speed_area = S::zero();
        for fi in topology.incident_faces[*c].iter().copied() {
            let f = &gas.faces[fi];
            let is_left = f.left == *c;
            if f.area_m2.value() == 0. {
                continue;
            }
            speed_area = speed_area
                + f.area_m2
                    * (S::from_f64(cmax)
                        + (gas.gas_rt_m2_s2 + dot(f.ale_velocity_m_s, f.ale_velocity_m_s)).sqrt());
            let sample = match f.boundary {
                TransientGasBoundary::Interior { right } => {
                    let r = a.right_centroid_to_face_m[fi].ok_or_else(|| {
                        fail("interior multiscale face needs both centroid offsets")
                    })?;
                    let d = std::array::from_fn(|i| a.left_centroid_to_face_m[fi][i] - r[i]);
                    if is_left {
                        Some((d, &density[&right]))
                    } else {
                        Some((d.map(|v| -v), &density[&f.left]))
                    }
                }
                TransientGasBoundary::Port { port } => Some((
                    a.left_centroid_to_face_m[fi].map(|v| v * 2.),
                    &gas.port_density_kg_m3[port],
                )),
                TransientGasBoundary::DiffuseWall { .. } => None,
            };
            if let Some((mut d, sample)) = sample {
                for i in 0..3 {
                    if !policy.gradient_axes[i] {
                        d[i] = S::zero();
                    }
                }
                let ds = dot(d, d);
                if ds.value() == 0. {
                    continue;
                }
                for i in 0..3 {
                    for j in 0..3 {
                        mat[i][j] = mat[i][j] + d[i] * d[j] / ds;
                    }
                    for k in 0..nv {
                        rhs[k][i] = rhs[k][i] + d[i] * (sample[k] - density[c][k]) / ds;
                    }
                }
            }
        }
        let old_mass = gas.previous_inventory_kg[*c * nv..(*c + 1) * nv]
            .iter()
            .copied()
            .fold(S::zero(), |a, b| a + b);
        if old_mass.value() == 0.
            && gas.previous_inventory_kg[*c * nv..(*c + 1) * nv]
                .iter()
                .any(|q| q.has_direction())
        {
            return Err(fail(
                "old-vacuum reconstruction derivative changes a history stratum",
            ));
        }
        let inv = if gas.volume_m3[*c].value() > 0.
            && old_mass.value() > 0.
            && policy.quadratic_reconstruction_condition_limit.is_none()
        {
            invert_active(mat, policy.gradient_axes)?
        } else {
            [[S::zero(); 3]; 3]
        };
        if let Some(limit) = policy.quadratic_reconstruction_condition_limit {
            let mut samples = Vec::<(usize, [S; 3])>::new();
            let neighbor = |c: usize, fi: usize| -> CaeResult<(usize, [S; 3])> {
                let f = &gas.faces[fi];
                match f.boundary {
                    TransientGasBoundary::Interior { right } => {
                        let r = a.right_centroid_to_face_m[fi]
                            .ok_or_else(|| fail("quadratic neighbor lacks centroid offset"))?;
                        let d = std::array::from_fn(|i| a.left_centroid_to_face_m[fi][i] - r[i]);
                        if f.left == c {
                            Ok((right, d))
                        } else {
                            Ok((f.left, d.map(|v| -v)))
                        }
                    }
                    _ => Err(fail("quadratic neighbor is not interior")),
                }
            };
            for fi in &topology.incident_faces[*c] {
                if gas.faces[*fi].area_m2.value() > 0.
                    && matches!(
                        gas.faces[*fi].boundary,
                        TransientGasBoundary::Interior { .. }
                    )
                {
                    let (j, d) = neighbor(*c, *fi)?;
                    insert_quadratic_sample(&mut samples, j, d)?;
                    for fj in &topology.incident_faces[j] {
                        if gas.faces[*fj].area_m2.value() > 0.
                            && matches!(
                                gas.faces[*fj].boundary,
                                TransientGasBoundary::Interior { .. }
                            )
                        {
                            let (k, dk) = neighbor(j, *fj)?;
                            if k != *c {
                                let distance = std::array::from_fn(|i| d[i] + dk[i]);
                                insert_quadratic_sample(&mut samples, k, distance)?;
                            }
                        }
                    }
                }
            }
            let (g, h) = if old_mass.value() > 0. {
                let samples = samples
                    .iter()
                    .map(
                        |(j, d)| crate::gas_quadratic_reconstruction::QuadraticSample {
                            offset_m: *d,
                            density_difference_kg_m3: (0..nv)
                                .map(|k| density[j][k] - density[c][k])
                                .collect(),
                        },
                    )
                    .collect::<Vec<_>>();
                let q = crate::gas_quadratic_reconstruction::reconstruct_scalar(
                    &samples,
                    policy.gradient_axes,
                    nv,
                    limit,
                )?;
                (q.gradient_kg_m4, q.quadratic_kg_m5)
            } else {
                (vec![[S::zero(); 3]; nv], vec![[[S::zero(); 3]; 3]; nv])
            };
            gradients.insert(*c, g);
            quadratic.insert(*c, h);
        } else {
            gradients.insert(
                *c,
                if old_mass.value() == 0. {
                    vec![[S::zero(); 3]; nv]
                } else {
                    rhs.iter()
                        .map(|r| std::array::from_fn(|i| dot(inv[i], *r)))
                        .collect::<Vec<_>>()
                },
            );
            quadratic.insert(*c, vec![[[S::zero(); 3]; 3]; nv]);
        }
        let cap = gas.volume_m3[*c] * policy.local_transport_cfl;
        let t = if cap.value() == 0. {
            S::zero()
        } else {
            gas.step_s * cap / (cap + gas.step_s * speed_area)
        };
        local_time.insert(*c, t);
    }
    let mut residual = BTreeMap::new();
    let mut heat = BTreeMap::new();
    let mut forcing_work = BTreeMap::new();
    let mut forcing_heat = BTreeMap::new();
    for c in &output {
        let q = (0..nv).map(|k| state(*c, k)).collect::<Vec<_>>();
        let mut r = (0..nv)
            .map(|k| q[k] - gas.previous_inventory_kg[*c * nv + k])
            .collect::<Vec<_>>();
        let rho = density[c].iter().copied().fold(S::zero(), |a, b| a + b);
        if gas.volume_m3[*c].value() > 0. && rho.value() > 0. {
            let rho = density[c].iter().copied().fold(S::zero(), |a, b| a + b);
            let j = std::array::from_fn::<_, 3, _>(|i| {
                density[c]
                    .iter()
                    .zip(&gas.grid.velocity_m_s)
                    .map(|(q, v)| *q * v[i])
                    .fold(S::zero(), |a, b| a + b)
            });
            let g = if rho.value() == 0. {
                velocity_equilibrium_density(
                    &gas.grid,
                    rho,
                    [S::zero(); 3],
                    gas.gas_rt_m2_s2,
                    &policy.kinetic,
                )?
            } else {
                velocity_equilibrium_density(
                    &gas.grid,
                    rho,
                    j.map(|j| j / rho),
                    gas.gas_rt_m2_s2,
                    &policy.kinetic,
                )?
            };
            let nu = rho * gas.gas_rt_m2_s2 / gas.viscosity_pa_s;
            let mut bath = S::zero();
            let equilibrium_inventory =
                g.iter().map(|v| *v * gas.volume_m3[*c]).collect::<Vec<_>>();
            let collisions = conservative_collision_increment_scalar(
                &q,
                &equilibrium_inventory,
                &gas.grid,
                gas.gas_rt_m2_s2,
                gas.step_s * nu,
            )?;
            for k in 0..nv {
                let collision = collisions[k];
                r[k] = r[k] - collision;
                let e = 0.5 * gas.grid.velocity_m_s[k].iter().map(|v| v * v).sum::<f64>();
                bath = bath - collision * e;
            }
            heat.insert(*c, bath);
        } else {
            heat.insert(*c, S::zero());
        }
        let force = a
            .force_density_n_m3
            .as_ref()
            .map(|f| f[*c])
            .unwrap_or([S::zero(); 3]);
        if force.iter().any(|v| v.value() != 0. || v.has_direction()) {
            let forced = crate::gas_weak_force::entropy_tangent_force(
                &gas.grid,
                &q,
                gas.volume_m3[*c],
                gas.gas_rt_m2_s2,
                gas.step_s,
                force,
                &policy.kinetic,
            )?;
            for k in 0..nv {
                r[k] = r[k] - forced.inventory_increment_kg[k];
            }
            forcing_work.insert(*c, forced.mechanical_work_j);
            forcing_heat.insert(*c, forced.heat_bath_energy_j);
        } else {
            forcing_work.insert(*c, S::zero());
            forcing_heat.insert(*c, S::zero());
        }
        residual.insert(*c, r);
    }
    let mut packets = Vec::new();
    for fi in active_faces {
        let f = &gas.faces[fi];
        let right = if let TransientGasBoundary::Interior { right } = f.boundary {
            Some(right)
        } else {
            None
        };
        if !output.contains(&f.left) && !right.is_some_and(|r| output.contains(&r)) {
            continue;
        }
        if f.area_m2.value() == 0. {
            continue;
        }
        let mut dt = local_time[&f.left];
        if let Some(r) = right {
            let tr = local_time[&r];
            if dt.value() > 0. && tr.value() > 0. {
                dt = dt * tr / (dt + tr);
            } else {
                return Err(fail("closed multiscale cell has an open interior face"));
            }
        }
        if dt.value() <= 0. {
            return Err(fail("closed multiscale cell has an open boundary face"));
        }
        let mut f0 = Vec::with_capacity(nv);
        let mut gx = Vec::with_capacity(nv);
        for k in 0..nv {
            let xi = std::array::from_fn(|i| {
                S::from_f64(gas.grid.velocity_m_s[k][i]) - f.ale_velocity_m_s[i]
            });
            let s = dot(xi, f.normal);
            if s.value() == 0.
                && s.has_direction()
                && !matches!(f.boundary, TransientGasBoundary::DiffuseWall { .. })
            {
                return Err(fail(
                    "multiscale molecular upwind branch changes at grazing incidence",
                ));
            }
            if s.value() == 0. && matches!(f.boundary, TransientGasBoundary::DiffuseWall { .. }) {
                if let Some(pilot) = policy.wall_grazing_pilots.as_ref().and_then(|p| p.get(&fi)) {
                    pilot.side(s, gas.grid.velocity_m_s[k])?;
                } else if s.has_direction() {
                    return Err(fail(
                        "multiscale wall grazing direction requires an immutable pilot",
                    ));
                }
            }
            let (base, grad, offset, hess) = if s.value() >= 0. {
                (
                    density[&f.left][k],
                    gradients[&f.left][k],
                    a.left_centroid_to_face_m[fi],
                    quadratic[&f.left][k],
                )
            } else {
                match f.boundary {
                    TransientGasBoundary::Interior { right } => (
                        density[&right][k],
                        gradients[&right][k],
                        a.right_centroid_to_face_m[fi].unwrap(),
                        quadratic[&right][k],
                    ),
                    TransientGasBoundary::Port { port } => (
                        gas.port_density_kg_m3[port][k],
                        [S::zero(); 3],
                        [S::zero(); 3],
                        [[S::zero(); 3]; 3],
                    ),
                    TransientGasBoundary::DiffuseWall { .. } => (
                        density[&f.left][k],
                        gradients[&f.left][k],
                        a.left_centroid_to_face_m[fi],
                        quadratic[&f.left][k],
                    ),
                }
            };
            f0.push(if policy.microscopic_linear_reconstruction {
                base + dot(grad, offset)
                    + (0..3)
                        .map(|i| {
                            (0..3)
                                .map(|j| hess[i][j] * offset[i] * offset[j])
                                .fold(S::zero(), |a, b| a + b)
                        })
                        .fold(S::zero(), |a, b| a + b)
            } else {
                base
            });
            let mut face_gradient = grad;
            for i in 0..3 {
                face_gradient[i] = face_gradient[i]
                    + (0..3)
                        .map(|j| hess[i][j] * offset[j] * 2.)
                        .fold(S::zero(), |a, b| a + b);
            }
            gx.push(face_gradient);
        }
        let mass_grad =
            std::array::from_fn(|i| gx.iter().map(|g| g[i]).fold(S::zero(), |a, b| a + b));
        let momentum_grad = std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                gx.iter()
                    .zip(&gas.grid.velocity_m_s)
                    .map(|(g, c)| g[i] * c[j])
                    .fold(S::zero(), |a, b| a + b)
            })
        });
        let face_a = MultiscaleGasFaceInputs {
            initial_density_kg_m3: f0,
            initial_density_gradient_kg_m4: if policy.microscopic_linear_reconstruction {
                gx
            } else {
                vec![[S::zero(); 3]; nv]
            },
            equilibrium_mass_gradient_kg_m4: mass_grad,
            equilibrium_momentum_gradient_kg_m3_s: momentum_grad,
            ale_velocity_m_s: f.ale_velocity_m_s,
            gas_rt_m2_s2: gas.gas_rt_m2_s2,
            viscosity_pa_s: gas.viscosity_pa_s,
            step_s: dt,
        };
        let average_density = crate::gas_multiscale_flux::transport_average_scalar(
            &face_a,
            &gas.grid,
            &policy.kinetic,
        )?;
        let exchange = face_exchange_scalar_with_wall_pilot(
            f,
            &average_density,
            &gas.grid,
            gas.gas_rt_m2_s2,
            gas.step_s,
            &policy.kinetic,
            policy.wall_grazing_pilots.as_ref().and_then(|p| p.get(&fi)),
        )?;
        if let Some(r) = residual.get_mut(&f.left) {
            for k in 0..nv {
                r[k] = r[k] + exchange.outward_inventory_kg[k];
            }
        }
        if let Some(right) = right {
            if let Some(r) = residual.get_mut(&right) {
                for k in 0..nv {
                    r[k] = r[k] - exchange.outward_inventory_kg[k];
                }
            }
        }
        packets.push((fi, exchange.packet));
    }
    let r = output_cells
        .iter()
        .flat_map(|c| residual[c].iter().copied())
        .collect::<Vec<_>>();
    if r.iter()
        .chain(heat.values())
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("multiscale network residual is not finite"));
    }
    Ok(MultiscaleNetworkResidual {
        cells: output_cells.to_vec(),
        inventory_residual_kg: r,
        face_packets: packets,
        collision_heat_bath_energy_j: output_cells.iter().map(|c| heat[c]).collect(),
        forcing_work_j: output_cells.iter().map(|c| forcing_work[c]).collect(),
        forcing_thermal_bath_energy_j: output_cells.iter().map(|c| forcing_heat[c]).collect(),
    })
}
pub fn physical_residual_scalar<S: TransientGasScalar>(
    a: &MultiscaleNetworkInputs<S>,
    q: &[S],
    policy: &MultiscaleNetworkPolicy,
) -> CaeResult<MultiscaleNetworkResidual<S>> {
    let n = a.gas.volume_m3.len();
    let nv = a.gas.grid.velocity_m_s.len();
    if q.len()
        != n.checked_mul(nv)
            .ok_or_else(|| fail("multiscale state size overflow"))?
    {
        return Err(fail("multiscale inventory shape mismatch"));
    }
    residual_cells_scalar(a, |c, k| q[c * nv + k], &(0..n).collect::<Vec<_>>(), policy)
}
