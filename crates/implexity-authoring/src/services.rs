// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use implexity_core::route_tables::RouteService;
use implexity_geometry::engineering_history::EngineeringHistory;
use implexity_geometry::eval::{self as EV, EvalOptions};
use implexity_geometry::value::{ArrayData, NdArray, ParamValue};
use implexity_runtime::provider_problem_document as ppd;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::incremental_fields::IncrementalProgressiveFieldStore;
use crate::interaction_runtime::{InteractionRuntime, RuntimeStores};
use crate::manipulation::{ManipulationManager, SharedHistory};
use crate::model_manager::{HistoryStore, ModelManager, to_geometry};
use crate::optimization_setup::OptimizationSetupStore;
use crate::problem::{ProblemCall, ProblemStore};
use crate::py::{canonical_unicode, dot3, jf, norm3, py_str, repr, sha256_hex, truthy};
use crate::sync::lock;

pub const SERVICES_KEY: &str = "implexity.authoring.services";
pub const HOOKS_KEY: &str = "implexity.authoring.service_hooks";

pub type JsonHook = Arc<dyn Fn() -> AResult<Value> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ServiceHooks {
    pub package_generation: Option<JsonHook>,
    pub endpoints: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
}

#[derive(Default)]
struct HooksSlot(RwLock<ServiceHooks>);

fn hooks_slot(svc: &dyn RouteService) -> Arc<dyn Any + Send + Sync> {
    svc.extension_state(HOOKS_KEY, &|| Arc::new(HooksSlot::default()))
}

pub fn install_service_hooks(svc: &dyn RouteService, hooks: ServiceHooks) {
    let slot = hooks_slot(svc);
    if let Some(s) = slot.downcast_ref::<HooksSlot>() {
        *s.0.write().unwrap_or_else(std::sync::PoisonError::into_inner) = hooks;
    }
}

fn hooks(svc: &dyn RouteService) -> ServiceHooks {
    let slot = hooks_slot(svc);
    slot.downcast_ref::<HooksSlot>()
        .map(|s| s.0.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
        .unwrap_or_default()
}

pub trait ServiceContext {
    fn state_dir(&self) -> PathBuf;
    fn current_case(&self) -> Option<Value>;
    fn physics_backend(&self) -> Option<String>;

    fn package_generation(&self) -> AResult<Value>;
    fn endpoints(&self) -> Vec<String>;

    fn authoring(&self) -> AResult<Arc<Authoring>>;
}

fn backend_env() -> Option<String> {
    std::env::var("IMPLEXITY_PHYSICS_BACKEND").ok().filter(|v| !v.trim().is_empty())
}

pub struct RouteCtx<'a>(pub &'a dyn RouteService);

#[derive(Default)]
struct ServicesSlot(Mutex<Option<Arc<Authoring>>>);


pub fn authoring(svc: &dyn RouteService) -> AResult<Arc<Authoring>> {
    let slot = svc.extension_state(SERVICES_KEY, &|| Arc::new(ServicesSlot::default()));
    let slot = slot
        .downcast_ref::<ServicesSlot>()
        .ok_or_else(|| AuthoringError::runtime("RuntimeError", "authoring slot type mismatch"))?;
    let mut g = lock(&slot.0);
    if let Some(a) = g.as_ref() {
        return Ok(Arc::clone(a));
    }
    let a = Arc::new(Authoring::open(&svc.state_dir())?);
    *g = Some(Arc::clone(&a));
    Ok(a)
}

impl ServiceContext for RouteCtx<'_> {
    fn state_dir(&self) -> PathBuf {
        self.0.state_dir()
    }

    fn current_case(&self) -> Option<Value> {
        let reg = &implexity_core::registries::global().contributions;
        implexity_core::backends::current_case(reg, self.0.as_any(), backend_env().as_deref())
    }

    fn physics_backend(&self) -> Option<String> {
        let reg = &implexity_core::registries::global().contributions;
        implexity_core::backends::selected_physics_name(reg, backend_env().as_deref())
    }

    fn package_generation(&self) -> AResult<Value> {
        match hooks(self.0).package_generation {
            Some(h) => h(),
            None => Err(AuthoringError::runtime(
                "RuntimeError",
                "the physics package session is not available in this service",
            )),
        }
    }

    fn endpoints(&self) -> Vec<String> {
        if let Some(h) = hooks(self.0).endpoints {
            return h();
        }
        let mut out = crate::api::route_table().map(|t| t.endpoints()).unwrap_or_default();
        for (_, t) in implexity_core::route_tables::contributed_tables(
            &implexity_core::registries::global().contributions,
        ) {
            out.extend(t.endpoints());
        }
        out.sort();
        out.dedup();
        out
    }

    fn authoring(&self) -> AResult<Arc<Authoring>> {
        authoring(self.0)
    }
}

