// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::{
    AddInCategory, AddInContract, ExecutionKind, Fidelity, PublishedContract, ResponseCapability,
    RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::array::Field;
use crate::errors::{PResult, ModelError};
use crate::pyval::{nums, strs};


pub fn presentation(descriptor: &ProviderDescriptor, editor: Value, execution: &str) -> CaseResult {
    Ok(ProviderCapabilities::Descriptor(Box::new(descriptor.clone().with_presentation(editor, execution)?)))
}

pub type CaseResult = CaeResult<ProviderCapabilities>;

#[must_use]
pub fn response_metadata(
    units: &[(&str, &str)],
    differentiable: bool,
    description: Option<&str>,
) -> Map<String, Value> {
    units
        .iter()
        .map(|(k, u)| {
            let mut m = Map::new();
            m.insert("unit".into(), json!(u));
            m.insert("differentiable".into(), json!(differentiable));
            m.insert("design_reachable".into(), json!(differentiable));
            if let Some(d) = description {
                m.insert("description".into(), json!(d));
            }
            ((*k).to_string(), Value::Object(m))
        })
        .collect()
}


pub fn evaluation_contract(name: &str, units: &[(&str, &str)], notes: &[&str]) -> CaeResult<AddInContract> {
    let mut c = AddInContract::new(name);
    c.category = AddInCategory::Field;
    c.responses = units
        .iter()
        .map(|(k, u)| {
            let mut r = ResponseCapability::new(*k);
            r.unit = (*u).to_string();
            r.differentiable = Some(false);
            r.design_reachable = Some(false);
            r
        })
        .collect();
    c.scope = vec!["compressible_transport".into()];
    c.fidelity = Fidelity::Screening;
    c.runtime_route = RuntimeRoute::Array;
    c.exact_design_derivatives = Some(false);
    c.exact_state_transpose = Some(false);
    c.notes = notes.iter().map(|s| (*s).to_string()).collect();
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = format!("provider:{name}");
    c.execution_kind = Some(ExecutionKind::Provider);
    c.supported_operations = vec!["preflight".into(), "evaluate".into()];
    c.no_op_operations = Vec::new();
    c.design_inputs = Vec::new();
    c.checked()
}


pub fn no_design(topology: Option<&ArrayD<f64>>) -> PResult<()> {
    if topology.is_some_and(|t| !t.is_empty()) {
        return Err(ModelError::invalid(
            "quasi-1D verification accepts authored sections, not a topology design",
        ));
    }
    Ok(())
}

pub const RESPONSE_UNITS: [(&str, &str); 3] =
    [("euler_exit_mach", "1"), ("euler_peak_pressure_Pa", "Pa"), ("euler_final_mass_kg", "kg")];

#[derive(Debug, Clone, Copy, Default)]
pub struct Quasi1DEulerProvider;

pub const NAME: &str = "compressible_quasi1d_euler";

impl Quasi1DEulerProvider {
    #[must_use]
    pub fn template() -> Value {
        let (gamma, gas, temperature, pressure, mach) =
            (1.4_f64, 287.0_f64, 300.0_f64, 100_000.0_f64, 0.3_f64);
        let (primitive, total_pressure, total_temperature) =
            crate::perfect_gas::primitive_and_stagnation(gamma, gas, temperature, pressure, mach);
        let row = json!(primitive);
        json!({
            "gamma": gamma, "gas_constant_J_kgK": gas,
            "x_faces_m": nums(&linspace(0.0, 1.0, 41)), "area_faces_m2": vec![0.01; 41],
            "initial_primitive": vec![row; 40],
            "boundaries": {"left": {"kind": "subsonic_reservoir", "total_pressure_Pa": total_pressure,
                                    "total_temperature_K": total_temperature},
                           "right": {"kind": "subsonic_pressure_outlet", "pressure_Pa": pressure}},
            "end_time_s": 0.01, "cfl": 0.8, "max_steps": 20000,
            "provenance": "Synthetic uniform perfect-gas duct demonstration; not calibrated material data",
        })
    }

