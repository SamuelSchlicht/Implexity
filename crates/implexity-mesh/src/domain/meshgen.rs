// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::HashMap;
use std::path::Path;

use implexity_geometry::domain_sdf::{self, TriMesh};

use crate::MeshError;
use crate::topology::{Tri, Vec3};

#[must_use]
pub fn blas_norm3(v: Vec3) -> f64 {
    v[2].mul_add(v[2], v[1].mul_add(v[1], v[0] * v[0])).sqrt()
}

#[must_use]
pub fn row_norm(v: Vec3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn trimesh(v: &[Vec3], f: &[Tri]) -> TriMesh {
    TriMesh { v: v.to_vec(), f: f.to_vec() }
}


pub fn check_mesh(v: &[Vec3], f: &[Tri], name: &str) -> Result<(), MeshError> {
    domain_sdf::check_mesh(&trimesh(v, f), name).map_err(|e| MeshError::Rejected(e.problems))
}

#[must_use]
pub fn weld(v: &[Vec3], f: &[Tri], tol_rel: f64) -> (Vec<Vec3>, Vec<Tri>) {
    let m = domain_sdf::weld(&trimesh(v, f), tol_rel);
    (m.v, m.f)
}


pub fn box_mesh(lo: Vec3, hi: Vec3) -> Result<(Vec<Vec3>, Vec<Tri>), MeshError> {
    let [x0, y0, z0] = lo;
    let [x1, y1, z1] = hi;
    let v = vec![
        [x0, y0, z0],
        [x1, y0, z0],
        [x1, y1, z0],
        [x0, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x1, y1, z1],
        [x0, y1, z1],
    ];
    let f = vec![
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [2, 3, 7],
        [2, 7, 6],
        [0, 4, 7],
        [0, 7, 3],
        [1, 2, 6],
        [1, 6, 5],
    ];
    check_mesh(&v, &f, "box")?;
    Ok((v, f))
}


pub fn icosphere(radius: f64, subdivisions: usize, centre: Vec3) -> Result<(Vec<Vec3>, Vec<Tri>), MeshError> {
    let t = f64::midpoint(1.0, 5.0f64.sqrt());
    let mut v: Vec<Vec3> = [
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ]
    .iter()
    .map(|p| {
        let n = row_norm(*p);
        p.map(|x| x / n)
    })
    .collect();
    let mut f: Vec<Tri> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    for _ in 0..subdivisions {
        let base = v.clone();
        let mut cache: HashMap<(usize, usize), usize> = HashMap::new();
        let mut mid = |a: usize, b: usize, v: &mut Vec<Vec3>| -> usize {
            let key = (a.min(b), a.max(b));
            *cache.entry(key).or_insert_with(|| {
                let p: Vec3 = std::array::from_fn(|k| f64::midpoint(base[a][k], base[b][k]));
                let n = blas_norm3(p);
                v.push(p.map(|x| x / n));
                v.len() - 1
            })
        };
        let mut f2 = Vec::with_capacity(4 * f.len());
        for &[a, b, c] in &f {
            let ab = mid(a, b, &mut v);
            let bc = mid(b, c, &mut v);
            let ca = mid(c, a, &mut v);
            f2.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        }
        f = f2;
    }
    let v: Vec<Vec3> = v.iter().map(|p| std::array::from_fn(|k| p[k] * radius + centre[k])).collect();
    check_mesh(&v, &f, "icosphere")?;
    Ok((v, f))
}


pub fn torus(
    r_major: f64,
    r_minor: f64,
    n_major: usize,
    n_minor: usize,
    centre: Vec3,
) -> Result<(Vec<Vec3>, Vec<Tri>), MeshError> {
    let two_pi = 2.0 * std::f64::consts::PI;
    #[allow(clippy::cast_precision_loss)]
    let (nm, nn) = (n_major as f64, n_minor as f64);
    let mut v = Vec::with_capacity(n_major * n_minor);
    for i in 0..n_major {
        #[allow(clippy::cast_precision_loss)]
        let u = two_pi * i as f64 / nm;
        for j in 0..n_minor {
            #[allow(clippy::cast_precision_loss)]
            let w = two_pi * j as f64 / nn;
            let x = (r_major + r_minor * w.cos()) * u.cos();
            let y = (r_major + r_minor * w.cos()) * u.sin();
            let z = r_minor * w.sin();
            v.push([x + centre[0], y + centre[1], z + centre[2]]);
        }
    }
    let vid = |i: usize, j: usize| (i % n_major) * n_minor + (j % n_minor);
    let mut f = Vec::with_capacity(2 * n_major * n_minor);
    for i in 0..n_major {
        for j in 0..n_minor {
            let (a, b, c, d) = (vid(i, j), vid(i + 1, j), vid(i + 1, j + 1), vid(i, j + 1));
            f.push([a, b, c]);
            f.push([a, c, d]);
        }
    }
    check_mesh(&v, &f, "torus")?;
    Ok((v, f))
}

#[must_use]
pub fn torus_sdf(p: &[Vec3], r_major: f64, r_minor: f64, centre: Vec3) -> Vec<f64> {
    p.iter()
        .map(|q| {
            let q: Vec3 = std::array::from_fn(|k| q[k] - centre[k]);
            let ring = (q[0] * q[0] + q[1] * q[1]).sqrt() - r_major;
            (ring * ring + q[2] * q[2]).sqrt() - r_minor
        })
        .collect()
}

#[must_use]
pub fn box_sdf(p: &[Vec3], lo: Vec3, hi: Vec3) -> Vec<f64> {
    let c: Vec3 = std::array::from_fn(|k| f64::midpoint(lo[k], hi[k]));
    let b: Vec3 = std::array::from_fn(|k| (hi[k] - lo[k]) / 2.0);
    p.iter()
        .map(|x| {
            let q: Vec3 = std::array::from_fn(|k| (x[k] - c[k]).abs() - b[k]);
            let outside = row_norm(q.map(|v| v.max(0.0)));
            outside + q[0].max(q[1]).max(q[2]).min(0.0)
        })
        .collect()
}


pub fn ear_clip(poly: &[[f64; 2]]) -> Result<Vec<Tri>, MeshError> {
    let pts = poly;
    let mut idx: Vec<usize> = (0..pts.len()).collect();
    let cr = |o: usize, a: usize, b: usize| {
        (pts[a][0] - pts[o][0]) * (pts[b][1] - pts[o][1]) - (pts[a][1] - pts[o][1]) * (pts[b][0] - pts[o][0])
    };
    let in_tri = |p: [f64; 2], a: usize, b: usize, c: usize| {
        let d1 = (p[0] - pts[b][0]) * (pts[a][1] - pts[b][1]) - (pts[a][0] - pts[b][0]) * (p[1] - pts[b][1]);
        let d2 = (p[0] - pts[c][0]) * (pts[b][1] - pts[c][1]) - (pts[b][0] - pts[c][0]) * (p[1] - pts[c][1]);
        let d3 = (p[0] - pts[a][0]) * (pts[c][1] - pts[a][1]) - (pts[c][0] - pts[a][0]) * (p[1] - pts[a][1]);
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    };
    if idx.len() < 3 {
        return Err(MeshError::invalid("ear clipping needs at least three vertices"));
    }
    let mut tris = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < 10_000 {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        for k in 0..n {
            let (o, a, b) = (idx[(k + n - 1) % n], idx[k], idx[(k + 1) % n]);
            if cr(o, a, b) <= 1e-18 {
                continue;
            }
            if idx.iter().any(|&j| j != o && j != a && j != b && in_tri(pts[j], o, a, b)) {
                continue;
            }
            tris.push([o, a, b]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            return Err(MeshError::invalid("ear clipping failed (polygon not simple/CCW?)"));
        }
    }
    tris.push([idx[0], idx[1], idx[2]]);
    Ok(tris)
}


pub fn extrude_polygon(poly: &[[f64; 2]], z0: f64, z1: f64) -> Result<(Vec<Vec3>, Vec<Tri>), MeshError> {
    let n = poly.len();
    let mut v: Vec<Vec3> = poly.iter().map(|p| [p[0], p[1], z0]).collect();
    v.extend(poly.iter().map(|p| [p[0], p[1], z1]));
    let caps = ear_clip(poly)?;
    let mut f = Vec::new();
    for [a, b, c] in caps {
        f.push([a, c, b]);
        f.push([n + a, n + b, n + c]);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        f.push([i, j, n + j]);
        f.push([i, n + j, n + i]);
    }
    let (v2, f2) = weld(&v, &f, 1e-9);
    check_mesh(&v2, &f2, "extrusion")?;
    Ok((v2, f2))
}

fn l_polygon(leg_x: f64, leg_y: f64, thick: f64, fillet: f64, n_fillet: usize) -> Vec<[f64; 2]> {
    let (t, r) = (thick, fillet);
    let mut poly = vec![[0.0, 0.0], [leg_x, 0.0], [leg_x, t], [t + r, t]];
    for k in 1..n_fillet {
        #[allow(clippy::cast_precision_loss)]
        let a = 0.5 * std::f64::consts::PI * k as f64 / n_fillet as f64;
        poly.push([t + r - r * a.sin(), t + r - r * a.cos()]);
    }
    poly.extend([[t, t + r], [t, leg_y], [0.0, leg_y]]);
    poly
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LBracket {
    pub leg_x: f64,
    pub leg_y: f64,
    pub thick: f64,
    pub length: f64,
    pub fillet: f64,
    pub n_fillet: usize,
    pub origin: Vec3,
}

impl Default for LBracket {
    fn default() -> Self {
        Self {
            leg_x: 12.0e-3,
            leg_y: 12.0e-3,
            thick: 5.0e-3,
            length: 12.0e-3,
            fillet: 3.0e-3,
            n_fillet: 10,
            origin: [0.0; 3],
        }
    }
}


pub fn l_bracket(p: &LBracket) -> Result<(Vec<Vec3>, Vec<Tri>), MeshError> {
    let poly: Vec<[f64; 2]> = l_polygon(p.leg_x, p.leg_y, p.thick, p.fillet, p.n_fillet)
        .iter()
        .map(|q| [q[0] + p.origin[0], q[1] + p.origin[1]])
        .collect();
    extrude_polygon(&poly, p.origin[2], p.origin[2] + p.length)
}

#[must_use]
pub fn l_bracket_polygon_area(leg_x: f64, leg_y: f64, thick: f64, fillet: f64, n_fillet: usize) -> f64 {
    let poly = l_polygon(leg_x, leg_y, thick, fillet, n_fillet);
    let n = poly.len();
    let (mut a, mut b) = (0.0, 0.0);
    for i in 0..n {
        a += poly[i][0] * poly[(i + 1) % n][1];
        b += poly[(i + 1) % n][0] * poly[i][1];
    }
    0.5 * (a - b).abs()
}


pub fn write_stl(path: &Path, v: &[Vec3], f: &[Tri], name: &str) -> Result<u64, MeshError> {
    crate::formats::write_stl_named(path, v, f, name)
}
