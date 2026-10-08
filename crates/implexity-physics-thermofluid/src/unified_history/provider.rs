// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, LegacySingleArrayProviderCapabilities, MatchingTimeNewtonGuess,
    ProviderCapabilities, ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, validate_declaration};
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::provider_ops::{
    CachedEvaluation, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, ProviderScope,
};
use implexity_solve::native_history::HistorySolution;

#[path = "provider_split.rs"]
mod split_adapter;

use super::kernel::{UnifiedKernel, coordinates, design_key, kernel};
use super::{
    COORDS, KINDS, LIMITATIONS, NAME, STAGED_COUPLING_IDS, STAGED_FLOW_TO_THERMAL, STAGED_THERMAL_TO_FLOW,
    installed_response_units, normalise, published_response_metadata, selected_limitations,
    unified_history_starter,
};
use crate::nodal_transport::{PROFILES as NODAL_PROFILES, SMOOTH_PROFILE};
use crate::unified_history_preconditioner::{
    DIRECT_POLICY, SPARSE_ILU_KRYLOV_POLICY, UnifiedHistoryExactProfile,
};

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn arr(values: Vec<f64>, shape: &[usize]) -> CaeResult<FieldValue> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map(FieldValue::Array)
        .map_err(|e| CaeError::contract(format!("internal array shape error: {e}")))
}

fn problem_value(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem.downcast_ref::<Value>().ok_or_else(|| {
        CaeError::contract("native_unified_history requires its own normalised problem mapping")
    })
}

thread_local! {
    static ACTIVE_EXACT_PROFILE: RefCell<Option<UnifiedHistoryExactProfile>> = const { RefCell::new(None) };
}

struct ProfileGuard;

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        ACTIVE_EXACT_PROFILE.with(|p| *p.borrow_mut() = None);
    }
}

struct StagedGuard(Arc<UnifiedKernel>);

impl Drop for StagedGuard {
    fn drop(&mut self) {
        self.0.lock().staged = None;
    }
}

#[must_use]
pub fn active_exact_profile() -> UnifiedHistoryExactProfile {
    ACTIVE_EXACT_PROFILE.with(|p| p.borrow().clone()).unwrap_or_else(UnifiedHistoryExactProfile::accelerated)
}


pub fn with_exact_profile<T>(profile: UnifiedHistoryExactProfile, f: impl FnOnce() -> T) -> CaeResult<T> {
    let advertised = NativeUnifiedHistoryProvider::exact_computation_effort_capability();
    if !advertised["profiles"].as_array().is_some_and(|rows| rows.contains(&profile.to_wire())) {
        return contract("native exact effort profile is not an advertised server profile");
    }
    if ACTIVE_EXACT_PROFILE.with(|p| p.borrow().is_some()) {
        return contract("nested native exact computation-effort scopes are forbidden");
    }
    ACTIVE_EXACT_PROFILE.with(|p| *p.borrow_mut() = Some(profile));
    let _guard = ProfileGuard;
    Ok(f())
}

struct EndpointOwner {
    problem: String,
    design: String,
    token: (u64, String),
    kernel: Weak<UnifiedKernel>,
    key: Vec<u64>,
    solution: Weak<HistorySolution>,
    responses: BTreeMap<String, f64>,
}

pub struct Parts {
    pub p: Value,
    pub k: Arc<UnifiedKernel>,
    pub x: Vec<f64>,
    pub control: Vec<f64>,
    pub spacing: Vec<f64>,
    pub material: Vec<f64>,
}

#[derive(Default)]
pub struct NativeUnifiedHistoryProvider {
    endpoint: Mutex<Option<EndpointOwner>>,
    approximate: Mutex<Vec<Arc<split_adapter::ApproximateRecord>>>,
    approximate_committed: Mutex<Option<String>>,
}

impl std::fmt::Debug for NativeUnifiedHistoryProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeUnifiedHistoryProvider")
    }
}

fn binding_value() -> Value {
    let token = implexity_core::registries::global().addins.binding_token();
    json!([token.generation, token.fingerprint])
}

fn registration(k: &UnifiedKernel, spacing: &[f64]) -> CaeResult<Value> {
    let bounds: [f64; 3] = std::array::from_fn(|a| k.grid[a] as f64 * spacing[a]);
    implexity_geometry::field_registration::axis_aligned_registration(k.grid, [0.0; 3], bounds, "cell")
        .map(|r| r.to_wire())
        .map_err(|e| CaeError::contract(e.to_string()))
}

fn cell_average(k: &UnifiedKernel, values: &[f64], width: usize) -> Vec<f64> {
    let mut out = vec![0.0; k.nc * width];
    for c in 0..k.nc {
        for j in 0..width {
            out[c * width + j] = (0..6).map(|t| values[(6 * c + t) * width + j]).sum::<f64>() / 6.0;
        }
    }
    out
}

