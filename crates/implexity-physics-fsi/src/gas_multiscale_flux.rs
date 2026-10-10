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
    TransientGasPolicy, TransientGasScalar, TransientVelocityGrid, velocity_equilibrium_density,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
pub struct MultiscaleGasFaceInputs<S = f64> {
    pub initial_density_kg_m3: Vec<S>,
    pub initial_density_gradient_kg_m4: Vec<[S; 3]>,
    pub equilibrium_mass_gradient_kg_m4: [S; 3],
    pub equilibrium_momentum_gradient_kg_m3_s: [[S; 3]; 3],
    pub ale_velocity_m_s: [S; 3],
    pub gas_rt_m2_s2: S,
    pub viscosity_pa_s: S,
    pub step_s: S,
}
pub struct MultiscaleGasFaceSolution<S = f64> {
    pub average_density_kg_m3: Vec<S>,
    pub equilibrium_density_kg_m3: Vec<S>,
    pub equilibrium_density_gradient_kg_m4: Vec<[S; 3]>,
    pub equilibrium_density_time_derivative_kg_m3_s: Vec<S>,
    pub collision_step_ratio: S,
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).fold(S::zero(), |a, b| a + b)
}
fn inverse<S: TransientGasScalar>(m: [[S; 3]; 3]) -> CaeResult<[[S; 3]; 3]> {
    let co = std::array::from_fn::<_, 3, _>(|i| {
        std::array::from_fn::<_, 3, _>(|j| {
            let a = (i + 1) % 3;
            let b = (i + 2) % 3;
            let c = (j + 1) % 3;
            let d = (j + 2) % 3;
            m[a][c] * m[b][d] - m[a][d] * m[b][c]
        })
    });
    let det = (0..3)
        .map(|j| m[0][j] * co[0][j])
        .fold(S::zero(), |a, b| a + b);
    if !det.finite_first_order() || det.value() <= 0. {
        return Err(fail("multiscale gas covariance is not invertible"));
    }
    Ok(std::array::from_fn(|i| {
        std::array::from_fn(|j| co[j][i] / det)
    }))
}
pub(crate) fn coefficients<S: TransientGasScalar>(d: S, dt: S) -> [S; 4] {
    if d.value() < 0.001 {
        let d2 = d * d;
        let d3 = d2 * d;
        let d4 = d3 * d;
        let d5 = d4 * d;
        [
            S::one() - d * 0.5 + d2 / 6. - d3 / 24. + d4 / 120. - d5 / 720.,
            -dt * (S::from_f64(0.5) - d / 3. + d2 / 8. - d3 / 30. + d4 / 144. - d5 / 840.),
            -dt * (d / 6. - d2 / 12. + d3 / 40. - d4 / 180. + d5 / 1008.),
            dt * (d / 6. - d2 / 24. + d3 / 120. - d4 / 720. + d5 / 5040.),
        ]
    } else {
        let e = (-d).exp();
        let one = -(-d).exp_m1();
        [
            one / d,
            -dt * (one - d * e) / (d * d),
            dt * (one * 2. / (d * d) - (S::one() + e) / d),
            dt * (S::from_f64(0.5) - S::one() / d + one / (d * d)),
        ]
    }
}
fn validate_face_inputs<S: TransientGasScalar>(
    a: &MultiscaleGasFaceInputs<S>,
    grid: &TransientVelocityGrid,
) -> CaeResult<usize> {
    let nv = grid.velocity_m_s.len();
    if a.initial_density_kg_m3.len() != nv
        || a.initial_density_gradient_kg_m4.len() != nv
        || a.initial_density_kg_m3
            .iter()
            .any(|v| !v.finite_first_order() || v.value() < 0.)
        || a.initial_density_gradient_kg_m4
            .iter()
            .flatten()
            .chain(&a.equilibrium_mass_gradient_kg_m4)
            .chain(a.equilibrium_momentum_gradient_kg_m3_s.iter().flatten())
            .chain(&a.ale_velocity_m_s)
            .any(|v| !v.finite_first_order())
        || [a.gas_rt_m2_s2, a.viscosity_pa_s, a.step_s]
            .iter()
            .any(|v| !v.finite_first_order() || v.value() <= 0.)
    {
        return Err(fail("invalid multiscale gas face data"));
    }
    Ok(nv)
}

