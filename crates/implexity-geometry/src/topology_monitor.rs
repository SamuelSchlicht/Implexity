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


use std::collections::VecDeque;

use serde_json::{Map, Value, json};

use crate::pyfmt::str_repr;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TopologyMonitorError(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct TopologySignature {
    pub occupied_components: usize,
    pub void_components: usize,
    pub enclosed_voids: usize,
    pub occupied_fraction: f64,
    pub boundary_contacts: usize,
    pub shape: [usize; 3],
    pub threshold: f64,
}

impl TopologySignature {
    #[must_use]
    pub fn serialise(&self) -> Value {
        json!({"occupied_components": self.occupied_components, "void_components": self.void_components,
            "enclosed_voids": self.enclosed_voids, "occupied_fraction": self.occupied_fraction,
            "boundary_contacts": self.boundary_contacts, "shape": self.shape, "threshold": self.threshold})
    }
}

#[must_use]
pub fn components(mask: &[bool], shape: [usize; 3]) -> (usize, Vec<bool>) {
    let [nx, ny, nz] = shape;
    let idx = |i: usize, j: usize, k: usize| (i * ny + j) * nz + k;
    let mut visited = vec![false; mask.len()];
    let mut touches_boundary = Vec::new();
    for seed in 0..mask.len() {
        if !mask[seed] || visited[seed] {
            continue;
        }
        let mut queue = VecDeque::new();
        visited[seed] = true;
        queue.push_back((seed / (ny * nz), (seed / nz) % ny, seed % nz));
        let mut touches = false;
        while let Some((i, j, k)) = queue.pop_front() {
            if i == 0 || i == nx - 1 || j == 0 || j == ny - 1 || k == 0 || k == nz - 1 {
                touches = true;
            }
            let nb = [
                (i + 1 < nx).then(|| (i + 1, j, k)),
                (i > 0).then(|| (i - 1, j, k)),
                (j + 1 < ny).then(|| (i, j + 1, k)),
                (j > 0).then(|| (i, j - 1, k)),
                (k + 1 < nz).then(|| (i, j, k + 1)),
                (k > 0).then(|| (i, j, k - 1)),
            ];
            for (a, b, c) in nb.into_iter().flatten() {
                let q = idx(a, b, c);
                if mask[q] && !visited[q] {
                    visited[q] = true;
                    queue.push_back((a, b, c));
                }
            }
        }
        touches_boundary.push(touches);
    }
    (touches_boundary.len(), touches_boundary)
}


pub fn topology_signature(
    values: &[f64],
    shape: &[usize],
    role: &str,
    threshold: f64,
) -> Result<TopologySignature, TopologyMonitorError> {
    if shape.len() != 3 || shape.iter().any(|n| *n < 2) || values.len() != shape.iter().product::<usize>() {
        return Err(TopologyMonitorError("topology monitoring requires a three-dimensional field".into()));
    }
    if !values.iter().all(|v| v.is_finite()) {
        return Err(TopologyMonitorError("topology field contains non-finite values".into()));
    }
    let role = role.trim().to_lowercase();
    let occupied: Vec<bool> = match role.as_str() {
        "level_set" | "signed_distance" | "implicit" => values.iter().map(|v| *v <= threshold).collect(),
        "occupancy" | "density" | "material" => values.iter().map(|v| *v >= threshold).collect(),
        _ => {
            return Err(TopologyMonitorError(format!(
                "unsupported topology field role: {}",
                str_repr(&role)
            )));
        }
    };
    let sh = [shape[0], shape[1], shape[2]];
    let (occ, occ_b) = components(&occupied, sh);
    let void: Vec<bool> = occupied.iter().map(|o| !o).collect();
    let (voids, void_b) = components(&void, sh);
    #[allow(clippy::cast_precision_loss)]
    let fraction = occupied.iter().filter(|o| **o).count() as f64 / occupied.len() as f64;
    Ok(TopologySignature {
        occupied_components: occ,
        void_components: voids,
        enclosed_voids: void_b.iter().filter(|t| !**t).count(),
        occupied_fraction: fraction,
        boundary_contacts: occ_b.iter().filter(|t| **t).count(),
        shape: sh,
        threshold,
    })
}

#[must_use]
pub fn compare_topology(before: &TopologySignature, after: &TopologySignature) -> Value {
    let mut changes = Vec::new();
    for (kind, b, a) in [
        ("occupied_components", before.occupied_components, after.occupied_components),
        ("enclosed_voids", before.enclosed_voids, after.enclosed_voids),
        ("boundary_contacts", before.boundary_contacts, after.boundary_contacts),
    ] {
        if b != a {
            changes.push(json!({"kind": kind, "before": b, "after": a}));
        }
    }
    let changed = !changes.is_empty();
    json!({"changed": changed, "changes": changes, "occupied_fraction_delta": after.occupied_fraction - before.occupied_fraction,
        "before": before.serialise(), "after": after.serialise(), "severity": if changed { "warning" } else { "none" }})
}


pub fn monitor_if_applicable(
    before: &[f64],
    after: &[f64],
    shape: &[usize],
    metadata: Option<&Map<String, Value>>,
) -> Result<Option<Value>, TopologyMonitorError> {
    let empty = Map::new();
    let md = metadata.unwrap_or(&empty);
    let role = md
        .get("role")
        .or_else(|| md.get("field_role"))
        .map(crate::document::py_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let level = ["level_set", "signed_distance", "implicit"].contains(&role.as_str());
    if !level && !["occupancy", "density", "material"].contains(&role.as_str()) {
        return Ok(None);
    }
    let threshold = match md.get("topology_threshold") {
        None => {
            if level {
                0.0
            } else {
                0.5
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => s.trim().parse().map_err(|_| {
            TopologyMonitorError(format!("could not convert string to float: {}", str_repr(s)))
        })?,
        Some(_) => return Err(TopologyMonitorError("topology_threshold must be a number".into())),
    };
    let b = topology_signature(before, shape, &role, threshold)?;
    let a = topology_signature(after, shape, &role, threshold)?;
    Ok(Some(compare_topology(&b, &a)))
}
