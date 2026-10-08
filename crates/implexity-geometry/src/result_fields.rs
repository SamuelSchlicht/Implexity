// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::backends::selected_physics;
use implexity_core::contributions::ContributionRegistry;
use serde_json::{Value, json};

#[must_use]
pub fn catalogue(reg: &ContributionRegistry, backend: Option<&str>) -> Value {
    let selected = selected_physics(reg, backend);
    let mut rows = Vec::new();
    if let Some(b) = &selected {
        for r in b.result_fields() {
            rows.push(json!({"id": r.id, "source": r.source, "rank": r.rank, "location": r.location, "units": r.units,
                "description": r.doc, "differentiable": true, "backend": b.name()}));
        }
    }
    let note = if selected.is_some() {
        "Fields are declarations of state already produced by the selected backend; no synthetic result quantity is introduced here. Scalar engineering responses are separate reductions over these fields."
    } else {
        "no physics backend is selected; load a physics package"
    };
    json!({"kind": "implicit_result_field_catalogue", "schema": "implexity-result-fields/1",
        "backend": selected.as_ref().map(|b| b.name().to_string()), "count": rows.len(), "fields": rows, "note": note})
}