pub fn transport_average_scalar<S: TransientGasScalar>(
    a: &MultiscaleGasFaceInputs<S>,
    grid: &TransientVelocityGrid,
    policy: &TransientGasPolicy,
) -> CaeResult<Vec<S>> {
    let nv = validate_face_inputs(a, grid)?;
    let rho = a
        .initial_density_kg_m3
        .iter()
        .copied()
        .fold(S::zero(), |a, b| a + b);
    if !rho.finite_first_order() {
        return Err(fail("nonfinite transport face density"));
    }
    if rho.value() > 0. {
        return Ok(evaluate_scalar(a, grid, policy)?.average_density_kg_m3);
    }
    if a.initial_density_gradient_kg_m4
        .iter()
        .flatten()
        .chain(&a.equilibrium_mass_gradient_kg_m4)
        .chain(a.equilibrium_momentum_gradient_kg_m3_s.iter().flatten())
        .any(|v| v.value() != 0.)
    {
        return Err(fail("vacuum transport has a nonzero primal reconstruction"));
    }
    let average = (0..nv)
        .map(|k| {
            let xi = std::array::from_fn(|j| {
                S::from_f64(grid.velocity_m_s[k][j]) - a.ale_velocity_m_s[j]
            });
            a.initial_density_kg_m3[k]
                - a.step_s * 0.5 * dot(a.initial_density_gradient_kg_m4[k], xi)
        })
        .collect::<Vec<_>>();
    if average.iter().any(|v| !v.finite_first_order()) {
        return Err(fail("nonfinite vacuum transport linearization"));
    }
    Ok(average)
}

