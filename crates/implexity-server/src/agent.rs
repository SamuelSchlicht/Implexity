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



use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, Weak};

use implexity_agent::host::{HostGuard, ManagedCapsule, ManagedSupervisor};
use implexity_agent::{AgentError, AgentHost, AgentManagers, AgentModel, AgentResult, HostOp};
use implexity_core::route_tables::RouteService;
use serde_json::Value;

use crate::http::{Reply, Request, RouteError};
use crate::routes::{Registry, RouteDeclarationError};
use crate::service::Service;

pub struct ServiceAgentHost {
    service: Weak<Service>,
}

#[derive(Default)]
pub struct InstalledManagers(RwLock<Option<Arc<dyn AgentManagers>>>);

const MANAGERS: &str = "kernel.agent_managers";

fn installed(service: &Service) -> Option<Arc<dyn AgentManagers>> {
    let slot = service.extension_any(MANAGERS, &|| Arc::new(InstalledManagers::default()));
    let slot = slot.downcast_ref::<InstalledManagers>()?;
    slot.0.read().unwrap_or_else(PoisonError::into_inner).clone()
}

pub fn install_managers(service: &Service, managers: Arc<dyn AgentManagers>) {
    let slot = service.extension_any(MANAGERS, &|| Arc::new(InstalledManagers::default()));
    if let Some(slot) = slot.downcast_ref::<InstalledManagers>() {
        *slot.0.write().unwrap_or_else(PoisonError::into_inner) = Some(managers);
    }
}

struct LeaseHold(Arc<implexity_io::heavy_lease::HeavyOperationLease>);

impl Drop for LeaseHold {
    fn drop(&mut self) {
        let _ = self.0.release();
    }
}

fn unavailable(what: &str) -> AgentError {
    AgentError::refused(format!(
        "{what} is not available in this service: its manager (WP-11) is not installed"
    ))
}

impl ServiceAgentHost {
    fn service(&self) -> AgentResult<Arc<Service>> {
        self.service.upgrade().ok_or_else(|| AgentError::failed("the service has stopped"))
    }

    fn managers(&self) -> Option<Arc<dyn AgentManagers>> {
        self.service.upgrade().and_then(|s| installed(&s))
    }
}

impl AgentHost for ServiceAgentHost {
    fn state_dir(&self) -> PathBuf {
        self.service.upgrade().map_or_else(
            || crate::service::state_directory(None),
            |s| s.state_dir().unwrap_or_else(|_| crate::service::state_directory(s.workspace())),
        )
    }

    fn dynamic_results_available(&self) -> bool {
        self.service.upgrade().is_some_and(|s| crate::rust_extension::available(&s))
    }

    fn dynamic_catalogue(&self) -> implexity_runtime::dynamic_frames::catalogue::Catalogue {
        match self.service.upgrade() {
            Some(s) => crate::rust_extension::catalogue(&s),
            None => implexity_runtime::dynamic_frames::catalogue::Catalogue {
                service_root: Some(
                    self.state_dir().join(implexity_runtime::dynamic_frames::catalogue::SERVICE_ROOT),
                ),
                jobs_root: None,
            },
        }
    }

    fn call(&self, op: HostOp) -> AgentResult<Value> {
        match self.managers() {
            Some(m) => m.call(op),
            None => Err(op.unavailable()),
        }
    }

    fn model(&self) -> Option<Arc<dyn AgentModel>> {
        self.managers().and_then(|m| m.model())
    }

    fn detached_model(&self, document: &Value) -> AgentResult<Arc<dyn AgentModel>> {
        match self.managers() {
            Some(m) => m.detached_model(document),
            None => Err(AgentError::refused(
                "implicit.api.DetachedModelView is not available in this service: its manager (WP-12) is not installed",
            )),
        }
    }

    fn result_store(
        &self,
        artifact_id: &str,
    ) -> AgentResult<Arc<dyn implexity_render::artifact::ResultArtifactStore + Send + Sync>> {
        match self.managers() {
            Some(m) => m.result_store(artifact_id),
            None => Err(unavailable("implicit.result_arrays_http.resolve_store")),
        }
    }

