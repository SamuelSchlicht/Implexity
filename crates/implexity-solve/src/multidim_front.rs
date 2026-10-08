// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};

fn err(message: &str) -> CaeError {
    CaeError::contract(message)
}

fn rows(len: usize, dim: usize) -> CaeResult<usize> {
    if dim == 0 || !len.is_multiple_of(dim) {
        return Err(err("vector arrays must have a trailing dimension"));
    }
    Ok(len / dim)
}



pub fn normal_from_levelset_gradient(grad_phi: &[f64], dim: usize, eps: f64) -> CaeResult<Vec<f64>> {
    rows(grad_phi.len(), dim)?;
    let mut out = Vec::with_capacity(grad_phi.len());
    for g in grad_phi.chunks(dim) {
        let n = g.iter().map(|v| v * v).sum::<f64>().sqrt();
        if n <= eps {
            return Err(err("undefined interface normal"));
        }
        out.extend(g.iter().map(|v| v / n));
    }
    Ok(out)
}



pub fn normal_jump_speed(
    ql: &[f64],
    qr: &[f64],
    flux_l: &[f64],
    flux_r: &[f64],
    normal: &[f64],
    dim: usize,
    eps: f64,
) -> CaeResult<Vec<f64>> {
    let n = ql.len();
    if qr.len() != n || flux_l.len() != n * dim || flux_r.len() != n * dim || normal.len() != n * dim {
        return Err(err("front arrays have inconsistent shapes"));
    }
    if ql.iter().zip(qr).any(|(a, b)| (a - b).abs() <= eps) {
        return Err(err("vanishing state jump"));
    }
    Ok((0..n)
        .map(|i| {
            let num: f64 =
                (0..dim).map(|k| (flux_l[i * dim + k] - flux_r[i * dim + k]) * normal[i * dim + k]).sum();
            num / (ql[i] - qr[i])
        })
        .collect())
}



#[allow(clippy::too_many_arguments)]
pub fn normal_jump_speed_gradient(
    ql: &[f64],
    qr: &[f64],
    flux_l: &[f64],
    flux_r: &[f64],
    normal: &[f64],
    dql: &[f64],
    dqr: &[f64],
    dflux_l: &[f64],
    dflux_r: &[f64],
    dnormal: &[f64],
    dim: usize,
) -> CaeResult<Vec<f64>> {
    let n = ql.len();
    let vec_ok = [flux_l, flux_r, normal, dflux_l, dflux_r, dnormal].iter().all(|a| a.len() == n * dim);
    if qr.len() != n || dql.len() != n || dqr.len() != n || !vec_ok {
        return Err(err("front arrays have inconsistent shapes"));
    }
    Ok((0..n)
        .map(|i| {
            let (mut num, mut dnum) = (0.0, 0.0);
            for k in 0..dim {
                let j = i * dim + k;
                let df = flux_l[j] - flux_r[j];
                num += df * normal[j];
                dnum += (dflux_l[j] - dflux_r[j]) * normal[j] + df * dnormal[j];
            }
            let den = ql[i] - qr[i];
            (dnum * den - num * (dql[i] - dqr[i])) / (den * den)
        })
        .collect())
}



pub fn advect_front(
    points: &[f64],
    normals: &[f64],
    speed: &[f64],
    dt: f64,
    dim: usize,
) -> CaeResult<Vec<f64>> {
    let n = rows(points.len(), dim)?;
    if normals.len() != points.len() || speed.len() != n {
        return Err(err("front arrays have inconsistent shapes"));
    }
    Ok(points.iter().enumerate().map(|(j, p)| p + dt * speed[j / dim] * normals[j]).collect())
}

