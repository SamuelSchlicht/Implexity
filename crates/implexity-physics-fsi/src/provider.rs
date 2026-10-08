// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, MatchingTimeNewtonGuess, ProviderCapabilities, ProviderDescriptor,
    ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge, PhysicsPort};
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PortSpec, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::{
    AdmissionReply, CandidateDesign, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity,
    LifecycleDeclaration,
};
use implexity_runtime::dynamic_frames::availability::{FrameProducer, PRODUCER_INTERFACE};
use implexity_solve::periodic::{PeriodicGuess, regime_record};
use implexity_solve::state_store::StoreBudget;
use implexity_solve::time_stepper::TimeStepper;

use crate::model::FsiModel;
use crate::problem::design::COORDINATE;
use crate::problem::{FsiProblem, TimeKind, normalise};
use crate::run::{DynamicResult, RunSettings, STABILITY_RESPONSES, evaluate, gradients};

pub const NAME: &str = "lattice_boltzmann_fsi_dynamic";
pub const IMPLEMENTATION: &str = "implexity_physics_fsi::provider::FsiDynamicProvider";

pub static FRAME_PRODUCER: FrameProducer = FrameProducer {
    description: "fluid and solid fields, deformed bodies and coupling series of each dynamic evaluation",
};

pub const NOTES: [&str; 5] = [
    "Two-way fluid-structure interaction: moving-occupancy lattice Boltzmann flow (partially saturated cells or Brinkman penalization, TRT/BGK/MRT/regularized/cumulant, laminar or LES closures) coupled to a large-deformation soft solid (soft-matter laws, mixed u-p, AVF midpoint or generalized-alpha) with fluid substeps per solid macro step.",
    "Density topology optimization of the solid on its reference voxels (model:control), pushed forward to the lattice through the current deformation; exact gradients of time-weighted, windowed and cycle-averaged responses (discrete_history_exact, periodic_orbit_exact).",
    "Coupling strength loose (added-mass preflight), IQN-ILS or Newton-Krylov (monolithic in Schur form); periodic states by Picard, Newton-Krylov or Newton-Picard shooting with Floquet stability gate; chaotic regimes refused (nonperiodic_regime).",
    "Diffuse interface of the push-forward kernel width; angular momentum conserved to O(l^2); the fluid inside the body moves with it (inertia compensation optional); no self-contact.",
    "Screening physics: results are not validated or qualified (physical_qualification false); coupling contract in product/FLUID_STRUCTURE_DYNAMICS.md.",
];

pub const CONTRACT_RESPONSES: [(&str, &str); 10] = [
    ("tip_amplitude_m", "m"),
    ("mean_drag_N", "N"),
    ("mean_force_x_N", "N"),
    ("frequency_hz", "Hz"),
    ("volume_fraction", "1"),
    ("uy_A_mean_m", "m"),
    ("uy_A_rms_m", "m"),
    ("uy_A_crossing_period_s", "s"),
    ("growth_rate_per_s", "1/s"),
    ("angular_frequency_rad_s", "rad/s"),
];

pub const OPTION_PORTS: [(&str, &str); 12] = [
    ("fsi_velocity_set", "fluid"),
    ("fsi_collision_model", "fluid"),
    ("fsi_turbulence_closure", "fluid"),
    ("fsi_boundary_representation", "fluid"),
    ("fsi_solid_law", "solid"),
    ("fsi_solid_formulation", "solid"),
    ("fsi_time_integrator", "solid"),
    ("fsi_solid_damping", "solid"),
    ("fsi_solid_contact", "solid"),
    ("fsi_coupling_strength", "coupling"),
    ("fsi_periodic_state_method", "time"),
    ("fsi_objective_averaging", "time"),
];

#[must_use]
pub fn option_port(quantity: &str, domain: &str) -> PortSpec {
    let mut p = PortSpec::new(quantity);
    p.unit = "-".into();
    p.domain = domain.into();
    p.temporal = "steady".into();
    p.port_id = format!("{NAME}.{quantity}");
    p.cardinality = "singleton".into();
    p
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn arr(values: Vec<f64>, shape: &[usize]) -> CaeResult<ArrayD<f64>> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map_err(|e| CaeError::contract(format!("internal array shape: {e}")))
}

#[derive(Default)]
pub struct FsiDynamicProvider {
    seeds: Mutex<BTreeMap<String, PeriodicGuess>>,
}

