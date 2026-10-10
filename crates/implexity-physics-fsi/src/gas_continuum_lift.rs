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
#[derive(Clone)]
pub struct ContinuumGasInputs<S = f64> {
    pub density_kg_m3: S,
    pub momentum_kg_m2_s: [S; 3],
    pub density_gradient_kg_m4: [S; 3],
    pub momentum_gradient_kg_m3_s: [[S; 3]; 3],
    pub ale_velocity_m_s: [S; 3],
    pub gas_rt_m2_s2: S,
    pub viscosity_pa_s: S,
}
#[derive(Clone)]
pub struct ContinuumGasPolicy {
    pub kinetic: TransientGasPolicy,
    pub characteristic_length_m: f64,
    pub forcing_timescale_s: f64,
    pub maximum_knudsen_number: f64,
    pub maximum_collision_time_ratio: f64,
    pub maximum_relative_correction: f64,
}
pub struct ContinuumGasLift<S = f64> {
    pub density_kg_m3: Vec<S>,
    pub equilibrium_density_kg_m3: Vec<S>,
    pub stress_pa: [[S; 3]; 3],
    pub collision_time_s: S,
    pub transport_relaxation_time_s: S,
    pub knudsen_number: S,
    pub collision_time_ratio: S,
    pub relative_correction_squared: S,
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
pub fn lift_scalar<S: TransientGasScalar>(
    a: &ContinuumGasInputs<S>,
    grid: &TransientVelocityGrid,
    p: &ContinuumGasPolicy,
) -> CaeResult<ContinuumGasLift<S>> {
    lift_with_transport_viscosity_scalar(a, grid, p, a.viscosity_pa_s)
}
pub fn lift_with_transport_viscosity_scalar<S: TransientGasScalar>(
    a: &ContinuumGasInputs<S>,
    grid: &TransientVelocityGrid,
    p: &ContinuumGasPolicy,
    transport_viscosity_pa_s: S,
) -> CaeResult<ContinuumGasLift<S>> {
    if [
        a.density_kg_m3,
        a.gas_rt_m2_s2,
        a.viscosity_pa_s,
        transport_viscosity_pa_s,
    ]
    .iter()
    .any(|x| !x.finite_first_order() || x.value() <= 0.)
        || a.momentum_kg_m2_s
            .iter()
            .chain(&a.density_gradient_kg_m4)
            .chain(a.momentum_gradient_kg_m3_s.iter().flatten())
            .chain(&a.ale_velocity_m_s)
            .any(|x| !x.finite_first_order())
        || [
            p.characteristic_length_m,
            p.forcing_timescale_s,
            p.maximum_knudsen_number,
            p.maximum_collision_time_ratio,
            p.maximum_relative_correction,
        ]
        .iter()
        .any(|x| !x.is_finite() || *x <= 0.)
    {
        return Err(fail("invalid continuum gas lift data"));
    }
    let rho = a.density_kg_m3;
    let tau = a.viscosity_pa_s / (rho * a.gas_rt_m2_s2);
    let transport_tau = transport_viscosity_pa_s / (rho * a.gas_rt_m2_s2);
    let kn = tau * a.gas_rt_m2_s2.sqrt() / p.characteristic_length_m;
    let time_ratio = tau / p.forcing_timescale_s;
    if kn.value() > p.maximum_knudsen_number || time_ratio.value() > p.maximum_collision_time_ratio
    {
        return Err(fail(
            "continuum gas lift requires kinetic representation for the declared length or time scale",
        ));
    }
    let policy = &p.kinetic;
    let momentum = a.momentum_kg_m2_s;
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
            (a.momentum_gradient_kg_m3_s[d][j] - u[j] * a.density_gradient_kg_m4[d]) / (rho * root)
        });
        std::array::from_fn::<_, 3, _>(|i| dot(inv[i], du))
    });
    let gx = g
        .iter()
        .zip(&centered)
        .map(|(g, c)| {
            std::array::from_fn::<_, 3, _>(|d| {
                *g * (a.density_gradient_kg_m4[d] / rho + dot(gradient[d], *c))
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

    let mut correction = cg
        .iter()
        .zip(&gt)
        .map(|(c, t)| -transport_tau * (*c + *t))
        .collect::<Vec<_>>();
    for _ in 0..2 {
        let defect = correction.iter().copied().fold(S::zero(), |a, b| a + b);
        let centered_defect = std::array::from_fn(|i| {
            correction
                .iter()
                .zip(&centered)
                .map(|(q, c)| *q * c[i] / rho)
                .fold(S::zero(), |a, b| a + b)
        });
        let coeff = std::array::from_fn::<_, 3, _>(|i| dot(inv[i], centered_defect));
        for k in 0..g.len() {
            correction[k] = correction[k] - g[k] * (defect / rho + dot(coeff, centered[k]));
        }
    }
    let relative = correction
        .iter()
        .zip(&g)
        .map(|(c, g)| *c * *c / *g)
        .fold(S::zero(), |a, b| a + b)
        / rho;
    if !relative.finite_first_order()
        || relative.value() > p.maximum_relative_correction * p.maximum_relative_correction
    {
        return Err(fail(
            "continuum gas lift exceeds the declared first-order correction limit",
        ));
    }
    let f = g
        .iter()
        .zip(&correction)
        .map(|(g, c)| *g + *c)
        .collect::<Vec<_>>();
    if f.iter().any(|x| !x.finite_first_order() || x.value() < 0.) {
        return Err(fail(
            "continuum gas lift requires kinetic representation for nonpositive populations",
        ));
    }
    let stress = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            f.iter()
                .zip(&grid.velocity_m_s)
                .map(|(q, c)| *q * (S::from_f64(c[i]) - u[i]) * (S::from_f64(c[j]) - u[j]))
                .fold(S::zero(), |a, b| a + b)
        })
    });
    if stress
        .iter()
        .flatten()
        .chain([tau, transport_tau, kn, time_ratio, relative].iter())
        .any(|x| !x.finite_first_order())
    {
        return Err(fail("continuum gas lift diagnostics are not finite"));
    }
    Ok(ContinuumGasLift {
        density_kg_m3: f,
        equilibrium_density_kg_m3: g,
        stress_pa: stress,
        collision_time_s: tau,
        transport_relaxation_time_s: transport_tau,
        knudsen_number: kn,
        collision_time_ratio: time_ratio,
        relative_correction_squared: relative,
    })
}
