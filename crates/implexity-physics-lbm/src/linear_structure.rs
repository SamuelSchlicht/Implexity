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
use implexity_linalg::dense::{DenseLu, DenseMatrix, eigvalsh};
use implexity_physics_solid::structural_dynamics::step_force_history;
use serde_json::Value;

use crate::nparray::asarray;

fn linalg<T>(r: Result<T, implexity_linalg::LinalgError>) -> CaeResult<T> {
    r.map_err(|e| CaeError::contract(e.to_string()))
}

#[derive(Clone, Debug)]
pub struct Mechanics {
    pub free: Vec<usize>,
    pub mass: DenseMatrix,
    pub stiffness: DenseMatrix,
    pub damping: DenseMatrix,
    pub stress: DenseMatrix,
    pub volumes: Vec<f64>,
    pub nodes: usize,
    pub young: Vec<f64>,
    pub poisson: Vec<f64>,
}

#[must_use]
pub fn dense_sub(full: &[f64], ndof: usize, free: &[usize]) -> DenseMatrix {
    let n = free.len();
    let mut data = Vec::with_capacity(n * n);
    for &i in free {
        for &j in free {
            data.push(full[i * ndof + j]);
        }
    }
    DenseMatrix { nrows: n, ncols: n, data }
}


pub fn well_conditioned(matrix: &DenseMatrix) -> CaeResult<bool> {
    let eigen = linalg(eigvalsh(matrix))?;
    let (lo, hi) = (eigen[0], eigen[eigen.len() - 1]);
    Ok(lo > 0.0 && hi / lo <= 1e12)
}

impl Mechanics {
    #[must_use]
    pub fn elements(&self) -> usize {
        self.volumes.len()
    }


    pub fn respond(
        &self,
        nodal: &[Vec<f64>],
        times: &[f64],
        thermal_stress: Option<&[Vec<f64>]>,
    ) -> CaeResult<Motion> {
        let forces: Vec<Vec<f64>> =
            nodal.iter().map(|row| self.free.iter().map(|&i| row[i]).collect()).collect();
        let zero = vec![0.0; self.free.len()];
        let (history, ledger) =
            step_force_history(&self.mass, &self.damping, &self.stiffness, &forces, times, &zero, &zero)?;
        let ndof = 3 * self.nodes;
        let ne = self.elements();
        let full: Vec<Vec<f64>> = history
            .displacement
            .iter()
            .map(|u| {
                let mut out = vec![0.0; ndof];
                for (k, &i) in self.free.iter().enumerate() {
                    out[i] = u[k];
                }
                out
            })
            .collect();
        let mut stress = Vec::with_capacity(full.len());
        for (t, u) in full.iter().enumerate() {
            let flat = linalg(self.stress.matvec(u))?;
            stress.push(
                (0..ne)
                    .map(|e| {
                        let mut s: [f64; 6] = std::array::from_fn(|i| flat[6 * e + i]);
                        if let Some(th) = thermal_stress {
                            for v in &mut s[..3] {
                                *v -= th[t][e];
                            }
                        }
                        s
                    })
                    .collect::<Vec<_>>(),
            );
        }
        Ok(Motion {
            velocity: history.velocity,
            acceleration_start: history.acceleration,
            acceleration_end: history.acceleration_end,
            full_displacement_m: full,
            stress_physical_pa: stress,
            ledger,
        })
    }

    #[must_use]
    pub fn strain_measures(&self, stress: &[Vec<[f64; 6]>], eigenstrain: Option<&[Vec<f64>]>) -> (f64, f64) {
        let mut elastic = f64::NEG_INFINITY;
        let mut total = f64::NEG_INFINITY;
        for (t, row) in stress.iter().enumerate() {
            for (e, s) in row.iter().enumerate() {
                let (young, nu) = (self.young[e], self.poisson[e]);
                let trace = s[0] + s[1] + s[2];
                let normals: [f64; 3] = std::array::from_fn(|i| ((1.0 + nu) * s[i] - nu * trace) / young);
                let shear: f64 = (0..3).map(|i| (2.0 * (1.0 + nu) * s[3 + i] / young).powi(2)).sum();
                elastic = elastic.max(normals.iter().map(|v| v * v).sum::<f64>() + 0.5 * shear);
                if let Some(eig) = eigenstrain {
                    let shifted: f64 = normals.iter().map(|v| (v + eig[t][e]).powi(2)).sum();
                    total = total.max(shifted + 0.5 * shear);
                }
            }
        }
        (elastic.sqrt(), if eigenstrain.is_some() { total.sqrt() } else { elastic.sqrt() })
    }

