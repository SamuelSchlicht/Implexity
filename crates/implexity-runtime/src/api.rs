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



use std::sync::Arc;

use implexity_core::contracts::ResponseSpec;
use implexity_core::package_session::PackageSession;
use implexity_core::route_tables::{
    BodyPolicy, RouteBody, RouteDecl, RouteFailure, RouteReply, RouteRequest, RouteService, RouteTable,
};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use crate::cae_runtime::CaeRuntime;
use crate::results::{ExecutionOutput, evaluation_value};

const MODULE: &str = "implexity.cae.api";

fn body(req: &RouteRequest) -> Map<String, Value> {
    match &req.body {
        RouteBody::Json(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

fn failure(e: &CaeError) -> RouteFailure {
    RouteFailure::Contract(e.message().to_string())
}

fn truthy_get<'a>(b: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    b.get(key).filter(|v| crate::pyval::truthy(Some(v)))
}

fn or_empty(b: &Map<String, Value>, key: &str) -> Value {
    truthy_get(b, key).cloned().unwrap_or_else(|| Value::Object(Map::new()))
}

fn design_arg(b: &Map<String, Value>) -> Option<&Value> {
    if b.contains_key("design") { b.get("design") } else { b.get("topology") }
}

fn with_selection(runtime: &CaeRuntime, declaration: &Value, output: Map<String, Value>) -> CaeResult<Value> {
    let prepared = runtime.prepare(declaration)?;
    let mut row = output;
    for (k, v) in prepared.coordinate_selection() {
        row.insert(k, v);
    }
    Ok(Value::Object(row))
}

fn handle(
    f: impl Fn(&Map<String, Value>, &CaeRuntime) -> CaeResult<Value> + Send + Sync + 'static,
) -> implexity_core::route_tables::RouteHandler {
    Arc::new(move |req: &RouteRequest, _svc: &dyn RouteService| {
        let runtime = CaeRuntime::default();
        f(&body(req), &runtime).map(|v| RouteReply::json(200, &v)).map_err(|e| failure(&e))
    })
}

fn response_specs(raw: Option<&Value>) -> CaeResult<Option<Vec<ResponseSpec>>> {
    match raw {
        Some(Value::Array(items)) if !items.is_empty() => {
            Ok(Some(items.iter().map(ResponseSpec::from_dict).collect::<CaeResult<Vec<_>>>()?))
        }
        _ => Ok(None),
    }
}


pub fn preflight(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let declaration =
        if let Some(provider) = truthy_get(b, "provider").or_else(|| truthy_get(b, "providerId")) {
            let mut d = Map::new();
            d.insert("provider".into(), provider.clone());
            d.insert("problem".into(), or_empty(b, "problem"));
            d.insert("design_freedom".into(), b.get("design_freedom").cloned().unwrap_or(Value::Null));
            d.insert("schedule".into(), b.get("schedule").cloned().unwrap_or(Value::Null));
            if let Some(v) = b.get("designCoordinates") {
                d.insert("design_coordinates".into(), v.clone());
            } else if let Some(v) = b.get("design_coordinates") {
                d.insert("design_coordinates".into(), v.clone());
            }
            Value::Object(d)
        } else {
            truthy_get(b, "declaration").cloned().unwrap_or_else(|| Value::Object(b.clone()))
        };
    let out = runtime.preflight(&declaration, design_arg(b))?;
    with_selection(runtime, &declaration, out)
}


pub fn evaluate(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let declaration = or_empty(b, "problem");
    let out = runtime.evaluate(&declaration, design_arg(b))?;
    with_selection(runtime, &declaration, out)
}


pub fn sensitivity(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let declaration = or_empty(b, "problem");
    let response = crate::pyval::text_or(b.get("response"), "");
    let out = runtime.sensitivity(&declaration, design_arg(b), &response)?;
    with_selection(runtime, &declaration, out)
}


pub fn optimize(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let declaration = or_empty(b, "problem");
    let out = runtime.optimize(&declaration, design_arg(b), b.get("settings"), None)?;
    with_selection(runtime, &declaration, out)
}


pub fn orchestration_plan(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let intent = truthy_get(b, "intent").cloned().unwrap_or_else(|| Value::Object(b.clone()));
    runtime.plan_intent(&intent).map(Value::Object)
}

fn context_of(b: &Map<String, Value>) -> Map<String, Value> {
    crate::pyval::mapping_or_empty(truthy_get(b, "context"))
}


pub fn orchestration_evaluate(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let intent = or_empty(b, "intent");
    let ctx = context_of(b);
    let out = runtime.orchestrated_execute(
        &intent,
        "evaluate",
        b.get("topology"),
        b.get("design"),
        Some(&ctx),
        None,
        None,
        None,
    )?;
    Ok(match out {
        ExecutionOutput::Evaluation(e) => {
            Value::Object(evaluation_value(Some("implexity-orchestrated-evaluation/1"), &e))
        }
        other => other.to_value(),
    })
}


pub fn orchestration_sensitivity(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let intent = or_empty(b, "intent");
    let mut ctx = context_of(b);
    let response = [b.get("response"), ctx.get("response")]
        .into_iter()
        .flatten()
        .find(|v| crate::pyval::truthy(Some(v)))
        .map_or_else(String::new, crate::pyval::py_str);
    ctx.insert("response".into(), Value::String(response));
    let out = runtime.orchestrated_execute(
        &intent,
        "sensitivity",
        b.get("topology"),
        b.get("design"),
        Some(&ctx),
        None,
        None,
        None,
    )?;
    Ok(match out {
        ExecutionOutput::Sensitivity(s) => json!({
            "schema": "implexity-orchestrated-sensitivity/1",
            "provider": s.provider,
            "response": s.response,
            "value": implexity_optim::numeric::float_value(s.value),
            "gradient": implexity_optim::numeric::array_to_value(&s.gradient),
            "diagnostics": s.diagnostics,
            "topology_coordinate": "model:control",
        }),
        other => other.to_value(),
    })
}


pub fn orchestration_optimize(b: &Map<String, Value>, runtime: &CaeRuntime) -> CaeResult<Value> {
    let intent = or_empty(b, "intent");
    let ctx = context_of(b);
    let responses = response_specs(b.get("responses"))?;
    let out = runtime.orchestrated_execute(
        &intent,
        "optimize",
        b.get("topology"),
        b.get("design"),
        Some(&ctx),
        responses,
        b.get("settings"),
        None,
    )?;
    Ok(out.to_value())
}

fn route(
    method: &str,
    pattern: &str,
    body: BodyPolicy,
    handler: implexity_core::route_tables::RouteHandler,
) -> Result<RouteDecl, implexity_core::route_tables::RouteTableError> {
    RouteDecl::new(method, pattern, body, "", MODULE, handler)
}


pub fn route_table() -> Result<RouteTable, implexity_core::route_tables::RouteTableError> {
    let mut t = RouteTable::new();
    t.add(route(
        "GET",
        "/v1/implicit/cae/catalogue",
        BodyPolicy::None,
        handle(|_b, runtime| runtime.catalogue()),
    )?)?;
    t.add(route("POST", "/v1/implicit/cae/preflight", BodyPolicy::Json, handle(preflight))?)?;
    t.add(route("POST", "/v1/implicit/cae/evaluate", BodyPolicy::Json, handle(evaluate))?)?;
    t.add(route("POST", "/v1/implicit/cae/sensitivity", BodyPolicy::Json, handle(sensitivity))?)?;
    t.add(route("POST", "/v1/implicit/cae/optimize", BodyPolicy::Json, handle(optimize))?)?;
    t.add(route(
        "POST",
        "/v1/implicit/cae/orchestration/plan",
        BodyPolicy::Json,
        handle(orchestration_plan),
    )?)?;
    t.add(route(
        "POST",
        "/v1/implicit/cae/orchestration/evaluate",
        BodyPolicy::Json,
        handle(orchestration_evaluate),
    )?)?;
    t.add(route(
        "POST",
        "/v1/implicit/cae/orchestration/sensitivity",
        BodyPolicy::Json,
        handle(orchestration_sensitivity),
    )?)?;
    t.add(route(
        "POST",
        "/v1/implicit/cae/orchestration/optimize",
        BodyPolicy::Json,
        handle(orchestration_optimize),
    )?)?;
    Ok(t)
}


pub fn packages_status_reply(session: &PackageSession<'_>) -> Result<RouteReply, RouteFailure> {
    session
        .status()
        .map(|v| RouteReply::json(200, &v))
        .map_err(|e| RouteFailure::Internal(e.message().to_string()))
}

#[must_use]
pub fn packages_change_reply(session: &PackageSession<'_>, body: &Value) -> RouteReply {
    let b = body.as_object().cloned().unwrap_or_default();
    let package = crate::pyval::text_or(b.get("package"), "");
    let operation = crate::pyval::text_or(b.get("operation"), "");
    match session.change(&package, &operation, b.get("expected_generation")) {
        Ok(v) => RouteReply::json(200, &v),
        Err(e) => {
            let message = e.message().to_string();
            let status =
                if message.contains("PHYSICS_IN_USE") || message.contains("STALE_") { 409 } else { 422 };
            RouteReply::json(status, &json!({"ok": false, "error": message}))
        }
    }
}

pub fn imported_provider_reply(session: &crate::provider_import::ImportSession, request:&Value, check:bool)->RouteReply {
    let result=if check {session.verify(request)} else {session.change(request)};
    match result {
        Ok(v)=>RouteReply::json(200,&v),
        Err(e)=>{let message=e.message();let status=if message.contains("PHYSICS_IN_USE")||message.contains("STALE_"){409}else{422};RouteReply::json(status,&json!({"ok":false,"error":message}))}
    }
}
