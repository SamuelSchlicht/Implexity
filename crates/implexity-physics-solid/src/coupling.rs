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
