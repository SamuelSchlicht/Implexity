// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::route_tables::{
    BodyPolicy, RouteBody, RouteDecl, RouteFailure, RouteHandler, RouteReply, RouteRequest, RouteService,
    RouteTable, RouteTableError,
};
use implexity_geometry::GeometryError;
use implexity_geometry::document as D;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::interaction_runtime::InteractionRuntime;
use crate::model_manager::{evaluate_model, max_eval_points};
use crate::py::{canonical_ascii, py_int, py_str, repr, sha256_hex, truthy, type_name};
use crate::services::{Authoring, NativeStores, RouteCtx, ServiceContext};

pub const MODULE: &str = "implexity.implicit.api";
pub const INTERACTION_MODULE: &str = "implexity.implicit.interaction_http";
pub const PROGRESSIVE_MODULE: &str = "implexity.implicit.progressive_http";
pub const GLSL_MODULE: &str = "implexity.implicit.glsl";
pub const CAPABILITY_SCHEMA: &str = "implexity-implicit-capabilities/1";

#[must_use]
pub fn endpoints() -> Vec<String> {
    route_table().map(|t| t.endpoints()).unwrap_or_default()
}

fn reply(status: u16, value: &Value) -> RouteReply {
    RouteReply::json(status, value)
}

fn problems_reply(error: &str, problems: &[String]) -> RouteReply {
    reply(422, &json!({"error": error, "problems": problems}))
}

fn internal(e: &AuthoringError) -> RouteFailure {
    RouteFailure::Internal(e.to_string())
}

fn call(result: AResult<Value>) -> Result<RouteReply, RouteFailure> {
    match result {
        Ok(v) => Ok(reply(200, &v)),
        Err(e) if e.is_model_doc() => Ok(problems_reply("model document rejected", &e.problem_list())),
        Err(AuthoringError::Problems { class: "SeedError", problems }) => {
            Ok(problems_reply("geometry seed rejected", &problems))
        }
        Err(AuthoringError::Problems { class: "StudyError", problems }) => {
            Ok(problems_reply("study rejected", &problems))
        }
        Err(AuthoringError::Geometry(g @ (GeometryError::Model(_) | GeometryError::Transpile { .. }))) => {
            Ok(problems_reply("model document rejected", &[g.to_string()]))
        }
        Err(e) => Err(internal(&e)),
    }
}

fn manip_call(result: AResult<Value>) -> Result<RouteReply, RouteFailure> {
    match result {
        Ok(v) => Ok(reply(200, &v)),
        Err(AuthoringError::Problems { class: "ManipulationError", problems }) => {
            Ok(problems_reply("direct manipulation was refused", &problems))
        }
        Err(e) if e.is_model_doc() => Ok(problems_reply("model document rejected", &e.problem_list())),
        Err(e) => Err(internal(&e)),
    }
}

fn seed_call(result: AResult<Value>) -> Result<RouteReply, RouteFailure> {
    match result {
        Ok(v) => Ok(reply(200, &v)),
        Err(AuthoringError::Problems { class: "SeedError", problems }) => {
            Ok(problems_reply("geometry seed rejected", &problems))
        }
        Err(AuthoringError::Problems { class: "ManipulationError", problems }) => {
            Ok(problems_reply("geometry seed commit was refused", &problems))
        }
        Err(e) if e.is_model_doc() => Ok(problems_reply("model document rejected", &e.problem_list())),
        Err(e) => Err(internal(&e)),
    }
}

fn uncaught(e: &AuthoringError) -> RouteFailure {
    match e {
        AuthoringError::Value { class: "CAEContractError", message } => {
            RouteFailure::Contract(message.clone())
        }
        other => internal(other),
    }
}

fn body(req: &RouteRequest) -> Value {
    match &req.body {
        RouteBody::Json(v) => v.clone(),
        RouteBody::Raw(_) => json!({}),
    }
}

fn body_get(b: &Value, key: &str) -> AResult<Option<Value>> {
    match b {
        Value::Object(m) => Ok(m.get(key).cloned()),
        other => Err(AuthoringError::runtime(
            "AttributeError",
            format!("'{}' object has no attribute 'get'", type_name(other)),
        )),
    }
}

