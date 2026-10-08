// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::{Arc, Mutex, PoisonError};

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::route_tables::{
    BodyPolicy, RouteBody, RouteDecl, RouteFailure, RouteReply, RouteTable, RouteTableError,
};

use crate::error::{CfdError, CfdResult};
use crate::optimizer_bridge::{UniversalOptimizerManager, run_with_universal_optimizer};
use crate::preflight::run_preflight;
use crate::resolved_stokes::ResolvedStokesBrinkmanBackend;
use crate::workspace_contract::{CfdProblem, FACES, KINDS, MODELS, from_mapping};

pub const CATALOG_SCHEMA: &str = "implexity-cfd-catalog/3";

#[must_use]
pub fn catalog() -> Value {
    json!({
        "schema": CATALOG_SCHEMA,
        "models": MODELS,
        "boundary_kinds": KINDS,
        "faces": FACES,
        "topology_parameter": "model:control",
        "topology_always_free": true,
        "units": {
            "density": "kg/m^3", "dynamic_viscosity": "Pa s", "velocity": "m/s", "static_pressure": "Pa",
            "volume_flow": "m^3/s", "mass_flow": "kg/s", "length": "m", "permeability": "m^2",
            "temperature": "K", "heat_flux": "W/m^2", "volumetric_heat": "W/m^3"
        }
    })
}


pub fn topology_from_json(v: &Value) -> CfdResult<ArrayD<f64>> {
    fn walk(v: &Value, depth: usize, shape: &mut Vec<usize>, out: &mut Vec<f64>) -> CfdResult<()> {
        match v {
            Value::Array(items) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape.get(depth) != Some(&items.len()) {
                    return Err(CfdError::Input(
                        "setting an array element with a sequence. The requested array has an inhomogeneous shape"
                            .into(),
                    ));
                }
                items.iter().try_for_each(|x| walk(x, depth + 1, shape, out))
            }
            Value::Number(n) if shape.len() == depth => {
                out.push(n.as_f64().unwrap_or(f64::NAN));
                Ok(())
            }
            Value::Bool(b) if shape.len() == depth => {
                out.push(f64::from(u8::from(*b)));
                Ok(())
            }
            Value::Number(_) | Value::Bool(_) => Err(CfdError::Input(
                "setting an array element with a sequence. The requested array has an inhomogeneous shape"
                    .into(),
            )),
            Value::String(s) => Err(CfdError::Input(format!(
                "could not convert string to float: {}",
                implexity_core::py_repr::repr_str(s)
            ))),
            _ => Err(CfdError::Type(
                "float() argument must be a string or a real number, not 'NoneType'".into(),
            )),
        }
    }
    let mut shape = Vec::new();
    let mut out = Vec::new();
    walk(v, 0, &mut shape, &mut out)?;
    ArrayD::from_shape_vec(IxDyn(&shape), out).map_err(|e| CfdError::Input(e.to_string()))
}

pub trait CfdBackend: Send + Sync {
    fn solve(
        &self,
        _problem: &CfdProblem,
        _solid_fraction: Option<&ArrayD<f64>>,
    ) -> Option<CfdResult<Value>> {
        None
    }
    fn solve_and_adjoint(
        &self,
        _problem: &CfdProblem,
        _solid_fraction: Option<&ArrayD<f64>>,
        _response: Option<&str>,
    ) -> Option<CfdResult<Value>> {
        None
    }
    fn sensitivity(
        &self,
        _problem: &CfdProblem,
        _solid_fraction: Option<&ArrayD<f64>>,
        _response: Option<&str>,
    ) -> Option<CfdResult<Value>> {
        None
    }
}

impl CfdBackend for ResolvedStokesBrinkmanBackend {
    fn solve(&self, problem: &CfdProblem, solid_fraction: Option<&ArrayD<f64>>) -> Option<CfdResult<Value>> {
        Some(
            ResolvedStokesBrinkmanBackend::solve(self, problem, solid_fraction.map(|a| a.view()), &[])
                .map(|r| r.as_dict(true)),
        )
    }
    fn solve_and_adjoint(
        &self,
        problem: &CfdProblem,
        solid_fraction: Option<&ArrayD<f64>>,
        response: Option<&str>,
    ) -> Option<CfdResult<Value>> {
        Some(ResolvedStokesBrinkmanBackend::solve_and_adjoint(
            self,
            problem,
            solid_fraction.map(|a| a.view()),
            response,
        ))
    }
}

