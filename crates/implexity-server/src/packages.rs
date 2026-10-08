// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use implexity_core::package_session::{PackageService, PackageSession};
use implexity_io::heavy_lease::HeavyOperationLease;
use serde_json::{Value, json};

use crate::http::{Reply, Request, RouteError};
use crate::routes::{BodyPolicy, Registry, RouteDeclarationError};
use crate::service::Service;

struct PackageHost {
    service: Weak<Service>,
    state_dir: PathBuf,
}

struct EvaluationHold(Arc<HeavyOperationLease>);

impl Drop for EvaluationHold {
    fn drop(&mut self) {
        let _ = self.0.release();
    }
}

impl PackageService for PackageHost {
    fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    fn try_acquire_evaluation(&self) -> Option<Box<dyn Any>> {
        let service = self.service.upgrade()?;
        let lease = service.eval_lock().ok()?;
        match lease.acquire(false, None) {
            Ok(true) => Some(Box::new(EvaluationHold(lease))),
            _ => None,
        }
    }

    fn jobs(&self) -> Value {
        self.service.upgrade().map_or_else(|| json!({"jobs": []}), |s| s.jobs_list())
    }

    fn broadcast(&self, message: Value) {
        if let Some(s) = self.service.upgrade() {
            s.broadcast(&message);
        }
    }
}

pub struct Packages {
    session: PackageSession<'static>,
    imports: implexity_runtime::provider_import::ImportSession,
}

impl std::fmt::Debug for Packages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packages").field("restoration_error", &self.session.restoration_error()).finish()
    }
}

impl Packages {
    pub(crate) fn open(service: &Arc<Service>) -> Self {
        let state_dir =
            service.state_dir().unwrap_or_else(|_| crate::service::state_directory(service.workspace()));

        let host: &'static PackageHost =
            Box::leak(Box::new(PackageHost { service: Arc::downgrade(service), state_dir }));
        Self { session: PackageSession::new(host, implexity_core::packages::global()), imports: implexity_runtime::provider_import::ImportSession::new(host) }
    }

    pub fn imports(&self) -> &implexity_runtime::provider_import::ImportSession { &self.imports }

    #[must_use]
    pub fn session(&self) -> &PackageSession<'static> {
        &self.session
    }
}

fn neutral(
    reply: Result<implexity_core::route_tables::RouteReply, implexity_core::route_tables::RouteFailure>,
) -> Result<Reply, RouteError> {
    match reply {
        Ok(r) => Ok(crate::contributed::server_reply(r)),
        Err(f) => crate::contributed::failure_reply(f),
    }
}



pub fn routes() -> Result<Registry, RouteDeclarationError> {
    const MODULE: &str = "implexity.cae.api";
    let mut r = Registry::new();
    r.route(
        "GET",
        "/v1/physics/packages",
        BodyPolicy::None,
        "",
        MODULE,
        Arc::new(|req: &Request| {
            neutral(implexity_runtime::api::packages_status_reply(req.service.packages().session()))
        }),
    )?;
    r.route(
        "POST",
        "/v1/physics/packages",
        BodyPolicy::Json,
        "",
        MODULE,
        Arc::new(|req: &Request| {
            Ok(crate::contributed::server_reply(implexity_runtime::api::packages_change_reply(
                req.service.packages().session(),
                req.json(),
            )))
        }),
    )?;
    r.route("GET", "/v1/physics/providers/imported", BodyPolicy::None, "", MODULE, Arc::new(|req: &Request| {
        Ok(Reply::json(&req.service.packages().imports().status(), 200))
    }))?;
    r.route("POST", "/v1/physics/providers/imported", BodyPolicy::Json, "", MODULE, Arc::new(|req: &Request| {
        Ok(crate::contributed::server_reply(implexity_runtime::api::imported_provider_reply(req.service.packages().imports(),req.json(),false)))
    }))?;
    r.route("POST", "/v1/physics/providers/imported/check", BodyPolicy::Json, "", MODULE, Arc::new(|req: &Request| {
        Ok(crate::contributed::server_reply(implexity_runtime::api::imported_provider_reply(req.service.packages().imports(),req.json(),true)))
    }))?;
    Ok(r)
}
