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
