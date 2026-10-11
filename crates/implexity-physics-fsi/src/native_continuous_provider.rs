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

use crate::native_continuous_history::{
    NativeContinuousHistoryDocument, NativeContinuousHistoryProblem,
};
use implexity_core::{
    CaeError, CaeResult,
    contracts::{
        CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor,
        ProviderProblem, Sensitivity,
    },
    packages::InstallContext,
};
use implexity_optim::{
    design::{NamedArrays, design_identity},
    optimizer::OptimizerLifecycleConfig,
    provider_ops::{
        DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, LifecycleDeclaration,
    },
};
use implexity_solve::state_store::StoreBudget;
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};
use std::{
    any::Any,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
pub const NAME: &str = "moving_fsi_dry_gap_history";
const COORDS: [&str; 2] = ["body0:model:control", "body1:model:control"];
fn fail(s: impl std::fmt::Display) -> CaeError {
    CaeError::contract(s.to_string())
}
fn normalized(p: &ProviderProblem) -> CaeResult<&NativeContinuousHistoryProblem> {
    p.downcast_ref()
        .ok_or_else(|| fail("dry gap provider problem type"))
}
fn diagnostics() -> Map<String, Value> {
    json!({"contact_scope":"paired dry unilateral contact","derivative_scope":"fixed clock and fixed geometry strata","gas_scope":"continuous kinetic and stationary continuum transport","initial_state_owner":"native solid, bulk fluid, gas inventory and port stress","checkpoint_owner":"generic checkpointed history"}).as_object().unwrap().clone()
}
fn problem_diagnostics(p: &NativeContinuousHistoryProblem) -> CaeResult<Map<String, Value>> {
    let mut info = diagnostics();
    info.insert(
        "declared_reference_screening".into(),
        json!({
            "body0": crate::regime::screening(&p.problem.model.native_body(0)?.problem),
            "body1": crate::regime::screening(&p.problem.model.native_body(1)?.problem)
        }),
    );
    Ok(info)
}

#[derive(Clone)]
struct Cached {
    key: String,
    evaluation: Evaluation,
    gradients: Option<BTreeMap<String, NamedArrays>>,
}
#[derive(Default)]
pub struct NativeContinuousHistoryProvider {
    cache: Mutex<Option<Cached>>,
}
impl NativeContinuousHistoryProvider {
    fn controls(p: &NativeContinuousHistoryProblem, d: &NamedArrays) -> CaeResult<[Vec<f64>; 2]> {
        if d.names().len() != 2 || COORDS.iter().any(|n| !d.contains(n)) {
            return Err(fail("dry gap provider requires both native named controls"));
        }
        let mut values = vec![];
        for i in 0..2 {
            let a = d
                .get(COORDS[i])
                .ok_or_else(|| fail("dry gap coordinate absent"))?;
            if a.shape() != p.problem.model.native_body(i)?.problem.solid.grid.shape {
                return Err(fail("dry gap coordinate voxel shape"));
            }
            values.push(a.iter().copied().collect());
        }
        values.try_into().map_err(|_| fail("dry gap body count"))
    }
    fn solve(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        point: usize,
        need_gradients: bool,
    ) -> CaeResult<Cached> {
        if point != 0 {
            return Err(fail("dry gap provider has one operating point"));
        }
        let p = normalized(p)?;
        let capture = if p.document.state_checkpoints.is_some() {
            Some(
                implexity_runtime::dynamic_frames::capture::current_root()
                    .map_err(fail)?
                    .ok_or_else(|| {
                        fail("native history checkpoints require a job output directory")
                    })?,
            )
        } else {
            None
        };
        let capture_identity = if capture.is_some() {
            implexity_runtime::dynamic_frames::capture::identity()
        } else {
            None
        };
        let key = implexity_core::json::canonical_sha256(
            &json!({"document":p.document,"design":design_identity(d)?,"capture":capture.as_ref().map(|r|r.dir.to_string_lossy().into_owned()),"job":capture_identity}),
        );
        {
            let cache = self
                .cache
                .lock()
                .map_err(|_| fail("dry gap provider cache lock"))?;
            if let Some(c) = cache.as_ref() {
                if c.key == key && (!need_gradients || c.gradients.is_some()) {
                    return Ok(c.clone());
                }
            }
        }
        let controls = Self::controls(p, d)?;
        let budget = StoreBudget::from_environment()?;
        let mut observer = match (&p.document.state_checkpoints, capture.as_ref()) {
            (Some(c), Some(root)) => Some(
                implexity_solve::persistent_history::PersistentHistoryObserver::new(
                    &root.dir.join("native-history-states"),
                    key.clone(),
                    c.interval,
                    c.period,
                    c.disk_budget_bytes,
                )?,
            ),
            (None, None) => None,
            _ => return Err(fail("native history checkpoint output mismatch")),
        };
        let run = p.execute_observed(
            [&controls[0], &controls[1]],
            &budget,
            need_gradients,
            observer
                .as_mut()
                .map(|v| v as &mut dyn implexity_solve::checkpointed_history::HistoryObserver),
        )?;
        if let Some(observer) = observer.as_mut() {
            observer.finish(
                p.problem.model.native_body(0)?.problem.time.history_steps(),
                &run.final_state,
            )?;
        }
        let mut responses = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for (name, r) in run.responses {
            responses.insert(name.clone(), r.value);
            if need_gradients {
                let g = r
                    .control_gradients
                    .ok_or_else(|| fail("dry gap response gradient absent"))?;
                let mut arrays = NamedArrays::new();
                for (i, g) in g.into_iter().enumerate() {
                    arrays.insert(
                        COORDS[i],
                        ArrayD::from_shape_vec(
                            IxDyn(&p.problem.model.native_body(i)?.problem.solid.grid.shape),
                            g,
                        )
                        .map_err(fail)?,
                    );
                }
                gradients.insert(name, arrays);
            }
        }
        let mut fields = BTreeMap::new();
        for (col, name) in run.sample_names.iter().enumerate() {
            let values = (0..run.samples.nrows)
                .map(|r| run.samples.data[r * run.samples.ncols + col])
                .collect();
            fields.insert(
                format!("history:{name}"),
                FieldValue::Array(
                    ArrayD::from_shape_vec(IxDyn(&[run.samples.nrows]), values).map_err(fail)?,
                ),
            );
        }
        if p.document.include_final_state {
            fields.insert(
                "native:final_packed_state".into(),
                FieldValue::Array(
                    ArrayD::from_shape_vec(IxDyn(&[run.final_state.len()]), run.final_state)
                        .map_err(fail)?,
                ),
            );
        }
        let dt = p.problem.model.native_body(0)?.problem.time.macro_step_s();
        fields.insert(
            "native:sample_time_s".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(
                    IxDyn(&[run.samples.nrows]),
                    (1..=run.samples.nrows).map(|i| i as f64 * dt).collect(),
                )
                .map_err(fail)?,
            ),
        );
        let mut info = problem_diagnostics(p)?;
        info.insert(
            "transport_models".into(),
            json!({
                "continuum_wale":p.document.problem.transport.continuum.wale.is_some(),
                "resolved_kinetic_sgs":p.document.problem.transport.resolved_kinetic_sgs,
                "other_kinetic_cells":"molecular transport"
            }),
        );

        if let Some(observer) = observer.as_ref() {
            info.insert(
                "state_checkpoints".into(),
                json!({"directory":observer.directory(),"identity":key}),
            );
        }
        info.insert("native_history".into(), run.record);
        info.insert("coupling_admission".into(), run.coupling_admission);
        info.insert(
            "native_ledger".into(),
            serde_json::to_value(run.ledger).map_err(fail)?,
        );
        info.insert(
            "normalized_problem_identity".into(),
            json!(run.problem_identity),
        );
        let out = Cached {
            key,
            evaluation: Evaluation {
                provider: NAME.into(),
                responses,
                diagnostics: info,
                fields,
            },
            gradients: if need_gradients {
                Some(gradients)
            } else {
                None
            },
        };
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| fail("dry gap provider cache lock"))?;
        if let Some(c) = cache.as_ref() {
            if c.key == out.key && c.gradients.is_some() && out.gradients.is_none() {
                return Ok(c.clone());
            }
        }
        *cache = Some(out.clone());
        Ok(out)
    }
}
impl CaeProvider for NativeContinuousHistoryProvider {
    fn name(&self) -> &str {
        NAME
    }
    fn implementation(&self) -> &str {
        "implexity_physics_fsi::native_continuous_provider::NativeContinuousHistoryProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut d = ProviderDescriptor::new(NAME, vec![NAME.into()], vec![]);
        d.nonlinear = true;
        d.sensitivities = true;
        d.design_coordinates = COORDS.iter().map(|s| (*s).into()).collect();
        d.traits = diagnostics();
        Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Dry contact flow history","schema":editor_schema()}),"array")?)))
    }
    fn normalise_problem(&self, v: &Value) -> CaeResult<ProviderProblem> {
        Ok(Arc::new(
            NativeContinuousHistoryDocument::from_json(&serde_json::to_vec(v).map_err(fail)?)?
                .build()?,
        ))
    }
    fn preflight(
        &self,
        p: &ProviderProblem,
        _: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let mut info = problem_diagnostics(normalized(p)?)?;
        info.insert("ok".into(), Value::Bool(true));
        info.insert("input_document_validation".into(), Value::String("complete".into()));
        info.insert("physical_initialization".into(), Value::String("pending".into()));
        Ok(info)
    }
    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        Some((|| {
            use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge, PhysicsPort};
            use implexity_solve::multirate_coupling::CouplingMode;
            let mut notes = vec![
                "Both bodies and their contact forces are assembled in the structural field."
                    .into(),
            ];
            let mode = if let Some(problem) = problem {
                match &normalized(problem)?
                    .problem
                    .model
                    .native_body(0)?
                    .problem
                    .coupling
                    .mode
                {
                    CouplingMode::StrongNewtonKrylov { .. } => "monolithic",
                    CouplingMode::StrongQuasiNewton { .. } => "iterative",
                    CouplingMode::Loose { .. } => {
                        notes.push("Loose coupling uses lagged forces. The interface-work defect is recorded; a converged interface residual is not certified.".into());
                        "iterative"
                    }
                }
            } else {
                notes.push("The coupling mode is resolved from the normalized problem.".into());
                "iterative"
            };
            let edge = |a: &str, b: &str, q: &str, reason: &str| {
                CouplingEdge::new(a, b, q, mode, true, reason).map_err(|e| fail(e.0))
            };
            Ok(CouplingDeclaration{
    provider:NAME.into(),active_physics:vec!["flow".into(),"structure".into()],
    ports:vec![
     PhysicsPort{name:"interface_position".into(),owner:"flow".into(),direction:"input".into(),conserved:false,units:"m".into()},
     PhysicsPort{name:"interface_velocity".into(),owner:"flow".into(),direction:"input".into(),conserved:false,units:"m s-1".into()},
     PhysicsPort{name:"fsi_force".into(),owner:"structure".into(),direction:"input".into(),conserved:true,units:"N".into()},
    ],
    edges:vec![edge("flow","structure","pressure_and_shear_load","Fluid momentum exchange is transferred to both bodies through the native trace transpose.")?,edge("structure","flow","deformed_flow_domain","Both body positions and velocities determine moving occupancy and fluid momentum exchange.")?],
    closed_loops:vec![vec!["flow".into(),"structure".into()]],intentionally_frozen:vec![],notes,
   }.to_value())
        })())
    }

    fn evaluate(&self, _: &ProviderProblem, _: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(fail(
            "dry gap provider requires named native body coordinates",
        ))
    }
    fn sensitivity(&self, _: &ProviderProblem, _: &ArrayD<f64>, _: &str) -> CaeResult<Sensitivity> {
        Err(fail(
            "dry gap provider requires named native body coordinates",
        ))
    }
    fn interface(&self, n: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(n)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl DesignOperations for NativeContinuousHistoryProvider {
    fn problem_document(&self, p: &ProviderProblem) -> Option<CaeResult<Value>> {
        Some(normalized(p).and_then(|p| serde_json::to_value(&p.document).map_err(fail)))
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
        Some(normalized(p).map(|_| COORDS.iter().map(|n| (*n).into()).collect()))
    }
    fn problem_responses(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(normalized(p).and_then(|p| {
            Ok(implexity_solve::dynamic_program::normalise(&p.document.responses)?.responses())
        }))
    }
    fn prepare_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let p = normalized(p)?;
        let controls = Self::controls(p, d)?;
        let _ = p.problem.physical_design([&controls[0], &controls[1]])?;
        let mut info = problem_diagnostics(p)?;
        info.insert("ok".into(), Value::Bool(true));
        info.insert("input_validation".into(), Value::String("complete".into()));
        info.insert("physical_initialization".into(), Value::String("pending".into()));
        Ok(info)
    }
    fn preflight_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let p = normalized(p)?;
        let controls = Self::controls(p, d)?;
        let rho = p.problem.physical_design([&controls[0], &controls[1]])?;
        let _ = p.problem.stepper(&rho)?;
        let mut info = problem_diagnostics(p)?;
        info.insert("ok".into(), Value::Bool(true));
        info.insert("input_validation".into(), Value::String("complete".into()));
        info.insert("physical_initialization".into(), Value::String("complete".into()));
        Ok(info)
    }
    fn evaluate_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        o: usize,
    ) -> CaeResult<Evaluation> {
        Ok(self.solve(p, d, o, false)?.evaluation)
    }
    fn sensitivity_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        n: &str,
        o: usize,
    ) -> CaeResult<DesignSensitivity> {
        let r = self.solve(p, d, o, true)?;
        let value = *r
            .evaluation
            .responses
            .get(n)
            .ok_or_else(|| fail("dry gap response unknown"))?;
        let gradients = r
            .gradients
            .ok_or_else(|| fail("dry gap gradients absent"))?
            .remove(n)
            .ok_or_else(|| fail("dry gap response gradient unknown"))?;
        Ok(DesignSensitivity {
            value,
            gradients,
            diagnostics: r.evaluation.diagnostics,
        })
    }
    fn sensitivities_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        n: &[String],
        o: usize,
    ) -> CaeResult<DesignSensitivities> {
        let r = self.solve(p, d, o, true)?;
        let all = r
            .gradients
            .ok_or_else(|| fail("dry gap gradients absent"))?;
        let mut responses = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in n {
            responses.insert(
                name.clone(),
                *r.evaluation
                    .responses
                    .get(name)
                    .ok_or_else(|| fail("dry gap response unknown"))?,
            );
            gradients.insert(
                name.clone(),
                all.get(name)
                    .ok_or_else(|| fail("dry gap response gradient unknown"))?
                    .clone(),
            );
        }
        Ok(DesignSensitivities {
            responses,
            gradients,
            diagnostics: r.evaluation.diagnostics,
        })
    }
    fn optimizer_lifecycle(&self, _: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            COORDS.iter().map(|s| (*s).into()).collect(),
            "sensitivity_design",
            "evaluate_design",
            None,
            None,
            false,
            false,
        )?))
    }
}
pub fn normalized_design_coordinate_shape(
    p: &ProviderProblem,
    coordinate: &str,
) -> CaeResult<Option<Vec<usize>>> {
    let p = normalized(p)?;
    match COORDS.iter().position(|name| *name == coordinate) {
        Some(i) => Ok(Some(
            p.problem
                .model
                .native_body(i)?
                .problem
                .solid
                .grid
                .shape
                .to_vec(),
        )),
        None => Ok(None),
    }
}
pub fn derived_geometry_enabled(p: &ProviderProblem) -> bool {
    p.downcast_ref::<NativeContinuousHistoryProblem>()
        .is_some_and(|p| p.document.geometry_outputs.is_some())
}
pub fn derived_geometry_refs(p: &ProviderProblem) -> CaeResult<Vec<String>> {
    normalized(p)?
        .document
        .geometry_outputs
        .as_ref()
        .map(|v| v.to_vec())
        .ok_or_else(|| fail("dry gap geometry outputs not configured"))
}
pub fn derived_geometry_updates(
    p: &ProviderProblem,
    d: &BTreeMap<String, ArrayD<f64>>,
) -> CaeResult<BTreeMap<String, ArrayD<f64>>> {
    let p = normalized(p)?;
    let refs = p
        .document
        .geometry_outputs
        .as_ref()
        .ok_or_else(|| fail("dry gap geometry outputs not configured"))?;
    let mut named = NamedArrays::new();
    for (name, a) in d {
        named.insert(name.clone(), a.clone());
    }
    let controls = NativeContinuousHistoryProvider::controls(p, &named)?;
    let mut outputs = BTreeMap::new();
    for i in 0..2 {
        let body = p.problem.model.native_body(i)?;
        let rho = body.chain.forward(&controls[i])?;
        outputs.insert(
            refs[i].clone(),
            ArrayD::from_shape_vec(IxDyn(&body.problem.solid.grid.shape), rho).map_err(fail)?,
        );
    }
    Ok(outputs)
}
pub fn install(ctx: &InstallContext<'_>) -> CaeResult<()> {
    ctx.register_provider(Arc::new(NativeContinuousHistoryProvider::default()))
        .map(|_| ())
}
pub fn editor_schema() -> Value {
    crate::native_continuous_history::editor_schema()
}