#[must_use]
pub fn capabilities(ctx: &dyn ServiceContext) -> Value {
    let mut have: Vec<String> = ctx.endpoints();
    have.sort();
    have.dedup();
    let has = |e: &str| have.iter().any(|x| x == e);
    let mine: Vec<String> = have.iter().filter(|e| e.contains("/v1/implicit/")).cloned().collect();
    let sel = |f: &dyn Fn(&str) -> bool| -> Vec<String> { mine.iter().filter(|e| f(e)).cloned().collect() };
    let feat = |available: bool, endpoints: Vec<String>, what: &str, how: Option<&str>| -> Value {
        let mut d = Map::new();
        d.insert("available".into(), json!(available));
        d.insert("endpoints".into(), json!(endpoints));
        d.insert("what".into(), json!(what));
        if let (Some(h), false) = (how, available) {
            d.insert("how".into(), json!(h));
        }
        Value::Object(d)
    };
    let strs = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_string()).collect() };
    let mut optimize = feat(
        has("POST /v1/implicit/optimize"),
        sel(&|e| e.contains("/optimize")),
        "run the Optimize NODE as a job: the design variables are the MODEL's own parameters, not the case lattice's control channels",
        Some("IMPLEXITY_PLUGINS=implexity.implicit.api"),
    );
    if let Some(o) = optimize.as_object_mut() {
        o.insert(
            "node_kind_registered".into(),
            json!(implexity_geometry::node::registry().get("optimize").is_some()),
        );
        o.insert(
            "node_kind_note".into(),
            json!("whether a DOCUMENT may carry an `optimize` node here.  The endpoint does not need it -- a run is declared in the request and built in the subprocess -- so the kind is loaded on first use rather than by importing this module, which would put a kind the GLSL transpiler cannot emit into its table without it being asked"),
        );
    }
    let formats = implexity_mesh::interop::export_available();
    let export_any = formats.values().any(|v| v.get("available").is_some_and(truthy));
    json!({
        "schema": CAPABILITY_SCHEMA,
        "endpoints": mine,
        "service_endpoints": have,
        "features": {
            "model": feat(has("GET /v1/implicit/model"), sel(&|e| e.ends_with("/model")), "store, fetch and validate a model document", None),
            "geometry_seeds": feat(has("GET /v1/implicit/seeds"), sel(&|e| e.contains("/seeds")),
                "registered, physics-neutral parametric starting geometries; preview or commit a seed, then explicitly bake the tuned current DAG into registered editable occupancy", None),
            "evaluate": feat(has("POST /v1/implicit/evaluate"), strs(&["POST /v1/implicit/evaluate"]), "the field of any named node, with its extent", None),
            "parameters": feat(has("POST /v1/implicit/parameters"), sel(&|e| e.ends_with("/parameters")),
                "the named parameter table, and the fast path that moves it without a recompile", None),
            "graph_authoring": feat(has("POST /v1/implicit/graph/edit"), strs(&["POST /v1/implicit/graph/edit"]),
                "transactional node insertion, deletion, rewiring, root selection and parameter binding; one validated graph identity is committed per batch", None),
            "sensitivity": feat(has("POST /v1/implicit/sensitivity"), strs(&["POST /v1/implicit/sensitivity"]),
                "one-point coupled CAE evaluation plus reverse-mode AD gradient with respect to arbitrary selected model parameters; no optimiser update", None),
            "engineering_problem": feat(has("GET /v1/implicit/problem"), strs(&["GET /v1/implicit/problem", "PUT /v1/implicit/problem"]),
                "engineering intent bound to the live implicit model and validated case; unsupported generic setup is refused", None),
            "result_fields": feat(has("GET /v1/implicit/results"), strs(&["GET /v1/implicit/results", "POST /v1/implicit/results"]),
                "scalar/vector/tensor CAE state fields actually produced by the selected backend, plus bounded current-design evaluation", None),
            "derivatives": feat(has("POST /v1/implicit/derivatives"), strs(&["GET /v1/implicit/derivatives", "POST /v1/implicit/derivatives"]),
                "Jacobian and matrix-free JVP/VJP operators over engineering responses", None),
            "studies": feat(has("GET /v1/implicit/studies"), strs(&["GET /v1/implicit/studies", "POST /v1/implicit/studies", "GET /v1/implicit/studies/<id>", "POST /v1/implicit/studies/<id>"]),
                "persistent named experiment variants bound to a model identity and executed through the existing optimisation job/provenance path", None),
            "direct_manipulation": feat(has("POST /v1/implicit/manipulation/begin"), sel(&|e| e.contains("/manipulation")),
                "cursor picking and semantic or differentiable surface drag over authored model parameters, with transactional preview, commit, cancel and gesture-level undo/redo", None),
            "optimize": optimize,
            "shader": feat(has("GET /v1/implicit/shader"), sel(&|e| e.ends_with("/shader")),
                "transpile a node graph to GLSL for a raymarching viewport",
                Some("IMPLEXITY_PLUGINS=implexity.implicit.glsl (it imports implexity.implicit.api, so one name brings both)")),
            "export": {"available": export_any, "formats": formats,
                "what": "mesh a subtree and write it out (STL, PLY, 3MF, STEP), through the existing exporters"},
        },
        "note": "read from routes.REGISTRY in this process.  A feature is available here if and only if its endpoints are in the table above; nothing in this block is a version claim",
    })
}

