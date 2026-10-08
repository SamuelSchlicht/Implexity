// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;

use implexity_ad::Scalar;
use implexity_core::contracts::{
    CaeProvider, Evaluation, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::NamedArrays;
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::{
    AdmissionReply, CandidateDesign, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity,
    LifecycleDeclaration,
};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::array::Field;
use crate::errors::PResult;
use crate::euler3d::{
    Problem, WallLayout,
};
use crate::providers::quasi1d_euler::presentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Euler3DTopologyProvider {
    scheme: Scheme,
}

impl Euler3DTopologyProvider {
    #[must_use]
    pub const fn first_order() -> Self {
        Self { scheme: Scheme::FirstOrder }
    }

    #[must_use]
    pub const fn muscl() -> Self {
        Self { scheme: Scheme::Muscl }
    }

    #[must_use]
    pub const fn scheme(&self) -> Scheme {
        self.scheme
    }

    #[must_use]
    pub const fn id(&self) -> &'static str {
        match self.scheme {
            Scheme::FirstOrder => NAME,
            Scheme::Muscl => crate::providers::euler3d_muscl::NAME,
        }
    }

    const fn class(self) -> &'static str {
        match self.scheme {
            Scheme::FirstOrder => "Euler3DTopologyProvider",
            Scheme::Muscl => "MusclTopologyProvider",
        }
    }

    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut n: Vec<String> = [
            "Experimental fixed-step transient perfect-gas Euler with relaxed volume fractions.",
            "Requires an explicit shape-matched model:control array and bounds; the current CAD geometry is not automatically converted.",
            "The closed-box template demonstrates admission, not a useful transport objective or calibrated benchmark.",
            "No viscosity, turbulence, combustion or sharp-wall qualification.",
            "Signed final fluxes include pressure momentum; not steady-state certification.",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        if self.scheme == Scheme::Muscl {
            n.push("Primitive MC reconstruction with SSPRK2; first order next to boundaries.".into());
            n.push(
                "Limiter derivatives are branch-local; gradients need not exist at limiter switches.".into(),
            );
        }
        n
    }

    #[must_use]
    pub fn template() -> Value {
        let mut flow = crate::providers::euler3d::Euler3DProvider::template();
        flow["shape"] = json!([4, 3, 2]);
        flow["spacing_m"] = json!([0.1, 0.1, 0.1]);
        flow["end_time_s"] = json!(0.00001);
        json!({"flow": flow, "topology_map": {"filter_radius_m": 0.0, "projection_beta": 0.0, "projection_eta": 0.5,
                "minimum_fluid_fraction": 0.01, "design_region": region_template(), "fixed_design": vec![vec![vec![1.0; 2]; 3]; 4]},
               "time_integration": {"step_s": 1.0e-6, "step_count": 10, "history_byte_budget": 1_048_576}})
    }


    pub fn capabilities_value(&self) -> CaeResult<ProviderCapabilities> {
        let names = response_names();
        let mut d =
            ProviderDescriptor::new(self.id(), vec!["compressible_inviscid_flow".into()], names.clone());
        d.fields = ["fluid_fraction", "density_kg_m3", "velocity_m_s", "pressure_Pa", "temperature_K"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        d.sensitivities = true;
        d.design_coordinates = vec![COORDINATE.into()];
        d.traits = json!({"experimental": true, "requires_explicit_design": true})
            .as_object()
            .cloned()
            .unwrap_or_default();
        d.response_metadata = names
            .iter()
            .zip(response_units())
            .map(|(n, u)| (n.clone(), json!({"unit": u, "differentiable": true, "design_reachable": true})))
            .collect();
        d.notes = self.notes();
        let title = match self.scheme {
            Scheme::FirstOrder => "3-D compressible topology \u{2014} experimental inviscid model",
            Scheme::Muscl => "3-D compressible topology \u{2014} MUSCL/SSPRK2 experimental",
        };
        let editor = json!({"kind": "native_json", "title": title,
            "design_template": {COORDINATE: {"value": vec![vec![vec![1.0; 2]; 3]; 4], "lower": 0.0, "upper": 1.0, "designable": region_template()}},
            "problem_template": Self::template(),
            "schema": {"type": "object", "properties": {
                "flow": {"title": "Compressible flow problem", "format": "json",
                    "description": "Perfect-gas inviscid flow on an all-fluid base grid. Fixed CAD masks are not topology coordinates."},
                "topology_map": {"title": "Topology filter, projection and protected cells", "format": "json",
                    "description": "Arrays must match flow.shape. Explicit model:control design values lie in [0,1]: zero is the finite fluid-fraction floor, one is fluid. False design_region cells use fixed_design."},
                "time_integration": {"title": "Fixed transient schedule and memory budget", "type": "object",
                    "description": "Time step \u{d7} count must equal the flow end time. Every candidate trajectory must pass CFL and positivity admission.",
                    "properties": {
                        "step_s": {"title": "CFD time step", "unit": "s", "type": "number", "exclusiveMinimum": 0},
                        "step_count": {"title": "CFD step count", "type": "integer", "minimum": 1, "maximum": 1_000_000},
                        "history_byte_budget": {"title": "Returned state-history budget", "unit": "bytes", "type": "integer", "minimum": 1,
                            "description": "Does not include all solver or differentiation working memory."}}}}}});
        presentation(&d, editor, "array")
    }


    pub fn lifecycle(&self) -> CaeResult<OptimizerLifecycleConfig> {
        OptimizerLifecycleConfig::new(
            vec![COORDINATE.into()],
            "sensitivity_design",
            "evaluate_design",
            Some("candidate_design_admission"),
            None,
            false,
            false,
        )
    }


    pub fn contract(&self) -> CaeResult<AddInContract> {
        let id = self.id();
        let port = format!("{id}.design.0");
        let mut c = AddInContract::new(id);
        c.category = AddInCategory::Field;
        c.responses = response_names()
            .iter()
            .zip(response_units())
            .map(|(n, u)| {
                let mut r = ResponseCapability::new(n.as_str());
                r.unit = u.to_string();
                r.differentiable = Some(true);
                r.design_reachable = Some(true);
                r.depends_on = vec![port.clone()];
                r
            })
            .collect();
        c.scope = vec!["compressible_transport".into()];
        c.fidelity = Fidelity::Screening;
        c.priority = 50;
        c.runtime_route = RuntimeRoute::Array;
        c.exact_design_derivatives = Some(true);
        c.exact_state_transpose = Some(true);
        c.notes = self.notes();
        c.contract_version = 2;
        c.compatibility_mode = false;
        c.owner_id = format!("provider:{id}");
        c.execution_kind = Some(ExecutionKind::Provider);
        c.supported_operations =
            ["preflight", "preflight_design", "evaluate", "sensitivity", "sensitivities", "optimize"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        c.no_op_operations = Vec::new();
        let mut input = DesignCoordinateRef::new(COORDINATE, port);
        input.addin_id = id.to_string();
        c.design_inputs = vec![input];
        c.checked()
    }

    #[must_use]
    pub fn declaration(&self) -> Value {
        CouplingDeclaration {
            provider: self.id().into(),
            active_physics: vec!["flow".into()],
            notes: self.notes(),
            ..CouplingDeclaration::default()
        }
        .to_value()
    }

    pub fn validate(problem:&Value)->PResult<()>{crate::euler3d_ad::EulerTopology::validate(problem).map_err(Into::into)}


    pub fn parts(&self, problem: &Value, design: &NamedArrays) -> PResult<Parts> { crate::euler3d_ad::EulerTopology{scheme:self.scheme}.parts(problem,design).map_err(Into::into) }


    pub fn preflight_design_value(
        &self,
        problem: &Value,
        design: &NamedArrays,
    ) -> PResult<Map<String, Value>> { crate::euler3d_ad::EulerTopology{scheme:self.scheme}.preflight_design_value(problem,design).map_err(Into::into) }


    pub fn admission_value(
        &self,
        problem: &Value,
        current: &NamedArrays,
        candidate: &NamedArrays,
    ) -> CaeResult<Value> { crate::euler3d_ad::EulerTopology{scheme:self.scheme}.admission_value(problem,current,candidate) }


    pub fn evaluate_design_value(&self, problem: &Value, design: &NamedArrays) -> PResult<Evaluation> { crate::euler3d_ad::EulerTopology{scheme:self.scheme}.evaluate_design_value(problem,design).map(|mut v|{v.provider=self.id().into();v}) }


    pub fn sensitivities_value(
        &self,
        problem: &Value,
        design: &NamedArrays,
        responses: &[String],
    ) -> PResult<DesignSensitivities> { crate::euler3d_ad::EulerTopology{scheme:self.scheme}.sensitivities_value(problem,design,responses).map_err(Into::into) }

}

fn region_template() -> Value {
    let region: Vec<Vec<Vec<bool>>> = (0..4).map(|i| vec![vec![i != 0 && i != 3; 2]; 3]).collect();
    json!(region)
}

pub static PROVIDER: Euler3DTopologyProvider = Euler3DTopologyProvider::first_order();

fn value_of(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("Euler topology providers require their own problem mapping"))
}

