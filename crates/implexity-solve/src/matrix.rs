// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::fmt;
use std::sync::Arc;

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::{CscMatrix, CsrMatrix};

#[must_use]
pub fn py_shape(rows: usize, cols: usize) -> String {
    format!("({rows}, {cols})")
}

pub trait MatrixAction: Send + Sync {
    fn shape(&self) -> (usize, usize);


    fn matvec(&self, x: &[f64]) -> CaeResult<Vec<f64>>;


    fn rmatvec(&self, y: &[f64]) -> CaeResult<Vec<f64>>;


    fn matmat(&self, x: &DenseMatrix) -> CaeResult<DenseMatrix> {
        columnwise(x, self.shape().0, |c| self.matvec(c))
    }


    fn rmatmat(&self, y: &DenseMatrix) -> CaeResult<DenseMatrix> {
        columnwise(y, self.shape().1, |c| self.rmatvec(c))
    }
}



pub fn columnwise(
    x: &DenseMatrix,
    out_rows: usize,
    f: impl Fn(&[f64]) -> CaeResult<Vec<f64>>,
) -> CaeResult<DenseMatrix> {
    let mut out = DenseMatrix::zeros(out_rows, x.ncols);
    let mut col = vec![0.0; x.nrows];
    for j in 0..x.ncols {
        for (i, c) in col.iter_mut().enumerate() {
            *c = x.data[i * x.ncols + j];
        }
        let y = f(&col)?;
        if y.len() != out_rows {
            return Err(CaeError::contract("matrix action returned a column of the wrong length"));
        }
        for (i, v) in y.into_iter().enumerate() {
            out.data[i * x.ncols + j] = v;
        }
    }
    Ok(out)
}

type VecFn = dyn Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync;

pub struct FnAction {
    shape: (usize, usize),
    forward: Box<VecFn>,
    transpose: Box<VecFn>,
}

impl FnAction {
    pub fn new(
        shape: (usize, usize),
        forward: impl Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync + 'static,
        transpose: impl Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync + 'static,
    ) -> Self {
        Self { shape, forward: Box::new(forward), transpose: Box::new(transpose) }
    }
}

impl MatrixAction for FnAction {
    fn shape(&self) -> (usize, usize) {
        self.shape
    }
    fn matvec(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        (self.forward)(x)
    }
    fn rmatvec(&self, y: &[f64]) -> CaeResult<Vec<f64>> {
        (self.transpose)(y)
    }
}

#[derive(Clone)]
pub enum Jacobian {
    Dense(DenseMatrix),
    Csr(CsrMatrix),
    Csc(CscMatrix),
    Operator(Arc<dyn MatrixAction>),
}

impl fmt::Debug for Jacobian {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dense(m) => write!(f, "Jacobian::Dense{}", py_shape(m.nrows, m.ncols)),
            Self::Csr(m) => write!(f, "Jacobian::Csr{} nnz={}", py_shape(m.nrows(), m.ncols()), m.nnz()),
            Self::Csc(m) => write!(f, "Jacobian::Csc{} nnz={}", py_shape(m.nrows(), m.ncols()), m.nnz()),
            Self::Operator(a) => {
                let (r, c) = a.shape();
                write!(f, "Jacobian::Operator{}", py_shape(r, c))
            }
        }
    }
}

impl From<DenseMatrix> for Jacobian {
    fn from(m: DenseMatrix) -> Self {
        Self::Dense(m)
    }
}
impl From<CsrMatrix> for Jacobian {
    fn from(m: CsrMatrix) -> Self {
        Self::Csr(m)
    }
}
impl From<CscMatrix> for Jacobian {
    fn from(m: CscMatrix) -> Self {
        Self::Csc(m)
    }
}

impl Jacobian {
    #[must_use]
    pub fn shape(&self) -> (usize, usize) {
        match self {
            Self::Dense(m) => (m.nrows, m.ncols),
            Self::Csr(m) => m.shape(),
            Self::Csc(m) => m.shape(),
            Self::Operator(a) => a.shape(),
        }
    }

    #[must_use]
    pub fn is_sparse(&self) -> bool {
        matches!(self, Self::Csr(_) | Self::Csc(_))
    }

    fn finite(&self) -> bool {
        match self {
            Self::Dense(m) => m.data.iter().all(|v| v.is_finite()),
            Self::Csr(m) => m.is_finite(),
            Self::Csc(m) => m.is_finite(),
            Self::Operator(_) => true,
        }
    }