pub struct Authoring {
    pub models: Arc<ModelManager>,
    pub problems: Arc<ProblemStore>,
    pub manipulation: Arc<ManipulationManager>,
    pub interactions: Arc<InteractionRuntime>,
    pub setup: Arc<OptimizationSetupStore>,
    pub history: Arc<EngineeringHistory<HistoryStore>>,
    fields: OnceLock<Arc<IncrementalProgressiveFieldStore>>,
    pub guided_reviews: Mutex<Vec<crate::guided_setup::GuidedReview>>,
}

impl Authoring {

    pub fn open(state_dir: &std::path::Path) -> AResult<Self> {
        let models = Arc::new(ModelManager::new(state_dir)?);
        let problems = Arc::new(ProblemStore::new(models.dir())?);
        let history = Arc::new(EngineeringHistory::new(HistoryStore(Arc::clone(&models))));
        let manipulation = Arc::new(ManipulationManager::new(Arc::clone(&models), None));
        let setup = Arc::new(OptimizationSetupStore::new(models.dir())?);
        Ok(Self {
            models,
            problems,
            manipulation,
            interactions: Arc::new(InteractionRuntime::new(300.0)),
            setup,
            history,
            fields: OnceLock::new(),
            guided_reviews: Mutex::new(Vec::new()),
        })
    }


    pub fn edit_graph(&self, ctx: &dyn ServiceContext, request: &Value) -> AResult<Value> {
        let stores = NativeStores { ctx, a: self };
        self.manipulation.run_reserved_idle("edit the graph", None, || {
            let _guard = self.models.live_lock();
            self.interactions.record_mutation(
                &stores,
                &mut || self.models.edit(request),
                "Edit geometry graph",
                "graph_edit",
            )
        })
    }

    pub fn field_store(&self) -> AResult<Arc<IncrementalProgressiveFieldStore>> {
        if let Some(s) = self.fields.get() {
            return Ok(Arc::clone(s));
        }
        let mb: usize = std::env::var("IMPLEXITY_FIELD_STREAM_CACHE_MB")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(256);
        let store = Arc::new(IncrementalProgressiveFieldStore::new(
            &self.models.dir().join("field_stream_v22"),
            mb.max(16) * 1024 * 1024,
            8192,
        )?);
        Ok(Arc::clone(self.fields.get_or_init(|| store)))
    }


    pub fn model_identity(&self) -> AResult<Value> {
        let m = self.models.require()?;
        Ok(json!({"structure_id": m.structure_id(), "content_id": m.content_id(), "sha256": m.sha256()?}))
    }


    pub fn problem_call(&self, ctx: &dyn ServiceContext) -> AResult<ProblemCall> {
        Ok(ProblemCall {
            model_identity: Some(self.model_identity()?),
            case_doc: ctx.current_case(),
            physics_backend: ctx.physics_backend(),
            interface: None,
            actor: None,
        })
    }
}

pub struct NativeStores<'a> {
    pub ctx: &'a dyn ServiceContext,
    pub a: &'a Authoring,
}

impl NativeStores<'_> {
    #[must_use]
    pub fn shared(&self) -> SharedHistory<'_> {
        SharedHistory { runtime: &self.a.interactions, stores: self }
    }
}

fn stable_identity(value: &Value) -> Value {
    let mut native = Map::new();
    for key in ["revision", "content_id", "problem_id", "sha256"] {
        if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
            native.insert(key.into(), v.clone());
        }
    }
    if !native.is_empty() {
        return json!({"native": native});
    }
    json!({"canonical_sha256": sha256_hex(canonical_unicode(value).as_bytes())})
}

fn param_flat_json(a: &NdArray) -> Vec<Value> {
    match a.data() {
        ArrayData::F64(v) => v.iter().map(|x| jf(*x)).collect(),
        ArrayData::F32(v) => v.iter().map(|x| jf(f64::from(*x))).collect(),
        ArrayData::I64(v) => v.iter().map(|x| json!(x)).collect(),
        ArrayData::I32(v) => v.iter().map(|x| json!(x)).collect(),
        ArrayData::I16(v) => v.iter().map(|x| json!(x)).collect(),
        ArrayData::I8(v) => v.iter().map(|x| json!(x)).collect(),
        ArrayData::U8(v) => v.iter().map(|x| json!(x)).collect(),
        ArrayData::Bool(v) => v.iter().map(|x| json!(x)).collect(),
    }
}

