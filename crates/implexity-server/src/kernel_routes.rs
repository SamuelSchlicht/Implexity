// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::Value;

use crate::http::{Reply, Request, RouteError, body_get};
use crate::jobs::JobError;
use crate::preview::{PreviewOp, finish};
use crate::routes::{BodyPolicy, Handler, Registry, RouteDeclarationError};

pub const NO_BACKEND: &str = "no geometry backend: the preview evaluator arrives with the geometry kernel (WP-03) and the meshing/raster module (WP-04)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub method: &'static str,
    pub pattern: &'static str,
    pub body: BodyPolicy,
    pub owner: &'static str,
    pub python: &'static str,
    pub doc: &'static str,
}

pub const PENDING: &[Pending] = &[
    Pending {
        method: "GET",
        pattern: "/v1/agent/capabilities",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/context",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/guidance",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/intent",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/manual",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/policy",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/state",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/agent/tools",
        body: BodyPolicy::None,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/body/jobs",
        body: BodyPolicy::None,
        owner: "WP-04",
        python: "implexity.bodyapi",
        doc: "Every export job, newest first.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/body/jobs/<id>",
        body: BodyPolicy::None,
        owner: "WP-04",
        python: "implexity.bodyapi",
        doc: "One export job, its provenance record, and its artefacts.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/cae/catalogue",
        body: BodyPolicy::None,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/catalogue",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Every node kind with its PARAMS, ARITY, STRUCT, units, docs and class.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/derivatives",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Public derivative operators over engineering responses.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/engineering",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Engineering responses/constraints that share the differentiable chain.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/field-stream/<id>/delta",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.progressive_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/field-stream/<id>/manifest",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.progressive_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/field-stream/<id>/tile",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.progressive_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/history",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "``GET .../engineering/history``: the engineering history list.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/interactions",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/interactions/history",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/manipulation",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``GET /v1/implicit/manipulation``: direct-manipulation status.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/model",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "The stored model document, its graph, and its two ids.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/optimize/jobs",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Every model optimisation, newest first.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/optimize/jobs/<id>",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "One job; ``<id>/record`` and ``<id>/before`` behind the same prefix.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/parameters",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "The named parameter table, resolved, with what each one drives.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/problem",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "The engineering problem bound to the live implicit model and case.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/result-artifact/<id>/array",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.result_arrays_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/result-artifact/<id>/manifest",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.result_arrays_http",
        doc: "",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/results",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Result fields the selected physics backend actually produces.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/seeds",
        body: BodyPolicy::None,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Versioned starting geometries registered by core and add-ins.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/shader",
        body: BodyPolicy::None,
        owner: "WP-03 (+WP-12 handler)",
        python: "implexity.implicit.glsl",
        doc: "The stored model's root as a sphere-tracing shader, with defaults.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/studies",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "``GET /v1/implicit/studies``: every study with its runs' status.",
    },
    Pending {
        method: "GET",
        pattern: "/v1/implicit/studies/<id>",
        body: BodyPolicy::None,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "``GET /v1/implicit/studies/<id>``: one study with its run status.",
    },
    Pending { method: "GET", pattern: "/v1/physics/providers/imported", body: BodyPolicy::None, owner: "WP-07", python: "implexity.cae.api", doc: "" },
    Pending { method: "POST", pattern: "/v1/physics/providers/imported", body: BodyPolicy::Json, owner: "WP-07", python: "implexity.cae.api", doc: "" },
    Pending { method: "POST", pattern: "/v1/physics/providers/imported/check", body: BodyPolicy::Json, owner: "WP-07", python: "implexity.cae.api", doc: "" },
    Pending {
        method: "GET",
        pattern: "/v1/physics/packages",
        body: BodyPolicy::None,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/agent/action",
        body: BodyPolicy::Json,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/agent/plan",
        body: BodyPolicy::Json,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/agent/validate",
        body: BodyPolicy::Json,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/body",
        body: BodyPolicy::Json,
        owner: "WP-04",
        python: "implexity.bodyapi",
        doc: "Start an export job.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/body/estimate",
        body: BodyPolicy::Json,
        owner: "WP-04",
        python: "implexity.bodyapi",
        doc: "Triangles and chord at a tolerance, from one cached coarse pass.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/body/jobs/<id>",
        body: BodyPolicy::Json,
        owner: "WP-04",
        python: "implexity.bodyapi",
        doc: "``{\"op\": \"cancel\"}`` against one export job.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/evaluate",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/optimize",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/orchestration/evaluate",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/orchestration/optimize",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/orchestration/plan",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/orchestration/sensitivity",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/preflight",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/cae/sensitivity",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/derivatives",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Apply Jacobian, JVP or VJP at the current model/case.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/evaluate",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "The field of ANY named node in the graph, not only the root.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/graph/edit",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Atomically add/delete/rewire nodes and bindings in the stored DAG.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/history/<id>/restore",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Restore an engineering snapshot under idle model authority.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/history/snapshot",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Record a labelled engineering snapshot with optional details.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/begin",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/cage/promote",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/cancel",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/commit",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/field",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/preview",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/redo",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/refine",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/interactions/undo",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.interaction_http",
        doc: "",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/begin",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/begin``: open a manipulation session.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/cancel",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/cancel``: cancel the manipulation.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/commit",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/commit``: commit the manipulation.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/guidance",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/guidance``: guidance for a manipulation.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/preview",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/preview``: preview a manipulation.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/redo",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/redo``: redo the last undone manipulation.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/manipulation/undo",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "``POST .../manipulation/undo``: undo the last committed manipulation.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/model",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "POST-as-PUT, the accommodation ``/v1/case`` and ``/v1/domain`` make.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/optimize",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Start an optimisation of the MODEL's own parameters.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/optimize/jobs/<id>",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "``{\"op\": \"pause|resume|stop|accept|discard\"}`` against one job.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/optimize/jobs/<id>/branch",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Re-preflight and start a provenance-linked job after manual intervention.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/optimize/jobs/<id>/numerical-attention",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Resolve one typed bounded numerical deviation without narrative data.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/optimize/jobs/<id>/steer",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Move a FIXED model parameter while the job runs.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/optimize/preflight",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "What the run would do, and what it cannot score -- MEASURED.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/parameters",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Set named parameters -- the fast path; ``structure_id`` cannot move.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/regions/rebind",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Rebind a semantic region to candidates; refusals are 422.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/results",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "Evaluate selected CAE state fields at the current model/case.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/seeds/bake-current",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Explicitly hand a tuned current DAG to local occupancy authoring.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/seeds/bake-current/preview",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Preview the exact tune-to-occupancy handoff without mutating state.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/seeds/commit",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Commit one previewable seed under an exact current-content guard.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/seeds/preview",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Build and validate a detached seed document without changing state.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/sensitivity",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "One-point coupled CAE value and exact AD gradient; no design update.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/shader",
        body: BodyPolicy::Json,
        owner: "WP-03 (+WP-12 handler)",
        python: "implexity.implicit.glsl",
        doc: "Compile any named node to GLSL: mode, smoothing, texture resolution.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/studies",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "``POST /v1/implicit/studies``: declare every variant, then create.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/studies/<id>",
        body: BodyPolicy::Json,
        owner: "WP-11",
        python: "implexity.implicit.api",
        doc: "``POST /v1/implicit/studies/<id>``: ``op`` ``run`` or ``delete``.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/implicit/validate",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Check a document without storing it: problems, warnings, round trip.",
    },
    Pending {
        method: "POST",
        pattern: "/v1/physics/packages",
        body: BodyPolicy::Json,
        owner: "WP-07",
        python: "implexity.cae.api",
        doc: "",
    },
    Pending {
        method: "PUT",
        pattern: "/v1/agent/intent",
        body: BodyPolicy::Json,
        owner: "WP-13",
        python: "implexity.agent.api",
        doc: "",
    },
    Pending {
        method: "PUT",
        pattern: "/v1/implicit/model",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Store a model document; 422 lists every problem at once.",
    },
    Pending {
        method: "PUT",
        pattern: "/v1/implicit/problem",
        body: BodyPolicy::Json,
        owner: "WP-12",
        python: "implexity.implicit.api",
        doc: "Persist engineering intent; unsupported backend setup is refused.",
    },
];

