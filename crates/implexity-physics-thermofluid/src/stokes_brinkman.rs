// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, LegacySingleArrayProviderCapabilities, ProviderCapabilities,
    ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::{DesignOp, DesignOperations, design_interface};
use implexity_physics_cfd::CfdProblem;
use implexity_physics_cfd::preflight::{port_mask, run_preflight};
use implexity_physics_cfd::resolved_stokes::{
    QUALIFIED_RESPONSES, ResolvedStokesBrinkmanBackend, selected_faces,
};
use implexity_physics_cfd::workspace_contract::{from_mapping, is_open_kind, port_responses};

pub const NAME: &str = "resolved_stokes_brinkman";
pub const IMPLEMENTATION: &str = "implexity.physics_library.stokes_brinkman.ResolvedStokesProvider";

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolvedStokesProvider {
    pub backend: ResolvedStokesBrinkmanBackend,
}

fn problem_of(problem: &ProviderProblem) -> CaeResult<&CfdProblem> {
    problem.downcast_ref::<CfdProblem>().ok_or_else(|| {
        CaeError::contract("resolved_stokes_brinkman: problem was not normalised by this provider")
    })
}

fn topology_3d(p: &CfdProblem, topology: &ArrayD<f64>) -> CaeResult<ArrayD<f64>> {
    let cells = p.domain.cells;
    if topology.shape() != cells.as_slice() {
        let parts: Vec<String> = topology.shape().iter().map(ToString::to_string).collect();
        let shown =
            if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) };
        return Err(CaeError::contract(format!(
            "topology shape {shown} does not match CFD cells {}",
            p.domain.cells_repr()
        )));
    }
    Ok(topology.clone())
}

fn field_metadata() -> Value {
    json!({
        "u_m_s": {"units": "m/s", "association": "face_x", "rank": "scalar", "vector": "velocity_m_s"},
        "v_m_s": {"units": "m/s", "association": "face_y", "rank": "scalar", "vector": "velocity_m_s"},
        "w_m_s": {"units": "m/s", "association": "face_z", "rank": "scalar", "vector": "velocity_m_s"},
        "pressure_Pa": {"units": "Pa", "association": "cell", "rank": "scalar"},
        "divergence_s-1": {"units": "1/s", "association": "cell", "rank": "scalar"},
    })
}

impl ResolvedStokesProvider {
    #[must_use]
    pub fn legacy_capabilities() -> LegacySingleArrayProviderCapabilities {
        let ports = port_responses();
        let mut responses: Vec<String> = QUALIFIED_RESPONSES.iter().map(|s| (*s).to_string()).collect();
        responses.extend(ports.iter().map(|(n, _, _)| n.clone()));
        let mut caps =
            LegacySingleArrayProviderCapabilities::new(NAME, vec!["stokes_brinkman".into()], responses);
        let mut metadata = Map::new();
        for (name, face, quantity) in &ports {
            let unit = match *quantity {
                "volume_flow" => "m³/s",
                "mass_flow" => "kg/s",
                _ => "m/s",
            };
            let mut label = quantity.replace('_', " ");
            if let Some(first) = label.get(..1) {
                label = format!("{}{}", first.to_uppercase(), &label[1..]);
            }
            metadata.insert(
                name.clone(),
                json!({
                    "label": format!("{label} · {face}"),
                    "unit": unit,
                    "description": "Signed outward response on this whole domain face. Requires an enabled open boundary; negative means inflow. Density and face area are fixed.",
                    "family": "flow",
                    "differentiable": true,
                }),
            );
        }
        metadata.insert(
            "pressure_drop".into(),
            json!({"label": "Pressure drop", "unit": "Pa",
                   "description": "Mean inlet pressure minus mean outlet pressure on the selected domain faces.",
                   "family": "flow", "differentiable": true}),
        );
        metadata.insert(
            "volume_flow".into(),
            json!({"label": "Inlet volume flow", "unit": "m³/s",
                   "description": "Signed volume flow through the selected inlet face; positive into the domain.",
                   "family": "flow", "differentiable": true}),
        );
        metadata.insert(
            "pumping_power".into(),
            json!({"label": "Pumping power", "unit": "W",
                   "description": "Selected inlet-to-outlet pressure drop multiplied by signed inlet volume flow. This is not a sum over all ports.",
                   "family": "flow", "differentiable": true}),
        );
        caps.base.response_metadata = metadata;
        caps.base.fields =
            ["u_m_s", "v_m_s", "w_m_s", "pressure_Pa", "divergence_s-1"].map(String::from).to_vec();
        caps.base.sensitivities = true;
        caps.execution = "array".into();
        caps.editor = json!({"kind": "embedded_cfd", "global": "ImplexityCFDWorkspace"})
            .as_object()
            .cloned()
            .unwrap_or_default();
        caps.condition_types = [
            "no_slip_wall",
            "moving_wall",
            "prescribed_velocity",
            "prescribed_volumetric_flow",
            "prescribed_mass_flow",
            "static_pressure",
            "symmetry",
            "traction_outlet",
        ]
        .map(String::from)
        .to_vec();
        caps.material_model = Some("newtonian_fluid".into());
        caps.base.design_coordinates = vec!["model:control".into()];
        caps.compatibility_routes = vec!["/v1/implicit/cfd/*".into()];
        caps.base.notes = vec![
            "3-D conservative MAC discretisation".into(),
            "exact discrete-adjoint topology gradient".into(),
            "Open boundaries use component-Laplacian Stokes with zero normal viscous derivative, not general full symmetric-stress traction.".into(),
        ];
        caps
    }


