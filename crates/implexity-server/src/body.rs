// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use implexity_core::route_tables::RouteTable;
use implexity_geometry::preview::{Design, RealEvaluator};
use implexity_mesh::bodyapi::{
    BodyDone, BodyError, BodyHost, BodyManager, BodyWork, PartDomain, SharedEvaluator, SharedRecipeWriter,
};
use implexity_mesh::bodyexport::{BodyEvaluator, DesignSnapshot, NativeSdf, SdfKernel};
use implexity_mesh::model_view::LiveGuard;
use ndarray::ArrayD;
use serde_json::{Value, json};

use crate::geometry::GeometryPreview;
use crate::http::{Reply, Request, RouteError};
use crate::jobs::JobError;
use crate::preview::PreviewResult;
use crate::routes::{BodyPolicy, Registry, RouteDeclarationError};
use crate::service::Service;

pub const EXTENSION: &str = "kernel.body";

pub const ENDPOINTS: [(&str, &str, BodyPolicy); 5] = [
    ("GET", "/v1/body/jobs", BodyPolicy::None),
    ("GET", "/v1/body/jobs/<id>", BodyPolicy::None),
    ("POST", "/v1/body", BodyPolicy::Json),
    ("POST", "/v1/body/estimate", BodyPolicy::Json),
    ("POST", "/v1/body/jobs/<id>", BodyPolicy::Json),
];

pub struct ServiceBodyHost {
    service: Weak<Service>,
}

fn physics_name() -> Option<String> {
    std::env::var("IMPLEXITY_PHYSICS_BACKEND").ok().filter(|s| !s.trim().is_empty())
}

impl ServiceBodyHost {
    fn service(&self) -> Option<Arc<Service>> {
        self.service.upgrade()
    }

    fn geometry(&self) -> Option<Arc<GeometryPreview>> {
        self.service().and_then(|s| s.geometry().cloned())
    }
}

struct LeaseHold(Option<Arc<implexity_io::heavy_lease::HeavyOperationLease>>);

impl Drop for LeaseHold {
    fn drop(&mut self) {
        if let Some(l) = &self.0 {
            let _ = l.release();
        }
    }
}

fn job_error(e: BodyError) -> JobError {
    match e {
        BodyError::Case(p) => JobError::Case(p),
        BodyError::NotFound(m) | BodyError::Key(m) => JobError::failed("KeyError", m),
        BodyError::Failed(m) => JobError::failed("RuntimeError", m),
    }
}

fn body_error(e: JobError) -> BodyError {
    match e {
        JobError::Case(p) => BodyError::Case(p),
        JobError::Failed { kind, message } if kind == "KeyError" => BodyError::Key(message),
        other => BodyError::Failed(other.to_string()),
    }
}

impl BodyHost for ServiceBodyHost {
    fn current_case(&self) -> Value {
        let Some(service) = self.service() else { return json!({}) };
        let reg = &implexity_core::registries::global().contributions;
        implexity_core::backends::current_case(reg, service.as_ref(), physics_name().as_deref())
            .unwrap_or_else(|| json!({}))
    }

    fn part_domain(&self) -> Result<Option<PartDomain>, String> {
        let Some(service) = self.service() else { return Ok(None) };
        let reg = &implexity_core::registries::global().contributions;
        let Some(store) =
            implexity_core::backends::part_domain(reg, service.as_ref(), physics_name().as_deref())
        else {
            return Ok(None);
        };
        store.downcast::<PartDomain>().map(|d| Some((*d).clone())).map_err(|_| {
            "the physics backend's part store is not an implexity_mesh::bodyapi::PartDomain".into()
        })
    }

    fn domain_mm(&self) -> [f64; 3] {
        self.geometry()
            .and_then(|g| g.design().meta().ok())
            .and_then(|m| m.get("domain_mm").cloned())
            .and_then(|v| serde_json::from_value::<[f64; 3]>(v).ok())
            .unwrap_or([0.0; 3])
    }

    fn served_evaluator(&self) -> Result<SharedEvaluator, BodyError> {
        match self.geometry() {
            Some(g) => Ok(g),
            None => Err(BodyError::Case(vec![format!(
                "the served geometry backend {} does not implement the two-rate sampling internals \
                 (evalbase.TwoRateMixin) a body extraction needs; body estimates and exports are available \
                 on the 'synthetic' and 'real' backends",
                implexity_core::py_repr::repr_str(&self.backend_name())
            )])),
        }
    }

    fn backend_name(&self) -> String {
        self.service().map_or_else(|| "none".into(), |s| s.backend_name())
    }

    fn load_design(&self, path: &Path) -> Result<SharedEvaluator, BodyError> {
        let meta = self
            .geometry()
            .and_then(|g| g.design().meta().ok())
            .ok_or_else(|| BodyError::Failed("no served design to take the domain from".into()))?;
        let dom = meta
            .get("domain_mm")
            .cloned()
            .and_then(|v| serde_json::from_value::<[f64; 3]>(v).ok())
            .unwrap_or([0.0; 3]);
        let grid =
            meta.get("design_grid").cloned().and_then(|v| serde_json::from_value::<[usize; 3]>(v).ok());
        let period = meta.get("period_mm").and_then(Value::as_f64).unwrap_or(4.0);
        let design = Arc::new(
            Design::from_npz(path, dom, grid, period).map_err(|e| BodyError::Failed(e.to_string()))?,
        );
        let ev =
            RealEvaluator::new(Arc::clone(&design), None).map_err(|e| BodyError::Failed(e.to_string()))?;
        Ok(Arc::new(GeometryPreview::new(Arc::new(ev), design)))
    }

