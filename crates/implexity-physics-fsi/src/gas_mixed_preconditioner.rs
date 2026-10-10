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

use crate::gas_mixed_network::{MixedGasLayout, MixedGasLocalPlan, MixedGasPolicy};
use crate::gas_multiscale_network::{MultiscaleNetworkInputs, MultiscaleNetworkTopology};
use crate::gas_transient_kinetic::velocity_equilibrium_density;
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{lu::SparseLu, sparse::CscMatrix};
use std::collections::BTreeSet;

pub struct MixedMomentCoarse {
    active: Vec<usize>,
    indices: Vec<Vec<usize>>,
    restriction: Vec<Vec<[f64; 4]>>,
    prolongation: Vec<Vec<[f64; 4]>>,
    factor: SparseLu,
    dimension: usize,
    nonzeros: usize,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
impl MixedMomentCoarse {
    pub fn new(
        inputs: &MultiscaleNetworkInputs<f64>,
        layout: &MixedGasLayout,
        policy: &MixedGasPolicy,
        previous: &[f64],
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
    ) -> CaeResult<Self> {
        let n = inputs.gas.volume_m3.len();
        let nv = layout.population_count();
        if inputs
            .gas
            .volume_m3
            .iter()
            .any(|v| !v.is_finite() || *v < 0.)
            || n != layout.continuum_cells().len()
            || inputs.gas.grid.velocity_m_s.len() != nv
            || physical.len() != layout.dimension()
            || previous.len() != layout.dimension()
            || columns.len() != scales.len()
            || columns.is_empty()
            || columns.iter().any(|i| *i >= layout.dimension())
            || scales.iter().any(|s| !s.is_finite() || *s <= 0.)
            || physical.iter().chain(previous).any(|q| !q.is_finite())
            || !inputs.gas.gas_rt_m2_s2.is_finite()
            || inputs.gas.gas_rt_m2_s2 <= 0.
        {
            return Err(fail("invalid mixed moment preconditioner coordinates"));
        }
        let mut solver_index = vec![None; layout.dimension()];
        for (j, &g) in columns.iter().enumerate() {
            if solver_index[g].replace(j).is_some() {
                return Err(fail("duplicate mixed solver coordinate"));
            }
        }
        let thermal = inputs.gas.gas_rt_m2_s2.sqrt();
        let mut owner = vec![None; layout.dimension()];
        let mut active = vec![];
        let mut indices = vec![vec![]; n];
        let mut restriction = vec![vec![]; n];
        let mut prolongation = vec![vec![]; n];
        let mut coarse_index = vec![None; n];
        for c in 0..n {
            if inputs.gas.volume_m3[c] <= 0. {
                continue;
            }
            let o = layout
                .cell_offset(c)
                .ok_or_else(|| fail("missing active mixed cell"))?;
            let count = if layout.continuum_cells()[c] { 4 } else { nv };
            if o.checked_add(count)
                .is_none_or(|end| end > layout.dimension())
            {
                return Err(fail("invalid mixed cell coordinate range"));
            }
            coarse_index[c] = Some(active.len());
            active.push(c);
            for k in 0..count {
                if owner[o + k].replace((c, k)).is_some() {
                    return Err(fail("overlapping mixed cell coordinate ranges"));
                }
                indices[c].push(
                    solver_index[o + k]
                        .ok_or_else(|| fail("missing active mixed solver coordinate"))?,
                );
            }
            if layout.continuum_cells()[c] {
                restriction[c] = (0..4)
                    .map(|k| std::array::from_fn(|j| f64::from(k == j)))
                    .collect();
                prolongation[c] = restriction[c].clone();
                let mass = scales[indices[c][0]];
                if (1..4).any(|j| {
                    (scales[indices[c][j]] / (mass * thermal) - 1.).abs() > 128. * f64::EPSILON
                }) {
                    return Err(fail(
                        "mixed continuum moment scales differ from the declared thermal speed",
                    ));
                }
            } else {
                let mass = indices[c].iter().map(|j| scales[*j]).sum::<f64>();
                if !mass.is_finite() || mass <= 0. {
                    return Err(fail("invalid mixed kinetic mass scale"));
                }
                let weights = indices[c]
                    .iter()
                    .map(|j| scales[*j] / mass)
                    .collect::<Vec<_>>();
                restriction[c] = inputs
                    .gas
                    .grid
                    .velocity_m_s
                    .iter()
                    .zip(&weights)
                    .map(|(v, w)| {
                        [
                            *w,
                            *w * v[0] / thermal,
                            *w * v[1] / thermal,
                            *w * v[2] / thermal,
                        ]
                    })
                    .collect();
                let mut moments = [0.; 4];
                for k in 0..nv {
                    if physical[o + k] < 0. {
                        return Err(fail("negative mixed kinetic inventory"));
                    }
                    let x = physical[o + k] / scales[indices[c][k]];
                    for j in 0..4 {
                        moments[j] += restriction[c][k][j] * x;
                    }
                }
                if moments[0] == 0. {
                    moments = [1., 0., 0., 0.];
                }
                let m: [Dual<4>; 4] = std::array::from_fn(|j| {
                    let mut e = [0.; 4];
                    e[j] = 1.;
                    Dual::new(moments[j], e)
                });
                let g = velocity_equilibrium_density(
                    &inputs.gas.grid,
                    m[0],
                    std::array::from_fn(|j| m[j + 1] * thermal / m[0]),
                    Dual::constant(inputs.gas.gas_rt_m2_s2),
                    &policy.network.kinetic,
                )?;
                prolongation[c] = g
                    .iter()
                    .zip(&weights)
                    .map(|(g, w)| g.eps.map(|e| e / *w))
                    .collect();
            }
            for i in 0..4 {
                for j in 0..4 {
                    let value = restriction[c]
                        .iter()
                        .zip(&prolongation[c])
                        .map(|(r, e)| r[i] * e[j])
                        .sum::<f64>();
                    if !value.is_finite() || (value - f64::from(i == j)).abs() > 1e-9 {
                        return Err(fail(
                            "mixed moment restriction and prolongation are not reciprocal",
                        ));
                    }
                }
            }
        }
        if active.is_empty() || columns.iter().any(|g| owner[*g].is_none()) {
            return Err(fail("mixed moment preconditioner has unowned coordinates"));
        }
        let topology = MultiscaleNetworkTopology::new_with_policy(inputs, &policy.network)?;
        let mut rows = vec![];
        let mut cols = vec![];
        let mut values = vec![];
        for (ri, &c) in active.iter().enumerate() {
            let plan = MixedGasLocalPlan::new_prepared(inputs, layout, policy, &topology, &[c])?;
            let support = plan
                .state_columns
                .iter()
                .filter_map(|g| owner[*g].map(|p| p.0))
                .collect::<BTreeSet<_>>();
            let old = plan
                .state_columns
                .iter()
                .map(|g| Dual::<4>::constant(previous[*g]))
                .collect::<Vec<_>>();
            for d in support {
                let ci = coarse_index[d].ok_or_else(|| fail("missing mixed coarse cell"))?;
                let current = plan
                    .state_columns
                    .iter()
                    .map(|g| {
                        let mut e = [0.; 4];
                        if let Some((cell, k)) = owner[*g] {
                            if cell == d {
                                let index = solver_index[*g].unwrap();
                                e = prolongation[d][k].map(|p| p * scales[index]);
                            }
                        }
                        Dual::new(physical[*g], e)
                    })
                    .collect::<Vec<_>>();
                let result = plan.evaluate(&current, &old)?;
                let mut block = [[0.; 4]; 4];
                for &(global, local) in &plan.output_columns {
                    let Some((cell, k)) = owner[global] else {
                        return Err(fail("unowned mixed output coordinate"));
                    };
                    if cell != c {
                        return Err(fail("mixed local plan contains a foreign output cell"));
                    }
                    let scale = scales[solver_index[global].unwrap()];
                    for i in 0..4 {
                        for j in 0..4 {
                            block[i][j] += restriction[c][k][i]
                                * result.inventory_residual[local].eps[j]
                                / scale;
                        }
                    }
                }
                for i in 0..4 {
                    for j in 0..4 {
                        let value = block[i][j];
                        if !value.is_finite() {
                            return Err(fail("nonfinite mixed coarse Jacobian"));
                        }
                        if value != 0. {
                            rows.push(ri * 4 + i);
                            cols.push(ci * 4 + j);
                            values.push(value);
                        }
                    }
                }
            }
        }
        let dimension = active
            .len()
            .checked_mul(4)
            .ok_or_else(|| fail("mixed coarse dimension overflow"))?;
        let matrix = CscMatrix::from_triplets(dimension, dimension, &rows, &cols, &values)
            .map_err(|e| fail(e.to_string()))?;
        let factor = SparseLu::new(&matrix).map_err(|e| fail(e.to_string()))?;
        Ok(Self {
            active,
            indices,
            restriction,
            prolongation,
            factor,
            dimension: columns.len(),
            nonzeros: values.len(),
        })
    }
    pub fn coarse_dimension(&self) -> usize {
        self.active.len() * 4
    }
    pub fn nonzeros(&self) -> usize {
        self.nonzeros
    }
    pub fn restrict(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        self.map(v, transpose, true)
    }
    pub fn lift(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        self.map(v, transpose, false)
    }
    fn map(&self, v: &[f64], transpose: bool, restriction: bool) -> CaeResult<Vec<f64>> {
        let large_to_small = restriction != transpose;
        let expected = if large_to_small {
            self.dimension
        } else {
            self.coarse_dimension()
        };
        if v.len() != expected || v.iter().any(|x| !x.is_finite()) {
            return Err(fail("mixed coarse vector shape or value"));
        }
        let mut out = vec![
            0.;
            if large_to_small {
                self.coarse_dimension()
            } else {
                self.dimension
            }
        ];
        let coefficients = if restriction {
            &self.restriction
        } else {
            &self.prolongation
        };
        for (i, &cell) in self.active.iter().enumerate() {
            for (k, &index) in self.indices[cell].iter().enumerate() {
                for j in 0..4 {
                    if large_to_small {
                        out[4 * i + j] += coefficients[cell][k][j] * v[index];
                    } else {
                        out[index] += coefficients[cell][k][j] * v[4 * i + j];
                    }
                }
            }
        }
        if out.iter().any(|x| !x.is_finite()) {
            return Err(fail("mixed coarse vector overflow"));
        }
        Ok(out)
    }
    pub fn solve(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if v.len() != self.coarse_dimension() || v.iter().any(|v| !v.is_finite()) {
            return Err(fail("mixed coarse solve shape or value"));
        }
        let out = if transpose {
            self.factor.solve_transpose(v)
        } else {
            self.factor.solve(v)
        }
        .map_err(|e| fail(e.to_string()))?;
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("mixed coarse solution overflow"));
        }
        Ok(out)
    }
}

