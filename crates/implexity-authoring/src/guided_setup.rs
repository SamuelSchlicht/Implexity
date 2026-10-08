// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::time::{Duration, Instant};

use implexity_core::contracts::{ResponseSpec, reject_removed_constraint_keys};
use implexity_core::providers::ProviderRegistryToken;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError, cae};
use crate::optimization_setup::{self as setup, canonical_json};
use crate::py::{py_eq, py_str, sha256_hex, truthy, type_name, uuid_hex};
use crate::services::{Authoring, ServiceContext};
use crate::sync::lock;

pub const SCHEMA: &str = "implexity-guided-setup/1";
pub const TOKEN_TTL: f64 = 600.0;
pub const MAX_REVIEWS: usize = 4;

fn verr(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("ValueError", message)
}

#[derive(Clone, Debug)]
pub struct GuidedReview {
    pub key: String,
    pub public: Value,
    pub label: String,
    pub expires: Instant,
    pub registry: ProviderRegistryToken,
}


pub fn request_digest(request: &Value) -> AResult<String> {
    setup::request_digest(request)
}

fn summary(value: &Value) -> AResult<Value> {
    if value.is_object() || value.is_array() {
        let text = canonical_json(value)?;
        if text.len() > 320 {
            let entries = value.as_object().map_or_else(|| value.as_array().map_or(0, Vec::len), Map::len);
            return Ok(
                json!({"summary": type_name(value), "entries": entries, "sha256": sha256_hex(text.as_bytes())}),
            );
        }
    }
    Ok(value.clone())
}