pub fn evaluate_scalar<S: TransientGasScalar>(
    a: &MultiscaleGasFaceInputs<S>,
    grid: &TransientVelocityGrid,
    policy: &TransientGasPolicy,
) -> CaeResult<MultiscaleGasFaceSolution<S>> {
    let nv = validate_face_inputs(a, grid)?;
    let rho = a
        .initial_density_kg_m3
        .iter()
        .copied()
        .fold(S::zero(), |a, b| a + b);
    if rho.value() == 0. {
        if a.initial_density_kg_m3.iter().any(|v| v.has_direction())
            || a.initial_density_gradient_kg_m4
                .iter()
                .flatten()
                .chain(&a.equilibrium_mass_gradient_kg_m4)
                .chain(a.equilibrium_momentum_gradient_kg_m3_s.iter().flatten())
                .any(|v| v.value() != 0. || v.has_direction())
        {
            return Err(fail(
                "vacuum multiscale derivative or reconstruction changes its active stratum",
            ));
        }
        return Ok(MultiscaleGasFaceSolution {
            average_density_kg_m3: vec![S::zero(); nv],
            equilibrium_density_kg_m3: vec![S::zero(); nv],
            equilibrium_density_gradient_kg_m4: vec![[S::zero(); 3]; nv],
            equilibrium_density_time_derivative_kg_m3_s: vec![S::zero(); nv],
            collision_step_ratio: S::zero(),
        });
    }
    let momentum = std::array::from_fn::<_, 3, _>(|j| {
        a.initial_density_kg_m3
            .iter()
            .zip(&grid.velocity_m_s)
            .map(|(q, c)| *q * c[j])
            .fold(S::zero(), |a, b| a + b)
    });
    let u = momentum.map(|j| j / rho);
    let g = velocity_equilibrium_density(grid, rho, u, a.gas_rt_m2_s2, policy)?;
    let root = a.gas_rt_m2_s2.sqrt();
    let centered = grid
        .velocity_m_s
        .iter()
        .map(|c| std::array::from_fn::<_, 3, _>(|j| (S::from_f64(c[j]) - u[j]) / root))
        .collect::<Vec<_>>();
    let cov = std::array::from_fn::<_, 3, _>(|i| {
        std::array::from_fn::<_, 3, _>(|j| {
            g.iter()
                .zip(&centered)
                .map(|(g, c)| *g / rho * c[i] * c[j])
                .fold(S::zero(), |a, b| a + b)
        })
    });
    let inv = inverse(cov)?;
    let gradient = std::array::from_fn::<_, 3, _>(|d| {
        let du = std::array::from_fn::<_, 3, _>(|j| {
            (a.equilibrium_momentum_gradient_kg_m3_s[d][j]
                - u[j] * a.equilibrium_mass_gradient_kg_m4[d])
                / (rho * root)
        });
        std::array::from_fn::<_, 3, _>(|i| dot(inv[i], du))
    });
    let gx = g
        .iter()
        .zip(&centered)
        .map(|(g, c)| {
            std::array::from_fn::<_, 3, _>(|d| {
                *g * (a.equilibrium_mass_gradient_kg_m4[d] / rho + dot(gradient[d], *c))
            })
        })
        .collect::<Vec<_>>();
    let xi = grid
        .velocity_m_s
        .iter()
        .map(|c| std::array::from_fn::<_, 3, _>(|j| S::from_f64(c[j]) - a.ale_velocity_m_s[j]))
        .collect::<Vec<_>>();
    let cg = gx
        .iter()
        .zip(&xi)
        .map(|(gx, c)| dot(*gx, *c))
        .collect::<Vec<_>>();
    let mt = -cg.iter().copied().fold(S::zero(), |a, b| a + b);
    let jt = std::array::from_fn::<_, 3, _>(|j| {
        -cg.iter()
            .zip(&grid.velocity_m_s)
            .map(|(q, c)| *q * c[j])
            .fold(S::zero(), |a, b| a + b)
    });
    let du = std::array::from_fn::<_, 3, _>(|j| (jt[j] - u[j] * mt) / (rho * root));
    let lambda_t = std::array::from_fn::<_, 3, _>(|i| dot(inv[i], du));
    let gt = g
        .iter()
        .zip(&centered)
        .map(|(g, c)| *g * (mt / rho + dot(lambda_t, *c)))
        .collect::<Vec<_>>();
    let delta = a.step_s * rho * a.gas_rt_m2_s2 / a.viscosity_pa_s;
    if !delta.finite_first_order()
        || gx
            .iter()
            .flatten()
            .chain(&gt)
            .any(|v| !v.finite_first_order())
    {
        return Err(fail(
            "multiscale gas coefficient or equilibrium derivative is not finite",
        ));
    }
    let coeff = coefficients(delta, a.step_s);
    let avg = (0..nv)
        .map(|k| {
            coeff[0] * a.initial_density_kg_m3[k]
                + (S::one() - coeff[0]) * g[k]
                + coeff[1] * dot(a.initial_density_gradient_kg_m4[k], xi[k])
                + coeff[2] * cg[k]
                + coeff[3] * gt[k]
        })
        .collect::<Vec<_>>();
    if avg
        .iter()
        .any(|v| !v.finite_first_order() || v.value() < 0.)
    {
        return Err(fail(
            "multiscale gas face reconstruction requires time or spatial refinement",
        ));
    }
    Ok(MultiscaleGasFaceSolution {
        average_density_kg_m3: avg,
        equilibrium_density_kg_m3: g,
        equilibrium_density_gradient_kg_m4: gx,
        equilibrium_density_time_derivative_kg_m3_s: gt,
        collision_step_ratio: delta,
    })
}
pub fn evaluate(
    a: &MultiscaleGasFaceInputs,
    grid: &TransientVelocityGrid,
    policy: &TransientGasPolicy,
) -> CaeResult<MultiscaleGasFaceSolution> {
    evaluate_scalar(a, grid, policy)
}

