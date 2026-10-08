// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use crate::MeshError;
use crate::grid::Field3;
use crate::mc::{GradientDirection, marching_cubes};

pub const HAVE_MC: bool = true;

pub const MESH_MAGIC: &[u8; 8] = b"CVMSH2\0\0";
pub const POLY_MAGIC: &[u8; 8] = b"CVPLY1\0\0";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewMesh {
    pub vertices: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub attribute: Vec<f32>,
}




pub fn slab_mesh(
    field: &Field3<'_>,
    origin: [f64; 3],
    axes: [[f64; 3]; 3],
    h: f64,
    level: f64,
    attribute: Option<&Field3<'_>>,
) -> Result<PreviewMesh, MeshError> {
    if field.shape.iter().any(|&n| n < 2) {
        return Err(MeshError::invalid("a slab mesh needs at least 2 samples on every axis"));
    }
    if let Some(a) = attribute
        && a.shape != field.shape
    {
        return Err(MeshError::invalid("the slab attribute must be sampled on the field's grid"));
    }
    let (lo, hi) = field.min_max();
    if !(lo < level && level < hi) {
        return Ok(PreviewMesh::default());
    }
    let mc = marching_cubes(field, level, GradientDirection::Descent, false)?;
    let mut out = PreviewMesh {
        vertices: Vec::with_capacity(mc.vertices.len()),
        normals: Vec::with_capacity(mc.vertices.len()),
        triangles: mc.faces.clone(),
        attribute: Vec::new(),
    };
    for (v, n) in mc.vertices.iter().zip(&mc.normals) {

        let hf = crate::cast::f32_of(h);
        let p = [f64::from(v[0] * hf), f64::from(v[1] * hf), f64::from(v[2] * hf)];
        let mut w = [0.0; 3];
        let mut nr = [0.0; 3];
        for j in 0..3 {
            w[j] = origin[j] + (p[0] * axes[0][j] + p[1] * axes[1][j] + p[2] * axes[2][j]);
            nr[j] =
                -(f64::from(n[0]) * axes[0][j] + f64::from(n[1]) * axes[1][j] + f64::from(n[2]) * axes[2][j]);
        }
        let len = (nr[0] * nr[0] + nr[1] * nr[1] + nr[2] * nr[2]).sqrt();
        let d = if len < 1e-30 { 1.0 } else { len };
        out.vertices.push([(w[0] * 1000.0) as f32, (w[1] * 1000.0) as f32, (w[2] * 1000.0) as f32]);
        out.normals.push([(nr[0] / d) as f32, (nr[1] / d) as f32, (nr[2] / d) as f32]);
    }
    if let Some(a) = attribute {
        out.attribute = mc.vertices.iter().map(|v| trilinear_clamped(a, *v) as f32).collect();
    }
    Ok(out)
}

fn trilinear_clamped(a: &Field3<'_>, v: [f32; 3]) -> f64 {
    let f = [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])];
    let mut i0 = [0usize; 3];
    let mut fr = [0.0; 3];
    for ax in 0..3 {
        let top = (a.shape[ax] - 2) as f64;
        let lo = f[ax].floor().clamp(0.0, top);
        i0[ax] = lo as usize;
        fr[ax] = (f[ax] - lo).clamp(0.0, 1.0);
    }
    let [i, j, k] = i0;
    let [fx, fy, fz] = fr;
    let c00 = a.at(i, j, k) * (1.0 - fx) + a.at(i + 1, j, k) * fx;
    let c01 = a.at(i, j, k + 1) * (1.0 - fx) + a.at(i + 1, j, k + 1) * fx;
    let c10 = a.at(i, j + 1, k) * (1.0 - fx) + a.at(i + 1, j + 1, k) * fx;
    let c11 = a.at(i, j + 1, k + 1) * (1.0 - fx) + a.at(i + 1, j + 1, k + 1) * fx;
    (c00 * (1.0 - fy) + c10 * fy) * (1.0 - fz) + (c01 * (1.0 - fy) + c11 * fy) * fz
}



#[must_use]
pub fn encode_mesh(mesh: &PreviewMesh) -> Vec<u8> {
    let nv = mesh.vertices.len();
    let nt = mesh.triangles.len();
    let na = usize::from(mesh.attribute.len() == nv);
    let mut out = Vec::with_capacity(20 + nv * 24 + nt * 12 + na * nv * 4);
    out.extend_from_slice(MESH_MAGIC);
    for n in [nv, nt, na] {
        out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
    }
    for v in mesh.vertices.iter().flatten().chain(mesh.normals.iter().flatten()) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for i in mesh.triangles.iter().flatten() {
        out.extend_from_slice(&i.to_le_bytes());
    }
    if na == 1 {
        for a in &mesh.attribute {
            out.extend_from_slice(&a.to_le_bytes());
        }
    }
    out
}

#[must_use]
pub fn encode_polylines(polys: &[Vec<[f32; 3]>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(POLY_MAGIC);
    out.extend_from_slice(&u32::try_from(polys.len()).unwrap_or(u32::MAX).to_le_bytes());
    for p in polys {
        out.extend_from_slice(&u32::try_from(p.len()).unwrap_or(u32::MAX).to_le_bytes());
        for v in p.iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

#[must_use]
pub fn decimate_polylines(polys: &[Vec<[f32; 3]>], tol_mm: f64) -> Vec<Vec<[f32; 3]>> {
    polys
        .iter()
        .map(|p| {
            let pts: Vec<[f64; 3]> =
                p.iter().map(|v| [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]).collect();
            rdp(&pts, tol_mm)
        })
        .filter(|p| p.len() >= 2)
        .map(|p| p.iter().map(|v| [v[0] as f32, v[1] as f32, v[2] as f32]).collect())
        .collect()
}

#[must_use]
pub fn rdp(pts: &[[f64; 3]], tol: f64) -> Vec<[f64; 3]> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let tol2 = tol * tol;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let a = pts[i];
        let ab = [pts[j][0] - a[0], pts[j][1] - a[1], pts[j][2] - a[2]];
        let l2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
        let mut best = (0usize, f64::NEG_INFINITY);
        let thr = if l2 < 1e-24 { tol2 } else { tol2 * l2 };
        for (off, p) in pts[i + 1..j].iter().enumerate() {
            let s = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
            let d2 = if l2 < 1e-24 {
                s[0] * s[0] + s[1] * s[1] + s[2] * s[2]
            } else {
                let cr = ab[0] * s[1] - ab[1] * s[0];
                let cy = ab[1] * s[2] - ab[2] * s[1];
                let cz = ab[2] * s[0] - ab[0] * s[2];
                cr * cr + cy * cy + cz * cz
            };

            if d2 > best.1 {
                best = (off, d2);
            }
        }
        if best.1 > thr {
            let k = i + 1 + best.0;
            keep[k] = true;
            stack.push((i, k));
            stack.push((k, j));
        }
    }
    pts.iter().zip(&keep).filter(|(_, k)| **k).map(|(p, _)| *p).collect()
}