#[must_use]
pub fn catalogue_kinds() -> Vec<Value> {
    let reg = implexity_geometry::node::registry();
    let mut kinds = D::catalogue(Some(&reg)).as_array().cloned().unwrap_or_default();
    for k in &mut kinds {
        let kind = py_str(&k["kind"]);
        let shape = crate::kinds::shape(&kind);
        let discrete: Vec<String> = reg.get(&kind).map(|e| e.info.discrete.clone()).unwrap_or_default();
        if let Some(o) = k.as_object_mut() {
            o.insert("arity".into(), json!(shape.arity));
            o.insert("arity_declared".into(), json!(shape.arity.is_some()));
            o.insert("struct".into(), json!(shape.structural));
            o.insert("discrete".into(), json!(discrete));
        }
    }
    kinds
}

#[must_use]
pub fn catalogue_reply(ctx: &dyn ServiceContext) -> Value {
    let kinds = catalogue_kinds();
    let mut dims: Vec<String> = kinds
        .iter()
        .flat_map(|k| k["params"].as_array().cloned().unwrap_or_default())
        .map(|p| py_str(&p["dimension"]))
        .collect();
    dims.sort();
    dims.dedup();
    let mut funcs: Vec<&str> = D::expr::FUNCS.to_vec();
    funcs.sort_unstable();
    let mut consts: Vec<&str> = D::expr::CONSTANTS.iter().map(|(n, _)| *n).collect();
    consts.sort_unstable();
    let count = kinds.len();
    let mut out = Map::new();
    out.insert("kind".into(), json!("implicit_catalogue"));
    out.insert("units".into(), json!("mm"));
    out.insert("schema".into(), json!(D::SCHEMA));
    out.insert("kinds".into(), Value::Array(kinds));
    out.insert("count".into(), json!(count));
    out.insert(
        "units_note".into(),
        json!("every length is millimetres, on the wire and in this kernel's PARAMS; docs/PROTOCOL.md"),
    );
    out.insert("dimensions".into(), json!(dims));
    out.insert("functions".into(), json!(funcs));
    out.insert("constants".into(), json!(consts));
    out.insert("export".into(), json!(implexity_mesh::interop::export_available()));
    out.insert("max_eval_points".into(), json!(max_eval_points()));
    out.insert("inline_array_bytes".into(), json!(D::array_inline_max_bytes()));
    out.insert(
        "kind_fields".into(),
        json!({
            "arity": "how many children this kind takes; null where the kind declares none",
            "struct": "attribute names that are part of the SHAPE: hashed into structure_id, so changing one recompiles.  A document spells them 'attrs'",
            "discrete": "parameters with no useful derivative; an optimiser refuses them by name",
        }),
    );
    out.insert("capabilities".into(), capabilities(ctx));
    Value::Object(out)
}

fn with_problem_capabilities(mut rec: Value, backend: Option<&str>) -> Value {
    let caps = if rec.get("schema").and_then(Value::as_str)
        == Some(implexity_runtime::provider_problem_document::SCHEMA)
    {
        rec.pointer("/provenance/provider_capabilities")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| json!({}))
    } else {
        crate::problem::capabilities(backend)
    };
    if let Some(o) = rec.as_object_mut() {
        o.insert("capabilities".into(), caps);
    }
    rec
}

fn idle<T>(a: &Authoring, operation: &str, f: impl FnOnce() -> AResult<T>) -> AResult<T> {
    a.manipulation.run_reserved_idle(operation, None, f)
}

type Handler = fn(&RouteRequest, &dyn RouteService) -> Result<RouteReply, RouteFailure>;

fn authoring_or(ctx: &RouteCtx<'_>) -> AResult<Arc<Authoring>> {
    ctx.authoring()
}

fn get_catalogue(_req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    call(Ok(catalogue_reply(&ctx)))
}

fn get_seeds(_req: &RouteRequest, _svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    call(Ok(crate::seeds::catalogue()))
}

fn post_seed_preview(req: &RouteRequest, _svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    call(crate::seeds::preview(&body(req)))
}

fn post_seed_commit(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    seed_call(authoring_or(&ctx).and_then(|a| {
        idle(&a, "replace the model from a seed", || crate::seeds::commit(&a.models, &body(req)))
    }))
}

fn post_seed_bake(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    seed_call(authoring_or(&ctx).and_then(|a| {
        idle(&a, "bake the current model", || crate::seeds::bake_current(&a.models, &body(req)))
    }))
}

fn post_seed_bake_preview(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    seed_call(authoring_or(&ctx).and_then(|a| {
        idle(&a, "preview a current-model bake", || crate::seeds::preview_current(&a.models, &body(req)))
    }))
}

fn get_engineering(_req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    let backend = ctx.physics_backend();
    call(Ok(implexity_geometry::engineering::catalogue(
        &implexity_core::registries::global().contributions,
        backend.as_deref(),
    )))
}

fn get_problem(_req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    call((|| {
        let a = ctx.authoring()?;
        let call = a.problem_call(&ctx)?;
        let rec = a.problems.get(&call)?;
        Ok(with_problem_capabilities(rec, call.physics_backend.as_deref()))
    })())
}

