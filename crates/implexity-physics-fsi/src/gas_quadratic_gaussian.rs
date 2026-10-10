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
use crate::gas_quadratic_velocity::{
    QuadraticEntropyPolicy, QuadraticEquilibrium, QuadraticVelocityBasis,
};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn dot19(a: [f64; 19], b: [f64; 19]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
const POWERS: [[usize; 3]; 20] = [
    [0, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [0, 0, 1],
    [2, 0, 0],
    [0, 2, 0],
    [0, 0, 2],
    [1, 1, 0],
    [1, 0, 1],
    [0, 1, 1],
    [3, 0, 0],
    [0, 3, 0],
    [0, 0, 3],
    [2, 1, 0],
    [2, 0, 1],
    [1, 2, 0],
    [0, 2, 1],
    [1, 0, 2],
    [0, 1, 2],
    [1, 1, 1],
];
#[derive(Clone)]
pub struct QuadraticGaussianBasis {
    basis: Arc<QuadraticVelocityBasis>,
    reference_speed: f64,
    count: usize,
    weights: Vec<f64>,
    log_weights: Vec<f64>,
    features: Vec<[f64; 19]>,
    integrated: Vec<[f64; 20]>,
    primal_cache: Option<Arc<Mutex<ExactPrimalCache>>>,
}
#[derive(Clone)]
struct ExactPrimalData {
    prob: Vec<f64>,
    mean: [f64; 19],
    inverse: Vec<f64>,
    norm: f64,
    iteration: usize,
    condition: f64,
    condition_limit: f64,
}
struct ExactPrimalCache {
    maximum_entries: usize,
    entries: VecDeque<(Vec<u64>, ExactPrimalData)>,
    hits: usize,
    misses: usize,
}
impl QuadraticGaussianBasis {
    pub fn new(basis: Arc<QuadraticVelocityBasis>, reference_speed: f64) -> CaeResult<Self> {
        if !reference_speed.is_finite()
            || reference_speed <= 0.
            || !reference_speed.powi(3).is_finite()
            || reference_speed.powi(3) == 0.
        {
            return Err(fail("invalid Gaussian numerical reference speed"));
        }
        let count = basis.coefficient_count();
        let dims = basis.dimensions();
        let mut axial: Vec<Vec<[f64; 4]>> = Vec::new();
        for axis in 0..3 {
            let mut rows = Vec::new();
            for i in 0..dims[axis] {
                let mut row = [0.; 4];
                for interval in basis.intervals(axis, i)? {
                    let lo = interval.lower_m_s;
                    let hi = interval.upper_m_s;
                    for (z, w) in [
                        (-0.7745966692414834, 5. / 9.),
                        (0., 8. / 9.),
                        (0.7745966692414834, 5. / 9.),
                    ] {
                        let x = lo + (hi - lo) * (z + 1.) * 0.5;
                        let value = basis.axial_values(axis, x)?[i];
                        for p in 0..4 {
                            row[p] +=
                                w * (hi - lo) * 0.5 * value * (x / reference_speed).powi(p as i32);
                        }
                    }
                }
                rows.push(row);
            }
            axial.push(rows);
        }
        let mut integrated = vec![[0.; 20]; count];
        let mut features = vec![[0.; 19]; count];
        let legacy = basis.integrated_moment_rows();
        for k in 0..count {
            let c = basis.coefficient_coordinate(k)?;
            for j in 0..20 {
                let degree = POWERS[j].iter().sum::<usize>() as i32;
                integrated[k][j] = if j < 10 {
                    legacy[k][j]
                } else {
                    (0..3)
                        .map(|a| axial[a][c[a]][POWERS[j][a]])
                        .product::<f64>()
                        * reference_speed.powi(degree)
                };
                if j > 0 {
                    features[k][j - 1] =
                        integrated[k][j] / legacy[k][0] / reference_speed.powi(degree);
                }
            }
        }
        let weights = legacy.iter().map(|r| r[0]).collect::<Vec<_>>();
        if integrated
            .iter()
            .flatten()
            .chain(features.iter().flatten())
            .any(|v| !v.is_finite())
        {
            return Err(fail("nonfinite Gaussian integrated features"));
        }
        Ok(Self {
            basis,
            reference_speed,
            count,
            log_weights: weights.iter().map(|w| w.ln()).collect(),
            weights,
            features,
            integrated,
            primal_cache: None,
        })
    }
    pub fn with_exact_primal_cache(mut self, maximum_entries: usize) -> CaeResult<Self> {
        if maximum_entries == 0 {
            return Err(fail("exact Gaussian cache requires positive capacity"));
        }
        self.primal_cache = Some(Arc::new(Mutex::new(ExactPrimalCache {
            maximum_entries,
            entries: VecDeque::new(),
            hits: 0,
            misses: 0,
        })));
        Ok(self)
    }
    pub fn exact_primal_cache_stats(&self) -> CaeResult<Option<(usize, usize, usize, usize)>> {
        let Some(cache) = &self.primal_cache else {
            return Ok(None);
        };
        let c = cache
            .lock()
            .map_err(|_| fail("exact Gaussian cache lock poisoned"))?;
        let bytes = c.entries.capacity() * std::mem::size_of::<(Vec<u64>, ExactPrimalData)>()
            + c.entries
                .iter()
                .map(|(k, d)| k.capacity() * 8 + d.prob.capacity() * 8 + d.inverse.capacity() * 8)
                .sum::<usize>();
        Ok(Some((c.hits, c.misses, c.entries.len(), bytes)))
    }
    fn lift_exact_primal<S: GasMassScalar>(
        &self,
        rho: S,
        target: [S; 19],
        tv: [f64; 19],
        data: &ExactPrimalData,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        let change = std::array::from_fn::<_, 19, _>(|i| {
            (0..19).fold(S::zero(), |s, j| {
                s + (target[j] - S::from_f64(tv[j])) * data.inverse[i * 19 + j]
            })
        });
        let coeff = (0..self.count)
            .map(|k| {
                let linear = (0..19).fold(S::zero(), |s, j| {
                    s + change[j] * (self.features[k][j] - data.mean[j])
                });
                rho * (data.prob[k] / self.weights[k]) * (S::one() + linear)
            })
            .collect::<Vec<_>>();
        if coeff
            .iter()
            .any(|x| !x.finite_first_order() || x.value() < 0.)
        {
            return Err(fail("nonfinite quadratic equilibrium derivative"));
        }
        Ok(QuadraticEquilibrium {
            coefficient_density: coeff,
            maximum_normalized_moment_residual: data.norm,
            newton_iterations: data.iteration,
        })
    }
    pub fn exact_primal_cache_certificates(&self) -> CaeResult<Vec<(f64, f64, f64, usize)>> {
        let Some(cache) = &self.primal_cache else {
            return Ok(Vec::new());
        };
        let c = cache
            .lock()
            .map_err(|_| fail("exact Gaussian cache lock poisoned"))?;
        Ok(c.entries
            .iter()
            .map(|(_, d)| (d.norm, d.condition, d.condition_limit, d.iteration))
            .collect())
    }
    pub fn basis(&self) -> &Arc<QuadraticVelocityBasis> {
        &self.basis
    }
    pub fn integrated_moment_rows(&self) -> &[[f64; 20]] {
        &self.integrated
    }
    fn scale(&self, j: usize) -> f64 {
        self.reference_speed
            .powi(POWERS[j].iter().sum::<usize>() as i32)
    }
    pub fn coefficient_moments<S: GasMassScalar>(&self, a: &[S]) -> CaeResult<[S; 20]> {
        if a.len() != self.count || a.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("Gaussian coefficient moment shape or value"));
        }
        let m = std::array::from_fn(|j| {
            a.iter()
                .zip(&self.integrated)
                .fold(S::zero(), |v, (a, r)| v + *a * r[j])
        });
        if m.iter().any(|v: &S| !v.finite_first_order()) {
            return Err(fail("nonfinite Gaussian coefficient moments"));
        }
        Ok(m)
    }
    pub fn gaussian<S: GasMassScalar>(
        &self,
        rho: S,
        u: [S; 3],
        cov: [[S; 3]; 3],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        if !rho.finite_first_order()
            || rho.value() < 0.
            || u.iter()
                .chain(cov.iter().flatten())
                .any(|v| !v.finite_first_order())
        {
            return Err(fail("invalid Gaussian mean or covariance"));
        }
        for i in 0..3 {
            for j in 0..3 {
                let difference = cov[i][j] - cov[j][i];
                if difference.value() != 0. || difference.has_direction() {
                    return Err(fail("Gaussian covariance must be symmetric"));
                }
            }
        }
        let mut l = [[0.; 3]; 3];
        for i in 0..3 {
            for j in 0..=i {
                let x = cov[i][j].value() - (0..j).map(|k| l[i][k] * l[j][k]).sum::<f64>();
                if i == j {
                    if !x.is_finite() || x <= 0. {
                        return Err(fail("Gaussian covariance must be positive definite"));
                    }
                    l[i][j] = x.sqrt();
                } else {
                    l[i][j] = x / l[j][j];
                }
            }
        }
        let moments = std::array::from_fn(|j| {
            let mut indices = Vec::new();
            for a in 0..3 {
                for _ in 0..POWERS[j][a] {
                    indices.push(a);
                }
            }
            rho * match indices.len() {
                0 => S::one(),
                1 => u[indices[0]],
                2 => u[indices[0]] * u[indices[1]] + cov[indices[0]][indices[1]],
                3 => {
                    let (a, b, c) = (indices[0], indices[1], indices[2]);
                    u[a] * u[b] * u[c] + u[a] * cov[b][c] + u[b] * cov[a][c] + u[c] * cov[a][b]
                }
                _ => unreachable!(),
            }
        });
        let mut inverse = [[0.; 3]; 3];
        for column in 0..3 {
            let mut forward = [0.; 3];
            for i in 0..3 {
                forward[i] = (f64::from(i == column)
                    - (0..i).map(|j| l[i][j] * forward[j]).sum::<f64>())
                    / l[i][i];
            }
            for i in (0..3).rev() {
                inverse[i][column] = (forward[i]
                    - (i + 1..3)
                        .map(|j| l[j][i] * inverse[j][column])
                        .sum::<f64>())
                    / l[i][i];
            }
        }
        let mut initial = [0.; 19];
        for i in 0..3 {
            initial[i] =
                self.reference_speed * (0..3).map(|j| inverse[i][j] * u[j].value()).sum::<f64>();
        }
        for (j, (a, b)) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)]
            .into_iter()
            .enumerate()
        {
            initial[3 + j] =
                -self.reference_speed.powi(2) * inverse[a][b] * if a == b { 0.5 } else { 1. };
        }
        if initial.iter().any(|v| !v.is_finite()) || self.entropy_statistics(&initial).is_err() {
            initial = [0.; 19];
        }
        self.moment_entropy_initial(moments, initial, p)
    }
    pub fn equilibrium<S: GasMassScalar>(
        &self,
        rho: S,
        u: [S; 3],
        rt: S,
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        if !rt.finite_first_order() || rt.value() <= 0. {
            return Err(fail("invalid Gaussian thermal speed"));
        }
        self.gaussian(
            rho,
            u,
            std::array::from_fn(|i| std::array::from_fn(|j| if i == j { rt } else { S::zero() })),
            p,
        )
    }
    pub fn isotropic_moment_direction<S: GasMassScalar>(
        &self,
        rho: S,
        u: [S; 3],
        rt: S,
        drho: S,
        dj: [S; 3],
    ) -> CaeResult<[S; 20]> {
        if [rho, rt, drho]
            .iter()
            .chain(&u)
            .chain(&dj)
            .any(|x| !x.finite_first_order())
            || rho.value() <= 0.
            || rt.value() <= 0.
        {
            return Err(fail("invalid Gaussian hydrodynamic direction"));
        }
        let du = std::array::from_fn::<_, 3, _>(|j| (dj[j] - u[j] * drho) / rho);
        let covariance = |i: usize, j: usize| if i == j { rt } else { S::zero() };
        let out = std::array::from_fn(|j| {
            let mut ix = Vec::new();
            for a in 0..3 {
                for _ in 0..POWERS[j][a] {
                    ix.push(a);
                }
            }
            match ix.len() {
                0 => drho,
                1 => dj[ix[0]],
                2 => {
                    let (a, b) = (ix[0], ix[1]);
                    drho * (u[a] * u[b] + covariance(a, b)) + rho * (du[a] * u[b] + u[a] * du[b])
                }
                3 => {
                    let (a, b, c) = (ix[0], ix[1], ix[2]);
                    drho * (u[a] * u[b] * u[c]
                        + u[a] * covariance(b, c)
                        + u[b] * covariance(a, c)
                        + u[c] * covariance(a, b))
                        + rho
                            * (du[a] * u[b] * u[c]
                                + u[a] * du[b] * u[c]
                                + u[a] * u[b] * du[c]
                                + du[a] * covariance(b, c)
                                + du[b] * covariance(a, c)
                                + du[c] * covariance(a, b))
                }
                _ => unreachable!(),
            }
        });
        if out.iter().any(|x: &S| !x.finite_first_order()) {
            return Err(fail("nonfinite Gaussian hydrodynamic direction"));
        }
        Ok(out)
    }
    pub fn moment_entropy<S: GasMassScalar>(
        &self,
        m: [S; 20],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        self.moment_entropy_initial(m, [0.; 19], p)
    }
    fn moment_entropy_initial<S: GasMassScalar>(
        &self,
        m: [S; 20],
        initial: [f64; 19],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        if m.iter().any(|v| !v.finite_first_order())
            || m[0].value() < 0.
            || !p.moment_tolerance.is_finite()
            || p.moment_tolerance <= 0.
            || p.maximum_iterations == 0
            || !p.maximum_covariance_condition.is_finite()
            || p.maximum_covariance_condition <= 1.
        {
            return Err(fail("invalid Gaussian entropy domain"));
        }
        if m[0].value() == 0. {
            if m.iter().any(|v| v.value() != 0. || v.has_direction()) {
                return Err(fail("Gaussian vacuum requires a fixed zero moment stratum"));
            }
            return Ok(QuadraticEquilibrium {
                coefficient_density: vec![S::zero(); self.count],
                maximum_normalized_moment_residual: 0.,
                newton_iterations: 0,
            });
        }
        let target = std::array::from_fn(|j| m[j + 1] / m[0] / self.scale(j + 1));
        self.entropy_target(m[0], target, initial, p)
    }
    pub fn moment_entropy_vjp(
        &self,
        m: [f64; 20],
        bar: &[f64],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<[f64; 20]> {
        if bar.len() != self.count || bar.iter().any(|x| !x.is_finite()) {
            return Err(fail("Gaussian cotangent shape or value"));
        }
        let seed = std::array::from_fn(|j| {
            let mut d = [0.; 20];
            d[j] = 1.;
            Dual::new(m[j], d)
        });
        let eq = self.moment_entropy(seed, p)?;
        let mut out = [0.; 20];
        for (a, b) in eq.coefficient_density.iter().zip(bar) {
            for j in 0..20 {
                out[j] += a.eps[j] * b;
            }
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite Gaussian moment pullback"));
        }
        Ok(out)
    }
    fn entropy_target<S: GasMassScalar>(
        &self,
        rho: S,
        target: [S; 19],
        mut lambda: [f64; 19],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        let tv = target.map(|x| x.value());
        let mut key = Vec::new();
        if let Some(cache) = &self.primal_cache {
            key.extend(tv.iter().map(|v| v.to_bits()));
            key.extend(lambda.iter().map(|v| v.to_bits()));
            key.extend([
                p.moment_tolerance.to_bits(),
                p.maximum_covariance_condition.to_bits(),
                p.maximum_iterations as u64,
            ]);
            let found = {
                let mut c = cache
                    .lock()
                    .map_err(|_| fail("exact Gaussian cache lock poisoned"))?;
                if let Some(i) = c.entries.iter().position(|(k, _)| k == &key) {
                    let entry = c.entries.remove(i).unwrap();
                    let data = entry.1.clone();
                    c.entries.push_back(entry);
                    c.hits += 1;
                    Some(data)
                } else {
                    c.misses += 1;
                    None
                }
            };
            if let Some(data) = found {
                return self.lift_exact_primal(rho, target, tv, &data);
            }
        }
        for iteration in 0..p.maximum_iterations {
            let (prob, mean, cov, logz) = self.entropy_statistics(&lambda)?;
            let residual = std::array::from_fn::<_, 19, _>(|j| mean[j] - tv[j]);
            let norm = residual.iter().map(|x| x.abs()).fold(0., f64::max);
            let factor = DenseLu::new(
                &DenseMatrix::new(19, 19, cov.to_vec()).map_err(|e| fail(e.to_string()))?,
            )
            .map_err(|_| {
                fail("quadratic Gaussian entropy target is outside strict realizability")
            })?;
            if norm <= p.moment_tolerance {
                let mut inverse = vec![0.; 361];
                for j in 0..19 {
                    let mut unit = vec![0.; 19];
                    unit[j] = 1.;
                    let column = factor
                        .solve(&unit, 1, false)
                        .map_err(|e| fail(e.to_string()))?;
                    for i in 0..19 {
                        inverse[i * 19 + j] = column[i];
                    }
                }
                let matrix_norm = (0..19)
                    .map(|i| {
                        cov[i * 19..(i + 1) * 19]
                            .iter()
                            .map(|x| x.abs())
                            .sum::<f64>()
                    })
                    .fold(0., f64::max);
                let inverse_norm = (0..19)
                    .map(|i| {
                        inverse[i * 19..(i + 1) * 19]
                            .iter()
                            .map(|x| x.abs())
                            .sum::<f64>()
                    })
                    .fold(0., f64::max);
                let condition = matrix_norm * inverse_norm;
                if condition > p.maximum_covariance_condition {
                    return Err(fail(
                        "quadratic Gaussian entropy covariance requires velocity refinement",
                    ));
                }
                let data = ExactPrimalData {
                    prob,
                    mean,
                    inverse,
                    norm,
                    iteration,
                    condition,
                    condition_limit: p.maximum_covariance_condition,
                };
                let result = self.lift_exact_primal(rho, target, tv, &data)?;
                if let Some(cache) = &self.primal_cache {
                    let mut c = cache
                        .lock()
                        .map_err(|_| fail("exact Gaussian cache lock poisoned"))?;
                    if let Some(i) = c.entries.iter().position(|(k, _)| k == &key) {
                        c.entries.remove(i);
                    }
                    if c.entries.len() == c.maximum_entries {
                        c.entries.pop_front();
                    }
                    c.entries.push_back((key, data));
                }
                return Ok(result);
            }
            let step = factor
                .solve(&residual, 1, false)
                .map_err(|e| fail(e.to_string()))?;
            let merit = logz - dot19(lambda, tv);
            let decrement = dot19(residual, std::array::from_fn(|i| step[i]));
            let mut alpha = 1.;
            let mut accepted = None;
            for _ in 0..48 {
                let trial = std::array::from_fn(|i| lambda[i] - alpha * step[i]);
                if let Ok((_, trial_mean, _, lz)) = self.entropy_statistics(&trial) {
                    let trial_merit = lz - dot19(trial, tv);
                    let trial_norm = (0..19)
                        .map(|j| (trial_mean[j] - tv[j]).abs())
                        .fold(0., f64::max);
                    let roundoff = 16.
                        * f64::EPSILON
                        * (1.
                            + logz.abs()
                            + dot19(lambda, tv).abs()
                            + lz.abs()
                            + dot19(trial, tv).abs());
                    if trial_merit <= merit - 1e-4 * alpha * decrement
                        || (trial_norm < norm && trial_merit <= merit + roundoff)
                    {
                        accepted = Some(trial);
                        break;
                    }
                }
                alpha *= 0.5;
            }
            lambda = accepted
                .ok_or_else(|| fail("quadratic Gaussian entropy solve failed admissibility"))?;
        }
        Err(fail(
            "quadratic Gaussian entropy moment solve did not converge",
        ))
    }
    pub fn moment_direction<S: GasMassScalar>(
        &self,
        equilibrium: &[S],
        moments: [S; 20],
        direction: [S; 20],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<Vec<S>> {
        let [out] = self.moment_directions(equilibrium, moments, [direction], p)?;
        Ok(out)
    }
    pub fn moment_directions<S: GasMassScalar, const N: usize>(
        &self,
        equilibrium: &[S],
        moments: [S; 20],
        directions: [[S; 20]; N],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<[Vec<S>; N]> {
        if equilibrium.len() != self.count
            || equilibrium
                .iter()
                .any(|v| !v.finite_first_order() || v.value() <= 0.)
            || moments
                .iter()
                .chain(directions.iter().flatten())
                .any(|v| !v.finite_first_order())
            || moments[0].value() <= 0.
            || !p.moment_tolerance.is_finite()
            || p.moment_tolerance <= 0.
            || !p.maximum_covariance_condition.is_finite()
            || p.maximum_covariance_condition <= 1.
        {
            return Err(fail(
                "invalid quadratic Gaussian entropy moment-gradient domain",
            ));
        }
        let primitive_available = S::try_first_order_lift(0., &[], &[])?.is_some();
        let rho = moments[0];
        let actual = self.coefficient_moments(equilibrium)?;
        for j in 0..20 {
            let scale = rho.value() * self.scale(j);
            if !scale.is_finite()
                || scale <= 0.
                || (actual[j].value() - moments[j].value()).abs() > p.moment_tolerance * scale
            {
                return Err(fail(
                    "quadratic Gaussian entropy gradient state does not match its moments",
                ));
            }
        }
        let prob = equilibrium
            .iter()
            .zip(&self.weights)
            .map(|(g, w)| *g * *w / rho)
            .collect::<Vec<_>>();
        let mut inputs = equilibrium.to_vec();
        inputs.push(rho);
        let probability = prob.iter().map(|v| v.value()).collect::<Vec<_>>();
        let mean_value = std::array::from_fn::<_, 19, _>(|j| {
            probability
                .iter()
                .zip(&self.features)
                .fold(0., |v, (a, f)| v + a * f[j])
        });
        let mut mean = [S::zero(); 19];
        for j in 0..19 {
            let mut partials = self
                .weights
                .iter()
                .zip(&self.features)
                .map(|(w, f)| w * f[j] / rho.value())
                .collect::<Vec<_>>();
            partials.push(-mean_value[j] / rho.value());
            mean[j] = match S::try_first_order_lift(mean_value[j], &inputs, &partials)? {
                Some(v) => v,
                None => prob
                    .iter()
                    .zip(&self.features)
                    .fold(S::zero(), |v, (a, f)| v + *a * f[j]),
            };
        }
        let legacy_cov = if primitive_available {
            None
        } else {
            let mut cov = [S::zero(); 361];
            for i in 0..19 {
                for j in i..19 {
                    let v = prob
                        .iter()
                        .zip(&self.features)
                        .fold(S::zero(), |v, (a, f)| {
                            v + *a * (S::from_f64(f[i]) - mean[i]) * (S::from_f64(f[j]) - mean[j])
                        });
                    cov[i * 19 + j] = v;
                    cov[j * 19 + i] = v;
                }
            }
            Some(cov)
        };
        let mut cv = [0.; 361];
        for i in 0..19 {
            for j in i..19 {
                let value = if let Some(cov) = &legacy_cov {
                    cov[i * 19 + j].value()
                } else {
                    prob.iter().zip(&self.features).fold(0., |v, (a, f)| {
                        v + a.value() * (f[i] - mean_value[i]) * (f[j] - mean_value[j])
                    })
                };
                cv[i * 19 + j] = value;
                cv[j * 19 + i] = value;
            }
        }
        let factor =
            DenseLu::new(&DenseMatrix::new(19, 19, cv.to_vec()).map_err(|e| fail(e.to_string()))?)
                .map_err(|e| fail(e.to_string()))?;
        let mut inverse = vec![0.; 361];
        for j in 0..19 {
            let mut unit = vec![0.; 19];
            unit[j] = 1.;
            let col = factor
                .solve(&unit, 1, false)
                .map_err(|e| fail(e.to_string()))?;
            for i in 0..19 {
                inverse[i * 19 + j] = col[i];
            }
        }
        let cn = (0..19)
            .map(|i| {
                cv[i * 19..(i + 1) * 19]
                    .iter()
                    .map(|v| v.abs())
                    .sum::<f64>()
            })
            .fold(0., f64::max);
        let ci = (0..19)
            .map(|i| {
                inverse[i * 19..(i + 1) * 19]
                    .iter()
                    .map(|v| v.abs())
                    .sum::<f64>()
            })
            .fold(0., f64::max);
        if !cn.is_finite() || !ci.is_finite() || cn * ci > p.maximum_covariance_condition {
            return Err(fail(
                "quadratic Gaussian entropy gradient covariance requires refinement",
            ));
        }
        let mut outputs: [Vec<S>; N] = std::array::from_fn(|_| Vec::new());
        for (index, direction) in directions.into_iter().enumerate() {
            let target = std::array::from_fn::<_, 19, _>(|j| {
                (direction[j + 1] - moments[j + 1] / rho * direction[0]) / rho / self.scale(j + 1)
            });
            let rhs = target.map(|v| v.value());
            let value = factor
                .solve(&rhs, 1, false)
                .map_err(|e| fail(e.to_string()))?;
            let action = if legacy_cov.is_some() || value.iter().all(|v| *v == 0.) {
                [S::zero(); 19]
            } else {
                let probability_sum = probability.iter().sum::<f64>();
                let mean_product = (0..19).map(|j| mean_value[j] * value[j]).sum::<f64>();
                let feature_product = self
                    .features
                    .iter()
                    .map(|f| (0..19).map(|j| f[j] * value[j]).sum::<f64>())
                    .collect::<Vec<_>>();
                let mut action = [S::zero(); 19];
                for i in 0..19 {
                    let primal = probability
                        .iter()
                        .zip(&self.features)
                        .zip(&feature_product)
                        .fold(0., |v, ((p, f), product)| {
                            v + p * (f[i] - mean_value[i]) * (product - mean_product)
                        });
                    let probability_partials = self
                        .features
                        .iter()
                        .zip(&feature_product)
                        .map(|(f, product)| {
                            f[i] * product
                                + mean_value[i] * mean_product
                                + (probability_sum - 2.)
                                    * (f[i] * mean_product + mean_value[i] * product)
                        })
                        .collect::<Vec<_>>();
                    let mut partials = probability_partials
                        .iter()
                        .zip(&self.weights)
                        .map(|(p, w)| p * w / rho.value())
                        .collect::<Vec<_>>();
                    partials.push(
                        -probability_partials
                            .iter()
                            .zip(&probability)
                            .map(|(a, p)| a * p)
                            .sum::<f64>()
                            / rho.value(),
                    );
                    action[i] = match S::try_first_order_lift(primal, &inputs, &partials)? {
                        Some(v) => v,
                        None => prob
                            .iter()
                            .zip(&self.features)
                            .fold(S::zero(), |v, (p, f)| {
                                let product = (0..19).fold(S::zero(), |v, j| {
                                    v + (S::from_f64(f[j]) - mean[j]) * value[j]
                                });
                                v + *p * (S::from_f64(f[i]) - mean[i]) * product
                            }),
                    };
                }
                action
            };
            let linear = if legacy_cov.is_some() {
                [S::zero(); 19]
            } else {
                std::array::from_fn::<_, 19, _>(|j| {
                    let covariance_change = action[j] - S::from_f64(action[j].value());
                    target[j] - S::from_f64(rhs[j]) - covariance_change
                })
            };
            let change = std::array::from_fn::<_, 19, _>(|i| {
                if let Some(cov) = &legacy_cov {
                    S::from_f64(value[i])
                        + (0..19).fold(S::zero(), |sum, j| {
                            let linear = target[j]
                                - S::from_f64(rhs[j])
                                - (0..19).fold(S::zero(), |v, k| {
                                    v + (cov[j * 19 + k] - S::from_f64(cv[j * 19 + k])) * value[k]
                                });
                            sum + linear * inverse[i * 19 + j]
                        })
                } else {
                    S::from_f64(value[i])
                        + (0..19).fold(S::zero(), |sum, j| sum + linear[j] * inverse[i * 19 + j])
                }
            });
            let out = (0..self.count)
                .map(|k| {
                    equilibrium[k]
                        * (direction[0] / rho
                            + (0..19).fold(S::zero(), |v, j| {
                                v + (S::from_f64(self.features[k][j]) - mean[j]) * change[j]
                            }))
                })
                .collect::<Vec<_>>();
            if out.iter().any(|v| !v.finite_first_order()) {
                return Err(fail(
                    "quadratic Gaussian entropy moment-gradient is nonfinite",
                ));
            }
            outputs[index] = out;
        }
        Ok(outputs)
    }
    fn entropy_statistics(
        &self,
        lambda: &[f64; 19],
    ) -> CaeResult<(Vec<f64>, [f64; 19], [f64; 361], f64)> {
        let logs = self
            .features
            .iter()
            .zip(&self.log_weights)
            .map(|(h, w)| *w + dot19(*h, *lambda))
            .collect::<Vec<_>>();
        let max = logs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let sum = logs.iter().map(|x| (x - max).exp()).sum::<f64>();
        if !max.is_finite() || !sum.is_finite() || sum <= 0. {
            return Err(fail("nonfinite quadratic Gaussian entropy normalization"));
        }
        let prob = logs
            .iter()
            .map(|x| (x - max).exp() / sum)
            .collect::<Vec<_>>();
        if prob.iter().any(|x| *x <= 0.) {
            return Err(fail(
                "quadratic Gaussian entropy coefficient underflow requires numerical refinement",
            ));
        }
        let mean =
            std::array::from_fn(|j| prob.iter().zip(&self.features).map(|(p, h)| p * h[j]).sum());
        let mut cov = [0.; 361];
        for i in 0..19 {
            for j in i..19 {
                let value = prob
                    .iter()
                    .zip(&self.features)
                    .map(|(p, h)| p * (h[i] - mean[i]) * (h[j] - mean[j]))
                    .sum();
                cov[i * 19 + j] = value;
                cov[j * 19 + i] = value;
            }
        }
        Ok((prob, mean, cov, max + sum.ln()))
    }
}