    pub fn apply(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let lin = |e: implexity_linalg::error::LinalgError| CaeError::contract(e.to_string());
        match (self, transpose) {
            (Self::Dense(m), false) => m.matvec(x).map_err(lin),
            (Self::Dense(m), true) => dense_rmatvec(m, x),
            (Self::Csr(m), false) => m.matvec(x).map_err(lin),
            (Self::Csr(m), true) => m.matvec_transpose(x).map_err(lin),
            (Self::Csc(m), false) => m.matvec(x).map_err(lin),
            (Self::Csc(m), true) => m.matvec_transpose(x).map_err(lin),
            (Self::Operator(a), false) => a.matvec(x),
            (Self::Operator(a), true) => a.rmatvec(x),
        }
    }



    pub fn apply_block(&self, x: &DenseMatrix, transpose: bool) -> CaeResult<DenseMatrix> {
        let (r, c) = self.shape();
        let out_rows = if transpose { c } else { r };
        if let Self::Csr(matrix) = self
            && transpose
            && x.ncols > 1
            && x.nrows == r
            && x.nrows.checked_mul(x.ncols) == Some(x.data.len())
        {
            let width = x.ncols;
            let mut out = DenseMatrix::zeros(out_rows, width);
            for i in 0..r {
                let input = &x.data[i * width..(i + 1) * width];
                let (columns, values) = matrix.row(i);
                for (&column, &value) in columns.iter().zip(values) {
                    let output = &mut out.data[column * width..(column + 1) * width];
                    for j in 0..width { output[j] += value * input[j]; }
                }
            }
            return Ok(out);
        }
        match self {
            Self::Operator(a) if transpose => a.rmatmat(x),
            Self::Operator(a) => a.matmat(x),
            _ => columnwise(x, out_rows, |col| self.apply(col, transpose)),
        }
    }



    pub fn to_csr(&self) -> CaeResult<CsrMatrix> {
        match self {
            Self::Dense(m) => CsrMatrix::from_dense(m.nrows, m.ncols, &m.data)
                .map_err(|e| CaeError::contract(e.to_string())),
            Self::Csr(m) => Ok(m.clone()),
            Self::Csc(m) => Ok(m.to_csr()),
            Self::Operator(_) => Err(CaeError::contract("a matrix-free Jacobian cannot be assembled")),
        }
    }
}

fn dense_rmatvec(m: &DenseMatrix, y: &[f64]) -> CaeResult<Vec<f64>> {
    if y.len() != m.nrows {
        return Err(CaeError::contract("transpose product operand has the wrong length"));
    }
    let mut out = vec![0.0; m.ncols];
    for (i, &yi) in y.iter().enumerate() {
        let row = &m.data[i * m.ncols..(i + 1) * m.ncols];
        for (o, &a) in out.iter_mut().zip(row) {
            *o += a * yi;
        }
    }
    Ok(out)
}

#[must_use]
pub fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    match n {
        0 => Vec::new(),
        1 => vec![a],
        _ => {
            let div = (n - 1) as f64;
            let step = (b - a) / div;
            let mut out: Vec<f64> = (0..n).map(|i| a + (i as f64) * step).collect();
            out[n - 1] = b;
            out
        }
    }
}

#[must_use]
pub fn allclose(a: &[f64], b: &[f64], rtol: f64, atol: f64) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= atol + rtol * y.abs())
}




pub fn checked_matrix(
    matrix: Jacobian,
    shape: (usize, usize),
    label: &str,
    allow_operator: bool,
) -> CaeResult<Jacobian> {
    if let Jacobian::Operator(op) = &matrix {
        if !allow_operator {
            return Err(CaeError::contract(format!(
                "{label}: state solve requires an assembled dense/sparse Jacobian"
            )));
        }
        let actual = op.shape();
        if actual != shape {
            return Err(CaeError::contract(format!(
                "{label}: expected shape {}, got {}",
                py_shape(shape.0, shape.1),
                py_shape(actual.0, actual.1)
            )));
        }
        probe_operator(op.as_ref(), shape, label)?;
        return Ok(matrix);
    }
    let actual = matrix.shape();
    if actual != shape || !matrix.finite() {
        return Err(CaeError::contract(format!(
            "{label}: expected finite shape {}, got {}",
            py_shape(shape.0, shape.1),
            py_shape(actual.0, actual.1)
        )));
    }
    Ok(matrix)
}

