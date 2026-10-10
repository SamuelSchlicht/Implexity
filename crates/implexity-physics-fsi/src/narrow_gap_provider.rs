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

use crate::narrow_gap::{ContactParameters, GapGrid, GapSolution};
use implexity_ad::{HyperDual, Scalar};
use implexity_core::{
    CaeError, CaeResult,
    contracts::{
        CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor,
        ProviderProblem, Sensitivity,
    },
    packages::InstallContext,
};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
use implexity_optim::{
    design::NamedArrays,
    optimizer::OptimizerLifecycleConfig,
    provider_ops::{
        DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, LifecycleDeclaration,
    },
};
use ndarray::{ArrayD, IxDyn};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{any::Any, collections::BTreeMap, sync::Arc};

pub const NAME: &str = "conservative_narrow_gap";
pub const COORDINATE: &str = "interface:gap_m";
pub const RESPONSES: [&str; 3] = [
    "normal_force_N",
    "fluid_volume_m3",
    "outward_volume_flux_m3_s",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Liquid,
    Gas,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapDocument {
    pub schema: String,
    pub phase: Phase,
    pub grid: GapGrid,
    pub old_gap_m: Vec<f64>,
    pub old_inventory_m: Vec<f64>,
    pub contact: ContactParameters,
    pub pressure_reference_pa: f64,
    pub absolute_gas_pressure_pa: f64,
    pub maximum_gas_pressure_ratio: f64,
    pub tolerance: f64,
    pub maximum_iterations: usize,
}

fn fail(e: impl std::fmt::Display) -> CaeError {
    CaeError::contract(e.to_string())
}
fn problem(p: &ProviderProblem) -> CaeResult<&GapDocument> {
    p.downcast_ref()
        .ok_or_else(|| fail("narrow-gap problem type"))
}
fn diagnostics() -> Map<String, Value> {
    json!({"physical_qualification":false,"surface_scope":"fixed planar orthogonal patch","coordinate_array_axes":["y","x"],"fluid_model":"incompressible creeping Reynolds flow","liquid_model":"conservative inventory and cavitation complementarity","derivative_scope":"implicit derivative on a fixed cavitation and contact stratum","native_moving_fsi_attachment":false,"bulk_fluid_replacement":false,"maximum_sensitivity_cells":256,"maximum_evaluation_cells":65536}).as_object().unwrap().clone()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GapPortLinearization {
    pub solution: GapSolution,
    pub flux_m3_s: [f64; 4],
    pub flux_gap_m2_s: Vec<Vec<f64>>,
    pub flux_previous_m2_s: Vec<Vec<f64>>,
    pub flux_pressure_m3_per_pa_s: [[f64; 4]; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedbackPortSolution {
    pub boundary_pressure_pa: [Option<f64>; 4],
    pub linearization: GapPortLinearization,
    pub iterations: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureVolumeReservoir {
    pub compliance_m3_per_pa: f64,
    pub previous_pressure_pa: f64,
    pub volume_source_m3_s: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReservoirGapSolution {
    pub feedback: FeedbackPortSolution,
    pub reservoir_storage_m3_s: [f64; 4],
    pub combined_volume_balance_m3_s: f64,
}

impl GapDocument {
    pub fn validate(&self) -> CaeResult<()> {
        self.grid
            .validate(&self.old_gap_m, matches!(self.phase, Phase::Liquid))?;
        if self.schema != "implexity-narrow-gap/1"
            || self.old_inventory_m.len() != self.grid.cells()
            || self
                .old_inventory_m
                .iter()
                .zip(&self.old_gap_m)
                .any(|(m, h)| !m.is_finite() || *m < 0.0 || *m > *h)
            || [
                self.contact.asperity_height_m,
                self.contact.penalty_pa_per_m,
                self.contact.damping_pa_s_per_m,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
            || !self.pressure_reference_pa.is_finite()
            || !self.tolerance.is_finite()
            || self.tolerance <= 0.0
            || self.tolerance >= 1.0
            || self.maximum_iterations == 0
            || self.maximum_iterations > 1_000_000
            || !self.absolute_gas_pressure_pa.is_finite()
            || self.absolute_gas_pressure_pa <= 0.0
            || !self.maximum_gas_pressure_ratio.is_finite()
            || self.maximum_gas_pressure_ratio <= 0.0
            || self.maximum_gas_pressure_ratio >= 1.0
        {
            return Err(fail("invalid narrow-gap document"));
        }
        if matches!(self.phase, Phase::Gas) && self.old_inventory_m != self.old_gap_m {
            return Err(fail("gas-gap inventory must equal its gap"));
        }
        Ok(())
    }

    fn solve(&self, gap: &[f64]) -> CaeResult<GapSolution> {
        self.validate()?;
        match self.phase {
            Phase::Liquid => self.grid.solve_liquid(
                gap,
                &self.old_inventory_m,
                self.tolerance,
                self.maximum_iterations,
            ),
            Phase::Gas => self.grid.solve_gas(
                gap,
                &self.old_gap_m,
                self.absolute_gas_pressure_pa,
                self.maximum_gas_pressure_ratio,
                self.tolerance,
                self.maximum_iterations,
            ),
        }
    }

    fn reduced_fields<S: Scalar>(
        &self,
        gap: &[S],
        unknown: &[S],
        active: &[bool],
    ) -> (Vec<S>, Vec<S>) {
        let pressure = unknown
            .iter()
            .zip(active)
            .map(|(x, a)| if *a { *x } else { S::zero() })
            .collect();
        let inventory = unknown
            .iter()
            .zip(active)
            .zip(gap)
            .map(|((x, a), h)| if *a { *h } else { *x })
            .collect();
        (pressure, inventory)
    }

    fn reduced_residual<S: Scalar>(
        &self,
        gap: &[S],
        unknown: &[S],
        active: &[bool],
    ) -> CaeResult<Vec<S>> {
        let (pressure, inventory) = self.reduced_fields(gap, unknown, active);
        let old = self
            .old_inventory_m
            .iter()
            .map(|v| S::from_f64(*v))
            .collect::<Vec<_>>();
        self.grid
            .conservation_residual(gap, &pressure, &inventory, &old)
    }

    fn response<S: Scalar>(
        &self,
        name: &str,
        gap: &[S],
        unknown: &[S],
        active: &[bool],
    ) -> CaeResult<S> {
        let (pressure, inventory) = self.reduced_fields(gap, unknown, active);
        let area = self.grid.spacing_m[0] * self.grid.spacing_m[1];
        match name {
            "normal_force_N" => Ok(gap
                .iter()
                .zip(&pressure)
                .zip(&self.old_gap_m)
                .map(|((h, p), old)| {
                    let elastic = if h.value() < self.contact.asperity_height_m {
                        (S::from_f64(self.contact.asperity_height_m) - *h)
                            * self.contact.penalty_pa_per_m
                    } else {
                        S::zero()
                    };
                    let closing = (S::from_f64(*old) - *h) / self.grid.time_step_s;
                    let viscous =
                        if h.value() < self.contact.asperity_height_m && closing.value() > 0.0 {
                            closing * self.contact.damping_pa_s_per_m
                        } else {
                            S::zero()
                        };
                    (*p + self.pressure_reference_pa + elastic + viscous) * area
                })
                .sum()),
            "fluid_volume_m3" => Ok(inventory.into_iter().map(|v| v * area).sum()),
            "outward_volume_flux_m3_s" => Ok(inventory
                .into_iter()
                .zip(&self.old_inventory_m)
                .map(|(v, o)| (S::from_f64(*o) - v) * (area / self.grid.time_step_s))
                .sum()),
            _ => Err(fail("unknown narrow-gap response")),
        }
    }

    pub fn pressure_port_linearization(
        &self,
        gap: &[f64],
        ports: [Option<f64>; 4],
    ) -> CaeResult<GapPortLinearization> {
        let mut doc = self.clone();
        doc.grid.boundary_pressure_pa = ports;
        let solved = doc.solve(gap)?;
        let n = gap.len();
        if n > 256 {
            return Err(fail(
                "gap port linearization supports at most 256 patch cells",
            ));
        }
        let active = solved
            .state
            .pressure_above_cavitation_pa
            .iter()
            .map(|p| matches!(doc.phase, Phase::Gas) || *p > 0.0)
            .collect::<Vec<_>>();
        for i in 0..n {
            if matches!(doc.phase, Phase::Liquid)
                && !active[i]
                && (1.0 - solved.state.inventory_m[i] / gap[i]).abs() < 1e-8
            {
                return Err(fail(
                    "gap port linearization is undefined at cavitation transition",
                ));
            }
        }
        let unknown = solved
            .state
            .pressure_above_cavitation_pa
            .iter()
            .zip(&solved.state.inventory_m)
            .zip(&active)
            .map(|((p, m), a)| if *a { *p } else { *m })
            .collect::<Vec<_>>();
        let constant = |v: &[f64]| {
            v.iter()
                .map(|x| HyperDual::new(*x, 0., 0., 0.))
                .collect::<Vec<_>>()
        };
        let h = constant(gap);
        let u = constant(&unknown);
        let old = constant(&doc.old_inventory_m);
        let bp = ports.map(|p| p.map(|p| HyperDual::new(p, 0., 0., 0.)));
        let (p, m) = doc.reduced_fields(&h, &u, &active);
        let flux = doc.grid.boundary_flux_ports(
            gap,
            &solved.state.pressure_above_cavitation_pa,
            &solved.state.inventory_m,
            &ports,
        )?;
        let mut jac = DenseMatrix::zeros(n, n);
        let mut qstate = vec![vec![0.; n]; 4];
        for col in 0..n {
            let mut variable = u.clone();
            variable[col].e1 = 1.;
            let (p, m) = doc.reduced_fields(&h, &variable, &active);
            let r = doc
                .grid
                .conservation_residual_ports(&h, &p, &m, &old, &bp)?;
            let q = doc.grid.boundary_flux_ports(&h, &p, &m, &bp)?;
            for row in 0..n {
                jac.data[row * n + col] = r[row].e1;
            }
            for side in 0..4 {
                qstate[side][col] = q[side].e1;
            }
        }
        let factor = DenseLu::new(&jac).map_err(fail)?;
        let adjoint = (0..4)
            .map(|side| factor.solve(&qstate[side], 1, true).map_err(fail))
            .collect::<CaeResult<Vec<_>>>()?;
        let mut qgap = vec![vec![0.; n]; 4];
        let mut qprevious = vec![vec![0.; n]; 4];
        let area_dt = doc.grid.spacing_m[0] * doc.grid.spacing_m[1] / doc.grid.time_step_s;
        for col in 0..n {
            let mut variable = h.clone();
            variable[col].e1 = 1.;
            let (pv, mv) = doc.reduced_fields(&variable, &u, &active);
            let r = doc
                .grid
                .conservation_residual_ports(&variable, &pv, &mv, &old, &bp)?;
            let q = doc.grid.boundary_flux_ports(&variable, &pv, &mv, &bp)?;
            for side in 0..4 {
                qgap[side][col] = q[side].e1
                    - adjoint[side]
                        .iter()
                        .zip(&r)
                        .map(|(a, b)| a * b.e1)
                        .sum::<f64>();
                qprevious[side][col] = adjoint[side][col] * area_dt;
            }
        }
        let mut qp = [[0.; 4]; 4];
        for col in 0..4 {
            if let Some(mut b) = bp[col] {
                b.e1 = 1.;
                let mut variable = bp;
                variable[col] = Some(b);
                let r = doc
                    .grid
                    .conservation_residual_ports(&h, &p, &m, &old, &variable)?;
                let q = doc.grid.boundary_flux_ports(&h, &p, &m, &variable)?;
                for side in 0..4 {
                    qp[side][col] = q[side].e1
                        - adjoint[side]
                            .iter()
                            .zip(&r)
                            .map(|(a, b)| a * b.e1)
                            .sum::<f64>();
                }
            }
        }
        Ok(GapPortLinearization {
            solution: solved,
            flux_m3_s: flux,
            flux_gap_m2_s: qgap,
            flux_previous_m2_s: qprevious,
            flux_pressure_m3_per_pa_s: qp,
        })
    }

    pub fn solve_reservoir_ports(
        &self,
        gap: &[f64],
        reservoirs: [Option<PressureVolumeReservoir>; 4],
        tolerance_pa: f64,
        max_iterations: usize,
    ) -> CaeResult<ReservoirGapSolution> {
        let dt = self.grid.time_step_s;
        let mut base = self.grid.boundary_pressure_pa;
        let mut gain = [0.; 4];
        for side in 0..4 {
            if let Some(r) = &reservoirs[side] {
                if !r.compliance_m3_per_pa.is_finite()
                    || r.compliance_m3_per_pa <= 0.
                    || !r.previous_pressure_pa.is_finite()
                    || !r.volume_source_m3_s.is_finite()
                {
                    return Err(fail("invalid linear pressure-volume reservoir"));
                }
                gain[side] = dt / r.compliance_m3_per_pa;
                base[side] = Some(r.previous_pressure_pa + gain[side] * r.volume_source_m3_s);
            }
        }
        let feedback = self.solve_feedback_ports(gap, base, gain, tolerance_pa, max_iterations)?;
        let mut storage = [0.; 4];
        let mut prescribed_outflow = 0.;
        let mut source = 0.;
        for side in 0..4 {
            if let Some(r) = &reservoirs[side] {
                storage[side] = r.compliance_m3_per_pa
                    * (feedback.boundary_pressure_pa[side]
                        .ok_or_else(|| fail("reservoir pressure absent"))?
                        - r.previous_pressure_pa)
                    / dt;
                source += r.volume_source_m3_s;
            } else {
                prescribed_outflow += feedback.linearization.flux_m3_s[side];
            }
        }
        let patch_storage = feedback
            .linearization
            .solution
            .state
            .inventory_m
            .iter()
            .zip(&self.old_inventory_m)
            .map(|(m, o)| (m - o) * self.grid.spacing_m[0] * self.grid.spacing_m[1] / dt)
            .sum::<f64>();
        let combined_volume_balance_m3_s =
            patch_storage + storage.iter().sum::<f64>() + prescribed_outflow - source;
        Ok(ReservoirGapSolution {
            feedback,
            reservoir_storage_m3_s: storage,
            combined_volume_balance_m3_s,
        })
    }

    pub fn solve_feedback_ports(
        &self,
        gap: &[f64],
        base: [Option<f64>; 4],
        pressure_per_flux_pa_s_m3: [f64; 4],
        tolerance_pa: f64,
        max_iterations: usize,
    ) -> CaeResult<FeedbackPortSolution> {
        if pressure_per_flux_pa_s_m3
            .iter()
            .any(|v| !v.is_finite() || *v < 0.)
            || !tolerance_pa.is_finite()
            || tolerance_pa <= 0.
            || max_iterations == 0
        {
            return Err(fail("invalid gap port feedback parameters"));
        }
        let mut ports = base;
        for iteration in 1..=max_iterations {
            let map = self.pressure_port_linearization(gap, ports)?;
            let residual = (0..4)
                .map(|side| {
                    ports[side].map_or(0., |p| {
                        p - base[side].unwrap_or(0.)
                            - pressure_per_flux_pa_s_m3[side] * map.flux_m3_s[side]
                    })
                })
                .collect::<Vec<_>>();
            if residual.iter().all(|v| v.abs() <= tolerance_pa) {
                return Ok(FeedbackPortSolution {
                    boundary_pressure_pa: ports,
                    linearization: map,
                    iterations: iteration,
                });
            }
            let mut jac = DenseMatrix::identity(4);
            for row in 0..4 {
                if ports[row].is_some() {
                    for col in 0..4 {
                        jac.data[row * 4 + col] -= pressure_per_flux_pa_s_m3[row]
                            * map.flux_pressure_m3_per_pa_s[row][col];
                    }
                }
            }
            let delta = DenseLu::new(&jac)
                .map_err(fail)?
                .solve(&residual, 1, false)
                .map_err(fail)?;
            for side in 0..4 {
                if let Some(p) = ports[side] {
                    ports[side] = Some(p - delta[side]);
                }
            }
        }
        Err(CaeError::convergence(
            "coupled gap pressure ports did not converge",
        ))
    }

    pub fn evaluate_gap(&self, gap: &[f64]) -> CaeResult<Evaluation> {
        let solved = self.solve(gap)?;
        let active = solved
            .state
            .pressure_above_cavitation_pa
            .iter()
            .map(|p| matches!(self.phase, Phase::Gas) || *p > 0.0)
            .collect::<Vec<_>>();
        let unknown = solved
            .state
            .pressure_above_cavitation_pa
            .iter()
            .zip(&solved.state.inventory_m)
            .zip(&active)
            .map(|((p, m), a)| if *a { *p } else { *m })
            .collect::<Vec<_>>();
        let responses = RESPONSES
            .iter()
            .map(|name| {
                Ok((
                    name.to_string(),
                    self.response(name, gap, &unknown, &active)?,
                ))
            })
            .collect::<CaeResult<BTreeMap<_, _>>>()?;
        let velocity = gap
            .iter()
            .zip(&self.old_gap_m)
            .map(|(h, o)| (h - o) / self.grid.time_step_s)
            .collect::<Vec<_>>();
        let exchange = self.grid.surface_exchange(
            gap,
            &self.old_inventory_m,
            &solved.state,
            gap,
            &velocity,
            &self.contact,
            self.pressure_reference_pa,
        )?;
        let mut fields = BTreeMap::new();
        for (name, v) in [
            ("gap_m", gap.to_vec()),
            (
                "pressure_pa",
                solved.state.pressure_above_cavitation_pa.clone(),
            ),
            ("inventory_m", solved.state.inventory_m.clone()),
        ] {
            fields.insert(
                name.into(),
                FieldValue::Array(
                    ArrayD::from_shape_vec(IxDyn(&self.grid.array_shape()), v).map_err(fail)?,
                ),
            );
        }
        fields.insert(
            "surface_exchange".into(),
            FieldValue::Json(serde_json::to_value(exchange).map_err(fail)?),
        );
        let mut info = diagnostics();
        info.insert("iterations".into(), json!(solved.iterations));
        info.insert("relative_residual".into(), json!(solved.relative_residual));
        Ok(Evaluation {
            provider: NAME.into(),
            responses,
            fields,
            diagnostics: info,
        })
    }

    pub fn gradient_gap(&self, gap: &[f64], name: &str) -> CaeResult<Vec<f64>> {
        let solved = self.solve(gap)?;
        let n = gap.len();
        if n > 256 {
            return Err(fail(
                "narrow-gap sensitivity supports at most 256 patch cells",
            ));
        }
        let active = solved
            .state
            .pressure_above_cavitation_pa
            .iter()
            .map(|p| matches!(self.phase, Phase::Gas) || *p > 0.0)
            .collect::<Vec<_>>();
        for i in 0..n {
            if matches!(self.phase, Phase::Liquid)
                && !active[i]
                && (1.0 - solved.state.inventory_m[i] / gap[i]).abs() < 1e-8
            {
                return Err(fail(
                    "narrow-gap sensitivity crosses the cavitation transition",
                ));
            }
            if self.contact.penalty_pa_per_m > 0.0 || self.contact.damping_pa_s_per_m > 0.0 {
                if (gap[i] - self.contact.asperity_height_m).abs() < 1e-8 * gap[i]
                    || (self.contact.damping_pa_s_per_m > 0.0
                        && (gap[i] - self.old_gap_m[i]).abs() < 1e-8 * gap[i])
                {
                    return Err(fail(
                        "narrow-gap sensitivity crosses the contact transition",
                    ));
                }
            }
        }
        let unknown = solved
            .state
            .pressure_above_cavitation_pa
            .iter()
            .zip(&solved.state.inventory_m)
            .zip(&active)
            .map(|((p, m), a)| if *a { *p } else { *m })
            .collect::<Vec<_>>();
        let constant = |v: &[f64]| {
            v.iter()
                .map(|x| HyperDual::new(*x, 0.0, 0.0, 0.0))
                .collect::<Vec<_>>()
        };
        let hg = constant(gap);
        let u0 = constant(&unknown);
        let mut jac = DenseMatrix::zeros(n, n);
        let mut response_state = vec![0.0; n];
        for column in 0..n {
            let mut u = u0.clone();
            u[column].e1 = 1.0;
            let r = self.reduced_residual(&hg, &u, &active)?;
            for row in 0..n {
                jac.data[row * n + column] = r[row].e1;
            }
            response_state[column] = self.response(name, &hg, &u, &active)?.e1;
        }
        let factor = DenseLu::new(&jac).map_err(fail)?;
        let adjoint = factor.solve(&response_state, 1, true).map_err(fail)?;
        let mut gradient = vec![0.0; n];
        for column in 0..n {
            let mut h = hg.clone();
            h[column].e1 = 1.0;
            let r = self.reduced_residual(&h, &u0, &active)?;
            gradient[column] = self.response(name, &h, &u0, &active)?.e1
                - adjoint.iter().zip(r).map(|(a, b)| a * b.e1).sum::<f64>();
        }
        if gradient.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite narrow-gap sensitivity"));
        }
        Ok(gradient)
    }
}

pub struct NarrowGapProvider;
fn gap_design(p: &GapDocument, d: &NamedArrays) -> CaeResult<Vec<f64>> {
    if d.names().len() != 1 {
        return Err(fail("narrow-gap provider requires one gap coordinate"));
    }
    let a = d
        .get(COORDINATE)
        .ok_or_else(|| fail("narrow-gap coordinate absent"))?;
    if a.shape() != p.grid.array_shape() {
        return Err(fail("narrow-gap coordinate shape"));
    }
    Ok(a.iter().copied().collect())
}
impl CaeProvider for NarrowGapProvider {
    fn name(&self) -> &str {
        NAME
    }
    fn implementation(&self) -> &str {
        "implexity_physics_fsi::narrow_gap_provider::NarrowGapProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut d = ProviderDescriptor::new(NAME, vec![NAME.into()], vec![]);
        d.nonlinear = true;
        d.sensitivities = true;
        d.design_coordinates = vec![COORDINATE.into()];
        d.traits = diagnostics();
        Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Narrow gap and surface film","schema":editor_schema()}),"array")?)))
    }
    fn coupling_declaration(&self, _: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        Some(gap_coupling_declaration(NAME, false))
    }
    fn normalise_problem(&self, v: &Value) -> CaeResult<ProviderProblem> {
        let p: GapDocument = serde_json::from_value(v.clone()).map_err(fail)?;
        p.validate()?;
        Ok(Arc::new(p))
    }
    fn preflight(
        &self,
        p: &ProviderProblem,
        _: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        problem(p)?.validate()?;
        Ok(diagnostics())
    }
    fn evaluate(&self, p: &ProviderProblem, gap: &ArrayD<f64>) -> CaeResult<Evaluation> {
        let p = problem(p)?;
        if gap.shape() != p.grid.array_shape() {
            return Err(fail("narrow-gap coordinate shape"));
        }
        p.evaluate_gap(&gap.iter().copied().collect::<Vec<_>>())
    }
    fn sensitivity(
        &self,
        p: &ProviderProblem,
        gap: &ArrayD<f64>,
        name: &str,
    ) -> CaeResult<Sensitivity> {
        let p = problem(p)?;
        if gap.shape() != p.grid.array_shape() {
            return Err(fail("narrow-gap coordinate shape"));
        }
        let h = gap.iter().copied().collect::<Vec<_>>();
        let e = p.evaluate_gap(&h)?;
        let value = *e
            .responses
            .get(name)
            .ok_or_else(|| fail("unknown narrow-gap response"))?;
        Ok(Sensitivity {
            provider: NAME.into(),
            response: name.into(),
            value,
            gradient: ArrayD::from_shape_vec(
                IxDyn(&p.grid.array_shape()),
                p.gradient_gap(&h, name)?,
            )
            .map_err(fail)?,
            diagnostics: diagnostics(),
        })
    }
    fn interface(&self, n: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(n)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl DesignOperations for NarrowGapProvider {
    fn problem_document(&self, p: &ProviderProblem) -> Option<CaeResult<Value>> {
        Some(problem(p).and_then(|p| serde_json::to_value(p).map_err(fail)))
    }
    fn provides(&self, o: DesignOp) -> bool {
        matches!(
            o,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::OptimizerLifecycle
        )
    }
    fn authoring_design_coordinates(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(problem(p).map(|_| vec![COORDINATE.into()]))
    }
    fn problem_responses(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(problem(p).map(|_| RESPONSES.iter().map(|s| s.to_string()).collect()))
    }
    fn preflight_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let p = problem(p)?;
        p.grid
            .validate(&gap_design(p, d)?, matches!(p.phase, Phase::Liquid))?;
        Ok(diagnostics())
    }
    fn evaluate_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        o: usize,
    ) -> CaeResult<Evaluation> {
        if o != 0 {
            return Err(fail("narrow-gap provider has one operating point"));
        }
        let p = problem(p)?;
        p.evaluate_gap(&gap_design(p, d)?)
    }
    fn sensitivity_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        n: &str,
        o: usize,
    ) -> CaeResult<DesignSensitivity> {
        let e = self.evaluate_design(p, d, o)?;
        let p = problem(p)?;
        let gap = gap_design(p, d)?;
        let mut gradients = NamedArrays::new();
        gradients.insert(
            COORDINATE,
            ArrayD::from_shape_vec(IxDyn(&p.grid.array_shape()), p.gradient_gap(&gap, n)?)
                .map_err(fail)?,
        );
        Ok(DesignSensitivity {
            value: *e
                .responses
                .get(n)
                .ok_or_else(|| fail("unknown narrow-gap response"))?,
            gradients,
            diagnostics: e.diagnostics,
        })
    }
    fn sensitivities_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        n: &[String],
        o: usize,
    ) -> CaeResult<DesignSensitivities> {
        let mut responses = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in n {
            let r = self.sensitivity_design(p, d, name, o)?;
            responses.insert(name.clone(), r.value);
            gradients.insert(name.clone(), r.gradients);
        }
        Ok(DesignSensitivities {
            responses,
            gradients,
            diagnostics: diagnostics(),
        })
    }
    fn optimizer_lifecycle(&self, _: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            vec![COORDINATE.into()],
            "sensitivity_design",
            "evaluate_design",
            None,
            None,
            false,
            false,
        )?))
    }
}
pub fn install(ctx: &InstallContext<'_>) -> CaeResult<()> {
    ctx.register_provider(Arc::new(NarrowGapProvider))
        .map(|_| ())
}
pub fn editor_schema() -> Value {
    let mut grid = Map::new();
    for n in [
        "viscosity_pa_s",
        "density_kg_m3",
        "time_step_s",
        "flow_timescale_s",
        "maximum_aspect_ratio",
        "maximum_reduced_reynolds",
        "maximum_unsteady_inertia",
        "maximum_gap_slope",
    ] {
        grid.insert(n.into(), json!({"type":"number","exclusiveMinimum":0}));
    }
    grid.insert("shape".into(),json!({"type":"array","minItems":2,"maxItems":2,"items":{"type":"integer","minimum":1,"maximum":4096}}));
    for n in ["spacing_m", "mean_velocity_m_s", "relative_velocity_m_s"] {
        grid.insert(
            n.into(),
            json!({"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}}),
        );
    }
    grid.insert(
        "boundary_pressure_pa".into(),
        json!({"type":"array","minItems":4,"maxItems":4,"items":{"type":["number","null"]}}),
    );
    grid.insert(
        "boundary_inventory_m".into(),
        json!({"type":"array","minItems":4,"maxItems":4,"items":{"type":"number","minimum":0}}),
    );
    let required_grid = grid.keys().cloned().collect::<Vec<_>>();
    json!({"type":"object","additionalProperties":false,"required":["schema","phase","grid","old_gap_m","old_inventory_m","contact","pressure_reference_pa","absolute_gas_pressure_pa","maximum_gas_pressure_ratio","tolerance","maximum_iterations"],"properties":{
        "schema":{"type":"string","enum":["implexity-narrow-gap/1"]},"phase":{"type":"string","enum":["liquid","gas"]},"grid":{"type":"object","additionalProperties":false,"required":required_grid,"properties":grid},
        "old_gap_m":{"type":"array","items":{"type":"number","exclusiveMinimum":0}},"old_inventory_m":{"type":"array","items":{"type":"number","minimum":0}},
        "contact":{"type":"object","additionalProperties":false,"required":["asperity_height_m","penalty_pa_per_m","damping_pa_s_per_m"],"properties":{"asperity_height_m":{"type":"number","minimum":0},"penalty_pa_per_m":{"type":"number","minimum":0},"damping_pa_s_per_m":{"type":"number","minimum":0}}},
        "pressure_reference_pa":{"type":"number"},"absolute_gas_pressure_pa":{"type":"number","exclusiveMinimum":0},"maximum_gas_pressure_ratio":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":1},"tolerance":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":1},"maximum_iterations":{"type":"integer","minimum":1,"maximum":1000000}
    },"x-qualification":diagnostics()})
}

pub fn gap_coupling_declaration(provider: &str, bulk: bool) -> CaeResult<Value> {
    use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge, PhysicsPort};
    let mut declaration = CouplingDeclaration {
        provider: provider.into(), active_physics: vec!["gap_flow".into()],
        ports: vec![PhysicsPort { name: "gap_geometry".into(), owner: "gap_flow".into(), direction: "input".into(), conserved: false, units: "m".into() }],
        edges: vec![], closed_loops: vec![], intentionally_frozen: vec![],
        notes: vec!["Gap geometry is prescribed. No structural equation is solved.".into()],
    };
    if bulk {
        declaration.active_physics.push("bulk_flow".into());
        declaration.ports.push(PhysicsPort { name: "volume_exchange".into(), owner: "gap_flow".into(), direction: "output".into(), conserved: true, units: "m3 s-1".into() });
        declaration.ports.push(PhysicsPort { name: "port_pressure".into(), owner: "bulk_flow".into(), direction: "output".into(), conserved: false, units: "Pa".into() });
        declaration.edges = vec![
            CouplingEdge::new("bulk_flow", "gap_flow", "port_pressure", "monolithic", true, "Native pressure determines gap transport.").map_err(|e| fail(e.0))?,
            CouplingEdge::new("gap_flow", "bulk_flow", "mass_exchange", "monolithic", true, "Conservative gap flux changes native port pressure in the same step.").map_err(|e| fail(e.0))?,
        ];
        declaration.closed_loops.push(vec!["bulk_flow".into(), "gap_flow".into()]);
        declaration.notes.push("Port momentum uses a prescribed velocity closure with an external momentum ledger.".into());
    }
    Ok(declaration.to_value())
}
