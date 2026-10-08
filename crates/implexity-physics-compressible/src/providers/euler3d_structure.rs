// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, Evaluation, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge};
use implexity_core::orchestration::PublishedContract;
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::errors::PResult;
use crate::pyval::strs;
use crate::providers::quasi1d_euler::{evaluation_contract, no_design, presentation};

#[derive(Debug, Clone, Copy, Default)]
pub struct EulerStructureProvider;

impl EulerStructureProvider {

    pub fn template() -> CaeResult<Value> {
        let mesh = implexity_physics_solid::solid_history::mesh([1, 1, 1])?;
        let h = [0.2, 0.3, 0.4];
        #[allow(clippy::cast_precision_loss)]
        let nodes: Vec<[f64; 3]> =
            mesh.ijk.iter().map(|n| std::array::from_fn(|a| (n[a] as f64 + 1.0) * h[a])).collect();
        let mask: Vec<Vec<Vec<bool>>> = (0..3)
            .map(|i| (0..3).map(|j| (0..3).map(|k| !(i == 1 && j == 1 && k == 1)).collect()).collect())
            .collect();
        let mut flow = crate::providers::euler3d::Euler3DProvider::template();
        flow["shape"] = json!([3, 3, 3]);
        flow["spacing_m"] = json!(h);
        flow["fluid_mask"] = json!(mask);
        flow["initial_primitive"] = json!([1.0, 0.1, 0.0, 0.0, 1.0]);
        flow["gas_constant_J_kgK"] = json!(1.0);
        flow["end_time_s"] = json!(0.008);
        let mut boundaries = Map::new();
        for a in ["x", "y", "z"] {
            for s in ["min", "max"] {
                boundaries.insert(format!("{a}{s}"), json!({"kind": "transmissive"}));
            }
        }
        flow["boundaries"] = Value::Object(boundaries);
        #[allow(clippy::float_cmp)]
        let fixed: Vec<[bool; 3]> = nodes.iter().map(|n| [n[2] == h[2]; 3]).collect();
        let ne = mesh.tets.len();
        let zeros = vec![[0.0; 3]; nodes.len()];
        Ok(
            json!({"flow": flow, "time_integration": {"step_s": 0.001, "step_count": 8, "solid_substeps": 2, "flow_scheme": "first_order",
                    "flow_history_byte_budget": 1_000_000, "solid_history_byte_budget": 1_000_000},
                  "solid": {"nodes_m": nodes, "tetrahedra": mesh.tets, "density_kg_m3": vec![1000.0; ne], "young_Pa": vec![1e4; ne],
                    "poisson": vec![0.25; ne], "fixed_dofs": fixed, "initial_displacement_m": zeros, "initial_velocity_m_s": zeros,
                    "maximum_strain": 0.01},
                  "provenance": "Synthetic clamped solid voxel in a gas; not calibrated material data"}),
        )
    }