impl NativeUnifiedHistoryProvider {
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.unified_history.NativeUnifiedHistoryProvider";

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn runtime_support_map() -> Map<String, Value> {
        json!({"status": "native_field_solver", "history": true, "data": "user_required", "limitations": LIMITATIONS})
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn component_slots_map() -> Map<String, Value> {
        let mut out = Map::new();
        for (key, kind) in KINDS {
            out.insert(
                key.into(),
                json!({"component_kind": kind, "required": true, "integration": "shared_domain_residual_and_complete_history_adjoint"}),
            );
        }
        out.insert("solid.material_history".into(), json!({"component_kind": "material_state_evolution", "required": false, "integration": "current_constitutive_residual_and_energy_with_full_history_adjoint"}));
        out.insert("solid.viscoelasticity".into(), json!({"component_kind": "viscoelastic_solid", "required": false, "integration": "native_branch_state_stress_heat_in_shared_residual"}));
        out.insert("solid.fatigue_observer".into(), json!({"component_kind": "fatigue_history_observer", "required": false, "integration": "postprocess_solved_solid_history_no_gradient_or_feedback"}));
        out.insert("field_sources".into(), json!({"component_kind": "coupled_history_source", "required": false, "multiple": true, "integration": "additional_sparse_field_blocks_and_conservative_sources_in_same_history_residual"}));
        out.insert("history_observers".into(), json!({"component_kind": "history_response_observer", "required": false, "multiple": true, "integration": "full_history_response_and_admissibility_NOT_feedback_residual"}));
        out
    }

    #[must_use]
    pub fn exact_computation_effort_capability() -> Value {
        json!({
            "schema": "implexity-provider-exact-effort-capability/1",
            "default_solver_policy": SPARSE_ILU_KRYLOV_POLICY,
            "profiles": [UnifiedHistoryExactProfile::direct().to_wire(), UnifiedHistoryExactProfile::accelerated().to_wire()],
        })
    }

    #[must_use]
    pub fn coupling_approximation_capability() -> Value {
        json!({
            "schema": "implexity-provider-coupling-approximation-capability/1",
            "presets": {
                "exact": {"sweeps": 0, "lagged_coupling_ids": []},
                "staged": {"sweeps": 2, "lagged_coupling_ids": STAGED_COUPLING_IDS},
                "interactive": {"sweeps": 1, "lagged_coupling_ids": STAGED_COUPLING_IDS},
            },
            "explicit_laggable_coupling_ids": STAGED_COUPLING_IDS,
            "restoration": "unchanged_full_grid_monolithic_exact_solve",
            "authoritative": false,
        })
    }

    #[must_use]
    pub fn coupling_control_capability() -> Value {
        let rows = [
            (
                STAGED_THERMAL_TO_FLOW,
                "Temperature-dependent fluid-property feedback",
                "Exchange from the shared temperature state into the provider fluid-property laws.",
                "Keeping this exchange active couples the flow correction to the current thermal state.",
            ),
            (
                STAGED_FLOW_TO_THERMAL,
                "Flow enthalpy and work feedback",
                "Exchange from the provider flow state into shared enthalpy transport and work terms.",
                "Keeping this exchange active couples the shared thermal correction to the current flow state.",
            ),
        ];
        let couplings: Vec<Value> = rows
            .iter()
            .map(|(id, label, description, note)| {
                json!({"id": id, "label": label, "description": description, "cost_tier": "high", "cost_note": note,
                    "default_state": "active", "allowed_states": ["active", "lagged"],
                    "truth_status_by_state": {"active": "exact_within_provider_scope", "lagged": "approximate_initialization_only"},
                    "current_state": "active",
                    "configuration": {"kind": "computation_effort_request", "field": "coupling_approximation.lagged_coupling_ids", "active_when_absent": true},
                    "restoration_required_before_commit": true})
            })
            .collect();
        json!({
            "schema": "implexity-provider-coupling-control/1",
            "selection_scope": "initialization_only",
            "default_policy": "all_active",
            "couplings": couplings,
            "presets": {
                "exact": {"lagged_coupling_ids": []},
                "staged": {"lagged_coupling_ids": STAGED_COUPLING_IDS},
                "interactive": {"lagged_coupling_ids": STAGED_COUPLING_IDS},
            },
        })
    }

    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn editor_schema(problem: Option<&Value>) -> Value {
        let Some(problem) = problem.filter(|p| p.is_object()) else { return json!({}) };
        let registry = &implexity_core::registries::global().addins;
        let catalog = super::source_catalog().ok();
        let mut properties = Map::new();
        let rows = problem.get("field_sources").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut schemas = Vec::new();
        for row in &rows {
            let name = row.get("component").and_then(Value::as_str);
            let adapter = name.and_then(|n| {
                let known = catalog.as_ref().is_some_and(|c| c.contains_key(n));
                if known { registry.get(n).ok().and_then(|r| r.adapter.clone()) } else { None }
            });
            let hook = adapter.as_ref().and_then(|a| {
                a.interface(implexity_physics_fields::host::AUTHORING_INTERFACE)
                    .and_then(|i| i.downcast_ref::<implexity_physics_fields::adapter::FieldSourceAdapter>())
                    .map(|f| {
                        let schema =
                            f.authoring().editor_schema(row.get("settings").unwrap_or(&Value::Null), problem);
                        (schema, f.editor_label().map(str::to_string))
                    })
            });
            match (name, hook) {

                (Some(n), Some((schema, label))) if schema.as_object().is_some_and(|m| !m.is_empty()) => schemas.push(
                    json!({"title": label.as_deref().unwrap_or(n), "properties": {"component": {"title": "Source component", "enum": [n]}, "settings": schema}}),
                ),
                _ => schemas.push(json!({})),
            }
        }
        if !schemas.is_empty() {
            properties.insert(
                "field_sources".into(),
                json!({"title": "Coupled field sources", "type": "array", "prefixItems": schemas}),
            );
        }
        let solid_base = json!({"title": "Solid field",
            "properties": implexity_physics_solid::solid_history::solid_editor_properties(false)});
        let solid_overlay = implexity_physics_solid::solid_history::provider_editor_schema(
            problem.get("solid").unwrap_or(&Value::Null),
        );
        properties.insert(
            "solid".into(),
            implexity_physics_solid::solid_history::merge_editor_schema(&solid_base, &solid_overlay),
        );
        properties.insert("fluid".into(), crate::incompressible_transport::fluid_editor_schema());
        let mut transports = vec![json!("trilinear_cell_average_v1")];
        transports.extend(NODAL_PROFILES.iter().map(|p| json!(p)));
        properties.insert("temperature_transport".into(), json!({"title": "Shared-temperature transport discretization",
            "enum": transports, "default": "trilinear_cell_average_v1",
            "description": format!("trilinear_cell_average_v1: cell-averaged enthalpy transport. nodal_dual_upwind_v1: two-point nodal diffusion and first-order upwind enthalpy on T4 nodal dual volumes. {SMOOTH_PROFILE}: the same with a C1-smoothed upwind split and requires temperature_transport_smoothing_velocity_m_s. Numerical-diffusion and mesh studies remain required.")}));
        properties.insert("temperature_transport_smoothing_velocity_m_s".into(), json!({"title": "Upwind smoothing velocity", "type": "number",
            "exclusiveMinimum": 0, "unit": "m/s", "default": 1e-3,
            "description": format!("Only for {SMOOTH_PROFILE}, where it is required; every other transport refuses it. The split is (u ± sqrt(u² + δ²))/2 with δ this speed times the face effective fluid fraction: faces much faster than δ recover first-order upwinding, slower faces receive additional smoothing diffusion.")}));
        properties.insert("phase_map".into(), json!({"title": "Phase map", "type": "object", "properties": {
            "projection_beta": {"title": "Projection sharpness", "type": "number", "minimum": 0, "maximum": 32, "unit": "1", "default": 0,
                "description": "Endpoint-preserving tanh projection of the (filtered) control about 0.5; 0 means the occupancy equals the control."},
            "provenance": {"title": "Phase-map provenance", "type": "string", "minLength": 1,
                "description": "Why this occupancy/phase relation represents the authored geometry."},
            "density_filter": {"title": "Density filter (length scale)", "type": "object",
                "description": "Optional. Helmholtz PDE filter of model:control before the projection, with Neumann boundaries; the gradient includes its exact adjoint. Omit for no filter.",
                "properties": {
                    "method": {"title": "Method", "enum": ["helmholtz"], "default": "helmholtz"},
                    "radius_mm": {"title": "Filter radius", "type": "number", "exclusiveMinimum": 0, "unit": "mm",
                        "description": "Equivalent cone-filter radius R on the analysis grid; the Helmholtz length is R/(2*sqrt(3)). Choose at least about two cells."},
                    "provenance": {"title": "Length-scale provenance", "type": "string", "minLength": 1,
                        "description": "Manufacturing or modelling reason for this length scale."}},
                "required": ["method", "radius_mm", "provenance"]}},
            "required": ["projection_beta", "provenance"]}));
        properties.insert("validity".into(), json!({"title": "Validity limits", "type": "object", "properties": {
            "max_displacement_over_cell": {"title": "Maximum displacement per cell", "type": "number",
                "exclusiveMinimum": 0, "maximum": super::MAX_DISPLACEMENT_OVER_CELL, "unit": "1 (fraction of the smallest cell)", "default": 0.05,
                "description": "Fixed-geometry bound on max |u| over solid nodes divided by the smallest cell spacing. The diffuse interface spans about one cell, so at most a quarter cell is admissible; justify the value for the study."},
            "min_fluid_feature_cells": {"title": "Narrowest resolved fluid feature", "type": "number", "minimum": 1, "unit": "cells", "default": 1,
                "description": "Width of the narrowest fluid channel or gap the study must resolve, in cells of the smallest spacing. Narrower features are not resolved by the grid."},
            "max_relative_channel_width_change": {"title": "Maximum relative channel-width change", "type": "number",
                "exclusiveMinimum": 0, "exclusiveMaximum": 1, "unit": "1", "default": 0.1,
                "description": "Bound on 2 max |u| / (narrowest fluid feature width): two facing walls may each move by max |u| while the fluid geometry stays fixed."},
            "solid_reporting_threshold": {"title": "Solid reporting threshold", "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1, "unit": "1", "default": 0.5,
                "description": "Cells with solid occupancy at or above this value are reported as solid; their nodes are the ones the displacement screens examine."},
            "fluid_reporting_threshold": {"title": "Fluid reporting threshold", "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1, "unit": "1", "default": 0.5,
                "description": "Cells with fluid fraction at or above this value are reported as fluid."}},
            "required": super::VALIDITY_KEYS}));
        properties.insert("applicability_policy".into(), json!({"type": "string", "enum": ["enforce", "report_only"], "default": "enforce",
            "description": "Report-only exploration retains finite states, Newton convergence and conservation checks; material and kinematic limits remain reported."}));
        properties.insert("numerical_initial_guess".into(), json!({"type":"object","additionalProperties":false,"properties":{"schema":{"const":"implexity-source-bound-numerical-guess/1"},"destination_design_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"destination_initial_state_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"time_coordinates_s":{"type":"array","items":{"type":"number"}},"states":{"type":"array","items":{"type":"array","items":{"type":"number"}}},"source_archive_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"source_design_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"provenance":{"type":"object"}},"required":["schema","destination_design_sha256","destination_initial_state_sha256","time_coordinates_s","states","source_archive_sha256","source_design_sha256","provenance"]}));
        properties.insert("operator_split_history".into(),json!({"oneOf":[{"type":"object","additionalProperties":false,"properties":{"schema":{"const":"implexity-operator-split-history/1"},"local_substeps":{"type":"integer","minimum":1},"coupling_sweeps":{"type":"integer","enum":[1,2]},"provenance":{"type":"string","minLength":1}},"required":["schema","local_substeps","coupling_sweeps","provenance"]},{"type":"object","additionalProperties":false,"properties":{"schema":{"enum":["implexity-frozen-reference-aged-condition/1","implexity-prescribed-stress-relaxing-aged-condition/1"]},"provenance":{"type":"string","minLength":1}},"required":["schema","provenance"]}]}));
        properties.insert("numerical_fallback".into(), json!({"type": "string", "enum": ["none", "heat_continuation"], "default": "none",
            "description": "No load continuation by default. Explicit heat_continuation permits auxiliary heat-load solves after Newton failure, then still requires the exact authored final load and all physical/identity gates."}));
        json!({"properties": properties})
    }

    #[must_use]
    pub fn study_templates(problem: Option<&Value>) -> Vec<Value> {
        let registry = &implexity_core::registries::global().addins;
        let mut rows = Vec::new();
        if let Ok(order) = super::source_component_order() {
            for name in &order {
                let Some(adapter) = registry.get(name).ok().and_then(|r| r.adapter.clone()) else { continue };
                if let Some(hook) = adapter
                    .interface(implexity_physics_fields::host::AUTHORING_INTERFACE)
                    .and_then(|i| i.downcast_ref::<implexity_physics_fields::adapter::FieldSourceAdapter>())
                {
                    rows.extend(hook.authoring().study_templates(problem.unwrap_or(&Value::Null)));
                }
            }
        }
        if let Some(p) = problem.filter(|p| p.is_object()) {
            rows.extend(implexity_physics_solid::solid_history::solid_study_templates(
                p.get("solid").unwrap_or(&Value::Null),
                &["solid"],
            ));
        }
        rows
    }


    pub fn contract() -> CaeResult<AddInContract> {
        let mut c = AddInContract::new(NAME);
        c.category = AddInCategory::Field;
        let inputs: Vec<DesignCoordinateRef> = COORDS
            .iter()
            .enumerate()
            .map(|(i, coord)| {
                let mut d = DesignCoordinateRef::new(*coord, format!("{NAME}.design.{i}"));
                d.addin_id = NAME.into();
                d
            })
            .collect();
        let deps: Vec<String> = inputs.iter().map(|d| d.port_id.clone()).collect();
        let units = installed_response_units()?;
        let presentation = published_response_metadata(&units)?;
        c.responses = units
            .iter()
            .map(|(name, unit)| {
                let meta = &presentation[name];
                let text = |k: &str| meta.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let mut cap = ResponseCapability::new(name.clone());
                cap.unit.clone_from(unit);
                cap.differentiable = Some(true);
                cap.design_reachable = Some(true);
                cap.depends_on.clone_from(&deps);
                cap.label = text("label");
                cap.description = text("description");
                cap.family = text("family");
                cap
            })
            .collect();
        c.scope = vec!["*".into()];
        c.fidelity = Fidelity::Intermediate;
        c.priority = 50;
        c.runtime_route = RuntimeRoute::Array;
        c.exact_design_derivatives = Some(true);
        c.exact_state_transpose = Some(true);
        c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        c.contract_version = 2;
        c.compatibility_mode = false;
        c.owner_id = format!("provider:{NAME}");
        c.execution_kind = Some(ExecutionKind::Provider);
        c.supported_operations = [
            "preflight",
            "preflight_design",
            "evaluate",
            "sensitivity",
            "sensitivities",
            "optimize",
            "accept_design",
            "install_matching_time_guess",
            "export_matching_time_guess",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        c.no_op_operations = Vec::new();
        c.design_inputs = inputs;
        c.checked()
    }


    pub fn declaration(p: &Value) -> CaeResult<Value> {
        use implexity_physics_solid::coupling::edge;
        let solid = p.get("solid").cloned().unwrap_or_else(|| json!({}));
        let components = solid.get("components").cloned().unwrap_or_else(|| json!({}));
        let reversible = components.get("material") == Some(&json!("constant_strain_thermoelastic_solid"));
        let dissipative =
            ["plasticity", "creep"].iter().any(|k| components.get(*k).is_some_and(|v| !v.is_null()))
                || solid.get("viscoelasticity").is_some_and(|v| !v.is_null());
        let mut edges = vec![
            edge(
                "flow",
                "thermal",
                "wall_heat_flux",
                "monolithic",
                "shared temperature FE/FV conservative enthalpy residual",
            ),
            edge(
                "thermal",
                "flow",
                "wall_temperature",
                "monolithic",
                "same temperature enters fluid material laws",
            ),
            edge(
                "thermal",
                "structure",
                "temperature_field",
                "monolithic",
                "same temperature enters inelastic constitutive history",
            ),
            edge(
                "flow",
                "structure",
                "pressure_and_shear_load",
                "monolithic",
                "density-jump load from solved absolute pressure and total viscous stress",
            ),
        ];
        if reversible {
            edges.push(edge(
                "structure",
                "thermal",
                "reversible_thermoelastic_heat",
                "monolithic",
                "explicit Helmholtz entropy coupling",
            ));
        }
        if dissipative {
            edges.push(edge(
                "structure",
                "thermal",
                "inelastic_dissipation_heat",
                "monolithic",
                "selected inelastic/viscoelastic heat independent of reversible entropy storage",
            ));
        }
        let strings = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let base = CouplingDeclaration {
            provider: NAME.into(),
            active_physics: strings(&["flow", "thermal", "structure"]),
            ports: Vec::new(),
            edges,
            closed_loops: if reversible || dissipative {
                vec![strings(&["flow", "thermal", "structure"])]
            } else {
                vec![strings(&["flow", "thermal"])]
            },
            intentionally_frozen: Vec::new(),
            notes: strings(&LIMITATIONS),
        };
        let with_material = implexity_physics_solid::coupling::with_material_couplings(
            base,
            solid.get("material_history").unwrap_or(&Value::Null),
        )?;
        let with_maxwell =
            implexity_physics_solid::polymer::with_environmental_maxwell_couplings(with_material, &solid);
        implexity_core::history_field_sources::with_source_couplings(
            &implexity_core::registries::global().addins,
            &super::source_catalog()?,
            with_maxwell.to_value(),
            p.get("field_sources"),
            p as &dyn Any,
        )
    }


    pub fn parts(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Parts> {
        let p = normalise(problem)?;
        let names = design.names();
        if names.len() != 3 || COORDS.iter().any(|c| !design.contains(c)) {
            return contract("all three shared-domain coordinate families required");
        }
        let k = kernel(&p, &binding_value(), &active_exact_profile())?;
        let (control, spacing, material) = coordinates(k.grid, design)?;
        let x = k.physical(&control, &spacing, &material)?;
        let solid: f64 = x[..k.nc].iter().sum();
        let fluid: f64 = x[..k.nc].iter().map(|v| 1.0 - v).sum();
        if solid < 1e-10 || fluid < 1e-10 {
            return contract("coupled responses require nonzero physical solid AND fluid volume");
        }
        Ok(Parts { p, k, x, control, spacing, material })
    }

    #[allow(clippy::unused_self)]
    fn split(&self, g: &[f64], parts: &Parts) -> CaeResult<NamedArrays> {
        let k = &parts.k;
        let (control, spacing) = k.occupancy_vjp(&parts.control, &parts.spacing, &g[..k.nc])?;
        let grid = k.grid.to_vec();
        let mut out = NamedArrays::new();
        let to = |v: Vec<f64>, shape: &[usize]| {
            ArrayD::from_shape_vec(IxDyn(shape), v).map_err(|e| CaeError::contract(e.to_string()))
        };
        out.insert(COORDS[0], to(control, &grid)?);
        out.insert(COORDS[1], to((0..3).map(|a| g[k.nc + a] + spacing[a]).collect(), &[3])?);
        out.insert(COORDS[2], to(g[k.nc + 3..].to_vec(), &grid)?);
        Ok(out)
    }


    pub fn evaluate_named(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Evaluation> {
        self.evaluate_named_with(problem, design, true)
    }


    pub fn evaluate_forward_named(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Evaluation> {
        self.evaluate_named_with(problem, design, false)
    }

    fn evaluate_named_with(
        &self,
        problem: &Value,
        design: &NamedArrays,
        retain: bool,
    ) -> CaeResult<Evaluation> {
        let parts = self.parts(problem, design)?;
        if self.split_selected(&parts) { return self.split_evaluation(&parts, design, true); }
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        let k = &parts.k;
        let x = &parts.x;
        let operation = k.new_operation_context("canonical_lambda_one_evaluate_design", true)?;
        if retain {
            k.begin_exact_factorization_reuse(Some(x), Some(&operation))?;
        } else {
            k.discard_exact_factorization_reuse();
        }
        let solved = (|| -> CaeResult<(HistorySolution, Map<String, Value>)> {
            let sol = k.solve(x, Some(&operation))?;
            let mut dg = k.diagnostics(x, &sol, &parts.control, &parts.spacing, &design_identity(design)?)?;
            dg.insert("forward_convergence".into(), k.forward_convergence(&sol));
            k.record_guarded_solution(x, &sol, Some(&operation))?;
            Ok((sol, dg))
        })();
        let (sol, dg) = match solved {
            Ok(v) => v,
            Err(e) => {
                k.discard_exact_factorization_reuse();
                return Err(e);
            }
        };
        let result = self.evaluation_from_solution(&parts, design, &sol, true, dg, None)?;
        self.remember_endpoint(&parts, design, &result.responses)?;
        Ok(result)
    }


    pub fn audit_named(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Value> {
        let evaluation = self.evaluate_forward_named(problem, design)?;
        let parts = self.parts(problem, design)?;
        let k = &parts.k;
        if self.split_selected(&parts) {
            let record=self.split_record(&parts, design, true)?.ok_or_else(|| CaeError::contract("split history unavailable"))?;
            return Ok(json!({"responses":evaluation.responses,"audit":k.field_audit(&record.trajectory.states,&parts.x)?,"history_method":record.trajectory.identity}));
        }
        let key = design_key(&parts.x);
        let solution =
            k.lock().last.as_ref().filter(|(k2, _)| *k2 == key).map(|(_, s)| Arc::clone(s)).ok_or_else(
                || CaeError::contract("the audited solution is no longer the kernel's latest history"),
            )?;
        let audit = k.field_audit(&solution.states, &parts.x)?;
        Ok(json!({
            "responses": evaluation.responses,
            "audit": audit,
            "coupling_history": evaluation.diagnostics.get("coupling_history").cloned().unwrap_or(Value::Null),
        }))
    }


    pub fn field_snapshot_named(
        &self,
        problem: &Value,
        design: &NamedArrays,
        state: Option<usize>,
    ) -> CaeResult<Value> {
        let latest = |parts: &Parts| {
            let key = design_key(&parts.x);
            parts.k.lock().last.as_ref().filter(|(k2, _)| *k2 == key).map(|(_, s)| Arc::clone(s))
        };
        let mut parts = self.parts(problem, design)?;
        if self.split_selected(&parts) {
            let kernel=Arc::clone(&parts.k);let _operation=kernel.exclusive();
            let record=self.split_record(&parts,design,true)?.ok_or_else(||CaeError::contract("split history unavailable"))?;
            return parts.k.field_snapshot(&record.trajectory.states,&parts.x,state.unwrap_or(parts.k.nt-1));
        }
        if latest(&parts).is_none() {
            self.evaluate_forward_named(problem, design)?;
            parts = self.parts(problem, design)?;
        }
        let k = &parts.k;
        let solution = latest(&parts)
            .ok_or_else(|| CaeError::contract("the solution is no longer the kernel's latest history"))?;
        let n = state.unwrap_or(solution.states.len().saturating_sub(1));
        k.field_snapshot(&solution.states, &parts.x, n)
    }


    #[allow(clippy::type_complexity)]
    pub fn nodal_temperatures_named(
        &self,
        problem: &Value,
        design: &NamedArrays,
    ) -> CaeResult<Vec<(Vec<f64>, Option<Vec<f64>>)>> {
        self.evaluate_forward_named(problem, design)?;
        let parts = self.parts(problem, design)?;
        let k = &parts.k;
        if self.split_selected(&parts) {
            let record=self.split_record(&parts,design,false)?.ok_or_else(||CaeError::contract("split history unavailable"))?;
            return record.trajectory.states.iter().enumerate().map(|(n,z)|{let full=k.expand(n,z)?;Ok((k.nodal_temperature_generic(n,&full),k.film.as_ref().map(|film|film.fluid_nodal_temperature(n,&full))))}).collect();
        }
        let key = design_key(&parts.x);
        let solution = k
            .lock()
            .last
            .as_ref()
            .filter(|(k2, _)| *k2 == key)
            .map(|(_, s)| Arc::clone(s))
            .ok_or_else(|| CaeError::contract("the solution is no longer the kernel's latest history"))?;
        solution
            .states
            .iter()
            .enumerate()
            .map(|(n, z)| {
                let full = k.expand(n, z)?;
                Ok((
                    k.nodal_temperature_generic(n, &full),
                    k.film.as_ref().map(|film| film.fluid_nodal_temperature(n, &full)),
                ))
            })
            .collect()
    }

    fn remember_endpoint(
        &self,
        parts: &Parts,
        design: &NamedArrays,
        responses: &BTreeMap<String, f64>,
    ) -> CaeResult<()> {
        let k = &parts.k;
        let key = design_key(&parts.x);
        let cached = k.lock().last.as_ref().filter(|(k2, _)| *k2 == key).map(|(_, s)| Arc::downgrade(s));
        let mut slot = self.endpoint.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = match cached {
            None => None,
            Some(solution) => {
                let token = implexity_core::registries::global().addins.binding_token();
                Some(EndpointOwner {
                    problem: implexity_core::wire::fingerprint_value(&parts.p),
                    design: design_identity(design)?,
                    token: (token.generation, token.fingerprint),
                    kernel: Arc::downgrade(k),
                    key,
                    solution,
                    responses: responses.clone(),
                })
            }
        };
        Ok(())
    }


    pub fn cached_evaluation(
        &self,
        problem: &Value,
        design: &NamedArrays,
        operating_point: usize,
    ) -> CaeResult<CachedEvaluation> {
        let parts = self.parts(problem, design)?;
        if self.split_selected(&parts) {
            return if operating_point != 0 { Ok(CachedEvaluation::Unavailable(Map::new())) } else { self.split_cached(&parts, design) };
        }
        let unavailable = |reason: &str| {
            Ok(CachedEvaluation::Unavailable(
                json!({"available": false, "reason": reason}).as_object().cloned().unwrap_or_default(),
            ))
        };
        if operating_point != 0 {
            return unavailable("operating_point_not_supported");
        }
        let slot = self.endpoint.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = slot.as_ref() else { return unavailable("cache_missing") };
        let normalized = normalise(problem)?;
        let token = implexity_core::registries::global().addins.binding_token();
        if implexity_core::wire::fingerprint_value(&normalized) != record.problem
            || design_identity(design)? != record.design
            || (token.generation, token.fingerprint) != record.token
        {
            return unavailable("identity_mismatch");
        }
        let Some(k) = record.kernel.upgrade() else { return unavailable("owner_released") };
        let current = k.lock().last.as_ref().map(|(key, s)| (key.clone(), Arc::clone(s)));
        let Some((key, solution)) = current else { return unavailable("cached_state_replaced") };
        let same = record.solution.upgrade().is_some_and(|s| Arc::ptr_eq(&s, &solution));
        if key != record.key || !same {
            return unavailable("cached_state_replaced");
        }
        k.require_authoritative_solution(&solution, None)?;
        let (control, spacing, material) = coordinates(k.grid, design)?;
        let x = k.physical(&control, &spacing, &material)?;
        let responses = record.responses.clone();
        drop(slot);
        let parts = Parts { p: normalized, k: Arc::clone(&k), x, control, spacing, material };
        let dg = json!({
            "design_state_id": design_identity(design)?,
            "cached_endpoint": {"solve_started": false, "history_copied": false,
                "engineering_acceptance": false, "physical_qualification": false},
            "residual_norms": solution.residual_norms,
        });
        let result = self.evaluation_from_solution(
            &parts,
            design,
            &solution,
            false,
            dg.as_object().cloned().unwrap_or_default(),
            Some(responses),
        )?;
        Ok(CachedEvaluation::Available(result))
    }

    #[allow(clippy::too_many_lines)]
    fn evaluation_from_solution(
        &self,
        parts: &Parts,
        design: &NamedArrays,
        sol: &HistorySolution,
        include_history: bool,
        mut dg: Map<String, Value>,
        responses: Option<BTreeMap<String, f64>>,
    ) -> CaeResult<Evaluation> {
        let k = &parts.k;
        let p = &parts.p;
        let x = &parts.x;
        let s = &k.s;
        let f = &k.f;
        let m = &s.model;
        let grid = k.grid.to_vec();
        let nc = k.nc;
        let sl = k.solid_slice.clone();
        let fl = k.fluid_slice.clone();
        let nt = k.nt;
        let full: Vec<Vec<f64>> = if include_history {
            sol.states.iter().enumerate().map(|(n, z)| k.expand(n, z)).collect::<CaeResult<_>>()?
        } else {
            vec![k.expand(nt - 2, &sol.states[nt - 2])?, k.expand(nt - 1, &sol.states[nt - 1])?]
        };
        let last = &full[full.len() - 1];
        let prev = &full[full.len() - 2];
        let sd = s.observe(nt - 1, &last[sl.clone()], &prev[sl.clone()], x);
        let ff = f.fields(&last[fl.clone()]);
        let rho = &x[..nc];
        let tr = k.transfer.observables(last, x);
        let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
        let put = |fields: &mut BTreeMap<String, FieldValue>, name: &str, v: FieldValue| {
            fields.insert(name.to_string(), v);
        };
        let grid3 = |extra: &[usize]| {
            let mut g = grid.clone();
            g.extend_from_slice(extra);
            g
        };
        put(&mut fields, "design_control", arr(parts.control.clone(), &grid)?);
        put(&mut fields, "design_material", arr(parts.material.clone(), &grid)?);
        put(&mut fields, "solid_fraction", arr(rho.to_vec(), &grid)?);
        put(&mut fields, "fluid_fraction", arr(rho.iter().map(|v| 1.0 - v).collect(), &grid)?);
        put(&mut fields, "temperature_K", arr(ff.temperature.clone(), &grid)?);
        put(
            &mut fields,
            "fluid_velocity_m_s",
            arr(ff.velocity.iter().flatten().copied().collect(), &grid3(&[3]))?,
        );
        put(
            &mut fields,
            "fluid_pressure_absolute_Pa",
            arr(ff.pressure.iter().map(|v| f.pref + v).collect(), &grid)?,
        );
        put(&mut fields, "solid_von_mises_Pa", arr(cell_average(k, &sd.von_mises, 1), &grid)?);
        put(
            &mut fields,
            "solid_equivalent_plastic_strain",
            arr(cell_average(k, &sd.equivalent_plastic, 1), &grid)?,
        );
        put(
            &mut fields,
            "solid_equivalent_creep_strain",
            arr(cell_average(k, &sd.equivalent_creep, 1), &grid)?,
        );
        let sthr = p["validity"]["solid_reporting_threshold"].as_f64().unwrap_or(f64::NAN);
        let fthr = p["validity"]["fluid_reporting_threshold"].as_f64().unwrap_or(f64::NAN);
        put(
            &mut fields,
            "solid_reporting_mask",
            arr(rho.iter().map(|r| f64::from(u8::from(*r >= sthr))).collect(), &grid)?,
        );
        put(
            &mut fields,
            "fluid_reporting_mask",
            arr(rho.iter().map(|r| f64::from(u8::from(1.0 - r >= fthr))).collect(), &grid)?,
        );
        let faces = tr.interface_force_n.len();
        put(
            &mut fields,
            "density_jump_face_force_N",
            arr(tr.interface_force_n.iter().flatten().copied().collect(), &[faces, 3])?,
        );
        put(
            &mut fields,
            "density_jump_face_traction_Pa",
            arr(tr.traction_pa.iter().flatten().copied().collect(), &[faces, 3])?,
        );
        put(&mut fields, "density_jump_signed_measure", arr(tr.signed_density_jump.clone(), &[faces])?);
        let ne = s.ne;
        put(
            &mut fields,
            "solid_tetrahedral_stress_mandel_Pa",
            arr(sd.stress.iter().flatten().copied().collect(), &[ne, 6])?,
        );
        put(
            &mut fields,
            "solid_tetrahedral_backstress_mandel_Pa",
            arr(sd.backstress.iter().flatten().copied().collect(), &[ne, 6])?,
        );
        put(
            &mut fields,
            "solid_tetrahedral_yield_surface_residual_Pa",
            arr(sd.yield_residual.clone(), &[ne])?,
        );
        if k.density_filter.is_some() {
            put(
                &mut fields,
                "design_control_filtered",
                arr(k.filtered_control(&parts.control, &parts.spacing)?, &grid)?,
            );
        }
        let h = &parts.spacing;
        put(
            &mut fields,
            "solid_mesh_nodes_m",
            arr(
                s.mesh.ijk.iter().flat_map(|q| (0..3).map(move |a| q[a] as f64 * h[a] * 1e-3)).collect(),
                &[s.nn, 3],
            )?,
        );
        put(
            &mut fields,
            "solid_tetrahedron_nodes",
            arr(s.mesh.tets.iter().flatten().map(|v| *v as f64).collect(), &[ne, 4])?,
        );
        put(
            &mut fields,
            "solid_tetrahedron_native_cell",
            arr(s.mesh.owners.iter().map(|v| *v as f64).collect(), &[ne])?,
        );
        if include_history {
            let states: Vec<f64> = sol.states.iter().flatten().copied().collect();
            put(&mut fields, "state_history_nondimensional", arr(states, &[nt, k.state_size])?);
            put(&mut fields, "times_s", arr(s.times.clone(), &[nt])?);
            let temps: Vec<f64> =
                full.iter().enumerate().flat_map(|(n, z)| s.nodal_temperature(n, &z[sl.clone()])).collect();
            put(&mut fields, "solid_temperature_nodes_history_K", arr(temps, &[nt, s.nn])?);
            let ft: Vec<f64> = full.iter().flat_map(|z| f.fields(&z[fl.clone()]).temperature).collect();
            let mut shape = vec![nt];
            shape.extend(&grid);
            put(&mut fields, "fluid_temperature_history_K", arr(ft, &shape)?);
            let u: Vec<f64> = full
                .iter()
                .enumerate()
                .flat_map(|(n, z)| s.nodal_displacement(n, &z[sl.clone()]).into_iter().flatten())
                .collect();
            put(&mut fields, "solid_displacement_nodes_history_m", arr(u, &[nt, s.nn, 3])?);
        }
        let mut solid_history_metadata: Map<String, Value> = Map::new();
        let registration_value = registration(k, &parts.spacing)?;
        let blocks: Vec<(&str, Option<Vec<Value>>, std::ops::Range<usize>, Vec<&str>)> = vec![
            (
                "material",
                m.history.as_ref().map(|h| h.metadata.clone()),
                m.layout.material_start()..s.internal_size,
                vec!["stored_energy_J_m3", "conductivity_W_mK", "yield_stress_Pa"],
            ),
            (
                "viscoelastic",
                m.viscoelastic.as_ref().map(|v| v.metadata()),
                m.layout.viscoelastic(),
                vec![
                    "stored_energy_J_m3",
                    "heat_increment_J_m3",
                    "assembled_heat_increment_J_m3",
                    "assembled_numerical_dissipation_increment_J_m3",
                ],
            ),
        ];
        for (prefix, metadata, part, names) in blocks {
            let Some(descriptions) = metadata else { continue };
            if include_history {
                let key = format!("{prefix}_state_history");
                let width = part.len();
                let mut data = Vec::with_capacity(nt * ne * width);
                for (n, z) in full.iter().enumerate() {
                    for fe in s.fields(n, &z[sl.clone()], x) {
                        data.extend_from_slice(&fe.state[part.clone()]);
                    }
                }
                put(&mut fields, &key, arr(data, &[nt, ne, width])?);
                let units: std::collections::BTreeSet<String> = descriptions
                    .iter()
                    .map(|d| d["units"].as_str().unwrap_or_default().to_string())
                    .collect();
                let unit = if units.len() == 1 {
                    json!(descriptions[0]["units"])
                } else {
                    json!("mixed_explicit_states")
                };
                solid_history_metadata.insert(key, json!({"units": unit, "association": "material_point_history",
                    "axes": ["time", "tetrahedron", "state"], "state_metadata": descriptions, "rank": "scalar",
                    "source": "native_shared_domain_solid_internal_state"}));
            }
            for suffix in names {
                let key = format!("{prefix}_{suffix}");
                let values = match (prefix, suffix) {
                    ("material", "stored_energy_J_m3") => sd.material_stored_energy.clone(),
                    ("material", "conductivity_W_mK") => sd.conductivity.clone(),
                    ("material", "yield_stress_Pa") => sd.yield_stress.clone(),
                    (_, name) => sd.polymer_column(name),
                };
                put(&mut fields, &key, arr(cell_average(k, &values, 1), &grid)?);
                let units = if suffix.ends_with("J_m3") {
                    "J/m^3"
                } else if suffix == "conductivity_W_mK" {
                    "W/(m*K)"
                } else {
                    "Pa"
                };
                solid_history_metadata.insert(key, json!({"units": units, "association": "cell", "rank": "scalar",
                    "phase_mask": "solid_reporting_mask", "source": "T4_volume_average_ghost_states_require_phase_mask",
                    "temporal_association": "final_stored_state", "time_index": nt - 1, "time_s": s.times[nt - 1]}));
            }
        }
        if let Some(mh) = m.history.as_ref().filter(|h| h.endpoints.is_some()) {
            let mut data = Vec::new();
            for e in 0..ne {
                let o = s.mesh.owners[e];
                data.extend(mh.physical_support(x[o], x[nc + 3 + o])?);
            }
            let width = mh.endpoints.as_ref().map_or(0, Vec::len);
            put(&mut fields, "material_state_support_fraction", arr(data, &[ne, width])?);
            solid_history_metadata.insert(
                "material_state_support_fraction".into(),
                json!({
                "units": "1", "association": "material_point_state_support", "axes": ["tetrahedron", "state"],
                "rank": "scalar", "source": "exact_physical_endpoint_volume_fraction_no_threshold",
                "state_endpoints": mh.endpoints, "temporal_association": "fixed_design_all_history_steps"}),
            );
            if include_history && let Some(row) = solid_history_metadata.get_mut("material_state_history") {
                row["physical_support_field"] = json!("material_state_support_fraction");
                row["state_interpretation"] =
                    json!("potential_inventory; physical_only_where_support_is_positive");
                row["numerical_extension"] = mh.numerical_extension_report();
            }
        }
        let mut observer_metadata: Map<String, Value> = Map::new();
        for (additions, descriptions) in k.observer_fields(&sol.states[sol.states.len() - 1], x)? {
            if additions.keys().any(|a| fields.contains_key(a)) {
                return contract("observer field collision");
            }
            let declared: std::collections::BTreeSet<&String> = descriptions.keys().collect();
            let emitted: std::collections::BTreeSet<&String> = additions.keys().collect();
            if declared != emitted {
                return contract("observer field metadata must exactly declare all emitted fields");
            }
            for (name, description) in &descriptions {
                if !["units", "association", "rank", "source"].iter().all(|k| description.get(*k).is_some()) {
                    return contract("observer fields require explicit units/association/rank/source");
                }
                observer_metadata.insert(name.clone(), description.clone());
            }
            for (name, values) in additions {
                let cell = descriptions[&name]["association"] == "cell";
                if cell && values.len() != nc {
                    return contract("observer cell field disagrees with the host mesh");
                }
                let shape = if cell { grid.clone() } else { vec![values.len()] };
                put(&mut fields, &name, arr(values, &shape)?);
            }
        }
        if include_history {
            for source in &k.sources {
                let (values, descriptions) = source.source.fields(&full, x)?;
                let declared: std::collections::BTreeSet<&String> = descriptions.keys().collect();
                let emitted: std::collections::BTreeSet<&String> = values.keys().collect();
                if values.keys().any(|a| fields.contains_key(a)) || declared != emitted {
                    return contract("source field identity/metadata collision");
                }
                for (name, value) in values {
                    put(&mut fields, &name, value);
                }
                for (name, d) in descriptions {
                    observer_metadata.insert(name, d);
                }
            }
        }
        for (a, face) in ff.faces.iter().enumerate() {
            put(
                &mut fields,
                &format!("fluid_velocity_{}_faces_m_s", ["x", "y", "z"][a]),
                arr(face.clone(), &f.map_shapes[a])?,
            );
        }
        let metadata = self.field_metadata(
            k,
            p,
            &fields,
            &registration_value,
            &solid_history_metadata,
            &observer_metadata,
        )?;
        dg.insert("field_registration".into(), registration_value.clone());
        dg.insert(
            "design_field_registrations".into(),
            json!({COORDS[0]: registration_value, COORDS[2]: registration_value}),
        );
        dg.insert("field_metadata".into(), Value::Object(metadata));
        let responses = if let Some(r) = responses {
            r
        } else {
            let values = k.response_values(&sol.states, x)?;
            k.response_names().into_iter().zip(values).collect()
        };
        let _ = design;
        Ok(Evaluation { provider: NAME.into(), responses, diagnostics: dg, fields })
    }

    #[allow(clippy::too_many_lines, clippy::unused_self)]
    fn field_metadata(
        &self,
        k: &UnifiedKernel,
        p: &Value,
        fields: &BTreeMap<String, FieldValue>,
        reg: &Value,
        solid_history: &Map<String, Value>,
        observers: &Map<String, Value>,
    ) -> CaeResult<Map<String, Value>> {
        let s = &k.s;
        let cell_fields = [
            "design_control",
            "design_control_filtered",
            "design_material",
            "solid_fraction",
            "fluid_fraction",
            "temperature_K",
            "fluid_velocity_m_s",
            "fluid_pressure_absolute_Pa",
            "solid_von_mises_Pa",
            "solid_equivalent_plastic_strain",
            "solid_equivalent_creep_strain",
            "solid_reporting_mask",
            "fluid_reporting_mask",
        ];
        let static_cell_fields = [
            "design_control",
            "design_control_filtered",
            "design_material",
            "solid_fraction",
            "fluid_fraction",
            "solid_reporting_mask",
            "fluid_reporting_mask",
            "liquid_guard_reporting_mask",
        ];
        let terminal_cell_fields = [
            "temperature_K",
            "fluid_velocity_m_s",
            "fluid_pressure_absolute_Pa",
            "solid_von_mises_Pa",
            "solid_equivalent_plastic_strain",
            "solid_equivalent_creep_strain",
            "fluid_saturation_temperature_K",
            "fluid_subcooling_K",
            "phase_cell_nodal_subcooling_K",
        ];
        let nodal = NODAL_PROFILES.contains(&p["temperature_transport"].as_str().unwrap_or_default());
        let terminal_index = k.nt - 1;
        let terminal_time = s.times[terminal_index];
        let mut out = Map::new();
        for name in fields.keys() {
            let n = name.as_str();
            let units = if n.ends_with("_m_s") {
                "m/s"
            } else if n.ends_with("_Pa") {
                "Pa"
            } else if n.ends_with("_K") {
                "K"
            } else if n.ends_with("_N") {
                "N"
            } else if n == "times_s" {
                "s"
            } else {
                "1"
            };
            let mut row = json!({"units": units, "association": "exact_history_or_interface",
                "source": "native_shared_domain_field_solution", "rank": "scalar"});
            let set = |row: &mut Value, pairs: Value| {
                if let (Some(r), Some(pm)) = (row.as_object_mut(), pairs.as_object()) {
                    for (a, b) in pm {
                        r.insert(a.clone(), b.clone());
                    }
                }
            };
            if cell_fields.contains(&n) {
                set(&mut row, json!({"association": "cell", "registration": reg, "coordinate_units": "mm"}));
            }
            if n == "fluid_velocity_m_s" {
                set(
                    &mut row,
                    json!({"rank": "vector", "components": ["x", "y", "z"], "component_frame": "model_cartesian",
                    "source": "exact_MAC_face_average_not_displacement", "phase_mask": "fluid_reporting_mask"}),
                );
            }
            if n == "temperature_K" {
                set(
                    &mut row,
                    json!({"source": if nodal {
                        "cell_average_for_coefficients_and_reporting; transport_is_nodal_dual_upwind"
                    } else {
                        "shared_nodal_temperature_trilinear_cell_centre_average_for_fluid_transport_reporting; caloric_storage_and_authored_source_use_T4_nodal_dual_volume"
                    }}),
                );
            }
            if n == "solid_temperature_nodes_history_K" {
                set(
                    &mut row,
                    json!({"association": "node_history", "axes": ["time", "node"],
                    "source": "shared_temperature_DOF_history; T4_Galerkin_conduction_and_row_sum_lumped_solid_fluid_volume_terms"}),
                );
            }
            if n == "fluid_temperature_history_K" {
                set(
                    &mut row,
                    json!({"association": "cell_history", "axes": ["time", "cell_x", "cell_y", "cell_z"],
                    "source": if nodal {
                        "cell_average_for_coefficients_and_reporting; transport_is_nodal_dual_upwind"
                    } else {
                        "trilinear_cell_centre_Q_average_of_shared_nodal_temperature_for_fluid_transport; not_the_fluid_caloric_quadrature"
                    }}),
                );
            }
            if ["solid_von_mises_Pa", "solid_equivalent_plastic_strain", "solid_equivalent_creep_strain"]
                .contains(&n)
            {
                set(
                    &mut row,
                    json!({"phase_mask": "solid_reporting_mask", "source": "T4_volume_average_ghost_states_require_phase_mask"}),
                );
            }
            if n == "fluid_pressure_absolute_Pa" {
                set(&mut row, json!({"phase_mask": "fluid_reporting_mask"}));
            }
            if n.starts_with("density_jump_face_") {
                set(
                    &mut row,
                    json!({"association": "internal_face", "rank": "vector", "components": ["x", "y", "z"]}),
                );
            }
            if ["solid_tetrahedral_stress_mandel_Pa", "solid_tetrahedral_backstress_mandel_Pa"].contains(&n) {
                set(
                    &mut row,
                    json!({"association": "tetrahedron", "rank": "tensor",
                    "components": ["xx", "yy", "zz", "sqrt(2)yz", "sqrt(2)xz", "sqrt(2)xy"], "tensor_convention": "orthonormal_Mandel"}),
                );
            }
            if n == "solid_tetrahedral_yield_surface_residual_Pa" {
                set(
                    &mut row,
                    json!({"association": "tetrahedron", "source": "selected_constitutive_yield_function; not von_Mises_minus_initial_yield"}),
                );
            }
            if n.contains("_faces_") {
                let axis = ["x", "y", "z"].iter().position(|a| n.split('_').nth(2) == Some(*a)).unwrap_or(0);
                set(
                    &mut row,
                    json!({"association": "face", "face_axis": axis, "source": "exact_MAC_velocity_DOFs"}),
                );
            }
            if n == "material_state_history"
                && let Some(mh) = &s.model.history
            {
                let units: std::collections::BTreeSet<&str> =
                    mh.metadata.iter().map(|a| a["units"].as_str().unwrap_or_default()).collect();
                set(
                    &mut row,
                    json!({"units": if units.len() == 1 { json!(mh.metadata[0]["units"]) } else { json!("mixed_explicit_states") },
                    "association": "material_point_history", "axes": ["time", "tetrahedron", "state"], "state_metadata": mh.metadata}),
                );
            }
            if let Some(extra) = solid_history.get(n) {
                set(&mut row, extra.clone());
                if row["association"] == "cell" {
                    set(&mut row, json!({"registration": reg, "coordinate_units": "mm"}));
                }
            }
            if n == "solid_mesh_nodes_m" {
                set(
                    &mut row,
                    json!({"units": "m", "association": "node", "rank": "vector", "components": ["x", "y", "z"],
                    "configuration": "reference", "geometric_role": "position", "coordinate_units": "m"}),
                );
            }
            if n == "solid_tetrahedron_nodes" {
                set(
                    &mut row,
                    json!({"association": "tetrahedron_connectivity", "index_base": 0, "node_field": "solid_mesh_nodes_m"}),
                );
            }
            if n == "solid_tetrahedron_native_cell" {
                set(
                    &mut row,
                    json!({"association": "tetrahedron", "index_base": 0, "cell_order": "C", "cell_grid": k.grid, "registration": reg}),
                );
            }
            if n == "solid_displacement_nodes_history_m" {
                set(
                    &mut row,
                    json!({"units": "m", "association": "node_history", "axes": ["time", "node", "component"],
                    "rank": "vector", "components": ["x", "y", "z"], "reference_coordinate_field": "solid_mesh_nodes_m"}),
                );
            }
            if n == "solid_temperature_nodes_history_K" {
                set(&mut row, json!({"reference_coordinate_field": "solid_mesh_nodes_m"}));
            }
            if let Some(observer) = observers.get(n) {
                row = observer.clone();
                if row["association"] == "cell" {
                    set(&mut row, json!({"registration": reg, "coordinate_units": "mm"}));
                }
                if let Some(mask) = row.get("phase_mask").and_then(Value::as_str)
                    && !mask.is_empty()
                    && !fields.contains_key(mask)
                {
                    return contract("observer references an unavailable phase mask");
                }
            }
            if static_cell_fields.contains(&n) {
                set(&mut row, json!({"temporal_association": "time_invariant_design_derived"}));
            } else if terminal_cell_fields.contains(&n) {
                set(
                    &mut row,
                    json!({"temporal_association": "final_stored_state", "time_index": terminal_index, "time_s": terminal_time}),
                );
            }
            let beta = p["phase_map"]["projection_beta"].as_f64().unwrap_or(f64::NAN);
            if n == "design_control" {
                set(
                    &mut row,
                    json!({"meaning": if k.density_filter.is_some() {
                    "topology_control_coordinate_before_density_filter_and_complementary_phase_projection"
                } else {
                    "topology_control_coordinate_before_complementary_phase_projection"
                }, "projection_beta": beta}),
                );
            }
            if n == "design_control_filtered" {
                set(
                    &mut row,
                    json!({"meaning": "helmholtz_filtered_control_entering_the_complementary_phase_projection",
                    "filter_radius_mm": k.density_filter.as_ref().map(|f| f.radius_mm), "projection_beta": beta}),
                );
            }
            if n == "design_material" {
                set(
                    &mut row,
                    json!({"meaning": "continuous_volume_fraction_coordinate_between_ordered_solid_material_endmembers",
                    "material_coordinate_endpoints": {"0": s.p["materials"][0]["name"], "1": s.p["materials"][1]["name"]},
                    "relaxation_caveat": "intermediate values are the authored continuous material interpolation, not a discrete material assignment"}),
                );
            }
            if n == "times_s" {
                set(
                    &mut row,
                    json!({"association": "time_coordinates", "coordinate_for": "stored_state_history"}),
                );
            }
            out.insert(name.clone(), row);
        }
        Ok(out)
    }


    pub fn sensitivities_named(
        &self,
        problem: &Value,
        design: &NamedArrays,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        let parts = self.parts(problem, design)?;
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        let k = &parts.k;
        let x = &parts.x;
        let names = k.response_names();
        let unique: std::collections::BTreeSet<&String> = responses.iter().collect();
        if responses.is_empty()
            || unique.len() != responses.len()
            || responses.iter().any(|r| !names.contains(r))
        {
            return contract("invalid shared-domain response set");
        }
        if self.split_selected(&parts) { return self.split_sensitivities(&parts, design, responses); }
        let operation = k.new_operation_context("canonical_lambda_one_sensitivities_design", true)?;
        k.begin_exact_factorization_reuse(Some(x), Some(&operation))?;
        let result = (|| -> CaeResult<DesignSensitivities> {
            let sol = k.solve(x, Some(&operation))?;
            let mut dg = k.diagnostics(x, &sol, &parts.control, &parts.spacing, &design_identity(design)?)?;
            k.record_guarded_solution(x, &sol, Some(&operation))?;
            k.certify_sensitivity(&sol, x)?;
            let (all_values, gu, gx) = k.response_partials(&sol.states, x, responses)?;
            let out = k.canonical_adjoint_many(x, &sol, &gu, &gx, &operation)?;
            let selected: Vec<f64> = responses
                .iter()
                .map(|r| names.iter().position(|n| n == r).map_or(f64::NAN, |i| all_values[i]))
                .collect();
            dg.insert(
                "forward_convergence".into(),
                k.certify_forward_convergence(&sol, &out, responses, &selected)?,
            );
            let reg = registration(k, &parts.spacing)?;
            dg.insert("field_registration".into(), reg.clone());
            dg.insert("design_field_registrations".into(), json!({COORDS[0]: reg, COORDS[2]: reg}));
            dg.insert("adjoint_factorizations".into(), json!(out.adjoint_factorizations));
            dg.insert("adjoint_factorization_builds".into(), json!(out.adjoint_factorization_builds));
            dg.insert("adjoint_factorization_reuses".into(), json!(out.adjoint_factorization_reuses));
            dg.insert(
                "maximum_transpose_relative_residual".into(),
                json!(out.maximum_transpose_relative_residual),
            );
            dg.insert("history_states_retained".into(), json!(out.history_states_retained));
            dg.insert("history_derivative".into(), json!(out.history_derivative));
            if let Some(d) = out.initial_state_design_derivative {
                dg.insert("initial_state_design_derivative".into(), json!(d));
                dg.insert(
                    "initial_state_design_gradient_norm".into(),
                    json!(out.initial_state_design_gradient_norm),
                );
            }
            let all: BTreeMap<String, f64> = names.iter().cloned().zip(all_values.iter().copied()).collect();
            let mut result = DesignSensitivities { diagnostics: dg, ..DesignSensitivities::default() };
            let m = responses.len();
            for (j, name) in responses.iter().enumerate() {
                result.responses.insert(name.clone(), all[name]);
                let g: Vec<f64> = (0..out.gradients.nrows).map(|i| out.gradients.data[i * m + j]).collect();
                result.gradients.insert(name.clone(), self.split(&g, &parts)?);
            }
            self.remember_endpoint(&parts, design, &all)?;
            Ok(result)
        })();
        k.discard_exact_factorization_reuse();
        result
    }


    pub fn preflight_named(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Map<String, Value>> {
        let parts = self.parts(problem, design)?;
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        let k = &parts.k;
        let x = &parts.x;
        let connectivity = k.connectivity(x)?;
        let initial_reduced = k.reduction.initial_for(x)?;
        k.validate_observers(std::slice::from_ref(&initial_reduced), x)?;
        let initial = k.expand(0, &initial_reduced)?;
        let zs = &initial[k.solid_slice.clone()];
        k.s.check(0, zs, zs, x)?;
        k.volume_transfer.check(0, &initial, &initial, x)?;
        let zf = &initial[k.fluid_slice.clone()];
        k.f.check(0, zf, zf, &k.fx(x))?;
        let opening = k.f.pressure_opening_support(&k.fx(x))?;
        let mut warnings: Vec<Value> = connectivity["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| json!({"code": "discrete_connectivity_not_satisfied", "message": m, "severity": "warning", "physical_qualification": false}))
            .collect();
        if opening["boundaries"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["pressure_bc_intersects_positive_solid_fraction"] == json!(true))
        {
            warnings.push(json!({"code": "pressure_boundary_exceeds_void_aperture",
                "message": "Declared pressure-boundary support intersects solid/intermediate cells. Fixed void regions do not automatically mask a boundary; review the actual full-face or explicit-aperture support in pressure_opening_support.",
                "severity": "warning", "physical_qualification": false}));
        }
        let initialization = match &k.preload {
            Some(p) => p.report(x)?,
            None => {
                json!({"method": "fixed_reference_legacy", "mechanical_preload_equilibrium_established": false})
            }
        };
        Ok(json!({"ok": true, "issues": [], "warnings": warnings, "pressure_opening_support": opening,
            "state_unknowns_per_step": k.state_size, "phase_connectivity": connectivity,
            "initialization": initialization, "shared_state_reduction": k.reduction.report(),
            "fluid_nodal_dual_volume": k.volume_transfer.report(),
            "fluid_dual_volume_initial_material_validity": k.volume_transfer.material_validity(0, &initial, x)?,
            "physical_qualification": false, "limitations": selected_limitations(&parts.p)})
        .as_object()
        .cloned()
        .unwrap_or_default())
    }


    pub fn validate_responses(problem: &Value, names: &[String]) -> CaeResult<()> {
        let p = normalise(problem)?;
        let mut available: std::collections::BTreeSet<String> =
            super::RESPONSES.iter().map(|s| (*s).to_string()).collect();
        available.extend(
            super::observers::selected_response_units(p.get("history_observers"))?
                .into_iter()
                .map(|(k, _)| k),
        );
        let sources = implexity_core::history_field_sources::response_units(
            &implexity_core::registries::global().addins,
            &super::source_catalog()?,
            p.get("field_sources"),
            &p as &dyn Any,
        )?;
        available.extend(sources.into_keys());
        let absent: Vec<&str> = names
            .iter()
            .filter(|n| !available.contains(*n))
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !absent.is_empty() {
            return contract(format!(
                "native history response components not selected in this problem: {}",
                implexity_core::pyobj::list_repr(&absent)
            ));
        }
        Ok(())
    }


    pub fn staged_scope(
        &self,
        problem: &Value,
        design: &NamedArrays,
        policy: &Value,
    ) -> CaeResult<ProviderScope> {
        let keys = ["coupling_approximation", "mode", "ood_policy"];
        let Some(pm) = policy.as_object().filter(|m| m.len() == 3 && keys.iter().all(|k| m.contains_key(*k)))
        else {
            return contract("staged coupling policy is malformed");
        };
        let none = || Ok(ProviderScope { value: None, guard: None });
        let mode = pm["mode"].as_str().unwrap_or_default();
        if mode == "exact" {
            return none();
        }
        let ood = pm["ood_policy"].as_str().unwrap_or_default();
        let coupling = &pm["coupling_approximation"];
        let coupling_ok = coupling.as_object().is_some_and(|m| {
            m.len() == 2 && m.contains_key("preset") && m.contains_key("lagged_coupling_ids")
        });
        if !matches!(mode, "verified_preview" | "interactive_preview")
            || !matches!(ood, "refuse" | "require_exact" | "hold_exact_anchor")
            || !coupling_ok
        {
            return contract("staged coupling policy is unsupported");
        }
        let preset = coupling["preset"].as_str().unwrap_or_default();
        let raw: Vec<String> = coupling["lagged_coupling_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        if preset == "exact" {
            return none();
        }
        let capability = Self::coupling_approximation_capability();
        let (requested, sweeps) = if preset == "explicit" {
            (raw, if mode == "verified_preview" { 2 } else { 1 })
        } else if matches!(preset, "staged" | "interactive") && raw.is_empty() {
            let row = &capability["presets"][preset];
            (
                row["lagged_coupling_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect(),
                row["sweeps"].as_u64().unwrap_or(0) as usize,
            )
        } else {
            return contract("named coupling preset cannot carry explicit coupling ids");
        };
        let unique: std::collections::BTreeSet<&String> = requested.iter().collect();
        if requested.is_empty()
            || unique.len() != requested.len()
            || requested.iter().any(|r| !STAGED_COUPLING_IDS.contains(&r.as_str()))
        {
            return contract("requested coupling is not registered as safely laggable");
        }
        let parts = self.parts(problem, design)?;
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        let k = Arc::clone(&parts.k);
        {
            let st = k.lock();
            if st.last.is_some() || st.warm.is_some() || st.pending.is_some() {
                return none();
            }
            if st.staged.is_some() {
                return contract("nested staged coupling initializers are forbidden");
            }
        }
        let schedule = |completed: bool| {
            json!([
                {"stage": "preview_initial_guess", "lagged_coupling_ids": requested, "inactive_coupling_ids": requested, "completed": completed},
                {"stage": "exact_correction", "lagged_coupling_ids": [], "inactive_coupling_ids": [], "method": "unchanged_full_grid_monolithic_solve"},
            ])
        };
        match k.staged_coupling_guess(&parts.x, sweeps, &requested) {
            Err(CaeError::Convergence(message) | CaeError::NewtonConvergence(message)) => {
                if ood == "refuse" {
                    return Err(CaeError::convergence(message));
                }
                Ok(ProviderScope {
                    value: Some(json!({
                        "schema": "implexity-optimization-preview-trace/1", "phase": "cold_staged_initializer",
                        "authoritative": false, "correction_required": true, "available": false,
                        "fallback": "exact_cold_start", "reason": message,
                        "provider_laggable_coupling_ids": STAGED_COUPLING_IDS,
                        "lagged_coupling_ids": requested, "inactive_coupling_ids": requested,
                        "restoration_schedule": schedule(false)})),
                    guard: None,
                })
            }
            Err(e) => Err(e),
            Ok((states, residuals)) => {
                k.lock().staged = Some(super::kernel::StagedGuess {
                    design_key: design_key(&parts.x),
                    states,
                    consumed: false,
                });
                Ok(ProviderScope {
                    value: Some(json!({
                        "schema": "implexity-optimization-preview-trace/1", "phase": "cold_staged_initializer",
                        "truth_status": "approximate", "authoritative": false, "correction_required": true,
                        "available": true, "provider": NAME, "preset": preset, "sweeps": sweeps,
                        "provider_laggable_coupling_ids": STAGED_COUPLING_IDS,
                        "lagged_coupling_ids": requested, "inactive_coupling_ids": requested,
                        "residual_history": residuals, "restoration_schedule": schedule(true),
                        "commit_rule": "exact_correction_only", "design_state_id": design_identity(design)?})),
                    guard: Some(Box::new(StagedGuard(k))),
                })
            }
        }
    }


    pub fn effort_scope(&self, binding: &Value) -> CaeResult<ProviderScope> {
        let capabilities = self.capabilities()?;
        let physics = implexity_core::packages::global().status()?;
        let facts = implexity_runtime::provider_job_authority::ProviderFacts {
            provider_name: NAME,
            capabilities: &capabilities,
            physics: &physics,
            candidates: None,
        };
        let (selection, checked) =
            implexity_runtime::provider_job_authority::validate_effort_binding(binding, Some(&facts))?;
        let profile = &checked["provider_profile"];
        if profile["provider_id"] != NAME
            || selection.effective.normalized_profile_digest != selection.requested.normalized_profile_digest
        {
            return contract("native exact effort binding is not content-bound to this provider");
        }
        let exact = match profile.get("exact_solver_profile").filter(|v| !v.is_null()) {
            None => {
                if profile["solver_policy"] != DIRECT_POLICY {
                    return contract("native exact effort binding omits its solver profile");
                }
                UnifiedHistoryExactProfile::direct()
            }
            Some(raw) => {
                let e = UnifiedHistoryExactProfile::from_wire(raw)?;
                if profile["solver_policy"] != json!(e.solver_policy) {
                    return contract("native exact effort solver policy and profile disagree");
                }
                e
            }
        };
        let advertised = Self::exact_computation_effort_capability();
        if !advertised["profiles"].as_array().is_some_and(|rows| rows.contains(&exact.to_wire())) {
            return contract("native exact effort profile is not an advertised server profile");
        }
        if ACTIVE_EXACT_PROFILE.with(|p| p.borrow().is_some()) {
            return contract("nested native exact computation-effort scopes are forbidden");
        }
        ACTIVE_EXACT_PROFILE.with(|p| *p.borrow_mut() = Some(exact.clone()));
        Ok(ProviderScope { value: Some(exact.to_wire()), guard: Some(Box::new(ProfileGuard)) })
    }
}

impl CaeProvider for NativeUnifiedHistoryProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let units = installed_response_units()?;
        let mut caps = LegacySingleArrayProviderCapabilities::new(
            NAME,
            ["flow", "thermal", "structure", "plasticity", "creep"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            units.iter().map(|(k, _)| k.clone()).collect(),
        );
        caps.base.fields = [
            "solid_fraction",
            "fluid_fraction",
            "temperature_K",
            "fluid_velocity_m_s",
            "solid_equivalent_plastic_strain",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        caps.base.sensitivities = true;
        caps.base.nonlinear = true;
        caps.base.design_coordinates = COORDS.iter().map(|s| (*s).to_string()).collect();
        caps.editor = json!({"kind": "native_json", "title": "Shared-domain thermofluid inelastic topology",
            "required_packages": ["unified_heat_mechanics", "solid_mechanics", "inelastic_materials", "incompressible_transport"],
            "problem_template": unified_history_starter()})
        .as_object()
        .cloned()
        .unwrap_or_default();
        caps.base.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        caps.base.response_metadata = published_response_metadata(&units)?;
        caps.base.traits = json!({
            "computation_effort": Self::exact_computation_effort_capability(),
            "coupling_approximation": Self::coupling_approximation_capability(),
            "coupling_control": Self::coupling_control_capability(),
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        Ok(ProviderCapabilities::Legacy(Box::new(caps)))
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(Self::contract().map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(Arc::new(normalise(problem)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let p = normalise(problem_value(problem)?)?;
        let transport = p["temperature_transport"].as_str().unwrap_or_default();
        let nodal = NODAL_PROFILES.contains(&transport);
        Ok(json!({"ok": true, "requires_complete_design": true, "physical_qualification": false,
            "shared_temperature_contract": {
                "volume_terms": "solid_and_fluid_T4_row_sum_lumped_nodal_dual_volume",
                "fluid_work_to_heat": "native_cell_and_edge_integration_then_T4_nodal_distribution",
                "fluid_transport": transport,
                "exact_derivatives": ["current_state", "previous_state", "all_native_design_coordinates"],
                "maximum_principle_scope": if nodal {
                    "fixed_coefficient_nodal_upwind_only"
                } else {
                    "not_claimed_for_Galerkin_Q_transpose_transport; local_material_guards_retained"
                }},
            "limitations": selected_limitations(&p)})
        .as_object()
        .cloned()
        .unwrap_or_default())
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(CaeError::contract("'NativeUnifiedHistoryProvider' object has no attribute 'evaluate'"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(CaeError::contract("'NativeUnifiedHistoryProvider' object has no attribute 'sensitivity'"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>()).cloned().unwrap_or_else(|| json!({}));
        Some(Self::declaration(&p))
    }

    fn coupling_validation(
        &self,
        problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>())?;
        let run = || -> CaeResult<Value> {
            let normalized = normalise(p)?;
            let declaration = CouplingDeclaration::from_value(&Self::declaration(&normalized)?)
                .map_err(|e| CaeError::contract(e.to_string()))?;
            let rules = implexity_core::registries::global().extensions.coupling_rules();
            Ok(validate_declaration(&declaration, &rules, for_optimization))
        };
        Some(run().map_err(|e| e.message().to_string()))
    }

    fn coupling_inventory(&self, problem: Option<&ProviderProblem>) -> Option<Value> {
        let normalized = match problem.and_then(|p| p.downcast_ref::<Value>()) {
            Some(p) => normalise(p).ok(),
            None => None,
        };
        implexity_physics_solid::coupling_inventory::report(NAME, normalized.as_ref(), false).ok()
    }

    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(Self::runtime_support_map())
    }

    fn component_slots(&self) -> Option<Map<String, Value>> {
        Some(Self::component_slots_map())
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for NativeUnifiedHistoryProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::AcceptDesign
                | DesignOp::InstallMatchingTimeGuess
                | DesignOp::ExportMatchingTimeGuess
        )
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _operating_point: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(problem_value(problem)?, design)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        self.preflight_named(problem_value(problem)?, design)
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _operating_point: usize,
    ) -> CaeResult<DesignSensitivity> {
        let mut out = self.sensitivities_named(problem_value(problem)?, design, &[response.to_string()])?;
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
        _operating_point: usize,
    ) -> CaeResult<DesignSensitivities> {
        self.sensitivities_named(problem_value(problem)?, design, responses)
    }

    fn accept_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Option<Map<String, Value>>> {
        let parts = self.parts(problem_value(problem)?, design)?;
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        if self.split_selected(&parts) { return self.split_commit(&parts, design).map(Some); }
        let operation = parts.k.new_operation_context("accept_design", true)?;
        parts.k.promote_accepted(&parts.x, Some(&operation))?;
        Ok(json!({"design_state_id": design_identity(design)?}).as_object().cloned())
    }

    fn install_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        guess: &MatchingTimeNewtonGuess,
    ) -> CaeResult<Map<String, Value>> {
        let parts = self.parts(problem_value(problem)?, design)?;
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        if self.split_selected(&parts) { return self.split_install(&parts, design, guess); }
        let operation = parts.k.new_operation_context("install_matching_time_guess", false)?;
        let states: Vec<Vec<f64>> = guess.states().iter().map(|s| s.to_vec()).collect();
        parts.k.install_matching_time_guess(
            &parts.x,
            &states,
            &Value::Object(guess.provider_identity().clone()),
            Some(&operation),
        )?;
        Ok(json!({"design_state_id": design_identity(design)?, "installed_as": "matching_time_newton_guess_only",
            "canonical_cache_admission": false})
        .as_object()
        .cloned()
        .unwrap_or_default())
    }

    fn export_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        require_accepted: bool,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        let parts = self.parts(problem_value(problem)?, design)?;
        let kernel = Arc::clone(&parts.k);
        let _operation = kernel.exclusive();
        if self.split_selected(&parts) { return self.split_export(&parts, design, require_accepted); }
        let operation = parts.k.new_operation_context("export_matching_time_guess", true)?;
        let (states, identity, provenance) =
            parts.k.export_matching_time_guess(&parts.x, require_accepted, Some(&operation))?;
        MatchingTimeNewtonGuess::new(
            states,
            identity.as_object().cloned().unwrap_or_default(),
            provenance.as_object().cloned().unwrap_or_default(),
        )
    }

    fn validate_response_selection(
        &self,
        problem: &ProviderProblem,
        names: &[String],
    ) -> Option<CaeResult<()>> {
        Some(problem_value(problem).and_then(|p| Self::validate_responses(p, names)))
    }

    fn coupling_control(&self, _problem: Option<&Value>) -> Option<CaeResult<Value>> {
        Some(Ok(Self::coupling_control_capability()))
    }

    fn preflight_effects(&self, problem: &Value) -> Option<CaeResult<Value>> {
        let mut effects = vec!["geometry_evaluation", "constitutive_evaluation"];
        if problem.get("initialization").is_some_and(super::truthy) {
            effects.push("initial_equilibrium_solve");
        }
        Some(Ok(json!({"status": "declared", "possible_effects": effects})))
    }

    fn cached_evaluation_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        operating_point: usize,
    ) -> Option<CaeResult<CachedEvaluation>> {
        Some(problem_value(problem).and_then(|p| self.cached_evaluation(p, design, operating_point)))
    }

    fn has_computation_effort_scope(&self) -> bool {
        true
    }

    fn computation_effort_scope(&self, binding: &Value) -> Option<CaeResult<ProviderScope>> {
        Some(self.effort_scope(binding))
    }

    fn staged_initial_guess_scope(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        policy: &Value,
    ) -> Option<CaeResult<ProviderScope>> {
        Some(problem_value(problem).and_then(|p| self.staged_scope(p, design, policy)))
    }

    fn workspace_declaration(&self, name: &str, problem: Option<&Value>) -> Option<CaeResult<Value>> {
        match name {
            "editor_schema" => Some(Ok(Self::editor_schema(problem))),
            "study_templates" => Some(Ok(Value::Array(Self::study_templates(problem)))),
            _ => None,
        }
    }

    fn exact_computation_effort_candidate_profiles(&self) -> Option<CaeResult<Value>> {
        Some(Ok(Self::exact_computation_effort_capability()))
    }
}