const SERVER_MODULE: &str = "implexity.server";

fn handler<F>(f: F) -> Handler
where
    F: Fn(&Request) -> Result<Reply, RouteError> + Send + Sync + 'static,
{
    Arc::new(f)
}

fn job_error_reply(e: JobError) -> Reply {
    match e {
        JobError::NotImplemented(m) => Reply::err(501, &m, Value::Null),
        JobError::Case(problems) => {
            Reply::json(&serde_json::json!({"error": "case rejected", "problems": problems}), 422)
        }
        JobError::Superseded(m) => Reply::err(409, &m, Value::Null),
        JobError::Failed { kind, message } => {
            Reply::err(500, &message, Value::String(format!("{kind}: {message}")))
        }
    }
}

fn no_backend() -> Reply {
    Reply::err(501, NO_BACKEND, Value::Null)
}

fn submit_preview(req: &Request, op: PreviewOp) -> Result<Reply, RouteError> {

    body_get(req.json(), "channel")?;
    let job = crate::ws::submit(&req.service, op, req.json())?;
    finish(&job, req.json())
}



pub fn kernel_registry() -> Result<Registry, RouteDeclarationError> {
    let mut r = Registry::new();
    let m = SERVER_MODULE;
    let index = handler(|_req| Ok(Reply::redirect("/viewer/model.html")));
    r.route(
        "GET",
        "/",
        BodyPolicy::None,
        "The single authoritative production workbench.",
        m,
        Arc::clone(&index),
    )?;
    r.route(
        "GET",
        "/index.html",
        BodyPolicy::None,
        "The single authoritative production workbench.",
        m,
        index,
    )?;
    r.route(
        "GET",
        "/viewer/<file>",
        BodyPolicy::None,
        "One file out of the viewer directory, if one is configured.",
        m,
        handler(|req| {
            let name = req.ident.as_deref().unwrap_or("");
            if name.is_empty() || name == "index.html" {
                return Ok(Reply::redirect("/viewer/model.html"));
            }
            Ok(crate::rust_extension::serve_viewer(&req.service, name))
        }),
    )?;
    r.route(
        "GET",
        "/v1/model",
        BodyPolicy::None,
        "The design, the backend, the envelope, the LOD ladder, the endpoints.",
        m,
        handler(|req| {
            Ok(match req.service.backend() {
                None => no_backend(),
                Some(b) => b.model_info().map_or_else(job_error_reply, |v| Reply::json(&v, 200)),
            })
        }),
    )?;
    r.route(
        "GET",
        "/v1/fidelity",
        BodyPolicy::None,
        "What the preview is and is not, stated in measured numbers.",
        m,
        handler(|req| {
            Ok(match req.service.backend() {
                None => no_backend(),
                Some(b) => b.fidelity().map_or_else(job_error_reply, |v| Reply::json(&v, 200)),
            })
        }),
    )?;
    r.route(
        "GET",
        "/v1/stats",
        BodyPolicy::None,
        "Pool, cache, LOD budget, WebSocket clients and frames dropped.",
        m,
        handler(|req| Ok(Reply::json(&req.service.stats(), 200))),
    )?;
    r.route(
        "GET",
        "/v1/lod",
        BodyPolicy::None,
        "The level-of-detail ladder and the budget's current estimate.",
        m,
        handler(|req| Ok(Reply::json(&req.service.lod_info(), 200))),
    )?;
    r.route(
        "GET",
        "/v1/stream",
        BodyPolicy::None,
        "The WebSocket: server-push for hosts that can open one.",
        m,
        handler(|req| Ok(crate::ws::validate_upgrade(req))),
    )?;
    r.route(
        "GET",
        "/v1/health",
        BodyPolicy::None,
        "Liveness, and which backend answered.",
        m,
        handler(|req| Ok(Reply::json(&req.service.health(), 200))),
    )?;
    add_preview_routes(&mut r)?;
    add_pending_routes(&mut r)?;
    Ok(r)
}

