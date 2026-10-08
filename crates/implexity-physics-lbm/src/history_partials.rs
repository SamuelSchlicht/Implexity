// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;

use crate::d3q19::Q;
use crate::design::resistance;
use crate::ports::Cell;
use crate::solver::{LbmProblem, collide_cell};

pub trait PointwiseViscosity: Send + Sync {
    fn viscosity<S: Scalar>(&self, temperature: S, raw: S) -> S;
}

pub trait CellCollision: Send + Sync {
    fn collide<S: Scalar>(&self, f: &Cell<S>, alpha: S, acceleration: [f64; 3], tau: S) -> Cell<S>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BgkGuo;

impl CellCollision for BgkGuo {
    fn collide<S: Scalar>(&self, f: &Cell<S>, alpha: S, acceleration: [f64; 3], tau: S) -> Cell<S> {
        collide_cell(f, alpha, acceleration, tau)
    }
}

#[derive(Clone, Debug)]
pub struct Stages {
    pub post_old: CsrMatrix,
    pub post_design: CsrMatrix,
    pub post_temperature: CsrMatrix,
    pub stream_post: CsrMatrix,
    pub stream_old: CsrMatrix,
    pub port_stream: CsrMatrix,
}

fn value_err(message: &str) -> CaeError {
    CaeError::contract(message)
}

fn csr(nrows: usize, ncols: usize, rows: &[usize], cols: &[usize], vals: &[f64]) -> CaeResult<CsrMatrix> {
    CsrMatrix::from_triplets(nrows, ncols, rows, cols, vals).map_err(|e| value_err(&e.to_string()))
}


pub fn stages<C: CellCollision, V: PointwiseViscosity>(
    p: &LbmProblem,
    collision: &C,
    viscosity: &V,
    current: &[f64],
    old: &[f64],
    raw: &[f64],
    temperature: &[f64],
    batch_size: usize,
) -> CaeResult<Stages> {
    let nc = p.cells();
    let n = Q * nc;
    for (values, width) in [(current, n), (old, n), (raw, nc), (temperature, nc)] {
        if values.len() != width || !values.iter().all(|v| f64::is_finite(*v)) {
            return Err(value_err("finite shape-matched stencil inputs required"));
        }
    }
    if batch_size < 1 {
        return Err(value_err("positive local derivative batch size required"));
    }
    let phi = p.fraction(raw);
    let a = p.acceleration_lattice();
    let (dt, dx) = (p.step_s, p.spacing_m);
    let (dmax, shape) = (p.drag_max_per_s, p.drag_shape);

    let mut rows_a = Vec::with_capacity(nc * Q * Q);
    let mut cols_a = Vec::with_capacity(nc * Q * Q);
    let mut vals_a = Vec::with_capacity(nc * Q * Q);
    let mut d_phase = vec![0.0; n];
    let mut d_temp = vec![0.0; n];
    let mut d_raw = vec![0.0; n];
    for c in 0..nc {
        let mut input = [0.0; Q + 3];
        input[..Q].copy_from_slice(&old[c * Q..(c + 1) * Q]);
        input[Q] = phi[c];
        input[Q + 1] = temperature[c];
        input[Q + 2] = raw[c];
        let jac = implexity_ad::forward::jacobian::<{ Q + 3 }, _>(
            |v: &[Dual<{ Q + 3 }>]| {
                let cell: Cell<_> = std::array::from_fn(|q| v[q]);
                let phase = v[Q];
                let alpha = (S1::one() - phase) * (dt * dmax * shape) / (phase + shape);
                let tau = relaxation(viscosity.viscosity(v[Q + 1], v[Q + 2]), dt, dx);
                collision.collide(&cell, alpha, a, tau).to_vec()
            },
            &input,
        )
        .map_err(|e| value_err(&e.to_string()))?;
        for q in 0..Q {
            for k in 0..Q {
                rows_a.push(c * Q + q);
                cols_a.push(c * Q + k);
                vals_a.push(jac.matrix[q * (Q + 3) + k]);
            }
            d_phase[c * Q + q] = jac.matrix[q * (Q + 3) + Q];
            d_temp[c * Q + q] = jac.matrix[q * (Q + 3) + Q + 1];
            d_raw[c * Q + q] = jac.matrix[q * (Q + 3) + Q + 2];
        }
    }
    let post_old = csr(n, n, &rows_a, &cols_a, &vals_a)?;
    let diag_columns = |values: &[f64]| {
        let rows: Vec<usize> = (0..n).collect();
        let cols: Vec<usize> = (0..n).map(|i| i / Q).collect();
        csr(n, nc, &rows, &cols, values)
    };
    let f_map = p.design_map.partial(raw)?;
    let raw_columns = diag_columns(&d_raw)?;
    let post_design = diag_columns(&d_phase)?
        .matmul(&f_map)
        .and_then(|m| m.add_scaled(1.0, &raw_columns, 1.0))
        .map_err(|e| value_err(&e.to_string()))?;
    let post_temperature = diag_columns(&d_temp)?;

    let mut s_rows = Vec::new();
    let mut s_cols = Vec::new();
    let mut o_rows = Vec::new();
    for i in 0..n {
        match p.streaming.source_of(i) {
            Some(src) => {
                s_rows.push(i);
                s_cols.push(src);
            }
            None => o_rows.push(i),
        }
    }
    let stream_post = csr(n, n, &s_rows, &s_cols, &vec![1.0; s_rows.len()])?;
    let stream_old = csr(n, n, &o_rows, &o_rows, &vec![1.0; o_rows.len()])?;

    let streamed = {
        let mut post = vec![0.0; n];
        for c in 0..nc {
            let tau = relaxation(viscosity.viscosity(temperature[c], raw[c]), dt, dx);
            let alpha = resistance(dt, dmax, shape, phi[c]);
            let cell: Cell<f64> = std::array::from_fn(|q| old[c * Q + q]);
            post[c * Q..(c + 1) * Q].copy_from_slice(&collision.collide(&cell, alpha, a, tau));
        }
        let mut out = vec![0.0; n];
        p.streaming.stream(&post, old, &mut out);
        out
    };
    let mut port_rows: std::collections::BTreeMap<usize, Vec<(usize, f64)>> =
        std::collections::BTreeMap::new();
    for port in &p.ports {
        for (&dest, &src) in port.port.cells.iter().zip(&port.sources) {
            if port_rows.contains_key(&(dest * Q)) {
                return Err(value_err("overlapping port rows"));
            }
            let jac = port.cell_jacobian(&streamed[src * Q..(src + 1) * Q], a)?;
            for q in 0..Q {
                let entries: Vec<(usize, f64)> = (0..Q)
                    .filter(|&k| jac[q * Q + k] != 0.0)
                    .map(|k| (src * Q + k, jac[q * Q + k]))
                    .collect();
                port_rows.insert(dest * Q + q, entries);
            }
        }
    }
    let mut q_rows = Vec::new();
    let mut q_cols = Vec::new();
    let mut q_vals = Vec::new();
    for i in 0..n {
        if let Some(entries) = port_rows.get(&i) {
            for (col, v) in entries {
                q_rows.push(i);
                q_cols.push(*col);
                q_vals.push(*v);
            }
        } else {
            q_rows.push(i);
            q_cols.push(i);
            q_vals.push(1.0);
        }
    }
    let port_stream = csr(n, n, &q_rows, &q_cols, &q_vals)?;
    let out = Stages { post_old, post_design, post_temperature, stream_post, stream_old, port_stream };
    for (m, width) in [
        (&out.post_old, n),
        (&out.post_design, nc),
        (&out.post_temperature, nc),
        (&out.stream_post, n),
        (&out.stream_old, n),
        (&out.port_stream, n),
    ] {
        if m.shape() != (n, width) || !m.is_finite() {
            return Err(value_err("invalid sparse stencil stage"));
        }
    }
    Ok(out)
}

type S1 = Dual<{ Q + 3 }>;

#[inline]
pub fn relaxation<S: Scalar>(nu: S, step_s: f64, spacing_m: f64) -> S {
    nu * 3.0 * step_s / (spacing_m * spacing_m) + 0.5
}


pub fn partials<C: CellCollision, V: PointwiseViscosity>(
    p: &LbmProblem,
    collision: &C,
    viscosity: &V,
    current: &[f64],
    old: &[f64],
    raw: &[f64],
    temperature: &[f64],
    batch_size: usize,
) -> CaeResult<[CsrMatrix; 4]> {
    let s = stages(p, collision, viscosity, current, old, raw, temperature, batch_size)?;
    let e = |e: implexity_linalg::error::LinalgError| value_err(&e.to_string());
    let n = s.post_old.nrows();
    let nc = s.post_design.ncols();
    let sa = s.stream_post.matmul(&s.post_old).map_err(e)?.add_scaled(1.0, &s.stream_old, 1.0).map_err(e)?;
    let old_partial = s.port_stream.matmul(&sa).map_err(e)?.add_scaled(-1.0, &sa, 0.0).map_err(e)?;
    let qs = s.port_stream.matmul(&s.stream_post).map_err(e)?;
    let design = qs.matmul(&s.post_design).map_err(e)?.add_scaled(-1.0, &s.post_design, 0.0).map_err(e)?;
    let temp =
        qs.matmul(&s.post_temperature).map_err(e)?.add_scaled(-1.0, &s.post_temperature, 0.0).map_err(e)?;
    let out = [CsrMatrix::identity(n), old_partial, design, temp];
    for (m, width) in out.iter().zip([n, n, nc, nc]) {
        if m.shape() != (n, width) || !m.is_finite() {
            return Err(value_err("invalid sparse stencil partial"));
        }
    }
    Ok(out)
}


pub fn local_partials<const N: usize, F>(
    law: F,
    arguments: &[&[f64]],
    widths: &[usize],
    output_width: usize,
    batch_size: usize,
) -> CaeResult<Vec<CsrMatrix>>
where
    F: Fn(&[Dual<N>]) -> Vec<Dual<N>>,
{
    if batch_size < 1 {
        return Err(value_err("positive integer batch size required"));
    }
    if arguments.is_empty() {
        return Err(value_err("at least one cell argument required"));
    }
    if arguments.iter().any(|a| !a.iter().all(|v| f64::is_finite(*v))) || widths.len() != arguments.len() {
        return Err(value_err("finite arrays with leading cell dimension required"));
    }
    let count = arguments[0].len().checked_div(widths[0]).unwrap_or(0);
    if count < 1 || arguments.iter().zip(widths).any(|(a, w)| *w == 0 || a.len() != count * w) {
        return Err(value_err("matching nonempty leading cell dimensions required"));
    }
    let total: usize = widths.iter().sum();
    let mut triplets: Vec<(Vec<usize>, Vec<usize>, Vec<f64>)> =
        widths.iter().map(|_| (Vec::new(), Vec::new(), Vec::new())).collect();
    for cell in 0..count {
        let mut input = Vec::with_capacity(total);
        for (a, w) in arguments.iter().zip(widths) {
            input.extend_from_slice(&a[cell * w..(cell + 1) * w]);
        }
        let jac =
            implexity_ad::forward::jacobian::<N, _>(&law, &input).map_err(|e| value_err(&e.to_string()))?;
        if jac.rows != output_width || !jac.matrix.iter().all(|v| f64::is_finite(*v)) {
            return Err(value_err("finite matching local output derivatives required"));
        }
        let mut offset = 0;
        for (i, w) in widths.iter().enumerate() {
            for r in 0..output_width {
                for k in 0..*w {
                    triplets[i].0.push(cell * output_width + r);
                    triplets[i].1.push(cell * w + k);
                    triplets[i].2.push(jac.matrix[r * total + offset + k]);
                }
            }
            offset += w;
        }
    }
    triplets
        .into_iter()
        .zip(widths)
        .map(|((r, c, v), w)| csr(count * output_width, count * w, &r, &c, &v))
        .collect()
}