impl std::fmt::Debug for FsiDynamicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsiDynamicProvider").finish_non_exhaustive()
    }
}

fn problem_value(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("lattice_boltzmann_fsi_dynamic requires its own problem document"))
}

fn compile(problem: &ProviderProblem, operating_point: usize) -> CaeResult<FsiModel> {
    let p = normalise(problem_value(problem)?)?;
    FsiModel::new(p.at_operating_point(operating_point)?)
}

fn layered(model: &FsiModel, len: usize) -> bool {
    let shape = model.problem.solid.grid.shape;
    shape[2] == 1 && len == 2 * model.chain.len()
}

fn coordinate(model: &FsiModel, design: &NamedArrays) -> CaeResult<Vec<f64>> {
    if design.names().len() != 1 || !design.contains(COORDINATE) {
        return Err(CaeError::contract(format!(
            "{NAME} requires exactly the design coordinate {COORDINATE}"
        )));
    }
    let a = design
        .get(COORDINATE)
        .ok_or_else(|| CaeError::contract(format!("missing coordinate {COORDINATE}")))?;
    let raw: Vec<f64> = a.iter().copied().collect();
    let x = if layered(model, raw.len()) {
        raw.chunks_exact(2).map(|pair| 0.5 * (pair[0] + pair[1])).collect()
    } else {
        raw
    };
    model.chain.forward(&x)?;
    Ok(x)
}

fn layer_pullback(model: &FsiModel, design_shape: &[usize], gx: Vec<f64>) -> CaeResult<ArrayD<f64>> {
    let len: usize = design_shape.iter().product();
    if layered(model, len) {
        let halves: Vec<f64> = gx.iter().flat_map(|g| [0.5 * g, 0.5 * g]).collect();
        arr(halves, design_shape)
    } else {
        arr(gx, design_shape)
    }
}


pub fn design_coordinate_shape(problem: &Value) -> CaeResult<Vec<usize>> {
    let shape = normalise(problem)?.solid.grid.shape;
    Ok(if shape[2] == 1 { vec![shape[0], shape[1], 2] } else { shape.to_vec() })
}

#[must_use]
pub fn required_topology_semantics() -> Value {
    json!({"coordinate": COORDINATE, "inside": "greater", "isovalue": 0.5})
}

fn blocking_saturation_record(model: &FsiModel) -> CaeResult<Value> {
    let fluid = model.fluid_field()?;
    let trace = vec![0.0; model.points.trace_size()];
    let full = vec![1.0; model.chain.len()];
    let r = fluid.inner().blocking_saturation(&trace, &full)?;
    Ok(json!({"admitted": r.admitted(), "max_fill": r.max_fill, "required_fill": r.required_fill,
        "saturated_cells": r.saturated_cells, "active_cells": r.active_cells,
        "max_blocking": r.max_blocking, "tau_plus": r.tau_plus,
        "warning": if r.admitted() { Value::Null } else { json!(format!(
            "the fully dense solid never blocks the fluid fully (largest fill {:.4} < {:.4}): raise coupling.pushforward.blocking_scale", r.max_fill, r.required_fill)) }}))
}

fn support_record(model: &FsiModel) -> Value {
    let fixed = &model.soft.fixed;
    let points = &model.soft.mesh.points;
    let n = points.len();

    let held = |i: usize| (0..3).all(|c| fixed[3 * i + c] || (c == 2 && model.problem.solid.plane_strain));
    let fixed_nodes = (0..n).filter(|&i| held(i)).count();
    let loose: Vec<usize> = (0..n)
        .filter(|&i| crate::problem::fluid::wall_at(&model.problem.fluid, points[i]) && !held(i))
        .collect();
    let warning = if loose.is_empty() {
        Value::Null
    } else {
        json!(format!(
            "{} reference nodes lie inside fixed lattice wall cells but are not fully supported (e.g. node {} at {:?}): the solid is drawn into the wall there but free to move; extend solid.supports over every attachment voxel",
            loose.len(),
            loose[0],
            points[loose[0]]
        ))
    };
    json!({"supports": model.problem.solid.supports.len(), "fixed_nodes": fixed_nodes, "nodes": n,
        "unsupported_wall_nodes": loose.len(), "warning": warning})
}

