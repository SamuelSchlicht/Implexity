// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CscMatrix;
use sha2::{Digest, Sha256};

use crate::matrix::{Jacobian, py_shape};

const IDENTITY_SCHEMA: &[u8] = b"implexity-exact-linear-matrix/1\0";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExactMatrixIdentity {
    pub sha256: String,
    pub size: usize,
    pub storage: &'static str,
    pub entries: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalMatrix {
    Sparse(CscMatrix),
    Dense(DenseMatrix),
}

impl CanonicalMatrix {
    #[must_use]
    pub fn size(&self) -> usize {
        match self {
            Self::Sparse(m) => m.nrows(),
            Self::Dense(m) => m.nrows,
        }
    }



    pub fn apply(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let lin = |e: implexity_linalg::error::LinalgError| CaeError::contract(e.to_string());
        match (self, transpose) {
            (Self::Sparse(m), false) => m.matvec(x).map_err(lin),
            (Self::Sparse(m), true) => m.matvec_transpose(x).map_err(lin),
            (Self::Dense(m), false) => m.matvec(x).map_err(lin),
            (Self::Dense(m), true) => Jacobian::Dense(m.clone()).apply(x, true),
        }
    }

    #[must_use]
    pub fn is_sparse(&self) -> bool {
        matches!(self, Self::Sparse(_))
    }

    #[must_use]
    pub fn entries(&self) -> usize {
        match self {
            Self::Sparse(m) => m.nnz(),
            Self::Dense(m) => m.data.len(),
        }
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        match self {
            Self::Sparse(m) => {
                let width = index_width(m);
                m.nnz() * 8 + (m.nnz() + m.indptr().len()) * width
            }
            Self::Dense(m) => m.data.len() * 8,
        }
    }
}

fn index_width(m: &CscMatrix) -> usize {
    let max = m.nnz().max(m.nrows()).max(m.ncols());
    if i32::try_from(max).is_ok() { 4 } else { 8 }
}

#[derive(Clone, Debug)]
pub struct AdmittedExactMatrix {
    matrix: Arc<CanonicalMatrix>,
    identity: ExactMatrixIdentity,
}

impl AdmittedExactMatrix {
    #[must_use]
    pub fn matrix(&self) -> &CanonicalMatrix {
        &self.matrix
    }
    #[must_use]
    pub fn shared(&self) -> Arc<CanonicalMatrix> {
        Arc::clone(&self.matrix)
    }
    #[must_use]
    pub fn identity(&self) -> &ExactMatrixIdentity {
        &self.identity
    }
    #[must_use]
    pub fn size(&self) -> usize {
        self.identity.size
    }
    #[must_use]
    pub fn same_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.matrix, &other.matrix)
    }
}

fn hash_array(digest: &mut Sha256, label: &[u8], dtype: &str, shape: &[usize], bytes: &[u8]) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update((dtype.len() as u64).to_be_bytes());
    digest.update(dtype.as_bytes());
    digest.update((shape.len() as u64).to_be_bytes());
    for &e in shape {
        digest.update((e as u64).to_be_bytes());
    }
    digest.update(bytes);
}

fn index_bytes(values: &[usize], width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * width);
    for &v in values {
        if width == 4 {
            out.extend_from_slice(&u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes());
        } else {
            out.extend_from_slice(&(v as u64).to_le_bytes());
        }
    }
    out
}

