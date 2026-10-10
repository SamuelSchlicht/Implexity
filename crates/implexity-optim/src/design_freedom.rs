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



use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::design::NamedArrays;
use crate::numeric::str_list_repr;
use crate::pyval::{str_tuple, text_or};

pub const ROLES: [&str; 10] = [
    "fixed_geometry",
    "parametric_geometry",
    "topology_free",
    "shape_free",
    "phase_free",
    "material_free",
    "cooling_only",
    "preserve_interface",
    "manufacturing_protected",
    "manual_locked",
];

pub const FREE_ROLES: [&str; 6] =
    ["parametric_geometry", "topology_free", "shape_free", "phase_free", "material_free", "cooling_only"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignBlock {
    pub id: String,
    pub role: String,
    pub coordinates: Vec<String>,
    pub label: String,
}

impl DesignBlock {

    pub fn from_dict(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("every design-freedom block must be a mapping"));
        };
        let ident = text_or(map, &["id"], "");
        if ident.is_empty() {
            return Err(CaeError::contract("every design-freedom block requires a non-empty id"));
        }
        let role = text_or(map, &["role"], "manual_locked");
        if !ROLES.contains(&role.as_str()) {
            return Err(CaeError::contract(format!(
                "design block {}: unsupported role {}",
                repr_str(&ident),
                repr_str(&role)
            )));
        }
        let coords = match crate::pyval::first_truthy(map, &["coordinates"]) {
            None => Vec::new(),
            Some(v) => str_tuple(v, &format!("design block {} coordinates", repr_str(&ident)))?,
        };
        if coords.is_empty() {
            return Err(CaeError::contract(format!(
                "design block {}: at least one coordinate is required",
                repr_str(&ident)
            )));
        }
        let mut unique = coords.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != coords.len() {
            return Err(CaeError::contract(format!(
                "design block {}: coordinates must be unique",
                repr_str(&ident)
            )));
        }
        let label = crate::pyval::raw_text_or(map, &["label"], &ident);
        Ok(Self { id: ident, role, coordinates: coords, label })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), Value::String(self.id.clone()));
        m.insert("role".into(), Value::String(self.role.clone()));
        m.insert(
            "coordinates".into(),
            Value::Array(self.coordinates.iter().cloned().map(Value::String).collect()),
        );
        m.insert("label".into(), Value::String(self.label.clone()));
        Value::Object(m)
    }
}


pub fn validate_blocks(raw: Option<&[Value]>, coordinates: &[String]) -> CaeResult<Vec<DesignBlock>> {
    if coordinates.first().map(String::as_str) != Some(TOPOLOGY_COORDINATE)
        || coordinates.iter().filter(|c| *c == TOPOLOGY_COORDINATE).count() != 1
    {
        return Err(CaeError::contract(format!(
            "{TOPOLOGY_COORDINATE} must be the first and unique mandatory coordinate"
        )));
    }
    let blocks = raw.unwrap_or_default().iter().map(DesignBlock::from_dict).collect::<CaeResult<Vec<_>>>()?;
    let mut seen: Vec<&str> = Vec::new();
    let mut owners: Vec<(&str, &str)> = Vec::new();
    for block in &blocks {
        if seen.contains(&block.id.as_str()) {
            return Err(CaeError::contract(format!("duplicate design block id {}", repr_str(&block.id))));
        }
        seen.push(&block.id);
        for coord in &block.coordinates {
            if !coordinates.contains(coord) {
                return Err(CaeError::contract(format!(
                    "design block {} references unknown coordinate {}",
                    repr_str(&block.id),
                    repr_str(coord)
                )));
            }
            if let Some((_, owner)) = owners.iter().find(|(c, _)| c == coord) {
                return Err(CaeError::contract(format!(
                    "coordinate {} belongs to both {} and {}; one authoritative block must own each coordinate",
                    repr_str(coord),
                    repr_str(owner),
                    repr_str(&block.id)
                )));
            }
            owners.push((coord, &block.id));
        }
    }
    if !blocks.is_empty() && !owners.iter().any(|(c, _)| *c == TOPOLOGY_COORDINATE) {
        return Err(CaeError::contract(format!(
            "hierarchical design freedom must assign {TOPOLOGY_COORDINATE} to a block"
        )));
    }
    Ok(blocks)
}


pub fn active_coordinates(
    blocks: &[DesignBlock],
    released_blocks: Option<&[String]>,
    all_coordinates: &[String],
) -> CaeResult<Vec<String>> {
    if blocks.is_empty() {
        return Ok(all_coordinates.to_vec());
    }
    let release: Vec<&String> = released_blocks.unwrap_or_default().iter().collect();
    let mut unknown: Vec<&String> =
        release.iter().copied().filter(|r| !blocks.iter().any(|b| &b.id == *r)).collect();
    unknown.sort();
    unknown.dedup();
    if !unknown.is_empty() {
        return Err(CaeError::contract(format!(
            "schedule releases unknown design block(s): {}",
            str_list_repr(&unknown)
        )));
    }
    let mut active = Vec::new();
    for block in blocks {
        if release.contains(&&block.id) {
            if !FREE_ROLES.contains(&block.role.as_str()) {
                return Err(CaeError::contract(format!(
                    "schedule cannot release block {} with role {}; change its role deliberately in the GUI first",
                    repr_str(&block.id),
                    repr_str(&block.role)
                )));
            }
            active.extend(block.coordinates.iter().cloned());
        }
    }
    Ok(active)
}


pub fn freeze_inactive(
    candidate: &NamedArrays,
    stage_entry: &NamedArrays,
    active: &[String],
) -> CaeResult<NamedArrays> {
    let mut out = NamedArrays::new();
    for (name, value) in candidate.iter() {
        if active.iter().any(|a| a == name) {
            out.insert(name, value.clone());
        } else {
            let entry = stage_entry.get(name).ok_or_else(|| {
                CaeError::contract(format!("stage entry design omits coordinate {}", repr_str(name)))
            })?;
            out.insert(name, entry.clone());
        }
    }
    Ok(out)
}