pub struct MixedDiagonalBlocks {
    dimension: usize,
    blocks: Vec<(Vec<usize>, implexity_linalg::dense::DenseLu)>,
}
impl MixedDiagonalBlocks {
    pub fn new(
        inputs: &MultiscaleNetworkInputs<f64>,
        layout: &MixedGasLayout,
        policy: &MixedGasPolicy,
        previous: &[f64],
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
        maximum_population_count: usize,
    ) -> CaeResult<Self> {
        if inputs.gas.volume_m3.len() != layout.continuum_cells().len()
            || inputs.gas.grid.velocity_m_s.len() != layout.population_count()
            || inputs
                .gas
                .volume_m3
                .iter()
                .any(|v| !v.is_finite() || *v < 0.)
            || layout.population_count() > maximum_population_count
            || maximum_population_count == 0
            || physical.len() != layout.dimension()
            || previous.len() != layout.dimension()
            || columns.len() != scales.len()
            || columns.is_empty()
            || scales.iter().any(|s| !s.is_finite() || *s <= 0.)
            || physical.iter().chain(previous).any(|x| !x.is_finite())
        {
            return Err(fail("invalid mixed diagonal block domain"));
        }
        let mut index = vec![None; layout.dimension()];
        for (i, &g) in columns.iter().enumerate() {
            if g >= index.len() || index[g].replace(i).is_some() {
                return Err(fail("invalid mixed diagonal coordinate ownership"));
            }
        }
        let topology = MultiscaleNetworkTopology::new_with_policy(inputs, &policy.network)?;
        let mut blocks = vec![];
        let mut owned = vec![false; columns.len()];
        for c in 0..inputs.gas.volume_m3.len() {
            if inputs.gas.volume_m3[c] == 0. {
                continue;
            }
            let Some(offset) = layout.cell_offset(c) else {
                continue;
            };
            let count = if layout.continuum_cells()[c] {
                4
            } else {
                layout.population_count()
            };
            let indices = (0..count)
                .map(|k| {
                    index
                        .get(offset + k)
                        .copied()
                        .flatten()
                        .ok_or_else(|| fail("missing mixed diagonal coordinate"))
                })
                .collect::<CaeResult<Vec<_>>>()?;
            for &i in &indices {
                if owned[i] {
                    return Err(fail("overlapping mixed diagonal cells"));
                }
                owned[i] = true;
            }
            let plan = MixedGasLocalPlan::new_prepared(inputs, layout, policy, &topology, &[c])?;
            let old = plan
                .state_columns
                .iter()
                .map(|g| Dual::<16>::constant(previous[*g]))
                .collect::<Vec<_>>();
            let entries = count
                .checked_mul(count)
                .ok_or_else(|| fail("mixed diagonal block size overflow"))?;
            let mut matrix = vec![0.; entries];
            for base in (0..count).step_by(16) {
                let state = plan
                    .state_columns
                    .iter()
                    .map(|g| {
                        let mut eps = [0.; 16];
                        if *g >= offset && *g < offset + count {
                            let k = *g - offset;
                            if k >= base && k < base + 16 {
                                eps[k - base] = scales[indices[k]];
                            }
                        }
                        Dual::new(physical[*g], eps)
                    })
                    .collect::<Vec<_>>();
                let result = plan.evaluate(&state, &old)?;
                for &(g, local) in &plan.output_columns {
                    if g < offset || g >= offset + count {
                        return Err(fail("foreign mixed diagonal output"));
                    }
                    let row = g - offset;
                    for j in base..(base + 16).min(count) {
                        matrix[row * count + j] =
                            result.inventory_residual[local].eps[j - base] / scales[indices[row]];
                    }
                }
            }
            if matrix.iter().any(|v| !v.is_finite()) {
                return Err(fail("nonfinite mixed diagonal Jacobian"));
            }
            let matrix = implexity_linalg::dense::DenseMatrix::new(count, count, matrix)
                .map_err(|e| fail(e.to_string()))?;
            let factor =
                implexity_linalg::dense::DenseLu::new(&matrix).map_err(|e| fail(e.to_string()))?;
            blocks.push((indices, factor));
        }
        if owned.iter().any(|v| !v) {
            return Err(fail("unowned mixed diagonal coordinate"));
        }
        Ok(Self {
            dimension: columns.len(),
            blocks,
        })
    }
    pub fn apply(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if v.len() != self.dimension || v.iter().any(|x| !x.is_finite()) {
            return Err(fail("mixed diagonal vector shape or value"));
        }
        let mut out = vec![0.; self.dimension];
        for (indices, factor) in &self.blocks {
            let rhs = indices.iter().map(|i| v[*i]).collect::<Vec<_>>();
            let value = if transpose {
                factor.solve(&rhs, 1, true)
            } else {
                factor.solve(&rhs, 1, false)
            }
            .map_err(|e| fail(e.to_string()))?;
            for (&i, &x) in indices.iter().zip(&value) {
                out[i] = x;
            }
        }
        if out.iter().any(|x| !x.is_finite()) {
            return Err(fail("mixed diagonal solve overflow"));
        }
        Ok(out)
    }
}

