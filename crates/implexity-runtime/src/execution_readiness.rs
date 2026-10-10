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


use implexity_core::CaeResult;
use implexity_core::orchestration::{AddInRegistry, RuntimeRoute};
use serde_json::{Map, Value};



pub fn inspect_execution(
    intent: &Value,
    plan: &Map<String, Value>,
    context: Option<&Map<String, Value>>,
    registry: &AddInRegistry,
) -> CaeResult<Map<String, Value>> {
    let mut out = plan.clone();
    out.insert("graph_status".into(), plan.get("status").cloned().unwrap_or(Value::Null));
    let ctx: Map<String, Value> = match context {
        Some(c) if !c.is_empty() => c.clone(),
        _ => match intent.get("context") {
            Some(Value::Object(m)) if !m.is_empty() => m.clone(),
            _ => Map::new(),
        },
    };
    let authored = match intent.get("authoring") {
        Some(Value::Object(m)) if !m.is_empty() => m.clone(),
        _ => Map::new(),
    };
    let child_inputs: Map<String, Value> = [ctx.get("provider_problems"), authored.get("provider_problems")]
        .into_iter()
        .flatten()
        .find(|v| crate::pyval::truthy(Some(v)))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut issues: Vec<Value> = Vec::new();
    let mut reports = Map::new();
    let selected: Vec<String> = plan
        .get("selected_addins")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(crate::pyval::py_str).collect())
        .unwrap_or_default();
    for aid in selected {
        let entry = registry.get(&aid)?;
        let Some(provider) = entry.adapter.as_ref().and_then(|a| a.provider()) else { continue };
        let raw = child_inputs
            .get(&aid)
            .or_else(|| ctx.get("problem"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let attempt = (|| -> CaeResult<()> {
            let p = provider.normalise_problem(&raw)?;
            if let Some(ops) = implexity_optim::provider_ops::design_operations(provider.as_ref()) {
                let responses = provider.capabilities()?.to_map();
                let available: Vec<String> = responses
                    .get("responses")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(crate::pyval::py_str).collect())
                    .unwrap_or_default();
                let goals = intent.get("goals").and_then(Value::as_array).cloned().unwrap_or_default();
                let names: Vec<String> = goals
                    .iter()
                    .filter_map(|g| g.get("response"))
                    .map(crate::pyval::py_str)
                    .filter(|r| available.contains(r))
                    .collect();
                if let Some(result) = ops.validate_response_selection(&p, &names) {
                    result?;
                }
            }
            let rep = provider.preflight(&p, None)?;
            let ok = crate::pyval::truthy(rep.get("ok"));
            reports.insert(aid.clone(), Value::Object(rep));
            if !ok {
                issues.push(Value::String(format!("{aid}: provider preflight rejected authored input")));
            }
            if entry.contract.runtime_route == RuntimeRoute::MatureJob {
                issues.push(Value::String(format!(
                    "{aid}: mature implicit-job provider requires its native model-aware preflight; array dispatch is not supported"
                )));
            }
            Ok(())
        })();
        if let Err(e) = attempt {
            issues.push(Value::String(format!("{aid}: {}", e.message())));
        }
    }
    let ready = plan.get("status").and_then(Value::as_str) == Some("ready");
    out.insert(
        "execution_status".into(),
        Value::String(
            if !issues.is_empty() || !ready { "blocked" } else { "requires_model_preflight" }.into(),
        ),
    );
    out.insert("execution_issues".into(), Value::Array(issues.clone()));
    out.insert("child_preflights".into(), Value::Object(reports));
    out.insert("optimization_admitted".into(), Value::Bool(false));
    if !issues.is_empty() && out.get("status").and_then(Value::as_str) == Some("ready") {
        out.insert("status".into(), Value::String("needs_authoring".into()));
        let mut missing: Vec<Value> = match out.get("missing_authoring") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        missing.extend(issues);
        out.insert("missing_authoring".into(), Value::Array(missing));
    }
    Ok(out)
}