pub fn difference(before: &Value, after: &Value, limit: usize) -> AResult<Value> {
    fn walk(a: &Value, b: &Value, path: &str, out: &mut Vec<Value>, limit: usize) -> AResult<()> {
        if type_name(a) == type_name(b) && py_eq(a, b) {
            return Ok(());
        }
        if out.len() >= limit {
            return Ok(());
        }
        if let (Value::Object(x), Value::Object(y)) = (a, b) {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let p = format!("{path}/{escaped}");
                match (x.get(key), y.get(key)) {
                    (None, Some(bv)) => {
                        out.push(json!({"path": p, "operation": "add", "after": summary(bv)?}));
                    }
                    (Some(av), None) => {
                        out.push(json!({"path": p, "operation": "remove", "before": summary(av)?}));
                    }
                    (Some(av), Some(bv)) => walk(av, bv, &p, out, limit)?,
                    (None, None) => {}
                }
                if out.len() >= limit {
                    break;
                }
            }
        } else {
            out.push(json!({"path": if path.is_empty() { "/" } else { path }, "operation": "replace",
                "before": summary(a)?, "after": summary(b)?}));
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(before, after, "", &mut out, limit)?;
    let truncated = out.len() >= limit;
    Ok(json!({"changes": out, "possibly_truncated": truncated,
        "before_sha256": request_digest(before)?, "after_sha256": request_digest(after)?}))
}

fn problem_record(a: &Authoring) -> AResult<Value> {
    if !a.problems.path.exists() {
        return Ok(Value::Null);
    }
    let text = std::fs::read_to_string(&a.problems.path).map_err(|e| AuthoringError::Io(e.to_string()))?;
    implexity_core::json::parse_strict(&text).map_err(|e| verr(e.to_string()))
}


pub fn inspect(ctx: &dyn ServiceContext) -> AResult<Value> {
    let a = ctx.authoring()?;
    let _p = a.problems.lock();
    let _s = a.setup.lock();
    crate::setup_transaction::recover(a.models.dir())?;
    let mut state = a.setup.inspect(&setup::binding(ctx, &a)?)?;
    let applied =
        state["record"].get("applied").is_some_and(truthy) && !state["stale"].as_bool().unwrap_or(true);
    if let Some(m) = state.as_object_mut() {
        m.insert("schema".into(), json!(SCHEMA));
        m.insert("problem_record".into(), problem_record(&a)?);
        m.insert("applied_current".into(), json!(applied));
    }
    Ok(state)
}

#[allow(clippy::too_many_lines)]
fn prepare(
    ctx: &dyn ServiceContext,
    a: &Authoring,
    request: &Value,
    previous: &Value,
) -> AResult<(Value, Value)> {
    let mut candidate = request.clone();
    reject_removed_constraint_keys(&candidate, "guided run setup").map_err(cae)?;
    if let Some(settings) = candidate.get("settings").filter(|s| s.is_object()) {
        reject_removed_constraint_keys(settings, "guided run setup settings").map_err(cae)?;
    }
    if ["cae", "problem", "applied_setup"].iter().any(|k| candidate.get(*k).is_some()) {
        return Err(verr(
            "Guided setup requires one unambiguous physics declaration in physics.problem. Remove alternate envelopes or applied_setup before review.",
        ));
    }
    let physics = candidate.get("physics").cloned().unwrap_or(Value::Null);
    if !physics.is_object() || !physics.get("problem").is_some_and(Value::is_object) {
        return Err(verr("Guided setup requires physics.provider and physics.problem."));
    }
    let name = match physics.get("provider") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(verr("Provider identities in the request disagree.")),
    };
    if candidate.get("provider").is_some_and(|p| p.as_str() != Some(name.as_str())) {
        return Err(verr("Provider identities in the request disagree."));
    }
    let registries = implexity_core::registries::global();
    if crate::physics_binding::for_provider(&registries.contributions, &name).is_some() {
        return Err(verr("Whole-request application currently requires a modular provider problem."));
    }
    let ps = implexity_runtime::provider_problem_document::SCHEMA;
    if !truthy(previous) || previous.get("schema").and_then(Value::as_str) != Some(ps) {
        return Err(verr(
            "First author and validate a modular provider problem using Physics setup, then review its complete run setup.",
        ));
    }
    if previous.get("provider").and_then(Value::as_str) != Some(name.as_str()) {
        return Err(verr(
            "Switch physics providers explicitly before applying a saved run for a different provider.",
        ));
    }
    let generation = ctx.package_generation()?;
    if candidate.get("physics_generation").is_some_and(|g| !py_eq(g, &generation)) {
        return Err(verr(
            "Saved request references another physics-package generation. Review package selection and the request before applying.",
        ));
    }
    let provider = registries.providers.get(&name).map_err(cae)?;
    let old = previous.get("problem").cloned().unwrap_or(Value::Null);
    provider.normalise_problem(&old).map_err(cae)?;
    let rows = candidate.get("responses").cloned().unwrap_or(Value::Null);
    let Some(list) = rows.as_array().filter(|l| !l.is_empty() && l.iter().all(Value::is_object)) else {
        return Err(verr(
            "Run setup needs an explicit nonempty response list. Configure objectives and physical limits first.",
        ));
    };
    let responses: Vec<ResponseSpec> =
        list.iter().map(ResponseSpec::from_dict).collect::<Result<_, _>>().map_err(cae)?;
    let mut proposed = physics["problem"].clone();
    let intent = provider
        .as_any()
        .downcast_ref::<implexity_runtime::intent_orchestrated::IntentOrchestratedProvider>();
    let ops = implexity_optim::provider_ops::design_operations(provider.as_ref());
    let mut replanned = false;
    if let Some(p) = intent {
        proposed = p.replan_problem_for_responses(&old, &proposed, &responses).map_err(cae)?;
        replanned = true;
    } else if !py_eq(&proposed, &old)
        && let Some(r) = ops.and_then(|o| o.replan_problem_revision(&old, &proposed))
    {
        proposed = r.map_err(cae)?;
        replanned = true;
    }

    let has_replan = intent.is_some() || replanned;
    let model = a.models.require()?;
    let identity = json!({"structure_id": model.structure_id(), "content_id": model.content_id(), "sha256": model.sha256()?});
    let record = implexity_runtime::provider_problem_document::normalise_provider_problem_envelope(
        &json!({"schema": ps, "provider": name, "problem": proposed,
            "provenance": {"operation": "guided_setup_apply", "previous_problem_id": previous["problem_id"],
                "provider_owned_replan": has_replan && !py_eq(&proposed, &old)}}),
        &identity,
        "public_agent_action",
        "engineering_agent",
    )
    .map_err(cae)?;
    if let Some(c) = candidate.as_object_mut() {
        c.insert("provider".into(), json!(name));
        if let Some(p) = c.get_mut("physics").and_then(Value::as_object_mut) {
            p.insert("problem".into(), record["problem"].clone());
        }
    }
    canonical_json(&candidate)?;
    Ok((candidate, record))
}


pub fn validate_apply(payload: &Value) -> AResult<()> {
    let ok = payload.as_object().is_some_and(|m| {
        let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
        k.sort_unstable();
        k == ["request_sha256", "review_id"] && m.values().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
    });
    if ok {
        Ok(())
    } else {
        Err(verr("Applying a guided setup requires the exact review_id and request_sha256."))
    }
}


