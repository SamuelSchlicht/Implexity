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


use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::contracts::{CaeProvider, ProviderProblem};
use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, dumps};

pub const SCHEMA: &str = "implexity-coupling-inventory/1";
pub const STATUSES: [&str; 6] = [
    "active",
    "configured_zero",
    "available_not_selected",
    "postprocess_only",
    "unsupported",
    "selection_unknown",
];
pub const DERIVATIVES: [&str; 4] = ["declared_discrete", "declared_zero", "none", "unknown"];


pub fn validate_inventory(raw: &Value, provider_id: &str) -> CaeResult<Value> {
    if !raw.is_object() {
        return Err(CaeError::contract("coupling inventory must be a mapping"));
    }
    let encoded = dumps(raw, &DumpOptions::default().sorted(true));
    if encoded.len() > 262_144 {
        return Err(CaeError::contract("coupling inventory exceeds its metadata budget"));
    }
    let result = raw.clone();
    let Some(r) = result.as_object() else {
        return Err(CaeError::contract("coupling inventory must be a mapping"));
    };
    let required: BTreeSet<&str> =
        ["schema", "provider", "context", "solve_performed", "physical_qualification", "edges", "notes"]
            .into_iter()
            .collect();
    let keys: BTreeSet<&str> = r.keys().map(String::as_str).collect();
    if keys != required || r["schema"] != SCHEMA {
        return Err(CaeError::contract("unsupported coupling inventory schema"));
    }
    if r["provider"] != provider_id {
        return Err(CaeError::contract("coupling inventory provider mismatch"));
    }
    let context = r["context"].as_str().unwrap_or_default();
    if context != "authored_problem" && context != "capability_only" {
        return Err(CaeError::contract("coupling inventory context is not explicit"));
    }
    if r["solve_performed"] != json!(false) || r["physical_qualification"] != json!(false) {
        return Err(CaeError::contract(
            "declaration inspection cannot claim a solve or physical qualification",
        ));
    }
    let Some(edges) = r["edges"].as_array().filter(|e| e.len() <= 128) else {
        return Err(CaeError::contract("coupling inventory requires a bounded edge list"));
    };
    let edge_keys: BTreeSet<&str> =
        ["id", "source", "target", "mechanism", "status", "derivative_scope", "reason", "configuration"]
            .into_iter()
            .collect();
    let mut names = BTreeSet::new();
    for edge in edges {
        let Some(e) =
            edge.as_object().filter(|e| e.keys().map(String::as_str).collect::<BTreeSet<_>>() == edge_keys)
        else {
            return Err(CaeError::contract("malformed coupling inventory edge"));
        };
        for name in ["id", "source", "target", "mechanism", "reason"] {
            if e[name].as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err(CaeError::contract("coupling inventory requires explicit textual identities"));
            }
        }
        let id = e["id"].as_str().unwrap_or_default().to_string();
        if !names.insert(id) {
            return Err(CaeError::contract("duplicate coupling inventory identity"));
        }
        let status = e["status"].as_str().unwrap_or_default();
        let scope = e["derivative_scope"].as_str().unwrap_or_default();
        if !STATUSES.contains(&status) || !DERIVATIVES.contains(&scope) {
            return Err(CaeError::contract("unknown coupling inventory status"));
        }
        if !e["configuration"].is_object() {
            return Err(CaeError::contract("coupling configuration must be an object"));
        }
        if matches!(status, "postprocess_only" | "unsupported") && scope != "none" {
            return Err(CaeError::contract("unimplemented feedback cannot declare a derivative"));
        }
        if context == "capability_only" && matches!(status, "active" | "configured_zero") {
            return Err(CaeError::contract("active coupling needs an authored problem"));
        }
    }
    if !r["notes"].as_array().is_some_and(|n| n.iter().all(Value::is_string)) {
        return Err(CaeError::contract("coupling inventory notes must be strings"));
    }
    Ok(result)
}


pub fn inspect_inventory(
    provider: &dyn CaeProvider,
    provider_id: &str,
    problem: Option<&ProviderProblem>,
) -> CaeResult<Value> {
    match provider.coupling_inventory(problem) {
        None => Ok(json!({"available": false, "reason": "provider_inventory_not_declared"})),
        Some(raw) => Ok(json!({"available": true, "inventory": validate_inventory(&raw, provider_id)?})),
    }
}
