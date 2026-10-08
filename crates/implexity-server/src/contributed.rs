// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use implexity_core::route_tables::{
    self as neutral, RouteBody, RouteDecl, RouteFailure, RouteReply, RouteRequest, RouteService, RouteTable,
};
use serde_json::{Value, json};

use crate::http::{Reply, Request, RequestBody, RouteError};
use crate::routes::{BodyPolicy, Handler, Registry, Route, RouteDeclarationError};
use crate::service::Service;

#[must_use]
pub fn body_policy(policy: neutral::BodyPolicy) -> BodyPolicy {
    match policy {
        neutral::BodyPolicy::None => BodyPolicy::None,
        neutral::BodyPolicy::Json => BodyPolicy::Json,
        neutral::BodyPolicy::Raw => BodyPolicy::Raw,
    }
}

#[must_use]
pub fn neutral_request(req: &Request) -> RouteRequest {
    RouteRequest {
        method: req.method.clone(),
        path: req.path.clone(),
        query: req.query.clone(),
        ident: req.ident.clone(),
        body: match &req.body {
            RequestBody::Json(v) => RouteBody::Json(v.clone()),
            RequestBody::Raw(b) => RouteBody::Raw(b.clone()),
        },
        headers: req.headers.clone(),
    }
}

#[must_use]
pub fn server_reply(reply: RouteReply) -> Reply {
    Reply::send(reply.status, reply.body, &reply.content_type, reply.headers)
}



pub fn failure_reply(failure: RouteFailure) -> Result<Reply, RouteError> {
    match failure {
        RouteFailure::Contract(message) => Err(RouteError::Contract(message)),
        RouteFailure::Case(e) => {
            Ok(Reply::json(&json!({"error": "case rejected", "problems": e.problems}), 422))
        }
        RouteFailure::Status(status, message, detail) => {
            Ok(Reply::err(status, &message, detail.map_or(Value::Null, Value::String)))
        }
        RouteFailure::Internal(message) => Err(RouteError::internal("Exception", message)),
    }
}



pub fn call_neutral(decl: &RouteDecl, req: &Request) -> Result<Reply, RouteError> {
    let neutral = neutral_request(req);
    match (decl.handler)(&neutral, req.service.as_ref()) {
        Ok(reply) => Ok(server_reply(reply)),
        Err(failure) => failure_reply(failure),
    }
}



pub fn adapt(decl: &RouteDecl) -> Result<Route, RouteDeclarationError> {
    let captured = decl.clone();
    let handler: Handler = Arc::new(move |req: &Request| call_neutral(&captured, req));
    Route::new(&decl.method, &decl.pattern, body_policy(decl.body), &decl.doc, &decl.module, handler)
}



pub fn adapt_table(table: &RouteTable) -> Result<Registry, RouteDeclarationError> {
    let mut registry = Registry::new();
    for decl in table.all() {
        registry.add(adapt(decl)?)?;
    }
    Ok(registry)
}

impl RouteService for Service {
    fn state_dir(&self) -> PathBuf {

        Service::state_dir(self).unwrap_or_else(|_| crate::service::state_directory(self.workspace()))
    }

    fn broadcast(&self, message: &Value) {
        Service::broadcast(self, message);
    }

    fn extension_state(
        &self,
        key: &str,
        factory: &dyn Fn() -> Arc<dyn Any + Send + Sync>,
    ) -> Arc<dyn Any + Send + Sync> {
        self.extension_any(key, factory)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
