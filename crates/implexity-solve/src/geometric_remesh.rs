// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

fn err(message: &str) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimplexTransfer {
    pub weights: Vec<f64>,
    pub dweights_dpoint: DenseMatrix,
    pub vertices: DenseMatrix,
}



pub fn simplex_barycentric_with_geometry(
    vertices: &DenseMatrix,
    point: &[f64],
) -> CaeResult<SimplexTransfer> {
    let d = vertices.ncols;
    if vertices.nrows != d + 1 || point.len() != d || d == 0 {
        return Err(err("simplex requires d+1 vertices"));
    }
    let mut b = DenseMatrix::zeros(d, d);
    for i in 0..d {
        for j in 0..d {
            b.data[i * d + j] = vertices.get(j + 1, i) - vertices.get(0, i);
        }
    }
    let det = implexity_linalg::dense::det(&b).map_err(|e| err(&e.to_string()))?;
    if det.abs() < 1e-14 {
        return Err(err("degenerate simplex"));
    }
    let binv = implexity_linalg::dense::inv(&b).map_err(|_| err("degenerate simplex"))?;
    let rel: Vec<f64> = (0..d).map(|k| point[k] - vertices.get(0, k)).collect();
    let tail = binv.matvec(&rel).map_err(|e| err(&e.to_string()))?;
    let mut weights = vec![1.0 - tail.iter().sum::<f64>()];
    weights.extend(&tail);
    let mut dw = DenseMatrix::zeros(d + 1, d);
    for k in 0..d {
        dw.data[k] = -(0..d).map(|r| binv.get(r, k)).sum::<f64>();
    }
    dw.data[d..].copy_from_slice(&binv.data);
    Ok(SimplexTransfer { weights, dweights_dpoint: dw, vertices: vertices.clone() })
}



pub fn transfer_value_and_point_gradient(
    values: &[f64],
    transfer: &SimplexTransfer,
) -> CaeResult<(f64, Vec<f64>)> {
    if values.len() != transfer.weights.len() {
        return Err(err("value/simplex mismatch"));
    }
    let value = values.iter().zip(&transfer.weights).map(|(a, b)| a * b).sum();
    let d = transfer.dweights_dpoint.ncols;
    let grad = (0..d)
        .map(|k| values.iter().enumerate().map(|(i, v)| v * transfer.dweights_dpoint.get(i, k)).sum())
        .collect();
    Ok((value, grad))
}

