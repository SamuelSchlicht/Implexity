// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::design_ops::{
    Design, DesignSensitivities, LbmOperations, array, check_responses, presentation, single_coordinate,
};
use crate::geometry;
use crate::nparray::{to_nested, to_nested_bool};
use crate::solver::{History, LbmProblem, Tau, VECTOR_LEN};
use crate::wall_exchange;

pub const NAME: &str = "lattice_boltzmann_d3q19";
pub const COORDINATE: &str = "model:control";
pub const RESPONSES: [&str; VECTOR_LEN] = [
    "mean_velocity_x_m_s",
    "mean_velocity_y_m_s",
    "mean_velocity_z_m_s",
    "kinetic_energy_J",
    "porous_dissipation_W",
    "fluid_volume_m3",
];
pub const UNITS: [&str; VECTOR_LEN] = ["m/s", "m/s", "m/s", "J", "W", "m^3"];

pub const NOTES: [&str; 5] = [
    "Independent implementation; no FluidX3D code or dependency.",
    "Isothermal, laminar, low-Mach fixed-horizon model. No steady-state certification.",
    "Planar pressure/velocity ports or periodic driving; fixed no-slip walls elsewhere. Port corners with other closed faces are unsupported.",
    "Explicit grid design required; CAD is not automatically voxelized. Zero design means finite porous resistance, not an impermeable wall.",
    "No thermal, moving-wall, turbulence or structural coupling is declared.",
];

pub const TEMPLATE_SHAPE: [usize; 3] = [4, 6, 4];

#[must_use]
pub fn problem_template() -> Value {
    let n: usize = TEMPLATE_SHAPE.iter().product();
    json!({
        "shape": TEMPLATE_SHAPE, "spacing_m": 0.001, "step_s": 0.1, "step_count": 10,
        "origin_m": [0.0, 0.0, 0.0],
        "density_kg_m3": 1000.0, "kinematic_viscosity_m2_s": 1e-6,
        "acceleration_m_s2": [1e-5, 0.0, 0.0], "periodic_axes": [true, false, true],
        "solid_mask": to_nested_bool(&vec![false; n], &TEMPLATE_SHAPE),
        "design_region": to_nested_bool(&vec![true; n], &TEMPLATE_SHAPE),
        "fixed_design": to_nested(&vec![1.0; n], &TEMPLATE_SHAPE), "drag_max_per_s": 10.0,
        "drag_shape": 0.1, "mach_limit": 0.1, "history_byte_budget": 16_000_000, "ports": [],
        "topology_map": {"filter_radius_m": 0.0015, "projection_beta": 2.0, "projection_eta": 0.5},
    })
}

#[must_use]
pub fn design_template(shape: &[usize]) -> Value {
    let n: usize = shape.iter().product();
    json!({COORDINATE: {
        "value": to_nested(&vec![1.0; n], shape), "lower": 0.0, "upper": 1.0,
        "designable": to_nested_bool(&vec![true; n], shape),
    }})
}