fn put_problem(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    let result = (|| {
        let a = ctx.authoring()?;
        let call = a.problem_call(&ctx)?;
        let rec = a.problems.put(&body(req), &call)?;
        Ok(with_problem_capabilities(rec, call.physics_backend.as_deref()))
    })();
    match result {
        Ok(v) => Ok(reply(200, &v)),
        Err(AuthoringError::Problems { class: "ProblemError", problems }) => {
            Ok(problems_reply("engineering problem rejected", &problems))
        }
        Err(e) => Err(uncaught(&e)),
    }
}

fn get_model(_req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    call(ctx.authoring().and_then(|a| a.models.status()))
}

fn put_model(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    manip_call(ctx.authoring().and_then(|a| idle(&a, "replace the model", || a.models.put(&body(req)))))
}

fn post_graph_edit(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    manip_call(ctx.authoring().and_then(|a| a.edit_graph(&ctx, &body(req))))
}

fn post_validate(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    call((|| {
        let b = body(req);
        let doc = body_get(&b, "document")?.unwrap_or_else(|| b.clone());
        let a = ctx.authoring()?;
        let (problems, warnings) = D::problems_of(&doc, Some(a.models.dir()), None)?;
        let mut out = json!({"kind": "implicit_validate", "units": "mm", "valid": problems.is_empty(),
            "problems": problems, "warnings": warnings});
        if out["valid"] == json!(true) {
            let mut rt = D::check_roundtrip(&doc, Some(a.models.dir()), None)?;
            if let Some(o) = rt.as_object_mut() {
                o.shift_remove("first_difference");
            }
            if let Some(o) = out.as_object_mut() {
                o.insert("roundtrip".into(), rt);
            }
        }
        Ok(out)
    })())
}

fn post_evaluate(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    call(ctx.authoring().and_then(|a| {
        let m = a.models.require()?;
        let b = body(req);
        body_get(&b, "node")?;
        evaluate_model(&m, &b)
    }))
}

fn get_parameters(_req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    call(ctx.authoring().and_then(|a| {
        let m = a.models.require()?;
        Ok(json!({"kind": "implicit_parameters", "units": "mm", "parameters": m.parameter_table(),
            "structure_id": m.structure_id(), "content_id": m.content_id()}))
    }))
}

fn post_parameters(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    manip_call((|| {
        let b = body(req);
        let vals = body_get(&b, "values")?.unwrap_or_else(|| b.clone());
        let Some(map) = vals.as_object().filter(|m| !m.is_empty()) else {
            return Err(AuthoringError::model_doc(vec![
                "send {\"values\": {\"wall\": 1.4, ...}} -- the parameter names and their new numbers, in the unit the table declares"
                    .into(),
            ]));
        };
        let values: BTreeMap<String, Value> =
            map.iter().filter(|(k, _)| k.as_str() != "values").map(|(k, v)| (k.clone(), v.clone())).collect();
        let a = ctx.authoring()?;
        let stores = NativeStores { ctx: &ctx, a: &a };
        idle(&a, "set parameters", || {
            a.interactions.record_mutation(
                &stores,
                &mut || a.models.set_parameters(&values),
                "Edit named geometry parameters",
                "http_parameters",
            )
        })
    })())
}

fn manipulation(
    req: &RouteRequest,
    svc: &dyn RouteService,
    op: fn(&Authoring, &NativeStores<'_>, &Value) -> AResult<Value>,
) -> Result<RouteReply, RouteFailure> {
    let ctx = RouteCtx(svc);
    manip_call(ctx.authoring().and_then(|a| {
        let stores = NativeStores { ctx: &ctx, a: &a };
        op(&a, &stores, &body(req))
    }))
}

fn get_manipulation(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, s, _| a.manipulation.status(Some(&s.shared())))
}

fn post_manip_begin(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, s, b| a.manipulation.begin(b, Some(&s.shared())))
}

fn post_manip_preview(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, _, b| a.manipulation.preview(b))
}

fn post_manip_guidance(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, _, b| a.manipulation.guidance(b))
}

fn post_manip_commit(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, s, b| a.manipulation.commit(b, Some(&s.shared())))
}

fn post_manip_cancel(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, s, b| a.manipulation.cancel(b, Some(&s.shared())))
}

fn post_manip_undo(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, s, b| a.manipulation.undo(b, Some(&s.shared())))
}

fn post_manip_redo(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    manipulation(req, svc, |a, s, b| a.manipulation.redo(b, Some(&s.shared())))
}

