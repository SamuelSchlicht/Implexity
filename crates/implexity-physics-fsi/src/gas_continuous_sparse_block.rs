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

use crate::gas_quadratic_network::ConstantWeakTransportCell;
use crate::gas_quadratic_velocity::QuadraticVelocityBasis;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{dense::DenseMatrix, sparse::CscMatrix};
use implexity_solve::sparse_low_rank::SparseLowRankLimits;
use implexity_solve::sparse_spd_low_rank::SparseSpdLowRankFactor as SparseLowRankFactor;
use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousSparseBlockPolicy {
    pub maximum_population_count: usize,
    pub maximum_base_nonzeros: usize,
    pub maximum_moment_rank: usize,
    pub maximum_dense_entries: usize,
    pub numerical_rank_relative_tolerance: f64,
    pub inverse_relative_tolerance: f64,
}
pub(crate) struct ContinuousSparseBlock {
    factor: SparseLowRankFactor,
    base_nonzeros: usize,
    maximum_discarded_moment_relative_norm: f64,
    inverse_relative_tolerance: f64,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
fn norm(x: &[f64]) -> CaeResult<f64> {
    implexity_solve::certificate::stable_l2(x).map_err(fail)
}
impl ContinuousSparseBlockPolicy {
    pub fn validate(&self) -> CaeResult<()> {
        if self.maximum_population_count == 0
            || self.maximum_base_nonzeros == 0
            || self.maximum_moment_rank == 0
            || self.maximum_dense_entries == 0
            || !self.numerical_rank_relative_tolerance.is_finite()
            || self.numerical_rank_relative_tolerance <= 0.
            || self.numerical_rank_relative_tolerance >= 1.
            || !self.inverse_relative_tolerance.is_finite()
            || self.inverse_relative_tolerance <= 0.
            || self.inverse_relative_tolerance >= 1.
        {
            return Err(fail("continuous sparse block numerical policy invalid"));
        }
        Ok(())
    }
}
impl ContinuousSparseBlock {
    pub(crate) fn new<F>(
        basis: &QuadraticVelocityBasis,
        descriptor: &ConstantWeakTransportCell,
        policy: &ContinuousSparseBlockPolicy,
        mut jacobian: F,
    ) -> CaeResult<Self>
    where
        F: FnMut(&[Vec<f64>]) -> CaeResult<Vec<Vec<f64>>>,
    {
        policy.validate()?;
        let n = basis.coefficient_count();
        if n == 0
            || n > policy.maximum_population_count
            || !descriptor.storage_weight.is_finite()
            || descriptor.storage_weight < 1.
        {
            return Err(fail("continuous sparse block size or storage coefficient"));
        }
        let mut rows = vec![BTreeMap::<usize, f64>::new(); n];
        for (i, row) in basis.mass_rows()?.iter().enumerate() {
            for (j, v) in row {
                *rows[i].entry(*j).or_default() += descriptor.storage_weight * v;
            }
        }
        let mut moments = (0..4)
            .map(|j| {
                basis
                    .integrated_moment_rows()
                    .iter()
                    .map(|r| r[j])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for face in &descriptor.faces {
            if !face.weight.is_finite() || face.weight < 0. {
                return Err(fail("continuous sparse block face weight"));
            }
            let op = face.speed.rows();
            if op.len() != n {
                return Err(fail("continuous sparse block face shape"));
            }
            for (i, row) in op.iter().enumerate() {
                for (j, v) in row {
                    if *j >= n || !v.is_finite() {
                        return Err(fail("continuous sparse block face entry"));
                    }
                    *rows[i].entry(*j).or_default() += face.weight * v;
                }
            }
            for j in 0..4 {
                moments.push(
                    face.density.transpose_apply(
                        &basis
                            .reproduction_vectors()
                            .iter()
                            .map(|r| r[j])
                            .collect::<Vec<_>>(),
                    )?,
                );
            }
        }
        for row in &mut rows {
            row.retain(|_, v| *v != 0.);
            if row.values().any(|v| !v.is_finite()) {
                return Err(fail("continuous sparse block base arithmetic"));
            }
        }
        let nnz = rows.iter().map(BTreeMap::len).sum::<usize>();
        if nnz > policy.maximum_base_nonzeros {
            return Err(fail("continuous sparse block base cost"));
        }
        let mut q = Vec::<Vec<f64>>::new();
        let mut discarded = 0f64;
        for row in moments {
            if row.len() != n || row.iter().any(|v| !v.is_finite()) {
                return Err(fail("continuous sparse block moment row"));
            }
            let magnitude = norm(&row)?;
            if !magnitude.is_finite() || magnitude == 0. {
                return Err(fail("continuous sparse block unresolved moment row"));
            }
            let mut remainder = row.iter().map(|v| v / magnitude).collect::<Vec<_>>();
            for _ in 0..2 {
                for basis in &q {
                    let dot = remainder.iter().zip(basis).map(|(a, b)| a * b).sum::<f64>();
                    for (a, b) in remainder.iter_mut().zip(basis) {
                        *a -= dot * b;
                    }
                }
            }
            let relative = norm(&remainder)?;
            if !relative.is_finite() {
                return Err(fail("continuous sparse block moment rank arithmetic"));
            }
            if relative <= policy.numerical_rank_relative_tolerance {
                discarded = discarded.max(relative);
                continue;
            }
            if q.len() == policy.maximum_moment_rank {
                return Err(fail("continuous sparse block moment rank cost"));
            }
            for a in &mut remainder {
                *a /= relative;
            }
            q.push(remainder);
        }
        let rank = q.len();
        let retained = n
            .checked_mul(rank)
            .and_then(|v| v.checked_mul(4))
            .and_then(|v| {
                rank.checked_mul(rank)
                    .and_then(|k| k.checked_mul(2))
                    .and_then(|k| v.checked_add(k))
            })
            .ok_or_else(|| fail("continuous sparse block storage overflow"))?;
        if retained > policy.maximum_dense_entries {
            return Err(fail("continuous sparse block dense correction cost"));
        }
        let mut u = vec![0.; n * rank];
        let mut v = vec![0.; n * rank];
        for (batch_index, batch) in q.chunks(16).enumerate() {
            let values = jacobian(batch)?;
            if values.len() != batch.len()
                || values
                    .iter()
                    .any(|x| x.len() != n || x.iter().any(|v| !v.is_finite()))
            {
                return Err(fail("continuous sparse block current Jacobian shape"));
            }
            for (local, (direction, value)) in batch.iter().zip(values).enumerate() {
                let k = batch_index * 16 + local;
                for i in 0..n {
                    let base = rows[i].iter().map(|(j, s)| s * direction[*j]).sum::<f64>();
                    u[i * rank + k] = value[i] - base;
                    v[i * rank + k] = direction[i];
                }
            }
        }
        let mut ri = Vec::with_capacity(nnz);
        let mut ci = Vec::with_capacity(nnz);
        let mut data = Vec::with_capacity(nnz);
        for (i, row) in rows.iter().enumerate() {
            for (j, value) in row {
                ri.push(i);
                ci.push(*j);
                data.push(*value);
            }
        }
        let base =
            CscMatrix::from_triplets(n, n, &ri, &ci, &data).map_err(|e| fail(e.to_string()))?;
        let factor = SparseLowRankFactor::new(
            base,
            DenseMatrix::new(n, rank, u).map_err(|e| fail(e.to_string()))?,
            DenseMatrix::new(n, rank, v).map_err(|e| fail(e.to_string()))?,
            SparseLowRankLimits {
                maximum_size: policy.maximum_population_count,
                maximum_base_nonzeros: policy.maximum_base_nonzeros,
                maximum_rank: policy.maximum_moment_rank,
                maximum_dense_entries: policy.maximum_dense_entries,
            },
        )?;
        Ok(Self {
            factor,
            base_nonzeros: nnz,
            maximum_discarded_moment_relative_norm: discarded,
            inverse_relative_tolerance: policy.inverse_relative_tolerance,
        })
    }
    pub(crate) fn apply(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        self.factor.apply(x, transpose)
    }
    pub(crate) fn solve(&self, x: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        self.factor
            .solve_checked(x, transpose, 0., self.inverse_relative_tolerance)
    }
    pub(crate) fn rank(&self) -> usize {
        self.factor.rank()
    }
    pub(crate) fn base_nonzeros(&self) -> usize {
        self.base_nonzeros
    }
    pub(crate) fn numeric_and_matrix_bytes(&self) -> usize {
        self.factor.numeric_and_matrix_bytes()
    }
    pub(crate) fn discarded_moment_relative_norm(&self) -> f64 {
        self.maximum_discarded_moment_relative_norm
    }
}