    #[must_use]
    pub fn motion(&self, m: &Motion) -> f64 {
        m.full_displacement_m
            .iter()
            .flat_map(|u| u.chunks(3).map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()))
            .fold(f64::NEG_INFINITY, f64::max)
    }

    #[must_use]
    pub fn mean_squared_stress(&self, stress: &[Vec<[f64; 6]>]) -> f64 {
        let total: f64 = self.volumes.iter().sum();
        let spatial: Vec<f64> = stress
            .iter()
            .map(|row| {
                row.iter()
                    .zip(&self.volumes)
                    .map(|(s, v)| {
                        (s[0] * s[0]
                            + s[1] * s[1]
                            + s[2] * s[2]
                            + 2.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5]))
                            * v
                    })
                    .sum::<f64>()
                    / total
            })
            .collect();
        let intervals = spatial.len() - 1;
        spatial.windows(2).map(|w| 0.5 * (w[1] + w[0])).sum::<f64>() / intervals as f64
    }

    #[must_use]
    pub fn mean_squared_stress_bar(&self, stress: &[Vec<[f64; 6]>]) -> Vec<Vec<[f64; 6]>> {
        let total: f64 = self.volumes.iter().sum();
        let steps = stress.len() - 1;
        stress
            .iter()
            .enumerate()
            .map(|(t, row)| {
                let c = 0.5 / steps as f64 * if t == 0 || t == steps { 1.0 } else { 2.0 };
                row.iter()
                    .zip(&self.volumes)
                    .map(|(s, v)| {
                        let w = c * v / total;
                        std::array::from_fn(|i| w * if i < 3 { 2.0 } else { 4.0 } * s[i])
                    })
                    .collect()
            })
            .collect()
    }


    pub fn reverse(
        &self,
        times: &[f64],
        mut full_bar: Vec<Vec<f64>>,
        stress_bar: Option<&[Vec<[f64; 6]>]>,
    ) -> CaeResult<(Vec<Vec<f64>>, Vec<Vec<f64>>)> {
        let nt = times.len();
        let steps = nt - 1;
        let ne = self.elements();
        let mut thermal_bar = vec![vec![0.0; ne]; nt];
        if let Some(sb) = stress_bar {
            let transpose = self.stress.transpose();
            for t in 0..nt {
                let flat: Vec<f64> = sb[t].iter().flatten().copied().collect();
                let add = linalg(transpose.matvec(&flat))?;
                for (a, b) in full_bar[t].iter_mut().zip(&add) {
                    *a += b;
                }
                for e in 0..ne {
                    thermal_bar[t][e] = -(sb[t][e][0] + sb[t][e][1] + sb[t][e][2]);
                }
            }
        }
        let nf = self.free.len();
        let mut u_bar: Vec<f64> = self.free.iter().map(|&i| full_bar[steps][i]).collect();
        let mut v_bar = vec![0.0; nf];
        let mut nodal_bar = vec![vec![0.0; 3 * self.nodes]; steps];
        for n in (0..steps).rev() {
            let dt = times[n + 1] - times[n];
            let effective = DenseMatrix {
                nrows: nf,
                ncols: nf,
                data: (0..nf * nf)
                    .map(|k| {
                        self.mass.data[k]
                            + 0.5 * dt * self.damping.data[k]
                            + 0.25 * dt * dt * self.stiffness.data[k]
                    })
                    .collect(),
            };
            let du_bar: Vec<f64> = u_bar.iter().zip(&v_bar).map(|(u, v)| u + 2.0 / dt * v).collect();
            let rhs_bar = linalg(linalg(DenseLu::new(&effective))?.solve(&du_bar, 1, true))?;
            let k_rhs = linalg(self.stiffness.transpose().matvec(&rhs_bar))?;
            let m_rhs = linalg(self.mass.transpose().matvec(&rhs_bar))?;
            for i in 0..nf {
                nodal_bar[n][self.free[i]] = 0.5 * dt * dt * rhs_bar[i];
                u_bar[i] += -0.5 * dt * dt * k_rhs[i] + full_bar[n][self.free[i]];
                v_bar[i] = -v_bar[i] + dt * m_rhs[i];
            }
        }
        Ok((nodal_bar, thermal_bar))
    }
}

#[derive(Clone, Debug)]
pub struct Motion {
    pub velocity: Vec<Vec<f64>>,
    pub acceleration_start: Vec<Vec<f64>>,
    pub acceleration_end: Vec<Vec<f64>>,
    pub full_displacement_m: Vec<Vec<f64>>,
    pub stress_physical_pa: Vec<Vec<[f64; 6]>>,
    pub ledger: Value,
}

impl Motion {
    #[must_use]
    pub fn is_finite(&self) -> bool {
        let rows = |v: &[Vec<f64>]| v.iter().flatten().all(|x| x.is_finite());
        rows(&self.velocity)
            && rows(&self.acceleration_start)
            && rows(&self.acceleration_end)
            && rows(&self.full_displacement_m)
            && self.stress_physical_pa.iter().flatten().flatten().all(|x| x.is_finite())
            && self.ledger.as_object().is_some_and(|m| {
                m.values().all(|v| {
                    let a = asarray(v);
                    a.is_real() && a.all_finite()
                })
            })
    }

    #[must_use]
    pub fn energy_error(&self) -> f64 {
        let values = |key: &str| asarray(&self.ledger[key]).data;
        let energy = values("mechanical_energy_J");
        let work = values("external_work_J");
        let dissipation = values("damping_dissipation_J");
        let balance = values("energy_balance_error_J");
        let mut error = f64::NEG_INFINITY;
        for n in 0..balance.len() {
            let scale =
                (energy[n + 1].abs() + energy[n].abs() + work[n].abs() + dissipation[n].abs()).max(1e-30);
            error = error.max(balance[n].abs() / scale);
        }
        error
    }
}