fn seed_key(p: &FsiProblem, operating_point: usize) -> String {
    format!("{}#{operating_point}", p.identity())
}

fn option_summary(p: &FsiProblem) -> Value {
    let n = p.normal_form();
    json!({
        "lattice": n["fluid"]["lattice"], "collision": n["fluid"]["collision"], "turbulence": n["fluid"]["turbulence"],
        "coupling_law": n["fluid"]["coupling_law"], "solid_law": n["solid"]["material"]["law"],
        "formulation": n["solid"]["formulation"], "integrator": n["solid"]["integrator"],
        "coupling_mode": n["coupling"]["mode"], "substeps": n["coupling"]["substeps"],
        "time": n["time"]["kind"], "method": n["time"].get("method").cloned().unwrap_or(Value::Null),
        "checkpoint": n["time"]["checkpoint"]
    })
}

#[must_use]
pub fn cost_estimate(p: &FsiProblem, model: &FsiModel) -> Value {
    let q = match p.fluid.lattice {
        implexity_physics_lbm::moving::field::LatticeKind::D2Q9 => 9,
        implexity_physics_lbm::moving::field::LatticeKind::D3Q19 => 19,
        implexity_physics_lbm::moving::field::LatticeKind::D3Q27 => 27,
    };
    let cells: usize = p.fluid.shape.iter().product();
    let steps = p.time.history_steps();
    let updates = cells * p.coupling.substeps * steps;
    let fluid_bytes = 8 * q * cells;
    let solid_dofs = 3 * model.soft.n();
    let stored = match &p.time.checkpoint {
        implexity_solve::checkpointed_history::CheckpointPolicy::All => steps + 1,
        implexity_solve::checkpointed_history::CheckpointPolicy::Binomial { ram_snapshots, .. }
        | implexity_solve::checkpointed_history::CheckpointPolicy::Online { ram_snapshots, .. } => {
            *ram_snapshots
        }
    };
    json!({"lattice_cells": cells, "populations": q, "fluid_state_bytes": fluid_bytes,
        "solid_displacement_dofs": solid_dofs, "push_forward_points": model.points.len(),
        "macro_steps_per_history": steps, "fluid_substeps": p.coupling.substeps,
        "lattice_updates_per_history": updates,
        "stored_states": stored, "stored_state_bytes_estimate": stored * (fluid_bytes + 8 * 6 * solid_dofs),
        "note": "a gradient costs about 3-4 forward histories (recomputation + reverse sweeps); periodic shooting multiplies by the tangent periods of the Krylov iterations"})
}

