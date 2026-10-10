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
