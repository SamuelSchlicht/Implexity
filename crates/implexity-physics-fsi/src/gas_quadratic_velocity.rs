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
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone, Debug)]
pub struct QuadraticBasisInterval {
    pub lower_m_s: f64,
    pub upper_m_s: f64,
    pub coefficient: [f64; 3],
}
#[derive(Clone)]
struct Axis {
    knots: Vec<f64>,
    n: usize,
    rows: Vec<Vec<(usize, f64)>>,
    chol: Vec<[f64; 3]>,
    greville: Vec<f64>,
    quadratic: Vec<f64>,
}
impl Axis {
    fn new(knots: Vec<f64>) -> CaeResult<Self> {
        if knots.len() < 7
            || knots.iter().any(|v| !v.is_finite())
            || knots.windows(2).any(|w| w[1] < w[0])
        {
            return Err(fail("invalid quadratic velocity knots"));
        }
        let n = knots.len() - 3;
        let lo = knots[0];
        let hi = knots[knots.len() - 1];
        if lo >= hi
            || knots[..3].iter().any(|v| *v != lo)
            || knots[knots.len() - 3..].iter().any(|v| *v != hi)
            || knots[2..knots.len() - 2].windows(2).any(|w| w[0] >= w[1])
        {
            return Err(fail(
                "quadratic velocity basis requires clamped ends and distinct interior knots",
            ));
        }
        let mut a = Self {
            greville: (0..n)
                .map(|i| (knots[i + 1] + knots[i + 2]) * 0.5)
                .collect(),
            quadratic: (0..n).map(|i| knots[i + 1] * knots[i + 2]).collect(),
            knots,
            n,
            rows: vec![vec![]; n],
            chol: vec![[0.; 3]; n],
        };
        let mut band = vec![[0.; 3]; n];
        let z = [-0.7745966692414834, 0., 0.7745966692414834];
        let w = [5. / 9., 8. / 9., 5. / 9.];
        for seg in 2..a.knots.len() - 3 {
            let l = a.knots[seg];
            let h = a.knots[seg + 1];
            for q in 0..3 {
                let x = l + (z[q] + 1.) * (h - l) * 0.5;
                let ww = w[q] * (h - l) * 0.5;
                let values = a.values(x);
                for i in 0..n {
                    for j in i.saturating_sub(2)..=i {
                        band[i][i - j] += ww * values[i] * values[j];
                    }
                }
            }
        }
        for i in 0..n {
            for j in i.saturating_sub(2)..=i {
                let mut v = band[i][i - j];
                for k in i.saturating_sub(2).max(j.saturating_sub(2))..j {
                    v -= a.chol[i][i - k] * a.chol[j][j - k];
                }
                if i == j {
                    if !v.is_finite() || v <= 0. {
                        return Err(fail("quadratic basis mass matrix is not positive definite"));
                    }
                    a.chol[i][0] = v.sqrt();
                } else {
                    a.chol[i][i - j] = v / a.chol[j][0];
                }
            }
        }
        for i in 0..n {
            for j in i.saturating_sub(2)..(i + 3).min(n) {
                a.rows[i].push((
                    j,
                    if i >= j {
                        band[i][i - j]
                    } else {
                        band[j][j - i]
                    },
                ));
            }
        }
        Ok(a)
    }
    fn values(&self, x: f64) -> Vec<f64> {
        if x == self.knots[self.knots.len() - 1] {
            let mut out = vec![0.; self.n];
            out[self.n - 1] = 1.;
            return out;
        }
        let mut b = (0..self.knots.len() - 1)
            .map(|i| {
                if x >= self.knots[i] && x < self.knots[i + 1] {
                    1.
                } else {
                    0.
                }
            })
            .collect::<Vec<_>>();
        for p in 1..=2 {
            let mut next = vec![0.; b.len() - 1];
            for i in 0..next.len() {
                let dl = self.knots[i + p] - self.knots[i];
                let dr = self.knots[i + p + 1] - self.knots[i + 1];
                if dl > 0. {
                    next[i] += (x - self.knots[i]) / dl * b[i];
                }
                if dr > 0. {
                    next[i] += (self.knots[i + p + 1] - x) / dr * b[i + 1];
                }
            }
            b = next;
        }
        b
    }
    fn solve_line<S: Scalar>(&self, b: &[S]) -> Vec<S> {
        let mut x = b.to_vec();
        for i in 0..self.n {
            for k in i.saturating_sub(2)..i {
                x[i] = x[i] - x[k] * self.chol[i][i - k];
            }
            x[i] = x[i] / self.chol[i][0];
        }
        for i in (0..self.n).rev() {
            for j in i + 1..(i + 3).min(self.n) {
                x[i] = x[i] - x[j] * self.chol[j][j - i];
            }
            x[i] = x[i] / self.chol[i][0];
        }
        x
    }
}
#[derive(Clone)]
pub struct QuadraticVelocityBasis {
    axes: [Axis; 3],
    dimensions: [usize; 3],
    count: usize,
    reference_speed: f64,
    reproduction: Vec<[f64; 10]>,
    integrated: Vec<[f64; 10]>,
    weights: Vec<f64>,
    features: Vec<[f64; 9]>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuadraticEntropyPolicy {
    pub moment_tolerance: f64,
    pub maximum_iterations: usize,
    pub maximum_covariance_condition: f64,
}
#[derive(Clone, Debug)]
pub struct QuadraticEquilibrium<S = f64> {
    pub coefficient_density: Vec<S>,
    pub maximum_normalized_moment_residual: f64,
    pub newton_iterations: usize,
}
#[derive(Clone, Debug)]
pub struct QuadraticCollision<S = f64> {
    pub coefficient_inventory: Vec<S>,
    pub thermal_bath_energy_j: S,
}
impl QuadraticVelocityBasis {
    pub fn new(knots: [Vec<f64>; 3], reference_speed_m_s: f64) -> CaeResult<Self> {
        if !reference_speed_m_s.is_finite() || reference_speed_m_s <= 0. {
            return Err(fail("invalid quadratic velocity reference speed"));
        }
        let [x, y, z] = knots;
        let axes = [Axis::new(x)?, Axis::new(y)?, Axis::new(z)?];
        let dimensions = axes.each_ref().map(|a| a.n);
        let count = dimensions.iter().try_fold(1usize, |a, n| {
            a.checked_mul(*n)
                .ok_or_else(|| fail("quadratic velocity tensor size overflow"))
        })?;
        let mut b = Self {
            axes,
            dimensions,
            count,
            reference_speed: reference_speed_m_s,
            reproduction: vec![],
            integrated: vec![],
            weights: vec![],
            features: vec![],
        };
        for i in 0..count {
            let c = b.coordinate(i);
            let g = std::array::from_fn::<_, 3, _>(|j| b.axes[j].greville[c[j]]);
            let q = std::array::from_fn::<_, 3, _>(|j| b.axes[j].quadratic[c[j]]);
            b.reproduction.push([
                1.,
                g[0],
                g[1],
                g[2],
                q[0],
                q[1],
                q[2],
                g[0] * g[1],
                g[0] * g[2],
                g[1] * g[2],
            ]);
        }
        b.integrated = vec![[0.; 10]; count];
        for k in 0..10 {
            let row = b.mass_apply(&b.reproduction.iter().map(|r| r[k]).collect::<Vec<_>>())?;
            for i in 0..count {
                b.integrated[i][k] = row[i];
            }
        }
        b.weights = b.integrated.iter().map(|r| r[0]).collect();
        if b.weights.iter().any(|w| !w.is_finite() || *w <= 0.)
            || b.integrated.iter().flatten().any(|v| !v.is_finite())
        {
            return Err(fail("nonfinite quadratic velocity integrated moments"));
        }
        let speed = b.reference_speed;
        b.features = b
            .integrated
            .iter()
            .zip(&b.weights)
            .map(|(r, w)| {
                std::array::from_fn(|j| r[j + 1] / w / if j < 3 { speed } else { speed * speed })
            })
            .collect();
        Ok(b)
    }
    pub fn mass_rows(&self) -> CaeResult<Vec<Vec<(usize, f64)>>> {
        let mut out = Vec::with_capacity(self.count);
        for i in 0..self.count {
            let c = self.coefficient_coordinate(i)?;
            let mut row = Vec::new();
            for &(x, a) in &self.axes[0].rows[c[0]] {
                for &(y, b) in &self.axes[1].rows[c[1]] {
                    for &(z, d) in &self.axes[2].rows[c[2]] {
                        let v = a * b * d;
                        if !v.is_finite() {
                            return Err(fail("quadratic tensor mass entry is nonfinite"));
                        }
                        row.push((self.tensor_index([x, y, z])?, v));
                    }
                }
            }
            row.sort_by_key(|p| p.0);
            out.push(row);
        }
        Ok(out)
    }
    pub fn coefficient_count(&self) -> usize {
        self.count
    }
    pub fn reference_speed_m_s(&self) -> f64 {
        self.reference_speed
    }
    pub fn dimensions(&self) -> [usize; 3] {
        self.dimensions
    }
    pub fn knots(&self, axis: usize) -> CaeResult<&[f64]> {
        self.axes
            .get(axis)
            .map(|a| a.knots.as_slice())
            .ok_or_else(|| fail("invalid velocity axis"))
    }
    pub fn reproduction_vectors(&self) -> &[[f64; 10]] {
        &self.reproduction
    }
    pub fn integrated_moment_rows(&self) -> &[[f64; 10]] {
        &self.integrated
    }
    fn coordinate(&self, i: usize) -> [usize; 3] {
        [
            i / (self.dimensions[1] * self.dimensions[2]),
            i / self.dimensions[2] % self.dimensions[1],
            i % self.dimensions[2],
        ]
    }
    fn index(&self, c: [usize; 3]) -> usize {
        (c[0] * self.dimensions[1] + c[1]) * self.dimensions[2] + c[2]
    }
    pub fn axial_values(&self, axis: usize, x: f64) -> CaeResult<Vec<f64>> {
        let a = self
            .axes
            .get(axis)
            .ok_or_else(|| fail("invalid velocity axis"))?;
        if !x.is_finite() || x < a.knots[0] || x > a.knots[a.knots.len() - 1] {
            return Err(fail("velocity outside basis support"));
        }
        Ok(a.values(x))
    }
    pub fn intervals(&self, axis: usize, index: usize) -> CaeResult<Vec<QuadraticBasisInterval>> {
        let a = self
            .axes
            .get(axis)
            .ok_or_else(|| fail("invalid velocity axis"))?;
        if index >= a.n {
            return Err(fail("invalid axial velocity basis index"));
        }
        let mut out = vec![];
        for i in index..index + 3 {
            let l = a.knots[i];
            let h = a.knots[i + 1];
            if h <= l {
                continue;
            }
            let width = h - l;
            let y0 = a.values(l)[index];
            let ym = a.values(l + width * 0.5)[index];
            let y1 = a.values(h)[index];
            let c2 = 2. * (y1 - 2. * ym + y0) / (width * width);
            let c1 = (y1 - y0) / width - c2 * width;
            out.push(QuadraticBasisInterval {
                lower_m_s: l,
                upper_m_s: h,
                coefficient: [y0, c1, c2],
            });
        }
        Ok(out)
    }
    pub fn mass_apply<S: Scalar>(&self, v: &[S]) -> CaeResult<Vec<S>> {
        if v.len() != self.count {
            return Err(fail("quadratic mass action shape mismatch"));
        }
        let mut old = v.to_vec();
        for axis in 0..3 {
            let mut next = vec![S::zero(); self.count];
            for i in 0..self.count {
                let mut c = self.coordinate(i);
                for &(j, w) in &self.axes[axis].rows[c[axis]] {
                    c[axis] = j;
                    next[i] = next[i] + old[self.index(c)] * w;
                }
            }
            old = next;
        }
        Ok(old)
    }
    pub fn mass_solve<S: Scalar>(&self, v: &[S]) -> CaeResult<Vec<S>> {
        if v.len() != self.count {
            return Err(fail("quadratic mass solve shape mismatch"));
        }
        let mut out = v.to_vec();
        for axis in 0..3 {
            for i in 0..self.count {
                let mut c = self.coordinate(i);
                if c[axis] != 0 {
                    continue;
                }
                let ids = (0..self.dimensions[axis])
                    .map(|j| {
                        c[axis] = j;
                        self.index(c)
                    })
                    .collect::<Vec<_>>();
                let line = ids.iter().map(|&i| out[i]).collect::<Vec<_>>();
                for (&i, x) in ids.iter().zip(self.axes[axis].solve_line(&line)) {
                    out[i] = x;
                }
            }
        }
        Ok(out)
    }
    pub fn weak_inventory<S: GasMassScalar>(&self, a: &[S]) -> CaeResult<Vec<S>> {
        self.check_inventory(a)?;
        self.mass_apply(a)
    }
    pub fn coefficient_moments<S: GasMassScalar>(&self, a: &[S]) -> CaeResult<[S; 10]> {
        self.check_inventory(a)?;
        Ok(std::array::from_fn(|j| {
            a.iter()
                .zip(&self.integrated)
                .fold(S::zero(), |s, (a, r)| s + *a * r[j])
        }))
    }
    pub fn weak_moments<S: GasMassScalar>(&self, u: &[S]) -> CaeResult<[S; 10]> {
        if u.len() != self.count || u.iter().any(|x| !x.finite_first_order()) {
            return Err(fail("quadratic weak moment shape or value"));
        }
        let moments = std::array::from_fn(|j| {
            u.iter()
                .zip(&self.reproduction)
                .fold(S::zero(), |s, (u, r)| s + *u * r[j])
        });
        if moments.iter().any(|x| !x.finite_first_order()) {
            return Err(fail("nonfinite quadratic weak moments"));
        }
        Ok(moments)
    }
    pub fn coefficient_coordinate(&self, index: usize) -> CaeResult<[usize; 3]> {
        if index >= self.count {
            return Err(fail("invalid tensor velocity coefficient index"));
        }
        Ok(self.coordinate(index))
    }
    pub fn tensor_index(&self, coordinate: [usize; 3]) -> CaeResult<usize> {
        if (0..3).any(|j| coordinate[j] >= self.dimensions[j]) {
            return Err(fail("invalid tensor velocity coefficient coordinate"));
        }
        Ok(self.index(coordinate))
    }
    fn check_inventory<S: GasMassScalar>(&self, a: &[S]) -> CaeResult<()> {
        if a.len() != self.count || a.iter().any(|x| !x.finite_first_order() || x.value() < 0.) {
            return Err(fail(
                "quadratic gas coefficient inventory is not finite and nonnegative",
            ));
        }
        Ok(())
    }
    pub fn equilibrium<S: GasMassScalar>(
        &self,
        rho: S,
        velocity: [S; 3],
        rt: S,
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        if !rho.finite_first_order()
            || rho.value() < 0.
            || velocity.iter().any(|v| !v.finite_first_order())
            || !rt.finite_first_order()
            || rt.value() <= 0.
            || !p.moment_tolerance.is_finite()
            || p.moment_tolerance <= 0.
            || p.maximum_iterations == 0
            || !p.maximum_covariance_condition.is_finite()
            || p.maximum_covariance_condition <= 1.
        {
            return Err(fail("invalid quadratic gas entropy domain"));
        }
        if rho.value() == 0. && !rho.has_direction() {
            return Ok(QuadraticEquilibrium {
                coefficient_density: vec![S::zero(); self.count],
                maximum_normalized_moment_residual: 0.,
                newton_iterations: 0,
            });
        }
        let speed = self.reference_speed;
        let u = velocity.map(|v| v / speed);
        let theta = rt / (speed * speed);
        let target = [
            u[0],
            u[1],
            u[2],
            u[0] * u[0] + theta,
            u[1] * u[1] + theta,
            u[2] * u[2] + theta,
            u[0] * u[1],
            u[0] * u[2],
            u[1] * u[2],
        ];
        let mut lambda = [0.; 9];
        for j in 0..3 {
            lambda[j] = u[j].value() / theta.value();
            lambda[j + 3] = -0.5 / theta.value();
        }
        self.entropy_target(rho, target, lambda, p)
    }
    pub fn moment_entropy<S: GasMassScalar>(
        &self,
        moments: [S; 10],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        if moments.iter().any(|x| !x.finite_first_order())
            || moments[0].value() < 0.
            || !p.moment_tolerance.is_finite()
            || p.moment_tolerance <= 0.
            || p.maximum_iterations == 0
            || !p.maximum_covariance_condition.is_finite()
            || p.maximum_covariance_condition <= 1.
        {
            return Err(fail("invalid quadratic gas moment entropy domain"));
        }
        if moments[0].value() == 0. {
            if moments.iter().any(|x| x.value() != 0. || x.has_direction()) {
                return Err(fail(
                    "zero-mass moment entropy requires a fixed vacuum stratum",
                ));
            }
            return Ok(QuadraticEquilibrium {
                coefficient_density: vec![S::zero(); self.count],
                maximum_normalized_moment_residual: 0.,
                newton_iterations: 0,
            });
        }
        let speed = self.reference_speed;
        let target = std::array::from_fn(|j| {
            moments[j + 1] / moments[0] / if j < 3 { speed } else { speed * speed }
        });
        self.entropy_target(moments[0], target, [0.; 9], p)
    }
    pub fn moment_entropy_vjp(
        &self,
        moments: [f64; 10],
        bar: &[f64],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<[f64; 10]> {
        if bar.len() != self.count || bar.iter().any(|x| !x.is_finite()) {
            return Err(fail("quadratic moment entropy cotangent shape or value"));
        }
        let seed = std::array::from_fn(|i| {
            let mut direction = [0.; 10];
            direction[i] = 1.;
            Dual::<10>::new(moments[i], direction)
        });
        let eq = self.moment_entropy(seed, p)?;
        let mut out = [0.; 10];
        for (f, b) in eq.coefficient_density.iter().zip(bar) {
            for j in 0..10 {
                out[j] += f.eps[j] * b;
            }
        }
        if out.iter().any(|x| !x.is_finite()) {
            return Err(fail("nonfinite quadratic moment entropy pullback"));
        }
        Ok(out)
    }
    fn entropy_target<S: GasMassScalar>(
        &self,
        rho: S,
        target: [S; 9],
        mut lambda: [f64; 9],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticEquilibrium<S>> {
        let tv = target.map(|x| x.value());
        for iteration in 0..p.maximum_iterations {
            let (prob, mean, cov, logz) = self.entropy_statistics(&lambda)?;
            let residual = std::array::from_fn::<_, 9, _>(|j| mean[j] - tv[j]);
            let norm = residual.iter().map(|x| x.abs()).fold(0., f64::max);
            let factor = DenseLu::new(
                &DenseMatrix::new(9, 9, cov.to_vec()).map_err(|e| fail(e.to_string()))?,
            )
            .map_err(|_| fail("quadratic entropy target is outside strict realizability"))?;
            if norm <= p.moment_tolerance {
                let mut inverse = vec![0.; 81];
                for j in 0..9 {
                    let mut unit = vec![0.; 9];
                    unit[j] = 1.;
                    let column = factor
                        .solve(&unit, 1, false)
                        .map_err(|e| fail(e.to_string()))?;
                    for i in 0..9 {
                        inverse[i * 9 + j] = column[i];
                    }
                }
                let matrix_norm = (0..9)
                    .map(|i| cov[i * 9..(i + 1) * 9].iter().map(|x| x.abs()).sum::<f64>())
                    .fold(0., f64::max);
                let inverse_norm = (0..9)
                    .map(|i| {
                        inverse[i * 9..(i + 1) * 9]
                            .iter()
                            .map(|x| x.abs())
                            .sum::<f64>()
                    })
                    .fold(0., f64::max);
                if matrix_norm * inverse_norm > p.maximum_covariance_condition {
                    return Err(fail(
                        "quadratic entropy covariance requires velocity refinement",
                    ));
                }
                let change = std::array::from_fn::<_, 9, _>(|i| {
                    (0..9).fold(S::zero(), |s, j| {
                        s + (target[j] - S::from_f64(tv[j])) * inverse[i * 9 + j]
                    })
                });
                let coeff = (0..self.count)
                    .map(|k| {
                        let linear = (0..9).fold(S::zero(), |s, j| {
                            s + change[j] * (self.features[k][j] - mean[j])
                        });
                        rho * (prob[k] / self.weights[k]) * (S::one() + linear)
                    })
                    .collect::<Vec<_>>();
                if coeff
                    .iter()
                    .any(|x| !x.finite_first_order() || x.value() < 0.)
                {
                    return Err(fail("nonfinite quadratic equilibrium derivative"));
                }
                return Ok(QuadraticEquilibrium {
                    coefficient_density: coeff,
                    maximum_normalized_moment_residual: norm,
                    newton_iterations: iteration,
                });
            }
            let step = factor
                .solve(&residual, 1, false)
                .map_err(|e| fail(e.to_string()))?;
            let merit = logz - dot9(lambda, tv);
            let decrement = dot9(residual, std::array::from_fn(|i| step[i]));
            let mut alpha = 1.;
            let mut accepted = None;
            for _ in 0..48 {
                let trial = std::array::from_fn(|i| lambda[i] - alpha * step[i]);
                if let Ok((_, trial_mean, _, lz)) = self.entropy_statistics(&trial) {
                    let trial_merit = lz - dot9(trial, tv);
                    let trial_norm = (0..9)
                        .map(|j| (trial_mean[j] - tv[j]).abs())
                        .fold(0., f64::max);
                    let roundoff = 16.
                        * f64::EPSILON
                        * (1.
                            + logz.abs()
                            + dot9(lambda, tv).abs()
                            + lz.abs()
                            + dot9(trial, tv).abs());
                    if trial_merit <= merit - 1e-4 * alpha * decrement
                        || (trial_norm < norm && trial_merit <= merit + roundoff)
                    {
                        accepted = Some(trial);
                        break;
                    }
                }
                alpha *= 0.5;
            }
            lambda =
                accepted.ok_or_else(|| fail("quadratic entropy solve failed admissibility"))?;
        }
        Err(fail("quadratic entropy moment solve did not converge"))
    }
    pub fn equilibrium_moment_direction<S: GasMassScalar>(
        &self,
        equilibrium: &[S],
        moments: [S; 10],
        direction: [S; 10],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<Vec<S>> {
        if equilibrium.len() != self.count
            || equilibrium
                .iter()
                .any(|v| !v.finite_first_order() || v.value() <= 0.)
            || moments
                .iter()
                .chain(&direction)
                .any(|v| !v.finite_first_order())
            || moments[0].value() <= 0.
            || !p.moment_tolerance.is_finite()
            || p.moment_tolerance <= 0.
            || !p.maximum_covariance_condition.is_finite()
            || p.maximum_covariance_condition <= 1.
        {
            return Err(fail("invalid quadratic entropy moment-gradient domain"));
        }
        let rho = moments[0];
        let speed = self.reference_speed;
        let actual = self.coefficient_moments(equilibrium)?;
        for j in 0..10 {
            let scale = rho.value()
                * if j == 0 {
                    1.
                } else if j <= 3 {
                    speed
                } else {
                    speed * speed
                };
            if !scale.is_finite()
                || scale <= 0.
                || (actual[j].value() - moments[j].value()).abs() > p.moment_tolerance * scale
            {
                return Err(fail(
                    "quadratic entropy gradient state does not match its moments",
                ));
            }
        }
        let prob = equilibrium
            .iter()
            .zip(&self.weights)
            .map(|(g, w)| *g * *w / rho)
            .collect::<Vec<_>>();
        let mean = std::array::from_fn::<_, 9, _>(|j| {
            prob.iter()
                .zip(&self.features)
                .fold(S::zero(), |v, (a, f)| v + *a * f[j])
        });
        let cov = std::array::from_fn::<_, 81, _>(|ij| {
            let (i, j) = (ij / 9, ij % 9);
            prob.iter()
                .zip(&self.features)
                .fold(S::zero(), |v, (a, f)| {
                    v + *a * (S::from_f64(f[i]) - mean[i]) * (S::from_f64(f[j]) - mean[j])
                })
        });
        let target = std::array::from_fn::<_, 9, _>(|j| {
            (direction[j + 1] - moments[j + 1] / rho * direction[0])
                / rho
                / if j < 3 { speed } else { speed * speed }
        });
        let cv = cov.map(|v| v.value());
        let rhs = target.map(|v| v.value());
        let factor =
            DenseLu::new(&DenseMatrix::new(9, 9, cv.to_vec()).map_err(|e| fail(e.to_string()))?)
                .map_err(|e| fail(e.to_string()))?;
        let value = factor
            .solve(&rhs, 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let mut inverse = vec![0.; 81];
        for j in 0..9 {
            let mut unit = vec![0.; 9];
            unit[j] = 1.;
            let col = factor
                .solve(&unit, 1, false)
                .map_err(|e| fail(e.to_string()))?;
            for i in 0..9 {
                inverse[i * 9 + j] = col[i];
            }
        }
        let cn = (0..9)
            .map(|i| cv[i * 9..(i + 1) * 9].iter().map(|v| v.abs()).sum::<f64>())
            .fold(0., f64::max);
        let ci = (0..9)
            .map(|i| {
                inverse[i * 9..(i + 1) * 9]
                    .iter()
                    .map(|v| v.abs())
                    .sum::<f64>()
            })
            .fold(0., f64::max);
        if !cn.is_finite() || !ci.is_finite() || cn * ci > p.maximum_covariance_condition {
            return Err(fail(
                "quadratic entropy gradient covariance requires refinement",
            ));
        }
        let change = std::array::from_fn::<_, 9, _>(|i| {
            S::from_f64(value[i])
                + (0..9).fold(S::zero(), |sum, j| {
                    let linear = target[j]
                        - S::from_f64(rhs[j])
                        - (0..9).fold(S::zero(), |v, k| {
                            v + (cov[j * 9 + k] - S::from_f64(cv[j * 9 + k])) * value[k]
                        });
                    sum + linear * inverse[i * 9 + j]
                })
        });
        let out = (0..self.count)
            .map(|k| {
                equilibrium[k]
                    * (direction[0] / rho
                        + (0..9).fold(S::zero(), |v, j| {
                            v + (S::from_f64(self.features[k][j]) - mean[j]) * change[j]
                        }))
            })
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("quadratic entropy moment-gradient is nonfinite"));
        }
        Ok(out)
    }
    pub fn equilibrium_vjp(
        &self,
        rho: f64,
        velocity: [f64; 3],
        rt: f64,
        bar: &[f64],
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<(f64, [f64; 3], f64)> {
        if bar.len() != self.count || bar.iter().any(|x| !x.is_finite()) {
            return Err(fail("quadratic entropy cotangent shape or value"));
        }
        let seed = |value: f64, index: usize| {
            let mut direction = [0.; 5];
            direction[index] = 1.;
            Dual::<5>::new(value, direction)
        };
        let eq = self.equilibrium(
            seed(rho, 0),
            std::array::from_fn(|j| seed(velocity[j], j + 1)),
            seed(rt, 4),
            p,
        )?;
        let mut out = [0.; 5];
        for (f, b) in eq.coefficient_density.iter().zip(bar) {
            for j in 0..5 {
                out[j] += f.eps[j] * b;
            }
        }
        if out.iter().any(|x| !x.is_finite()) {
            return Err(fail("nonfinite quadratic entropy pullback"));
        }
        Ok((out[0], [out[1], out[2], out[3]], out[4]))
    }
    fn entropy_statistics(
        &self,
        lambda: &[f64; 9],
    ) -> CaeResult<(Vec<f64>, [f64; 9], [f64; 81], f64)> {
        let logs = self
            .features
            .iter()
            .zip(&self.weights)
            .map(|(h, w)| w.ln() + dot9(*h, *lambda))
            .collect::<Vec<_>>();
        let max = logs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let sum = logs.iter().map(|x| (x - max).exp()).sum::<f64>();
        if !max.is_finite() || !sum.is_finite() || sum <= 0. {
            return Err(fail("nonfinite quadratic entropy normalization"));
        }
        let prob = logs
            .iter()
            .map(|x| (x - max).exp() / sum)
            .collect::<Vec<_>>();
        if prob.iter().any(|x| *x <= 0.) {
            return Err(fail(
                "quadratic entropy coefficient underflow requires numerical refinement",
            ));
        }
        let mean =
            std::array::from_fn(|j| prob.iter().zip(&self.features).map(|(p, h)| p * h[j]).sum());
        let cov = std::array::from_fn(|ij| {
            let i = ij / 9;
            let j = ij % 9;
            prob.iter()
                .zip(&self.features)
                .map(|(p, h)| p * (h[i] - mean[i]) * (h[j] - mean[j]))
                .sum()
        });
        Ok((prob, mean, cov, max + sum.ln()))
    }
    pub fn collide<S: GasMassScalar>(
        &self,
        a: &[S],
        volume: S,
        rt: S,
        viscosity: S,
        step: S,
        p: &QuadraticEntropyPolicy,
    ) -> CaeResult<QuadraticCollision<S>> {
        self.check_inventory(a)?;
        if [volume, rt, viscosity, step]
            .iter()
            .any(|v| !v.finite_first_order())
            || volume.value() < 0.
            || rt.value() <= 0.
            || viscosity.value() <= 0.
            || step.value() < 0.
        {
            return Err(fail("invalid quadratic gas collision domain"));
        }
        if volume.value() == 0. {
            if volume.has_direction() || a.iter().any(|v| v.value() != 0. || v.has_direction()) {
                return Err(fail(
                    "closed quadratic gas cell must have fixed zero inventory",
                ));
            }
            return Ok(QuadraticCollision {
                coefficient_inventory: a.to_vec(),
                thermal_bath_energy_j: S::zero(),
            });
        }
        let old = self.coefficient_moments(a)?;
        if old[0].value() == 0. {
            return Ok(QuadraticCollision {
                coefficient_inventory: a.to_vec(),
                thermal_bath_energy_j: S::zero(),
            });
        }
        let rho = old[0] / volume;
        let velocity = std::array::from_fn(|j| old[j + 1] / old[0]);
        let eq = self.equilibrium(rho, velocity, rt, p)?;
        let exponent = rho * rt / viscosity * step;
        if !exponent.finite_first_order() {
            return Err(fail(
                "quadratic collision exposure exceeds finite derivative domain",
            ));
        }
        let fraction = -(-exponent).exp_m1();
        let decay = (-exponent).exp();
        let next = a
            .iter()
            .zip(eq.coefficient_density)
            .map(|(a, f)| decay * *a + fraction * volume * f)
            .collect::<Vec<_>>();
        self.check_inventory(&next)?;
        let after = self.coefficient_moments(&next)?;
        let heat = (old[4] + old[5] + old[6] - after[4] - after[5] - after[6]) * 0.5;
        if !heat.finite_first_order() {
            return Err(fail("nonfinite quadratic collision bath ledger"));
        }
        Ok(QuadraticCollision {
            coefficient_inventory: next,
            thermal_bath_energy_j: heat,
        })
    }
}
fn dot9(a: [f64; 9], b: [f64; 9]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