fn probe_operator(op: &dyn MatrixAction, shape: (usize, usize), label: &str) -> CaeResult<()> {
    let domain = linspace(0.5, 1.5, shape.1);
    let range = linspace(-0.75, 0.25, shape.0);
    let domain2 =
        DenseMatrix { nrows: shape.1, ncols: 2, data: domain.iter().flat_map(|&d| [d, -0.5 * d]).collect() };
    let range2 =
        DenseMatrix { nrows: shape.0, ncols: 2, data: range.iter().flat_map(|&r| [r, 0.25 - r]).collect() };
    let wrap = |e: CaeError| {
        CaeError::contract(format!(
            "{label}: matrix-free Jacobian must provide finite matvec, rmatvec, matmat, and rmatmat actions: {}",
            e.message()
        ))
    };
    let forward = op.matvec(&domain).map_err(wrap)?;
    let reverse = op.rmatvec(&range).map_err(wrap)?;
    let forward_many = op.matmat(&domain2).map_err(wrap)?;
    let reverse_many = op.rmatmat(&range2).map_err(wrap)?;
    if forward.len() != shape.0 || forward.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free matvec returned invalid shape or nonfinite values"
        )));
    }
    if reverse.len() != shape.1 || reverse.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free rmatvec returned invalid shape or nonfinite values"
        )));
    }
    if (forward_many.nrows, forward_many.ncols) != (shape.0, 2)
        || forward_many.data.iter().any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free matmat returned invalid shape or nonfinite values"
        )));
    }
    if (reverse_many.nrows, reverse_many.ncols) != (shape.1, 2)
        || reverse_many.data.iter().any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free rmatmat returned invalid shape or nonfinite values"
        )));
    }
    let first_forward: Vec<f64> = (0..shape.0).map(|i| forward_many.data[i * 2]).collect();
    if !allclose(&first_forward, &forward, 1e-12, 1e-14) {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free matvec and matmat actions are inconsistent"
        )));
    }
    let first_reverse: Vec<f64> = (0..shape.1).map(|i| reverse_many.data[i * 2]).collect();
    if !allclose(&first_reverse, &reverse, 1e-12, 1e-14) {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free rmatvec and rmatmat actions are inconsistent"
        )));
    }
    let fp: f64 = range.iter().zip(&forward).map(|(a, b)| a * b).sum();
    let rp: f64 = domain.iter().zip(&reverse).map(|(a, b)| a * b).sum();
    let scale = 1.0_f64.max(fp.abs()).max(rp.abs());
    if (fp - rp).abs() > 1e-10 * scale {
        return Err(CaeError::contract(format!(
            "{label}: matrix-free forward and transpose actions are inconsistent"
        )));
    }
    Ok(())
}



pub fn checked_product(matrix: &Jacobian, rhs: &[f64], label: &str, transpose: bool) -> CaeResult<Vec<f64>> {
    if rhs.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!("{label}: operand must be a finite vector or matrix")));
    }
    let (r, c) = matrix.shape();
    let (source, target) = if transpose { (r, c) } else { (c, r) };
    if rhs.len() != source {
        return Err(CaeError::contract(format!(
            "{label}: operand leading dimension must be {source}, got {}",
            rhs.len()
        )));
    }
    let value = matrix
        .apply(rhs, transpose)
        .map_err(|e| CaeError::contract(format!("{label}: Jacobian action failed: {}", e.message())))?;
    if value.len() != target || value.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!(
            "{label}: Jacobian action returned expected finite shape ({target},), got ({},)",
            value.len()
        )));
    }
    Ok(value)
}



pub fn checked_product_block(
    matrix: &Jacobian,
    rhs: &DenseMatrix,
    label: &str,
    transpose: bool,
) -> CaeResult<DenseMatrix> {
    if rhs.data.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!("{label}: operand must be a finite vector or matrix")));
    }
    let (r, c) = matrix.shape();
    let (source, target) = if transpose { (r, c) } else { (c, r) };
    if rhs.nrows != source {
        return Err(CaeError::contract(format!(
            "{label}: operand leading dimension must be {source}, got {}",
            rhs.nrows
        )));
    }
    let value = matrix
        .apply_block(rhs, transpose)
        .map_err(|e| CaeError::contract(format!("{label}: Jacobian action failed: {}", e.message())))?;
    if (value.nrows, value.ncols) != (target, rhs.ncols) || value.data.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!(
            "{label}: Jacobian action returned expected finite shape {}, got {}",
            py_shape(target, rhs.ncols),
            py_shape(value.nrows, value.ncols)
        )));
    }
    Ok(value)
}

#[must_use]
pub fn eliminate_zeros(m: &CsrMatrix) -> CsrMatrix {
    let (nrows, ncols) = m.shape();
    let mut indptr = Vec::with_capacity(nrows + 1);
    let mut indices = Vec::with_capacity(m.nnz());
    let mut data = Vec::with_capacity(m.nnz());
    indptr.push(0);
    for i in 0..nrows {
        let (cols, vals) = m.row(i);
        for (&c, &v) in cols.iter().zip(vals) {
            if v != 0.0 {
                indices.push(c);
                data.push(v);
            }
        }
        indptr.push(indices.len());
    }
    CsrMatrix::try_new(nrows, ncols, indptr, indices, data).unwrap_or_else(|_| m.clone())
}

