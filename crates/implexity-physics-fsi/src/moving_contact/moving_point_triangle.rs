// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::Scalar;

pub struct InteriorGeometry<S> {
    pub gap: S,
    pub normal: [S; 3],
    pub barycentric: [S; 3],
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn interior_geometry<S: Scalar>(
    positions: [[S; 3]; 4],
    minimum_barycentric: f64,
    minimum_area_ratio: f64,
) -> Result<InteriorGeometry<S>, &'static str> {
    if positions.iter().flatten().any(|x| !x.value().is_finite()) {
        return Err("finite current coordinates required");
    }
    if !minimum_barycentric.is_finite()
        || minimum_barycentric <= 0.0
        || minimum_barycentric >= 1.0 / 3.0
        || !minimum_area_ratio.is_finite()
        || minimum_area_ratio <= 0.0
        || minimum_area_ratio >= 1.0
    {
        return Err("invalid declared feature eligibility");
    }
    let [p, a, b, c] = positions;
    let u = sub(b, a);
    let v = sub(c, a);
    let w = sub(p, a);
    let norm = |edge: &[S; 3]| {
        edge.iter()
            .fold(0.0_f64, |length, x| length.hypot(x.value()))
    };
    let scale = norm(&u).max(norm(&v));
    if !scale.is_finite() || scale <= 0.0 {
        return Err("degenerate or unrepresentable edges");
    }
    let factor = S::from_f64(scale);
    let u = u.map(|x| x / factor);
    let v = v.map(|x| x / factor);
    let ws = w.map(|x| x / factor);
    let n = cross(u, v);
    let n2 = dot(n, n);
    if !n2.value().is_finite() || n2.value() <= minimum_area_ratio * minimum_area_ratio {
        return Err("triangle below declared area eligibility");
    }
    let length = n2.sqrt();
    let normal = n.map(|x| x / length);
    let beta = dot(cross(ws, v), n) / n2;
    let gamma = dot(cross(u, ws), n) / n2;
    let barycentric = [S::from_f64(1.0) - beta - gamma, beta, gamma];
    if barycentric
        .iter()
        .any(|x| !x.value().is_finite() || x.value() <= minimum_barycentric)
    {
        return Err("projection outside strict interior feature");
    }
    let gap = dot(w, normal);
    if !gap.value().is_finite() {
        return Err("unrepresentable signed gap");
    }
    Ok(InteriorGeometry {
        gap,
        normal,
        barycentric,
    })
}