fn post_regions_rebind(req: &RouteRequest, _svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let b = body(req);
    let region = body_get(&b, "region").map_err(|e| uncaught(&e))?.unwrap_or(Value::Null);
    let candidates =
        body_get(&b, "candidates").map_err(|e| uncaught(&e))?.filter(truthy).unwrap_or_else(|| json!([]));
    match implexity_geometry::semantic_regions::rebind_semantic_region(&region, &candidates, 3.0, 1.15, 0.35)
    {
        Ok(v) => Ok(reply(200, &v)),
        Err(e) => Ok(reply(
            422,
            &json!({"ok": false, "error": "semantic region rebinding was refused", "problems": [e.0]}),
        )),
    }
}

fn shader_reply(ctx: &RouteCtx<'_>, b: &Value) -> AResult<Value> {
    use implexity_geometry::glsl::{self, TranspileOptions};
    let a = ctx.authoring()?;
    let m = a.models.require()?;
    let name =
        b.get("node").filter(|v| truthy(v)).or_else(|| m.doc.get("root")).map(py_str).unwrap_or_default();
    let node = m.node(&name)?;
    let flag = |k: &str, d: bool| b.get(k).map_or(d, truthy);
    let int = |k: &str, d: usize| -> AResult<usize> {
        match b.get(k) {
            None => Ok(d),
            Some(v) => Ok(usize::try_from(py_int(v)?.max(0)).unwrap_or(0)),
        }
    };
    let opts = TranspileOptions {
        mode: b.get("mode").map_or_else(|| "exact".into(), py_str),
        smooth_kind: b.get("smooth_kind").map_or_else(|| "poly".into(), py_str),
        textures: flag("textures", true),
        texture_res: int("texture_res", glsl::texture_res())?,
        measure_samples: int("measure_samples", glsl::TEXTURE_MEASURE_SAMPLES)?,
        tpms_range_reduce: flag("tpms_range_reduce", true),
        texture_class: b.get("texture_class").map_or_else(|| "proven".into(), py_str),
        smooth_r_mm: match b.get("smooth_r_mm") {
            None | Some(Value::Null) => None,
            Some(v) => Some(crate::py::py_float(v)?),
        },
        ..TranspileOptions::default()
    };
    let t0 = std::time::Instant::now();
    let mut res = glsl::transpile(&node, &opts)?;
    res.payload.insert("seconds".into(), crate::py::jf(crate::py::py_round(t0.elapsed().as_secs_f64(), 4)));
    res.payload.insert("requested".into(), json!(name));
    res.payload.insert("node".into(), json!(m.id_of(&node)));
    let mut out = glsl::encode(&res, flag("include_textures", true), 8 << 20);
    if b.get("include_catalogue").is_some_and(truthy)
        && let Some(o) = out.as_object_mut()
    {
        o.insert("catalogue".into(), json!(glsl::catalogue(&implexity_geometry::node::registry())));
    }
    Ok(out)
}

fn get_shader(_req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    call(shader_reply(&RouteCtx(svc), &json!({})))
}

fn post_shader(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let b = body(req);
    let b = if truthy(&b) { b } else { json!({}) };
    call(shader_reply(&RouteCtx(svc), &b))
}

fn interaction(req: &RouteRequest, svc: &dyn RouteService, action: &str) -> RouteReply {
    let ctx = RouteCtx(svc);
    let result = (|| -> AResult<Value> {
        let a = ctx.authoring()?;
        let stores = NativeStores { ctx: &ctx, a: &a };
        let rt: &InteractionRuntime = &a.interactions;
        if action == "history_state" && a.models.model().is_none() {
            return Ok(rt.empty_history_state());
        }
        let mut payload = match body(req) {
            Value::Object(m) => Value::Object(m),
            Value::Null => json!({}),
            other => {
                return Err(AuthoringError::Type(format!("'{}' object is not iterable", type_name(&other))));
            }
        };
        if action == "begin"
            && let Some(o) = payload.as_object_mut()
        {
            o.insert("_client_origin".into(), json!("http"));
        }
        match action {
            "capabilities" => Ok(InteractionRuntime::capabilities()),
            "history_state" => rt.history_state(&stores),
            "field" => rt.field(&stores, &payload),
            "begin" => rt.begin(&stores, &payload),
            "preview" => rt.preview(&stores, &payload),
            "refine" => rt.refine(&stores, &payload),
            "commit" => rt.commit(&stores, &payload),
            "cancel" => rt.cancel(&payload),
            "undo" => rt.undo(&stores, &payload),
            "redo" => rt.redo(&stores, &payload),
            _ => rt.promote_cage(&stores, &payload),
        }
    })();
    match result {
        Ok(v) => reply(200, &v),
        Err(e) => reply(422, &json!({"error": "interaction rejected", "problems": [e.to_string()]})),
    }
}

#[must_use]
pub fn query_first(query: &str, key: &str) -> Option<String> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    let h = std::str::from_utf8(&bytes[i + 1..i + 3])
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok());
                    match h {
                        Some(b) => {
                            out.push(b);
                            i += 2;
                        }
                        None => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        if decode(k) == key {
            return Some(decode(v));
        }
    }
    None
}


