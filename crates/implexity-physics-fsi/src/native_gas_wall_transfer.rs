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

use crate::gas_transient_kinetic::TransientGasScalar;
use crate::moving_contact::mapped_pair_law::NodeBinding;
use crate::native_cut_cell_partition::CutWallQuadrature;
use implexity_core::{CaeError, CaeResult};

pub struct NativeGasWallTransfer<S: TransientGasScalar> {
    trace_size: usize,
    points: Vec<WallBinding<S>>,
}

struct WallBinding<S: TransientGasScalar> {
    reference_m: [S; 3],
    columns: [[usize; 3]; 4],
    weights: [S; 4],
}

pub struct NativeGasWallKinematics<S: TransientGasScalar> {
    pub position_m: Vec<[S; 3]>,
    pub velocity_m_s: Vec<[S; 3]>,
}

pub struct NativeGasWallLoad<S: TransientGasScalar> {
    pub force_flux_n: Vec<S>,
    pub impulse_n_s: [S; 3],
    pub wall_work_j: S,
}

fn finite<S: TransientGasScalar>(s: S) -> bool {
    s.value().is_finite() && s.maximum_direction().is_finite()
}

impl<S: TransientGasScalar> NativeGasWallTransfer<S> {
    pub fn new(
        nodes: &[Vec<NodeBinding>; 2],
        quadrature: &[CutWallQuadrature<S>],
        trace: &[S],
        length_tolerance_m: f64,
    ) -> CaeResult<Self> {
        if trace.is_empty()
            || trace.iter().any(|&s| !finite(s))
            || !length_tolerance_m.is_finite()
            || length_tolerance_m < 0.
        {
            return Err(CaeError::contract("invalid native gas wall binding input"));
        }
        let mut points = Vec::with_capacity(quadrature.len());
        for q in quadrature {
            if q.body >= nodes.len()
                || q.native_nodes.iter().any(|&i| i >= nodes[q.body].len())
                || q.native_barycentric.iter().any(|&s| !finite(s))
                || q.position_m.iter().any(|&s| !finite(s))
            {
                return Err(CaeError::contract("invalid native gas wall quadrature"));
            }
            let partition = q.native_barycentric.iter().copied().sum::<S>() - S::one();
            let tolerance = 256. * f64::EPSILON;
            if partition.value().abs() > tolerance || partition.maximum_direction() > tolerance {
                return Err(CaeError::contract(
                    "native gas wall weights do not sum to one",
                ));
            }
            let columns = q.native_nodes.map(|i| nodes[q.body][i].force_flux);
            if columns.iter().flatten().any(|&i| i >= trace.len()) {
                return Err(CaeError::contract(
                    "native gas wall trace column is out of range",
                ));
            }
            let reference_m = std::array::from_fn(|d| {
                (0..4)
                    .map(|i| {
                        q.native_barycentric[i] * nodes[q.body][q.native_nodes[i]].reference_m[d]
                    })
                    .sum::<S>()
            });
            for d in 0..3 {
                let reconstructed = reference_m[d]
                    + (0..4)
                        .map(|i| q.native_barycentric[i] * trace[columns[i][d]])
                        .sum::<S>();
                let defect = reconstructed - q.position_m[d];
                let arithmetic = 512.
                    * f64::EPSILON
                    * reconstructed
                        .value()
                        .abs()
                        .max(q.position_m[d].value().abs());
                if !finite(reconstructed)
                    || defect.value().abs() > length_tolerance_m + arithmetic
                    || defect.maximum_direction() > length_tolerance_m + arithmetic
                {
                    return Err(CaeError::contract(
                        "native gas wall position does not match its binding",
                    ));
                }
            }
            points.push(WallBinding {
                reference_m,
                columns,
                weights: q.native_barycentric,
            });
        }
        Ok(Self {
            trace_size: trace.len(),
            points,
        })
    }

