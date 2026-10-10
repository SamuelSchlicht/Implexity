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


use serde_json::Value;

use implexity_core::CaeError;

pub const SCHEMA: &str = "implexity-rectangular-pressure-aperture/1";

pub type ApertureBounds = ([usize; 2], [usize; 2], [usize; 2]);

fn contract<T>(message: &str) -> Result<T, CaeError> {
    Err(CaeError::contract(message))
}


pub fn aperture_bounds(boundary: &Value, grid: [usize; 3]) -> Result<ApertureBounds, CaeError> {
    let Some(b) = boundary.as_object() else {
        return contract("aperture owner must declare a valid axis and side");
    };
    let axis = match b.get("axis") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_u64().filter(|a| *a < 3),
        _ => None,
    };
    let side_ok = matches!(b.get("side").and_then(Value::as_str), Some("lo" | "hi"));
    let (Some(axis), true) = (axis, side_ok) else {
        return contract("aperture owner must declare a valid axis and side");
    };
    let axis = usize::try_from(axis).unwrap_or(0);
    let axes: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
    let axes = [axes[0], axes[1]];
    let opening = b.get("opening").filter(|v| !v.is_null());
    let Some(opening) = opening else {
        return Ok((axes, [0, 0], [grid[axes[0]], grid[axes[1]]]));
    };
    let keys = ["schema", "id", "lower_fraction", "upper_fraction", "closed_remainder", "provenance"];
    let complete =
        opening.as_object().is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
            && opening.get("schema").and_then(Value::as_str) == Some(SCHEMA);
    if !complete {
        return contract("pressure opening requires the complete versioned rectangular-aperture contract");
    }
    if b.get("momentum").and_then(Value::as_str) != Some("pressure")
        || b.get("thermal").and_then(Value::as_str) != Some("insulated")
    {
        return contract(
            "rectangular apertures currently require pressure momentum and insulated thermal boundary",
        );
    }
    if opening.get("closed_remainder").and_then(Value::as_str) != Some("stationary_no_slip_adiabatic") {
        return contract("aperture remainder must be explicitly stationary_no_slip_adiabatic");
    }
    for k in ["id", "provenance"] {
        if opening.get(k).and_then(Value::as_str).is_none_or(|s| s.trim().is_empty()) {
            return contract("pressure opening id and provenance required");
        }
    }
    let mut edges = [[0usize; 2]; 2];
    for (slot, key) in edges.iter_mut().zip(["lower_fraction", "upper_fraction"]) {
        let values = opening.get(key).and_then(Value::as_array);
        let fractions: Option<Vec<f64>> = values.filter(|v| v.len() == 2).and_then(|v| {
            v.iter()
                .map(|x| match x {
                    Value::Number(n) => n.as_f64().filter(|f| f.is_finite() && (0.0..=1.0).contains(f)),
                    _ => None,
                })
                .collect()
        });
        let Some(fractions) = fractions else {
            return contract("aperture fractions must be two finite real values in [0,1]");
        };
        for (k, f) in fractions.iter().enumerate() {
            let q = f * grid[axes[k]] as f64;
            let r = q.round_ties_even();
            if (q - r).abs() > 1e-10 {
                return contract(
                    "pressure aperture edges must align with the analysis grid; refine the grid explicitly, never round the opening",
                );
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

            {
                slot[k] = r as usize;
            }
        }
    }
    if edges[0].iter().zip(&edges[1]).any(|(lo, hi)| lo >= hi) {
        return contract("pressure aperture must have strictly positive extent on both tangential axes");
    }
    Ok((axes, edges[0], edges[1]))
}


pub fn pressure_cell(boundary: &Value, grid: [usize; 3], cell: [usize; 3]) -> Result<bool, CaeError> {
    if boundary.get("momentum").and_then(Value::as_str) != Some("pressure") {
        return Ok(false);
    }
    let (axes, lower, upper) = aperture_bounds(boundary, grid)?;
    Ok((0..2).all(|k| lower[k] <= cell[axes[k]] && cell[axes[k]] < upper[k]))
}


pub fn closed_edge_fraction(
    boundary: &Value,
    grid: [usize; 3],
    edge: [i64; 3],
    transverse_axis: usize,
) -> Result<f64, CaeError> {
    let axis =
        boundary.get("axis").and_then(Value::as_u64).and_then(|a| usize::try_from(a).ok()).unwrap_or(0);
    let lo = boundary.get("side").and_then(Value::as_str) == Some("lo");
    let mut cells = Vec::new();
    for offset in [-1i64, 0] {
        let mut cell = edge;
        cell[transverse_axis] += offset;
        cell[axis] = if lo { 0 } else { i64::try_from(grid[axis]).unwrap_or(0) - 1 };
        if (0..3).all(|a| cell[a] >= 0 && cell[a] < i64::try_from(grid[a]).unwrap_or(0)) {
            cells.push(cell.map(|c| usize::try_from(c).unwrap_or(0)));
        }
    }
    if cells.is_empty() {
        return contract("empty boundary-edge support");
    }
    let mut closed = 0usize;
    for c in &cells {
        if !pressure_cell(boundary, grid, *c)? {
            closed += 1;
        }
    }
    Ok(closed as f64 / cells.len() as f64)
}
