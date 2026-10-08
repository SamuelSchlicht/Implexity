// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use implexity_core::CaeError;
use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge};

#[must_use]
pub fn edge(source: &str, target: &str, quantity: &str, mode: &str, reason: &str) -> CouplingEdge {
    CouplingEdge {
        source: source.into(),
        target: target.into(),
        quantity: quantity.into(),
        mode: mode.into(),
        required: true,
        reason: reason.into(),
    }
}


pub fn with_material_couplings(
    mut base: CouplingDeclaration,
    row: &Value,
) -> Result<CouplingDeclaration, CaeError> {
    if row.is_null() {
        return Ok(base);
    }
    let component = crate::history::selected(row["component"].as_str().unwrap_or_default())?;
    let deps = component.state_dependencies_for(&row["settings"])?;
    let effects = component.property_effects();
    base.active_physics.push("material_state".into());
    for (name, source, quantity) in
        [("temperature", "thermal", "temperature_history"), ("stress", "structure", "stress_history")]
    {
        if deps.iter().any(|d| d == name) {
            base.edges.push(edge(
                source,
                "material_state",
                quantity,
                "monolithic",
                "explicit driving dependency of the selected constitutive law",
            ));
        }
    }
    if effects.contains(&"k") {
        base.edges.push(edge(
            "material_state",
            "thermal",
            "evolved_conductivity",
            "monolithic",
            "current constitutive property in the same conduction residual",
        ));
    }
    if effects.iter().any(|e| *e == "yield_stress" || *e == "creep_rate_ref") {
        base.edges.push(edge(
            "material_state",
            "structure",
            "evolved_strength_and_creep",
            "monolithic",
            "current properties in plastic and creep return mapping",
        ));
    }
    base.edges.push(edge(
        "material_state",
        "thermal",
        "material_energy_exchange",
        "monolithic",
        "explicit storage/source/release contract in the same energy residual; declared zero allowed",
    ));
    base.closed_loops = vec![base.active_physics.clone()];
    base.notes.push(
        "Material state is an explicitly selected local law, NOT inferred neutron transport or full ageing qualification."
            .into(),
    );
    Ok(base)
}