fn ierr(message: impl Into<String>) -> AuthoringError {
    AuthoringError::runtime("InteractionHttpError", message)
}

impl RuntimeStores for NativeStores<'_> {
    fn get_document(&self) -> AResult<Value> {
        self.a.models.snapshot()
    }

    fn set_document(&self, document: &Value) -> AResult<Value> {
        self.a.models.put(document)
    }

    fn get_problem(&self) -> AResult<Value> {
        let call = self.a.problem_call(self.ctx)?;
        let mut record = self.a.problems.get(&call)?;
        let schema = record.get("schema").and_then(Value::as_str).unwrap_or_default().to_string();
        if [ppd::SCHEMA, "implexity-differentiable-problem/1", "implexity-differentiable-problem/2"]
            .contains(&schema.as_str())
            && let Some(o) = record.as_object_mut()
        {
            o.shift_remove("updated");
        }
        Ok(record)
    }

    fn set_problem(&self, problem: &Value) -> AResult<Value> {
        let call = self.a.problem_call(self.ctx)?;
        let mut declaration = problem.clone();
        if problem.get("schema").and_then(Value::as_str) == Some(crate::problem::SCHEMA)
            && problem.get("setup").and_then(|v| v.get("mode")).and_then(Value::as_str) == Some("unset")
            && problem.get("backend").is_some_and(Value::is_null)
            && problem.get("analysis").is_some_and(Value::is_null)
            && ["responses", "result_fields"].iter().all(|key| problem.get(*key).and_then(Value::as_array).is_some_and(Vec::is_empty))
            && ["name", "doc"].iter().all(|key| problem.get(*key).is_some_and(Value::is_string))
            && problem.get("model").is_some_and(Value::is_object)
            && problem.as_object().is_some_and(|record| record.keys().all(|key| {
                ["schema", "name", "backend", "model", "setup", "analysis", "responses", "result_fields", "doc", "problem_id", "updated"].contains(&key.as_str())
            }))
            && problem.get("setup").and_then(Value::as_object).is_some_and(|setup| {
                setup.len() == 8 && setup.iter().all(|(key, value)| {
                    key == "mode" || (["case_schema", "case_name", "case_sha256", "materials", "regions", "loads", "boundary_conditions"].contains(&key.as_str()) && value.is_null())
                })
            })
        {
            declaration = json!({"schema":crate::problem::SCHEMA, "name":problem["name"], "doc":problem["doc"]});
        }
        if declaration.get("schema").and_then(Value::as_str) == Some(ppd::SCHEMA)
            && declaration.get("identity").is_some()
        {
            declaration = ppd::declaration_for_rebind(&declaration).map_err(crate::error::cae)?;
        }
        self.a.problems.put(&declaration, &call)
    }

    fn get_revision(&self) -> AResult<String> {
        let identities =
            json!([stable_identity(&self.get_document()?), stable_identity(&self.get_problem()?)]);
        Ok(sha256_hex(canonical_unicode(&identities).as_bytes()))
    }

    fn record_event(&self, kind: &str, label: &str, details: &Value) -> Option<AResult<Value>> {
        let d = details.as_object();
        Some(self.a.history.append(kind, label, d, None, None).map_err(AuthoringError::from))
    }

    fn refine_surface(&self, request: &Value) -> Option<AResult<Value>> {
        Some(native_refine(self.a, request))
    }

    fn get_spatial_field(&self, field_id: &str) -> Option<AResult<Value>> {
        Some(native_spatial_field(self.a, field_id))
    }

    fn run_authority_transaction(
        &self,
        operation: &str,
        callback: &mut dyn FnMut() -> AResult<Value>,
    ) -> AResult<Value> {
        self.a.manipulation.run_reserved_idle(operation, None, callback)
    }

    fn prepare_problem_revision(&self, before: &Value, revised: &Value) -> Option<AResult<(Value, usize)>> {
        Some(ppd::prepare_provider_revisions(before, revised).map_err(crate::error::cae))
    }

    fn external_edit_active(&self) -> bool {
        self.a.manipulation.has_active_geometry_writer()
    }

    fn run_history_read(&self, callback: &mut dyn FnMut() -> AResult<Value>) -> AResult<Value> {
        self.a.manipulation.observe_geometry_authority(callback)
    }
}