impl CaeProvider for Euler3DTopologyProvider {
    fn name(&self) -> &str {
        self.id()
    }

    fn provider_id(&self) -> Option<&str> {
        Some(self.id())
    }

    fn implementation(&self) -> &str {
        match self.scheme {
            Scheme::FirstOrder => "implexity.compressible.euler3d_ad.Euler3DTopologyProvider",
            Scheme::Muscl => "implexity.compressible.euler3d_muscl.MusclTopologyProvider",
        }
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        self.capabilities_value()
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(self.contract().map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Self::validate(problem)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        Self::validate(value_of(problem)?)?;
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("issues".into(), json!([]));
        m.insert("requires_complete_design".into(), json!(true));
        m.insert("physical_qualification".into(), json!(false));
        Ok(m)
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(CaeError::contract(format!("'{}' object has no attribute 'evaluate'", self.class())))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(CaeError::contract(format!("'{}' object has no attribute 'sensitivity'", self.class())))
    }

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        Some(Ok(self.declaration()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl DesignOperations for Euler3DTopologyProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::CandidateDesignAdmission
                | DesignOp::OptimizerLifecycle
        )
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _operating_point: usize,
    ) -> CaeResult<Evaluation> {
        Ok(self.evaluate_design_value(value_of(problem)?, design)?)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        Ok(self.preflight_design_value(value_of(problem)?, design)?)
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _operating_point: usize,
    ) -> CaeResult<DesignSensitivity> {
        let mut out = self.sensitivities_value(value_of(problem)?, design, &[response.to_string()])?;
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
        Ok(self.sensitivities_value(value_of(problem)?, design, responses)?)
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
        Ok(AdmissionReply::Record(self.admission_value(
            value_of(problem)?,
            &named(current)?,
            &named(trial)?,
        )?))
    }

    fn optimizer_lifecycle(&self, _problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Ok(LifecycleDeclaration::Typed(self.lifecycle()?))
    }
}

pub use crate::euler3d_ad::{face_fractions,HistoryResponses,WallForceHistory,WallForces,SolidLoadHistory,TopologyMap,topology_fraction,exact_int,schedule_matches,real_step,AdmittedHistory,history_bytes,Scheme,COORDINATE,RATE_NAMES,response_names,Parts};


pub fn transport_rhs<S: Scalar>(q: &[[S; 5]], p: &Problem, reconstruct: bool) -> PResult<Vec<[S; 5]>> {
    crate::euler3d_ad::transport_rhs(q,p,reconstruct).map_err(Into::into)
}


pub fn volume_fraction_rhs<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: &[S],
    reconstruct: bool,
) -> PResult<Vec<[S; 5]>> {
    crate::euler3d_ad::volume_fraction_rhs(q,p,phi,reconstruct).map_err(Into::into)
}