    pub fn normalise(problem: &Value) -> CaeResult<CfdProblem> {
        let raw = if problem.get("schema").and_then(Value::as_str) == Some("implexity-implicit-cae/1") {
            match problem.get("physics").and_then(|p| p.get("problem")) {
                Some(v) if !v.is_null() && v.as_object().is_some_and(|m| !m.is_empty()) => v.clone(),
                _ => json!({}),
            }
        } else {
            problem.clone()
        };
        from_mapping(&raw).map_err(CaeError::from)
    }


    pub fn required_topology_registration(p: &CfdProblem) -> CaeResult<Value> {
        let lo: [f64; 3] = p.domain.origin_m.map(|v| v * 1000.0);
        let hi: [f64; 3] = std::array::from_fn(|a| lo[a] + p.domain.extent_m[a] * 1000.0);
        let reg =
            implexity_geometry::field_registration::axis_aligned_registration(p.domain.cells, lo, hi, "cell")
                .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(json!({"units": "mm", "registration": reg.to_wire()}))
    }

    #[must_use]
    pub fn required_topology_semantics(_p: &CfdProblem) -> Value {
        json!({"coordinate": "model:control", "inside": "greater", "isovalue": 0.5})
    }

    #[must_use]
    pub fn evaluation_responses(p: &CfdProblem) -> Vec<String> {
        let mut requested: Vec<String> = Vec::new();
        for o in &p.objectives {
            if !requested.contains(&o.response) {
                requested.push(o.response.clone());
            }
        }
        let open_faces: Vec<&str> =
            p.enabled_boundaries().filter(|b| is_open_kind(&b.kind)).map(|b| b.face.as_str()).collect();
        for (name, face, _) in port_responses() {
            if open_faces.contains(&face) && !requested.contains(&name) {
                requested.push(name);
            }
        }
        for name in QUALIFIED_RESPONSES {
            if requested.iter().any(|r| r == name) {
                continue;
            }
            if selected_faces(p, None).is_ok() {
                requested.push(name.to_string());
            }
        }
        requested
    }


    pub fn project(p: &CfdProblem, topology: &ArrayD<f64>) -> CaeResult<ArrayD<f64>> {
        let mut x = topology_3d(p, topology)?;
        for bc in &p.boundaries {
            if bc.enabled && is_open_kind(&bc.kind) && bc.port_buffer_cells > 0 {
                let mask = port_mask(p.domain.cells, &bc.face, bc.port_buffer_cells);
                for (v, m) in x.iter_mut().zip(mask.iter()) {
                    if *m {
                        *v = 0.0;
                    }
                }
            }
        }
        Ok(x)
    }

