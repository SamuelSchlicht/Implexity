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

use crate::gas_multiscale_network::{
    MultiscaleNetworkInputs, MultiscaleNetworkPolicy, MultiscaleNetworkTopology,
    residual_cells_prepared_scalar,
};
use crate::gas_transient_kinetic::velocity_equilibrium_density;
use implexity_ad::Dual;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{lu::SparseLu, sparse::CscMatrix};

pub struct GasMomentCoarse {
    columns: Vec<Vec<[f64; 4]>>,
    restriction: Vec<[f64; 4]>,
    active: Vec<usize>,
    offsets: Vec<Option<usize>>,
    factor: SparseLu,
    dimension: usize,
}
impl GasMomentCoarse {
    pub fn new(
        inputs: &MultiscaleNetworkInputs<f64>,
        policy: &MultiscaleNetworkPolicy,
        topology: &MultiscaleNetworkTopology,
        state: &[f64],
        offsets: &[Option<usize>],
        scale: &[f64],
    ) -> CaeResult<Self> {
        let fail = |s: &str| CaeError::contract(format!("gas moment coarse operator: {s}"));
        let nv = inputs.gas.grid.velocity_m_s.len();
        let active = offsets
            .iter()
            .enumerate()
            .filter_map(|(c, o)| o.map(|_| c))
            .collect::<Vec<_>>();
        if nv == 0
            || active.is_empty()
            || offsets.len() != inputs.gas.volume_m3.len()
            || scale.len() != offsets.len()
            || active.len().checked_mul(nv) != Some(state.len())
            || active.iter().enumerate().any(|(i, c)| {
                offsets[*c] != Some(i * nv)
                    || !inputs.gas.volume_m3[*c].is_finite()
                    || inputs.gas.volume_m3[*c] <= 0.
            })
            || state.iter().any(|v| !v.is_finite() || *v < 0.)
            || scale.iter().any(|v| !v.is_finite() || *v <= 0.)
            || !inputs.gas.gas_rt_m2_s2.is_finite()
            || inputs.gas.gas_rt_m2_s2 <= 0.
            || topology.residual_support_cells.len() != offsets.len()
            || topology
                .residual_support_cells
                .iter()
                .flatten()
                .any(|c| *c >= offsets.len())
        {
            return Err(fail("invalid layout"));
        }
        let thermal = inputs.gas.gas_rt_m2_s2.sqrt();
        let restriction = inputs
            .gas
            .grid
            .velocity_m_s
            .iter()
            .map(|c| [1., c[0] / thermal, c[1] / thermal, c[2] / thermal])
            .collect::<Vec<_>>();
        let mut columns = vec![vec![]; offsets.len()];
        for &c in &active {
            let o = offsets[c].unwrap();
            let mut moment = [0.; 4];
            for k in 0..nv {
                for i in 0..4 {
                    moment[i] += restriction[k][i] * state[o + k];
                }
            }
            if moment[0] == 0. {
                moment = [1., 0., 0., 0.];
            }
            let dual: [Dual<4>; 4] = std::array::from_fn(|i| {
                let mut d = [0.; 4];
                d[i] = 1.;
                Dual::new(moment[i], d)
            });
            let g = velocity_equilibrium_density(
                &inputs.gas.grid,
                dual[0] * (scale[c] / inputs.gas.volume_m3[c]),
                std::array::from_fn(|i| dual[i + 1] * thermal / dual[0]),
                Dual::constant(inputs.gas.gas_rt_m2_s2),
                &policy.kinetic,
            )?;
            columns[c] = g
                .iter()
                .map(|g| g.eps.map(|d| d * inputs.gas.volume_m3[c] / scale[c]))
                .collect();
            for i in 0..4 {
                for j in 0..4 {
                    let p = (0..nv)
                        .map(|k| restriction[k][i] * columns[c][k][j])
                        .sum::<f64>();
                    if !p.is_finite() || (p - f64::from(i == j)).abs() > 1e-9 {
                        return Err(fail("equilibrium moment lift is not reciprocal"));
                    }
                }
            }
        }
        let dual = crate::gas_multiscale_solver::lift::<4>(inputs);
        let mut rows = vec![];
        let mut cols = vec![];
        let mut values = vec![];
        for (ri, &c) in active.iter().enumerate() {
            for &d in &topology.residual_support_cells[c] {
                let Some(doffset) = offsets[d] else { continue };
                let ci = doffset / nv;
                let r = residual_cells_prepared_scalar(
                    &dual,
                    topology,
                    |cell, k| {
                        offsets[cell].map_or(Dual::constant(0.), |o| {
                            Dual::new(
                                state[o + k] * scale[cell],
                                if cell == d {
                                    columns[d][k].map(|v| v * scale[cell])
                                } else {
                                    [0.; 4]
                                },
                            )
                        })
                    },
                    &[c],
                    policy,
                )?;
                for i in 0..4 {
                    for j in 0..4 {
                        let v = (0..nv)
                            .map(|k| {
                                restriction[k][i] * r.inventory_residual_kg[k].eps[j] / scale[c]
                            })
                            .sum::<f64>();
                        if !v.is_finite() {
                            return Err(fail("nonfinite Galerkin block"));
                        }
                        rows.push(ri * 4 + i);
                        cols.push(ci * 4 + j);
                        values.push(v);
                    }
                }
            }
        }
        let matrix =
            CscMatrix::from_triplets(active.len() * 4, active.len() * 4, &rows, &cols, &values)
                .map_err(|e| fail(&e.to_string()))?;
        let factor = SparseLu::new(&matrix).map_err(|e| fail(&e.to_string()))?;
        Ok(Self {
            columns,
            restriction,
            active,
            offsets: offsets.to_vec(),
            factor,
            dimension: state.len(),
        })
    }
    pub fn restrict(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if v.len() != self.dimension || v.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("gas coarse restriction shape"));
        }
        let nv = self.restriction.len();
        let mut out = vec![0.; self.active.len() * 4];
        for (i, &c) in self.active.iter().enumerate() {
            let o = self.offsets[c].unwrap();
            for k in 0..nv {
                for j in 0..4 {
                    out[4 * i + j] += v[o + k]
                        * if transpose {
                            self.columns[c][k][j]
                        } else {
                            self.restriction[k][j]
                        };
                }
            }
        }
        Ok(out)
    }
    pub fn lift(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if v.len() != self.active.len() * 4 || v.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("gas coarse lift shape"));
        }
        let nv = self.restriction.len();
        let mut out = vec![0.; self.dimension];
        for (i, &c) in self.active.iter().enumerate() {
            let o = self.offsets[c].unwrap();
            for k in 0..nv {
                out[o + k] = (0..4)
                    .map(|j| {
                        v[4 * i + j]
                            * if transpose {
                                self.restriction[k][j]
                            } else {
                                self.columns[c][k][j]
                            }
                    })
                    .sum();
            }
        }
        Ok(out)
    }
    pub fn solve(&self, v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        (if transpose {
            self.factor.solve_transpose(v)
        } else {
            self.factor.solve(v)
        })
        .map_err(|e| CaeError::contract(format!("gas coarse solve: {e}")))
    }
}
