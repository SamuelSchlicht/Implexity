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



use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::distributions::DistributionSet;
use crate::error::{CaeError, CaeResult};
use crate::ids::{is_field_name, is_snake_case};
use crate::py_repr::repr_str;

pub const SCHEMA: &str = "implexity-history-response-catalog/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryResponseManifest {
    pub response_units: Vec<(String, String)>,
    pub requires: Vec<String>,
}


pub fn validate_history_catalog(document: &Value) -> CaeResult<BTreeMap<String, HistoryResponseManifest>> {
    let invalid = || CaeError::contract("invalid distribution history-response catalog");
    let Some(m) = document.as_object() else { return Err(invalid()) };
    if m.len() != 2 || m.get("schema") != Some(&Value::String(SCHEMA.into())) {
        return Err(invalid());
    }
    let Some(rows) = m.get("components").and_then(Value::as_array).filter(|r| (1..=1024).contains(&r.len()))
    else {
        return Err(invalid());
    };
    let mut result = BTreeMap::new();
    let mut responses: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let Some(r) = row.as_object().filter(|r| {
            r.len() == 3
                && r.contains_key("component_id")
                && r.contains_key("response_units")
                && r.contains_key("requires")
        }) else {
            return Err(CaeError::contract("history response entry has missing/unknown fields"));
        };
        let name = match r["component_id"].as_str() {
            Some(n) if n.chars().count() <= 128 && is_snake_case(n) && !result.contains_key(n) => {
                n.to_string()
            }
            _ => return Err(CaeError::contract("unique canonical history component identifiers required")),
        };
        let Some(units) = r["response_units"].as_object().filter(|u| (1..=128).contains(&u.len())) else {
            return Err(CaeError::contract("bounded nonempty response-unit mapping required"));
        };
        let mut pairs = Vec::new();
        for (field, unit) in units {
            let ok_unit = unit.as_str().is_some_and(|u| {
                let n = u.chars().count();
                (1..=128).contains(&n) && !u.chars().any(|c| (c as u32) < 32)
            });
            if field.chars().count() > 128 || !is_field_name(field) || responses.contains(field) || !ok_unit {
                return Err(CaeError::contract("unique response names and bounded units required"));
            }
            responses.insert(field.clone());
            pairs.push((field.clone(), unit.as_str().unwrap_or_default().to_string()));
        }
        let requires: Option<Vec<String>> = r["requires"].as_array().and_then(|a| {
            a.iter()
                .map(|v| {
                    v.as_str().filter(|s| s.chars().count() <= 128 && is_field_name(s)).map(str::to_string)
                })
                .collect()
        });
        let Some(requires) = requires
            .filter(|q| (1..=128).contains(&q.len()) && q.iter().collect::<BTreeSet<_>>().len() == q.len())
        else {
            return Err(CaeError::contract("unique bounded sample names required"));
        };
        result.insert(name, HistoryResponseManifest { response_units: pairs, requires });
    }
    Ok(result)
}


pub fn load_history_catalog(set: &DistributionSet) -> CaeResult<BTreeMap<String, HistoryResponseManifest>> {
    let documents = set
        .catalogue_documents("history_responses")
        .map_err(|e| CaeError::contract(format!("cannot read installed history response catalogues: {e}")))?;
    let mut merged = BTreeMap::new();
    let mut responses: BTreeSet<String> = BTreeSet::new();
    for (distribution_id, document) in documents {
        for (name, manifest) in validate_history_catalog(&document)? {
            let fields: BTreeSet<String> = manifest.response_units.iter().map(|(f, _)| f.clone()).collect();
            if merged.contains_key(&name) || !responses.is_disjoint(&fields) {
                return Err(CaeError::contract(format!(
                    "history component {} or one of its responses is declared by more than one distribution (again by {})",
                    repr_str(&name),
                    repr_str(&distribution_id)
                )));
            }
            responses.extend(fields);
            merged.insert(name, manifest);
        }
    }
    Ok(merged)
}


pub fn advertised_history_responses(
    manifests: &BTreeMap<String, HistoryResponseManifest>,
    sample_names: &BTreeSet<String>,
) -> CaeResult<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for manifest in manifests.values() {
        if !manifest.requires.iter().all(|r| sample_names.contains(r)) {
            continue;
        }
        for (response, unit) in &manifest.response_units {
            if out.contains_key(response) {
                return Err(CaeError::contract(format!(
                    "ambiguous installed history response {}",
                    repr_str(response)
                )));
            }
            out.insert(response.clone(), unit.clone());
        }
    }
    Ok(out)
}


pub fn validate_history_manifest(
    manifests: &BTreeMap<String, HistoryResponseManifest>,
    name: &str,
    implementation: &HistoryResponseManifest,
) -> CaeResult<()> {
    let Some(declared) = manifests.get(name) else {
        return Err(CaeError::contract(format!(
            "{name}: missing trusted history-response deployment manifest"
        )));
    };
    let a: BTreeMap<&String, &String> = declared.response_units.iter().map(|(k, v)| (k, v)).collect();
    let b: BTreeMap<&String, &String> = implementation.response_units.iter().map(|(k, v)| (k, v)).collect();
    if a != b || declared.requires != implementation.requires {
        return Err(CaeError::contract(format!(
            "{name}: implementation disagrees with its installed history-response manifest"
        )));
    }
    Ok(())
}
