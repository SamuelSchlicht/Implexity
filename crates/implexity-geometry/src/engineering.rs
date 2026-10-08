// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::contributions::ContributionRegistry;
use implexity_core::objective_terms;
use serde_json::{Value, json};

#[must_use]
pub fn catalogue(reg: &ContributionRegistry, backend: Option<&str>) -> Value {
    let terms: Vec<Value> = objective_terms::catalogue(reg, backend)
        .iter()
        .map(|row| {
            let term = row["term"].as_str().unwrap_or_default();
            json!({
                "id": term, "label": term.replace('_', " "), "family": row["family"], "backend": row["backend"],
                "units": row["units"], "kind": row["kind"], "description": row["description"],
                "reads": row["reads"], "knobs": row["knobs"], "direction": row["direction"],
                "differentiable": true,
                "evaluation": "same coupled state and term implementation used by optimisation",
                "derivative": "reverse-mode AD through implicit occupancy and CAE",
            })
        })
        .collect();
    let note = if terms.is_empty() {
        json!("no physics package contributing objective terms is loaded")
    } else {
        Value::Null
    };
    json!({
        "kind": "implicit_engineering_catalogue",
        "schema": "implexity-engineering/1",
        "response_count": terms.len(),
        "responses": terms,
        "responses_note": note,
        "constraint_types": [
            {"id": "volume_fraction", "units": "fraction", "differentiable": true,
             "description": "Model occupancy fraction. Target may be a number or the start design; an inequality under MMA, a differentiable penalty under Adam."},
            {"id": "parameter_bounds", "units": "parameter native units", "differentiable": false,
             "description": "Hard box bounds on every free parameter, enforced by projection outside the traced CAE loss."},
            {"id": "response_bound", "units": "response units", "differentiable": true,
             "description": "An upper or lower bound on a response the physics binding declares or on an active objective term; an inequality differentiated through the physics under MMA, a one-sided penalty under Adam."},
        ],
        "derivative_surfaces": [
            {"id": "sensitivity", "endpoint": "POST /v1/implicit/sensitivity",
             "description": "One current design, one authored scalar objective, all selected design-variable derivatives; no update."},
            {"id": "derivatives", "endpoint": "POST /v1/implicit/derivatives",
             "description": "Jacobian, matrix-free JVP and VJP of authored engineering responses."},
            {"id": "optimize", "endpoint": "POST /v1/implicit/optimize",
             "description": "Repeated use of the same derivative to update the implicit model."},
        ],
        "identity": "responses, sensitivities and optimisation all bind to the same model structure_id/content_id and case declaration",
        "note": "This catalogue is a semantic view over the existing objective and constraint implementations; it does not duplicate solver equations.",
    })
}