    pub fn kinematics(
        &self,
        start: &[S],
        end: &[S],
        time_fraction: f64,
        step_s: S,
    ) -> CaeResult<NativeGasWallKinematics<S>> {
        self.admit(start, end, step_s)?;
        if !time_fraction.is_finite() || !(0. ..=1.).contains(&time_fraction) {
            return Err(CaeError::contract("invalid native gas wall time fraction"));
        }
        let position_m = self
            .points
            .iter()
            .map(|p| {
                std::array::from_fn(|d| {
                    p.reference_m[d]
                        + (0..4)
                            .map(|i| {
                                p.weights[i]
                                    * (start[p.columns[i][d]] * (1. - time_fraction)
                                        + end[p.columns[i][d]] * time_fraction)
                            })
                            .sum::<S>()
                })
            })
            .collect();
        let velocity_m_s = self
            .points
            .iter()
            .map(|p| {
                std::array::from_fn(|d| {
                    (0..4)
                        .map(|i| {
                            p.weights[i] * (end[p.columns[i][d]] - start[p.columns[i][d]]) / step_s
                        })
                        .sum::<S>()
                })
            })
            .collect();
        Ok(NativeGasWallKinematics {
            position_m,
            velocity_m_s,
        })
    }

    pub fn scatter(
        &self,
        impulses_n_s: &[[S; 3]],
        start: &[S],
        end: &[S],
        step_s: S,
    ) -> CaeResult<NativeGasWallLoad<S>> {
        self.admit(start, end, step_s)?;
        if impulses_n_s.len() != self.points.len()
            || impulses_n_s.iter().flatten().any(|&s| !finite(s))
        {
            return Err(CaeError::contract("invalid native gas wall impulses"));
        }
        let mut force_flux_n = vec![S::zero(); self.trace_size];
        let mut impulse_n_s = [S::zero(); 3];
        let mut wall_work_j = S::zero();
        for (p, impulse) in self.points.iter().zip(impulses_n_s) {
            for d in 0..3 {
                impulse_n_s[d] += impulse[d];
                for i in 0..4 {
                    let force = impulse[d] * p.weights[i] / step_s;
                    force_flux_n[p.columns[i][d]] += force;
                    wall_work_j += force * (end[p.columns[i][d]] - start[p.columns[i][d]]);
                }
            }
        }
        Ok(NativeGasWallLoad {
            force_flux_n,
            impulse_n_s,
            wall_work_j,
        })
    }

    fn admit(&self, start: &[S], end: &[S], step_s: S) -> CaeResult<()> {
        if start.len() != self.trace_size
            || end.len() != self.trace_size
            || !finite(step_s)
            || step_s.value() <= 0.
            || start.iter().chain(end).any(|&s| !finite(s))
        {
            return Err(CaeError::contract("invalid native gas wall motion"));
        }
        Ok(())
    }
}

pub struct NativeGasWallLoadBars {
    pub impulses_n_s: Vec<[f64; 3]>,
    pub native_barycentric: Vec<[f64; 4]>,
    pub trace_start: Vec<f64>,
    pub trace_end: Vec<f64>,
    pub step_s: f64,
}

impl NativeGasWallTransfer<f64> {
    pub fn scatter_adjoint(
        &self,
        impulses_n_s: &[[f64; 3]],
        start: &[f64],
        end: &[f64],
        step_s: f64,
        force_flux_bar: &[f64],
        total_impulse_bar: [f64; 3],
        wall_work_bar: f64,
    ) -> CaeResult<NativeGasWallLoadBars> {
        self.scatter(impulses_n_s, start, end, step_s)?;
        if force_flux_bar.len() != self.trace_size
            || force_flux_bar
                .iter()
                .chain(&total_impulse_bar)
                .any(|v| !v.is_finite())
            || !wall_work_bar.is_finite()
        {
            return Err(CaeError::contract("invalid native gas wall load cotangent"));
        }
        let mut bars = NativeGasWallLoadBars {
            impulses_n_s: vec![[0.; 3]; self.points.len()],
            native_barycentric: vec![[0.; 4]; self.points.len()],
            trace_start: vec![0.; self.trace_size],
            trace_end: vec![0.; self.trace_size],
            step_s: 0.,
        };
        for (q, (p, impulse)) in self.points.iter().zip(impulses_n_s).enumerate() {
            bars.impulses_n_s[q] = total_impulse_bar;
            for d in 0..3 {
                for i in 0..4 {
                    let k = p.columns[i][d];
                    let source = force_flux_bar[k] + wall_work_bar * (end[k] - start[k]);
                    bars.impulses_n_s[q][d] += p.weights[i] * source / step_s;
                    bars.native_barycentric[q][i] += impulse[d] * source / step_s;
                    let motion = wall_work_bar * impulse[d] * p.weights[i] / step_s;
                    bars.trace_start[k] -= motion;
                    bars.trace_end[k] += motion;
                    bars.step_s -= impulse[d] * p.weights[i] * source / (step_s * step_s);
                }
            }
        }
        Ok(bars)
    }
}
