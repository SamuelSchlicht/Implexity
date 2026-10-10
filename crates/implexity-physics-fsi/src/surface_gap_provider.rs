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

use crate::surface_gap_problem::{SurfaceGapHistoryDocument, SurfaceGapHistoryProblem};
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
pub const NAME: &str = "moving_fsi_surface_gap_history";
const COORDS: [&str; 2] = ["body0:model:control", "body1:model:control"];
fn fail(s: impl std::fmt::Display) -> CaeError {
    CaeError::contract(s.to_string())
}
fn normalized(p: &ProviderProblem) -> CaeResult<&SurfaceGapHistoryProblem> {
    p.downcast_ref()
        .ok_or_else(|| fail("surface gap provider problem type"))
}
fn diagnostics() -> Map<String, Value> {
    json!({"physical_qualification":false,"whole_history_gradient_runtime_verified":false,"interface_scope":"paired moving wedge covers with conservative bulk ports","gap_phase":"gas","liquid_film_history_attached":false,"coupling_modes":["strong_iterative","monolithic"],"derivative_scope":"fixed native time grid and local differentiable surface feature strata","initial_state_owner":"native solid, fluid and lagged-trace state","self_intersection_certificate_available":false,"ownership_certificate":"sharp native clipped tetrahedra at evaluated configurations","gap_sample_names":crate::surface_gap_field::sample_names(),"native_fluid_samples_supported":false,"checkpoint_owner":"generic checkpointed history"}).as_object().unwrap().clone()
}
#[derive(Clone)]
struct Cached {
    key: String,
    evaluation: Evaluation,
    gradients: Option<BTreeMap<String, NamedArrays>>,
}
#[derive(Default)]
pub struct SurfaceGapHistoryProvider {
    cache: Mutex<Option<Cached>>,
}
impl SurfaceGapHistoryProvider {
    fn controls(p: &SurfaceGapHistoryProblem, d: &NamedArrays) -> CaeResult<[Vec<f64>; 2]> {
        if d.names().len() != 2 || COORDS.iter().any(|n| !d.contains(n)) {
            return Err(fail(
                "surface gap provider requires both native named controls",
            ));
        }
        let mut values = vec![];
        for i in 0..2 {
            let a = d
                .get(COORDS[i])
                .ok_or_else(|| fail("surface gap coordinate absent"))?;
            if a.shape() != p.problem.model.native_body(i)?.problem.solid.grid.shape {
                return Err(fail("surface gap coordinate voxel shape"));
            }
            values.push(a.iter().copied().collect());
        }
        values
            .try_into()
            .map_err(|_| fail("surface gap body count"))
    }
    fn solve(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        point: usize,
        need_gradients: bool,
    ) -> CaeResult<Cached> {
        if point != 0 {
            return Err(fail("surface gap provider has one operating point"));
        }
        let p = normalized(p)?;
        let key = format!(
            "{}:{}",
            implexity_core::json::canonical_sha256(
                &serde_json::to_value(&p.document).map_err(fail)?
            ),
            design_identity(d)?
        );
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| fail("surface gap provider cache lock"))?;
        if let Some(c) = cache.as_ref() {
            if c.key == key && (!need_gradients || c.gradients.is_some()) {
                return Ok(c.clone());
            }
        }
        let controls = Self::controls(p, d)?;
        let run = p.execute(
            [&controls[0], &controls[1]],
            &StoreBudget::from_environment()?,
            need_gradients,
        )?;
        let mut responses = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for (name, r) in run.responses {
            responses.insert(name.clone(), r.value);
            if need_gradients {
                let g = r
                    .control_gradients
                    .ok_or_else(|| fail("surface gap response gradient absent"))?;
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
        fields.insert(
            "native:final_packed_state".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(IxDyn(&[run.final_state.len()]), run.final_state)
                    .map_err(fail)?,
            ),
        );
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
        let mut info = diagnostics();
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
        *cache = Some(out.clone());
        Ok(out)
    }
}
impl CaeProvider for SurfaceGapHistoryProvider {
    fn name(&self) -> &str {
        NAME
    }
    fn implementation(&self) -> &str {
        "implexity_physics_fsi::surface_gap_provider::SurfaceGapHistoryProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut d = ProviderDescriptor::new(NAME, vec![NAME.into()], vec![]);
        d.nonlinear = true;
        d.sensitivities = true;
        d.design_coordinates = COORDS.iter().map(|s| (*s).into()).collect();
        d.traits = diagnostics();
        Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Two-body gap flow and contact history","schema":editor_schema()}),"array")?)))
    }
    fn normalise_problem(&self, v: &Value) -> CaeResult<ProviderProblem> {
        Ok(Arc::new(
            SurfaceGapHistoryDocument::from_json(&serde_json::to_vec(v).map_err(fail)?)?.build()?,
        ))
    }
    fn preflight(
        &self,
        p: &ProviderProblem,
        _: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let _ = normalized(p)?;
        Ok(diagnostics())
    }
    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        Some((|| {
            use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge, PhysicsPort};
            use implexity_solve::multirate_coupling::CouplingMode;
            let mut notes=vec!["Gap pressure, viscous shear and contact loads are transferred to both native structural bodies.".into()];
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
            "surface gap provider requires named native body coordinates",
        ))
    }
    fn sensitivity(&self, _: &ProviderProblem, _: &ArrayD<f64>, _: &str) -> CaeResult<Sensitivity> {
        Err(fail(
            "surface gap provider requires named native body coordinates",
        ))
    }
    fn interface(&self, n: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(n)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl DesignOperations for SurfaceGapHistoryProvider {
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
    fn preflight_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let p = normalized(p)?;
        let controls = Self::controls(p, d)?;
        let rho = p.problem.physical_design([&controls[0], &controls[1]])?;
        let _ = p.problem.stepper(&rho)?;
        Ok(diagnostics())
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
            .ok_or_else(|| fail("surface gap response unknown"))?;
        let gradients = r
            .gradients
            .ok_or_else(|| fail("surface gap gradients absent"))?
            .remove(n)
            .ok_or_else(|| fail("surface gap response gradient unknown"))?;
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
            .ok_or_else(|| fail("surface gap gradients absent"))?;
        let mut responses = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in n {
            responses.insert(
                name.clone(),
                *r.evaluation
                    .responses
                    .get(name)
                    .ok_or_else(|| fail("surface gap response unknown"))?,
            );
            gradients.insert(
                name.clone(),
                all.get(name)
                    .ok_or_else(|| fail("surface gap response gradient unknown"))?
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
    p.downcast_ref::<SurfaceGapHistoryProblem>()
        .is_some_and(|p| p.document.geometry_outputs.is_some())
}
pub fn derived_geometry_refs(p: &ProviderProblem) -> CaeResult<Vec<String>> {
    normalized(p)?
        .document
        .geometry_outputs
        .as_ref()
        .map(|v| v.to_vec())
        .ok_or_else(|| fail("surface gap geometry outputs not configured"))
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
        .ok_or_else(|| fail("surface gap geometry outputs not configured"))?;
    let mut named = NamedArrays::new();
    for (name, a) in d {
        named.insert(name.clone(), a.clone());
    }
    let controls = SurfaceGapHistoryProvider::controls(p, &named)?;
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
    ctx.register_provider(Arc::new(SurfaceGapHistoryProvider::default()))
        .map(|_| ())
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}
fn bounded_number(minimum: f64, exclusive: bool) -> Value {
    if exclusive {
        json!({"type":"number","exclusiveMinimum":minimum})
    } else {
        json!({"type":"number","minimum":minimum})
    }
}
fn fixed_array(items: Value, count: usize) -> Value {
    json!({"type":"array","minItems":count,"maxItems":count,"items":items})
}
pub fn editor_schema() -> Value {
    let index = json!({"type":"integer","minimum":0});
    let positive = bounded_number(0., true);
    let nonnegative = bounded_number(0., false);
    let edge = object(
        json!({"nodes":fixed_array(index.clone(),2),"iso":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":1},"contrast_min":positive,"interior_margin":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":0.5}}),
        &["nodes", "iso", "contrast_min", "interior_margin"],
    );
    let tetra = object(
        json!({"nodes":fixed_array(index.clone(),4),"weights":fixed_array(json!({"type":"number"}),4),"weight_tolerance":{"type":"number","minimum":0,"maximum":32.*f64::EPSILON}}),
        &["nodes", "weights", "weight_tolerance"],
    );
    let feature = json!({"oneOf":[object(json!({"kind":{"const":"vertex"},"data":index}),&["kind","data"]),object(json!({"kind":{"const":"density_edge"},"data":edge}),&["kind","data"]),object(json!({"kind":{"const":"fixed_tetrahedron"},"data":tetra}),&["kind","data"])]});
    let cell = object(
        json!({"features":fixed_array(fixed_array(feature,3),2),"minimum_area_m2":positive,"minimum_wedge_jacobian_m3":positive,"maximum_tangent_offset_ratio":positive}),
        &[
            "features",
            "minimum_area_m2",
            "minimum_wedge_jacobian_m3",
            "maximum_tangent_offset_ratio",
        ],
    );
    let connection = object(
        json!({"left":index,"left_edge":{"type":"integer","minimum":0,"maximum":2},"right":{"oneOf":[fixed_array(index.clone(),2),{"type":"null"}]},"port":{"oneOf":[index.clone(),{"type":"null"}]}}),
        &["left", "left_edge", "right", "port"],
    );
    let ortho = object(
        json!({"maximum_skew_ratio":positive,"maximum_dihedral_angle_rad":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":std::f64::consts::FRAC_PI_2},"position_tolerance_m":positive}),
        &[
            "maximum_skew_ratio",
            "maximum_dihedral_angle_rad",
            "position_tolerance_m",
        ],
    );
    let contact = object(
        json!({"asperity_height_m":nonnegative,"penalty_pa_per_m":nonnegative,"damping_pa_s_per_m":nonnegative}),
        &[
            "asperity_height_m",
            "penalty_pa_per_m",
            "damping_pa_s_per_m",
        ],
    );
    let policy = object(
        json!({"normal_axis":{"type":"integer","minimum":0,"maximum":2},"envelope_cell_bounds":fixed_array(fixed_array(index.clone(),3),2),"geometry_tolerance_m":positive,"partition_absolute_tolerance_m3":positive,"partition_relative_tolerance":positive,"maximum_aspect_ratio":positive,"maximum_gap_slope":positive,"maximum_reduced_reynolds":positive,"maximum_unsteady_inertia":positive,"flow_timescale_s":positive,"absolute_gas_pressure_pa":positive,"maximum_gas_pressure_ratio":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":1}}),
        &[
            "normal_axis",
            "envelope_cell_bounds",
            "geometry_tolerance_m",
            "partition_absolute_tolerance_m3",
            "partition_relative_tolerance",
            "maximum_aspect_ratio",
            "maximum_gap_slope",
            "maximum_reduced_reynolds",
            "maximum_unsteady_inertia",
            "flow_timescale_s",
            "absolute_gas_pressure_pa",
            "maximum_gas_pressure_ratio",
        ],
    );
    let weighted_cell = json!({"type":"array","minItems":2,"maxItems":2,"prefixItems":[index,positive],"items":false});
    let gap = object(
        json!({"solid_iso":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":1},"cells":{"type":"array","minItems":1,"maxItems":255,"items":cell},"connections":{"type":"array","minItems":1,"items":connection},"excluded_cells":{"type":"array","minItems":1,"uniqueItems":true,"items":index},"ports":{"type":"array","minItems":1,"items":{"type":"array","minItems":1,"items":weighted_cell}},"removed_carrier_points":{"type":"array","uniqueItems":true,"items":index},"orthogonal_face_policy":ortho,"contact":contact,"policy":policy}),
        &[
            "solid_iso",
            "cells",
            "connections",
            "excluded_cells",
            "ports",
            "removed_carrier_points",
            "orthogonal_face_policy",
            "contact",
            "policy",
        ],
    );
    let mut body_schema = crate::editor::schema(None);
    body_schema["properties"]["observables"]["items"]["properties"]["kind"]["enum"] =
        json!(crate::problem::observables::SOLID_KINDS);
    body_schema["properties"]["observables"]["description"] =
        json!("Solid samples. Gap samples are selected in history.");
    let problem = object(
        json!({"schema":{"const":"implexity-surface-gap-fsi-pair/1"},"bodies":fixed_array(body_schema,2),"gap":gap}),
        &["schema", "bodies", "gap"],
    );
    let mut schema = object(
        json!({"schema":{"const":crate::surface_gap_problem::SCHEMA},"problem":problem,"geometry_outputs":fixed_array(json!({"type":"string","minLength":1}),2),"observables":{"type":"array"},"responses":{"type":"object"}}),
        &["schema", "problem", "observables", "responses"],
    );
    schema["x-qualification"] = json!(diagnostics());
    schema
}
