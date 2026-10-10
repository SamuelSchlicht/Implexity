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

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{dense::DenseMatrix, sparse::CscMatrix};
use crate::{factorization::Factorization, matrix::Jacobian, certificate::stable_l2};

#[derive(Clone, Copy, Debug)]
pub struct SparseLowRankLimits {
    pub maximum_size: usize,
    pub maximum_base_nonzeros: usize,
    pub maximum_rank: usize,
    pub maximum_dense_entries: usize,
}

pub struct SparseLowRankFactor {
    base: Factorization,
    u: DenseMatrix,
    v: DenseMatrix,
    inverse_u: DenseMatrix,
    transpose_inverse_v: DenseMatrix,
    core: Option<Factorization>,
}

fn invalid(message: &str) -> CaeError { CaeError::contract(message) }
fn finite(values: &[f64]) -> CaeResult<()> {
    if values.iter().any(|v| !v.is_finite()) { return Err(CaeError::convergence("sparse low-rank action is nonfinite")); }
    Ok(())
}

impl SparseLowRankFactor {
    pub fn new(base: CscMatrix, u: DenseMatrix, v: DenseMatrix, limits: SparseLowRankLimits) -> CaeResult<Self> {
        let n = base.nrows();
        let r = u.ncols;
        let entries = n.checked_mul(r).ok_or_else(||invalid("sparse low-rank size overflow"))?;
        let core_entries = r.checked_mul(r).ok_or_else(||invalid("sparse low-rank rank overflow"))?;
        let retained = entries.checked_mul(4).and_then(|k|core_entries.checked_mul(2).and_then(|c|k.checked_add(c))).ok_or_else(||invalid("sparse low-rank storage overflow"))?;
        if n == 0 || base.ncols() != n || u.nrows != n || v.nrows != n || v.ncols != r
            || u.data.len() != entries || v.data.len() != entries
            || limits.maximum_size == 0 || limits.maximum_base_nonzeros == 0
            || limits.maximum_rank == 0 || limits.maximum_dense_entries == 0
            || n > limits.maximum_size || base.nnz() > limits.maximum_base_nonzeros
            || r > limits.maximum_rank || retained > limits.maximum_dense_entries {
            return Err(invalid("sparse low-rank shape or cost bounds"));
        }
        finite(&u.data)?; finite(&v.data)?;
        let base = Factorization::for_correction(Jacobian::Csc(base), n, None)?;
        let (inverse_u, transpose_inverse_v, core) = if r == 0 {
            (DenseMatrix::zeros(n,0),DenseMatrix::zeros(n,0),None)
        } else {
            let w = base.solve_block(&u.data, r, false)?.solution;
            let z = base.solve_block(&v.data, r, true)?.solution;
            let mut k = DenseMatrix::identity(r);
            for i in 0..r { for j in 0..r { for row in 0..n { k.data[i*r+j] += v.data[row*r+i]*w[row*r+j]; } } }
            finite(&k.data)?;
            let core = Factorization::for_correction(Jacobian::Dense(k),r,None)?;
            (DenseMatrix::new(n,r,w).map_err(|e|invalid(&e.to_string()))?,DenseMatrix::new(n,r,z).map_err(|e|invalid(&e.to_string()))?,Some(core))
        };
        Ok(Self {base,u,v,inverse_u,transpose_inverse_v,core})
    }
    pub fn size(&self) -> usize { self.base.size() }
    pub fn rank(&self) -> usize { self.u.ncols }
    pub fn retained_bytes(&self) -> usize {
        self.base.retained_bytes() + self.core.as_ref().map_or(0,Factorization::retained_bytes)
            + (self.u.data.len()+self.v.data.len()+self.inverse_u.data.len()+self.transpose_inverse_v.data.len())*8
    }
    pub fn apply(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if x.len() != self.size() { return Err(invalid("sparse low-rank operand shape")); }
        finite(x)?;
        let mut y = self.base.matrix().matrix().apply(x,transpose)?;
        let (left,right) = if transpose {(&self.v,&self.u)} else {(&self.u,&self.v)};
        let r = self.rank();
        let mut projected = vec![0.;r];
        for row in 0..self.size() { for col in 0..r { projected[col] += right.data[row*r+col]*x[row]; } }
        for row in 0..self.size() { for col in 0..r { y[row] += left.data[row*r+col]*projected[col]; } }
        finite(&y)?; Ok(y)
    }
    pub fn solve(&self, rhs: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if rhs.len() != self.size() { return Err(invalid("sparse low-rank right-hand side shape")); }
        finite(rhs)?;
        let mut y = self.base.solve(rhs,transpose)?.0;
        if let Some(core) = &self.core {
            let (projector,correction) = if transpose {(&self.u,&self.transpose_inverse_v)} else {(&self.v,&self.inverse_u)};
            let r = self.rank();
            let mut b = vec![0.;r];
            for row in 0..self.size() { for col in 0..r { b[col] += projector.data[row*r+col]*y[row]; } }
            let c = core.solve(&b,transpose)?.0;
            for row in 0..self.size() { for col in 0..r { y[row] -= correction.data[row*r+col]*c[col]; } }
        }
        finite(&y)?; Ok(y)
    }
    pub fn solve_checked(&self, rhs: &[f64], transpose: bool, absolute_tolerance: f64, relative_tolerance: f64) -> CaeResult<Vec<f64>> {
        if !absolute_tolerance.is_finite() || absolute_tolerance < 0. || !relative_tolerance.is_finite() || relative_tolerance < 0. {
            return Err(invalid("sparse low-rank residual tolerances"));
        }
        let norm = |v: &[f64]| stable_l2(v).map_err(|e|CaeError::convergence(e.to_string()));
        let bound = absolute_tolerance + relative_tolerance*norm(rhs)?;
        if !bound.is_finite() { return Err(invalid("sparse low-rank residual bound overflow")); }
        let mut solution = self.solve(rhs,transpose)?;
        for refinement in 0..=3 {
            let error: Vec<_> = self.apply(&solution,transpose)?.iter().zip(rhs).map(|(a,b)|a-b).collect();
            if norm(&error)? <= bound { return Ok(solution); }
            if refinement == 3 { break; }
            let correction = self.solve(&error.iter().map(|v|-v).collect::<Vec<_>>(),transpose)?;
            let next: Vec<_> = solution.iter().zip(correction).map(|(a,b)|a+b).collect();
            if next.iter().zip(&solution).all(|(a,b)|a.to_bits()==b.to_bits()) { break; }
            solution = next;
        }
        Err(CaeError::convergence("sparse low-rank solve missed its required residual"))
    }
}