pub fn review(ctx: &dyn ServiceContext, payload: &Value) -> AResult<Value> {
    setup::validate_payload(payload)?;
    let a = ctx.authoring()?;
    a.manipulation.run_reserved_idle("review complete run setup", None, || {
        let _p = a.problems.lock();
        let _s = a.setup.lock();
        let state = inspect(ctx)?;
        if !py_eq(&payload["expected_revision"], &state["revision"])
            || !py_eq(&payload["expected_binding"], &state["current_binding"])
        {
            return Err(verr("Setup, model or physics changed. Refresh before reviewing."));
        }
        let registries = implexity_core::registries::global();
        let generation = registries.providers.binding_token();
        let (candidate, record) = prepare(ctx, &a, &payload["request"], &state["problem_record"])?;
        if registries.providers.binding_token() != generation
            || !py_eq(&setup::binding(ctx, &a)?, &state["current_binding"])
        {
            return Err(verr("Physics or geometry changed during review. Review again."));
        }
        let now = Instant::now();
        let key = uuid_hex();
        let saved_request = state["record"].get("request").cloned().unwrap_or_else(|| json!({}));
        let public = json!({
            "schema": SCHEMA, "review_id": key, "request_sha256": request_digest(&candidate)?,
            "candidate_request": candidate, "problem_record": record,
            "expected_revision": state["revision"], "expected_binding": state["current_binding"],
            "diff": difference(&saved_request, &candidate, 200)?,
            "normalization_diff": difference(&payload["request"], &candidate, 200)?,
            "physics_changed": !py_eq(&state["problem_record"]["problem"], &record["problem"]),
            "preflight_authorized": false, "expires_in_seconds": TOKEN_TTL,
        });
        let mut reviews = lock(&a.guided_reviews);
        reviews.retain(|r| now < r.expires);
        while reviews.len() >= MAX_REVIEWS {
            reviews.remove(0);
        }
        reviews.push(GuidedReview {
            key: key.clone(),
            public: public.clone(),
            label: payload["label"].as_str().unwrap_or_default().trim().to_string(),
            expires: now + Duration::from_secs_f64(TOKEN_TTL),
            registry: generation,
        });
        Ok(public)
    })
}


pub fn apply(ctx: &dyn ServiceContext, payload: &Value) -> AResult<Value> {
    validate_apply(payload)?;
    let a = ctx.authoring()?;
    a.manipulation.run_reserved_idle("apply complete run setup", None, || {
        let _p = a.problems.lock();
        let _s = a.setup.lock();
        let state = inspect(ctx)?;
        let review_id = py_str(&payload["review_id"]);
        let token = lock(&a.guided_reviews).iter().find(|r| r.key == review_id).cloned();
        let Some(token) = token.filter(|t| Instant::now() < t.expires) else {
            return Err(verr("This setup review expired or belongs to another service session. Review again."));
        };
        let view = &token.public;
        if payload["request_sha256"] != view["request_sha256"] {
            return Err(verr("Reviewed request fingerprint differs. Nothing was applied."));
        }
        let registries = implexity_core::registries::global();
        if !py_eq(&state["revision"], &view["expected_revision"])
            || !py_eq(&state["current_binding"], &view["expected_binding"])
            || registries.providers.binding_token() != token.registry
        {
            return Err(verr("Setup, geometry, physics or packages changed after review. Nothing was applied."));
        }
        let mut current = state["current_binding"].clone();
        let problem = &view["problem_record"];
        let declaration = json!({"schema": problem["schema"], "provider": problem["provider"], "problem": problem["problem"]});
        if let Some(c) = current.as_object_mut() {
            c.insert("problem_sha256".into(), json!(sha256_hex(canonical_json(&declaration)?.as_bytes())));
        }
        let revision = state["revision"].as_i64().unwrap_or(0) + 1;
        let record = json!({"schema": setup::SCHEMA, "revision": revision, "label": token.label,
            "saved_at": crate::py::now(), "binding": current, "request": view["candidate_request"],
            "preflight_authorized": false,
            "applied": {"schema": SCHEMA, "review_id": review_id, "request_sha256": view["request_sha256"],
                "problem_id": problem["problem_id"]}});
        canonical_json(&record)?;
        crate::setup_transaction::commit(a.models.dir(), problem, &record)?;
        lock(&a.guided_reviews).retain(|r| r.key != review_id);
        inspect(ctx)
    })
}


pub fn validate_launch(ctx: &dyn ServiceContext, request: &Value) -> AResult<()> {
    let Some(marker) = request.get("applied_setup") else { return Ok(()) };
    let ok = marker.as_object().is_some_and(|m| {
        let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
        k.sort_unstable();
        k == ["request_sha256", "revision"]
    });
    if !ok {
        return Err(verr("Malformed applied setup binding."));
    }
    let state = inspect(ctx)?;
    let record = state["record"].clone();
    let fine = state["applied_current"].as_bool().unwrap_or(false)
        && (marker["revision"].is_i64() || marker["revision"].is_u64())
        && record.get("revision").is_some_and(|r| py_eq(r, &marker["revision"]))
        && record.pointer("/applied/request_sha256").is_some_and(|r| py_eq(r, &marker["request_sha256"]))
        && json!(request_digest(request)?) == marker["request_sha256"];
    if !fine {
        return Err(verr(
            "Applied setup no longer matches this request, model or physics. Review and apply the current setup before preflight or launch.",
        ));
    }
    Ok(())
}
