// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::spatial::Delaunay;
use serde_json::{Value, json};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransferOperator {
    pub matrix: DenseMatrix,
    pub source_weights: Vec<f64>,
    pub target_weights: Vec<f64>,
}

impl TransferOperator {
    fn validate(&self) -> CaeResult<()> {
        if self.source_weights.len() != self.matrix.ncols || self.target_weights.len() != self.matrix.nrows {
            return Err(err("incompatible transfer matrix/weights"));
        }
        if self.source_weights.iter().chain(&self.target_weights).any(|w| !w.is_finite() || *w <= 0.0) {
            return Err(err("weights must be finite and positive"));
        }
        if self.matrix.data.len() != self.matrix.nrows.checked_mul(self.matrix.ncols).ok_or_else(|| err("transfer matrix dimensions overflow"))? || self.matrix.data.iter().any(|v| !v.is_finite()) {
            return Err(err("transfer matrix must have compatible finite entries"));
        }
        Ok(())
    }



    pub fn prolong(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        self.validate()?;
        self.matrix.matvec(x).map_err(|e| err(e.to_string()))
    }



    pub fn adjoint(&self, y: &[f64]) -> CaeResult<Vec<f64>> {
        self.validate()?;
        if y.len() != self.target_weights.len() {
            return Err(err("target cotangent has the wrong length"));
        }
        let weighted: Vec<f64> = y.iter().zip(&self.target_weights).map(|(a, w)| a * w).collect();
        let out = self.matrix.transpose().matvec(&weighted).map_err(|e| err(e.to_string()))?;
        Ok(out.iter().zip(&self.source_weights).map(|(v, w)| v / w).collect())
    }
}



pub fn barycentric_transfer(
    source_points: &[f64],
    target_points: &[f64],
    dim: usize,
    source_weights: Option<&[f64]>,
    target_weights: Option<&[f64]>,
) -> CaeResult<TransferOperator> {
    if dim == 0 || !source_points.len().is_multiple_of(dim) || !target_points.len().is_multiple_of(dim) {
        return Err(err("point dimensions mismatch"));
    }
    let (ns, nt) = (source_points.len() / dim, target_points.len() / dim);
    let tri = Delaunay::new(source_points, dim).map_err(|e| err(e.to_string()))?;
    let mut t = DenseMatrix::zeros(nt, ns);
    for (k, p) in target_points.chunks(dim).enumerate() {
        let s = tri
            .find_simplex(p)
            .map_err(|e| err(e.to_string()))?
            .ok_or_else(|| err("target point outside source convex hull"))?;
        let bary = tri.barycentric(s, p).map_err(|e| err(e.to_string()))?;
        for (&v, b) in tri.simplices[s].iter().zip(bary) {
            t.data[k * ns + v] = b;
        }
    }
    let sw = source_weights.map_or_else(|| vec![1.0; ns], <[f64]>::to_vec);
    let tw = target_weights.map_or_else(|| vec![1.0; nt], <[f64]>::to_vec);
    if sw.len() != ns || tw.len() != nt {
        return Err(err("point dimensions mismatch"));
    }
    if sw.iter().chain(&tw).any(|w| !w.is_finite() || *w <= 0.0) {
        return Err(err("weights must be finite and positive"));
    }
    Ok(TransferOperator { matrix: t, source_weights: sw, target_weights: tw })
}



pub fn conservative_correction(op: &TransferOperator) -> CaeResult<TransferOperator> {
    op.validate()?;
    let source: f64 = op.source_weights.iter().sum();
    let target: f64 = op.target_weights.iter().sum();
    if !source.is_finite() || source <= 0.0 || !target.is_finite() || target <= 0.0 {
        return Err(err("invalid total transfer measure"));
    }
    let corrected = TransferOperator {
        matrix: op.matrix.clone(),
        source_weights: op.source_weights.clone(),
        target_weights: op.target_weights.iter().map(|w| w * (source / target)).collect(),
    };
    corrected.validate()?;
    Ok(corrected)
}



pub fn transfer_diagnostics(op: &TransferOperator) -> CaeResult<Value> {
    let ones = vec![1.0; op.matrix.ncols];
    let pout = op.prolong(&ones)?;
    let source: f64 = op.source_weights.iter().sum();
    let target: f64 = op.target_weights.iter().sum();
    Ok(json!({
        "partition_unity_error": pout.iter().map(|v| (v - 1.0).abs()).fold(f64::NEG_INFINITY, f64::max),
        "source_measure": source,
        "target_measure": target,
        "measure_mismatch": target - source,
    }))
}