fn add_pending_routes(r: &mut Registry) -> Result<(), RouteDeclarationError> {
    for p in PENDING {
        let spec = format!("{} {}", p.method, p.pattern);
        let (owner, python) = (p.owner, p.python);
        r.route(
            p.method,
            p.pattern,
            p.body,
            p.doc,
            p.python,
            handler(move |_req| Ok(Reply::not_yet_ported(&spec, owner, python))),
        )?;
    }
    Ok(())
}

fn add_preview_routes(r: &mut Registry) -> Result<(), RouteDeclarationError> {
    let m = SERVER_MODULE;
    r.route(
        "POST",
        "/v1/params",
        BodyPolicy::Json,
        "Move one or more control channels; the design version bumps.",
        m,
        handler(|req| {
            let Some(b) = req.service.backend() else { return Ok(no_backend()) };
            Ok(match b.set_params(req.json()) {
                Ok(info) => {
                    let version = info.get("design_version").cloned().unwrap_or(Value::Null);
                    req.service.broadcast(&serde_json::json!({"event": "params", "design_version": version}));
                    Reply::json(&info, 200)
                }
                Err(e) => job_error_reply(e),
            })
        }),
    )?;
    r.route(
        "POST",
        "/v1/provenance/field",
        BodyPolicy::Json,
        "Bind an ad-hoc sampled field export to the live design identity.",
        m,
        handler(|req| {
            let Some(b) = req.service.backend() else { return Ok(no_backend()) };
            Ok(match b.field_record(req.json()) {
                Ok(v) => Reply::json(&v, 200),
                Err(JobError::Case(problems)) => Reply::json(
                    &serde_json::json!({"error": "field provenance was refused", "problems": problems}),
                    422,
                ),
                Err(e) => job_error_reply(e),
            })
        }),
    )?;
    r.route(
        "POST",
        "/v1/compare",
        BodyPolicy::Json,
        "How far this preview is from the one the exporter would produce.",
        m,
        handler(|req| submit_preview(req, PreviewOp::Compare)),
    )?;
    r.route(
        "POST",
        "/v1/probe",
        BodyPolicy::Json,
        "Control values and derived geometry at named points.",
        m,
        handler(|req| submit_preview(req, PreviewOp::Probe)),
    )?;
    r.route(
        "POST",
        "/v1/section",
        BodyPolicy::Json,
        "A section plane: contours, image, or the raw field.",
        m,
        handler(|req| submit_preview(req, PreviewOp::Section)),
    )?;
    r.route(
        "POST",
        "/v1/slab",
        BodyPolicy::Json,
        "A view-aligned slab, marching-cubed.",
        m,
        handler(|req| submit_preview(req, PreviewOp::Slab)),
    )
}
