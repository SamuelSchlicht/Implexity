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

use crate::{certificate::stable_l2, sparse_low_rank::SparseLowRankLimits};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{
    dense::{DenseLu, DenseMatrix},
    lu::SparseCholesky,
    sparse::CscMatrix,
};

pub struct SparseSpdLowRankFactor {
    matrix: CscMatrix,
    base: SparseCholesky,
    u: DenseMatrix,
    v: DenseMatrix,
    inverse_u: DenseMatrix,
    inverse_v: DenseMatrix,
    core: Option<DenseLu>,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn finite(v: &[f64]) -> CaeResult<()> {
    if v.iter().any(|v| !v.is_finite()) {
        return Err(fail("SPD low-rank nonfinite arithmetic"));
    }
    Ok(())
}
impl SparseSpdLowRankFactor {
    pub fn new(
        matrix: CscMatrix,
        u: DenseMatrix,
        v: DenseMatrix,
        limits: SparseLowRankLimits,
    ) -> CaeResult<Self> {
        let n = matrix.nrows();
        let r = u.ncols;
        let entries = n
            .checked_mul(r)
            .ok_or_else(|| fail("SPD low-rank size overflow"))?;
        let dense = entries
            .checked_mul(4)
            .and_then(|x| {
                r.checked_mul(r)
                    .and_then(|k| k.checked_mul(2))
                    .and_then(|k| x.checked_add(k))
            })
            .ok_or_else(|| fail("SPD low-rank storage overflow"))?;
        if n == 0
            || matrix.ncols() != n
            || u.nrows != n
            || v.nrows != n
            || v.ncols != r
            || u.data.len() != entries
            || v.data.len() != entries
            || limits.maximum_size == 0
            || limits.maximum_base_nonzeros == 0
            || limits.maximum_rank == 0
            || limits.maximum_dense_entries == 0
            || n > limits.maximum_size
            || matrix.nnz() > limits.maximum_base_nonzeros
            || r > limits.maximum_rank
            || dense > limits.maximum_dense_entries
        {
            return Err(fail("SPD low-rank shape or cost bounds"));
        }
        finite(matrix.data())?;
        finite(&u.data)?;
        finite(&v.data)?;
        for col in 0..n {
            for k in matrix.indptr()[col]..matrix.indptr()[col + 1] {
                let row = matrix.indices()[k];
                if matrix.data()[k] != matrix.get(col, row) {
                    return Err(fail("SPD low-rank base must be exactly symmetric"));
                }
            }
        }
        let base = SparseCholesky::new(&matrix).map_err(|e| fail(e.to_string()))?;
        let mut w = vec![0.; entries];
        let mut z = vec![0.; entries];
        for j in 0..r {
            let mut a = (0..n).map(|i| u.data[i * r + j]).collect::<Vec<_>>();
            let mut b = (0..n).map(|i| v.data[i * r + j]).collect::<Vec<_>>();
            base.solve_in_place(&mut a)
                .map_err(|e| fail(e.to_string()))?;
            base.solve_in_place(&mut b)
                .map_err(|e| fail(e.to_string()))?;
            for i in 0..n {
                w[i * r + j] = a[i];
                z[i * r + j] = b[i];
            }
        }
        finite(&w)?;
        finite(&z)?;
        let core = if r == 0 {
            None
        } else {
            let mut k = DenseMatrix::identity(r);
            for i in 0..r {
                for j in 0..r {
                    for row in 0..n {
                        k.data[i * r + j] += v.data[row * r + i] * w[row * r + j];
                    }
                }
            }
            finite(&k.data)?;
            Some(DenseLu::new(&k).map_err(|e| fail(e.to_string()))?)
        };
        Ok(Self {
            matrix,
            base,
            u,
            v,
            inverse_u: DenseMatrix::new(n, r, w).map_err(|e| fail(e.to_string()))?,
            inverse_v: DenseMatrix::new(n, r, z).map_err(|e| fail(e.to_string()))?,
            core,
        })
    }
    pub fn size(&self) -> usize {
        self.matrix.nrows()
    }
    pub fn rank(&self) -> usize {
        self.u.ncols
    }
    pub fn numeric_and_matrix_bytes(&self) -> usize {
        self.matrix.nbytes()
            + self.base.nbytes()
            + (self.u.data.len()
                + self.v.data.len()
                + self.inverse_u.data.len()
                + self.inverse_v.data.len()
                + 2 * self.rank() * self.rank())
                * 8
    }
    pub fn apply(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if x.len() != self.size() {
            return Err(fail("SPD low-rank operand shape"));
        }
        finite(x)?;
        let mut y = self.matrix.matvec(x).map_err(|e| fail(e.to_string()))?;
        let (a, b) = if transpose {
            (&self.v, &self.u)
        } else {
            (&self.u, &self.v)
        };
        let r = self.rank();
        let mut p = vec![0.; r];
        for i in 0..self.size() {
            for j in 0..r {
                p[j] += b.data[i * r + j] * x[i];
            }
        }
        for i in 0..self.size() {
            for j in 0..r {
                y[i] += a.data[i * r + j] * p[j];
            }
        }
        finite(&y)?;
        Ok(y)
    }
    pub fn solve(&self, rhs: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if rhs.len() != self.size() {
            return Err(fail("SPD low-rank rhs shape"));
        }
        finite(rhs)?;
        let mut y = rhs.to_vec();
        self.base
            .solve_in_place(&mut y)
            .map_err(|e| fail(e.to_string()))?;
        if let Some(core) = &self.core {
            let (p, c) = if transpose {
                (&self.u, &self.inverse_v)
            } else {
                (&self.v, &self.inverse_u)
            };
            let r = self.rank();
            let mut b = vec![0.; r];
            for i in 0..self.size() {
                for j in 0..r {
                    b[j] += p.data[i * r + j] * y[i];
                }
            }
            let a = core
                .solve(&b, 1, transpose)
                .map_err(|e| fail(e.to_string()))?;
            for i in 0..self.size() {
                for j in 0..r {
                    y[i] -= c.data[i * r + j] * a[j];
                }
            }
        }
        finite(&y)?;
        Ok(y)
    }
    pub fn solve_checked(
        &self,
        rhs: &[f64],
        transpose: bool,
        atol: f64,
        rtol: f64,
    ) -> CaeResult<Vec<f64>> {
        if !atol.is_finite() || atol < 0. || !rtol.is_finite() || rtol < 0. {
            return Err(fail("SPD low-rank solve tolerance"));
        }
        let norm = |a: &[f64]| stable_l2(a).map_err(fail);
        let bound = atol + rtol * norm(rhs)?;
        if !bound.is_finite() {
            return Err(fail("SPD low-rank residual bound overflow"));
        }
        let mut x = self.solve(rhs, transpose)?;
        for i in 0..=3 {
            let error = self
                .apply(&x, transpose)?
                .iter()
                .zip(rhs)
                .map(|(a, b)| a - b)
                .collect::<Vec<_>>();
            if norm(&error)? <= bound {
                return Ok(x);
            }
            if i == 3 {
                break;
            }
            let d = self.solve(&error.iter().map(|v| -v).collect::<Vec<_>>(), transpose)?;
            let next = x.iter().zip(d).map(|(a, b)| a + b).collect::<Vec<_>>();
            if next.iter().zip(&x).all(|(a, b)| a.to_bits() == b.to_bits()) {
                break;
            }
            x = next;
        }
        Err(CaeError::convergence(
            "SPD low-rank solve missed required true residual",
        ))
    }
}