    pub fn capabilities_value() -> CaeResult<ProviderCapabilities> {
        let mut d = ProviderDescriptor::new(
            NAME,
            vec!["one_way_flow_structural_history".into()],
            RESPONSE_UNITS.iter().map(|(k, _)| (*k).to_string()).collect(),
        );
        d.fields = [
            "displacement_history_m",
            "stress_history_Pa",
            "nodal_force_history_N",
            "time_s",
            "peak_displacement_history_m",
            "peak_von_mises_history_Pa",
            "peak_strain_history",
            "interval_start_support_reactions_N",
            "interval_end_support_reactions_N",
            "interval_nodal_forces_N",
            "fatigue_usage_per_element",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        d.sensitivities = false;
        d.design_coordinates = Vec::new();
        d.traits = json!({"evaluation_only": true, "experimental": true,
            "history_preview": {"kind": "scalar_time_histories", "time_field": "time_s", "series": [
                {"key": "peak_displacement_history_m", "label": "Maximum displacement magnitude", "unit": "m"},
                {"key": "peak_von_mises_history_Pa", "label": "Maximum von Mises stress", "unit": "Pa"},
                {"key": "peak_strain_history", "label": "Maximum strain norm", "unit": "1"}]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
        d.response_metadata = RESPONSE_UNITS
            .iter()
            .map(|(k, u)| {
                ((*k).to_string(), json!({"unit": u, "differentiable": false, "design_reachable": false}))
            })
            .collect();
        d.notes = LIMITS.iter().map(|s| (*s).to_string()).collect();
        let editor = json!({"kind": "native_json", "title": "3-D flow \u{2192} structural response \u{2014} one-way",
            "problem_template": Self::template()?, "schema": {"type": "object", "properties": {
                "flow": {"title": "Fixed-wall Euler flow", "format": "json", "description": LIMITS[3]},
                "solid": {"title": "Conforming solid mesh and material", "format": "json", "description": LIMITS[1]},
                "fatigue": {"title": "Optional calibrated fatigue usage", "format": "json",
                    "description": "Omit to disable. Requires provenance, stress_measure (normal_x/y/z or signed_von_mises_hydrostatic), increasing amplitudes_Pa and mean_stresses_Pa, cycles_to_failure table (mean rows, amplitude columns), solid_temperature_K, T_min_K, T_max_K and initial_usage. Open-history rainflow includes residual half cycles. No extrapolation, automatic mean correction or stiffness damage."},
                "time_integration": {"title": "Time steps and history memory", "type": "object", "description": LIMITS[2],
                    "properties": {
                        "flow_scheme": {"title": "CFD numerical scheme", "type": "string", "enum": ["first_order", "muscl_ssprk2"], "description": "First order is the original method. MUSCL/SSPRK2 reduces numerical diffusion; validate spatial and temporal convergence for your case."},
                        "step_s": {"title": "CFD time step", "unit": "s", "type": "number", "exclusiveMinimum": 0, "description": "Time step \u{d7} step count must equal the flow end time. Each stage is checked against CFL and positivity limits."},
                        "step_count": {"title": "CFD step count", "type": "integer", "minimum": 1, "maximum": 1_000_000},
                        "solid_substeps": {"title": "Structural substeps per CFD step", "type": "integer", "minimum": 1, "maximum": 100, "description": "Refines structural integration only; the applied CFD force remains constant within each CFD interval."},
                        "flow_history_byte_budget": {"title": "CFD state-history budget", "unit": "bytes", "type": "integer", "minimum": 1, "description": "Budget for returned flow states, not total solver or differentiation working memory."},
                        "solid_history_byte_budget": {"title": "Structural history budget", "unit": "bytes", "type": "integer", "minimum": 1}}}}}});
        presentation(&d, editor, "array")
    }


    pub fn declaration(problem: Option<&Value>) -> CaeResult<Value> {
        let edge = |s: &str, t: &str, q: &str, reason: &str| {
            CouplingEdge::new(s, t, q, "one_way", true, reason).map_err(|e| CaeError::contract(e.0))
        };
        let mut physics = vec!["flow".to_string(), "structure".to_string()];
        let mut edges = vec![edge(
            "flow",
            "structure",
            "fixed_wall_force_history",
            "Conforming nodal forces, interval-constant flow loads; no displacement feedback",
        )?];
        if problem.is_some_and(|p| p.get("fatigue").is_some()) {
            physics.push("fatigue".into());
            edges.push(edge(
                "structure",
                "fatigue",
                "stress_history",
                "Calibrated scalar rainflow usage from solved stress; no constitutive feedback",
            )?);
        }
        Ok(CouplingDeclaration {
            provider: NAME.into(),
            active_physics: physics,
            edges,
            notes: LIMITS.iter().map(|s| (*s).to_string()).collect(),
            ..CouplingDeclaration::default()
        }
        .to_value())
    }


    #[allow(clippy::too_many_lines)]
    pub fn evaluate_value(problem: &Value) -> PResult<Evaluation> { crate::euler3d_structure::evaluate_value(problem).map(|mut v|{v.provider=NAME.into();v}) }
}

fn value_of(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("verification providers require their own problem mapping"))
}

impl CaeProvider for EulerStructureProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn provider_id(&self) -> Option<&str> {
        Some(NAME)
    }

    fn implementation(&self) -> &'static str {
        "implexity.compressible.euler3d_structure.EulerStructureProvider"
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        Self::capabilities_value()
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(
            evaluation_contract(NAME, &RESPONSE_UNITS, &LIMITS)
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        prepare(problem)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        no_design(topology)?;
        prepare(value_of(problem)?)?;
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("optimization_supported".into(), json!(false));
        m.insert("limitations".into(), strs(&LIMITS));
        m.insert(
            "trajectory_admission".into(),
            json!("CFL, positivity and structural strain checked during evaluation"),
        );
        Ok(m)
    }

    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        no_design(Some(topology))?;
        Ok(Self::evaluate_value(value_of(problem)?)?)
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(CaeError::contract("'EulerStructureProvider' object has no attribute 'sensitivity'"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let value = match problem.map(value_of).transpose() {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        Some(Self::declaration(value))
    }

    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl implexity_optim::provider_ops::DesignOperations for EulerStructureProvider {
    fn provides(&self, op: implexity_optim::provider_ops::DesignOp) -> bool {
        matches!(
            op,
            implexity_optim::provider_ops::DesignOp::Evaluate
                | implexity_optim::provider_ops::DesignOp::EvaluateWithoutDesign
        )
    }

    fn evaluate_without_design(&self, problem: &ProviderProblem) -> CaeResult<Evaluation> {
        Ok(Self::evaluate_value(value_of(problem)?)?)
    }
}

pub use crate::euler3d_structure::{LIMITS,RESPONSE_UNITS,Prepared};

pub fn prepare(problem:&Value)->PResult<Prepared>{crate::euler3d_structure::prepare(problem).map_err(Into::into)}

pub const NAME: &str = "compressible_cartesian_euler3d_structure";