    #[must_use]
    pub fn descriptor() -> ProviderDescriptor {
        let mut d = ProviderDescriptor::new(
            NAME,
            vec!["quasi1d_compressible_flow".into()],
            RESPONSE_UNITS.iter().map(|(k, _)| (*k).to_string()).collect(),
        );
        d.fields = ["density_kg_m3", "velocity_m_s", "pressure_Pa", "mach"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        d.sensitivities = false;
        d.design_coordinates = Vec::new();
        d.traits = json!({"evaluation_only": true}).as_object().cloned().unwrap_or_default();
        d.response_metadata = response_metadata(
            &RESPONSE_UNITS,
            false,
            Some("Final-time quasi-1D Euler verification response; no topology dependence."),
        );
        d.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        d
    }

    #[must_use]
    pub fn editor() -> Value {
        json!({"kind": "native_json", "title": "Quasi-1D compressible flow \u{2014} evaluation only",
               "problem_template": Self::template(),
               "schema": {"type": "object", "properties": {
                   "gamma": {"title": "Specific heat ratio", "exclusiveMinimum": 1, "maximum": 2},
                   "gas_constant_J_kgK": {"title": "Specific gas constant", "unit": "J/(kg K)", "exclusiveMinimum": 0},
                   "end_time_s": {"title": "Simulation duration", "unit": "s", "exclusiveMinimum": 0},
                   "cfl": {"title": "CFL time-step factor", "exclusiveMinimum": 0, "maximum": 0.9},
                   "max_steps": {"title": "Maximum time steps", "type": "integer", "minimum": 1, "maximum": 1_000_000},
                   "area_faces_m2": {"title": "Cross-sectional face areas", "unit": "m\u{b2}", "items": {"exclusiveMinimum": 0}}}}})
    }


    pub fn evaluate_value(problem: &Value) -> PResult<Evaluation> {
        let out = solve(problem)?;
        let pressure_peak = out.w.iter().map(|x| x[2]).fold(f64::NEG_INFINITY, f64::max);
        let mut responses = std::collections::BTreeMap::new();
        responses.insert("euler_exit_mach".into(), *out.mach.last().unwrap_or(&f64::NAN));
        responses.insert("euler_peak_pressure_Pa".into(), pressure_peak);
        responses.insert("euler_final_mass_kg".into(), out.final_[0]);
        Ok(Evaluation {
            provider: NAME.into(),
            responses,
            diagnostics: out.diagnostics(),
            fields: out.fields().into_iter().map(|(k, f)| (k, FieldValue::Array(f.to_array()))).collect(),
        })
    }
}

fn value_of(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("verification providers require their own problem mapping"))
}

impl CaeProvider for Quasi1DEulerProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn provider_id(&self) -> Option<&str> {
        Some(NAME)
    }

    fn implementation(&self) -> &'static str {
        "implexity.compressible.quasi1d_euler.Quasi1DEulerProvider"
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        presentation(&Self::descriptor(), Self::editor(), "array")
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(
            evaluation_contract(NAME, &RESPONSE_UNITS, &LIMITATIONS)
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        normalize(problem).map_err(CaeError::from)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        no_design(topology).map_err(CaeError::from)?;
        let p = normalize(value_of(problem)?).map_err(CaeError::from)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("cells".into(), json!(p.x_faces.len() - 1));
        out.insert("optimization_supported".into(), json!(false));
        out.insert("limitations".into(), strs(&LIMITATIONS));
        Ok(out)
    }

    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        no_design(Some(topology)).map_err(CaeError::from)?;
        Self::evaluate_value(value_of(problem)?).map_err(CaeError::from)
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(CaeError::contract("'Quasi1DEulerProvider' object has no attribute 'sensitivity'"))
    }

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let d = CouplingDeclaration {
            provider: NAME.into(),
            active_physics: vec!["flow".into()],
            notes: LIMITATIONS.iter().map(|s| (*s).to_string()).collect(),
            ..CouplingDeclaration::default()
        };
        Some(Ok(d.to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl implexity_optim::provider_ops::DesignOperations for Quasi1DEulerProvider {
    fn provides(&self, op: implexity_optim::provider_ops::DesignOp) -> bool {
        matches!(
            op,
            implexity_optim::provider_ops::DesignOp::Evaluate
                | implexity_optim::provider_ops::DesignOp::EvaluateWithoutDesign
        )
    }

    fn evaluate_without_design(&self, problem: &ProviderProblem) -> CaeResult<Evaluation> {
        Self::evaluate_value(value_of(problem)?).map_err(CaeError::from)
    }
}

pub use crate::quasi1d_euler::{LIMITATIONS,Boundary,Problem,conservative,Quasi1DResult,linspace,rows};


pub fn array(value: &Value, name: &str, shape: Option<&[usize]>) -> PResult<Field> {
    crate::quasi1d_euler::array(value,name,shape).map_err(Into::into)
}


pub fn scalar(value: &Value, name: &str) -> PResult<f64> {
    crate::quasi1d_euler::scalar(value,name).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn normalize(problem: &Value) -> PResult<Problem> {
    crate::quasi1d_euler::normalize(problem).map_err(Into::into)
}


pub fn primitive(q: &[[f64; 3]], gamma: f64) -> PResult<Vec<[f64; 3]>> {
    crate::quasi1d_euler::primitive(q,gamma).map_err(Into::into)
}


pub fn characteristic_boundary(
    w: [f64; 3],
    b: &Boundary,
    left: bool,
    gamma: f64,
    gas: f64,
) -> PResult<[f64; 3]> {
    crate::quasi1d_euler::characteristic_boundary(w,b,left,gamma,gas).map_err(Into::into)
}


pub fn face_flux(q: &[[f64; 3]], p: &Problem) -> PResult<(Vec<[f64; 3]>, Vec<f64>)> {
    crate::quasi1d_euler::face_flux(q,p).map_err(Into::into)
}


pub fn solve(problem: &Value) -> PResult<Quasi1DResult> {
    crate::quasi1d_euler::solve(problem).map_err(Into::into)
}
