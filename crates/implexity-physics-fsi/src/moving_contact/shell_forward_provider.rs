// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::{
    boundary_law::{
        boundary_path::PathPolicy, plane_manifold::SupportingPlaneSelection,
        reference_embedding::AuthenticatedReferencePatch,
    },
    macro_control::macro_checkpoint::CheckpointMacroStepper,
    pair_problem::{PairEventDocument, PairPathDocument},
    phase_problem::PhaseDocument,
    sample_projection::from_observables,
    selected_transition::OnsetPolicy,
    separate_body::MovingFsiPairModel,
    shell_forward::prepare_authenticated_shell_forward,
};
use implexity_core::{
    CaeError, CaeResult,
    contracts::{
        CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
        Sensitivity,
    },
    packages::InstallContext,
};
use implexity_optim::{
    design::{NamedArrays, design_identity},
    provider_ops::{DesignOp, DesignOperations},
};
use implexity_solve::{
    dynamic_program::DynamicProgram,
    multirate_coupling::{FluxDrivenField, SubcycledField},
    time_stepper::TimeStepper,
};
use ndarray::{ArrayD, IxDyn};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    any::Any,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
pub const NAME: &str = "moving_fsi_authenticated_shell_forward";
pub const SCHEMA: &str = "implexity-authenticated-shell-fixed-horizon-forward/1";
const COORDS: [&str; 2] = ["body0:model:control", "body1:model:control"];
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn diagnostics() -> Map<String, Value> {
    json!({"physical_qualification":false,"contact_scope":"authenticated complete sampled-shell guard with declared incident VF families","full_curved_shell_feature_event_history_qualified":false,"unsupported_EE_perimeter_and_new_contact_events":"refuse","sensitivities_available":false,"ordinary_closed_gradients":false,"forward_does_not_require_design_adjoint":true,"measured_phonation_frequency_qualified":false,"state_owner":"complete native solid, fluid, lag and absolute clock; authored checkpoint policy"}).as_object().unwrap().clone()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellDocument {
    path: String,
    sha256: String,
    native_body_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaneDocument {
    body: usize,
    facet: usize,
    positive_body: usize,
    maximum_pairs: usize,
    path: PairPathDocument,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    source_graph_sha256: String,
    bodies: [Value; 2],
    shells: [ShellDocument; 2],
    feature_pairs: Vec<[usize; 2]>,
    gap_scale_m: f64,
    force_scale_n: f64,
    initial_plane: PlaneDocument,
    event: PairEventDocument,
    phase: PhaseDocument,
    external_force_n: Vec<f64>,
    observables: Value,
    responses: Value,
    macro_steps: usize,
    contact_scope: String,
}
struct Normalized {
    document: Document,
    model: MovingFsiPairModel,
    patches: [AuthenticatedReferencePatch; 2],
    program: DynamicProgram,
    identity: String,
}
fn parse(v: &Value) -> CaeResult<Normalized> {
    let d: Document = serde_json::from_value(v.clone()).map_err(|e| fail(&e.to_string()))?;
    if d.schema != SCHEMA
        || d.contact_scope != "authenticated_sampled_shell_incident_vf"
        || d.source_graph_sha256.len() != 64
        || !d.source_graph_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || d.feature_pairs.is_empty()
        || ![d.gap_scale_m, d.force_scale_n].iter().all(|v| v.is_finite() && *v > 0.)
        || d.external_force_n.iter().any(|v| !v.is_finite())
        || d.phase.clock != "native_tick_end"
        || d.phase.maximum_active_set_iterations == 0
    {
        return Err(fail("authenticated shell forward document domain"));
    }
    let _ = d.event.policy()?;
    let _ = super::phase_api::PhaseSchedule::new(
        d.phase.subdivisions,
        super::segment_quadrature::SegmentClock::NativeTickEnd,
    )?;
    let model = MovingFsiPairModel::from_bodies(
        crate::problem::normalise(&d.bodies[0])?,
        crate::problem::normalise(&d.bodies[1])?,
    )?;
    if !matches!(model.native_body(0)?.problem.coupling.mode, implexity_solve::multirate_coupling::CouplingMode::Loose { .. }) {
        return Err(fail("authenticated shell forward currently supports the authored canonical loose coupling only"));
    }
    for b in 0..2 {
        if model.native_body(b)?.problem.solid.supports.iter().any(|s| s.motion.is_some()) {
            return Err(fail("authenticated shell forward requires stationary prescribed supports"));
        }
    }
    if d.macro_steps == 0
        || d.macro_steps != model.native_body(0)?.problem.time.history_steps()
        || d.macro_steps != model.native_body(1)?.problem.time.history_steps()
    {
        return Err(fail("authenticated shell forward requires entire authored native horizon"));
    }
    let _ = model.fixed_horizon_autonomous()?;
    let p = &d.initial_plane.path;
    if p.minimum_signed_gap != 0.
        || !p.minimum_barycentric.is_finite()
        || p.minimum_barycentric < 0.
        || p.minimum_barycentric >= 1. / 3.
        || !p.minimum_area_ratio.is_finite()
        || p.minimum_area_ratio <= 0.
        || !p.time_resolution.is_finite()
        || p.time_resolution <= 0.
        || p.maximum_intervals == 0
        || p.maximum_depth == 0
        || p.maximum_depth > 60
        || d.initial_plane.body > 1
        || d.initial_plane.positive_body > 1
        || d.initial_plane.maximum_pairs == 0
    {
        return Err(fail("authenticated initial disjointness policy"));
    }
    let patches = [
        AuthenticatedReferencePatch::read(
            std::path::Path::new(&d.shells[0].path),
            &d.shells[0].sha256,
            &d.shells[0].native_body_sha256,
        )?,
        AuthenticatedReferencePatch::read(
            std::path::Path::new(&d.shells[1].path),
            &d.shells[1].sha256,
            &d.shells[1].native_body_sha256,
        )?,
    ];
    for b in 0..2 {
        if patches[b].patch().body() != b {
            return Err(fail("authenticated forward shell body ordering"));
        }
    }
    let program = implexity_solve::dynamic_program::normalise(&d.responses)?;
    let mut names = Vec::new();
    for b in 0..2 {
        names.extend(
            model.native_body(b)?.solid_sample_name_metadata()?.into_iter().map(|n| format!("body{b}:{n}")),
        );
    }
    let common = &model.native_body(0)?.problem.observables;
    names.extend(common.fluid.iter().map(|(n, _)| n.clone()));
    names.extend(common.interface.iter().cloned());
    let projection =
        from_observables(&d.observables, [&d.bodies[0]["observables"], &d.bodies[1]["observables"]], names)?;
    program
        .bind(
            projection.names(),
            &["design_volume_fraction", "removed_volume_fraction", "removed_volume_m3", "removal_depth_m"],
        )?
        .admit(d.macro_steps, false, model.fixed_horizon_autonomous()?)?;
    Ok(Normalized {
        document: d,
        model,
        patches,
        program,
        identity: implexity_core::json::canonical_sha256(v),
    })
}
fn normalized(p: &ProviderProblem) -> CaeResult<&Normalized> {
    p.downcast_ref::<Normalized>().ok_or_else(|| fail("authenticated shell forward normalized problem type"))
}
#[derive(Default)]
pub struct AuthenticatedShellForwardProvider {
    cache: Mutex<Option<(String, Evaluation)>>,
}
impl AuthenticatedShellForwardProvider {
    fn evaluate_named(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        operating_point: usize,
    ) -> CaeResult<Evaluation> {
        if operating_point != 0 {
            return Err(fail("authenticated shell forward has one operating point"));
        }
        let n = normalized(p)?;
        let key = format!("{}:{}", n.identity, design_identity(d)?);
        let mut cache = self.cache.lock().map_err(|_| fail("authenticated shell forward cache lock"))?;
        if let Some((_, v)) = cache.as_ref().filter(|(k, _)| k == &key) {
            return Ok(v.clone());
        }
        if d.names().len() != 2 || COORDS.iter().any(|c| !d.contains(c)) {
            return Err(fail("authenticated shell forward requires both native named controls"));
        }
        let mut controls = vec![];
        for b in 0..2 {
            let x = d.get(COORDS[b]).ok_or_else(|| fail("native body coordinate absent"))?;
            if x.shape() != n.model.native_body(b)?.problem.solid.grid.shape {
                return Err(fail("authenticated shell control native voxel shape"));
            }
            controls.push(x.iter().copied().collect::<Vec<_>>());
        }
        let doc = &n.document;
        let p = &doc.initial_plane.path;
        let plane = SupportingPlaneSelection {
            body: doc.initial_plane.body,
            facet: doc.initial_plane.facet,
            positive_body: doc.initial_plane.positive_body,
            maximum_pairs: doc.initial_plane.maximum_pairs,
            path_policy: PathPolicy {
                minimum_barycentric: p.minimum_barycentric,
                minimum_area_ratio: p.minimum_area_ratio,
                minimum_signed_gap: p.minimum_signed_gap,
                time_resolution: p.time_resolution,
                maximum_intervals: p.maximum_intervals,
                maximum_depth: p.maximum_depth,
            },
        };
        let event = doc.event.policy()?;
        let prepared = prepare_authenticated_shell_forward(
            &n.model,
            [&controls[0], &controls[1]],
            event,
            [&n.patches[0], &n.patches[1]],
            &doc.feature_pairs,
            doc.gap_scale_m,
            doc.force_scale_n,
            plane,
        )?;
        let ratio = n.model.native_body(0)?.problem.time.macro_step_s()
            / prepared.fields.fluid.inner().nominal_fluid_step_s();
        if !ratio.is_finite()
            || ratio < 1.
            || ratio.round() > usize::MAX as f64
            || (ratio - ratio.round()).abs() > 32. * f64::EPSILON * ratio
        {
            return Err(fail("authenticated shell native fluid cadence"));
        }
        let policy = OnsetPolicy {
            residual_tolerance: event.residual_tolerance,
            maximum_newton_iterations: event.maximum_newton_iterations,
            maximum_active_set_iterations: doc.phase.maximum_active_set_iterations,
            gap_target_fraction: event.maintained_gap_target_fraction,
            normal_velocity_tolerance_m_s: event.impact.normal_velocity_m_s,
        };
        let stepper = CheckpointMacroStepper::from_rest_canonical_loose(
            &n.model,
            &prepared.fields,
            &prepared.physical_design,
            &doc.external_force_n,
            &prepared.feature_identity,
            &doc.source_graph_sha256,
            prepared.physical_velocity,
            policy,
            doc.phase.subdivisions,
            ratio.round() as usize,
        )?;
        let schur_admission = stepper.admit_canonical_native_interface()?;
        let projection = from_observables(
            &doc.observables,
            [&doc.bodies[0]["observables"], &doc.bodies[1]["observables"]],
            stepper.sample_names().to_vec(),
        )?;
        let run = stepper.run()?;
        let responses = stepper.evaluate_forward_program(&run, &projection, &n.program)?;
        let source = run.samples();
        let mut fields = BTreeMap::new();
        let mut columns = vec![Vec::with_capacity(source.nrows); projection.names().len()];
        for r in 0..source.nrows {
            let values = projection.values(&source.data[r * source.ncols..(r + 1) * source.ncols])?;
            for (c, v) in columns.iter_mut().zip(values) {
                c.push(v);
            }
        }
        for (name, values) in projection.names().iter().zip(columns) {
            fields.insert(
                format!("history:{name}"),
                FieldValue::Array(
                    ArrayD::from_shape_vec(IxDyn(&[source.nrows]), values)
                        .map_err(|e| fail(&e.to_string()))?,
                ),
            );
        }
        let metadata = stepper.checkpoint_metadata()?;
        let entries =
            metadata["entries"].as_array().ok_or_else(|| fail("forward history missing clock metadata"))?;
        let mut times = vec![];
        for i in 1..=source.nrows {
            let entry = entries
                .iter()
                .find(|e| e["global_macro"].as_u64() == Some(i as u64))
                .ok_or_else(|| fail("forward history missing global clock"))?;
            let bits = entry["branch"]["owner_descriptor"]["absolute_clock_bits"]
                .as_str()
                .ok_or_else(|| fail("forward history clock bits"))?;
            let t = f64::from_bits(
                u64::from_str_radix(bits, 16).map_err(|_| fail("forward history clock encoding"))?,
            );
            if !t.is_finite() || times.last().is_some_and(|old| *old >= t) {
                return Err(fail("forward history absolute clock sequence"));
            }
            times.push(t);
        }
        if times.last().map(|t| t.to_bits()) != run.final_state().last().map(|t| t.to_bits()) {
            return Err(fail("forward history terminal clock/state mismatch"));
        }
        fields.insert(
            "native:sample_time_s".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(IxDyn(&[source.nrows]), times).map_err(|e| fail(&e.to_string()))?,
            ),
        );
        fields.insert(
            "native:final_packed_state".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(IxDyn(&[run.final_state().len()]), run.final_state().to_vec())
                    .map_err(|e| fail(&e.to_string()))?,
            ),
        );
        let mut info = diagnostics();
        info.insert("declared_source_graph_sha256".into(), json!(doc.source_graph_sha256));
        info.insert("normalized_document_identity".into(), json!(n.identity));
        info.insert("observable_definitions".into(), doc.observables.clone());
        info.insert("response_program".into(), doc.responses.clone());
        info.insert("initial_state_owner".into(), json!("native from-rest body/fluid and original loose predictor trace history"));
        info.insert("canonical_loose_schur_admission".into(), schur_admission);
        info.insert("coupling_derivative_admission".into(), json!("predictor-history and work-state reverse is not yet qualified; forward only"));
        info.insert("native_history".into(), run.record());
        info.insert("branch_admission_metadata".into(), metadata);
        info.insert("full_native_state_layout".into(),json!({"solid_scaled_entries":prepared.fields.contact.state_size(),"fluid_population_entries":prepared.fields.fluid.state_size(),"predictor_trace_history_entries":run.final_state().len()-prepared.fields.contact.state_size()-prepared.fields.fluid.state_size()-3,"cumulative_work_entries":2,"absolute_clock_entries":1,"native_body_state_scales":[prepared.fields.contact.native().body_field(0)?.core().scale(),prepared.fields.contact.native().body_field(1)?.core().scale()]}));
        info.insert(
            "native_ledger".into(),
            serde_json::to_value(run.ledger()).map_err(|e| fail(&e.to_string()))?,
        );
        let out = Evaluation { provider: NAME.into(), responses, diagnostics: info, fields };
        *cache = Some((key, out.clone()));
        Ok(out)
    }
}
fn forward_contract() -> CaeResult<implexity_core::orchestration::AddInContract> {
    use implexity_core::orchestration::{AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, RuntimeRoute};
    let mut c = AddInContract::new(NAME);
    c.category = AddInCategory::Field;
    c.scope = vec!["*".into()];
    c.fidelity = Fidelity::Intermediate;
    c.runtime_route = RuntimeRoute::Array;
    c.exact_design_derivatives = Some(false);
    c.exact_state_transpose = Some(false);
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = format!("provider:{NAME}");
    c.execution_kind = Some(ExecutionKind::Provider);
    c.supported_operations = ["preflight", "preflight_design", "evaluate", "report"].iter().map(|v| (*v).into()).collect();
    c.design_inputs = COORDS.iter().enumerate().map(|(i, coordinate)| {
        let mut input = DesignCoordinateRef::new(*coordinate, format!("{NAME}.design.{i}"));
        input.addin_id = NAME.into();
        input
    }).collect();
    c.checked()
}
impl CaeProvider for AuthenticatedShellForwardProvider {
    fn orchestration_contract(&self) -> Option<CaeResult<implexity_core::orchestration::PublishedContract>> {
        Some(forward_contract().map(|v| implexity_core::orchestration::PublishedContract::Contract(Box::new(v))))
    }
    fn name(&self) -> &str {
        NAME
    }
    fn implementation(&self) -> &str {
        "implexity_physics_fsi::moving_contact::shell_forward_provider::AuthenticatedShellForwardProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut d = ProviderDescriptor::new(NAME, vec![NAME.into()], vec![]);
        d.nonlinear = true;
        d.sensitivities = false;
        d.design_coordinates = COORDS.iter().map(|n| (*n).into()).collect();
        d.traits = diagnostics();
        Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Authenticated two-body shell forward window","schema":editor_schema()}),"array")?)))
    }
    fn normalise_problem(&self, v: &Value) -> CaeResult<ProviderProblem> {
        Ok(Arc::new(parse(v)?))
    }
    fn preflight(&self, p: &ProviderProblem, _: Option<&ArrayD<f64>>) -> CaeResult<Map<String, Value>> {
        let _ = normalized(p)?;
        Ok(diagnostics())
    }
    fn evaluate(&self, _: &ProviderProblem, _: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(fail("authenticated shell forward requires both named native controls"))
    }
    fn sensitivity(&self, _: &ProviderProblem, _: &ArrayD<f64>, _: &str) -> CaeResult<Sensitivity> {
        Err(fail("authenticated shell forward supplies no ordinary or directional design gradient"))
    }
    fn interface(&self, n: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(n)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl DesignOperations for AuthenticatedShellForwardProvider {
    fn provides(&self, o: DesignOp) -> bool {
        matches!(o, DesignOp::EvaluateDesign | DesignOp::PreflightDesign)
    }
    fn authoring_design_coordinates(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(normalized(p).map(|_| COORDS.iter().map(|s| (*s).into()).collect()))
    }
    fn preflight_design(&self, p: &ProviderProblem, d: &NamedArrays) -> CaeResult<Map<String, Value>> {
        let n = normalized(p)?;
        if d.names().len() != 2 || COORDS.iter().any(|c| !d.contains(c)) {
            return Err(fail("authenticated shell preflight requires both native named controls"));
        }
        let mut controls = Vec::new();
        for b in 0..2 {
            let x = d.get(COORDS[b]).ok_or_else(|| fail("native body coordinate absent"))?;
            if x.shape() != n.model.native_body(b)?.problem.solid.grid.shape || x.iter().any(|v| !v.is_finite()) {
                return Err(fail("authenticated shell preflight native control shape or finite domain"));
            }
            controls.push(x.iter().copied().collect::<Vec<_>>());
        }
        let physical = n.model.event_physical_design([&controls[0], &controls[1]])?;
        if physical.iter().any(|v| !v.is_finite()) {
            return Err(fail("authenticated shell preflight physical design domain"));
        }
        let mut out = diagnostics();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.insert("admission_scope".into(), json!("authored schema, authenticated shell inputs, named native control and physical design chain; full native path admission remains in forward evaluation"));
        Ok(out)
    }
    fn problem_responses(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(normalized(p).map(|n| n.program.responses()))
    }
    fn evaluate_design(&self, p: &ProviderProblem, d: &NamedArrays, o: usize) -> CaeResult<Evaluation> {
        self.evaluate_named(p, d, o)
    }
}
pub fn design_coordinate_shape(v: &Value, coordinate: &str) -> CaeResult<Option<Vec<usize>>> {
    let n = parse(v)?;
    COORDS
        .iter()
        .position(|c| *c == coordinate)
        .map(|i| n.model.native_body(i).map(|b| b.problem.solid.grid.shape.to_vec()))
        .transpose()
}
pub fn normalized_design_coordinate_shape(
    p: &ProviderProblem,
    coordinate: &str,
) -> CaeResult<Option<Vec<usize>>> {
    let n = normalized(p)?;
    COORDS
        .iter()
        .position(|c| *c == coordinate)
        .map(|i| n.model.native_body(i).map(|b| b.problem.solid.grid.shape.to_vec()))
        .transpose()
}
pub fn install(ctx: &InstallContext<'_>) -> CaeResult<()> {
    ctx.register_provider(Arc::new(AuthenticatedShellForwardProvider::default())).map(|_| ())
}
pub fn editor_schema() -> Value {
    let path = json!({"type":"object","additionalProperties":false,"required":["minimum_barycentric","minimum_area_ratio","minimum_signed_gap","time_resolution","maximum_intervals","maximum_depth"],"properties":{"minimum_barycentric":{"type":"number","minimum":0,"exclusiveMaximum":1./3.},"minimum_area_ratio":{"type":"number","exclusiveMinimum":0},"minimum_signed_gap":{"type":"number","enum":[0.],"unit":"m"},"time_resolution":{"type":"number","exclusiveMinimum":0},"maximum_intervals":{"type":"integer","minimum":1},"maximum_depth":{"type":"integer","minimum":1,"maximum":60}}});
    let impact = json!({"type":"object","additionalProperties":false,"required":["event_gap_m","normal_velocity_m_s","impulse_n_s","momentum_n_s","energy_j"],"properties":{"event_gap_m":{"type":"number","exclusiveMinimum":0,"unit":"m"},"normal_velocity_m_s":{"type":"number","exclusiveMinimum":0,"unit":"m/s"},"impulse_n_s":{"type":"number","exclusiveMinimum":0,"unit":"N s"},"momentum_n_s":{"type":"number","exclusiveMinimum":0,"unit":"N s"},"energy_j":{"type":"number","exclusiveMinimum":0,"unit":"J"}}});
    let event = json!({"type":"object","additionalProperties":false,"required":["restitution","maintained_gap_target_fraction","residual_tolerance","maximum_newton_iterations","maximum_localization_iterations","interpolation_kernel","impact"],"properties":{"restitution":{"type":"number","minimum":0,"maximum":1},"maintained_gap_target_fraction":{"type":"number","minimum":0,"maximum":0.5},"residual_tolerance":{"type":"number","exclusiveMinimum":0},"maximum_newton_iterations":{"type":"integer","minimum":1},"maximum_localization_iterations":{"type":"integer","minimum":1},"interpolation_kernel":{"type":"string","enum":["cubic","peskin4"]},"impact":impact}});
    let plane = json!({"type":"object","additionalProperties":false,"required":["body","facet","positive_body","maximum_pairs","path"],"properties":{"body":{"type":"integer","minimum":0,"maximum":1},"facet":{"type":"integer","minimum":0},"positive_body":{"type":"integer","minimum":0,"maximum":1},"maximum_pairs":{"type":"integer","minimum":1},"path":path},"x-role":"authenticated initial disjointness proof; not a substitute force plane"});
    let phase = json!({"type":"object","additionalProperties":false,"required":["subdivisions","clock","maximum_active_set_iterations"],"properties":{"subdivisions":{"type":"integer","enum":[1,2,4,8,16]},"clock":{"type":"string","enum":["native_tick_end"]},"maximum_active_set_iterations":{"type":"integer","minimum":1}}});
    json!({"type":"object","additionalProperties":false,"required":["schema","source_graph_sha256","bodies","shells","feature_pairs","gap_scale_m","force_scale_n","initial_plane","event","phase","external_force_n","observables","responses","macro_steps","contact_scope"],"properties":{"schema":{"type":"string","enum":[SCHEMA]},"source_graph_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"},"bodies":{"type":"array","minItems":2,"maxItems":2,"items":crate::editor::schema(None)},"shells":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"object","additionalProperties":false,"required":["path","sha256","native_body_sha256"],"properties":{"path":{"type":"string"},"sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"},"native_body_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"}}}},"feature_pairs":{"type":"array","minItems":1,"items":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"integer","minimum":0}},"x-role":"explicit authenticated physical incident-VF families; unresolved shell pairs are not automatically force rows"},"gap_scale_m":{"type":"number","exclusiveMinimum":0,"unit":"m"},"force_scale_n":{"type":"number","exclusiveMinimum":0,"unit":"N"},"initial_plane":plane,"event":event,"phase":phase,"external_force_n":{"type":"array","items":{"type":"number","unit":"N"}},"observables":{"type":"array"},"responses":{"type":"object"},"macro_steps":{"type":"integer","minimum":1},"contact_scope":{"type":"string","enum":["authenticated_sampled_shell_incident_vf"]}},"x-qualification":diagnostics()})
}