pub fn py_int_str(text: &str) -> AResult<i64> {
    let t = text.trim().replace('_', "");
    t.parse::<i64>().map_err(|_| {
        AuthoringError::value(
            "ValueError",
            format!("invalid literal for int() with base 10: {}", repr(&json!(text))),
        )
    })
}

fn triplet(text: Option<&str>, default: [i64; 3]) -> AResult<[i64; 3]> {
    let Some(t) = text.filter(|t| !t.is_empty()) else { return Ok(default) };
    let parts: Vec<i64> = t.split(',').map(py_int_str).collect::<AResult<_>>()?;
    if parts.len() != 3 || parts.iter().any(|v| *v <= 0) {
        return Err(AuthoringError::value(
            "ValueError",
            "tile shape must be three positive comma-separated integers",
        ));
    }
    Ok([parts[0], parts[1], parts[2]])
}

fn index_triplet(text: Option<&str>) -> AResult<[i64; 3]> {
    let t = text.filter(|t| !t.is_empty()).unwrap_or("0,0,0");
    let parts: Vec<i64> = t.split(',').map(py_int_str).collect::<AResult<_>>()?;
    if parts.len() != 3 || parts.iter().any(|v| *v < 0) {
        return Err(AuthoringError::value(
            "ValueError",
            "tile index must be three non-negative comma-separated integers",
        ));
    }
    Ok([parts[0], parts[1], parts[2]])
}

fn etag(value: &str) -> String {
    format!("\"{}\"", value.trim_matches('"'))
}

fn matches_etag(req: &RouteRequest, digest: &str) -> bool {
    let header = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("if-none-match"))
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for part in header.split(',') {
        let mut token = part.trim();
        if let Some(rest) = token.strip_prefix("W/") {
            token = rest.trim();
        }
        candidates.push(token.trim_matches('"').to_string());
    }
    candidates.iter().any(|c| c == "*" || c == digest)
}

const IMMUTABLE: &str = "public, immutable, max-age=31536000";

fn stream_failure(e: AuthoringError, ident: &str) -> Result<RouteReply, RouteFailure> {
    match e {
        AuthoringError::Key(_) => {
            Ok(RouteReply::error(404, "unknown progressive field", Some(&json!(ident))))
        }
        AuthoringError::Value { class: "ValueError" | "IndexError", message } => {
            Ok(RouteReply::error(422, &message, None))
        }
        AuthoringError::Geometry(GeometryError::Value(m)) => Ok(RouteReply::error(422, &m, None)),
        other => Err(internal(&other)),
    }
}

fn check_level(
    store: &crate::incremental_fields::IncrementalProgressiveFieldStore,
    id: &str,
    level: i64,
) -> AResult<usize> {
    let record = store.base.record(id)?;
    usize::try_from(level)
        .ok()
        .filter(|l| *l <= record.exact_level())
        .ok_or_else(|| AuthoringError::value("IndexError", level.to_string()))
}

fn get_stream_manifest(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ident = req.ident.clone().unwrap_or_default();
    let ctx = RouteCtx(svc);
    let result = (|| -> AResult<RouteReply> {
        let store = ctx.authoring()?.field_store()?;
        let body = store.manifest_v22(&ident)?;
        let field_id = py_str(&body["field_id"]);
        let headers =
            vec![("ETag".to_string(), etag(&field_id)), ("Cache-Control".to_string(), IMMUTABLE.to_string())];
        if matches_etag(req, &field_id) {
            return Ok(RouteReply {
                status: 304,
                content_type: "application/json".into(),
                body: Vec::new(),
                headers,
            });
        }
        let mut r = reply(200, &body);
        r.headers = headers;
        Ok(r)
    })();
    result.or_else(|e| stream_failure(e, &ident))
}

fn get_stream_delta(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ident = req.ident.clone().unwrap_or_default();
    let ctx = RouteCtx(svc);
    let result = (|| -> AResult<RouteReply> {
        let source = query_first(&req.query, "from");
        let level = match query_first(&req.query, "level").filter(|l| !l.is_empty()) {
            None => None,
            Some(t) => Some(py_int_str(&t)?),
        };
        let tile_shape = triplet(
            query_first(&req.query, "tile_shape").as_deref(),
            crate::incremental_fields::DEFAULT_TILE_SHAPE,
        )?;
        let store = ctx.authoring()?.field_store()?;
        let body = store.delta_manifest(&ident, source.as_deref(), level, Some(tile_shape), None)?;
        let tag = etag(&sha256_hex(canonical_ascii(&body).as_bytes()));
        let mut r = reply(200, &body);
        r.headers = vec![("ETag".to_string(), tag), ("Cache-Control".to_string(), IMMUTABLE.to_string())];
        Ok(r)
    })();
    result.or_else(|e| stream_failure(e, &ident))
}

