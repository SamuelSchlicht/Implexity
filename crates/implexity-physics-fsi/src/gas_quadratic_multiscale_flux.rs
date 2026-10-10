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

use crate::gas_multiscale_flux::coefficients;
use crate::gas_quadratic_gaussian::QuadraticGaussianBasis;
use crate::gas_quadratic_halfspace::{
    quadratic_halfspace_operator, quadratic_relative_velocity_halfspace_operator,
    QuadraticHalfspaceOperator,
};
use crate::gas_quadratic_reconstruction_admission::minimum_density;
use crate::gas_quadratic_velocity::{QuadraticEntropyPolicy, QuadraticVelocityBasis};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::gas_velocity_halfspace::HalfspaceMeasure;
use implexity_core::{CaeError, CaeResult};
use std::sync::Arc;

fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}

#[derive(Clone)]
pub struct QuadraticMultiscaleFaceInputs<S> {
    pub left_density: Vec<S>,
    pub right_density: Vec<S>,
    pub left_density_gradient: [Vec<S>; 3],
    pub right_density_gradient: [Vec<S>; 3],
    pub equilibrium_mass_gradient: [S; 3],
    pub equilibrium_momentum_gradient: [[S; 3]; 3],
    pub unit_normal: [S; 3],
    pub ale_velocity_m_s: [S; 3],
    pub gas_rt_m2_s2: S,
    pub molecular_viscosity_pa_s: S,
    pub step_s: S,
    pub maximum_operator_nonzeros: usize,
}

#[derive(Clone)]
pub struct QuadraticMultiscaleFaceSolution<S> {
    pub average_weak_flux: Vec<S>,
    pub positive_weak_flux: Vec<S>,
    pub average_moments: [S; 10],
    pub interface_density_kg_m3: S,
    pub interface_velocity_m_s: Option<[S; 3]>,
    pub equilibrium_density: Option<Vec<S>>,
    pub equilibrium_density_gradient: Option<[Vec<S>; 3]>,
    pub equilibrium_density_time_derivative: Option<Vec<S>>,
    pub collision_step_ratio: S,
    pub minimum_density_bernstein: f64,
}

fn combine<S: TransientGasScalar>(a: Vec<S>, b: Vec<S>, sign: f64) -> Vec<S> {
    a.into_iter().zip(b).map(|(x, y)| x + y * sign).collect()
}

#[derive(Clone)]
pub struct QuadraticMultiscaleFaceState<S> {
    pub left_density: Vec<S>,
    pub right_density: Vec<S>,
    pub left_density_gradient: [Vec<S>; 3],
    pub right_density_gradient: [Vec<S>; 3],
    pub equilibrium_mass_gradient: [S; 3],
    pub equilibrium_momentum_gradient: [[S; 3]; 3],
    pub gas_rt_m2_s2: S,
    pub molecular_viscosity_pa_s: S,
    pub step_s: S,
}
#[derive(Clone)]
pub struct PreparedQuadraticMultiscaleFace<S> {
    basis: Arc<QuadraticVelocityBasis>,
    gaussian: Arc<QuadraticGaussianBasis>,
    ale_velocity_m_s: [S; 3],
    unit_normal: [S; 3],
    density: [QuadraticHalfspaceOperator<S>; 2],
    speed: [QuadraticHalfspaceOperator<S>; 2],
    weighted_speed: Vec<[QuadraticHalfspaceOperator<S>; 2]>,
    weighted_density: Vec<[QuadraticHalfspaceOperator<S>; 2]>,
}
impl<S: TransientGasScalar> PreparedQuadraticMultiscaleFace<S> {
    pub fn interface_density_action(&self, left: &[S], right: &[S]) -> CaeResult<Vec<S>> {
        Ok(combine(
            self.density[0].apply(left)?,
            self.density[1].apply(right)?,
            1.,
        ))
    }