pub fn native_refine(a: &Authoring, request: &Value) -> AResult<Value> {
    let model = a.models.require()?;
    let supplied = request.get("model_identity").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
    let live_s = model.structure_id().unwrap_or_else(|| "None".into());
    let live_c = model.content_id().unwrap_or_else(|| "None".into());
    for (key, live) in [("structure_id", &live_s), ("content_id", &live_c)] {
        if let Some(v) = supplied.get(key).filter(|v| !v.is_null())
            && py_str(v) != *live
        {
            return Err(ierr("picked surface belongs to a stale model identity"));
        }
    }
    let p =
        crate::py::Arr::from_opt(request.get("point_mm")).ok().filter(|p| p.shape == [3] && p.all_finite());
    let Some(p) = p else { return Err(ierr("surface refinement requires one finite model-space point")) };
    let mut point = [p.data[0], p.data[1], p.data[2]];
    let Some(node) = model.root() else { return Err(ierr("the implicit model has no root surface")) };
    let opts = EvalOptions::exact();
    let value_at = |x: [f64; 3]| -> AResult<f64> {
        Ok(EV::eval_points(&node, &[x], &opts)?.first().copied().unwrap_or(f64::NAN))
    };
    let grad_at = |x: [f64; 3]| -> AResult<[f64; 3]> {
        Ok(EV::grad_x(&node, &[x], &opts)?.first().copied().unwrap_or([f64::NAN; 3]))
    };
    let tolerance = 1.0e-7;
    for _ in 0..12 {
        let value = value_at(point)?;
        let g = grad_at(point)?;
        let norm2 = dot3(g, g);
        if !value.is_finite() || !g.iter().all(|v| v.is_finite()) || norm2 <= 1.0e-20 {
            return Err(ierr("exact implicit projection became singular"));
        }
        if value.abs() <= tolerance {
            break;
        }
        let s = (value / norm2).clamp(-2.0, 2.0);
        point = [point[0] - s * g[0], point[1] - s * g[1], point[2] - s * g[2]];
    }
    let residual = value_at(point)?.abs();
    if residual > tolerance || residual.is_nan() {
        return Err(ierr("display hit did not converge to the exact implicit surface"));
    }
    let g = grad_at(point)?;
    let n = norm3(g);
    let normal = [g[0] / n, g[1] / n, g[2] / n];
    let mut clip = request
        .get("clip_evidence")
        .filter(|v| truthy(v))
        .cloned()
        .unwrap_or_else(|| json!({"active": false, "plane": null, "hit_on_clip_cap": false}));
    let plane = if clip.get("active").is_some_and(truthy) { clip.get("plane").cloned() } else { None };
    if let Some(pl) = plane.filter(|p| p.is_object() && p.get("n").is_some_and(|n| !n.is_null())) {
        let nv = crate::py::Arr::from_opt(pl.get("n"))?;
        let d = pl.get("d").map_or(Ok(0.0), crate::py::py_float)?;
        let side = crate::py::np_dot(&nv.data, &point) - d;
        if side > tolerance {
            return Err(ierr("exact surface point lies outside the active clip plane"));
        }
    }
    if let Some(o) = clip.as_object_mut() {
        o.insert("hit_on_clip_cap".into(), json!(false));
    }
    Ok(json!({"point_mm": point.map(jf), "normal": normal.map(jf), "exact": true, "approximate": false,
        "model_identity": {"structure_id": model.structure_id(), "content_id": model.content_id(), "revision": model.sha256()?},
        "clip_evidence": clip, "residual_mm": jf(residual)}))
}


