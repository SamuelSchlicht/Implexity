// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use crate::http::Request;
use crate::routes::{Registry, Route, RouteDeclarationError};
use crate::service::Service;



pub fn initialise_kernel() -> Result<(), String> {
    let registries = implexity_core::registries::global();
    let id = implexity_runtime::intent_orchestrated::PROVIDER_ID;
    if registries.providers.get(id).is_err() {

        let registered = registries.register_provider(Arc::new(
            implexity_runtime::intent_orchestrated::IntentOrchestratedProvider::default(),
        ));
        if let Err(e) = registered
            && registries.providers.get(id).is_err()
        {
            return Err(e.message().to_string());
        }
    }
    implexity_geometry::preview::register_backends();
    Ok(())
}

fn cae_routes() -> Result<Registry, RouteDeclarationError> {
    let table = implexity_runtime::api::route_table().map_err(|e| RouteDeclarationError(e.0))?;
    let mut r = Registry::new();
    for decl in table.all() {
        let captured = decl.clone();
        r.add(Route::new(
            &decl.method,
            &decl.pattern,
            crate::contributed::body_policy(decl.body),
            &decl.doc,
            &decl.module,
            Arc::new(move |req: &Request| {
                let _session = req.service.packages();
                crate::contributed::call_neutral(&captured, req)
            }),
        )?)?;
    }
    Ok(r)
}



pub fn published_kernel_routes() -> Result<Registry, RouteDeclarationError> {
    let mut all = Registry::new();
    let authoring = implexity_authoring::api::route_table().map_err(|e| RouteDeclarationError(e.0))?;
    let jobs = implexity_jobs::api::route_table().map_err(|e| RouteDeclarationError(e.0))?;
    for table in [
        cae_routes()?,
        crate::packages::routes()?,
        crate::body::routes()?,
        crate::agent::routes()?,
        crate::contributed::adapt_table(&authoring)?,
        crate::contributed::adapt_table(&jobs)?,
    ] {
        for route in table.all() {
            all.add(route.clone())?;
        }
    }
    Ok(all)
}



pub fn install_kernel_endpoints(service: &Service) -> Result<(), RouteDeclarationError> {
    for route in published_kernel_routes()?.all() {
        service.install_kernel_route(route.clone())?;
    }
    Ok(())
}

pub fn install_host_hooks(service: &Arc<Service>) {
    let for_generation = Arc::downgrade(service);
    let for_endpoints = Arc::downgrade(service);
    let hooks = implexity_authoring::services::ServiceHooks {
        package_generation: Some(Arc::new(move || {
            let service = for_generation.upgrade().ok_or_else(|| {
                implexity_authoring::error::AuthoringError::runtime(
                    "RuntimeError",
                    "the service has shut down",
                )
            })?;
            let status = service.packages().session().status().map_err(|e| {
                implexity_authoring::error::AuthoringError::runtime("CAEContractError", e.to_string())
            })?;
            Ok(status.get("generation").cloned().unwrap_or(serde_json::Value::Null))
        })),
        endpoints: Some(Arc::new(move || for_endpoints.upgrade().map(|s| s.endpoints()).unwrap_or_default())),
    };
    implexity_authoring::services::install_service_hooks(service.as_ref(), hooks);
    install_jobs_hooks(service);
}

fn install_jobs_hooks(service: &Arc<Service>) {
    use implexity_jobs::error::{JobError, JobResult};
    let weak = Arc::downgrade(service);
    let gone = || JobError::runtime("the service has shut down");
    let (w_lease, w_cast, w_guard, w_case, w_name, w_list) =
        (weak.clone(), weak.clone(), weak.clone(), weak.clone(), weak.clone(), weak);
    let hooks = implexity_jobs::api::JobsHooks {
        eval_lock: Some(Arc::new(move || {
            w_lease.upgrade().ok_or_else(|| std::io::Error::other("the service has shut down"))?.eval_lock()
        })),
        broadcast: Some(Arc::new(move |message| {
            if let Some(s) = w_cast.upgrade() {
                s.broadcast(message);
            }
        })),
        physics_runtime_guard: Some(Arc::new(move |request, body: &mut dyn FnMut() -> JobResult<()>| {
            let s = w_guard.upgrade().ok_or_else(gone)?;
            let mut inner = Ok(());
            s.packages().session().runtime_guard(request, || {
                inner = body();
                Ok(())
            })?;
            inner
        })),
        normalise_computation_effort_request: Some(Arc::new(|raw| {
            implexity_agent::contracts::normalise_computation_effort_request(raw).map_err(|e| e.to_string())
        })),
        current_case: Some(Arc::new(move |backend| {
            let s = w_case.upgrade()?;
            let svc: &dyn std::any::Any = s.as_ref();
            implexity_core::backends::current_case(
                &implexity_core::registries::global().contributions,
                svc,
                Some(backend),
            )
        })),
        service_version: Some(Arc::new(|| serde_json::Value::from(crate::http::SERVER_VERSION))),
        backend_name: Some(Arc::new(move || {
            w_name.upgrade().map_or_else(|| "none".to_owned(), |s| s.backend_name())
        })),
        worker_command: None,
        worker_cwd: None,
    };
    implexity_jobs::api::install_host_hooks(service.as_ref(), hooks);
    crate::managers::install(service);
    service.set_job_lister(Arc::new(move || {
        w_list
            .upgrade()
            .map_or_else(|| serde_json::json!({"jobs": []}), |s| implexity_jobs::api::jobs_list(s.as_ref()))
    }));
}
