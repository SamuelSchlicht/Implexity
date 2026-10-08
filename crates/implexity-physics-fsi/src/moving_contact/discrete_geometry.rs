// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use crate::moving_contact::moving_point_triangle::interior_geometry;
use implexity_ad::{Dual, Scalar};
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
fn sum<S: Scalar>(v: impl Iterator<Item = S>) -> S {
    v.fold(S::from_f64(0.), |a, b| a + b)
}
fn law<S: Scalar>(old: [[S; 3]; 4], new: [[S; 3]; 4]) -> Result<([S; 12], S, S), String> {
    let c = S::from_f64;
    let prev = interior_geometry(old, 1e-6, 1e-8).map_err(str::to_string)?;
    let current = interior_geometry(new, 1e-6, 1e-8).map_err(str::to_string)?;
    let mid = std::array::from_fn(|k| std::array::from_fn(|i| (old[k][i] + new[k][i]) * c(0.5)));
    let m = interior_geometry(mid, 1e-6, 1e-8).map_err(str::to_string)?;
    let gradient: [S; 12] = std::array::from_fn(|k| {
        if k < 3 {
            m.normal[k]
        } else {
            -m.barycentric[k / 3 - 1] * m.normal[k % 3]
        }
    });
    let d: [[S; 3]; 4] = std::array::from_fn(|k| sub(new[k], old[k]));
    if d.iter().flatten().all(|x| x.value() == 0.) {
        return Ok((gradient, prev.gap, current.gap));
    }
    let center: [S; 3] = std::array::from_fn(|i| sum(mid.iter().map(|p| p[i])) / c(4.));
    let mean: [S; 3] = std::array::from_fn(|i| sum(d.iter().map(|p| p[i])) / c(4.));
    let r: [[S; 3]; 4] = mid.map(|p| sub(p, center));
    let v = d.map(|p| sub(p, mean));
    let inertia: [[S; 3]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            sum(r
                .iter()
                .map(|p| (if i == j { dot(*p, *p) } else { c(0.) }) - p[i] * p[j]))
        })
    });
    let moment: [S; 3] =
        std::array::from_fn(|i| sum(r.iter().zip(v).map(|(p, w)| cross(*p, w)[i])));
    let det = dot(inertia[0], cross(inertia[1], inertia[2]));
    if !det.value().is_finite() || det.value() <= 0. {
        return Err("rigid projection degenerate".into());
    }
    let cols = [
        cross(inertia[1], inertia[2]),
        cross(inertia[2], inertia[0]),
        cross(inertia[0], inertia[1]),
    ];
    let omega: [S; 3] = std::array::from_fn(|i| sum((0..3).map(|j| cols[j][i] * moment[j])) / det);
    let projected: [S; 12] = std::array::from_fn(|k| sub(v[k / 3], cross(omega, r[k / 3]))[k % 3]);
    let flat: [S; 12] = std::array::from_fn(|k| d[k / 3][k % 3]);
    let den = sum(projected.iter().map(|x| *x * *x));
    let norm = sum(flat.iter().map(|x| *x * *x));
    if !den.value().is_finite() || den.value() <= 1e-16 * norm.value() {
        return Err("nearly rigid nonzero increment".into());
    }
    let rem = current.gap - prev.gap - sum((0..12).map(|i| gradient[i] * flat[i]));
    let result = std::array::from_fn(|i| gradient[i] + rem * projected[i] / den);
    if result.iter().any(|x| !x.value().is_finite()) {
        return Err("discrete gradient overflow".into());
    }
    Ok((result, prev.gap, current.gap))
}
pub struct Geometry {
    pub force: [f64; 12],
    pub current: [[f64; 12]; 12],
    pub previous: [[f64; 12]; 12],
    pub gap0: f64,
    pub gap1: f64,
    pub gap_gradient: [f64; 12],
    pub work_defect: f64,
    pub zero_increment: bool,
}
pub fn evaluate(old: [[f64; 3]; 4], new: [[f64; 3]; 4]) -> Result<Geometry, String> {
    let a = std::array::from_fn(|k| {
        std::array::from_fn(|i| Dual::<24>::variable(old[k][i], 3 * k + i))
    });
    let b = std::array::from_fn(|k| {
        std::array::from_fn(|i| Dual::<24>::variable(new[k][i], 12 + 3 * k + i))
    });
    let (g, g0, g1) = law(a, b)?;
    let force = g.map(|x| x.re);
    let previous = std::array::from_fn(|i| std::array::from_fn(|j| g[i].eps[j]));
    let current = std::array::from_fn(|i| std::array::from_fn(|j| g[i].eps[12 + j]));
    let gap_gradient = std::array::from_fn(|j| g1.eps[12 + j]);
    if previous
        .iter()
        .flatten()
        .chain(current.iter().flatten())
        .chain(gap_gradient.iter())
        .any(|x| !x.is_finite())
    {
        return Err("geometry derivative overflow".into());
    }
    let work_defect = (0..12)
        .map(|j| force[j] * (new[j / 3][j % 3] - old[j / 3][j % 3]))
        .sum::<f64>()
        - (g1.re - g0.re);
    Ok(Geometry {
        force,
        current,
        previous,
        gap0: g0.re,
        gap1: g1.re,
        gap_gradient,
        work_defect,
        zero_increment: old == new,
    })
}