#[allow(clippy::too_many_lines)]
pub fn native_spatial_field(a: &Authoring, field_id: &str) -> AResult<Value> {
    let model = a.models.require()?;
    let document = model.to_doc()?;
    if implexity_geometry::lattice::component_field::split_component_id(field_id)?.is_some() {
        let read = crate::field_interaction::read_spatial_field(&document, field_id)?;
        let metadata = read.metadata.clone().unwrap_or_else(|| json!({}));
        let reg = read.grid.registration.to_wire();
        return Ok(json!({
            "shape": read.grid.shape, "values": read.values.iter().map(|v| jf(*v)).collect::<Vec<_>>(),
            "grid": read.grid.serialise(),
            "bounds": metadata.get("bounds").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({})),
            "protected_masks": metadata.get("protected_masks").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({})),
            "metadata": metadata,
            "identity": {"structure_id": model.structure_id(), "content_id": model.content_id(),
                "revision": model.sha256()?, "registration_id": reg.get("registration_id").cloned().unwrap_or(Value::Null),
                "payload_sha256": read.entry.as_ref().and_then(|e| e.get("sha256")).cloned().unwrap_or(Value::Null)},
        }));
    }
    let mut hit: Option<(String, String)> = None;
    if let Some(nodes) = document.get("nodes").and_then(Value::as_object) {
        'outer: for (nid, node_doc) in nodes {
            if let Some(params) = node_doc.get("params").and_then(Value::as_object) {
                for (pname, spec) in params {
                    if spec.is_object() && spec.get("array").map(py_str).as_deref() == Some(field_id) {
                        hit = Some((nid.clone(), pname.clone()));
                        break 'outer;
                    }
                }
            }
        }
    }
    let Some((node_id, parameter)) = hit else {
        return Err(ierr(format!(
            "spatial field {} is not bound to the authoritative model",
            repr(&json!(field_id))
        )));
    };
    let node = model
        .node_table()
        .get(&node_id)
        .cloned()
        .ok_or_else(|| AuthoringError::Key(repr(&json!(node_id))))?;
    let values = match node.param(&parameter) {
        Some(ParamValue::Array(arr)) => (**arr).clone(),
        _ => {
            return Err(ierr(format!(
                "spatial field {} is not one finite three-dimensional field",
                repr(&json!(field_id))
            )));
        }
    };
    if values.ndim() != 3 || !values.to_f64_vec().iter().all(|v| v.is_finite()) {
        return Err(ierr(format!(
            "spatial field {} is not one finite three-dimensional field",
            repr(&json!(field_id))
        )));
    }
    let imp = document.pointer("/meta/implexity").cloned().unwrap_or_else(|| json!({}));
    let modern_meta =
        imp.pointer("/spatial_fields").and_then(|s| s.get(field_id)).cloned().unwrap_or_else(|| json!({}));
    let topology = imp.get("topology").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
    let node_doc = document["nodes"][&node_id].clone();
    let source =
        node_doc.pointer("/attrs/source").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
    let mut registration =
        if modern_meta.is_object() { modern_meta.get("grid").cloned().filter(truthy) } else { None };
    if registration.is_none() && topology.get("array_key").map(py_str).as_deref() == Some(field_id) {
        registration = topology.get("registration").cloned().filter(truthy);
    }
    if registration.is_none() {
        registration = source.get("registration").cloned().filter(truthy);
    }
    let registration = if let Some(r) = registration {
        r
    } else {
        let origin = node.param("origin").and_then(|p| p.to_f64_array().ok());
        let spacing = node.param("spacing").and_then(|p| p.to_f64_array().ok());
        let (Some((_, o)), Some((_, s))) = (origin, spacing) else {
            return Err(ierr(format!("spatial field {} lacks exact registration", repr(&json!(field_id)))));
        };
        json!({"schema": "implexity-grid-registration/1", "shape": values.shape(),
            "origin": o.iter().take(3).map(|v| jf(*v)).collect::<Vec<_>>(),
            "basis": [[jf(s[0]), 0.0, 0.0], [0.0, jf(s[1]), 0.0], [0.0, 0.0, jf(s[2])]],
            "centering": "cell", "axis_order": "xyz", "frame": "model"})
    };
    let entry =
        document.get("arrays").and_then(|arr| arr.get(field_id)).cloned().unwrap_or_else(|| json!({}));
    let native_sha = sha256_hex(&values.to_le_bytes());
    let declared = entry.get("sha256").filter(|v| truthy(v)).map(py_str);
    if let Some(d) = &declared
        && *d != native_sha
    {
        return Err(ierr(format!(
            "spatial field {} has stale native payload identity",
            repr(&json!(field_id))
        )));
    }
    let bounds = modern_meta.get("bounds").filter(|v| truthy(v)).cloned().unwrap_or_else(|| {
        json!({"lower": topology.get("lower").cloned().unwrap_or(Value::Null), "upper": topology.get("upper").cloned().unwrap_or(Value::Null)})
    });
    Ok(json!({
        "shape": values.shape(), "values": param_flat_json(&values), "grid": registration,
        "bounds": bounds,
        "protected_masks": modern_meta.get("protected_masks").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({})),
        "metadata": if modern_meta.is_object() { modern_meta.clone() } else { json!({}) },
        "identity": {"structure_id": model.structure_id(), "content_id": model.content_id(), "revision": model.sha256()?,
            "registration_id": registration.get("registration_id").cloned().unwrap_or(Value::Null),
            "payload_sha256": declared.unwrap_or(native_sha)},
    }))
}

#[must_use]
pub fn history_error(e: AuthoringError) -> implexity_geometry::GeometryError {
    to_geometry(e)
}
