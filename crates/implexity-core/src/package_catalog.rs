// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use crate::distributions::DistributionSet;
use crate::error::{CaeError, CaeResult};
use crate::ids::{is_absolute_module, is_snake_case};
use crate::py_repr::repr_str;

pub const SCHEMA: &str = "implexity-package-catalog/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDescriptor {
    pub package_id: String,
    pub label: String,
    pub installer: String,
    pub scope: String,
    pub distribution: String,
}

impl PackageDescriptor {
    #[must_use]
    pub fn status_row(&self, loaded: bool) -> Value {
        let mut m = Map::new();
        m.insert("label".into(), json!(self.label));
        m.insert("installer".into(), json!(self.installer));
        m.insert("scope".into(), json!(self.scope));
        m.insert("distribution".into(), json!(self.distribution));
        m.insert("id".into(), json!(self.package_id));
        m.insert("loaded".into(), json!(loaded));
        Value::Object(m)
    }
}


pub fn validate_catalog(document: &Value) -> CaeResult<Vec<PackageDescriptor>> {
    let invalid = || CaeError::contract("invalid distribution package catalog");
    let Some(m) = document.as_object() else { return Err(invalid()) };
    if m.len() != 2 || !m.contains_key("schema") || !m.contains_key("packages") || m["schema"] != SCHEMA {
        return Err(invalid());
    }
    let Some(rows) = m["packages"].as_array().filter(|r| (1..=1024).contains(&r.len())) else {
        return Err(invalid());
    };
    let mut out: Vec<PackageDescriptor> = Vec::new();
    for row in rows {
        let Some(r) = row.as_object() else {
            return Err(CaeError::contract("package catalog entry has unknown or missing fields"));
        };
        let fields = ["package_id", "label", "installer", "scope"];
        if r.len() != 4 || !fields.iter().all(|f| r.contains_key(*f)) {
            return Err(CaeError::contract("package catalog entry has unknown or missing fields"));
        }
        let bounded = |v: &Value| {
            v.as_str().is_some_and(|s| {
                !s.is_empty() && s.chars().count() <= 4096 && !s.chars().any(|c| (c as u32) < 32)
            })
        };
        if !r.values().all(bounded) {
            return Err(CaeError::contract("package catalog entry must contain bounded plain strings"));
        }
        let text = |k: &str| r[k].as_str().unwrap_or_default().to_string();
        let name = text("package_id");
        if !is_snake_case(&name) || out.iter().any(|d| d.package_id == name) {
            return Err(CaeError::contract("package identifiers must be unique canonical snake_case"));
        }
        if !is_absolute_module(&text("installer")) {
            return Err(CaeError::contract("package installer must be an absolute Python module name"));
        }
        out.push(PackageDescriptor {
            package_id: name,
            label: text("label"),
            installer: text("installer"),
            scope: text("scope"),
            distribution: String::new(),
        });
    }
    Ok(out)
}


pub fn load_distribution_catalog(set: &DistributionSet) -> CaeResult<Vec<PackageDescriptor>> {
    let documents = set
        .catalogue_documents("packages")
        .map_err(|e| CaeError::contract(format!("cannot read the distribution package catalogues: {e}")))?;
    let mut result: Vec<PackageDescriptor> = Vec::new();
    for (distribution_id, document) in documents {
        for mut row in validate_catalog(&document)? {
            if result.iter().any(|d| d.package_id == row.package_id) {
                return Err(CaeError::contract(format!(
                    "package {} is declared by more than one distribution (again by {})",
                    repr_str(&row.package_id),
                    repr_str(&distribution_id)
                )));
            }
            row.distribution.clone_from(&distribution_id);
            result.push(row);
        }
    }
    Ok(result)
}