    pub(crate) fn basis(&self) -> &QuadraticVelocityBasis {
        &self.basis
    }
    pub(crate) fn gaussian(&self) -> &QuadraticGaussianBasis {
        &self.gaussian
    }
    pub(crate) fn unit_normal(&self) -> [S; 3] {
        self.unit_normal
    }
    pub(crate) fn ale_velocity(&self) -> [S; 3] {
        self.ale_velocity_m_s
    }
    pub(crate) fn incoming_speed_action(&self, coefficients: &[S]) -> CaeResult<Vec<S>> {
        self.speed[1].apply(coefficients)
    }
    pub(crate) fn outgoing_speed_action(&self, coefficients: &[S]) -> CaeResult<Vec<S>> {
        self.speed[0].apply(coefficients)
    }

    pub fn with_constant_ale_velocity(&self, ale_velocity_m_s: [S; 3]) -> CaeResult<Self> {
        if self
            .unit_normal
            .iter()
            .chain(&self.ale_velocity_m_s)
            .chain(&ale_velocity_m_s)
            .any(|v| !v.finite_first_order() || v.has_direction())
        {
            return Err(fail("operator reuse needs fixed continuous face geometry"));
        }
        let normal_speed =
            |a: &[S; 3]| (0..3).fold(0., |v, j| v + self.unit_normal[j].value() * a[j].value());
        let old_speed = normal_speed(&self.ale_velocity_m_s);
        let new_speed = normal_speed(&ale_velocity_m_s);
        if !old_speed.is_finite() || !new_speed.is_finite() || old_speed != new_speed {
            return Err(fail("continuous operator reuse changes normal ALE speed"));
        }
        let mut result = self.clone();
        result.ale_velocity_m_s = ale_velocity_m_s;
        Ok(result)
    }
    pub(crate) fn numerical_halfspace_operators(
        &self,
        side: usize,
    ) -> CaeResult<(
        &QuadraticHalfspaceOperator<S>,
        &QuadraticHalfspaceOperator<S>,
    )> {
        if side >= 2 {
            return Err(fail("continuous numerical halfspace side invalid"));
        }
        Ok((&self.density[side], &self.speed[side]))
    }
    pub fn operator_family_id(&self) -> Option<usize> {
        self.density[0].shared_constant_storage()
    }
    fn relative_action(
        &self,
        axis: usize,
        side: usize,
        speed: bool,
        coefficients: &[S],
    ) -> CaeResult<Vec<S>> {
        let (weighted, plain) = if speed {
            (&self.weighted_speed[axis][side], &self.speed[side])
        } else {
            (&self.weighted_density[axis][side], &self.density[side])
        };
        let mut result = weighted.apply(coefficients)?;
        let shift = self.ale_velocity_m_s[axis];
        if shift.value() != 0. || shift.has_direction() {
            for (r, p) in result.iter_mut().zip(plain.apply(coefficients)?) {
                *r = *r - shift * p;
            }
        }
        if result.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("relative continuous face action is nonfinite"));
        }
        Ok(result)
    }
    pub fn constant_lift<T: TransientGasScalar>(&self) -> PreparedQuadraticMultiscaleFace<T> {
        PreparedQuadraticMultiscaleFace {
            basis: self.basis.clone(),
            gaussian: self.gaussian.clone(),
            unit_normal: self.unit_normal.map(|v| T::from_f64(v.value())),
            ale_velocity_m_s: self.ale_velocity_m_s.map(|v| T::from_f64(v.value())),
            density: std::array::from_fn(|i| self.density[i].constant_lift()),
            speed: std::array::from_fn(|i| self.speed[i].constant_lift()),
            weighted_speed: self
                .weighted_speed
                .iter()
                .map(|v| std::array::from_fn(|i| v[i].constant_lift()))
                .collect(),
            weighted_density: self
                .weighted_density
                .iter()
                .map(|v| std::array::from_fn(|i| v[i].constant_lift()))
                .collect(),
        }
    }
}
pub fn prepare<S: TransientGasScalar>(
    basis: Arc<QuadraticVelocityBasis>,
    unit_normal: [S; 3],
    ale_velocity_m_s: [S; 3],
    maximum_operator_nonzeros: usize,
) -> CaeResult<PreparedQuadraticMultiscaleFace<S>> {
    let gaussian = Arc::new(QuadraticGaussianBasis::new(
        basis.clone(),
        basis.reference_speed_m_s(),
    )?);
    prepare_with_gaussian(
        gaussian,
        unit_normal,
        ale_velocity_m_s,
        maximum_operator_nonzeros,
    )
}
pub fn prepare_with_gaussian<S: TransientGasScalar>(
    gaussian: Arc<QuadraticGaussianBasis>,
    unit_normal: [S; 3],
    ale_velocity_m_s: [S; 3],
    maximum_operator_nonzeros: usize,
) -> CaeResult<PreparedQuadraticMultiscaleFace<S>> {
    let basis = gaussian.basis().clone();
    if maximum_operator_nonzeros == 0
        || unit_normal
            .iter()
            .chain(&ale_velocity_m_s)
            .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid continuous gas face geometry"));
    }
    let n2 = unit_normal.iter().fold(S::zero(), |s, x| s + *x * *x);
    if (n2.value() - 1.).abs() > 1e-10 {
        return Err(fail("continuous gas face normal must be unit length"));
    }
    let normal = unit_normal.map(|v| v / n2.sqrt());
    let offset = -(0..3).fold(S::zero(), |s, j| s + normal[j] * ale_velocity_m_s[j]);
    let nm = normal.map(|v| -v);
    let make = |measure| -> CaeResult<[QuadraticHalfspaceOperator<S>; 2]> {
        Ok([
            quadratic_halfspace_operator(
                &basis,
                normal,
                offset,
                measure,
                maximum_operator_nonzeros,
            )?,
            quadratic_halfspace_operator(&basis, nm, -offset, measure, maximum_operator_nonzeros)?,
        ])
    };
    let density = make(HalfspaceMeasure::Density)?;
    let speed = make(HalfspaceMeasure::PositiveSpeed)?;
    let mut weighted_speed = Vec::new();
    let mut weighted_density = Vec::new();
    for j in 0..3 {
        for (measure, target) in [
            (HalfspaceMeasure::PositiveSpeed, &mut weighted_speed),
            (HalfspaceMeasure::Density, &mut weighted_density),
        ] {
            target.push([
                quadratic_relative_velocity_halfspace_operator(
                    &basis,
                    normal,
                    offset,
                    measure,
                    j,
                    S::zero(),
                    maximum_operator_nonzeros,
                )?,
                quadratic_relative_velocity_halfspace_operator(
                    &basis,
                    nm,
                    -offset,
                    measure,
                    j,
                    S::zero(),
                    maximum_operator_nonzeros,
                )?,
            ]);
        }
    }
    Ok(PreparedQuadraticMultiscaleFace {
        gaussian,
        basis,
        ale_velocity_m_s,
        unit_normal: normal,
        density,
        speed,
        weighted_speed,
        weighted_density,
    })
}
pub fn evaluate<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    a: &QuadraticMultiscaleFaceInputs<S>,
    policy: &QuadraticEntropyPolicy,
) -> CaeResult<QuadraticMultiscaleFaceSolution<S>> {
    let geometry = prepare(
        Arc::new(basis.clone()),
        a.unit_normal,
        a.ale_velocity_m_s,
        a.maximum_operator_nonzeros,
    )?;
    let state = QuadraticMultiscaleFaceState {
        left_density: a.left_density.clone(),
        right_density: a.right_density.clone(),
        left_density_gradient: a.left_density_gradient.clone(),
        right_density_gradient: a.right_density_gradient.clone(),
        equilibrium_mass_gradient: a.equilibrium_mass_gradient,
        equilibrium_momentum_gradient: a.equilibrium_momentum_gradient,
        gas_rt_m2_s2: a.gas_rt_m2_s2,
        molecular_viscosity_pa_s: a.molecular_viscosity_pa_s,
        step_s: a.step_s,
    };
    evaluate_prepared(&geometry, &state, policy)
}
fn certify<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticMultiscaleFaceState<S>,
    g: &[S],
    gx: &[Vec<S>; 3],
    gt: &[S],
    cf: [S; 4],
    cache: Option<&[crate::gas_quadratic_reconstruction_admission::MinimumDensityCache; 2]>,
) -> CaeResult<f64> {
    let nv = geometry.basis.coefficient_count();
    let mut minimum = f64::INFINITY;
    for (side, (f, fx)) in [
        (&a.left_density, &a.left_density_gradient),
        (&a.right_density, &a.right_density_gradient),
    ]
    .into_iter()
    .enumerate()
    {
        let base = (0..nv)
            .map(|k| cf[0] * f[k] + (S::one() - cf[0]) * g[k] + cf[3] * gt[k])
            .collect::<Vec<_>>();
        let gradient = std::array::from_fn::<_, 3, _>(|j| {
            (0..nv)
                .map(|k| cf[1] * fx[j][k] + cf[2] * gx[j][k])
                .collect::<Vec<_>>()
        });
        let value = if let Some(cache) = cache {
            crate::gas_quadratic_reconstruction_admission::minimum_density_cached(
                &cache[side],
                &geometry.basis,
                &base,
                std::array::from_fn(|j| gradient[j].as_slice()),
                geometry.ale_velocity_m_s,
            )?
        } else {
            minimum_density(
                &geometry.basis,
                &base,
                std::array::from_fn(|j| gradient[j].as_slice()),
                geometry.ale_velocity_m_s,
            )?
        };
        minimum = minimum.min(value);
    }
    if minimum < 0. {
        return Err(fail(
            "continuous multiscale reconstruction is not certified nonnegative",
        ));
    }
    Ok(minimum)
}