    fn design_params(&self, _ev: &dyn BodyEvaluator, snap: &DesignSnapshot) -> BTreeMap<String, ArrayD<f64>> {
        let Ok(params) = Arc::clone(&snap.params).downcast::<implexity_geometry::preview::Params>() else {
            return BTreeMap::new();
        };
        params
            .iter()
            .filter_map(|(k, c)| {
                ArrayD::from_shape_vec(ndarray::IxDyn(&c.shape), c.data.clone()).ok().map(|a| (k.clone(), a))
            })
            .collect()
    }

    fn sdf_kernel(&self) -> Option<Arc<dyn SdfKernel + Send + Sync>> {
        Some(Arc::new(NativeSdf))
    }

    fn recipe_writer(&self) -> Option<SharedRecipeWriter> {
        let reg = &implexity_core::registries::global().contributions;
        let name = implexity_core::backends::selected_physics_name(reg, physics_name().as_deref())?;
        reg.get("body_recipes", &name)
            .ok()
            .flatten()
            .and_then(|v| v.downcast::<SharedRecipeWriter>())
            .map(|w| (*w).clone())
    }

    fn eval_lock(&self) -> LiveGuard<'_> {
        let lease = self.service().and_then(|s| s.eval_lock().ok());
        let held = lease.filter(|l| l.acquire(true, None).unwrap_or(false));
        Box::new(LeaseHold(held))
    }

    fn submit(&self, channel: &str, seq: i64, work: BodyWork, done: BodyDone) {
        let Some(service) = self.service() else {
            done(Err(BodyError::Failed("the service has stopped".into())));
            return;
        };
        let job = service.pool.submit(
            channel,
            seq,
            Box::new(move |cancel| {
                let probe = || cancel.is_cancelled();
                let report = work(&probe).map_err(job_error)?;
                Ok(PreviewResult::from_meta(report))
            }),
        );
        let spawned = std::thread::Builder::new().name(format!("body-{channel}-{seq}")).spawn(move || {
            while !job.wait(Duration::from_hours(1)) {}
            let outcome = job.take_outcome().map_or_else(
                || Err(BodyError::Failed("body job outcome already consumed".into())),
                |(r, _)| r.map(|p| p.meta).map_err(body_error),
            );
            done(outcome);
        });
        drop(spawned);
    }

    fn broadcast(&self, event: Value) {
        if let Some(s) = self.service() {
            s.broadcast(&event);
        }
    }

    fn service_version(&self) -> Option<String> {
        Some(crate::http::SERVER_VERSION.to_owned())
    }
}

pub struct BodyEndpoints {
    pub manager: Arc<BodyManager<ServiceBodyHost>>,
    pub table: RouteTable,
}

fn endpoints(service: &Arc<Service>) -> Result<Arc<BodyEndpoints>, String> {
    let factory = || -> Arc<dyn std::any::Any + Send + Sync> {
        let built = (|| -> Result<BodyEndpoints, String> {
            let dir = service.state_dir().map_err(|e| e.to_string())?;
            let host = Arc::new(ServiceBodyHost { service: Arc::downgrade(service) });
            let manager = BodyManager::new(host, &dir).map_err(|e| e.to_string())?;
            let table = implexity_mesh::bodyapi::route_table(&manager).map_err(|e| e.to_string())?;
            Ok(BodyEndpoints { manager, table })
        })();
        match built {
            Ok(b) => Arc::new(b),
            Err(e) => Arc::new(e),
        }
    };
    let entry = service.extension_any(EXTENSION, &factory);
    if let Ok(b) = Arc::clone(&entry).downcast::<BodyEndpoints>() {
        return Ok(b);
    }
    Err(entry.downcast::<String>().map_or_else(|_| "body manager unavailable".into(), |e| (*e).clone()))
}



pub fn routes() -> Result<Registry, RouteDeclarationError> {
    let mut r = Registry::new();
    for (method, pattern, policy) in ENDPOINTS {
        let spec = format!("{method} {pattern}");
        let doc = crate::kernel_routes::PENDING
            .iter()
            .find(|p| p.method == method && p.pattern == pattern)
            .map_or("", |p| p.doc);
        r.route(
            method,
            pattern,
            policy,
            doc,
            "implexity.bodyapi",
            Arc::new(move |req: &Request| -> Result<Reply, RouteError> {
                let body = endpoints(&req.service).map_err(|e| RouteError::internal("RuntimeError", e))?;
                let Some(decl) = body.table.all().iter().find(|d| d.spec() == spec) else {
                    return Err(RouteError::internal("RuntimeError", format!("{spec} is not served")));
                };
                crate::contributed::call_neutral(decl, req)
            }),
        )?;
    }
    Ok(r)
}
