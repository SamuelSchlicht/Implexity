// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use implexity_linalg::CscMatrix;
use implexity_linalg::lu::{CholeskySymbolic, LuSymbolic, Parallelism, SparseCholesky, SparseLu};

use super::grid::flat;

pub struct CellPattern {
    pub shape: [usize; 3],
    pub n: usize,
    pub indptr: Vec<usize>,
    pub indices: Vec<usize>,
    pub diag: Vec<usize>,
    chol: OnceLock<Result<CholeskySymbolic, String>>,
    lu: OnceLock<Result<LuSymbolic, String>>,
}

impl std::fmt::Debug for CellPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CellPattern({:?})", self.shape)
    }
}

fn cache() -> &'static Mutex<HashMap<[usize; 3], Arc<CellPattern>>> {
    static C: OnceLock<Mutex<HashMap<[usize; 3], Arc<CellPattern>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

impl CellPattern {
    #[must_use]
    pub fn of(shape: [usize; 3]) -> Arc<Self> {
        let mut c = match cache().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if c.len() > 16 {
            c.clear();
        }
        Arc::clone(c.entry(shape).or_insert_with(|| Arc::new(Self::build(shape))))
    }

    fn build(s: [usize; 3]) -> Self {
        let n = s[0] * s[1] * s[2];
        let mut indptr = Vec::with_capacity(n + 1);
        let mut indices = Vec::with_capacity(7 * n);
        let mut diag = Vec::with_capacity(n);
        indptr.push(0);
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    let mut row = vec![flat(s, i, j, k)];
                    let p = [i, j, k];
                    for a in 0..3 {
                        if p[a] > 0 {
                            let mut q = p;
                            q[a] -= 1;
                            row.push(flat(s, q[0], q[1], q[2]));
                        }
                        if p[a] + 1 < s[a] {
                            let mut q = p;
                            q[a] += 1;
                            row.push(flat(s, q[0], q[1], q[2]));
                        }
                    }
                    row.sort_unstable();
                    let me = flat(s, i, j, k);
                    diag.push(indices.len() + row.iter().position(|&c| c == me).unwrap_or(0));
                    indices.extend_from_slice(&row);
                    indptr.push(indices.len());
                }
            }
        }
        Self { shape: s, n, indptr, indices, diag, chol: OnceLock::new(), lu: OnceLock::new() }
    }

    #[must_use]
    pub fn slot(&self, i: usize, j: usize) -> usize {
        let row = &self.indices[self.indptr[i]..self.indptr[i + 1]];
        self.indptr[i] + row.partition_point(|&c| c < j)
    }

    #[must_use]
    pub fn zeros(&self) -> Vec<f64> {
        vec![0.0; self.indices.len()]
    }

    fn csc_of(&self, data: Vec<f64>) -> Result<CscMatrix, String> {
        CscMatrix::try_new(self.n, self.n, self.indptr.clone(), self.indices.clone(), data)
            .map_err(|e| e.to_string())
    }

    pub fn cholesky(&self, data: Vec<f64>) -> Result<SparseCholesky, String> {
        let a = self.csc_of(data)?;
        let sym = self
            .chol
            .get_or_init(|| CholeskySymbolic::analyze(&a).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(Clone::clone)?;
        sym.factor(&a, Parallelism::Sequential).map_err(|e| e.to_string())
    }

    pub fn lu_of_transpose(&self, data: Vec<f64>) -> Result<SparseLu, String> {
        let a = self.csc_of(data)?;
        let sym = self
            .lu
            .get_or_init(|| LuSymbolic::analyze(&a).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(Clone::clone)?;
        sym.factor(&a, Parallelism::Sequential).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn matvec(&self, data: &[f64], x: &[f64]) -> Vec<f64> {
        (0..self.n)
            .map(|i| (self.indptr[i]..self.indptr[i + 1]).map(|p| data[p] * x[self.indices[p]]).sum())
            .collect()
    }
}