pub struct MultiscalePopulationSolution<S = f64> {
    pub inventory_kg: Vec<S>,
    pub cell_moments: Vec<crate::gas_transient_kinetic::TransientGasMoments<S>>,
    pub collision_heat_bath_energy_j: Vec<S>,
}
pub fn conservative_update_scalar<S: TransientGasScalar>(
    a: &crate::gas_transient_kinetic::TransientGasInputs<S>,
    outward_inventory_kg: &[S],
    policy: &TransientGasPolicy,
) -> CaeResult<MultiscalePopulationSolution<S>> {
    let nv = crate::gas_transient_kinetic::validate(a, policy)?;
    if outward_inventory_kg.len() != a.previous_inventory_kg.len()
        || outward_inventory_kg.iter().any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid multiscale gas transport inventory"));
    }
    let mut inventory = Vec::with_capacity(outward_inventory_kg.len());
    let mut moments = Vec::new();
    let mut heat = Vec::new();
    for i in 0..a.volume_m3.len() {
        let old = &a.previous_inventory_kg[i * nv..(i + 1) * nv];
        let transported = old
            .iter()
            .zip(&outward_inventory_kg[i * nv..(i + 1) * nv])
            .map(|(q, f)| *q - *f)
            .collect::<Vec<_>>();
        let m = crate::gas_transient_kinetic::moments(&transported, &a.grid);
        let previous = crate::gas_transient_kinetic::moments(old, &a.grid);
        let v = a.volume_m3[i];
        let ov = a.previous_volume_m3[i];
        if v.value() == 0. {
            if transported
                .iter()
                .any(|q| q.value() != 0. || q.has_direction())
            {
                return Err(fail(
                    "closing multiscale gas cell contains trapped inventory",
                ));
            }
            inventory.extend(vec![S::zero(); nv]);
            moments.push(m);
            heat.push(S::zero());
            continue;
        }
        if !m.mass_kg.finite_first_order()
            || m.mass_kg.value() < 0.
            || m.mass_kg.value() == 0.
                && transported
                    .iter()
                    .any(|q| q.value() != 0. || q.has_direction())
        {
            return Err(fail(
                "multiscale transported moments are not physically realizable",
            ));
        }
        let eqnew = if m.mass_kg.value() == 0. {
            vec![S::zero(); nv]
        } else {
            velocity_equilibrium_density(
                &a.grid,
                m.mass_kg / v,
                m.momentum_kg_m_s.map(|j| j / m.mass_kg),
                a.gas_rt_m2_s2,
                policy,
            )?
            .into_iter()
            .map(|q| q * v)
            .collect::<Vec<_>>()
        };
        let eqold = if previous.mass_kg.value() == 0. {
            vec![S::zero(); nv]
        } else {
            velocity_equilibrium_density(
                &a.grid,
                previous.mass_kg / ov,
                previous.momentum_kg_m_s.map(|j| j / previous.mass_kg),
                a.gas_rt_m2_s2,
                policy,
            )?
            .into_iter()
            .map(|q| q * ov)
            .collect::<Vec<_>>()
        };
        let delta = a.step_s * (previous.mass_kg + m.mass_kg) * a.gas_rt_m2_s2
            / ((ov + v) * a.viscosity_pa_s);
        if !delta.finite_first_order() || delta.value() < 0. {
            return Err(fail("multiscale collision frequency is not finite"));
        }
        let decay = (-delta).exp();
        let average = coefficients(delta, a.step_s)[0];
        let out = (0..nv)
            .map(|k| {
                decay * old[k]
                    + average * (transported[k] - old[k])
                    + (average - decay) * eqold[k]
                    + (S::one() - average) * eqnew[k]
            })
            .collect::<Vec<_>>();
        if out
            .iter()
            .any(|q| !q.finite_first_order() || q.value() < 0.)
        {
            return Err(fail(
                "multiscale gas collision update requires time or spatial refinement",
            ));
        }
        let after = crate::gas_transient_kinetic::moments(&out, &a.grid);
        let ht = m.kinetic_energy_j - after.kinetic_energy_j;
        if !ht.finite_first_order()
            || [
                after.mass_kg,
                after.momentum_kg_m_s[0],
                after.momentum_kg_m_s[1],
                after.momentum_kg_m_s[2],
                after.kinetic_energy_j,
            ]
            .iter()
            .any(|v| !v.finite_first_order())
        {
            return Err(fail("multiscale gas moment or heat ledger is not finite"));
        }
        heat.push(ht);
        moments.push(after);
        inventory.extend(out);
    }
    Ok(MultiscalePopulationSolution {
        inventory_kg: inventory,
        cell_moments: moments,
        collision_heat_bath_energy_j: heat,
    })
}