pub fn outward_boundary_rates<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: &[S],
    reconstruct: bool,
) -> PResult<[[S; 5]; 6]> {
    crate::euler3d_ad::outward_boundary_rates(q,p,phi,reconstruct).map_err(Into::into)
}


pub fn first_order_step<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: Option<&[S]>,
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    crate::euler3d_ad::first_order_step(q,p,phi,dt).map_err(Into::into)
}


pub fn fixed_history(q0: &[[f64; 5]], p: &Problem, dt: f64, count: usize) -> PResult<Vec<Vec<[f64; 5]>>> {
    crate::euler3d_ad::fixed_history(q0,p,dt,count).map_err(Into::into)
}


pub fn volume_fraction_history(
    q0: &[[f64; 5]],
    p: &Problem,
    phi: &[f64],
    dt: f64,
    count: usize,
) -> PResult<Vec<Vec<[f64; 5]>>> {
    crate::euler3d_ad::volume_fraction_history(q0,p,phi,dt,count).map_err(Into::into)
}


pub fn history_responses(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    phi: &[f64],
    dt: f64,
) -> PResult<HistoryResponses> {
    crate::euler3d_ad::history_responses(states,p,phi,dt).map_err(Into::into)
}


pub fn wall_forces(
    q: &[[f64; 5]],
    p: &Problem,
    layout: &WallLayout,
    reconstruct: bool,
) -> PResult<WallForces> {
    crate::euler3d_ad::wall_forces(q,p,layout,reconstruct).map_err(Into::into)
}


