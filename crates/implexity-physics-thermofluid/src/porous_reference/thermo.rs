// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use std::sync::Arc;

use super::ad::{A, Shape};
use super::fem::{ETA_N, Mesh3, XI_N, ZETA_N, constitutive, strain_displacement};
use super::grid::node_index;

pub const VOL6: [f64; 6] = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0];

#[must_use]
pub fn eigen_coeff(nu: f64) -> f64 {
    1.0 / (1.0 - 2.0 * nu)
}

#[must_use]
pub fn thermal_force_unit3(nu: f64, h: f64) -> [f64; 24] {
    let s = eigen_coeff(nu);
    let g = 1.0 / 3f64.sqrt();
    let det_j = (h / 2.0).powi(3);
    let mut v = [0.0; 24];
    for xi in [-g, g] {
        for eta in [-g, g] {
            for zeta in [-g, g] {
                for a in 0..8 {
                    let dnx = 2.0 / h * (0.125 * XI_N[a] * (1.0 + eta * ETA_N[a]) * (1.0 + zeta * ZETA_N[a]));
                    let dny = 2.0 / h * (0.125 * ETA_N[a] * (1.0 + xi * XI_N[a]) * (1.0 + zeta * ZETA_N[a]));
                    let dnz = 2.0 / h * (0.125 * ZETA_N[a] * (1.0 + xi * XI_N[a]) * (1.0 + eta * ETA_N[a]));
                    v[3 * a] += dnx * s * det_j;
                    v[3 * a + 1] += dny * s * det_j;
                    v[3 * a + 2] += dnz * s * det_j;
                }
            }
        }
    }
    v
}

#[must_use]
pub fn thermal_force_unit3_closed(nu: f64, h: f64) -> [f64; 24] {
    let q = eigen_coeff(nu) * h * h / 4.0;
    let mut v = [0.0; 24];
    for a in 0..8 {
        v[3 * a] = q * XI_N[a];
        v[3 * a + 1] = q * ETA_N[a];
        v[3 * a + 2] = q * ZETA_N[a];
    }
    v
}

#[must_use]
pub fn thermal_force<'g>(e: A<'g>, alpha_dt: A<'g>, gvec: &[f64; 24], mesh: &Arc<Mesh3>) -> A<'g> {
    let w = alpha_dt.flat() * e.flat();
    let m1 = Arc::clone(mesh);
    let m2 = Arc::clone(mesh);
    let gv = *gvec;
    let ndof = mesh.ndof;
    let nel = mesh.edof.len();
    w.graph().linear(
        w,
        Shape::d1(ndof),
        move |v| {
            let mut f = vec![0.0; ndof];
            for (e, dofs) in m1.edof.iter().enumerate() {
                for q in 0..24 {
                    f[dofs[q]] += v[e] * gv[q];
                }
            }
            for (fi, &fx) in f.iter_mut().zip(&m1.fixed) {
                if fx {
                    *fi = 0.0;
                }
            }
            f
        },
        move |gy| {
            let mut out = vec![0.0; nel];
            for (e, dofs) in m2.edof.iter().enumerate() {
                let mut s = 0.0;
                for q in 0..24 {
                    if !m2.fixed[dofs[q]] {
                        s += gy[dofs[q]] * gv[q];
                    }
                }
                out[e] = s;
            }
            out
        },
    )
}

#[must_use]
pub fn coolant_absolute_pressure<'g>(p_gauge: A<'g>, inlet_abs: A<'g>, drop: A<'g>) -> A<'g> {
    inlet_abs - drop + p_gauge
}

#[must_use]
pub fn internal_face_nodes(s: [usize; 3], axis: usize) -> Vec<[usize; 4]> {
    if s[axis] < 2 {
        return Vec::new();
    }
    let node = |i: usize, j: usize, k: usize| node_index(i, j, k, s[1], s[2]);
    let r = |a: usize| if a == axis { 1..s[a] } else { 0..s[a] };
    let mut out = Vec::new();
    for i in r(0) {
        for j in r(1) {
            for k in r(2) {
                out.push(match axis {
                    0 => [node(i, j, k), node(i, j + 1, k), node(i, j, k + 1), node(i, j + 1, k + 1)],
                    1 => [node(i, j, k), node(i + 1, j, k), node(i, j, k + 1), node(i + 1, j, k + 1)],
                    _ => [node(i, j, k), node(i + 1, j, k), node(i, j + 1, k), node(i + 1, j + 1, k)],
                });
            }
        }
    }
    out
}

