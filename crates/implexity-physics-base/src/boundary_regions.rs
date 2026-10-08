// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