fn schema_properties() -> Value {
    json!({
        "geometry_source": {"title": "Optional implicit geometry region source", "format": "json",
            "description": "Inline native document plus solid_nodes, design_nodes and protected_fluid_nodes lists. Set all three mask/fixed_design inputs to null. Samples cell centres, negative field inside, native geometry in mm. Fixed solids and protected fluid are never designable. Geometry is fixed during optimization."},
        "origin_m": {"title": "Grid lower corner [x,y,z]", "format": "json", "description": "Physical coordinates in metres. Cells are centred half a spacing above this origin."},
        "ports": {"title": "Optional pressure/velocity ports", "format": "json",
            "description": "List of id, kind (velocity/pressure), face, full-grid boolean mask and velocity_m_s. Pressure adds gauge_pressure_Pa and requires zero normal input velocity. Optional reconstruction: legacy_moment (default), experimental regularized_known_stress, or experimental neighbor_non_equilibrium (pressure only; protected fluid inward neighbor, not another port). Both experimental modes replace all19 populations, require zero acceleration and require accuracy validation. Cells must be protected fluid; no periodic faces, overlaps or closed-face corners. Empty list preserves closed/periodic flow."},
        "topology_map": {"title": "Feature-scale filter and smooth projection", "format": "json",
            "description": "filter_radius_m up to four cells; projection_beta 0\u{2013}32; projection_eta in (0,1). Filtering uses editable neighbors only, with periodic wrapping only on periodic axes. Protected cells stay exact."},
        "shape": {"title": "Uniform grid dimensions [x,y,z]", "format": "json"},
        "spacing_m": {"title": "Cubic cell width", "type": "number", "unit": "m"},
        "step_s": {"title": "Fixed lattice time step", "type": "number", "unit": "s"},
        "step_count": {"title": "Fixed transient step count", "type": "integer"},
        "density_kg_m3": {"title": "Reference density", "type": "number", "unit": "kg/m^3"},
        "kinematic_viscosity_m2_s": {"title": "Kinematic viscosity", "type": "number", "unit": "m^2/s"},
        "acceleration_m_s2": {"title": "Driving acceleration [x,y,z]", "format": "json"},
        "periodic_axes": {"title": "Periodic axes [x,y,z]; false = no-slip walls", "format": "json"},
        "solid_mask": {"title": "Fixed voxel obstacles (true = solid)", "format": "json"},
        "design_region": {"title": "Editable porous-design cells", "format": "json"},
        "fixed_design": {"title": "Protected fluid fractions [0,1]", "format": "json"},
        "drag_max_per_s": {"title": "Maximum porous resistance", "type": "number", "unit": "1/s"},
        "drag_shape": {"title": "Porous interpolation parameter", "type": "number"},
        "mach_limit": {"title": "Trajectory Mach limit (at most 0.2)", "type": "number"},
        "history_byte_budget": {"title": "Population history budget (additional AD memory required)", "type": "integer", "unit": "bytes"},
    })
}

#[must_use]
pub fn editor() -> Value {
    json!({
        "kind": "native_json",
        "title": "Lattice Boltzmann \u{2014} low-Mach viscous flow (experimental)",
        "problem_template": problem_template(),
        "schema": {"type": "object", "properties": schema_properties()},
        "design_template": design_template(&TEMPLATE_SHAPE),
    })
}

#[must_use]
pub fn response_metadata(names: &[&str], units: &[&str]) -> Map<String, Value> {
    names
        .iter()
        .zip(units)
        .map(|(n, u)| {
            ((*n).to_string(), json!({"unit": u, "differentiable": true, "design_reachable": true}))
        })
        .collect()
}

#[must_use]
pub fn descriptor(
    name: &str,
    analyses: &[&str],
    responses: &[&str],
    units: &[&str],
    fields: &[&str],
    notes: &[&str],
) -> ProviderDescriptor {
    let mut d = ProviderDescriptor::new(
        name,
        analyses.iter().map(|s| (*s).to_string()).collect(),
        responses.iter().map(|s| (*s).to_string()).collect(),
    );
    d.fields = fields.iter().map(|s| (*s).to_string()).collect();
    d.sensitivities = true;
    d.design_coordinates = vec![COORDINATE.to_string()];
    let mut traits = Map::new();
    traits.insert("experimental".into(), json!(true));
    traits.insert("requires_explicit_design".into(), json!(true));
    d.traits = traits;
    d.response_metadata = response_metadata(responses, units);
    d.notes = notes.iter().map(|s| (*s).to_string()).collect();
    d
}