    fn evaluate_problem(self, p: &CfdProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        let responses = Self::evaluation_responses(p);
        let out = self.backend.solve(p, Some(topology.view()), &responses).map_err(CaeError::from)?;
        let mut diagnostics = out.diagnostics.clone();
        diagnostics.insert("field_metadata".into(), field_metadata());
        let mut fields = BTreeMap::new();
        for (name, a) in [
            ("u_m_s", &out.u_m_s),
            ("v_m_s", &out.v_m_s),
            ("w_m_s", &out.w_m_s),
            ("pressure_Pa", &out.pressure_pa),
            ("divergence_s-1", &out.divergence_s_1),
        ] {
            fields.insert(name.to_string(), FieldValue::Array(a.clone().into_dyn()));
        }
        Ok(Evaluation {
            provider: NAME.into(),
            responses: out.responses.iter().cloned().collect(),
            diagnostics,
            fields,
        })
    }

    fn sensitivity_problem(
        self,
        p: &CfdProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        let out = self
            .backend
            .solve_and_adjoint(p, Some(topology.view()), Some(response))
            .map_err(CaeError::from)?;
        let gradient = nested_to_array(&out["gradient"], &p.domain.cells)?;
        Ok(Sensitivity {
            provider: NAME.into(),
            response: out["response"].as_str().unwrap_or(response).to_string(),
            value: out["value"].as_f64().unwrap_or(f64::NAN),
            gradient,
            diagnostics: out["diagnostics"].as_object().cloned().unwrap_or_default(),
        })
    }
}

fn nested_to_array(v: &Value, shape: &[usize; 3]) -> CaeResult<ArrayD<f64>> {
    let values = implexity_core::parity::json_f64s(v).map_err(CaeError::contract)?;
    ArrayD::from_shape_vec(IxDyn(shape), values).map_err(|e| CaeError::contract(e.to_string()))
}

impl CaeProvider for ResolvedStokesProvider {
    fn published_schemas(&self) -> &'static [implexity_core::schemas::SchemaDescriptor] {
        &implexity_physics_cfd::schemas::SCHEMAS
    }

    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        Ok(ProviderCapabilities::Legacy(Box::new(Self::legacy_capabilities().checked()?)))
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(Arc::new(Self::normalise(problem)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let p = problem_of(problem)?;
        let report = run_preflight(p, topology.map(|t| t.view())).as_dict();
        Ok(report.as_object().cloned().unwrap_or_default())
    }

    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        self.evaluate_problem(problem_of(problem)?, topology)
    }

    fn sensitivity(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        self.sensitivity_problem(problem_of(problem)?, topology, response)
    }

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        Some(Ok(CouplingDeclaration {
            provider: NAME.into(),
            active_physics: vec!["flow".into()],
            ports: Vec::new(),
            edges: Vec::new(),
            closed_loops: Vec::new(),
            intentionally_frozen: Vec::new(),
            notes: vec!["resolved Stokes-Brinkman provider declares an isolated flow solve".into()],
        }
        .to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for ResolvedStokesProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::Evaluate
                | DesignOp::Sensitivity
                | DesignOp::ProjectTopology
                | DesignOp::SensitivityMany
        )
    }

    fn project_topology(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        _reference: &ArrayD<f64>,
    ) -> CaeResult<ArrayD<f64>> {
        Self::project(problem_of(problem)?, topology)
    }

    fn sensitivity_many(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        responses: &[String],
        _operating_point: usize,
    ) -> CaeResult<BTreeMap<String, Sensitivity>> {
        let p = problem_of(problem)?;
        let out =
            self.backend.solve_and_adjoints(p, Some(topology.view()), responses).map_err(CaeError::from)?;
        let mut result = BTreeMap::new();
        for (name, item) in out.responses {
            let mut diag = out.diagnostics.clone();
            diag.insert("adjoint_relative_residual".into(), json!(item.adjoint_relative_residual));
            result.insert(
                name.clone(),
                Sensitivity {
                    provider: NAME.into(),
                    response: name,
                    value: item.value,
                    gradient: item.gradient.into_dyn(),
                    diagnostics: diag,
                },
            );
        }
        Ok(result)
    }
}