pub struct CfdWorkspaceController {
    pub backend: Option<Arc<dyn CfdBackend>>,
    pub optimizer_manager: Option<Arc<dyn UniversalOptimizerManager>>,
    problem: Mutex<Option<CfdProblem>>,
}

impl std::fmt::Debug for CfdWorkspaceController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CfdWorkspaceController")
            .field("backend", &self.backend.is_some())
            .field("optimizer_manager", &self.optimizer_manager.is_some())
            .finish_non_exhaustive()
    }
}

impl CfdWorkspaceController {
    #[must_use]
    pub fn new(
        backend: Option<Arc<dyn CfdBackend>>,
        optimizer_manager: Option<Arc<dyn UniversalOptimizerManager>>,
    ) -> Self {
        Self { backend, optimizer_manager, problem: Mutex::new(None) }
    }

    fn stored(&self) -> std::sync::MutexGuard<'_, Option<CfdProblem>> {
        self.problem.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn catalog(&self) -> Value {
        catalog()
    }

    #[must_use]
    pub fn get_problem(&self) -> Value {
        self.stored().as_ref().map_or(Value::Null, CfdProblem::as_dict)
    }


    pub fn put_problem(&self, payload: &Value) -> CfdResult<Value> {
        let p = from_mapping(payload)?;
        *self.stored() = Some(p.clone());
        Ok(json!({"ok": true, "problem": p.as_dict(), "preflight": run_preflight(&p, None).as_dict()}))
    }

    #[must_use]
    pub fn validate(&self, payload: &Value) -> Value {
        match from_mapping(payload) {
            Ok(p) => {
                json!({"ok": true, "problem": p.as_dict(), "preflight": run_preflight(&p, None).as_dict()})
            }
            Err(e) => json!({"ok": false, "error": e.python_name(), "message": e.to_string()}),
        }
    }

    fn resolve_problem(&self, payload: Option<&Value>) -> CfdResult<CfdProblem> {
        match payload.filter(|p| !p.is_null()) {
            Some(p) => from_mapping(p),
            None => {
                self.stored().clone().ok_or_else(|| CfdError::Input("no CFD problem has been defined".into()))
            }
        }
    }

    fn topology(solid_fraction: Option<&Value>) -> CfdResult<Option<ArrayD<f64>>> {
        solid_fraction.filter(|v| !v.is_null()).map(topology_from_json).transpose()
    }


    pub fn preflight(&self, payload: Option<&Value>, solid_fraction: Option<&Value>) -> CfdResult<Value> {
        let p = self.resolve_problem(payload)?;
        let s = Self::topology(solid_fraction)?;
        Ok(run_preflight(&p, s.as_ref().map(|a| a.view())).as_dict())
    }


    pub fn solve(&self, payload: Option<&Value>, solid_fraction: Option<&Value>) -> CfdResult<Value> {
        let p = self.resolve_problem(payload)?;
        let s = Self::topology(solid_fraction)?;
        if !run_preflight(&p, s.as_ref().map(|a| a.view())).ok {
            return Err(CfdError::Input("CFD preflight failed".into()));
        }
        let backend = self.backend.as_ref();
        match backend.and_then(|b| b.solve(&p, s.as_ref())) {
            Some(r) => r,
            None => Err(CfdError::Runtime("resolved CFD backend is not configured".into())),
        }
    }


    pub fn sensitivity(
        &self,
        payload: Option<&Value>,
        solid_fraction: Option<&Value>,
        response: Option<&str>,
    ) -> CfdResult<Value> {
        let p = self.resolve_problem(payload)?;
        let Some(backend) = self.backend.as_ref() else {
            return Err(CfdError::Runtime("resolved CFD backend is not configured".into()));
        };
        let s = Self::topology(solid_fraction)?;
        if let Some(r) = backend.solve_and_adjoint(&p, s.as_ref(), response) {
            return r;
        }
        if let Some(r) = backend.sensitivity(&p, s.as_ref(), response) {
            return r;
        }
        Err(CfdError::Runtime("resolved CFD backend does not expose a discrete adjoint".into()))
    }


