// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