pub fn contract(
    name: &str,
    responses: &[&str],
    units: &[&str],
    scope: &[&str],
    notes: &[String],
) -> CaeResult<AddInContract> {
    let port = format!("{name}.design.0");
    let mut c = AddInContract::new(name);
    c.category = AddInCategory::Field;
    c.responses = responses
        .iter()
        .zip(units)
        .map(|(n, u)| {
            let mut r = ResponseCapability::new(*n);
            r.unit = (*u).to_string();
            r.differentiable = Some(true);
            r.design_reachable = Some(true);
            r.depends_on = vec![port.clone()];
            r
        })
        .collect();
    c.scope = scope.iter().map(|s| (*s).to_string()).collect();
    c.fidelity = Fidelity::Screening;
    c.priority = 50;
    c.runtime_route = RuntimeRoute::Array;
    c.exact_design_derivatives = Some(true);
    c.exact_state_transpose = Some(true);
    c.notes = notes.to_vec();
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = format!("provider:{name}");
    c.execution_kind = Some(ExecutionKind::Provider);
    c.supported_operations =
        ["preflight", "preflight_design", "evaluate", "sensitivity", "sensitivities", "optimize"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    c.no_op_operations = Vec::new();
    let mut input = DesignCoordinateRef::new(COORDINATE, port);
    input.addin_id = name.to_string();
    c.design_inputs = vec![input];
    c.checked()
}


pub fn problem_value(problem: &ProviderProblem) -> CaeResult<&Value> {
    crate::design_ops::problem_value(problem)
}

#[must_use]
pub fn missing_method(class: &str, method: &str) -> CaeError {
    CaeError::contract(format!("'{class}' object has no attribute '{method}'"))
}

#[derive(Clone, Debug)]
pub struct Solved {
    pub p: LbmProblem,
    pub raw: Vec<f64>,
    pub phi: Vec<f64>,
    pub alpha: Vec<f64>,
    pub history: History,
    pub diagnostics: Map<String, Value>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LatticeBoltzmannProvider;

impl LatticeBoltzmannProvider {
    pub const IMPLEMENTATION: &'static str = "implexity.lbm.provider.LatticeBoltzmannProvider";


    pub fn parts(&self, problem: &Value, design: &Design) -> CaeResult<Solved> {
        let raw = single_coordinate(design, COORDINATE, "LBM requires exactly model:control")?;
        let p = LbmProblem::normalize(problem)?;
        let raw = p.checked_design(raw)?;
        let phi = p.fraction(&raw);
        let alpha = p.resistance(&phi);
        let history = p.history(&alpha, Tau::Uniform(p.tau));
        let mut diagnostics = p.admit(&alpha, &history, false)?;
        diagnostics
            .insert("field_metadata".into(), geometry::field_metadata(p.shape(), p.spacing_m, p.origin_m)?);
        if let Some(s) = &p.geometry_sampling {
            diagnostics.insert("geometry_sampling".into(), s.clone());
        }
        Ok(Solved { p, raw, phi, alpha, history, diagnostics })
    }
}


pub fn flow_fields(s: &Solved) -> CaeResult<BTreeMap<String, FieldValue>> {
    let p = &s.p;
    let shape = p.shape().to_vec();
    let final_state = s.history.states.last().map_or(&[][..], Vec::as_slice);
    let (rho, u) = p.fields(&s.alpha, final_state);
    let unit = (p.spacing_m / p.step_s).powi(2) / 3.0;
    let pressure: Vec<f64> = rho.iter().map(|r| (r - p.density_kg_m3) * unit).collect();
    let mut vshape = shape.clone();
    vshape.push(3);
    let mut fields = BTreeMap::new();
    fields.insert("fluid_fraction".into(), FieldValue::Array(array(s.phi.clone(), &shape)?));
    fields.insert("density_kg_m3".into(), FieldValue::Array(array(rho, &shape)?));
    fields.insert("velocity_m_s".into(), FieldValue::Array(array(u, &vshape)?));
    fields.insert("gauge_pressure_Pa".into(), FieldValue::Array(array(pressure, &shape)?));
    Ok(fields)
}


pub fn insert_wall_fields(
    fields: &mut BTreeMap<String, FieldValue>,
    loads: &wall_exchange::Loads,
    force_name: &str,
) -> CaeResult<()> {
    let n = loads.positions_m.len();
    if n == 0 {
        return Ok(());
    }
    let flat = |v: &[[f64; 3]]| v.iter().flatten().copied().collect::<Vec<f64>>();
    fields
        .insert("wall_link_positions_m".into(), FieldValue::Array(array(flat(&loads.positions_m), &[n, 3])?));
    fields.insert(force_name.into(), FieldValue::Array(array(flat(&loads.force_n), &[n, 3])?));
    Ok(())
}

impl LbmOperations for LatticeBoltzmannProvider {
    fn preflight_value(&self, problem: &Value, design: &Design) -> CaeResult<Map<String, Value>> {
        let s = self.parts(problem, design)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.extend(s.diagnostics);
        Ok(out)
    }

    fn evaluate_value(&self, problem: &Value, design: &Design) -> CaeResult<Evaluation> {
        let mut s = self.parts(problem, design)?;
        let p = &s.p;
        let states = &s.history.states;
        let before = &states[states.len() - 2];
        let wall = wall_exchange::loads(p, &s.alpha, before, 0.0, Tau::Uniform(p.tau));
        s.diagnostics.insert(
            "wall_load_convention".into(),
            json!("Last interval fixed-wall momentum exchange, gauge datum zero; includes pressure and viscous effects, not a solved structural coupling"),
        );
        let values = p.vector(&s.phi, &s.alpha, &states[states.len() - 1]);
        let mut fields = flow_fields(&s)?;
        insert_wall_fields(&mut fields, &wall, "wall_link_gauge_force_N")?;
        Ok(Evaluation {
            provider: NAME.into(),
            responses: RESPONSES.iter().zip(values).map(|(n, v)| ((*n).to_string(), v)).collect(),
            diagnostics: s.diagnostics,
            fields,
        })
    }

    fn sensitivities_value(
        &self,
        problem: &Value,
        design: &Design,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        check_responses(responses, &RESPONSES, "Unknown, duplicated or empty LBM responses")?;
        let s = self.parts(problem, design)?;
        let p = &s.p;
        let final_state = &s.history.states[p.step_count];
        let values = p.vector(&s.phi, &s.alpha, final_state);
        let dres = p.resistance_derivative(&s.phi);
        let mut out_values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in responses {
            let index = RESPONSES.iter().position(|r| r == name).unwrap_or(0);
            let mut w = [0.0; VECTOR_LEN];
            w[index] = 1.0;
            let (f_bar, alpha_bar_final, mut phi_bar) = p.vector_vjp(&s.phi, &s.alpha, final_state, &w);
            let (alpha_bar, _) = p.history_adjoint(&s.history, &s.alpha, Tau::Uniform(p.tau), f_bar)?;
            for x in 0..p.cells() {
                phi_bar[x] += (alpha_bar[x] + alpha_bar_final[x]) * dres[x];
            }
            let gradient = p.design_map.vjp(&s.raw, &phi_bar);
            if !gradient.iter().all(|v| v.is_finite()) {
                return Err(CaeError::contract("Nonfinite LBM sensitivity"));
            }
            out_values.insert(name.clone(), values[index]);
            let mut g = Design::new();
            g.insert(COORDINATE, array(gradient, &p.shape())?);
            gradients.insert(name.clone(), g);
        }
        Ok(DesignSensitivities { responses: out_values, gradients, diagnostics: s.diagnostics })
    }

    fn admission_value(&self, problem: &Value, candidate: &Design) -> CaeResult<Map<String, Value>> {
        Ok(self.parts(problem, candidate)?.diagnostics)
    }
}

crate::lbm_design_operations!(LatticeBoltzmannProvider, COORDINATE);

impl CaeProvider for LatticeBoltzmannProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let d = descriptor(
            NAME,
            &["isothermal_viscous_flow"],
            &RESPONSES,
            &UNITS,
            &[
                "fluid_fraction",
                "density_kg_m3",
                "velocity_m_s",
                "gauge_pressure_Pa",
                "wall_link_positions_m",
                "wall_link_gauge_force_N",
            ],
            &NOTES,
        );
        presentation(&d, editor(), "array")
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        let notes: Vec<String> = NOTES.iter().map(|s| (*s).to_string()).collect();
        Some(
            contract(NAME, &RESPONSES, &UNITS, &["flow"], &notes)
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        let p = LbmProblem::normalize(problem)?;
        let map = problem.as_object().cloned().unwrap_or_default();
        Ok(Arc::new(geometry::registered_problem(&map, p.shape(), p.spacing_m, p.origin_m)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let p = LbmProblem::normalize(problem_value(problem)?)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.insert("requires_complete_design".into(), json!(true));
        out.insert("relaxation_time".into(), json!(p.tau));
        out.insert("physical_qualification".into(), json!(false));
        Ok(out)
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(missing_method("LatticeBoltzmannProvider", "evaluate"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(missing_method("LatticeBoltzmannProvider", "sensitivity"))
    }

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let d = CouplingDeclaration {
            provider: NAME.into(),
            active_physics: vec!["flow".into()],
            notes: NOTES.iter().map(|s| (*s).to_string()).collect(),
            ..CouplingDeclaration::default()
        };
        Some(Ok(d.to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