#[must_use]
pub fn diffuse_pressure_force<'g>(rho: A<'g>, p_abs: A<'g>, mesh: &Mesh3) -> A<'g> {
    let g = rho.graph();
    let s = [mesh.nelx, mesh.nely, mesh.nelz];
    let area = mesh.h * mesh.h;
    let mut load = g.full(0.0, Shape::d1(mesh.ndof));
    for axis in 0..3 {
        let quads = internal_face_nodes(s, axis);
        if quads.is_empty() {
            continue;
        }
        let d_rho = super::ops::hi_part(rho, axis) - super::ops::lo_part(rho, axis);
        let p_face = (super::ops::hi_part(p_abs, axis) + super::ops::lo_part(p_abs, axis)) * 5e-7;
        let force = (p_face * area * d_rho).flat();
        let nf = quads.len();
        let rep: Vec<usize> = (0..nf).flat_map(|f| [f, f, f, f]).collect();
        let contrib = force.gather(rep, Shape::d1(4 * nf)) * 0.25;
        let dofs: Vec<usize> = quads.iter().flat_map(|q| q.map(|n| 3 * n + axis)).collect();
        load = load + contrib.scatter_add(dofs, Shape::d1(mesh.ndof));
    }
    load
}

#[must_use]
pub fn pressure_force_diagnostics(force: &[f64]) -> [(String, serde_json::Value); 4] {
    let n = force.len() / 3;
    let mut res = [0.0; 3];
    let mut l1 = 0.0;
    let mut l2 = 0.0;
    for i in 0..n {
        let v = [force[3 * i] * 1e6, force[3 * i + 1] * 1e6, force[3 * i + 2] * 1e6];
        for a in 0..3 {
            res[a] += v[a];
        }
        let s = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
        l1 += (s + 1e-30).sqrt();
        l2 += s;
    }
    let rn = (res[0] * res[0] + res[1] * res[1] + res[2] * res[2] + 1e-30).sqrt();
    [
        ("resultant_N".into(), serde_json::json!(res)),
        ("resultant_norm_N".into(), serde_json::json!(rn)),
        ("nodal_l1_N".into(), serde_json::json!(l1)),
        ("nodal_l2_N".into(), serde_json::json!((l2 + 1e-30).sqrt())),
    ]
}

#[must_use]
pub fn centroid_operators3(nu: f64, h: f64) -> [[f64; 24]; 6] {
    let d = constitutive(nu);
    let b = strain_displacement(0.0, 0.0, 0.0, h);
    let mut out = [[0.0; 24]; 6];
    for r in 0..6 {
        for c in 0..24 {
            let mut s = 0.0;
            for k in 0..6 {
                s += d[r][k] * b[k][c];
            }
            out[r][c] = s;
        }
    }
    out
}

#[must_use]
pub fn centroid_stress3<'g>(
    u: A<'g>,
    e: A<'g>,
    alpha_dt: A<'g>,
    nu: f64,
    h: f64,
    mesh: &Arc<Mesh3>,
) -> A<'g> {
    let g = u.graph();
    let nel = mesh.edof.len();
    let db = Arc::new(centroid_operators3(nu, h));
    let m1 = Arc::clone(mesh);
    let m2 = Arc::clone(mesh);
    let db2 = Arc::clone(&db);
    let ndof = mesh.ndof;
    let strain = g.linear(
        u,
        Shape::new(&[nel, 6]),
        move |v| {
            let mut out = vec![0.0; nel * 6];
            for (e, dofs) in m1.edof.iter().enumerate() {
                for r in 0..6 {
                    let mut s = 0.0;
                    for q in 0..24 {
                        if !m1.fixed[dofs[q]] {
                            s += v[dofs[q]] * db[r][q];
                        }
                    }
                    out[e * 6 + r] = s;
                }
            }
            out
        },
        move |gy| {
            let mut out = vec![0.0; ndof];
            for (e, dofs) in m2.edof.iter().enumerate() {
                for q in 0..24 {
                    if m2.fixed[dofs[q]] {
                        continue;
                    }
                    let mut s = 0.0;
                    for r in 0..6 {
                        s += gy[e * 6 + r] * db2[r][q];
                    }
                    out[dofs[q]] += s;
                }
            }
            out
        },
    );
    let rep6: Vec<usize> = (0..nel).flat_map(|e| [e; 6]).collect();
    let e6 = e.flat().gather(rep6.clone(), Shape::new(&[nel, 6]));
    let a6 = alpha_dt.flat().gather(rep6, Shape::new(&[nel, 6]));
    let vol: Vec<f64> = (0..nel).flat_map(|_| VOL6).collect();
    let volc = g.constant(vol, Shape::new(&[nel, 6]));
    let ec = eigen_coeff(nu);
    e6 * strain - e6 * (a6 * ec * volc)
}