    pub fn optimize(
        &self,
        payload: Option<&Value>,
        free: &[Value],
        constraints: &[Value],
        settings: Option<&Map<String, Value>>,
    ) -> CfdResult<Value> {
        let p = self.resolve_problem(payload)?;
        let Some(manager) = self.optimizer_manager.as_ref() else {
            return Err(CfdError::Runtime("universal optimizer manager is not configured".into()));
        };
        run_with_universal_optimizer(manager.as_ref(), &p, free, constraints, settings)
    }
}

fn failure(e: &CfdError) -> RouteFailure {
    if e.is_value_error() {
        RouteFailure::Contract(e.to_string())
    } else {
        RouteFailure::Internal(e.to_string())
    }
}

fn body(req: &implexity_core::route_tables::RouteRequest) -> Value {
    match &req.body {
        RouteBody::Json(v) => v.clone(),
        RouteBody::Raw(_) => json!({}),
    }
}

fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}


pub fn route_table(controller: &Arc<CfdWorkspaceController>) -> Result<RouteTable, RouteTableError> {
    const MODULE: &str = "implexity.cfd.api";
    let mut table = RouteTable::new();
    let ok = |v: Value| Ok(RouteReply::json(200, &v));
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "GET",
        "/v1/implicit/cfd/catalog",
        BodyPolicy::None,
        "CFD catalogue: models, boundary kinds, faces, units.",
        MODULE,
        Arc::new(move |_, _| ok(c.catalog())),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "GET",
        "/v1/implicit/cfd/problem",
        BodyPolicy::None,
        "The stored CFD problem.",
        MODULE,
        Arc::new(move |_, _| ok(c.get_problem())),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "PUT",
        "/v1/implicit/cfd/problem",
        BodyPolicy::Json,
        "Validate and store a CFD problem.",
        MODULE,
        Arc::new(move |req, _| c.put_problem(&body(req)).map_err(|e| failure(&e)).and_then(ok)),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "POST",
        "/v1/implicit/cfd/validate",
        BodyPolicy::Json,
        "Validate a CFD problem without storing it.",
        MODULE,
        Arc::new(move |req, _| ok(c.validate(&body(req)))),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "POST",
        "/v1/implicit/cfd/preflight",
        BodyPolicy::Json,
        "Physical preflight of a CFD problem and topology.",
        MODULE,
        Arc::new(move |req, _| {
            let b = body(req);
            c.preflight(b.get("problem"), b.get("solid_fraction")).map_err(|e| failure(&e)).and_then(ok)
        }),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "POST",
        "/v1/implicit/cfd/solve",
        BodyPolicy::Json,
        "Resolved Stokes-Brinkman solve.",
        MODULE,
        Arc::new(move |req, _| {
            let b = body(req);
            c.solve(b.get("problem"), b.get("solid_fraction")).map_err(|e| failure(&e)).and_then(ok)
        }),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "POST",
        "/v1/implicit/cfd/sensitivity",
        BodyPolicy::Json,
        "Resolved solve with the exact discrete-adjoint topology gradient.",
        MODULE,
        Arc::new(move |req, _| {
            let b = body(req);
            let response = b.get("response").and_then(Value::as_str);
            c.sensitivity(b.get("problem"), b.get("solid_fraction"), response)
                .map_err(|e| failure(&e))
                .and_then(ok)
        }),
    )?)?;
    let c = Arc::clone(controller);
    table.add(RouteDecl::new(
        "POST",
        "/v1/implicit/cfd/optimize",
        BodyPolicy::Json,
        "Declare a CFD topology optimisation with the universal optimizer.",
        MODULE,
        Arc::new(move |req, _| {
            let b = body(req);
            let settings = b.get("settings").and_then(Value::as_object);
            c.optimize(b.get("problem"), &list(b.get("free")), &list(b.get("constraints")), settings)
                .map_err(|e| failure(&e))
                .and_then(ok)
        }),
    )?)?;
    Ok(table)
}
