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