fn f64_bytes(values: &[f64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn identity_of(matrix: &CanonicalMatrix) -> ExactMatrixIdentity {
    let size = matrix.size();
    let mut digest = Sha256::new();
    digest.update(IDENTITY_SCHEMA);
    digest.update((size as u64).to_be_bytes());
    let (storage, entries) = match matrix {
        CanonicalMatrix::Sparse(m) => {
            let storage = "sparse_csc_canonical_float64";
            digest.update(storage.as_bytes());
            digest.update(b"\0");
            let width = index_width(m);
            let dtype = if width == 4 { "<i4" } else { "<i8" };
            hash_array(&mut digest, b"indptr", dtype, &[m.indptr().len()], &index_bytes(m.indptr(), width));
            hash_array(
                &mut digest,
                b"indices",
                dtype,
                &[m.indices().len()],
                &index_bytes(m.indices(), width),
            );
            hash_array(&mut digest, b"data", "<f8", &[m.nnz()], &f64_bytes(m.data()));
            (storage, m.nnz())
        }
        CanonicalMatrix::Dense(m) => {
            let storage = "dense_c_float64";
            digest.update(storage.as_bytes());
            digest.update(b"\0");
            hash_array(&mut digest, b"data", "<f8", &[m.nrows, m.ncols], &f64_bytes(&m.data));
            (storage, m.data.len())
        }
    };
    ExactMatrixIdentity { sha256: hex::encode(digest.finalize()), size, storage, entries }
}

fn check_square(rows: usize, cols: usize, size: Option<usize>) -> CaeResult<()> {
    if rows != cols || rows < 1 {
        return Err(CaeError::contract("exact linear matrix must be finite, nonempty, and square"));
    }
    if let Some(n) = size
        && rows != n
    {
        return Err(CaeError::contract(format!(
            "exact linear matrix expected shape {}, got {}",
            py_shape(n, n),
            py_shape(rows, cols)
        )));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum MatrixInput {
    Jacobian(Jacobian),
    Admitted(AdmittedExactMatrix),
}

impl From<Jacobian> for MatrixInput {
    fn from(j: Jacobian) -> Self {
        Self::Jacobian(j)
    }
}
impl From<AdmittedExactMatrix> for MatrixInput {
    fn from(a: AdmittedExactMatrix) -> Self {
        Self::Admitted(a)
    }
}
impl From<&AdmittedExactMatrix> for MatrixInput {
    fn from(a: &AdmittedExactMatrix) -> Self {
        Self::Admitted(a.clone())
    }
}



pub fn admit_exact_matrix(
    matrix: impl Into<MatrixInput>,
    size: Option<usize>,
) -> CaeResult<AdmittedExactMatrix> {
    match matrix.into() {
        MatrixInput::Admitted(a) => {
            if let Some(n) = size
                && a.identity.size != n
            {
                return Err(CaeError::contract(format!(
                    "exact linear matrix expected shape {}, got {}",
                    py_shape(n, n),
                    py_shape(a.identity.size, a.identity.size)
                )));
            }
            Ok(a)
        }
        MatrixInput::Jacobian(j) => {
            let canonical = match j {
                Jacobian::Csr(m) => {
                    check_square(m.nrows(), m.ncols(), size)?;
                    if !m.is_finite() {
                        return Err(CaeError::convergence("exact linear matrix contains nonfinite values"));
                    }
                    CanonicalMatrix::Sparse(m.to_csc())
                }
                Jacobian::Csc(m) => {
                    check_square(m.nrows(), m.ncols(), size)?;
                    if !m.is_finite() {
                        return Err(CaeError::convergence("exact linear matrix contains nonfinite values"));
                    }
                    CanonicalMatrix::Sparse(m)
                }
                Jacobian::Dense(m) => {
                    check_square(m.nrows, m.ncols, size)?;
                    if m.data.iter().any(|v| !v.is_finite()) {
                        return Err(CaeError::convergence("exact linear matrix contains nonfinite values"));
                    }
                    CanonicalMatrix::Dense(m)
                }
                Jacobian::Operator(_) => {
                    return Err(CaeError::contract(
                        "exact linear matrix cannot be represented as finite real values: matrix-free operator",
                    ));
                }
            };
            let identity = identity_of(&canonical);
            Ok(AdmittedExactMatrix { matrix: Arc::new(canonical), identity })
        }
    }
}



pub fn exact_matrix_identity(
    matrix: impl Into<MatrixInput>,
    size: Option<usize>,
) -> CaeResult<ExactMatrixIdentity> {
    Ok(admit_exact_matrix(matrix, size)?.identity)
}