impl FsiDynamicProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn settings(&self, p: &FsiProblem, operating_point: usize) -> CaeResult<RunSettings> {
        let guess = self.seeds.lock().ok().and_then(|m| m.get(&seed_key(p, operating_point)).cloned());
        Ok(RunSettings { budget: StoreBudget::from_environment()?, guess })
    }

    fn remember(&self, p: &FsiProblem, operating_point: usize, result: &DynamicResult) {
        if let (Some(g), Ok(mut m)) = (&result.guess, self.seeds.lock()) {
            m.insert(seed_key(p, operating_point), g.clone());
        }
    }

    fn records(model: &FsiModel, result: &DynamicResult) -> Map<String, Value> {
        let p = &model.problem;
        obj(json!({
            "notes": NOTES, "physical_qualification": false,
            "derivative_scope": result.derivative_scope, "regime": result.regime,
            "periodic_certificate": if p.time.periodic() { result.certificate.clone() } else { Value::Null },
            "certificate": result.certificate, "ledger": result.ledger,
            "period_s": result.period_s, "step_s": result.step_s,
            "admission": crate::regime::screening(p), "options": option_summary(p),
            "supports": support_record(model),
            "problem_identity": p.identity(), "label": p.label,
            "provenance": p.normal_form().get("provenance").cloned().unwrap_or(Value::Null)
        }))
    }


    pub fn evaluate_named(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        operating_point: usize,
    ) -> CaeResult<Evaluation> {
        let model = compile(problem, operating_point)?;
        let x = coordinate(&model, design)?;
        let rho = model.chain.forward(&x)?;
        let stepper = model.stepper()?;
        let settings = self.settings(&model.problem, operating_point)?;
        let result = evaluate(&model, &stepper, &rho, &settings).map_err(|e| typed(&e))?;
        self.remember(&model.problem, operating_point, &result);
        let (mut fields, frames) = crate::frames::capture(&model, &stepper, &rho, &result)?;
        let store = crate::frames::stream(&model, &stepper, &rho, &result, Some(design))?;
        for (k, name) in result.sample_names.iter().enumerate() {
            let series: Vec<f64> = (0..result.samples.nrows).map(|r| result.samples.get(r, k)).collect();
            let n = series.len();
            fields.insert(format!("cycle_{name}"), FieldValue::Array(arr(series, &[n])?));
        }
        let g = model.problem.solid.grid.shape;
        fields.insert("density_physical".into(), FieldValue::Array(arr(rho, &g)?));
        let mut diagnostics = Self::records(&model, &result);
        if let Some(d) = frames.get("declaration") {
            diagnostics.insert(implexity_runtime::dynamic_frames::import::DIAGNOSTICS_KEY.into(), d.clone());
        }
        diagnostics.insert("frames".into(), frames);
        diagnostics.insert("dynamic_frames_store".into(), store.unwrap_or(Value::Null));
        Ok(Evaluation { provider: NAME.into(), responses: result.values, diagnostics, fields })
    }


    pub fn sensitivities_named(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        operating_point: usize,
    ) -> CaeResult<DesignSensitivities> {
        let mut unique = responses.to_vec();
        unique.sort();
        unique.dedup();
        if responses.is_empty() || unique.len() != responses.len() {
            return Err(CaeError::contract("unique response names required"));
        }
        let model = compile(problem, operating_point)?;
        let x = coordinate(&model, design)?;
        let design_shape: Vec<usize> = design.get(COORDINATE).map_or_else(Vec::new, |a| a.shape().to_vec());
        let rho = model.chain.forward(&x)?;
        let stepper = model.stepper()?;
        let settings = self.settings(&model.problem, operating_point)?;
        let (result, grad) =
            gradients(&model, &stepper, &rho, responses, &settings).map_err(|e| typed(&e))?;
        self.remember(&model.problem, operating_point, &result);
        let n = model.chain.len();
        let mut values = BTreeMap::new();
        let mut out = BTreeMap::new();
        for name in responses {
            let (v, g) = grad
                .values
                .get(name)
                .ok_or_else(|| CaeError::contract(format!("missing response {name}")))?;
            let gx = model.chain.pullback(&x, g)?;
            debug_assert_eq!(gx.len(), n);
            let mut named = NamedArrays::new();
            named.insert(COORDINATE, layer_pullback(&model, &design_shape, gx)?);
            values.insert(name.clone(), *v);
            out.insert(name.clone(), named);
        }
        let mut diagnostics = Self::records(&model, &result);
        diagnostics.insert("derivative_scope".into(), json!(grad.derivative_scope));
        diagnostics.insert("authoritative".into(), json!(grad.authoritative));
        diagnostics.insert("adjoint_certificate".into(), grad.certificate);
        Ok(DesignSensitivities { responses: values, gradients: out, diagnostics })
    }
}

fn typed(e: &CaeError) -> CaeError {
    match regime_record(e) {
        Some(record) => CaeError::convergence(format!(
            "{}; record {record}: no authoritative gradient in this regime (FSI_DYNAMIC_TOPOLOGY.md 4.8)",
            e.message()
        )),
        None => e.clone(),
    }
}

fn capabilities() -> CaeResult<ProviderCapabilities> {
    let mut d = ProviderDescriptor::new(
        NAME,
        vec![NAME.to_string()],
        CONTRACT_RESPONSES.iter().map(|(k, _)| (*k).to_string()).collect(),
    );
    d.nonlinear = true;
    d.sensitivities = true;
    d.design_coordinates = vec![COORDINATE.to_string()];
    d.notes = strings(&NOTES);
    d.fields = strings(&[
        "cycle_<sample>",
        "frame_<k>_speed",
        "frame_<k>_pressure",
        "frame_<k>_occupancy",
        "frame_<k>_solid_displacement",
        "frame_<k>_von_mises",
        "frame_times_s",
        "density_physical",
    ]);
    d.response_metadata = CONTRACT_RESPONSES
        .iter()
        .map(|(k, u)| {

            let differentiable = *k != STABILITY_RESPONSES[1];
            ((*k).to_string(), json!({"unit": u, "differentiable": differentiable, "design_reachable": true}))
        })
        .collect();
    d.traits = obj(json!({"experimental": true, "requires_explicit_design": true, "dynamic": true,
        "problem_schema": crate::problem::SCHEMA, "rust_extension": true}));
    let template = crate::templates::forced_flap();
    let p = normalise(&template)?;
    let n = p.solid.grid.voxel_count();
    let editor = json!({"kind": "native_json", "title": "Dynamic fluid-structure interaction (soft solid in LBM flow)",
        "problem_template": p.normal_form(),
        "design_template": {COORDINATE: {"value": p.design.initial_density, "lower": 0.0, "upper": 1.0,
                                         "designable": p.design.region}},
        "schema": crate::editor::schema(Some(&p))});
    debug_assert_eq!(n, p.design.region.len());
    Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(editor, "array")?)))
}

