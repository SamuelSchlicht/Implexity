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
pub struct FieldPort {
    pub name: String,
    pub field_indices: Vec<i64>,
    pub weights: Vec<f64>,
    pub orientation: f64,
    pub conserved_quantity: String,
}

impl FieldPort {


    pub fn new(
        name: &str,
        field_indices: Vec<i64>,
        weights: Vec<f64>,
        orientation: f64,
        conserved_quantity: &str,
    ) -> CaeResult<Self> {
        if field_indices.len() != weights.len() || field_indices.is_empty() {
            return Err(err("field_indices and weights must be non-empty and equally sized"));
        }
        if !orientation.is_finite() || orientation == 0.0 {
            return Err(err("orientation must be finite and non-zero"));
        }
        Ok(Self {
            name: name.into(),
            field_indices,
            weights,
            orientation,
            conserved_quantity: conserved_quantity.into(),
        })
    }

    fn checked(&self, size: usize) -> CaeResult<(Vec<usize>, f64)> {
        let idx: Vec<usize> = self
            .field_indices
            .iter()
            .map(|&i| usize::try_from(i).ok().filter(|&i| i < size))
            .collect::<Option<_>>()
            .ok_or_else(|| err("field port index out of bounds"))?;
        let sw: f64 = self.weights.iter().sum();
        Ok((idx, sw))
    }
}



pub fn sample_field(field: &[f64], port: &FieldPort) -> CaeResult<f64> {
    let (idx, sw) = port.checked(field.len())?;
    if sw.abs() <= f64::MIN_POSITIVE {
        return Err(err("field port weights sum to zero"));
    }
    Ok(idx.iter().zip(&port.weights).map(|(&i, w)| w * field[i]).sum::<f64>() / sw)
}



pub fn scatter_port(value: f64, size: usize, port: &FieldPort) -> CaeResult<Vec<f64>> {
    let (idx, sw) = port.checked(size)?;
    let mut out = vec![0.0; size];
    for (&i, w) in idx.iter().zip(&port.weights) {
        out[i] += value * w / sw;
    }
    Ok(out)
}



pub fn port_adjoint_identity(field: &[f64], port: &FieldPort, scalar: f64) -> CaeResult<f64> {
    let lhs = sample_field(field, port)? * scalar;
    let rhs: f64 = field.iter().zip(scatter_port(scalar, field.len(), port)?).map(|(a, b)| a * b).sum();
    Ok(lhs - rhs)
}



pub fn incidence_matrix(edges: &[(i64, i64)], node_count: usize) -> CaeResult<DenseMatrix> {
    let mut b = DenseMatrix::zeros(node_count, edges.len());
    for (j, &(a, h)) in edges.iter().enumerate() {
        let (Some(a), Some(h)) = (
            usize::try_from(a).ok().filter(|&a| a < node_count),
            usize::try_from(h).ok().filter(|&h| h < node_count),
        ) else {
            return Err(err("invalid network edge"));
        };
        if a == h {
            return Err(err("invalid network edge"));
        }
        b.data[a * edges.len() + j] -= 1.0;
        b.data[h * edges.len() + j] += 1.0;
    }
    Ok(b)
}



pub fn network_conservation_residual(
    edge_fluxes: &[f64],
    edges: &[(i64, i64)],
    node_sources: &[f64],
    node_count: Option<usize>,
) -> CaeResult<Vec<f64>> {
    let n = node_count.unwrap_or(node_sources.len());
    if node_sources.len() != n {
        return Err(err("node source count mismatch"));
    }
    let b = incidence_matrix(edges, n)?;
    if edge_fluxes.len() != b.ncols {
        return Err(err("edge flux count mismatch"));
    }
    let bq = b.matvec(edge_fluxes).map_err(|e| err(&e.to_string()))?;
    Ok(bq.iter().zip(node_sources).map(|(a, s)| a + s).collect())
}