#[must_use]
pub fn comp(sigma: A<'_>, i: usize) -> A<'_> {
    let nel = sigma.len() / 6;
    sigma.gather((0..nel).map(|e| e * 6 + i).collect(), Shape::d1(nel))
}

#[must_use]
pub fn von_mises3<'g>(sigma: A<'g>) -> A<'g> {
    let s: [A<'g>; 6] = std::array::from_fn(|i| comp(sigma, i));
    sigma.graph().mapn(s, |[s0, s1, s2, s3, s4, s5]| {
        let dev = (s0 - s1) * (s0 - s1) + (s1 - s2) * (s1 - s2) + (s2 - s0) * (s2 - s0);
        let shear = s3 * s3 + s4 * s4 + s5 * s5;
        (dev * 0.5 + shear * 3.0 + 1e-30).sqrt()
    })
}

#[must_use]
pub fn principal_max3(s: &[f64; 6]) -> f64 {
    let p = (s[0] + s[1] + s[2]) / 3.0;
    let (d0, d1, d2) = (s[0] - p, s[1] - p, s[2] - p);
    let (txy, tyz, tzx) = (s[3], s[4], s[5]);
    let dev = (s[0] - s[1]).powi(2) + (s[1] - s[2]).powi(2) + (s[2] - s[0]).powi(2);
    let q = (0.5 * dev + 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5]) + 1e-30).sqrt();
    let j3 = d0 * (d1 * d2 - tyz * tyz) - txy * (txy * d2 - tyz * tzx) + tzx * (txy * tyz - d1 * tzx);
    let r = (13.5 * j3 / (q * q * q)).clamp(-1.0, 1.0);
    let theta = r.acos() / 3.0;
    p + 2.0 / 3.0 * q * theta.cos()
}

#[must_use]
pub fn hydrostatic3(s: &[f64; 6]) -> f64 {
    (s[0] + s[1] + s[2]) / 3.0
}

#[must_use]
pub fn stress_vm3(u: &[f64], e: &[f64], mesh: &Mesh3) -> Vec<f64> {
    let db = centroid_operators3(mesh.nu, mesh.h);
    mesh.edof
        .iter()
        .enumerate()
        .map(|(el, dofs)| {
            let mut s = [0.0; 6];
            for r in 0..6 {
                let mut acc = 0.0;
                for q in 0..24 {
                    if !mesh.fixed[dofs[q]] {
                        acc += u[dofs[q]] * db[r][q];
                    }
                }
                s[r] = e[el] * acc;
            }
            let dev = (s[0] - s[1]).powi(2) + (s[1] - s[2]).powi(2) + (s[2] - s[0]).powi(2);
            (0.5 * dev + 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5]) + 1e-30).sqrt()
        })
        .collect()
}

pub fn von_mises_reg<'g>(sigma: A<'g>, regularization: f64) -> A<'g> {
    let s: [A<'g>; 6] = std::array::from_fn(|i| comp(sigma, i));
    sigma.graph().mapn(s, move |[s0, s1, s2, s3, s4, s5]| {
        let dev = (s0 - s1) * (s0 - s1) + (s1 - s2) * (s1 - s2) + (s2 - s0) * (s2 - s0);
        let shear = s3 * s3 + s4 * s4 + s5 * s5;
        (dev * 0.5 + shear * 3.0 + regularization * regularization).sqrt()
    })
}

pub fn resolve_traction<'g>(sigma: A<'g>, normal: [A<'g>; 3], regularization: f64) -> (A<'g>, A<'g>) {
    let g = sigma.graph();
    let s: [A<'g>; 6] = std::array::from_fn(|i| comp(sigma, i));
    let ins = [s[0], s[1], s[2], s[3], s[4], s[5], normal[0], normal[1], normal[2]];
    let [snn, tau] = g.mapn_multi(ins, move |x| {
        let [s0, s1, s2, s3, s4, s5, nx, ny, nz] = x;
        let tx = s0 * nx + s3 * ny + s5 * nz;
        let ty = s3 * nx + s1 * ny + s4 * nz;
        let tz = s5 * nx + s4 * ny + s2 * nz;
        let snn = tx * nx + ty * ny + tz * nz;
        let t2 = tx * tx + ty * ty + tz * tz;
        let tau = (t2 - snn * snn + regularization * regularization).sqrt();
        [snn, tau]
    });
    (snn, tau)
}

pub fn smooth_positive(x: A<'_>, scale: f64) -> A<'_> {
    x.map(move |v| (v + (v * v + scale * scale).sqrt()) * 0.5)
}

pub fn half_space_temperature_rise<'g>(k: A<'g>, rho_cp: A<'g>, energy_per_area: f64, duration: f64) -> A<'g> {
    let q = energy_per_area / duration;
    k.graph().mapn([k, rho_cp], move |[kf, rc]| {
        (implexity_ad::Dual::constant(duration) / (kf * std::f64::consts::PI * rc)).sqrt() * (2.0 * q)
    })
}
