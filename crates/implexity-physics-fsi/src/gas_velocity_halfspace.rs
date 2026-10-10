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

use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfspaceMeasure {
    Density,
    PositiveSpeed,
}
#[derive(Clone, Debug)]
pub struct VelocityHalfspaceMoments<S> {
    pub degrees: [usize; 3],
    pub moments: Vec<S>,
}
impl<S: Scalar> VelocityHalfspaceMoments<S> {
    pub fn index(&self, exponent: [usize; 3]) -> CaeResult<usize> {
        if (0..3).any(|a| exponent[a] > self.degrees[a]) {
            return Err(fail(
                "velocity halfspace moment exponent outside declared degrees",
            ));
        }
        Ok(
            (exponent[0] * (self.degrees[1] + 1) + exponent[1]) * (self.degrees[2] + 1)
                + exponent[2],
        )
    }
}
#[derive(Clone, Copy)]
struct ClipPoint<S> {
    coordinate: [S; 2],
    threshold: Option<f64>,
}
fn clip<S: TransientGasScalar>(
    poly: &[ClipPoint<S>],
    f: impl Fn(ClipPoint<S>) -> S,
    _speed: bool,
    exclude_flat_boundary: bool,
    boundary_threshold: f64,
) -> CaeResult<Vec<ClipPoint<S>>> {
    if exclude_flat_boundary && poly.iter().all(|p| f(*p).value() == 0.) {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let fa = f(a);
        let fb = f(b);
        if !fa.finite_first_order() || !fb.finite_first_order() {
            return Err(fail("nonfinite velocity halfspace clipping value"));
        }
        if fa.value() >= 0. {
            out.push(a);
        }
        if (fa.value() < 0. && fb.value() > 0.) || (fb.value() < 0. && fa.value() > 0.) {
            let t = fa / (fa - fb);
            out.push(ClipPoint {
                coordinate: [
                    a.coordinate[0] + t * (b.coordinate[0] - a.coordinate[0]),
                    a.coordinate[1] + t * (b.coordinate[1] - a.coordinate[1]),
                ],
                threshold: Some(boundary_threshold),
            });
        }
    }
    Ok(out)
}
fn regions<S: TransientGasScalar>(
    square: &[ClipPoint<S>],
    threshold: impl Fn(ClipPoint<S>) -> S + Copy,
    az: S,
    speed: bool,
) -> CaeResult<(Vec<ClipPoint<S>>, Vec<ClipPoint<S>>)> {
    if !speed {
        let flat0 = square.iter().all(|p| threshold(*p).value() == 0.);
        let flat1 = square.iter().all(|p| threshold(*p).value() == 1.);
        if flat0 || flat1 {
            let full = (flat0 && az.value() > 0.) || (flat1 && az.value() < 0.);
            return Ok(if full {
                (vec![], square.to_vec())
            } else {
                (square.to_vec(), vec![])
            });
        }
    }
    let partial = clip(
        &clip(square, threshold, speed, false, 0.)?,
        |p| S::one() - threshold(p),
        speed,
        false,
        1.,
    )?;
    let full = if az.value() > 0. {
        clip(square, |p| -threshold(p), speed, true, 0.)?
    } else {
        clip(square, |p| threshold(p) - S::one(), speed, true, 1.)?
    };
    Ok((partial, full))
}
fn factorial(n: usize) -> f64 {
    (2..=n).fold(1., |v, k| v * k as f64)
}
fn integer_power<S: Scalar>(x: S, n: usize) -> S {
    (0..n).fold(S::one(), |v, _| v * x)
}
fn binomial(n: usize, k: usize) -> f64 {
    factorial(n) / (factorial(k) * factorial(n - k))
}
fn beta(a: usize, b: usize) -> f64 {
    factorial(a - 1) * factorial(b - 1) / factorial(a + b - 1)
}
fn gauss(n: usize) -> Vec<(f64, f64)> {
    let mut result = Vec::with_capacity(n);
    for i in 0..n {
        let mut x = (std::f64::consts::PI * (i as f64 + 0.75) / (n as f64 + 0.5)).cos();
        for _ in 0..30 {
            let mut old = 1.;
            let mut p = x;
            for k in 2..=n {
                let next = ((2 * k - 1) as f64 * x * p - (k - 1) as f64 * old) / k as f64;
                old = p;
                p = next;
            }
            let dp = n as f64 * (x * p - old) / (x * x - 1.);
            let dx = p / dp;
            x -= dx;
            if dx.abs() < 2e-16 {
                break;
            }
        }
        let mut old = 1.;
        let mut p = x;
        for k in 2..=n {
            let next = ((2 * k - 1) as f64 * x * p - (k - 1) as f64 * old) / k as f64;
            old = p;
            p = next;
        }
        let dp = n as f64 * (x * p - old) / (x * x - 1.);
        let w = 1. / ((1. - x * x) * dp * dp);
        result.push(((x + 1.) * 0.5, w));
    }
    result
}
fn bernstein<S: Scalar>(x: S, complement: S, n: usize) -> Vec<S> {
    (0..=n)
        .map(|i| integer_power(x, i) * integer_power(complement, n - i) * binomial(n, i))
        .collect()
}
fn individual_density_domain<S: TransientGasScalar>(
    bounds: [[f64; 2]; 3],
    normal: [S; 3],
    offset: S,
    measure: HalfspaceMeasure,
) -> CaeResult<()> {
    if measure == HalfspaceMeasure::Density {
        for i in 0..8 {
            let value =
                offset + (0..3).fold(S::zero(), |v, j| v + normal[j] * bounds[j][(i >> j) & 1]);
            if value.value() == 0. && value.has_direction() {
                return Err(fail(
                    "individual density-box derivative requires a coherent continuous-piece assembly at incidence",
                ));
            }
        }
    }
    Ok(())
}
pub fn positive_halfspace_bernstein_moments<S: TransientGasScalar>(
    bounds: [[f64; 2]; 3],
    normal: [S; 3],
    offset: S,
    degrees: [usize; 3],
    measure: HalfspaceMeasure,
) -> CaeResult<VelocityHalfspaceMoments<S>> {
    individual_density_domain(bounds, normal, offset, measure)?;
    bernstein_moments(bounds, normal, offset, degrees, measure)
}
pub(crate) fn continuous_piece_bernstein_moments<S: TransientGasScalar>(
    bounds: [[f64; 2]; 3],
    normal: [S; 3],
    offset: S,
    degrees: [usize; 3],
    measure: HalfspaceMeasure,
) -> CaeResult<VelocityHalfspaceMoments<S>> {
    bernstein_moments(bounds, normal, offset, degrees, measure)
}
fn bernstein_moments<S: TransientGasScalar>(
    bounds: [[f64; 2]; 3],
    normal: [S; 3],
    offset: S,
    degrees: [usize; 3],
    measure: HalfspaceMeasure,
) -> CaeResult<VelocityHalfspaceMoments<S>> {
    let width = std::array::from_fn::<_, 3, _>(|i| bounds[i][1] - bounds[i][0]);
    if bounds.iter().flatten().any(|v| !v.is_finite())
        || width.iter().any(|v| !v.is_finite() || *v <= 0.)
        || normal
            .iter()
            .chain(std::iter::once(&offset))
            .any(|v| !v.finite_first_order())
        || degrees.iter().any(|v| *v > 8)
    {
        return Err(fail("invalid Bernstein velocity halfspace data"));
    }
    let volume = width.iter().product::<f64>();
    let a = std::array::from_fn::<_, 3, _>(|i| normal[i] * width[i]);
    let b = offset + (0..3).fold(S::zero(), |v, i| v + normal[i] * bounds[i][0]);
    if !volume.is_finite()
        || volume <= 0.
        || !b.finite_first_order()
        || a.iter().any(|v| !v.finite_first_order())
    {
        return Err(fail("nonfinite Bernstein velocity plane or measure"));
    }
    let mut result = VelocityHalfspaceMoments {
        degrees,
        moments: vec![S::zero(); degrees.iter().map(|v| v + 1).product()],
    };
    let mut lower = b;
    let mut upper = b;
    for v in a {
        if v.value() < 0. {
            lower = lower + v;
        } else {
            upper = upper + v;
        }
    }
    if upper.value() < 0. {
        return Ok(result);
    }
    let denominator = degrees.iter().map(|v| (v + 1) as f64).product::<f64>();
    if lower.value() > 0. {
        for i in 0..=degrees[0] {
            for j in 0..=degrees[1] {
                for k in 0..=degrees[2] {
                    let ex = [i, j, k];
                    let v = if measure == HalfspaceMeasure::Density {
                        S::from_f64(volume / denominator)
                    } else {
                        (b + (0..3).fold(S::zero(), |v, d| {
                            v + a[d] * (ex[d] + 1) as f64 / (degrees[d] + 2) as f64
                        })) * volume
                            / denominator
                    };
                    if !v.finite_first_order() || v.value() < 0. {
                        return Err(fail("invalid full Bernstein velocity moment"));
                    }
                    let index = result.index(ex)?;
                    result.moments[index] = v;
                }
            }
        }
        return Ok(result);
    }
    let vertical = (0..3)
        .max_by(|i, j| a[*i].value().abs().total_cmp(&a[*j].value().abs()))
        .unwrap();
    if a[vertical].value() == 0. {
        return Err(fail("Bernstein velocity halfspace normal is zero"));
    }
    let axial = (0..3).filter(|i| *i != vertical).collect::<Vec<_>>();
    let az = a[vertical];
    let speed = measure == HalfspaceMeasure::PositiveSpeed;
    let threshold = |p: ClipPoint<S>| {
        p.threshold.map(S::from_f64).unwrap_or_else(|| {
            -(b + a[axial[0]] * p.coordinate[0] + a[axial[1]] * p.coordinate[1]) / az
        })
    };
    let square = [
        [S::zero(), S::zero()],
        [S::one(), S::zero()],
        [S::one(), S::one()],
        [S::zero(), S::one()],
    ]
    .map(|coordinate| ClipPoint {
        coordinate,
        threshold: None,
    });
    let (partial, full) = regions(&square, threshold, az, speed)?;
    let quadrature = gauss((degrees.iter().sum::<usize>() + 5) / 2);
    let da = degrees[axial[0]];
    let db = degrees[axial[1]];
    let dc = degrees[vertical];
    for (poly, is_partial) in [(&partial, true), (&full, false)] {
        for j in 1..poly.len().saturating_sub(1) {
            let vertices = [poly[0], poly[j], poly[j + 1]];
            let p = vertices.map(|v| v.coordinate);
            let mut det = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
            if det.value() == 0. {
                continue;
            }
            if det.value() < 0. {
                det = -det;
            }
            let t = vertices.map(threshold);
            let d = t.map(|v| S::one() - v);
            for &(u, wu) in &quadrature {
                for &(v, wv) in &quadrature {
                    let weights = [(1. - u) * (1. - v), u, (1. - u) * v];
                    let interpolate =
                        |values: [S; 3]| (0..3).fold(S::zero(), |s, i| s + values[i] * weights[i]);
                    let x = interpolate(p.map(|p| p[0]));
                    let xc = interpolate(p.map(|p| S::one() - p[0]));
                    let y = interpolate(p.map(|p| p[1]));
                    let yc = interpolate(p.map(|p| S::one() - p[1]));
                    let bx = bernstein(x, xc, da);
                    let by = bernstein(y, yc, db);
                    let tt = interpolate(t);
                    let dd = interpolate(d);
                    let weight = det * ((1. - u) * wu * wv * volume);
                    let mut z = vec![S::zero(); dc + 1];
                    for c in 0..=dc {
                        z[c] = if is_partial && az.value() > 0. {
                            (0..=c).fold(S::zero(), |sum, k| {
                                sum + integer_power(tt, c - k)
                                    * integer_power(dd, dc - c + k + if speed { 2 } else { 1 })
                                    * binomial(dc, c)
                                    * binomial(c, k)
                                    * beta(k + if speed { 2 } else { 1 }, dc - c + 1)
                            })
                        } else if is_partial {
                            (0..=dc - c).fold(S::zero(), |sum, k| {
                                sum + integer_power(dd, dc - c - k)
                                    * integer_power(tt, c + k + if speed { 2 } else { 1 })
                                    * binomial(dc, c)
                                    * binomial(dc - c, k)
                                    * beta(c + 1, k + if speed { 2 } else { 1 })
                            })
                        } else if !speed {
                            S::from_f64(1. / (dc + 1) as f64)
                        } else if az.value() > 0. {
                            (S::from_f64((c + 1) as f64 / (dc + 2) as f64) - tt) / (dc + 1) as f64
                        } else {
                            (tt - S::one() + S::from_f64((dc - c + 1) as f64 / (dc + 2) as f64))
                                / (dc + 1) as f64
                        };
                        if speed {
                            z[c] = z[c] * if az.value() > 0. { az } else { -az };
                        }
                    }
                    for i in 0..=da {
                        for j in 0..=db {
                            for k in 0..=dc {
                                let mut ex = [0; 3];
                                ex[axial[0]] = i;
                                ex[axial[1]] = j;
                                ex[vertical] = k;
                                let index = result.index(ex)?;
                                result.moments[index] =
                                    result.moments[index] + weight * bx[i] * by[j] * z[k];
                            }
                        }
                    }
                }
            }
        }
    }
    if result
        .moments
        .iter()
        .any(|v| !v.finite_first_order() || v.value() < 0.)
    {
        return Err(fail("nonfinite or negative Bernstein velocity integral"));
    }
    Ok(result)
}
