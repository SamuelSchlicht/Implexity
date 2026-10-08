// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
