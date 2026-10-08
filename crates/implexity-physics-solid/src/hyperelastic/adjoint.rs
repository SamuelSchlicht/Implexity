// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_ad::tape::{Pullback, Tape, Var};
use implexity_ad::{AdError, Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;

const W: usize = 8;

pub trait Kernel: Send + Sync + 'static {
    fn n_out(&self) -> usize;
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]);
}

#[derive(Debug, Clone)]
pub struct Gather {
    pub map: Vec<Vec<(usize, usize)>>,
}

impl Gather {
    #[must_use]
    pub fn from_blocks(blocks: &[Vec<Vec<usize>>]) -> Self {
        let ne = blocks.first().map_or(0, Vec::len);
        let map = (0..ne)
            .map(|e| blocks.iter().enumerate().flat_map(|(k, b)| b[e].iter().map(move |i| (k, *i))).collect())
            .collect();
        Self { map }
    }

    fn local(&self, e: usize, values: &[&[f64]]) -> Vec<f64> {
        self.map[e].iter().map(|(k, i)| values[*k][*i]).collect()
    }
}

#[allow(clippy::needless_pass_by_value)]
fn ad(e: AdError) -> CaeError {
    CaeError::contract(e.to_string())
}

fn local_jacobian<K: Kernel>(kernel: &K, e: usize, local: &[f64]) -> Result<Vec<f64>, AdError> {
    let n_out = kernel.n_out();
    let jac = implexity_ad::forward::jacobian::<W, _>(
        |s: &[Dual<W>]| {
            let mut out = vec![Dual::constant(0.0); n_out];
            kernel.eval(e, s, &mut out);
            out
        },
        local,
    )?;
    Ok(jac.matrix)
}


pub fn element_node<K: Kernel>(
    tape: &mut Tape,
    kernel: Arc<K>,
    inputs: &[Var],
    gather: Gather,
) -> Result<Var, CaeError> {
    let values: Vec<Vec<f64>> =
        inputs.iter().map(|v| tape.value(*v).map(<[f64]>::to_vec)).collect::<Result<_, _>>().map_err(ad)?;
    let refs: Vec<&[f64]> = values.iter().map(Vec::as_slice).collect();
    let ne = gather.map.len();
    let n_out = kernel.n_out();
    let mut out = vec![0.0; ne * n_out];
    let locals: Vec<Vec<f64>> = (0..ne).map(|e| gather.local(e, &refs)).collect();
    for (e, local) in locals.iter().enumerate() {
        kernel.eval(e, local, &mut out[e * n_out..(e + 1) * n_out]);
    }
    let lens: Vec<usize> = inputs.iter().map(Var::len).collect();
    let pull: Pullback = Box::new(move |ct: &[f64]| {
        let mut grads: Vec<Vec<f64>> = lens.iter().map(|l| vec![0.0; *l]).collect();
        for (e, local) in locals.iter().enumerate() {
            let c = &ct[e * n_out..(e + 1) * n_out];
            if c.iter().all(|v| *v == 0.0) {
                continue;
            }
            let jac = local_jacobian(kernel.as_ref(), e, local)?;
            let n_in = local.len();
            for (j, (k, i)) in gather.map[e].iter().enumerate() {
                let mut acc = 0.0;
                for (r, cr) in c.iter().enumerate() {
                    acc += cr * jac[r * n_in + j];
                }
                grads[*k][*i] += acc;
            }
        }
        Ok(grads)
    });
    tape.custom(inputs, out, pull).map_err(ad)
}

pub struct RootSpec<K: Kernel> {
    pub kernel: Arc<K>,
    pub gather: Gather,
    pub rows: Vec<Vec<Option<usize>>>,
    pub free: Vec<usize>,
    pub size: usize,
}


pub fn root_node<K: Kernel>(
    tape: &mut Tape,
    spec: RootSpec<K>,
    y: Vec<f64>,
    inputs: &[Var],
) -> Result<Var, CaeError> {
    let values: Vec<Vec<f64>> =
        inputs.iter().map(|v| tape.value(*v).map(<[f64]>::to_vec)).collect::<Result<_, _>>().map_err(ad)?;
    let mut all: Vec<&[f64]> = vec![y.as_slice()];
    all.extend(values.iter().map(Vec::as_slice));
    let ne = spec.gather.map.len();
    let locals: Vec<Vec<f64>> = (0..ne).map(|e| spec.gather.local(e, &all)).collect();

    let mut position = vec![usize::MAX; spec.size];
    for (k, f) in spec.free.iter().enumerate() {
        position[*f] = k;
    }
    let nf = spec.free.len();
    let mut jac_free = DenseMatrix::zeros(nf, nf);
    let mut local_jacs = Vec::with_capacity(ne);
    for (e, local) in locals.iter().enumerate() {
        let jac = local_jacobian(spec.kernel.as_ref(), e, local).map_err(ad)?;
        let n_in = local.len();
        for (r, row) in spec.rows[e].iter().enumerate() {
            let Some(row) = row else { continue };
            let pr = position[*row];
            if pr == usize::MAX {
                continue;
            }
            for (j, (k, i)) in spec.gather.map[e].iter().enumerate() {
                if *k == 0 && position[*i] != usize::MAX {
                    jac_free.data[pr * nf + position[*i]] += jac[r * n_in + j];
                }
            }
        }
        local_jacs.push(jac);
    }
    let lu = implexity_linalg::dense::DenseLu::new(&jac_free)
        .map_err(|e| CaeError::contract(format!("singular implicit tangent: {e}")))?;
    let lens: Vec<usize> = inputs.iter().map(Var::len).collect();
    let gather = spec.gather;
    let rows = spec.rows;
    let free = spec.free;
    let pull: Pullback = Box::new(move |ct: &[f64]| {

        let rhs: Vec<f64> = free.iter().map(|f| ct[*f]).collect();
        let w = lu.solve(&rhs, 1, true).map_err(|e| AdError::Shape(e.to_string()))?;
        let mut grads: Vec<Vec<f64>> = lens.iter().map(|l| vec![0.0; *l]).collect();
        for (e, jac) in local_jacs.iter().enumerate() {
            let n_in = gather.map[e].len();
            for (r, row) in rows[e].iter().enumerate() {
                let Some(row) = row else { continue };
                let pr = position[*row];
                if pr == usize::MAX || w[pr] == 0.0 {
                    continue;
                }
                for (j, (k, i)) in gather.map[e].iter().enumerate() {
                    if *k > 0 {
                        grads[*k - 1][*i] -= w[pr] * jac[r * n_in + j];
                    }
                }
            }
        }
        Ok(grads)
    });
    tape.custom(inputs, y, pull).map_err(ad)
}
