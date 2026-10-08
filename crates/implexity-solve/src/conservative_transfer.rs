// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use ndarray::{ArrayD, Axis, IxDyn};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransferOperator {
    matrix: DenseMatrix,
    source_measure: Vec<f64>,
    target_measure: Vec<f64>,
}

impl TransferOperator {


    pub fn new(matrix: DenseMatrix, source_measure: Vec<f64>, target_measure: Vec<f64>) -> CaeResult<Self> {
        if source_measure.len() != matrix.ncols || target_measure.len() != matrix.nrows {
            return Err(err("incompatible transfer matrix/measures"));
        }
        if source_measure.iter().chain(&target_measure).any(|m| !m.is_finite() || *m <= 0.0) {
            return Err(err("cell measures must be finite and positive"));
        }
        if matrix.data.len() != matrix.nrows.checked_mul(matrix.ncols).ok_or_else(|| err("transfer matrix dimensions overflow"))? || matrix.data.iter().any(|v| !v.is_finite()) {
            return Err(err("transfer matrix must have compatible finite entries"));
        }
        Ok(Self { matrix, source_measure, target_measure })
    }

    #[must_use]
    pub fn matrix(&self) -> &DenseMatrix {
        &self.matrix
    }

    #[must_use]
    pub fn source_measure(&self) -> &[f64] {
        &self.source_measure
    }

    #[must_use]
    pub fn target_measure(&self) -> &[f64] {
        &self.target_measure
    }



    pub fn apply(&self, source: &[f64]) -> CaeResult<Vec<f64>> {
        self.matrix.matvec(source).map_err(|e| err(e.to_string()))
    }



    pub fn adjoint(&self, target_cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        self.matrix.transpose().matvec(target_cotangent).map_err(|e| err(e.to_string()))
    }



    pub fn conservative_adjoint(&self, target_cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        if target_cotangent.len() != self.target_measure.len() {
            return Err(err("target cotangent has the wrong length"));
        }
        let weighted: Vec<f64> =
            target_cotangent.iter().zip(&self.target_measure).map(|(g, m)| m * g).collect();
        let out = self.adjoint(&weighted)?;
        Ok(out.iter().zip(&self.source_measure).map(|(v, m)| v / m).collect())
    }



    pub fn integral_error(&self, source: &[f64]) -> CaeResult<f64> {
        let t = self.apply(source)?;
        let target: f64 = self.target_measure.iter().zip(&t).map(|(a, b)| a * b).sum();
        let src: f64 = self.source_measure.iter().zip(source).map(|(a, b)| a * b).sum();
        Ok(target - src)
    }
}



pub fn overlap_transfer(
    overlap: &DenseMatrix,
    source_measure: &[f64],
    target_measure: &[f64],
    tolerance: f64,
) -> CaeResult<TransferOperator> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(err("overlap tolerance must be finite and non-negative"));
    }
    let (nt, ns) = (overlap.nrows, overlap.ncols);
    if overlap.data.len() != nt.checked_mul(ns).ok_or_else(|| err("overlap dimensions overflow"))? || overlap.data.iter().any(|v| !v.is_finite()) {
        return Err(err("overlap measures must have compatible finite entries"));
    }
    if source_measure.iter().chain(target_measure).any(|m| !m.is_finite() || *m <= 0.0) {
        return Err(err("cell measures must be finite and positive"));
    }
    if source_measure.len() != ns || target_measure.len() != nt {
        return Err(err("overlap dimensions mismatch"));
    }
    if overlap.data.iter().any(|v| *v < -tolerance) {
        return Err(err("overlap measures must be non-negative"));
    }
    let max_of = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let column_error = (0..ns)
        .map(|s| ((0..nt).map(|t| overlap.get(t, s)).sum::<f64>() - source_measure[s]).abs())
        .fold(f64::NEG_INFINITY, f64::max);
    if column_error > tolerance * max_of(source_measure).max(1.0) {
        return Err(err("overlaps do not cover each source measure conservatively"));
    }
    let row_error = (0..nt)
        .map(|t| (overlap.data[t * ns..(t + 1) * ns].iter().sum::<f64>() - target_measure[t]).abs())
        .fold(f64::NEG_INFINITY, f64::max);
    if row_error > tolerance * max_of(target_measure).max(1.0) {
        return Err(err("overlaps do not cover each target measure conservatively"));
    }
    let mut a = overlap.clone();
    for (row, m) in a.data.chunks_mut(ns.max(1)).zip(target_measure) {
        for v in row {
            *v /= m;
        }
    }
    TransferOperator::new(a, source_measure.to_vec(), target_measure.to_vec())
}

