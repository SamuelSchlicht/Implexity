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

use crate::gas_gap_geometry::GasGapGeometry;
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};

mod sealed {
    pub trait Sealed {}
    impl Sealed for f64 {}
    impl<const N: usize> Sealed for implexity_ad::Dual<N> {}
}
pub trait GasBoxScalar: Scalar + sealed::Sealed {
    fn has_direction(self) -> bool;
    fn finite(self) -> bool;
}
impl GasBoxScalar for f64 {
    fn has_direction(self) -> bool {
        false
    }
    fn finite(self) -> bool {
        self.is_finite()
    }
}
impl<const N: usize> GasBoxScalar for Dual<N> {
    fn has_direction(self) -> bool {
        self.eps.iter().any(|v| *v != 0.)
    }
    fn finite(self) -> bool {
        self.re.is_finite() && self.eps.iter().all(|v| v.is_finite())
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasBoxQuadraturePolicy {
    pub absolute_tolerance_m3: f64,
    pub relative_tolerance: f64,
    pub maximum_depth: usize,
    pub maximum_intervals: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct GasBoxQuadrature<S> {
    pub volume_m3: S,
    pub estimated_absolute_error_m3: f64,
    pub estimated_gradient_error_m2: f64,
    pub intervals: usize,
}

fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn det<S: Scalar>(a: [S; 3], b: [S; 3], c: [S; 3]) -> S {
    dot(cross(a, b), c)
}
fn polynomial<S: Scalar>(c: [S; 4], t: f64) -> S {
    ((c[3] * t + c[2]) * t + c[1]) * t + c[0]
}
fn roots(c: [f64; 4]) -> Vec<f64> {
    let scale = c.iter().map(|v| v.abs()).fold(0., f64::max);
    if scale == 0. {
        return Vec::new();
    }
    let c = c.map(|v| v / scale);
    let value = |t: f64| polynomial(c, t);
    let mut critical = vec![0., 1.];
    let a = 3. * c[3];
    let b = 2. * c[2];
    let cc = c[1];
    if a == 0. {
        if b != 0. {
            let t = -cc / b;
            if t > 0. && t < 1. {
                critical.push(t);
            }
        }
    } else {
        let d = b * b - 4. * a * cc;
        if d >= 0. {
            let q = -0.5 * (b + if b >= 0. { d.sqrt() } else { -d.sqrt() });
            let candidates = if q == 0. {
                [-b / (2. * a), -b / (2. * a)]
            } else {
                [q / a, cc / q]
            };
            for t in candidates {
                if t > 0. && t < 1. {
                    critical.push(t);
                }
            }
        }
    }
    critical.sort_by(f64::total_cmp);
    let mut out = Vec::new();
    for t in &critical {
        if *t > 0. && *t < 1. && value(*t).abs() <= 32. * f64::EPSILON {
            out.push(*t);
        }
    }
    for interval in critical.windows(2) {
        let mut a = interval[0];
        let mut b = interval[1];
        let va = value(a);
        let vb = value(b);
        if va == 0. || vb == 0. || va.signum() == vb.signum() {
            continue;
        }
        for _ in 0..64 {
            let t = (a + b) * 0.5;
            let v = value(t);
            if v == 0. {
                a = t;
                b = t;
                break;
            }
            if v.signum() == va.signum() {
                a = t;
            } else {
                b = t;
            }
        }
        let t = (a + b) * 0.5;
        if t > 0. && t < 1. {
            out.push(t);
        }
    }
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() <= 64. * f64::EPSILON);
    out
}

fn breakpoints(v: [[f64; 3]; 6], bounds: [[f64; 3]; 2]) -> CaeResult<Vec<f64>> {
    let z = 0.;
    let o = 1.;
    let mut planes = vec![
        ([-o, -o, o], [z; 3]),
        ([o, z, z], [z; 3]),
        ([z, o, z], [z; 3]),
    ];
    for a in 0..3 {
        let low = [v[1][a] - v[0][a], v[2][a] - v[0][a], v[0][a] - bounds[0][a]];
        let delta = [
            v[4][a] - v[3][a] - low[0],
            v[5][a] - v[3][a] - low[1],
            v[3][a] - v[0][a],
        ];
        planes.push((low, delta));
        planes.push((
            [-low[0], -low[1], bounds[1][a] - v[0][a]],
            delta.map(|q| -q),
        ));
    }
    let mut out = vec![z, o];
    for i in 0..planes.len() {
        for j in i + 1..planes.len() {
            for k in j + 1..planes.len() {
                let (a, da) = planes[i];
                let (b, db) = planes[j];
                let (c, dc) = planes[k];
                let p = [
                    det(a, b, c),
                    det(da, b, c) + det(a, db, c) + det(a, b, dc),
                    det(da, db, c) + det(da, b, dc) + det(a, db, dc),
                    det(da, db, dc),
                ];
                if p.iter().any(|v| !v.is_finite()) {
                    return Err(fail("nonfinite gas box topology polynomial"));
                }
                out.extend(roots(p));
            }
        }
    }
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() <= 64. * f64::EPSILON);
    Ok(out)
}

fn slice<S: GasBoxScalar>(v: [[S; 3]; 6], bounds: [[f64; 3]; 2], t: S) -> CaeResult<S> {
    let r: [[S; 3]; 3] =
        std::array::from_fn(|i| std::array::from_fn(|a| v[i][a] + t * (v[i + 3][a] - v[i][a])));
    let a = sub(r[1], r[0]);
    let b = sub(r[2], r[0]);
    let mut polygon = vec![
        [S::from_f64(0.), S::from_f64(0.)],
        [S::from_f64(1.), S::from_f64(0.)],
        [S::from_f64(0.), S::from_f64(1.)],
    ];
    for axis in 0..3 {
        for (bound, sign) in [(bounds[0][axis], 1.), (bounds[1][axis], -1.)] {
            if polygon.is_empty() {
                return Ok(S::from_f64(0.));
            }
            let value = |p: [S; 2]| (r[0][axis] + a[axis] * p[0] + b[axis] * p[1] - bound) * sign;
            let mut next = Vec::new();
            for i in 0..polygon.len() {
                let p = polygon[i];
                let q = polygon[(i + 1) % polygon.len()];
                let fp = value(p);
                let fq = value(q);
                if !fp.finite() || !fq.finite() {
                    return Err(fail("nonfinite gas box clipping root"));
                }
                if fp.value() >= 0. {
                    next.push(p);
                }
                if (fp.value() >= 0.) != (fq.value() >= 0.) {
                    let alpha = fp / (fp - fq);
                    next.push(std::array::from_fn(|i| p[i] + alpha * (q[i] - p[i])));
                }
            }
            polygon = next;
        }
    }
    if polygon.len() < 3 {
        return Ok(S::from_f64(0.));
    }
    let mut area = S::from_f64(0.);
    let mut moment = [S::from_f64(0.); 2];
    for i in 0..polygon.len() {
        let p = polygon[i];
        let q = polygon[(i + 1) % polygon.len()];
        let determinant = p[0] * q[1] - p[1] * q[0];
        area += determinant * 0.5;
        for axis in 0..2 {
            moment[axis] += (p[axis] + q[axis]) * determinant / 6.;
        }
    }
    if area.value() == 0. {
        return Ok(S::from_f64(0.));
    }
    if area.value() < 0. || !area.finite() {
        return Err(fail("inverted gas box polygon"));
    }
    let lambda = [moment[0] / area, moment[1] / area];
    let d0 = sub(v[3], v[0]);
    let d1 = sub(v[4], v[1]);
    let d2 = sub(v[5], v[2]);
    let d =
        std::array::from_fn(|i| d0[i] + lambda[0] * (d1[i] - d0[i]) + lambda[1] * (d2[i] - d0[i]));
    let result = area * dot(cross(a, b), d);
    if !result.finite() || result.value() < 0. {
        return Err(fail("invalid gas box slice volume"));
    }
    Ok(result)
}

fn rule<S: GasBoxScalar>(
    v: [[S; 3]; 6],
    bounds: [[f64; 3]; 2],
    a: S,
    b: S,
    fine: bool,
) -> CaeResult<S> {
    let points: &[(f64, f64)] = if fine {
        &[
            (0.9602898564975363, 0.1012285362903763),
            (0.7966664774136267, 0.2223810344533745),
            (0.525532409916329, 0.3137066458778873),
            (0.1834346424956498, 0.362683783378362),
        ]
    } else {
        &[
            (0.8611363115940526, 0.3478548451374538),
            (0.3399810435848563, 0.6521451548625461),
        ]
    };
    let middle = (a + b) * 0.5;
    let half = (b - a) * 0.5;
    let mut out = S::from_f64(0.);
    for (node, weight) in points {
        out += (slice(v, bounds, middle - half * *node)?
            + slice(v, bounds, middle + half * *node)?)
            * *weight;
    }
    Ok(out * half)
}

pub fn integrate_open_volume<S: GasBoxScalar>(
    gap: &GasGapGeometry<S>,
    bounds: [[f64; 3]; 2],
    policy: GasBoxQuadraturePolicy,
) -> CaeResult<GasBoxQuadrature<S>> {
    gap.require_open_derivatives()?;
    if bounds.iter().flatten().any(|v| !v.is_finite())
        || (0..3).any(|a| bounds[0][a] >= bounds[1][a])
        || !policy.absolute_tolerance_m3.is_finite()
        || policy.absolute_tolerance_m3 < 0.
        || !policy.relative_tolerance.is_finite()
        || policy.relative_tolerance < 0.
        || policy.maximum_depth == 0
        || policy.maximum_intervals == 0
    {
        return Err(fail("invalid gas box quadrature policy"));
    }
    let checked = crate::gas_gap_geometry::cell_geometry(
        gap.geometry.vertices_m,
        f64::MIN_POSITIVE,
        f64::MAX,
    )?;
    if checked.state != gap.state
        || checked.geometry.volume_m3.value() != gap.geometry.volume_m3.value()
    {
        return Err(fail("stale gas box geometry"));
    }
    let faces: &[&[usize]] = &[
        &[0, 1, 2],
        &[3, 4, 5],
        &[0, 1, 4, 3],
        &[1, 2, 5, 4],
        &[2, 0, 3, 5],
    ];
    for axis in 0..3 {
        for bound in [bounds[0][axis], bounds[1][axis]] {
            for face in faces {
                if face
                    .iter()
                    .all(|i| gap.geometry.vertices_m[*i][axis].value() == bound)
                    && face
                        .iter()
                        .any(|i| gap.geometry.vertices_m[*i][axis].has_direction())
                {
                    return Err(fail(
                        "coincident gas box face requires a one-sided derivative",
                    ));
                }
            }
        }
    }
    if gap
        .geometry
        .vertices_m
        .iter()
        .flatten()
        .any(|v| v.has_direction())
    {
        let vertices = gap.geometry.vertices_m.map(|p| p.map(|v| v.value()));
        let primal_gap =
            crate::gas_gap_geometry::cell_geometry(vertices, f64::MIN_POSITIVE, f64::MAX)?;
        let primal = integrate_open_volume(&primal_gap, bounds, policy)?;
        let (gradient, gradient_error, extra_intervals) = shape_gradient(vertices, bounds, policy)?;
        let inputs = gap
            .geometry
            .vertices_m
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let value = S::lift(primal.volume_m3, &inputs, &gradient, &vec![0.; 18 * 18]);
        if !value.finite() {
            return Err(fail("nonfinite gas box shape derivative"));
        }
        return Ok(GasBoxQuadrature {
            volume_m3: value,
            estimated_absolute_error_m3: primal.estimated_absolute_error_m3,
            estimated_gradient_error_m2: gradient_error,
            intervals: primal.intervals + extra_intervals,
        });
    }
    let tolerance =
        policy.absolute_tolerance_m3 + policy.relative_tolerance * gap.geometry.volume_m3.value();
    if !tolerance.is_finite() || tolerance <= 0. {
        return Err(fail("gas box quadrature needs a positive finite tolerance"));
    }
    let breaks = breakpoints(
        gap.geometry.vertices_m.map(|v| v.map(|x| x.value())),
        bounds,
    )?
    .into_iter()
    .map(S::from_f64)
    .collect::<Vec<_>>();
    let mut stack = breaks
        .windows(2)
        .map(|s| (s[0], s[1], 0usize))
        .collect::<Vec<_>>();
    let mut volume = S::from_f64(0.);
    let mut error = 0.;
    let mut intervals = 0;
    while let Some((a, b, depth)) = stack.pop() {
        intervals += 1;
        if intervals > policy.maximum_intervals {
            return Err(fail("gas box quadrature interval budget exhausted"));
        }
        let fine = rule(gap.geometry.vertices_m, bounds, a, b, true)?;
        let coarse = rule(gap.geometry.vertices_m, bounds, a, b, false)?;
        let e = (fine.value() - coarse.value()).abs();
        if e <= tolerance * (b.value() - a.value()) {
            volume += fine;
            error += e;
        } else {
            if depth == policy.maximum_depth {
                return Err(fail("gas box quadrature refinement did not converge"));
            }
            let middle = (a + b) * 0.5;
            stack.push((a, middle, depth + 1));
            stack.push((middle, b, depth + 1));
        }
    }
    if !volume.finite()
        || volume.value() < 0.
        || volume.value() > gap.geometry.volume_m3.value() + tolerance
    {
        return Err(fail("gas box quadrature violated volume bounds"));
    }
    Ok(GasBoxQuadrature {
        volume_m3: volume,
        estimated_absolute_error_m3: error,
        estimated_gradient_error_m2: 0.,
        intervals,
    })
}

fn cover_gradient(vertices: [[f64; 3]; 3], bounds: [[f64; 3]; 2], sign: f64) -> [[f64; 3]; 3] {
    let a = sub(vertices[1], vertices[0]);
    let b = sub(vertices[2], vertices[0]);
    let mut polygon = vec![[0., 0.], [1., 0.], [0., 1.]];
    for axis in 0..3 {
        for (bound, direction) in [(bounds[0][axis], 1.), (bounds[1][axis], -1.)] {
            if polygon.is_empty() {
                return [[0.; 3]; 3];
            }
            let value = |p: [f64; 2]| {
                (vertices[0][axis] + a[axis] * p[0] + b[axis] * p[1] - bound) * direction
            };
            let mut next = Vec::new();
            for i in 0..polygon.len() {
                let p = polygon[i];
                let q = polygon[(i + 1) % polygon.len()];
                let fp = value(p);
                let fq = value(q);
                if fp >= 0. {
                    next.push(p);
                }
                if (fp >= 0.) != (fq >= 0.) {
                    let alpha = fp / (fp - fq);
                    next.push(std::array::from_fn(|i| p[i] + alpha * (q[i] - p[i])));
                }
            }
            polygon = next;
        }
    }
    let mut area = 0.;
    let mut moment = [0.; 2];
    for i in 0..polygon.len() {
        let p = polygon[i];
        let q = polygon[(i + 1) % polygon.len()];
        let d = p[0] * q[1] - p[1] * q[0];
        area += 0.5 * d;
        for a in 0..2 {
            moment[a] += (p[a] + q[a]) * d / 6.;
        }
    }
    let integrals = [area - moment[0] - moment[1], moment[0], moment[1]];
    let normal = cross(a, b);
    std::array::from_fn(|i| normal.map(|v| v * sign * integrals[i]))
}

fn cap_gradient(vertices: [[f64; 3]; 6], bounds: [[f64; 3]; 2], edge: usize, t: f64) -> [f64; 18] {
    let next = (edge + 1) % 3;
    let a: [f64; 3] = std::array::from_fn(|i| {
        vertices[edge][i] + t * (vertices[edge + 3][i] - vertices[edge][i])
    });
    let b: [f64; 3] = std::array::from_fn(|i| {
        vertices[next][i] + t * (vertices[next + 3][i] - vertices[next][i])
    });
    let along = sub(b, a);
    let mut low = 0f64;
    let mut high = 1f64;
    for axis in 0..3 {
        if along[axis] == 0. {
            if a[axis] < bounds[0][axis] || a[axis] > bounds[1][axis] {
                return [0.; 18];
            }
        } else {
            let x = (bounds[0][axis] - a[axis]) / along[axis];
            let y = (bounds[1][axis] - a[axis]) / along[axis];
            low = low.max(x.min(y));
            high = high.min(x.max(y));
        }
    }
    if low >= high {
        return [0.; 18];
    }
    let d0 = sub(vertices[edge + 3], vertices[edge]);
    let d1 = sub(vertices[next + 3], vertices[next]);
    let midpoint = 0.5 * (low + high);
    let half = 0.5 * (high - low);
    let mut result = [0.; 18];
    for x in [-1. / 3f64.sqrt(), 1. / 3f64.sqrt()] {
        let s = midpoint + half * x;
        let across = std::array::from_fn(|i| d0[i] + s * (d1[i] - d0[i]));
        let normal = cross(along, across);
        let weights = [(1. - s) * (1. - t), s * (1. - t), (1. - s) * t, s * t];
        for (node, weight) in [edge, next, edge + 3, next + 3].into_iter().zip(weights) {
            for axis in 0..3 {
                result[3 * node + axis] += normal[axis] * weight * half;
            }
        }
    }
    result
}

fn cap_rule(
    vertices: [[f64; 3]; 6],
    bounds: [[f64; 3]; 2],
    edge: usize,
    a: f64,
    b: f64,
    fine: bool,
) -> [f64; 18] {
    let points: &[(f64, f64)] = if fine {
        &[
            (0.9602898564975363, 0.1012285362903763),
            (0.7966664774136267, 0.2223810344533745),
            (0.525532409916329, 0.3137066458778873),
            (0.1834346424956498, 0.362683783378362),
        ]
    } else {
        &[
            (0.8611363115940526, 0.3478548451374538),
            (0.3399810435848563, 0.6521451548625461),
        ]
    };
    let midpoint = 0.5 * (a + b);
    let half = 0.5 * (b - a);
    let mut out = [0.; 18];
    for (x, w) in points {
        let lower = cap_gradient(vertices, bounds, edge, midpoint - half * x);
        let upper = cap_gradient(vertices, bounds, edge, midpoint + half * x);
        for i in 0..18 {
            out[i] += (lower[i] + upper[i]) * w * half;
        }
    }
    out
}

fn shape_gradient(
    vertices: [[f64; 3]; 6],
    bounds: [[f64; 3]; 2],
    policy: GasBoxQuadraturePolicy,
) -> CaeResult<([f64; 18], f64, usize)> {
    let lower = cover_gradient([vertices[0], vertices[1], vertices[2]], bounds, -1.);
    let upper = cover_gradient([vertices[3], vertices[4], vertices[5]], bounds, 1.);
    let mut gradient = [0.; 18];
    for i in 0..3 {
        for a in 0..3 {
            gradient[3 * i + a] = lower[i][a];
            gradient[3 * (i + 3) + a] = upper[i][a];
        }
    }
    let mut length = 0f64;
    for a in vertices {
        for b in vertices {
            length = length.max(dot(sub(a, b), sub(a, b)).sqrt());
        }
    }
    if length <= 0. || !length.is_finite() {
        return Err(fail("invalid gas box shape scale"));
    }
    let tolerance =
        policy.absolute_tolerance_m3 / length + policy.relative_tolerance * length * length;
    if !tolerance.is_finite() || tolerance <= 0. {
        return Err(fail(
            "gas box shape gradient needs a positive finite tolerance",
        ));
    }
    let breaks = breakpoints(vertices, bounds)?;
    let mut total_error = 0.;
    let mut intervals = 0;
    for edge in 0..3 {
        let mut stack = breaks
            .windows(2)
            .map(|p| (p[0], p[1], 0usize))
            .collect::<Vec<_>>();
        while let Some((a, b, depth)) = stack.pop() {
            intervals += 1;
            if intervals > policy.maximum_intervals {
                return Err(fail("gas box gradient interval budget exhausted"));
            }
            let fine = cap_rule(vertices, bounds, edge, a, b, true);
            let coarse = cap_rule(vertices, bounds, edge, a, b, false);
            let error = fine
                .iter()
                .zip(coarse)
                .map(|(a, b)| (a - b).abs())
                .fold(0., f64::max);
            if error <= tolerance * (b - a) / 3. {
                for i in 0..18 {
                    gradient[i] += fine[i];
                }
                total_error += error;
            } else {
                if depth == policy.maximum_depth {
                    return Err(fail("gas box shape gradient did not converge"));
                }
                let middle = 0.5 * (a + b);
                stack.push((a, middle, depth + 1));
                stack.push((middle, b, depth + 1));
            }
        }
    }
    if gradient.iter().any(|v| !v.is_finite()) || !total_error.is_finite() {
        return Err(fail("nonfinite gas box shape gradient"));
    }
    Ok((gradient, total_error, intervals))
}