pub struct MultiscaleFaceExchange<S = f64> {
    pub average_density_kg_m3: Vec<S>,
    pub outward_inventory_kg: Vec<S>,
    pub packet: crate::gas_transient_kinetic::TransientGasPacket<S>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallGrazingDerivativeMode {
    Directional,
    SelectedJacobian,
}
#[derive(Clone, Debug)]
pub struct WallGrazingPilot {
    pub derivative_mode: WallGrazingDerivativeMode,
    pub reference_normal: [f64; 3],
    pub reference_ale_velocity_m_s: [f64; 3],
    pub normal_direction: [f64; 3],
    pub ale_velocity_direction_m_s: [f64; 3],
}
impl WallGrazingPilot {
    pub fn validate<S: TransientGasScalar>(
        &self,
        face: &crate::gas_transient_kinetic::TransientGasFace<S>,
    ) -> CaeResult<()> {
        if !matches!(
            face.boundary,
            crate::gas_transient_kinetic::TransientGasBoundary::DiffuseWall { .. }
        ) || self
            .reference_normal
            .iter()
            .chain(&self.reference_ale_velocity_m_s)
            .chain(&self.normal_direction)
            .chain(&self.ale_velocity_direction_m_s)
            .any(|v| !v.is_finite())
            || (0..3).any(|j| {
                face.normal[j].value() != self.reference_normal[j]
                    || face.ale_velocity_m_s[j].value() != self.reference_ale_velocity_m_s[j]
            })
        {
            return Err(fail("wall grazing pilot does not match the immutable face"));
        }
        Ok(())
    }
    pub fn side<S: TransientGasScalar>(&self, speed: S, velocity: [f64; 3]) -> CaeResult<i8> {
        if speed.value() != 0. {
            return Ok(if speed.value() > 0. { 1 } else { -1 });
        }
        let ds = (0..3)
            .map(|j| {
                (velocity[j] - self.reference_ale_velocity_m_s[j]) * self.normal_direction[j]
                    - self.ale_velocity_direction_m_s[j] * self.reference_normal[j]
            })
            .sum::<f64>();
        if !ds.is_finite() {
            return Err(fail("nonfinite wall grazing pilot speed"));
        }
        let side = if ds > 0. {
            1
        } else if ds < 0. {
            -1
        } else {
            0
        };
        if side == 0 && speed.has_direction() {
            return Err(fail(
                "wall grazing pilot leaves an active derivative unselected",
            ));
        }
        if self.derivative_mode == WallGrazingDerivativeMode::Directional {
            if let Some(directions) = speed.first_order_directions() {
                if directions
                    .iter()
                    .any(|d| !d.is_finite() || *d != 0. && (side == 0 || d.signum() != ds.signum()))
                {
                    return Err(fail(
                        "wall grazing derivative is outside the selected direction cone",
                    ));
                }
            } else if speed.has_direction() {
                return Err(fail(
                    "wall grazing reverse requires an explicit selected-Jacobian mode",
                ));
            }
        }
        Ok(side)
    }
    pub fn validate_directional_domain<S: TransientGasScalar>(
        &self,
        face: &crate::gas_transient_kinetic::TransientGasFace<S>,
        grid: &TransientVelocityGrid,
    ) -> CaeResult<()> {
        self.validate(face)?;
        let mut directional = self.clone();
        directional.derivative_mode = WallGrazingDerivativeMode::Directional;
        for c in &grid.velocity_m_s {
            let speed = dot(
                std::array::from_fn(|j| S::from_f64(c[j]) - face.ale_velocity_m_s[j]),
                face.normal,
            );
            directional.side(speed, *c)?;
        }
        Ok(())
    }
}
pub fn face_exchange_scalar<S: TransientGasScalar>(
    face: &crate::gas_transient_kinetic::TransientGasFace<S>,
    average_density_kg_m3: &[S],
    grid: &TransientVelocityGrid,
    rt: S,
    step_s: S,
    policy: &TransientGasPolicy,
) -> CaeResult<MultiscaleFaceExchange<S>> {
    face_exchange_scalar_with_wall_pilot(
        face,
        average_density_kg_m3,
        grid,
        rt,
        step_s,
        policy,
        None,
    )
}
pub fn face_exchange_scalar_with_wall_pilot<S: TransientGasScalar>(
    face: &crate::gas_transient_kinetic::TransientGasFace<S>,
    average_density_kg_m3: &[S],
    grid: &TransientVelocityGrid,
    rt: S,
    step_s: S,
    policy: &TransientGasPolicy,
    wall_pilot: Option<&WallGrazingPilot>,
) -> CaeResult<MultiscaleFaceExchange<S>> {
    if let Some(pilot) = wall_pilot {
        pilot.validate(face)?;
    }
    use crate::gas_transient_kinetic::{TransientGasBoundary, TransientGasPacket};
    let nv = grid.velocity_m_s.len();
    if nv < 4
        || grid.quadrature_weight.len() != nv
        || grid.velocity_m_s.iter().flatten().any(|v| !v.is_finite())
        || grid
            .quadrature_weight
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.)
        || !policy.normal_tolerance.is_finite()
        || policy.normal_tolerance <= 0.
        || (dot(face.normal, face.normal) - S::one()).maximum_direction()
            > policy.normal_tolerance
                * (1.
                    + face
                        .normal
                        .iter()
                        .map(|v| v.maximum_direction())
                        .fold(0., f64::max))
        || average_density_kg_m3.len() != nv
        || average_density_kg_m3
            .iter()
            .any(|q| !q.finite_first_order() || q.value() < 0.)
        || !face.area_m2.finite_first_order()
        || face.area_m2.value() < 0.
        || face
            .normal
            .iter()
            .chain(&face.ale_velocity_m_s)
            .chain(&face.wall_velocity_m_s)
            .any(|v| !v.finite_first_order())
        || (dot(face.normal, face.normal).value() - 1.).abs() > policy.normal_tolerance
        || !step_s.finite_first_order()
        || step_s.value() <= 0.
        || !rt.finite_first_order()
        || rt.value() <= 0.
    {
        return Err(fail("invalid multiscale molecular face exchange"));
    }
    let mut packet = TransientGasPacket {
        mass_kg: S::zero(),
        momentum_kg_m_s: [S::zero(); 3],
        kinetic_energy_j: S::zero(),
        wall_work_j: S::zero(),
        wall_heat_j: S::zero(),
    };
    if face.area_m2.value() == 0. {
        if face.area_m2.has_direction() {
            return Err(fail(
                "multiscale face aperture derivative changes a sealed stratum",
            ));
        }
        return Ok(MultiscaleFaceExchange {
            average_density_kg_m3: vec![S::zero(); nv],
            outward_inventory_kg: vec![S::zero(); nv],
            packet,
        });
    }
    let speed = grid
        .velocity_m_s
        .iter()
        .map(|c| {
            dot(
                std::array::from_fn(|j| S::from_f64(c[j]) - face.ale_velocity_m_s[j]),
                face.normal,
            )
        })
        .collect::<Vec<_>>();
    if speed.iter().any(|s| {
        !s.finite_first_order() || wall_pilot.is_none() && s.value() == 0. && s.has_direction()
    }) {
        return Err(fail(
            "multiscale grazing velocity changes a transport stratum",
        ));
    }
    let sides = speed
        .iter()
        .zip(&grid.velocity_m_s)
        .map(|(s, c)| {
            if let Some(pilot) = wall_pilot {
                pilot.side(*s, *c)
            } else {
                Ok(if s.value() > 0. {
                    1
                } else if s.value() < 0. {
                    -1
                } else {
                    0
                })
            }
        })
        .collect::<CaeResult<Vec<_>>>()?;
    let mut density = average_density_kg_m3.to_vec();
    if matches!(face.boundary, TransientGasBoundary::DiffuseWall { .. }) {
        if (0..3).any(|j| {
            (face.wall_velocity_m_s[j] - face.ale_velocity_m_s[j]).value() != 0.
                || (face.wall_velocity_m_s[j] - face.ale_velocity_m_s[j]).has_direction()
        }) {
            return Err(fail("multiscale wall and material face velocity differ"));
        }
        let wall =
            velocity_equilibrium_density(grid, S::one(), face.wall_velocity_m_s, rt, policy)?;
        let den = wall
            .iter()
            .zip(&speed)
            .zip(&sides)
            .filter(|(_, side)| **side < 0)
            .map(|((w, s), _)| -*w * *s)
            .fold(S::zero(), |a, b| a + b);
        if !den.finite_first_order() || den.value() <= 0. || !speed.iter().any(|s| s.value() > 0.) {
            return Err(fail("multiscale wall lacks resolved molecular half spaces"));
        }
        let out = density
            .iter()
            .zip(&speed)
            .zip(&sides)
            .filter(|(_, side)| **side > 0)
            .map(|((q, s), _)| *q * *s)
            .fold(S::zero(), |a, b| a + b);
        for k in 0..nv {
            if sides[k] < 0 {
                density[k] = wall[k] * out / den;
            }
        }
    }
    let flux = density
        .iter()
        .zip(&speed)
        .map(|(q, s)| step_s * face.area_m2 * *q * *s)
        .collect::<Vec<_>>();
    for (q, c) in flux.iter().zip(&grid.velocity_m_s) {
        packet.mass_kg = packet.mass_kg + *q;
        for j in 0..3 {
            packet.momentum_kg_m_s[j] = packet.momentum_kg_m_s[j] + *q * c[j];
        }
        packet.kinetic_energy_j =
            packet.kinetic_energy_j + *q * (0.5 * c.iter().map(|v| v * v).sum::<f64>());
    }
    if matches!(face.boundary, TransientGasBoundary::DiffuseWall { .. }) {
        packet.wall_work_j = dot(packet.momentum_kg_m_s, face.wall_velocity_m_s);
        packet.wall_heat_j = packet.kinetic_energy_j - packet.wall_work_j;
    }
    if density
        .iter()
        .chain(&flux)
        .copied()
        .chain([
            packet.mass_kg,
            packet.momentum_kg_m_s[0],
            packet.momentum_kg_m_s[1],
            packet.momentum_kg_m_s[2],
            packet.kinetic_energy_j,
            packet.wall_work_j,
            packet.wall_heat_j,
        ])
        .any(|v| !v.finite_first_order())
    {
        return Err(fail(
            "multiscale molecular exchange or ledger is not finite",
        ));
    }
    Ok(MultiscaleFaceExchange {
        average_density_kg_m3: density,
        outward_inventory_kg: flux,
        packet,
    })
}