#[allow(clippy::cast_precision_loss)]
fn inverse(r: usize) -> f64 {
    1.0 / r as f64
}



pub fn uniform_refine_1d(source_count: usize, factor: usize) -> CaeResult<TransferOperator> {
    let (n, r) = (source_count, factor);
    if n < 1 || r < 1 {
        return Err(err("source_count and factor must be positive"));
    }
    let mut o = DenseMatrix::zeros(n * r, n);
    for i in 0..n {
        for k in i * r..(i + 1) * r {
            o.data[k * n + i] = inverse(r);
        }
    }
    overlap_transfer(&o, &vec![1.0; n], &vec![inverse(r); n * r], 1e-10)
}



pub fn uniform_coarsen_1d(fine_count: usize, factor: usize) -> CaeResult<TransferOperator> {
    let (nf, r) = (fine_count, factor);
    if nf < 1 || r < 1 || nf % r != 0 {
        return Err(err("fine_count must be divisible by factor"));
    }
    let nc = nf / r;
    let mut o = DenseMatrix::zeros(nc, nf);
    for i in 0..nc {
        for k in i * r..(i + 1) * r {
            o.data[i * nf + k] = inverse(r);
        }
    }
    overlap_transfer(&o, &vec![inverse(r); nf], &vec![1.0; nc], 1e-10)
}

fn factors_for(ndim: usize, factors: &[usize]) -> CaeResult<Vec<usize>> {
    let f = if factors.len() == 1 { vec![factors[0]; ndim] } else { factors.to_vec() };
    if f.len() != ndim || f.contains(&0) {
        return Err(err("invalid refinement factor"));
    }
    Ok(f)
}



pub fn uniform_refine_nd(source: &ArrayD<f64>, factor: &[usize]) -> CaeResult<ArrayD<f64>> {
    let factors = factors_for(source.ndim(), factor)?;
    let shape: Vec<usize> = source.shape().iter().zip(&factors).map(|(n, r)| n * r).collect();
    Ok(ArrayD::from_shape_fn(IxDyn(&shape), |idx| {
        let src: Vec<usize> = (0..factors.len()).map(|a| idx[a] / factors[a]).collect();
        source[IxDyn(&src)]
    }))
}



pub fn uniform_refine_nd_adjoint(
    target_cotangent: &ArrayD<f64>,
    source_shape: &[usize],
    factor: &[usize],
    volume_weighted: bool,
) -> CaeResult<ArrayD<f64>> {
    let factors = factors_for(source_shape.len(), factor)?;
    let expected: Vec<usize> = source_shape.iter().zip(&factors).map(|(n, r)| n * r).collect();
    if target_cotangent.shape() != expected.as_slice() {
        let tuple = |v: &[usize]| {
            let parts: Vec<String> = v.iter().map(ToString::to_string).collect();
            if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
        };
        return Err(err(format!(
            "target cotangent shape {} != expected {}",
            tuple(target_cotangent.shape()),
            tuple(&expected)
        )));
    }
    let mut out = target_cotangent.clone();
    for ax in (0..source_shape.len()).rev() {
        let mut shape = out.shape().to_vec();
        shape.splice(ax..=ax, [source_shape[ax], factors[ax]]);
        let reshaped = out.into_shape_with_order(IxDyn(&shape)).map_err(|e| err(e.to_string()))?;
        out = reshaped.sum_axis(Axis(ax + 1));
    }
    if volume_weighted {
        #[allow(clippy::cast_precision_loss)]
        let scale = factors.iter().product::<usize>() as f64;
        out.mapv_inplace(|v| v / scale);
    }
    Ok(out)
}



pub fn uniform_coarsen_nd(fine: &ArrayD<f64>, factor: &[usize]) -> CaeResult<ArrayD<f64>> {
    let ndim = fine.ndim();
    let factors = if factor.len() == 1 { vec![factor[0]; ndim] } else { factor.to_vec() };
    if factors.len() != ndim || factors.iter().zip(fine.shape()).any(|(r, n)| *r < 1 || n % r != 0) {
        return Err(err("fine shape must be divisible by factor"));
    }
    let shape: Vec<usize> = fine.shape().iter().zip(&factors).flat_map(|(n, r)| [n / r, *r]).collect();
    let mut out = fine.clone().into_shape_with_order(IxDyn(&shape)).map_err(|e| err(e.to_string()))?;
    for ax in (1..2 * ndim).step_by(2).rev() {
        out = out.mean_axis(Axis(ax)).ok_or_else(|| err("fine shape must be divisible by factor"))?;
    }
    Ok(out)
}