pub fn wall_force_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    reconstruct: bool,
) -> PResult<WallForceHistory> {
    crate::euler3d_ad::wall_force_history(states,p,layout,dt,reconstruct).map_err(Into::into)
}


pub fn conforming_solid_load_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    solid_nodes: &[[f64; 3]],
) -> PResult<SolidLoadHistory> {
    crate::euler3d_ad::conforming_solid_load_history(states,p,layout,dt,solid_nodes).map_err(Into::into)
}


pub fn normalize_topology_map(
    settings: &Value,
    shape: [usize; 3],
    spacing: [f64; 3],
) -> PResult<TopologyMap> {
    crate::euler3d_ad::normalize_topology_map(settings,shape,spacing).map_err(Into::into)
}


pub fn checked_topology_fraction(design: &Field, map: &TopologyMap) -> PResult<Vec<f64>> {
    crate::euler3d_ad::checked_topology_fraction(design,map).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn checked_fixed_history(
    problem: &Value,
    step_s: &Value,
    step_count: &Value,
    budget: &Value,
    fluid_fraction: Option<&[f64]>,
) -> PResult<AdmittedHistory> {
    crate::euler3d_ad::checked_fixed_history(problem,step_s,step_count,budget,fluid_fraction).map_err(Into::into)
}


pub fn scheme_step<S: Scalar>(
    scheme: Scheme,
    q: &[[S; 5]],
    p: &Problem,
    phi: &[S],
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    crate::euler3d_ad::scheme_step(scheme,q,p,phi,dt).map_err(Into::into)
}


pub fn response_vector<S: Scalar>(scheme: Scheme, q: &[[S; 5]], p: &Problem, phi: &[S]) -> PResult<Vec<S>> {
    crate::euler3d_ad::response_vector(scheme,q,p,phi).map_err(Into::into)
}

use crate::euler3d_ad::response_units;

pub const NAME: &str = "compressible_cartesian_euler3d_topology";
