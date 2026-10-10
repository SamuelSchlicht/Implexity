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

use crate::moving_contact::sample_projection::{PairSampleProjection, from_observables};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_solve::{
    checkpointed_history::{history_adjoint_many, run_history},
    state_store::StoreBudget,
    time_stepper::{StepParameters, TimeStepper},
};
use serde_json::Value;
use std::collections::BTreeMap;
fn fail(s: impl std::fmt::Display) -> CaeError {
    CaeError::contract(s.to_string())
}

use crate::moving_contact::{
    native_surface_binding::NativeNodalBinding,
    separate_body::{MovingFsiPairModel, PairCarrier, PairSolidField},
};
use crate::narrow_gap::ContactParameters;
use crate::surface_gap::{SurfaceGapCell, SurfaceGapKinematics};
use crate::surface_gap_field::{
    NativeGapLattice, SurfaceGapField, SurfaceGapFieldConfig, SurfaceGapFieldPolicy,
};
use crate::surface_gap_history::{GapFaceConnection, OrthogonalFacePolicy};
use implexity_solve::multirate_coupling::{FluxDrivenField, MultirateOptions, MultirateStepper};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceGapDocument {
    pub solid_iso: f64,
    pub cells: Vec<SurfaceGapCell>,
    pub connections: Vec<GapFaceConnection>,
    pub excluded_cells: Vec<usize>,
    pub ports: Vec<Vec<(usize, f64)>>,
    pub removed_carrier_points: Vec<usize>,
    pub orthogonal_face_policy: OrthogonalFacePolicy,
    pub contact: ContactParameters,
    pub policy: SurfaceGapFieldPolicy,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceGapProblemDocument {
    pub schema: String,
    pub bodies: [Value; 2],
    pub gap: SurfaceGapDocument,
}
pub struct SurfaceGapProblem {
    pub model: MovingFsiPairModel,
    pub document: SurfaceGapProblemDocument,
    pub identity: String,
}
pub type SurfaceGapStepper<'a> = MultirateStepper<SurfaceGapField, PairSolidField<'a>>;
impl SurfaceGapProblemDocument {
    pub fn build(self) -> CaeResult<SurfaceGapProblem> {
        if self.schema != "implexity-surface-gap-fsi-pair/1" {
            return Err(fail("surface-gap pair schema"));
        }
        let model = MovingFsiPairModel::from_bodies(
            crate::problem::normalise(&self.bodies[0])?,
            crate::problem::normalise(&self.bodies[1])?,
        )?;
        let _ = model.solid_field()?;
        for body in 0..2 {
            let native = model.native_body(body)?;
            if !native.problem.observables.fluid.is_empty()
                || !native.problem.observables.interface.is_empty()
            {
                return Err(fail(
                    "surface-gap bodies support solid samples only; select post-exchange gap samples in history",
                ));
            }
        }
        for feature in self
            .gap
            .cells
            .iter()
            .flat_map(|c| c.features.iter().flatten())
        {
            if let crate::moving_contact::surface_map::SurfaceFeature::DensityEdge { iso, .. } =
                feature
            {
                if *iso != self.gap.solid_iso {
                    return Err(fail(
                        "surface-gap density features and geometric ownership must use identical solid iso",
                    ));
                }
            }
        }
        if matches!(
            model.native_body(0)?.problem.coupling.mode,
            implexity_solve::multirate_coupling::CouplingMode::Loose { .. }
        ) {
            return Err(fail(
                "surface-gap history requires strong iterative or monolithic coupling",
            ));
        }
        if self.gap.cells.is_empty() || self.gap.cells.len() + self.gap.ports.len() > 256 {
            return Err(fail("surface-gap cell-port count must be in 1..256"));
        }
        let time = &model.native_body(0)?.problem.time;
        let other = &model.native_body(1)?.problem.time;
        if time.history_steps() != other.history_steps()
            || format!("{:?}", time.checkpoint) != format!("{:?}", other.checkpoint)
        {
            return Err(fail("surface-gap native history policies differ"));
        }
        let identity =
            implexity_core::json::canonical_sha256(&serde_json::to_value(&self).map_err(fail)?);
        Ok(SurfaceGapProblem {
            model,
            document: self,
            identity,
        })
    }
}
impl SurfaceGapProblem {
    pub fn physical_design(&self, controls: [&[f64]; 2]) -> CaeResult<Vec<f64>> {
        self.model.event_physical_design(controls)
    }
    pub fn stepper(&self, design: &[f64]) -> CaeResult<SurfaceGapStepper<'_>> {
        let field = self.model.solid_field()?;
        let binding = NativeNodalBinding::new(&self.model, &field, design)?;
        let geometric_owner = Arc::new(
            crate::surface_gap_geometry_owner::NativeGapGeometryOwner::from_native(
                &binding,
                self.document.gap.solid_iso,
                self.document.gap.policy.geometry_tolerance_m,
            )?,
        );
        let kinematics =
            SurfaceGapKinematics::from_native(&binding, self.document.gap.cells.clone())?
                .as_trace_kinematics()?;
        let initial = field.initial_state(design)?;
        let trace = field.trace_operator().matvec(&initial).map_err(fail)?;
        let body = self.model.native_body(0)?;
        let mut bulk = body.lbm_config();
        for &i in &self.document.gap.excluded_cells {
            if i >= bulk.solid_mask.len() {
                return Err(fail("surface-gap excluded bulk cell index"));
            }
            bulk.solid_mask[i] = true;
        }
        let carrier = Arc::new(PairCarrier::new(
            body.carrier.clone(),
            self.model.native_body(1)?.carrier.clone(),
        ));
        let g = &self.document.gap;
        let lattice = match body.problem.fluid.lattice {
            implexity_physics_lbm::moving::field::LatticeKind::D3Q19 => NativeGapLattice::D3Q19,
            implexity_physics_lbm::moving::field::LatticeKind::D3Q27 => NativeGapLattice::D3Q27,
            _ => {
                return Err(fail(
                    "surface-gap native history requires a 3D bulk lattice",
                ));
            }
        };
        let fluid = SurfaceGapField::new(SurfaceGapFieldConfig {
            bulk,
            lattice,
            geometric_owner: Some(geometric_owner),
            original_carrier: carrier,
            removed_carrier_points: g.removed_carrier_points.clone(),
            kinematics,
            initial_trace: trace,
            excluded_cells: g.excluded_cells.clone(),
            ports: g.ports.clone(),
            connections: g.connections.clone(),
            orthogonal_face_policy: g.orthogonal_face_policy.clone(),
            contact: g.contact.clone(),
            policy: g.policy.clone(),
        })?;
        let c = &body.problem.coupling;
        Ok(MultirateStepper::new(
            fluid,
            field,
            MultirateOptions {
                mode: c.mode.clone(),
                schur_ratio_limit: c.schur_ratio_limit,
                work_defect_limit: c.work_defect_limit,
            },
        )?
        .with_identity(self.identity.clone())
        .with_initial_prediction(c.initial_prediction)?.with_field_newton(c.field_newton)?
        .with_linear_solves(c.linear_solves)?
        .with_step_cache_bytes(c.step_cache_bytes))
    }
    pub fn admit_loose_coupling(&self, _: &SurfaceGapStepper<'_>, _: &[f64]) -> CaeResult<Value> {
        Ok(
            serde_json::json!({"mode":format!("{:?}",self.model.native_body(0)?.problem.coupling.mode),"work_ledger_owner":"native_multirate_history","required":false}),
        )
    }
}

