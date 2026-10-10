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



use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::provider_ops::design_operations;
use serde_json::{Map, Value, json};

use crate::canonical::{canonical_sha256, canonical_text};

pub const SCHEMA: &str = "implexity-workspace-capabilities/1";

fn finite_json(value: &Value) -> CaeResult<Value> {
    implexity_core::wire::ensure_finite(value)
        .map_err(|_| CaeError::contract("workspace capability declarations must be finite JSON data"))?;
    implexity_core::json::parse_strict(&canonical_text(value)).map_err(|e| CaeError::contract(e.to_string()))
}

fn checked_provider_id(value: Option<&str>) -> CaeResult<Option<&str>> {
    match value {
        None => Ok(None),
        Some(v) if !v.trim().is_empty() && v == v.trim() && v.chars().count() <= 160 => Ok(Some(v)),
        Some(_) => Err(CaeError::contract("workspace capability provider must be canonical nonempty text")),
    }
}

fn single_provider_section(catalogue: &Value, provider_id: &str) -> CaeResult<Value> {
    let Some(rows) = catalogue.get("providers").and_then(Value::as_array) else {
        return Err(CaeError::contract("provider control catalogue omitted providers"));
    };
    rows.iter()
        .find(|r| r.get("provider_id").and_then(Value::as_str) == Some(provider_id))
        .cloned()
        .ok_or_else(|| {
            CaeError::contract(format!("provider control catalogue omitted {}", repr_str(provider_id)))
        })
}

fn provider_owned_declaration(
    provider: &dyn implexity_core::contracts::CaeProvider,
    hook: &str,
    problem: Option<&Value>,
    fallback: Option<&Value>,
) -> CaeResult<Value> {
    let value = match design_operations(provider).and_then(|o| o.workspace_declaration(hook, problem)) {
        Some(v) => Some(v?),
        None => fallback.cloned(),
    };
    match value.filter(|v| !v.is_null()) {
        None => Ok(if hook == "study_templates" {
            json!([])
        } else {
            json!({"available": false, "reason": format!("provider_did_not_publish_{hook}")})
        }),
        Some(v) => finite_json(&v),
    }
}


#[allow(clippy::too_many_lines)]
pub fn workspace_capabilities(
    provider_id: Option<&str>,
    problem_context: Option<&Value>,
    context_source: &str,
) -> CaeResult<Value> {
    let registries = implexity_core::registries::global();
    let provider_id = checked_provider_id(provider_id)?;
    let providers =
        registries.providers.catalogue(&registries.addins, Some(&crate::cae_runtime::catalogue_traits))?;
    let addins = registries.addins.catalogue();
    let names: Vec<String> = providers.keys().cloned().collect();
    let selected = provider_id.map(str::to_string).or_else(|| (names.len() == 1).then(|| names[0].clone()));
    let token = registries.providers.binding_token();
    let mut result = Map::new();
    result.insert("schema".into(), Value::String(SCHEMA.into()));
    result.insert(
        "provider_registry".into(),
        json!({"generation": token.generation, "fingerprint": token.fingerprint}),
    );
    result.insert("available_provider_ids".into(), json!(names));
    result.insert("selected_provider".into(), selected.clone().map_or(Value::Null, Value::String));
    result.insert("physics_addins".into(), Value::Object(addins));
    result.insert(
        "problem_context".into(),
        json!({"source": context_source, "supplied": problem_context.is_some()}),
    );
    result.insert(
        "request_contract".into(),
        json!({
            "inspection_only": true,
            "provider_owns_physical_meaning": true,
            "provider_owns_constraint_semantics": true,
            "all_provider_design_coordinates_active_by_default": true,
            "explicit_scope_limitation_required_to_deactivate_coordinates": true,
            "analysis_design_and_render_resolution_are_distinct": true,
            "exact_spatial_refinement_required_before_commit": true,
            "differentiable_guidance_is_not_an_exact_guarantee": true,
            "exact_final_guarantees_must_be_provider_declared": true,
        }),
    );
    match selected {
        None => {
            result.insert("selection_required".into(), Value::Bool(true));
        }
        Some(selected) => {
            let Some(provider) = providers.get(&selected).filter(|r| r.is_object()).cloned() else {
                return Err(CaeError::contract(format!("unknown CAE provider {}", repr_str(&selected))));
            };
            let traits = provider.get("traits").and_then(Value::as_object).cloned().unwrap_or_default();
            let boundaries = implexity_solve::boundary_control::boundary_catalogue(
                Some(&selected),
                problem_context,
                context_source,
            )?;
            let couplings = crate::coupling_control::coupling_catalogue(
                Some(&selected),
                problem_context,
                context_source,
            )?;
            let provider_object = registries.providers.get(&selected)?;
            let provider_problem =
                crate::coupling_control::problem_for_provider(&selected, problem_context, true);
            let list = |key: &str| {
                provider
                    .get(key)
                    .filter(|v| crate::pyval::truthy(Some(v)))
                    .cloned()
                    .unwrap_or_else(|| json!([]))
            };
            result.insert("selection_required".into(), Value::Bool(false));
            result.insert("design_coordinates".into(), list("design_coordinates"));
            result.insert("responses".into(), list("responses"));
            result.insert(
                "response_metadata".into(),
                provider
                    .get("response_metadata")
                    .filter(|v| crate::pyval::truthy(Some(v)))
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
            result.insert("result_fields".into(), list("fields"));
            result.insert("boundary_control".into(), single_provider_section(&boundaries, &selected)?);
            result.insert("coupling_control".into(), single_provider_section(&couplings, &selected)?);
            let object = provider_object.as_ref();
            result.insert(
                "constraint_controls".into(),
                provider_owned_declaration(
                    object,
                    "constraint_controls",
                    provider_problem,
                    traits.get("constraint_controls"),
                )?,
            );
            result.insert(
                "discretization_control".into(),
                provider_owned_declaration(
                    object,
                    "discretization_control",
                    provider_problem,
                    traits.get("discretization_control"),
                )?,
            );
            let templates = traits
                .get("study_templates")
                .filter(|v| crate::pyval::truthy(Some(v)))
                .or_else(|| provider.get("study_templates"));
            result.insert(
                "study_templates".into(),
                provider_owned_declaration(object, "study_templates", provider_problem, templates)?,
            );
            let editor_schema = provider.get("editor").and_then(|e| e.get("schema"));
            result.insert(
                "editor_schema".into(),
                provider_owned_declaration(object, "editor_schema", provider_problem, editor_schema)?,
            );
            result.insert(
                "truth_status".into(),
                traits.get("truth_status").cloned().unwrap_or_else(|| Value::String("provider_owned".into())),
            );
            result.insert("provider".into(), provider);
        }
    }
    let value = Value::Object(result);
    implexity_core::wire::ensure_finite(&value)
        .map_err(|_| CaeError::contract("workspace capability declarations must be finite JSON data"))?;
    let digest = canonical_sha256(&value);
    let mut out = value;
    if let Value::Object(m) = &mut out {
        m.insert("content_sha256".into(), Value::String(digest));
    }
    finite_json(&out)
}
