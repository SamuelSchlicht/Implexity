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
use crate::ids::is_field_name;
use crate::py_repr::repr_str;

pub const SCHEMA: &str = "implexity-field-source-catalog/1";

pub type FieldSourceCatalog = BTreeMap<String, BTreeMap<String, String>>;


pub fn validate_field_source_catalog(document: &Value) -> CaeResult<FieldSourceCatalog> {
    let invalid = || CaeError::contract("invalid field-source deployment catalog");
    let Some(m) = document.as_object() else { return Err(invalid()) };
    if m.len() != 2 || m.get("schema") != Some(&Value::String(SCHEMA.into())) {
        return Err(invalid());
    }
    let Some(rows) = m.get("components").and_then(Value::as_array).filter(|r| (1..=1024).contains(&r.len()))
    else {
        return Err(invalid());
    };
    let mut result = FieldSourceCatalog::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let Some(r) = row
            .as_object()
            .filter(|r| r.len() == 2 && r.contains_key("component_id") && r.contains_key("response_units"))
        else {
            return Err(CaeError::contract("invalid field-source catalog entry"));
        };
        let name = match r["component_id"].as_str() {
            Some(n)
                if n.chars().count() <= 128
                    && is_field_name(n)
                    && n.to_lowercase() == n
                    && !result.contains_key(n) =>
            {
                n.to_string()
            }
            _ => return Err(CaeError::contract("unique canonical field-source identifier required")),
        };
        let Some(units) = r["response_units"].as_object().filter(|u| (1..=128).contains(&u.len())) else {
            return Err(CaeError::contract("bounded nonempty field-source units required"));
        };
        let mut out = BTreeMap::new();
        for (field, unit) in units {
            let ok_unit = unit.as_str().is_some_and(|u| {
                let n = u.chars().count();
                (1..=128).contains(&n) && !u.chars().any(|c| (c as u32) < 32)
            });
            if field.chars().count() > 128 || !is_field_name(field) || names.contains(field) || !ok_unit {
                return Err(CaeError::contract("invalid or duplicate field-source response or unit"));
            }
            names.insert(field.clone());
            out.insert(field.clone(), unit.as_str().unwrap_or_default().to_string());
        }
        result.insert(name, out);
    }
    Ok(result)
}


pub fn load_field_source_catalog(set: &DistributionSet) -> CaeResult<FieldSourceCatalog> {
    let documents = set
        .catalogue_documents("field_sources")
        .map_err(|e| CaeError::contract(format!("cannot read installed field-source catalogues: {e}")))?;
    let mut merged = FieldSourceCatalog::new();
    let mut responses: BTreeSet<String> = BTreeSet::new();
    for (distribution_id, document) in documents {
        for (name, units) in validate_field_source_catalog(&document)? {
            if merged.contains_key(&name) || units.keys().any(|k| responses.contains(k)) {
                return Err(CaeError::contract(format!(
                    "field source {} or one of its responses is declared by more than one distribution (again by {})",
                    repr_str(&name),
                    repr_str(&distribution_id)
                )));
            }
            responses.extend(units.keys().cloned());
            merged.insert(name, units);
        }
    }
    Ok(merged)
}