fn orchestration_contract() -> CaeResult<AddInContract> {
    let mut c = AddInContract::new(NAME);
    c.category = AddInCategory::Field;
    let port = format!("{NAME}.design.0");
    c.responses = CONTRACT_RESPONSES
        .iter()
        .map(|(r, u)| {
            let mut cap = ResponseCapability::new(*r);
            cap.unit = (*u).into();
            let differentiable = *r != STABILITY_RESPONSES[1];
            cap.differentiable = Some(differentiable);
            cap.design_reachable = Some(true);

            cap.depends_on = std::iter::once(port.clone())
                .chain(OPTION_PORTS.iter().map(|(q, d)| option_port(q, d).port_id))
                .collect();
            cap
        })
        .collect();
    c.consumes = OPTION_PORTS.iter().map(|(q, d)| option_port(q, d)).collect();
    c.scope = strings(&["fluid_structure_interaction", "lattice_boltzmann", "soft_matter", "dynamics", "*"]);
    c.fidelity = Fidelity::Intermediate;
    c.runtime_route = RuntimeRoute::Array;
    c.exact_design_derivatives = Some(true);
    c.exact_state_transpose = Some(true);
    c.notes = strings(&NOTES);
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = format!("provider:{NAME}");
    c.execution_kind = Some(ExecutionKind::Provider);
    c.supported_operations =
        strings(&["preflight", "preflight_design", "evaluate", "sensitivity", "sensitivities", "optimize"]);
    let mut dc = DesignCoordinateRef::new(COORDINATE, port);
    dc.addin_id = NAME.into();
    c.design_inputs = vec![dc];
    c.checked()
}

fn coupling_declaration(mode: &str) -> CaeResult<Value> {
    let edge_mode = if mode == "strong_newton_krylov" { "monolithic" } else { "iterative" };
    let err = |e: implexity_core::coupling_graph::CouplingContractError| CaeError::contract(e.0);
    let mut notes = strings(&NOTES);
    if mode == "loose" {
        notes.push(
            "loose coupling: coupling_iterations 1, coupling_residual_certified false (a closed but lagged loop; the interface work defect is recorded)"
                .into(),
        );
    }
    Ok(CouplingDeclaration {
        provider: NAME.into(),
        active_physics: strings(&["flow", "structure"]),
        ports: vec![
            PhysicsPort { name: "solid_occupancy".into(), owner: "flow".into(), direction: "input".into(), conserved: false, units: "m".into() },
            PhysicsPort { name: "fsi_force".into(), owner: "structure".into(), direction: "input".into(), conserved: true, units: "N".into() },
        ],
        edges: vec![
            CouplingEdge::new("flow", "structure", "pressure_and_shear_load", edge_mode, true, "the momentum exchanged in the partially saturated cells loads the solid (fsi_force, the transposed velocity map)").map_err(err)?,
            CouplingEdge::new("structure", "flow", "deformed_flow_domain", edge_mode, true, "the pushed-forward occupancy and velocity of the deforming solid drive the flow (solid_occupancy)").map_err(err)?,
        ],
        closed_loops: vec![strings(&["flow", "structure"])],
        intentionally_frozen: Vec::new(),
        notes,
    }
    .to_value())
}