pub fn evaluate_prepared<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticMultiscaleFaceState<S>,
    entropy_policy: &QuadraticEntropyPolicy,
) -> CaeResult<QuadraticMultiscaleFaceSolution<S>> {
    evaluate_prepared_cached(geometry, a, entropy_policy, None)
}
pub fn evaluate_prepared_cached<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticMultiscaleFaceState<S>,
    entropy_policy: &QuadraticEntropyPolicy,
    cache: Option<&[crate::gas_quadratic_reconstruction_admission::MinimumDensityCache; 2]>,
) -> CaeResult<QuadraticMultiscaleFaceSolution<S>> {
    evaluate_prepared_equilibrium(geometry, a, entropy_policy, None, cache)
}
pub fn evaluate_prepared_with_equilibrium<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticMultiscaleFaceState<S>,
    entropy_policy: &QuadraticEntropyPolicy,
    density_kg_m3: S,
    velocity_m_s: [S; 3],
) -> CaeResult<QuadraticMultiscaleFaceSolution<S>> {
    evaluate_prepared_with_equilibrium_cached(
        geometry,
        a,
        entropy_policy,
        density_kg_m3,
        velocity_m_s,
        None,
    )
}
pub fn evaluate_prepared_with_equilibrium_cached<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticMultiscaleFaceState<S>,
    entropy_policy: &QuadraticEntropyPolicy,
    density_kg_m3: S,
    velocity_m_s: [S; 3],
    cache: Option<&[crate::gas_quadratic_reconstruction_admission::MinimumDensityCache; 2]>,
) -> CaeResult<QuadraticMultiscaleFaceSolution<S>> {
    if !density_kg_m3.finite_first_order()
        || density_kg_m3.value() < 0.
        || velocity_m_s.iter().any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid prescribed continuous interface equilibrium"));
    }
    evaluate_prepared_equilibrium(
        geometry,
        a,
        entropy_policy,
        Some((density_kg_m3, velocity_m_s)),
        cache,
    )
}
fn evaluate_prepared_equilibrium<S: TransientGasScalar>(
    geometry: &PreparedQuadraticMultiscaleFace<S>,
    a: &QuadraticMultiscaleFaceState<S>,
    entropy_policy: &QuadraticEntropyPolicy,
    prescribed: Option<(S, [S; 3])>,
    cache: Option<&[crate::gas_quadratic_reconstruction_admission::MinimumDensityCache; 2]>,
) -> CaeResult<QuadraticMultiscaleFaceSolution<S>> {
    let basis = &geometry.basis;
    let density = &geometry.density;
    let speed = &geometry.speed;
    let nv = basis.coefficient_count();
    let fields = [&a.left_density, &a.right_density];
    if fields
        .iter()
        .any(|v| v.len() != nv || v.iter().any(|x| !x.finite_first_order() || x.value() < 0.))
        || a.left_density_gradient
            .iter()
            .chain(&a.right_density_gradient)
            .any(|v| v.len() != nv || v.iter().any(|x| !x.finite_first_order()))
        || a.equilibrium_mass_gradient
            .iter()
            .chain(a.equilibrium_momentum_gradient.iter().flatten())
            .any(|x| !x.finite_first_order())
        || [a.gas_rt_m2_s2, a.molecular_viscosity_pa_s, a.step_s]
            .iter()
            .any(|x| !x.finite_first_order() || x.value() <= 0.)
    {
        return Err(fail("invalid continuous multiscale gas face data"));
    }
    let initial_weak = combine(
        density[0].apply(&a.left_density)?,
        density[1].apply(&a.right_density)?,
        1.,
    );
    let initial_moments = basis.weak_moments(&initial_weak)?;
    let rho = prescribed.map_or(initial_moments[0], |v| v.0);
    if rho.value() == 0. && initial_moments[0].value() > 0. {
        return Err(fail("vacuum equilibrium cannot own positive incident gas"));
    }
    if rho.value() < 0. || !rho.finite_first_order() {
        return Err(fail("continuous gas interface mass is invalid"));
    }
    let initial_positive = speed[0].apply(&a.left_density)?;
    let initial_flux = combine(
        initial_positive.clone(),
        speed[1].apply(&a.right_density)?,
        -1.,
    );
    let mut initial_gradient_flux = vec![S::zero(); nv];
    let mut initial_positive_gradient = vec![S::zero(); nv];
    for j in 0..3 {
        let positive_gradient =
            geometry.relative_action(j, 0, true, &a.left_density_gradient[j])?;
        let grad = combine(
            positive_gradient.clone(),
            geometry.relative_action(j, 1, true, &a.right_density_gradient[j])?,
            -1.,
        );
        for k in 0..nv {
            initial_gradient_flux[k] = initial_gradient_flux[k] + grad[k];
            initial_positive_gradient[k] = initial_positive_gradient[k] + positive_gradient[k];
        }
    }
    if rho.value() == 0. {
        if a.left_density_gradient
            .iter()
            .chain(&a.right_density_gradient)
            .flatten()
            .chain(&a.equilibrium_mass_gradient)
            .chain(a.equilibrium_momentum_gradient.iter().flatten())
            .any(|x| x.value() != 0.)
        {
            return Err(fail(
                "vacuum continuous face requires zero primal reconstruction gradients",
            ));
        }
        let zero = vec![S::zero(); nv];
        let zerograd = std::array::from_fn(|_| zero.clone());
        let minimum = certify(
            geometry,
            a,
            &zero,
            &zerograd,
            &zero,
            [S::one(), -a.step_s * 0.5, S::zero(), S::zero()],
            cache,
        )?;
        let flux = initial_flux
            .into_iter()
            .zip(initial_gradient_flux)
            .map(|(f, g)| f - a.step_s * g * 0.5)
            .collect::<Vec<_>>();
        return Ok(QuadraticMultiscaleFaceSolution {
            average_moments: basis.weak_moments(&flux)?,
            average_weak_flux: flux,
            positive_weak_flux: initial_positive
                .into_iter()
                .zip(initial_positive_gradient)
                .map(|(f, g)| f - a.step_s * g * 0.5)
                .collect(),
            interface_density_kg_m3: rho,
            interface_velocity_m_s: None,
            equilibrium_density: None,
            equilibrium_density_gradient: None,
            equilibrium_density_time_derivative: None,
            collision_step_ratio: S::zero(),
            minimum_density_bernstein: minimum,
        });
    }
    let u = prescribed.map_or_else(
        || std::array::from_fn(|j| initial_moments[j + 1] / rho),
        |v| v.1,
    );
    let gaussian = geometry.gaussian();
    let g = gaussian
        .equilibrium(rho, u, a.gas_rt_m2_s2, entropy_policy)?
        .coefficient_density;
    let gm = gaussian.coefficient_moments(&g)?;
    let mut directions = [[S::zero(); 20]; 3];
    for j in 0..3 {
        directions[j] = gaussian.isotropic_moment_direction(
            rho,
            u,
            a.gas_rt_m2_s2,
            a.equilibrium_mass_gradient[j],
            std::array::from_fn(|i| a.equilibrium_momentum_gradient[i][j]),
        )?;
    }
    let gx = gaussian.moment_directions(&g, gm, directions, entropy_policy)?;
    let mut time_weak = vec![S::zero(); nv];
    for j in 0..3 {
        let t = combine(
            geometry.relative_action(j, 0, false, &gx[j])?,
            geometry.relative_action(j, 1, false, &gx[j])?,
            1.,
        );
        for k in 0..nv {
            time_weak[k] = time_weak[k] - t[k];
        }
    }
    let tm = basis.weak_moments(&time_weak)?;
    let gt = gaussian.moment_direction(
        &g,
        gm,
        gaussian.isotropic_moment_direction(
            rho,
            u,
            a.gas_rt_m2_s2,
            tm[0],
            [tm[1], tm[2], tm[3]],
        )?,
        entropy_policy,
    )?;
    let positive_equilibrium = speed[0].apply(&g)?;
    let eqflux = combine(positive_equilibrium.clone(), speed[1].apply(&g)?, -1.);
    let positive_time = speed[0].apply(&gt)?;
    let timeflux = combine(positive_time.clone(), speed[1].apply(&gt)?, -1.);
    let mut eqgradientflux = vec![S::zero(); nv];
    let mut positive_eqgradient = vec![S::zero(); nv];
    for j in 0..3 {
        let positive = geometry.relative_action(j, 0, true, &gx[j])?;
        let x = combine(
            positive.clone(),
            geometry.relative_action(j, 1, true, &gx[j])?,
            -1.,
        );
        for k in 0..nv {
            eqgradientflux[k] = eqgradientflux[k] + x[k];
            positive_eqgradient[k] = positive_eqgradient[k] + positive[k];
        }
    }
    let delta = a.step_s * rho * a.gas_rt_m2_s2 / a.molecular_viscosity_pa_s;
    if !delta.finite_first_order() {
        return Err(fail("continuous molecular collision ratio is nonfinite"));
    }
    let cf = coefficients(delta, a.step_s);
    let minimum = certify(geometry, a, &g, &gx, &gt, cf, cache)?;
    let flux = (0..nv)
        .map(|k| {
            cf[0] * initial_flux[k]
                + (S::one() - cf[0]) * eqflux[k]
                + cf[1] * initial_gradient_flux[k]
                + cf[2] * eqgradientflux[k]
                + cf[3] * timeflux[k]
        })
        .collect::<Vec<_>>();
    let positive_flux = (0..nv)
        .map(|k| {
            cf[0] * initial_positive[k]
                + (S::one() - cf[0]) * positive_equilibrium[k]
                + cf[1] * initial_positive_gradient[k]
                + cf[2] * positive_eqgradient[k]
                + cf[3] * positive_time[k]
        })
        .collect::<Vec<_>>();

    if flux
        .iter()
        .chain(&positive_flux)
        .any(|x| !x.finite_first_order())
    {
        return Err(fail("continuous multiscale gas flux is nonfinite"));
    }
    Ok(QuadraticMultiscaleFaceSolution {
        average_moments: basis.weak_moments(&flux)?,
        average_weak_flux: flux,
        positive_weak_flux: positive_flux,
        interface_density_kg_m3: rho,
        interface_velocity_m_s: Some(u),
        equilibrium_density: Some(g),
        equilibrium_density_gradient: Some(gx),
        equilibrium_density_time_derivative: Some(gt),
        collision_step_ratio: delta,
        minimum_density_bernstein: minimum,
    })
}
