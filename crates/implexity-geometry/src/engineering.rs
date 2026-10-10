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
