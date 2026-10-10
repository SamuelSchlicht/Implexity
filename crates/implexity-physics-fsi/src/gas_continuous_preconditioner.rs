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

use crate::gas_continuous_layout::ContinuousGasRepresentation;
use crate::gas_continuous_mixed::ContinuousMixedNetwork;
use crate::gas_continuous_mixed_solver::ContinuousMixedState;
use crate::gas_continuous_sparse_block::{ContinuousSparseBlock, ContinuousSparseBlockPolicy};
use crate::gas_quadratic_gaussian::QuadraticGaussianBasis;
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{
    dense::{DenseLu, DenseMatrix},
    lu::SparseLu,
    sparse::CscMatrix,
};
use rayon::prelude::*;
use std::ops::Range;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousTransportPreconditionerPolicy {
    pub maximum_population_count: usize,
    pub maximum_local_matrix_entries: usize,
    pub maximum_coarse_dimension: usize,
    pub maximum_coarse_nonzeros: usize,
    pub vacuum_reference_density_kg_m3: f64,
    pub freeze_within_solve: bool,
}
impl ContinuousTransportPreconditionerPolicy {
    pub fn validate(&self) -> CaeResult<()> {
        if self.maximum_population_count == 0
            || self.maximum_local_matrix_entries == 0
            || self.maximum_coarse_dimension == 0
            || self.maximum_coarse_nonzeros == 0
            || !self.vacuum_reference_density_kg_m3.is_finite()
            || self.vacuum_reference_density_kg_m3 <= 0.
        {
            return Err(CaeError::contract(
                "continuous transport numerical cost policy invalid",
            ));
        }
        Ok(())
    }
}
enum CellInverse {
    Dense(DenseLu),
    Sparse {
        factor: ContinuousSparseBlock,
        scales: Vec<f64>,
    },
}
struct CellPacket {
    coarse: Vec<CaeResult<(usize, usize, f64)>>,
    inverse: Option<CaeResult<(CellInverse, usize)>>,
}
struct CellBlock {
    active: Range<usize>,
    restrict: Vec<[f64; 4]>,
    lift: Vec<[f64; 4]>,
    inverse: CellInverse,
}
pub struct ContinuousTransportPreconditioner {
    blocks: Vec<CellBlock>,
    factor: SparseLu,
    scales: Vec<f64>,
    coarse_nonzeros: usize,
    local_matrix_entries: usize,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
impl ContinuousTransportPreconditioner {
    pub fn new(
        network: &ContinuousMixedNetwork<f64>,
        authored: &ContinuousMixedState,
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
        policy: &ContinuousTransportPreconditionerPolicy,
    ) -> CaeResult<Self> {
        Self::build(
            network, authored, physical, columns, scales, policy, None, false,
        )
    }
    pub fn new_with_sparse_blocks(
        network: &ContinuousMixedNetwork<f64>,
        authored: &ContinuousMixedState,
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
        policy: &ContinuousTransportPreconditionerPolicy,
        sparse_policy: &ContinuousSparseBlockPolicy,
    ) -> CaeResult<Self> {
        sparse_policy.validate()?;
        Self::build(
            network,
            authored,
            physical,
            columns,
            scales,
            policy,
            Some(sparse_policy),
            false,
        )
    }
    pub fn new_with_constant_transport_surrogate(
        network: &ContinuousMixedNetwork<f64>,
        authored: &ContinuousMixedState,
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
        policy: &ContinuousTransportPreconditionerPolicy,
        sparse_policy: &ContinuousSparseBlockPolicy,
    ) -> CaeResult<Self> {
        sparse_policy.validate()?;
        Self::build(
            network,
            authored,
            physical,
            columns,
            scales,
            policy,
            Some(sparse_policy),
            true,
        )
    }
    fn build(
        network: &ContinuousMixedNetwork<f64>,
        authored: &ContinuousMixedState,
        physical: &[f64],
        columns: &[usize],
        scales: &[f64],
        policy: &ContinuousTransportPreconditionerPolicy,
        sparse_policy: Option<&ContinuousSparseBlockPolicy>,
        numerical_surrogate: bool,
    ) -> CaeResult<Self> {
        policy.validate()?;
        let layout = network.layout();
        let geometry = network.prepared().geometry();
        layout.validate(physical, &geometry.volume_m3)?;
        layout.validate(&authored.previous_inventory, &geometry.previous_volume_m3)?;
        if !authored.molecular_viscosity_pa_s.is_finite()
            || authored.molecular_viscosity_pa_s <= 0.
            || authored.port_coefficient_density.iter().any(|q| {
                q.len() != layout.basis().coefficient_count()
                    || q.iter().any(|v| !v.is_finite() || *v < 0.)
            })
        {
            return Err(fail("continuous transport authored gas domain invalid"));
        }
        if columns.len() != scales.len()
            || columns.is_empty()
            || scales.iter().any(|x| !x.is_finite() || *x <= 0.)
            || !policy.vacuum_reference_density_kg_m3.is_finite()
            || policy.vacuum_reference_density_kg_m3 <= 0.
            || policy.maximum_population_count == 0
            || policy.maximum_local_matrix_entries == 0
            || policy.maximum_coarse_dimension == 0
            || policy.maximum_coarse_nonzeros == 0
        {
            return Err(fail(
                "continuous transport preconditioner coordinates or cost policy invalid",
            ));
        }
        let mut mapping = vec![None; layout.state_size()];
        for (i, c) in columns.iter().enumerate() {
            if *c >= mapping.len() || mapping[*c].replace(i).is_some() {
                return Err(fail("continuous transport preconditioner columns invalid"));
            }
        }
        if !authored.gas_rt_m2_s2.is_finite() || authored.gas_rt_m2_s2 <= 0. {
            return Err(fail("continuous transport thermal scale invalid"));
        }
        let thermal = authored.gas_rt_m2_s2.sqrt();
        let gaussian = QuadraticGaussianBasis::new(
            layout.basis().clone(),
            layout.basis().reference_speed_m_s(),
        )?;
        let reproduction = layout.basis().reproduction_vectors();
        let moments = layout.basis().integrated_moment_rows();
        let mut metadata = Vec::new();
        let mut total_entries = 0usize;
        for (c, volume) in geometry.volume_m3.iter().enumerate() {
            if *volume == 0. {
                continue;
            }
            let full = layout.range(c)?;
            let width = full.len();
            if layout.representation(c)? == ContinuousGasRepresentation::Kinetic
                && width > policy.maximum_population_count
            {
                return Err(fail("continuous transport population cost bound exceeded"));
            }
            total_entries = total_entries
                .checked_add(
                    (if sparse_policy.is_some()
                        && layout.representation(c)? == ContinuousGasRepresentation::Kinetic
                    {
                        0
                    } else {
                        width
                    })
                    .checked_mul(width)
                    .ok_or_else(|| fail("continuous transport local matrix size overflow"))?,
                )
                .ok_or_else(|| fail("continuous transport local matrix size overflow"))?;
            if total_entries > policy.maximum_local_matrix_entries {
                return Err(fail(
                    "continuous transport local matrix entry bound exceeded",
                ));
            }
            let start = mapping[full.start]
                .ok_or_else(|| fail("continuous transport active cell coordinate missing"))?;
            if full
                .clone()
                .enumerate()
                .any(|(k, g)| mapping[g] != Some(start + k))
            {
                return Err(fail("continuous transport cell coordinates not contiguous"));
            }
            let active = start..start + width;
            let macro_state = layout.macro_inventory(physical, c)?;
            let mass = if macro_state[0] > 0. {
                macro_state[0]
            } else {
                policy.vacuum_reference_density_kg_m3 * volume
            };
            let units = [mass, mass * thermal, mass * thermal, mass * thermal];
            if units.iter().any(|v| !v.is_finite() || *v <= 0.) {
                return Err(fail("continuous transport moment units invalid"));
            }
            let (restriction, lift) = if layout.representation(c)?
                == ContinuousGasRepresentation::Continuum
            {
                (
                    (0..4)
                        .map(|k| {
                            std::array::from_fn(|j| {
                                if k == j {
                                    scales[start + k] / units[j]
                                } else {
                                    0.
                                }
                            })
                        })
                        .collect::<Vec<_>>(),
                    (0..4)
                        .map(|k| {
                            std::array::from_fn(|j| {
                                if k == j {
                                    units[j] / scales[start + k]
                                } else {
                                    0.
                                }
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                let m: [Dual<4>; 4] = std::array::from_fn(|j| {
                    let mut e = [0.; 4];
                    e[j] = units[j];
                    Dual::new(
                        if macro_state[0] > 0. {
                            macro_state[j]
                        } else if j == 0 {
                            mass
                        } else {
                            0.
                        },
                        e,
                    )
                });
                let eq = gaussian.equilibrium(
                    m[0] / *volume,
                    std::array::from_fn(|j| m[j + 1] / m[0]),
                    Dual::constant(authored.gas_rt_m2_s2),
                    &network.prepared().policy().entropy,
                )?;
                let restriction = (0..width)
                    .map(|k| {
                        std::array::from_fn(|j| reproduction[k][j] * scales[start + k] / units[j])
                    })
                    .collect::<Vec<_>>();
                let lift = eq
                    .coefficient_density
                    .iter()
                    .enumerate()
                    .map(|(k, q)| q.eps.map(|v| v * volume / scales[start + k]))
                    .collect::<Vec<_>>();
                for i in 0..4 {
                    for j in 0..4 {
                        let re = (0..width)
                            .map(|k| moments[k][i] * scales[start + k] / units[i] * lift[k][j])
                            .sum::<f64>();
                        if !re.is_finite() || (re - f64::from(i == j)).abs() > 1e-9 {
                            return Err(fail("continuous transport moment maps not reciprocal"));
                        }
                    }
                }
                (restriction, lift)
            };
            metadata.push((c, full, active, restriction, lift));
        }
        if metadata
            .iter()
            .map(|(_, _, r, _, _)| r.len())
            .sum::<usize>()
            != columns.len()
        {
            return Err(fail(
                "continuous transport columns include inactive coordinates",
            ));
        }
        if metadata
            .len()
            .checked_mul(4)
            .is_none_or(|n| n > policy.maximum_coarse_dimension)
        {
            return Err(fail("continuous transport coarse dimension bound exceeded"));
        }
        let mut input_to_outputs = vec![Vec::new(); layout.slots()];
        for (o, (c, ..)) in metadata.iter().enumerate() {
            for d in network.dependency_cells(&[*c])? {
                input_to_outputs[d].push(o);
            }
        }
        let tangent = network.constant_lift::<Dual<16>>()?;
        let current = physical
            .iter()
            .map(|v| Dual::<16>::constant(*v))
            .collect::<Vec<_>>();
        let old = authored
            .previous_inventory
            .iter()
            .map(|v| Dual::<16>::constant(*v))
            .collect::<Vec<_>>();
        let ports = authored
            .port_coefficient_density
            .iter()
            .map(|a| a.iter().map(|v| Dual::<16>::constant(*v)).collect())
            .collect::<Vec<_>>();
        let mut coarse_rows = Vec::new();
        let mut coarse_cols = Vec::new();
        let mut coarse_values = Vec::new();
        let mut blocks = Vec::new();
        let mut sparse_descriptors = std::collections::BTreeMap::new();
        if sparse_policy.is_some() {
            let kinetic = metadata
                .iter()
                .filter_map(|(cell, _, _, _, _)| {
                    (layout.representation(*cell).ok()
                        == Some(ContinuousGasRepresentation::Kinetic))
                    .then_some(*cell)
                })
                .collect::<Vec<_>>();
            if !kinetic.is_empty() {
                let descriptors = if numerical_surrogate {
                    network.constant_surrogate_weak_transport_cells(
                        physical,
                        &authored.previous_inventory,
                        &authored.port_coefficient_density,
                        authored.gas_rt_m2_s2,
                        authored.molecular_viscosity_pa_s,
                        &kinetic,
                    )?
                } else {
                    network.constant_weak_transport_cells(
                        physical,
                        &authored.previous_inventory,
                        &authored.port_coefficient_density,
                        authored.gas_rt_m2_s2,
                        authored.molecular_viscosity_pa_s,
                        &kinetic,
                    )?
                };
                for (cell, descriptor) in descriptors {
                    sparse_descriptors.insert(cell, descriptor);
                }
            }
        }
        let cached = tangent.moment_cache(&current, &old)?;
        let evaluate_cell = |current: &mut Vec<Dual<16>>, ci: usize| -> CaeResult<CellPacket> {
            let (cell, full, active, _restriction, lift) = &metadata[ci];
            let result = (|| -> CaeResult<CellPacket> {
                let selected = input_to_outputs[*cell]
                    .iter()
                    .map(|o| metadata[*o].0)
                    .collect::<Vec<_>>();
                for k in 0..full.len() {
                    let mut e = [0.; 16];
                    e[..4].copy_from_slice(&lift[k].map(|v| v * scales[active.start + k]));
                    current[full.start + k] = Dual::new(physical[full.start + k], e);
                }

                let out = cached.evaluate_cells(
                    &current,
                    &old,
                    &ports,
                    Dual::constant(authored.gas_rt_m2_s2),
                    Dual::constant(authored.molecular_viscosity_pa_s),
                    &selected,
                )?;
                let mut coarse = Vec::new();
                'coarse: for oi in &input_to_outputs[*cell] {
                    let (_, orf, ora, rr, _) = &metadata[*oi];
                    for i in 0..4 {
                        for j in 0..4 {
                            let v = orf
                                .clone()
                                .enumerate()
                                .map(|(k, g)| {
                                    rr[k][i] * out.residual[g].eps[j] / scales[ora.start + k]
                                })
                                .sum::<f64>();
                            if !v.is_finite() {
                                coarse.push(Err(fail(
                                    "continuous transport coarse matrix nonfinite",
                                )));
                                break 'coarse;
                            }
                            if v != 0. {
                                coarse.push(Ok((oi * 4 + i, ci * 4 + j, v)));
                            }
                        }
                    }
                }
                if coarse.last().is_some_and(Result::is_err) {
                    return Ok(CellPacket {
                        coarse,
                        inverse: None,
                    });
                }
                let n = full.len();
                let inverse = (|| -> CaeResult<(CellInverse, usize)> {
                    let inverse = if let Some(descriptor) = sparse_descriptors.get(cell) {
                        let sparse_config =
                            sparse_policy.ok_or_else(|| fail("sparse block policy absent"))?;
                        let factor = ContinuousSparseBlock::new(
                            layout.basis(),
                            descriptor,
                            sparse_config,
                            |directions| {
                                for k in 0..n {
                                    let mut eps = [0.; 16];
                                    for (j, d) in directions.iter().enumerate() {
                                        eps[j] = d[k];
                                    }
                                    current[full.start + k] =
                                        Dual::new(physical[full.start + k], eps);
                                }
                                let result = cached.evaluate_cells(
                                    &current,
                                    &old,
                                    &ports,
                                    Dual::constant(authored.gas_rt_m2_s2),
                                    Dual::constant(authored.molecular_viscosity_pa_s),
                                    &[*cell],
                                )?;
                                Ok((0..directions.len())
                                    .map(|j| {
                                        full.clone()
                                            .map(|g| result.residual[g].eps[j])
                                            .collect::<Vec<_>>()
                                    })
                                    .collect::<Vec<_>>())
                            },
                        )?;
                        let correction = n
                            .checked_mul(factor.rank())
                            .and_then(|k| k.checked_mul(4))
                            .and_then(|k| {
                                factor
                                    .rank()
                                    .checked_mul(factor.rank())
                                    .and_then(|r| r.checked_mul(2))
                                    .and_then(|r| k.checked_add(r))
                            })
                            .ok_or_else(|| fail("sparse block total correction overflow"))?;
                        return Ok((
                            CellInverse::Sparse {
                                factor,
                                scales: scales[active.clone()].to_vec(),
                            },
                            correction,
                        ));
                    } else {
                        let mut matrix = vec![0.; n * n];
                        for first in (0..n).step_by(16) {
                            for k in 0..n {
                                let mut e = [0.; 16];
                                if k >= first && k < first + 16 {
                                    e[k - first] = scales[active.start + k];
                                }
                                current[full.start + k] = Dual::new(physical[full.start + k], e);
                            }

                            let out = cached.evaluate_cells(
                                &current,
                                &old,
                                &ports,
                                Dual::constant(authored.gas_rt_m2_s2),
                                Dual::constant(authored.molecular_viscosity_pa_s),
                                &[*cell],
                            )?;
                            for i in 0..n {
                                for j in first..(first + 16).min(n) {
                                    matrix[i * n + j] = out.residual[full.start + i].eps[j - first]
                                        / scales[active.start + i];
                                }
                            }
                        }
                        for g in full.clone() {
                            current[g] = Dual::constant(physical[g]);
                        }
                        let dense =
                            DenseMatrix::new(n, n, matrix).map_err(|e| fail(e.to_string()))?;
                        CellInverse::Dense(DenseLu::new(&dense).map_err(|e| fail(e.to_string()))?)
                    };
                    Ok((inverse, 0))
                })();
                Ok(CellPacket {
                    coarse,
                    inverse: Some(inverse),
                })
            })();
            for g in full.clone() {
                current[g] = Dual::constant(physical[g]);
            }
            result
        };
        let workers = metadata.len().min(rayon::current_num_threads());
        let mut currents = vec![current; workers];
        for first in (0..metadata.len()).step_by(workers) {
            let count = workers.min(metadata.len() - first);
            let packets = if workers == 1 {
                vec![evaluate_cell(&mut currents[0], first)]
            } else {
                currents[..count]
                    .par_iter_mut()
                    .enumerate()
                    .map(|(offset, current)| evaluate_cell(current, first + offset))
                    .collect::<Vec<_>>()
            };
            for (offset, packet) in packets.into_iter().enumerate() {
                let (_cell, _full, active, restriction, lift) = &metadata[first + offset];
                let packet = packet?;
                for entry in packet.coarse {
                    let (row, col, value) = entry?;
                    coarse_rows.push(row);
                    coarse_cols.push(col);
                    coarse_values.push(value);
                    if coarse_values.len() > policy.maximum_coarse_nonzeros {
                        return Err(fail("continuous transport coarse nonzero bound exceeded"));
                    }
                }
                let (inverse, correction) = packet
                    .inverse
                    .ok_or_else(|| fail("continuous transport coarse matrix nonfinite"))??;
                if correction != 0 {
                    total_entries = total_entries
                        .checked_add(correction)
                        .ok_or_else(|| fail("sparse block total correction overflow"))?;
                }
                if total_entries > policy.maximum_local_matrix_entries {
                    return Err(fail(
                        "continuous transport local correction entry bound exceeded",
                    ));
                }
                blocks.push(CellBlock {
                    active: active.clone(),
                    restrict: restriction.clone(),
                    lift: lift.clone(),
                    inverse,
                });
            }
        }
        let nc = blocks.len() * 4;
        let matrix = CscMatrix::from_triplets(nc, nc, &coarse_rows, &coarse_cols, &coarse_values)
            .map_err(|e| fail(e.to_string()))?;
        Ok(Self {
            blocks,
            factor: SparseLu::new(&matrix).map_err(|e| fail(e.to_string()))?,
            scales: scales.to_vec(),
            coarse_nonzeros: coarse_values.len(),
            local_matrix_entries: total_entries,
        })
    }
    pub fn coarse_dimension(&self) -> usize {
        self.blocks.len() * 4
    }
    pub fn coarse_nonzeros(&self) -> usize {
        self.coarse_nonzeros
    }
    pub fn local_matrix_entries(&self) -> usize {
        self.local_matrix_entries
    }
    fn local(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let mut out = vec![0.; v.len()];
        for b in &self.blocks {
            let x = match &b.inverse {
                CellInverse::Dense(factor) => factor
                    .solve(&v[b.active.clone()], 1, transpose)
                    .map_err(|e| fail(e.to_string()))?,
                CellInverse::Sparse { factor, scales } => {
                    let rhs = v[b.active.clone()]
                        .iter()
                        .zip(scales)
                        .map(|(v, d)| if transpose { v / d } else { v * d })
                        .collect::<Vec<_>>();
                    factor
                        .solve(&rhs, transpose)?
                        .iter()
                        .zip(scales)
                        .map(|(v, d)| if transpose { v * d } else { v / d })
                        .collect::<Vec<_>>()
                }
            };
            out[b.active.clone()].copy_from_slice(&x);
        }
        Ok(out)
    }
    fn restriction(&self, v: &[f64], transpose: bool) -> Vec<f64> {
        if !transpose {
            let mut out = vec![0.; self.coarse_dimension()];
            for (c, b) in self.blocks.iter().enumerate() {
                for (k, i) in b.active.clone().enumerate() {
                    for j in 0..4 {
                        out[c * 4 + j] += b.restrict[k][j] * v[i];
                    }
                }
            }
            out
        } else {
            let mut out = vec![0.; self.scales.len()];
            for (c, b) in self.blocks.iter().enumerate() {
                for (k, i) in b.active.clone().enumerate() {
                    for j in 0..4 {
                        out[i] += b.restrict[k][j] * v[c * 4 + j];
                    }
                }
            }
            out
        }
    }
    fn prolongation(&self, v: &[f64], transpose: bool) -> Vec<f64> {
        if !transpose {
            let mut out = vec![0.; self.scales.len()];
            for (c, b) in self.blocks.iter().enumerate() {
                for (k, i) in b.active.clone().enumerate() {
                    for j in 0..4 {
                        out[i] += b.lift[k][j] * v[c * 4 + j];
                    }
                }
            }
            out
        } else {
            let mut out = vec![0.; self.coarse_dimension()];
            for (c, b) in self.blocks.iter().enumerate() {
                for (k, i) in b.active.clone().enumerate() {
                    for j in 0..4 {
                        out[c * 4 + j] += b.lift[k][j] * v[i];
                    }
                }
            }
            out
        }
    }
    pub fn apply<F>(
        &self,
        physical_residual: &[f64],
        transpose: bool,
        jacobian: F,
    ) -> CaeResult<Vec<f64>>
    where
        F: Fn(&[f64], bool) -> CaeResult<Vec<f64>>,
    {
        if physical_residual.len() != self.scales.len()
            || physical_residual.iter().any(|v| !v.is_finite())
        {
            return Err(fail(
                "continuous transport preconditioner direction invalid",
            ));
        }
        let input = physical_residual
            .iter()
            .zip(&self.scales)
            .map(|(v, s)| if transpose { v * s } else { v / s })
            .collect::<Vec<_>>();
        let base = self.local(&input, transpose)?;
        let result = if !transpose {
            let jb = jacobian(
                &base
                    .iter()
                    .zip(&self.scales)
                    .map(|(v, s)| v * s)
                    .collect::<Vec<_>>(),
                false,
            )?;
            if jb.len() != self.scales.len() || jb.iter().any(|v| !v.is_finite()) {
                return Err(fail("continuous transport Jacobian callback invalid"));
            }
            let remainder = input
                .iter()
                .zip(jb.iter().zip(&self.scales))
                .map(|(v, (j, s))| v - j / s)
                .collect::<Vec<_>>();
            let rhs = self.restriction(&remainder, false);
            let c = self.factor.solve(&rhs).map_err(|e| fail(e.to_string()))?;
            let correction = self.prolongation(&c, false);
            base.iter()
                .zip(correction)
                .map(|(v, c)| v + c)
                .collect::<Vec<_>>()
        } else {
            let rhs = self.prolongation(&input, true);
            let c = self
                .factor
                .solve_transpose(&rhs)
                .map_err(|e| fail(e.to_string()))?;
            let correction = self.restriction(&c, true);
            let jt = jacobian(
                &correction
                    .iter()
                    .zip(&self.scales)
                    .map(|(v, s)| v / s)
                    .collect::<Vec<_>>(),
                true,
            )?;
            if jt.len() != self.scales.len() || jt.iter().any(|v| !v.is_finite()) {
                return Err(fail("continuous transport transpose callback invalid"));
            }
            let jt = jt
                .iter()
                .zip(&self.scales)
                .map(|(v, s)| v * s)
                .collect::<Vec<_>>();
            let mc = self.local(&jt, true)?;
            base.iter()
                .zip(correction.iter().zip(mc))
                .map(|(b, (c, m))| b + c - m)
                .collect::<Vec<_>>()
        };
        let out = result
            .iter()
            .zip(&self.scales)
            .map(|(v, s)| if transpose { v / s } else { v * s })
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("continuous transport preconditioner output nonfinite"));
        }
        Ok(out)
    }
}
