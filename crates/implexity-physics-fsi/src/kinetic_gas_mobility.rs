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

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MODEL_ASSUMPTIONS: &str = "Isothermal linearized BGK gas, diffuse wall accommodation = 1, thin parallel local channel, flow speed small relative to molecular speed";
pub const PRIMARY_MODEL_DOI: &str = "10.1299/kikaic.53.829";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KineticMobilityPolicy {
    pub quadrature_absolute_tolerance: f64,
    pub quadrature_relative_tolerance: f64,
    pub mobility_absolute_tolerance: f64,
    pub mobility_relative_tolerance: f64,
    pub derivative_absolute_tolerance: f64,
    pub derivative_relative_tolerance: f64,
    pub initial_collocation_cells: usize,
    pub maximum_collocation_cells: usize,
    pub maximum_quadrature_intervals: usize,
    pub cache_entries: usize,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct KineticMobilityValue {
    pub flow_rate: f64,
    pub flow_rate_derivative: f64,
    pub collocation_cells: usize,
    pub flow_rate_refinement_difference: f64,
    pub derivative_refinement_difference: f64,
    pub kernel_quadrature_error_estimate: f64,
    pub maximum_linear_residual: f64,
}
pub struct KineticMobilityEvaluator {
    policy: KineticMobilityPolicy,
    cache: HashMap<u64, KineticMobilityValue>,
    fixed_cache: HashMap<u64, KineticMobilityValue>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KineticGasFace<S = f64> {
    pub gap_m: S,
    pub width_m: S,
    pub length_m: S,
    pub dynamic_viscosity_pa_s: S,
    pub gas_rt_m2_s2: S,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct KineticGasFaceValue {
    pub mass_flux_kg_s: f64,
    pub pressure_left_derivative_kg_pa_s: Option<f64>,
    pub pressure_right_derivative_kg_pa_s: Option<f64>,
    pub gap_derivative_kg_m_s: Option<f64>,
    pub viscosity_derivative_kg_pa_s2: Option<f64>,
    pub rt_derivative_kg_s_m2: Option<f64>,
    pub pressure_quadrature_error_estimate_kg_s: f64,
    pub maximum_mobility_refinement_difference: f64,
    pub maximum_mobility_derivative_refinement_difference: f64,
    pub pressure_quadrature_intervals: usize,
    pub maximum_collocation_cells: usize,
    pub sealed: bool,
}
pub trait KineticMobilityScalar: Scalar {
    fn finite_first_order(self) -> bool;
    fn has_direction(self) -> bool;
}
impl KineticMobilityScalar for f64 {
    fn finite_first_order(self) -> bool {
        self.is_finite()
    }
    fn has_direction(self) -> bool {
        false
    }
}
impl<const N: usize> KineticMobilityScalar for Dual<N> {
    fn finite_first_order(self) -> bool {
        self.re.is_finite() && self.eps.iter().all(|v| v.is_finite())
    }
    fn has_direction(self) -> bool {
        self.eps.iter().any(|v| *v != 0.)
    }
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
const X8: [f64; 4] = [
    0.1834346424956498,
    0.525532409916329,
    0.7966664774136267,
    0.9602898564975363,
];
const W8: [f64; 4] = [
    0.362683783378362,
    0.3137066458778873,
    0.2223810344533745,
    0.1012285362903763,
];
const X16: [f64; 8] = [
    0.09501250983763744,
    0.2816035507792589,
    0.4580167776572274,
    0.6178762444026438,
    0.755404408355003,
    0.8656312023878318,
    0.9445750230732326,
    0.9894009349916499,
];
const W16: [f64; 8] = [
    0.1894506104550685,
    0.1826034150449236,
    0.1691565193950025,
    0.1495959888165767,
    0.1246289712555339,
    0.09515851168249278,
    0.06225352393864789,
    0.027152459411754096,
];
#[derive(Clone, Copy)]
struct Integral {
    value: [f64; 2],
    error: [f64; 2],
    intervals: usize,
}
fn gauss<F: FnMut(f64) -> CaeResult<[f64; 2]>>(
    f: &mut F,
    a: f64,
    b: f64,
    x: &[f64],
    w: &[f64],
) -> CaeResult<[f64; 2]> {
    let mid = a + (b - a) * 0.5;
    let half = (b - a) * 0.5;
    let mut out = [0.; 2];
    for (&x, &w) in x.iter().zip(w) {
        let l = f(mid - half * x)?;
        let r = f(mid + half * x)?;
        for k in 0..2 {
            out[k] += half * w * (l[k] + r[k]);
        }
    }
    if out.iter().any(|v| !v.is_finite()) {
        return Err(fail("kinetic quadrature is not finite"));
    }
    Ok(out)
}
fn integrate<F: FnMut(f64) -> CaeResult<[f64; 2]>>(
    mut f: F,
    cuts: &[f64],
    abs: f64,
    rel: f64,
    max: usize,
) -> CaeResult<Integral> {
    let span = cuts.last().copied().unwrap_or(0.) - cuts.first().copied().unwrap_or(0.);
    if span <= 0. || !span.is_finite() {
        return Err(fail("kinetic quadrature interval"));
    }
    let mut stack = cuts
        .windows(2)
        .filter(|s| s[1] > s[0])
        .map(|s| (s[0], s[1]))
        .collect::<Vec<_>>();
    let mut out = Integral {
        value: [0.; 2],
        error: [0.; 2],
        intervals: 0,
    };
    while let Some((a, b)) = stack.pop() {
        out.intervals += 1;
        if out.intervals > max {
            return Err(fail(
                "kinetic quadrature did not converge within the interval budget",
            ));
        }
        let fine = gauss(&mut f, a, b, &X16, &W16)?;
        let coarse = gauss(&mut f, a, b, &X8, &W8)?;
        let error = [(fine[0] - coarse[0]).abs(), (fine[1] - coarse[1]).abs()];
        if (0..2).all(|k| error[k] <= abs * (b - a) / span + rel * fine[k].abs()) {
            for k in 0..2 {
                out.value[k] += fine[k];
                out.error[k] += error[k];
            }
        } else {
            let mid = a + (b - a) * 0.5;
            if mid == a || mid == b {
                return Err(fail("kinetic quadrature reached floating-point resolution"));
            }
            stack.push((a, mid));
            stack.push((mid, b));
        }
    }
    Ok(out)
}
impl KineticMobilityEvaluator {
    pub fn new(policy: KineticMobilityPolicy) -> CaeResult<Self> {
        let v = [
            policy.quadrature_absolute_tolerance,
            policy.quadrature_relative_tolerance,
            policy.mobility_absolute_tolerance,
            policy.mobility_relative_tolerance,
            policy.derivative_absolute_tolerance,
            policy.derivative_relative_tolerance,
        ];
        if v.iter().any(|v| !v.is_finite() || *v < 0.)
            || policy.quadrature_absolute_tolerance <= 0.
            || policy.quadrature_relative_tolerance <= 0.
            || policy.mobility_absolute_tolerance + policy.mobility_relative_tolerance == 0.
            || policy.derivative_absolute_tolerance + policy.derivative_relative_tolerance == 0.
            || policy.initial_collocation_cells < 2
            || policy.initial_collocation_cells % 2 != 0
            || policy.maximum_collocation_cells < policy.initial_collocation_cells.saturating_mul(2)
            || policy.maximum_collocation_cells % 4 != 0
            || policy.maximum_quadrature_intervals == 0
        {
            return Err(fail("invalid kinetic mobility policy"));
        }
        Ok(Self {
            policy,
            cache: HashMap::new(),
            fixed_cache: HashMap::new(),
        })
    }
    pub fn policy(&self) -> &KineticMobilityPolicy {
        &self.policy
    }
    pub fn evaluate(&mut self, d: f64) -> CaeResult<KineticMobilityValue> {
        if !d.is_finite() || d <= 0. {
            return Err(fail("kinetic inverse Knudsen number must be positive"));
        }
        if let Some(v) = self.cache.get(&d.to_bits()) {
            return Ok(*v);
        }
        let mut n = self.policy.initial_collocation_cells;
        let mut old = self.discrete(d, n)?;
        loop {
            n = n
                .checked_mul(2)
                .ok_or_else(|| fail("kinetic collocation size overflow"))?;
            if n > self.policy.maximum_collocation_cells {
                return Err(fail(
                    "kinetic mobility collocation did not converge within the cell budget",
                ));
            }
            let mut value = self.discrete(d, n)?;
            value.flow_rate_refinement_difference = (value.flow_rate - old.flow_rate).abs();
            value.derivative_refinement_difference =
                (value.flow_rate_derivative - old.flow_rate_derivative).abs();
            if value.flow_rate_refinement_difference
                <= self.policy.mobility_absolute_tolerance
                    + self.policy.mobility_relative_tolerance * value.flow_rate.abs()
                && value.derivative_refinement_difference
                    <= self.policy.derivative_absolute_tolerance
                        + self.policy.derivative_relative_tolerance
                            * value.flow_rate_derivative.abs()
            {
                if self.policy.cache_entries > 0 {
                    if self.cache.len() >= self.policy.cache_entries {
                        self.cache.clear();
                    }
                    self.cache.insert(d.to_bits(), value);
                }
                return Ok(value);
            }
            old = value;
        }
    }
    fn fixed_evaluate(&mut self, d: f64) -> CaeResult<KineticMobilityValue> {
        if !d.is_finite() || d <= 0. {
            return Err(fail("kinetic inverse Knudsen number must be positive"));
        }
        if let Some(v) = self.fixed_cache.get(&d.to_bits()) {
            return Ok(*v);
        }
        let n = self.policy.maximum_collocation_cells;
        let old = self.discrete(d, n / 2)?;
        let mut value = self.discrete(d, n)?;
        value.flow_rate_refinement_difference = (value.flow_rate - old.flow_rate).abs();
        value.derivative_refinement_difference =
            (value.flow_rate_derivative - old.flow_rate_derivative).abs();
        if value.flow_rate_refinement_difference
            > self.policy.mobility_absolute_tolerance
                + self.policy.mobility_relative_tolerance * value.flow_rate.abs()
            || value.derivative_refinement_difference
                > self.policy.derivative_absolute_tolerance
                    + self.policy.derivative_relative_tolerance * value.flow_rate_derivative.abs()
        {
            return Err(fail(
                "kinetic face mobility did not converge at the fixed collocation resolution",
            ));
        }
        if self.policy.cache_entries > 0 {
            if self.fixed_cache.len() >= self.policy.cache_entries {
                self.fixed_cache.clear();
            }
            self.fixed_cache.insert(d.to_bits(), value);
        }
        Ok(value)
    }
    fn discrete(&self, d: f64, n: usize) -> CaeResult<KineticMobilityValue> {
        let w = d / n as f64;
        if !w.is_finite() || w <= 0. {
            return Err(fail("kinetic cell width is not representable"));
        }
        let rootpi = std::f64::consts::PI.sqrt();
        let abs = self.policy.quadrature_absolute_tolerance * w.min(1.);
        if abs <= 0. {
            return Err(fail("kinetic quadrature tolerance is not representable"));
        }
        let tail = (-abs.ln() + 8.).sqrt().max(8.);
        let mut cuts = vec![0., w / 100., w, w * 10., 0.01, 0.1, 1., 3., tail];
        cuts.retain(|v| v.is_finite() && *v >= 0. && *v <= tail);
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let half = n / 2;
        let matrix_size = half
            .checked_mul(half)
            .ok_or_else(|| fail("kinetic matrix size overflow"))?;
        let mut kernel = Vec::new();
        let mut derivative = Vec::new();
        kernel
            .try_reserve_exact(n)
            .map_err(|e| fail(e.to_string()))?;
        derivative
            .try_reserve_exact(n)
            .map_err(|e| fail(e.to_string()))?;
        let mut max_error = 0_f64;
        for j in 0..n {
            let (ca, cb, mult) = if j == 0 {
                (0., 0.5 / n as f64, 2.)
            } else {
                ((j as f64 - 0.5) / n as f64, (j as f64 + 0.5) / n as f64, 1.)
            };
            let a = ca * d;
            let delta = (cb - ca) * d;
            let v = integrate(
                |t| {
                    let e = (-t * t - a / t).exp();
                    if e == 0. {
                        return Ok([0., 0.]);
                    }
                    let change = (-delta / t).exp_m1();
                    Ok([
                        e * (-change),
                        e * (if delta / t > 0.5 {
                            cb * (-delta / t).exp() - ca
                        } else {
                            (cb - ca) + cb * change
                        }) * (d / t),
                    ])
                },
                &cuts,
                abs,
                self.policy.quadrature_relative_tolerance,
                self.policy.maximum_quadrature_intervals,
            )
            .map_err(|e| {
                fail(format!(
                    "kinetic kernel D={d}, cells={n}, separation={j}: {e}"
                ))
            })?;
            kernel.push(mult * v.value[0] / rootpi);
            derivative.push(mult * v.value[1] / (rootpi * d));
            max_error = max_error
                .max(mult * v.error[0] / rootpi + 2. * (-tail * tail).exp() / (tail * rootpi))
                .max(mult * v.error[1] / rootpi + 2. * (-tail * tail).exp() / (tail * rootpi));
        }
        let mut data = Vec::new();
        data.try_reserve_exact(matrix_size)
            .map_err(|e| fail(e.to_string()))?;
        data.resize(matrix_size, 0.);
        let mut matrix = DenseMatrix::new(half, half, data).map_err(|e| fail(e.to_string()))?;
        let mut rhs = vec![0.; half];
        for i in 0..half {
            for j in 0..half {
                let k = kernel[i.abs_diff(j)] + kernel[i.abs_diff(n - 1 - j)];
                matrix.data[i * half + j] = if i == j { 1. - k } else { -k };
                rhs[i] += k;
            }
        }
        let factor = DenseLu::new(&matrix).map_err(|e| fail(e.to_string()))?;
        let velocity = factor
            .solve(&rhs, 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let mut drhs = vec![0.; half];
        for i in 0..half {
            for j in 0..half {
                drhs[i] += (derivative[i.abs_diff(j)] + derivative[i.abs_diff(n - 1 - j)])
                    * (velocity[j] + 1.);
            }
        }
        let dv = factor
            .solve(&drhs, 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let q = 2. * velocity.iter().sum::<f64>() / (n as f64 * d);
        let dq = 2. * dv.iter().sum::<f64>() / (n as f64 * d) - q / d;
        let mut residual = 0_f64;
        for i in 0..half {
            let r = (0..half)
                .map(|j| matrix.get(i, j) * velocity[j])
                .sum::<f64>()
                - rhs[i];
            let rd = (0..half).map(|j| matrix.get(i, j) * dv[j]).sum::<f64>() - drhs[i];
            residual = residual.max(r.abs()).max((d * rd).abs());
        }
        let residual_scale = rhs
            .iter()
            .map(|v| v.abs())
            .chain(drhs.iter().map(|v| (d * v).abs()))
            .fold(0., f64::max);
        if residual
            > self.policy.quadrature_absolute_tolerance * d.min(1.)
                + self.policy.quadrature_relative_tolerance * residual_scale
        {
            return Err(fail(
                "kinetic profile linear solve did not meet the residual tolerance",
            ));
        }
        if !q.is_finite() || q <= 0. || !dq.is_finite() || !residual.is_finite() {
            return Err(fail(
                "kinetic mobility solution is outside its numerical domain",
            ));
        }
        Ok(KineticMobilityValue {
            flow_rate: q,
            flow_rate_derivative: dq,
            collocation_cells: n,
            flow_rate_refinement_difference: f64::INFINITY,
            derivative_refinement_difference: f64::INFINITY,
            kernel_quadrature_error_estimate: max_error,
            maximum_linear_residual: residual,
        })
    }
    pub fn evaluate_scalar<S: KineticMobilityScalar>(&mut self, d: S) -> CaeResult<S> {
        if !d.finite_first_order() {
            return Err(fail("nonfinite kinetic mobility first-order input"));
        }
        let v = self.evaluate(d.value())?;
        let out = S::from_f64(v.flow_rate) + (d - S::from_f64(d.value())) * v.flow_rate_derivative;
        if !out.finite_first_order() {
            return Err(fail("nonfinite kinetic mobility first-order output"));
        }
        Ok(out)
    }
    pub fn face_mass_flux_scalar<S: KineticMobilityScalar>(
        &mut self,
        face: &KineticGasFace<S>,
        left: S,
        right: S,
    ) -> CaeResult<S> {
        let inputs = [
            face.gap_m,
            face.width_m,
            face.length_m,
            face.dynamic_viscosity_pa_s,
            face.gas_rt_m2_s2,
            left,
            right,
        ];
        if inputs.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("nonfinite kinetic face first-order input"));
        }
        let primal = KineticGasFace {
            gap_m: face.gap_m.value(),
            width_m: face.width_m.value(),
            length_m: face.length_m.value(),
            dynamic_viscosity_pa_s: face.dynamic_viscosity_pa_s.value(),
            gas_rt_m2_s2: face.gas_rt_m2_s2.value(),
        };
        let v = self.face_mass_flux(&primal, left.value(), right.value())?;
        let coefficients = [
            v.gap_derivative_kg_m_s,
            Some(v.mass_flux_kg_s / primal.width_m),
            Some(-v.mass_flux_kg_s / primal.length_m),
            v.viscosity_derivative_kg_pa_s2,
            v.rt_derivative_kg_s_m2,
            v.pressure_left_derivative_kg_pa_s,
            v.pressure_right_derivative_kg_pa_s,
        ];
        let mut out = S::from_f64(v.mass_flux_kg_s);
        for (input, coefficient) in inputs.into_iter().zip(coefficients) {
            if input.has_direction() {
                let c = coefficient.ok_or_else(|| {
                    fail("kinetic face derivative crosses a closed or vacuum endpoint")
                })?;
                out += (input - S::from_f64(input.value())) * c;
            }
        }
        if !out.finite_first_order() {
            return Err(fail("nonfinite kinetic face first-order output"));
        }
        Ok(out)
    }
    pub fn face_mass_flux(
        &mut self,
        face: &KineticGasFace,
        left: f64,
        right: f64,
    ) -> CaeResult<KineticGasFaceValue> {
        if [face.gap_m, left, right]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.)
            || [
                face.width_m,
                face.length_m,
                face.dynamic_viscosity_pa_s,
                face.gas_rt_m2_s2,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.)
        {
            return Err(fail("invalid kinetic gas face"));
        }
        if face.gap_m == 0. {
            return Ok(KineticGasFaceValue {
                mass_flux_kg_s: 0.,
                pressure_left_derivative_kg_pa_s: Some(0.),
                pressure_right_derivative_kg_pa_s: Some(0.),
                gap_derivative_kg_m_s: None,
                viscosity_derivative_kg_pa_s2: Some(0.),
                rt_derivative_kg_s_m2: Some(0.),
                pressure_quadrature_error_estimate_kg_s: 0.,
                maximum_mobility_refinement_difference: 0.,
                maximum_mobility_derivative_refinement_difference: 0.,
                pressure_quadrature_intervals: 0,
                maximum_collocation_cells: 0,
                sealed: true,
            });
        }
        let speed = (2. * face.gas_rt_m2_s2).sqrt();
        let c = face.gap_m / (face.dynamic_viscosity_pa_s * speed);
        let scale = face.width_m * face.gap_m * face.gap_m / (face.length_m * speed);
        if !c.is_finite() || c <= 0. || !scale.is_finite() || scale <= 0. {
            return Err(fail("kinetic face scales are not representable"));
        }
        let low = left.min(right);
        let high = left.max(right);
        let sign = if left >= right { 1. } else { -1. };
        let mut max_cells = 0;
        let mut max_refinement = 0_f64;
        let mut max_derivative_refinement = 0_f64;
        let mut intervals = 0;
        let mut qint = [0.; 2];
        let mut error = 0.;
        if high > low {
            let delta = high - low;
            let pol = self.policy.clone();
            let v = integrate(
                |x| {
                    let (p, jac) = if low == 0. {
                        (high * x * x, 2. * high * x)
                    } else {
                        (low + delta * x, delta)
                    };
                    let m = self.fixed_evaluate(c * p)?;
                    max_cells = max_cells.max(m.collocation_cells);
                    max_refinement = max_refinement.max(m.flow_rate_refinement_difference);
                    max_derivative_refinement =
                        max_derivative_refinement.max(m.derivative_refinement_difference);
                    Ok([jac * m.flow_rate, jac * (c * p) * m.flow_rate_derivative])
                },
                &[0., 1.],
                pol.quadrature_absolute_tolerance * delta,
                pol.quadrature_relative_tolerance,
                pol.maximum_quadrature_intervals,
            )?;
            qint = v.value;
            error = v.error[0];
            intervals = v.intervals;
        }
        let mut endpoint = |p: f64| -> CaeResult<Option<f64>> {
            if p == 0. {
                Ok(None)
            } else {
                let m = self.fixed_evaluate(c * p)?;
                max_cells = max_cells.max(m.collocation_cells);
                max_refinement = max_refinement.max(m.flow_rate_refinement_difference);
                max_derivative_refinement =
                    max_derivative_refinement.max(m.derivative_refinement_difference);
                Ok(Some(scale * m.flow_rate))
            }
        };
        let dl = endpoint(left)?;
        let dr = endpoint(right)?.map(|v| -v);
        let signed = sign * scale;
        let flux = signed * qint[0];
        let dh = signed * (2. * qint[0] + qint[1]) / face.gap_m;
        let dmu = -signed * qint[1] / face.dynamic_viscosity_pa_s;
        let drt = -signed * (qint[0] + qint[1]) / (2. * face.gas_rt_m2_s2);
        if [flux, dh, dmu, drt, error * scale]
            .iter()
            .any(|v| !v.is_finite())
        {
            return Err(fail("kinetic face flux or derivative is not finite"));
        }
        Ok(KineticGasFaceValue {
            mass_flux_kg_s: flux,
            pressure_left_derivative_kg_pa_s: dl,
            pressure_right_derivative_kg_pa_s: dr,
            gap_derivative_kg_m_s: Some(dh),
            viscosity_derivative_kg_pa_s2: Some(dmu),
            rt_derivative_kg_s_m2: Some(drt),
            pressure_quadrature_error_estimate_kg_s: error * scale,
            maximum_mobility_refinement_difference: max_refinement,
            maximum_mobility_derivative_refinement_difference: max_derivative_refinement,
            pressure_quadrature_intervals: intervals,
            maximum_collocation_cells: max_cells,
            sealed: false,
        })
    }
}