fn get_stream_tile(req: &RouteRequest, svc: &dyn RouteService) -> Result<RouteReply, RouteFailure> {
    let ident = req.ident.clone().unwrap_or_default();
    let ctx = RouteCtx(svc);
    let result = (|| -> AResult<RouteReply> {
        let level = py_int_str(&query_first(&req.query, "level").unwrap_or_else(|| "0".into()))?;
        let index = index_triplet(query_first(&req.query, "index").as_deref())?;
        let tile_shape = triplet(
            query_first(&req.query, "tile_shape").as_deref(),
            crate::incremental_fields::DEFAULT_TILE_SHAPE,
        )?;
        let encoding = query_first(&req.query, "encoding").unwrap_or_else(|| "raw".into()).to_lowercase();
        let store = ctx.authoring()?.field_store()?;
        let (tile, ctype, extra): (crate::progressive_fields::TilePayload, &str, Vec<(String, String)>) =
            match encoding.as_str() {
                "raw" => {
                    let level = check_level(&store, &ident, level)?;
                    (
                        store.tile_raw(&ident, level, index, tile_shape, None)?,
                        "application/vnd.implexity.field-tile+raw",
                        Vec::new(),
                    )
                }
                "gzip" => {
                    let level = check_level(&store, &ident, level)?;
                    (
                        store.tile(&ident, level, index, tile_shape, None)?,
                        "application/vnd.implexity.field-tile+gzip",
                        vec![("X-Implexity-Content-Encoding".to_string(), "gzip".to_string())],
                    )
                }
                _ => return Err(AuthoringError::value("ValueError", "encoding must be raw or gzip")),
            };
        let raw_digest = py_str(&tile.header["raw_sha256"]);
        if matches_etag(req, &raw_digest) {
            return Ok(RouteReply {
                status: 304,
                content_type: ctype.into(),
                body: Vec::new(),
                headers: vec![("ETag".into(), etag(&raw_digest)), ("Cache-Control".into(), IMMUTABLE.into())],
            });
        }
        let mut headers = vec![
            (
                "X-Implexity-Tile".to_string(),
                implexity_core::json::dumps(&tile.header, &implexity_core::json::DumpOptions::compact()),
            ),
            ("ETag".to_string(), etag(&raw_digest)),
            ("Cache-Control".to_string(), IMMUTABLE.to_string()),
        ];
        headers.extend(extra);
        Ok(RouteReply { status: 200, content_type: ctype.into(), body: tile.body, headers })
    })();
    result.or_else(|e| stream_failure(e, &ident))
}

fn h(f: Handler) -> RouteHandler {
    Arc::new(f)
}

fn hi(action: &'static str) -> RouteHandler {
    Arc::new(move |req: &RouteRequest, svc: &dyn RouteService| Ok(interaction(req, svc, action)))
}