    fn packages_status(&self) -> AgentResult<Value> {
        let s = self.service()?;
        Ok(s.packages().session().status()?)
    }

    fn packages_change(
        &self,
        package: &str,
        operation: &str,
        expected: Option<&Value>,
    ) -> AgentResult<Value> {
        let s = self.service()?;
        Ok(s.packages().session().change(package, operation, expected)?)
    }

    fn imported_providers(&self, request: Option<&Value>, check: bool) -> AgentResult<Value> {
        let s=self.service()?;let session=s.packages().imports();
        Ok(match request {None=>session.status(),Some(request)=>if check {session.verify(request)?}else{session.change(request)?}})
    }

    fn case_snapshot(&self) -> Option<(Value, Option<Value>)> {
        let s = self.service.upgrade()?;
        let name = std::env::var("IMPLEXITY_PHYSICS_BACKEND").ok().filter(|v| !v.trim().is_empty());
        let reg = &implexity_core::registries::global().contributions;
        let doc = implexity_core::backends::current_case(reg, s.as_ref(), name.as_deref())?;
        let revision = doc.get("revision").cloned();
        Some((doc, revision))
    }

    fn eval_lock(&self) -> AgentResult<HostGuard> {
        let lease = self.service()?.eval_lock().map_err(|e| AgentError::failed(e.to_string()))?;
        match lease.acquire(true, None) {
            Ok(true) => Ok(Box::new(LeaseHold(lease))),
            Ok(false) => Err(AgentError::failed("the heavy-operation lease could not be acquired")),
            Err(e) => Err(AgentError::failed(e.0)),
        }
    }

    fn state_lock(&self) -> HostGuard {
        match self.service.upgrade() {
            Some(s) => Box::new(s.hold_state()),
            None => Box::new(()),
        }
    }

    fn managed_supervisor(&self, private_root: &Path) -> AgentResult<Arc<dyn ManagedSupervisor>> {
        match self.managers() {
            Some(m) => m.managed_supervisor(private_root),
            None => Err(unavailable("implicit.managed_evaluation.ManagedEvaluationManager")),
        }
    }

    fn prepare_managed_evaluation(&self, kind: &str, request: Value) -> AgentResult<Arc<dyn ManagedCapsule>> {
        match self.managers() {
            Some(m) => m.prepare_managed_evaluation(kind, request),
            None => Err(unavailable("implicit.api.ModelOptimizeManager.prepare_managed_evaluation")),
        }
    }

    fn viewer_capture_origin(&self) -> Option<String> {
        self.service.upgrade().and_then(|s| s.viewer_capture_origin().map(str::to_owned))
    }

    fn viewer_asset(&self, name: &str) -> Option<Vec<u8>> {
        self.service.upgrade().and_then(|s| s.viewer().read(name))
    }
}



pub fn routes() -> Result<Registry, RouteDeclarationError> {
    let mut r = Registry::new();
    for spec in implexity_agent::api::ENDPOINTS {
        let (method, pattern) = spec.split_once(' ').unwrap_or(("GET", spec));
        let policy =
            if method == "GET" { crate::routes::BodyPolicy::None } else { crate::routes::BodyPolicy::Json };
        r.route(
            method,
            pattern,
            policy,
            "",
            "implexity.agent.api",
            Arc::new(move |req: &Request| -> Result<Reply, RouteError> {
                let weak = Arc::downgrade(&req.service);
                let factory: implexity_agent::api::HostFactory = Arc::new(move |_svc: &dyn RouteService| {
                    Arc::new(ServiceAgentHost { service: weak.clone() }) as Arc<dyn AgentHost>
                });
                let neutral = crate::contributed::neutral_request(req);
                let result = implexity_agent::api::manager(req.service.as_ref(), &factory)
                    .and_then(|m| implexity_agent::api::dispatch(spec, &m, &neutral));
                Ok(crate::contributed::server_reply(implexity_agent::api::reply_of(result)))
            }),
        )?;
    }
    Ok(r)
}
