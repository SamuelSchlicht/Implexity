// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

use implexity_core::route_tables::{
    BodyPolicy, RouteBody, RouteDecl, RouteHandler, RouteReply, RouteRequest, RouteService, RouteTable,
    RouteTableError,
};
use serde_json::{Value, json};

use crate::error::{AgentError, AgentResult};
use crate::host::AgentHost;
use crate::runtime::AgentManager;

pub const ENDPOINTS: [&str; 12] = [
    "GET /v1/agent/state",
    "GET /v1/agent/capabilities",
    "GET /v1/agent/tools",
    "GET /v1/agent/manual",
    "GET /v1/agent/context",
    "GET /v1/agent/guidance",
    "GET /v1/agent/policy",
    "GET /v1/agent/intent",
    "PUT /v1/agent/intent",
    "POST /v1/agent/validate",
    "POST /v1/agent/plan",
    "POST /v1/agent/action",
];

const MODULE: &str = "implexity.agent.api";
pub const EXTENSION: &str = "kernel.engineering_agent";

pub type HostFactory = Arc<dyn Fn(&dyn RouteService) -> Arc<dyn AgentHost> + Send + Sync>;



pub fn manager(svc: &dyn RouteService, host: &HostFactory) -> AgentResult<Arc<AgentManager>> {

    let gate = svc.extension_state("kernel.engineering_agent.initialization", &|| Arc::new(Mutex::new(())));
    let gate = gate
        .downcast_ref::<Mutex<()>>()
        .ok_or_else(|| AgentError::failed("agent initialization state holds another type"))?;
    let _initialization = gate.lock().unwrap_or_else(PoisonError::into_inner);
    let factory = || -> Arc<dyn Any + Send + Sync> {
        match AgentManager::new(host(svc)) {
            Ok(m) => {

                match m.initialize_managed_supervision() {
                    Ok(()) | Err(AgentError::Refused(_)) => Arc::new(Arc::new(m)),
                    Err(e) => Arc::new(e),
                }
            }
            Err(e) => Arc::new(e),
        }
    };
    let entry = svc.extension_state(EXTENSION, &factory);
    if let Some(m) = entry.downcast_ref::<Arc<AgentManager>>() {
        return Ok(Arc::clone(m));
    }
    Err(entry
        .downcast_ref::<AgentError>()
        .cloned()
        .unwrap_or_else(|| AgentError::failed("agent state holds another type")))
}

fn body(req: &RouteRequest) -> Value {
    match &req.body {
        RouteBody::Json(v) => v.clone(),
        RouteBody::Raw(_) => Value::Null,
    }
}



pub fn dispatch(spec: &str, m: &AgentManager, req: &RouteRequest) -> AgentResult<Value> {
    match spec {
        "GET /v1/agent/state" => m.state(),
        "GET /v1/agent/capabilities" => m.capabilities(),
        "GET /v1/agent/tools" => m.tool_manifest(),
        "GET /v1/agent/manual" => crate::manual::build_manual(m),
        "GET /v1/agent/context" => crate::manual::concise_context(m),
        "GET /v1/agent/guidance" => crate::guidance::build_guidance(m),
        "GET /v1/agent/policy" => m.policy(),
        "GET /v1/agent/intent" => Ok(json!({"intent": m.get_intent()?})),
        "PUT /v1/agent/intent" => m.execute(&json!({"action": "set_intent", "payload": body(req)})),
        "POST /v1/agent/validate" => m.validate_action(&body(req)),
        "POST /v1/agent/plan" => m.validate_plan(&body(req)),
        "POST /v1/agent/action" => m.execute(&body(req)),
        other => Err(AgentError::failed(format!("{other} is not an agent endpoint"))),
    }
}

#[must_use]
pub fn reply_of(result: AgentResult<Value>) -> RouteReply {
    match result {
        Ok(v) => RouteReply::json(200, &v),
        Err(e) => {
            let (status, body) = e.reply();
            RouteReply::json(status, &body)
        }
    }
}



pub fn route_table(host: &HostFactory) -> Result<RouteTable, RouteTableError> {
    let mut t = RouteTable::new();
    for spec in ENDPOINTS {
        let (method, pattern) = spec.split_once(' ').unwrap_or(("GET", spec));
        let host = Arc::clone(host);
        let policy = if method == "GET" { BodyPolicy::None } else { BodyPolicy::Json };
        let handler: RouteHandler = Arc::new(move |req: &RouteRequest, svc: &dyn RouteService| {
            Ok(reply_of(manager(svc, &host).and_then(|m| dispatch(spec, &m, req))))
        });
        t.add(RouteDecl::new(method, pattern, policy, "", MODULE, handler)?)?;
    }
    Ok(t)
}