impl CaeProvider for FsiDynamicProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        IMPLEMENTATION
    }

    fn capabilities(&self) -> Result<ProviderCapabilities, CaeError> {
        capabilities()
    }

    fn orchestration_contract(&self) -> Option<Result<PublishedContract, CaeError>> {
        Some(orchestration_contract().map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> Result<ProviderProblem, CaeError> {
        let p = normalise(problem)?;
        Ok(Arc::new(p.normal_form().clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> Result<Map<String, Value>, CaeError> {
        let p = normalise(problem_value(problem)?)?;
        let screening = crate::regime::screening(&p);
        Ok(obj(json!({"ok": screening["admissible"], "requires_complete_design": true,
            "design_coordinates": [COORDINATE], "admission": screening, "options": option_summary(&p),
            "physical_qualification": false})))
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> Result<Evaluation, CaeError> {
        Err(CaeError::contract(format!(
            "{NAME} evaluates named designs only (evaluate_design with {COORDINATE})"
        )))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> Result<Sensitivity, CaeError> {
        Err(CaeError::contract(format!(
            "{NAME} provides no legacy single-array sensitivity; use sensitivity_design"
        )))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<Result<Value, CaeError>> {
        let mode = problem
            .and_then(|p| p.downcast_ref::<Value>())
            .and_then(|v| {
                v.get("coupling").and_then(|c| c.get("mode")).and_then(Value::as_str).map(str::to_string)
            })
            .unwrap_or_else(|| "strong_newton_krylov".into());
        Some(coupling_declaration(&mode))
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        if name == PRODUCER_INTERFACE {
            return Some(&FRAME_PRODUCER);
        }
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for FsiDynamicProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::CandidateDesignAdmission
                | DesignOp::OptimizerLifecycle
                | DesignOp::InstallMatchingTimeGuess
                | DesignOp::ExportMatchingTimeGuess
        )
    }

    fn dispatches_operating_points(&self, op: DesignOp) -> bool {
        matches!(op, DesignOp::EvaluateDesign | DesignOp::SensitivityDesign | DesignOp::SensitivitiesDesign)
    }

    fn problem_responses(&self, problem: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {

        Some(problem_value(problem).and_then(normalise).map(|p| p.responses()))
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        operating_point: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(problem, design, operating_point)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let model = compile(problem, 0)?;
        let x = coordinate(&model, design)?;
        let rho = model.chain.forward(&x)?;
        let screening = crate::regime::screening(&model.problem);
        let schur = if matches!(
            model.problem.coupling.mode,
            implexity_solve::multirate_coupling::CouplingMode::Loose { .. }
        ) {
            let stepper = model.stepper()?;
            crate::run::schur_preflight(&model, &stepper, &rho)?
        } else {
            json!({"applies": false, "reason": "strong coupling has no added-mass limit"})
        };
        let points = model.problem.operating_points.len().max(1);
        let saturation = blocking_saturation_record(&model)?;
        let stability_kind = matches!(
            model.problem.time.kind,
            TimeKind::PeriodicForced | TimeKind::PeriodicAutonomous { .. } | TimeKind::SteadyStability { .. }
        );
        let integrator_warning = (stability_kind && crate::run::neutral_integrator(&model))
            .then_some(crate::run::NEUTRAL_INTEGRATOR_HINT);
        let supports = support_record(&model);
        Ok(obj(json!({"ok": screening["admissible"], "physical_qualification": false,
            "admission": screening, "schur_preflight": schur, "options": option_summary(&model.problem),
            "blocking_saturation": saturation, "supports": supports, "integrator_warning": integrator_warning,
            "cost": cost_estimate(&model.problem, &model), "operating_points": points,
            "responses": model.problem.responses(),
            "time_kind": model.problem.time.kind.name(),
            "gradients": if matches!(model.problem.time.kind, TimeKind::SteadyStability { .. }) { "growth_rate_per_s (stability_eigenvalue_exact; refused for options without second-order capability)" } else { "exact" }})))
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        operating_point: usize,
    ) -> CaeResult<DesignSensitivity> {
        let mut out = self.sensitivities_named(problem, design, &[response.to_string()], operating_point)?;
        Ok(DesignSensitivity {
            value: out.responses.get(response).copied().unwrap_or(f64::NAN),
            gradients: out.gradients.remove(response).unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }

    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        operating_point: usize,
    ) -> CaeResult<DesignSensitivities> {
        self.sensitivities_named(problem, design, responses, operating_point)
    }

    fn candidate_admission(
        &self,
        op: DesignOp,
        problem: &ProviderProblem,
        current: &CandidateDesign,
        trial: &CandidateDesign,
    ) -> CaeResult<AdmissionReply> {
        if op != DesignOp::CandidateDesignAdmission {
            return Err(CaeError::contract(format!("provider operation {:?} is unavailable", op.name())));
        }
        let named = |c: &CandidateDesign| match c {
            CandidateDesign::Named(n) => Ok(n.clone()),
            CandidateDesign::Array(_) => {
                Err(CaeError::contract("candidate named designs require nonempty text coordinate ids"))
            }
        };
        let (current, trial) = (named(current)?, named(trial)?);
        let evidence = (design_identity(&current)?, design_identity(&trial)?);
        let reply = match self.evaluate_named(problem, &trial, 0) {
            Err(e) => json!({"current_design_state_id": evidence.0, "candidate_design_state_id": evidence.1,
                "allow": false, "reason": e.message(),
                "diagnostics": {"dynamic_admitted": false, "regime_record": regime_record(&e)}}),
            Ok(ev) => json!({"current_design_state_id": evidence.0, "candidate_design_state_id": evidence.1,
                "allow": true, "reason": "coupled dynamic evaluation admitted",
                "diagnostics": {"dynamic_admitted": true, "regime": ev.diagnostics.get("regime").cloned().unwrap_or(Value::Null)}}),
        };
        Ok(AdmissionReply::Record(reply))
    }

    fn optimizer_lifecycle(&self, _problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            vec![COORDINATE.to_string()],
            "sensitivity_design",
            "evaluate_design",
            Some("candidate_design_admission"),
            None,
            false,
            false,
        )?))
    }

    fn install_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        _design: &NamedArrays,
        guess: &MatchingTimeNewtonGuess,
    ) -> CaeResult<Map<String, Value>> {
        let model = compile(problem, 0)?;
        let stepper = model.stepper()?;
        let seed = PeriodicGuess::from_newton_guess(guess, stepper.state_size())?;
        let key = seed_key(&model.problem, 0);
        if let Ok(mut m) = self.seeds.lock() {
            m.insert(key, seed);
        }
        Ok(obj(json!({"installed": true, "role": implexity_solve::periodic::PERIODIC_GUESS_ROLE,
            "note": "Newton seed only: the receiver re-enters the canonical periodic solve, gate and adjoint"})))
    }

    fn export_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        _design: &NamedArrays,
        _require_accepted: bool,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        let p = normalise(problem_value(problem)?)?;
        let seed = self
            .seeds
            .lock()
            .ok()
            .and_then(|m| m.get(&seed_key(&p, 0)).cloned())
            .ok_or_else(|| CaeError::contract("no accepted periodic orbit of this problem to export"))?;
        seed.to_newton_guess(
            obj(json!({"provider": NAME, "problem_identity": p.identity()})),
            obj(json!({"source": "accepted periodic orbit"})),
        )
    }

    fn workspace_declaration(&self, name: &str, problem: Option<&Value>) -> Option<CaeResult<Value>> {
        match name {
            "study_templates" => Some(Ok(Value::Array(crate::templates::study_templates(problem)))),
            "editor_schema" => Some(Ok(match problem.map(normalise) {
                Some(Ok(p)) => crate::editor::schema(Some(&p)),
                _ => crate::editor::schema(None),
            })),
            "discretization_control" => Some(Ok(json!({"controls": [
                {"path": ["fluid", "spacing_m"], "label": "Lattice spacing", "unit": "m", "effect": "push-forward kernel width and diffuse interface scale with it"},
                {"path": ["time", "steps_per_period"], "label": "Macro steps per period", "unit": "1"},
                {"path": ["coupling", "substeps"], "label": "Fluid substeps per macro step", "unit": "1", "effect": "tau+ and the lattice velocity"},
                {"path": ["coupling", "pushforward", "width_cells"], "label": "Push-forward kernel width", "unit": "cells"},
                {"path": ["coupling", "pushforward", "points_per_axis"], "label": "Push-forward points per tetrahedron axis", "unit": "1"}
            ]}))),
            _ => None,
        }
    }
}


pub fn install(ctx: &implexity_core::packages::InstallContext<'_>) -> CaeResult<()> {
    ctx.register_provider(Arc::new(FsiDynamicProvider::new()))?;
    Ok(())
}