pub const SCHEMA: &str = "implexity-surface-gap-fsi-history/1";
const DESIGN_KINDS: [&str; 4] = [
    "design_volume_fraction",
    "removed_volume_fraction",
    "removed_volume_m3",
    "removal_depth_m",
];
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceGapHistoryDocument {
    pub schema: String,
    pub problem: SurfaceGapProblemDocument,
    pub observables: Value,
    pub responses: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_outputs: Option<[String; 2]>,
}
pub struct SurfaceGapHistoryResponse {
    pub value: f64,
    pub control_gradients: Option<[Vec<f64>; 2]>,
}
pub struct SurfaceGapHistory {
    pub responses: BTreeMap<String, SurfaceGapHistoryResponse>,
    pub samples: DenseMatrix,
    pub sample_names: Vec<String>,
    pub final_state: Vec<f64>,
    pub ledger: Vec<BTreeMap<String, f64>>,
    pub record: Value,
    pub coupling_admission: Value,
    pub problem_identity: String,
}
pub struct SurfaceGapHistoryProblem {
    pub problem: SurfaceGapProblem,
    pub document: SurfaceGapHistoryDocument,
}
fn projected(projection: &PairSampleProjection, source: &DenseMatrix) -> CaeResult<DenseMatrix> {
    let mut samples = DenseMatrix::zeros(source.nrows, projection.names().len());
    for r in 0..source.nrows {
        let values = projection.values(&source.data[r * source.ncols..(r + 1) * source.ncols])?;
        let width = samples.ncols;
        samples.data[r * width..(r + 1) * width].copy_from_slice(&values);
    }
    Ok(samples)
}
fn source_bar(projection: &PairSampleProjection, bar: &DenseMatrix) -> CaeResult<DenseMatrix> {
    if bar.ncols != projection.names().len() {
        return Err(fail("surface-gap history response sample cotangent"));
    }
    let mut result = DenseMatrix::zeros(bar.nrows, projection.source_names().len());
    for r in 0..bar.nrows {
        let values = projection.pullback(&bar.data[r * bar.ncols..(r + 1) * bar.ncols])?;
        let width = result.ncols;
        result.data[r * width..(r + 1) * width].copy_from_slice(&values);
    }
    Ok(result)
}
impl SurfaceGapHistoryDocument {
    pub fn from_json(bytes: &[u8]) -> CaeResult<Self> {
        serde_json::from_slice(bytes).map_err(fail)
    }
    pub fn build(self) -> CaeResult<SurfaceGapHistoryProblem> {
        if self.schema != SCHEMA {
            return Err(fail("surface-gap history schema"));
        }
        if let Some(refs) = &self.geometry_outputs {
            if refs[0] == refs[1] || refs.iter().any(|v| v.is_empty() || v.trim() != v) {
                return Err(fail(
                    "surface-gap geometry output refs must be distinct nonempty trimmed strings",
                ));
            }
        }
        let problem = self.problem.clone().build()?;
        let autonomous = problem.model.fixed_horizon_autonomous()?;
        let time = &problem.model.native_body(0)?.problem.time;
        let other = &problem.model.native_body(1)?.problem.time;
        if time.history_steps() != other.history_steps()
            || format!("{:?}", time.checkpoint) != format!("{:?}", other.checkpoint)
        {
            return Err(fail(
                "surface-gap history native horizon or checkpoint policies differ",
            ));
        }
        let fields = problem.model.solid_field()?;
        use implexity_solve::multirate_coupling::FluxDrivenField;
        let mut names = fields.sample_names().to_vec();
        names.extend(crate::surface_gap_field::sample_names());
        let projection = from_observables(
            &self.observables,
            [
                &self.problem.bodies[0]["observables"],
                &self.problem.bodies[1]["observables"],
            ],
            names,
        )?;
        let program = projection.bind_program(
            &self.responses,
            &DESIGN_KINDS,
            time.history_steps(),
            false,
            autonomous,
        )?;
        let count =
            problem.model.native_body(0)?.chain.len() + problem.model.native_body(1)?.chain.len();
        let _ = problem
            .model
            .event_design_terms(&vec![0.; count], &program)?;
        Ok(SurfaceGapHistoryProblem {
            problem,
            document: self,
        })
    }
}
impl SurfaceGapHistoryProblem {
    pub fn execute(
        &self,
        controls: [&[f64]; 2],
        budget: &StoreBudget,
        gradients: bool,
    ) -> CaeResult<SurfaceGapHistory> {
        let design = self.problem.physical_design(controls)?;
        let stepper = self.problem.stepper(&design)?;
        let projection = from_observables(
            &self.document.observables,
            [
                &self.document.problem.bodies[0]["observables"],
                &self.document.problem.bodies[1]["observables"],
            ],
            stepper.sample_names().to_vec(),
        )?;
        let time = &self.problem.model.native_body(0)?.problem.time;
        let autonomous = self.problem.model.fixed_horizon_autonomous()?;
        let program = projection.bind_program(
            &self.document.responses,
            &DESIGN_KINDS,
            time.history_steps(),
            false,
            autonomous,
        )?;
        let coupling_admission = self.problem.admit_loose_coupling(&stepper, &design)?;
        let initial = stepper.initial_state(&design)?;
        let p = StepParameters {
            design: &design,
            time_scale: 1.,
        };
        let run = run_history(
            &stepper,
            p,
            &initial,
            1,
            Some(time.history_steps()),
            None,
            time.checkpoint.clone(),
            budget,
        )?;
        let samples = projected(&projection, run.samples())?;
        let values = program.evaluate(&samples, stepper.nominal_step_s(), false, autonomous)?;
        let dynamic_gradients = if gradients {
            let bars: Vec<_> = values
                .iter()
                .map(|(_, v)| source_bar(&projection, &v.d_samples))
                .collect::<CaeResult<_>>()?;
            let final_bars = vec![vec![0.; stepper.state_size()]; bars.len()];
            Some(history_adjoint_many(&stepper, p, &run, &bars, &final_bars)?)
        } else {
            None
        };
        let split = self.problem.model.native_body(0)?.chain.len();
        let pullback = |g: &[f64]| -> CaeResult<[Vec<f64>; 2]> {
            if g.len() != design.len() {
                return Err(fail("surface-gap history physical design cotangent shape"));
            }
            Ok([
                self.problem
                    .model
                    .native_body(0)?
                    .chain
                    .pullback(controls[0], &g[..split])?,
                self.problem
                    .model
                    .native_body(1)?
                    .chain
                    .pullback(controls[1], &g[split..])?,
            ])
        };
        let mut responses = BTreeMap::new();
        for (i, (name, v)) in values.into_iter().enumerate() {
            let control_gradients = if let Some(all) = &dynamic_gradients {
                let mut g = all[i].design.clone();
                let initial_bar = stepper.initial_state_vjp(&design, &all[i].initial_state)?;
                if initial_bar.len() != g.len() {
                    return Err(fail("surface-gap history initial design cotangent shape"));
                }
                for (a, b) in g.iter_mut().zip(initial_bar) {
                    *a += b;
                }
                Some(pullback(&g)?)
            } else {
                None
            };
            if responses
                .insert(
                    name,
                    SurfaceGapHistoryResponse {
                        value: v.value,
                        control_gradients,
                    },
                )
                .is_some()
            {
                return Err(fail("surface-gap history response duplicated"));
            }
        }
        for (name, value, g) in self.problem.model.event_design_terms(&design, &program)? {
            let control_gradients = if gradients { Some(pullback(&g)?) } else { None };
            if responses
                .insert(
                    name,
                    SurfaceGapHistoryResponse {
                        value,
                        control_gradients,
                    },
                )
                .is_some()
            {
                return Err(fail("surface-gap history design response duplicated"));
            }
        }
        Ok(SurfaceGapHistory {
            responses,
            samples,
            sample_names: projection.names().to_vec(),
            final_state: run.final_state().to_vec(),
            ledger: run.ledger().to_vec(),
            record: run.record(),
            coupling_admission,
            problem_identity: self.problem.identity.clone(),
        })
    }
}