pub struct MixedKineticSubdomain {
    diagonal: MixedDiagonalBlocks,
    kinetic_indices: Vec<usize>,
    factor: SparseLu,
}
impl MixedKineticSubdomain {
    pub fn new(
        inputs: &MultiscaleNetworkInputs<f64>,
        layout: &MixedGasLayout,
        policy: &MixedGasPolicy,
        previous: &[f64],
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
        maximum_population_count: usize,
        maximum_kinetic_unknowns: usize,
    ) -> CaeResult<Self> {
        if inputs.gas.volume_m3.len() != layout.continuum_cells().len()
            || inputs
                .gas
                .volume_m3
                .iter()
                .any(|v| !v.is_finite() || *v < 0.)
            || columns.len() != scales.len()
            || columns.is_empty()
            || columns.iter().any(|g| *g >= layout.dimension())
            || maximum_population_count == 0
            || layout.population_count() > maximum_population_count
            || maximum_kinetic_unknowns == 0
        {
            return Err(fail("invalid kinetic subdomain coordinate or cost domain"));
        }
        let mut solver = vec![None; layout.dimension()];
        for (i, &g) in columns.iter().enumerate() {
            if solver[g].replace(i).is_some() {
                return Err(fail("duplicate kinetic subdomain coordinate"));
            }
        }
        let mut ki = vec![None; layout.dimension()];
        let mut kinetic_indices = vec![];
        for c in 0..layout.continuum_cells().len() {
            if inputs.gas.volume_m3[c] > 0. && !layout.continuum_cells()[c] {
                if let Some(o) = layout.cell_offset(c) {
                    for k in 0..layout.population_count() {
                        ki[o + k] = Some(kinetic_indices.len());
                        kinetic_indices.push(
                            solver[o + k]
                                .ok_or_else(|| fail("missing kinetic subdomain coordinate"))?,
                        );
                    }
                }
            }
        }
        if kinetic_indices.is_empty() || kinetic_indices.len() > maximum_kinetic_unknowns {
            return Err(fail("kinetic subdomain exceeds its declared size domain"));
        }
        let diagonal = MixedDiagonalBlocks::new(
            inputs,
            layout,
            policy,
            previous,
            physical,
            columns,
            scales,
            maximum_population_count,
        )?;
        let topology = MultiscaleNetworkTopology::new_with_policy(inputs, &policy.network)?;
        let mut rows = vec![];
        let mut cols = vec![];
        let mut values = vec![];
        for c in 0..layout.continuum_cells().len() {
            if inputs.gas.volume_m3[c] == 0.
                || layout.continuum_cells()[c]
                || layout.cell_offset(c).is_none()
            {
                continue;
            }
            let plan = MixedGasLocalPlan::new_prepared(inputs, layout, policy, &topology, &[c])?;
            let selected = plan
                .state_columns
                .iter()
                .copied()
                .filter(|g| ki[*g].is_some())
                .collect::<Vec<_>>();
            let old = plan
                .state_columns
                .iter()
                .map(|g| Dual::<16>::constant(previous[*g]))
                .collect::<Vec<_>>();
            for batch in selected.chunks(16) {
                let state = plan
                    .state_columns
                    .iter()
                    .map(|g| {
                        let mut eps = [0.; 16];
                        if let Some(j) = batch.iter().position(|b| b == g) {
                            eps[j] = scales[solver[*g].unwrap()];
                        }
                        Dual::new(physical[*g], eps)
                    })
                    .collect::<Vec<_>>();
                let result = plan.evaluate(&state, &old)?;
                for &(global, local) in &plan.output_columns {
                    let row = ki[global].ok_or_else(|| fail("foreign kinetic subdomain output"))?;
                    for (j, &g) in batch.iter().enumerate() {
                        let value = result.inventory_residual[local].eps[j]
                            / scales[solver[global].unwrap()];
                        if !value.is_finite() {
                            return Err(fail("nonfinite kinetic subdomain Jacobian"));
                        }
                        if value != 0. {
                            rows.push(row);
                            cols.push(ki[g].unwrap());
                            values.push(value);
                        }
                    }
                }
            }
        }
        let n = kinetic_indices.len();
        let matrix = CscMatrix::from_triplets(n, n, &rows, &cols, &values)
            .map_err(|e| fail(e.to_string()))?;
        let factor = SparseLu::new(&matrix).map_err(|e| fail(e.to_string()))?;
        Ok(Self {
            diagonal,
            kinetic_indices,
            factor,
        })
    }
    pub fn apply(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let mut result = self.diagonal.apply(v, transpose)?;
        let rhs = self
            .kinetic_indices
            .iter()
            .map(|i| v[*i])
            .collect::<Vec<_>>();
        let out = (if transpose {
            self.factor.solve_transpose(&rhs)
        } else {
            self.factor.solve(&rhs)
        })
        .map_err(|e| fail(e.to_string()))?;
        for (&i, &x) in self.kinetic_indices.iter().zip(&out) {
            result[i] = x;
        }
        if result.iter().any(|x| !x.is_finite()) {
            return Err(fail("kinetic subdomain solve overflow"));
        }
        Ok(result)
    }
}