#[allow(clippy::too_many_lines)]
pub fn route_table() -> Result<RouteTable, RouteTableError> {
    let mut t = RouteTable::new();
    let mut add =
        |method: &str, pattern: &str, body: BodyPolicy, doc: &str, module: &str, handler: RouteHandler| {
            t.add(RouteDecl::new(method, pattern, body, doc, module, handler)?)
        };
    let (n, j) = (BodyPolicy::None, BodyPolicy::Json);
    add(
        "GET",
        "/v1/implicit/catalogue",
        n,
        "Every node kind with its PARAMS, ARITY, STRUCT, units, docs and class.",
        MODULE,
        h(get_catalogue),
    )?;
    add(
        "GET",
        "/v1/implicit/seeds",
        n,
        "Versioned starting geometries registered by core and add-ins.",
        MODULE,
        h(get_seeds),
    )?;
    add(
        "POST",
        "/v1/implicit/seeds/preview",
        j,
        "Build and validate a detached seed document without changing state.",
        MODULE,
        h(post_seed_preview),
    )?;
    add(
        "POST",
        "/v1/implicit/seeds/commit",
        j,
        "Commit one previewable seed under an exact current-content guard.",
        MODULE,
        h(post_seed_commit),
    )?;
    add(
        "POST",
        "/v1/implicit/seeds/bake-current",
        j,
        "Explicitly hand a tuned current DAG to local occupancy authoring.",
        MODULE,
        h(post_seed_bake),
    )?;
    add(
        "POST",
        "/v1/implicit/seeds/bake-current/preview",
        j,
        "Preview the exact tune-to-occupancy handoff without mutating state.",
        MODULE,
        h(post_seed_bake_preview),
    )?;
    add(
        "GET",
        "/v1/implicit/engineering",
        n,
        "Engineering responses/constraints that share the differentiable chain.",
        MODULE,
        h(get_engineering),
    )?;
    add(
        "GET",
        "/v1/implicit/problem",
        n,
        "The engineering problem bound to the live implicit model and case.",
        MODULE,
        h(get_problem),
    )?;
    add(
        "PUT",
        "/v1/implicit/problem",
        j,
        "Persist engineering intent; unsupported backend setup is refused.",
        MODULE,
        h(put_problem),
    )?;
    add(
        "GET",
        "/v1/implicit/model",
        n,
        "The stored model document, its graph, and its two ids.",
        MODULE,
        h(get_model),
    )?;
    add(
        "PUT",
        "/v1/implicit/model",
        j,
        "Store a model document; 422 lists every problem at once.",
        MODULE,
        h(put_model),
    )?;
    add(
        "POST",
        "/v1/implicit/model",
        j,
        "POST-as-PUT, the accommodation ``/v1/case`` and ``/v1/domain`` make.",
        MODULE,
        h(put_model),
    )?;
    add(
        "POST",
        "/v1/implicit/graph/edit",
        j,
        "Atomically add/delete/rewire nodes and bindings in the stored DAG.",
        MODULE,
        h(post_graph_edit),
    )?;
    add(
        "POST",
        "/v1/implicit/validate",
        j,
        "Check a document without storing it: problems, warnings, round trip.",
        MODULE,
        h(post_validate),
    )?;
    add(
        "POST",
        "/v1/implicit/evaluate",
        j,
        "The field of ANY named node in the graph, not only the root.",
        MODULE,
        h(post_evaluate),
    )?;
    add(
        "GET",
        "/v1/implicit/parameters",
        n,
        "The named parameter table, resolved, with what each one drives.",
        MODULE,
        h(get_parameters),
    )?;
    add(
        "POST",
        "/v1/implicit/parameters",
        j,
        "Set named parameters -- the fast path; ``structure_id`` cannot move.",
        MODULE,
        h(post_parameters),
    )?;
    add(
        "GET",
        "/v1/implicit/manipulation",
        n,
        "``GET /v1/implicit/manipulation``: direct-manipulation status.",
        MODULE,
        h(get_manipulation),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/begin",
        j,
        "``POST .../manipulation/begin``: open a manipulation session.",
        MODULE,
        h(post_manip_begin),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/preview",
        j,
        "``POST .../manipulation/preview``: preview a manipulation.",
        MODULE,
        h(post_manip_preview),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/guidance",
        j,
        "``POST .../manipulation/guidance``: guidance for a manipulation.",
        MODULE,
        h(post_manip_guidance),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/commit",
        j,
        "``POST .../manipulation/commit``: commit the manipulation.",
        MODULE,
        h(post_manip_commit),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/cancel",
        j,
        "``POST .../manipulation/cancel``: cancel the manipulation.",
        MODULE,
        h(post_manip_cancel),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/undo",
        j,
        "``POST .../manipulation/undo``: undo the last committed manipulation.",
        MODULE,
        h(post_manip_undo),
    )?;
    add(
        "POST",
        "/v1/implicit/manipulation/redo",
        j,
        "``POST .../manipulation/redo``: redo the last undone manipulation.",
        MODULE,
        h(post_manip_redo),
    )?;
    add(
        "POST",
        "/v1/implicit/regions/rebind",
        j,
        "Rebind a semantic region to candidates; refusals are 422.",
        MODULE,
        h(post_regions_rebind),
    )?;
    add(
        "GET",
        "/v1/implicit/shader",
        n,
        "The stored model's root as a sphere-tracing shader, with defaults.",
        GLSL_MODULE,
        h(get_shader),
    )?;
    add(
        "POST",
        "/v1/implicit/shader",
        j,
        "Compile any named node to GLSL: mode, smoothing, texture resolution.",
        GLSL_MODULE,
        h(post_shader),
    )?;
    for (method, path, action) in [
        ("GET", "/v1/implicit/interactions", "capabilities"),
        ("GET", "/v1/implicit/interactions/history", "history_state"),
        ("POST", "/v1/implicit/interactions/field", "field"),
        ("POST", "/v1/implicit/interactions/begin", "begin"),
        ("POST", "/v1/implicit/interactions/preview", "preview"),
        ("POST", "/v1/implicit/interactions/refine", "refine"),
        ("POST", "/v1/implicit/interactions/commit", "commit"),
        ("POST", "/v1/implicit/interactions/cancel", "cancel"),
        ("POST", "/v1/implicit/interactions/undo", "undo"),
        ("POST", "/v1/implicit/interactions/redo", "redo"),
        ("POST", "/v1/implicit/interactions/cage/promote", "promote_cage"),
    ] {
        add(method, path, if method == "GET" { n } else { j }, "", INTERACTION_MODULE, hi(action))?;
    }
    add("GET", "/v1/implicit/field-stream/<id>/manifest", n, "", PROGRESSIVE_MODULE, h(get_stream_manifest))?;
    add("GET", "/v1/implicit/field-stream/<id>/delta", n, "", PROGRESSIVE_MODULE, h(get_stream_delta))?;
    add("GET", "/v1/implicit/field-stream/<id>/tile", n, "", PROGRESSIVE_MODULE, h(get_stream_tile))?;
    Ok(t)
}
