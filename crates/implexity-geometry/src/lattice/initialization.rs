// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use crate::error::{GResult, model_err};
use crate::lattice::controls::Controls;
use crate::lattice::node::{LatticeSpec, node_controls};
use crate::node::Node;
use crate::numpy;
use crate::pyfmt::float_repr;

pub const INIT_SCHEMA: &str = "implexity-lattice-fraction-initialization/1";

fn finite_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}


pub fn initialize_solid_fraction(
    spec: &LatticeSpec,
    node: &Node,
    specification: &Value,
) -> GResult<(Controls, Value)> {
    let keys =
        ["schema", "target_solid_fraction", "scope", "coordinate_bounds", "tolerance", "max_iterations"];
    let ok = specification
        .as_object()
        .is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
        && specification["schema"] == INIT_SCHEMA
        && specification["scope"] == "whole_domain_geometry_grid";
    if !ok {
        return model_err("fraction initialization requires the complete versioned geometric contract");
    }
    let Some(ggrid) = spec.geometry_grid else {
        return model_err("fraction initialization requires an explicitly pinned geometry_grid");
    };
    let target = finite_number(&specification["target_solid_fraction"]);
    let tol = finite_number(&specification["tolerance"]);
    let (Some(target), Some(tol)) = (target, tol) else {
        return model_err("target must be in (0,1), tolerance finite and in (0,.01]");
    };
    if !(0.0 < target && target < 1.0) || !(0.0 < tol && tol <= 0.01) {
        return model_err("target must be in (0,1), tolerance finite and in (0,.01]");
    }
    let bounds: Option<Vec<f64>> =
        specification["coordinate_bounds"].as_array().and_then(|a| a.iter().map(finite_number).collect());
    let bounds = match bounds {
        Some(b) if b.len() == 2 && -6.0 <= b[0] && b[0] < b[1] && b[1] <= 6.0 => b,
        _ => return model_err("thickness coordinate bracket must lie within the declared [-6,6] range"),
    };
    let iterations = match &specification["max_iterations"] {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(0),
        _ => 0,
    };
    if !(1..=80).contains(&iterations) {
        return model_err("initialization max_iterations must be in [1,80]");
    }
    let original = node_controls(node, spec.control_grid)?;
    let comp0 = original.component(0);
    if comp0.iter().any(|v| *v != comp0[0]) {
        return model_err(
            "this seed initializer requires a uniform initial thickness field; existing local designs must not be flattened",
        );
    }
    let n = original.n();
    let with = |a: f64| {
        let mut c = original.clone();
        for v in &mut c.data[..n] {
            *v = a;
        }
        c
    };
    let measure = |a: f64| -> GResult<f64> { Ok(numpy::mean(&spec.geometry_fields(&with(a))?.rho)) };
    let (mut lo, mut hi) = (bounds[0], bounds[1]);
    let (flo, fhi) = (measure(lo)?, measure(hi)?);
    if !(flo.is_finite() && fhi.is_finite()) || !(flo <= target && target <= fhi) {
        return model_err(format!(
            "target {} is unattainable in authored thickness bracket [{}, {}]; no mask, fixed region or geometry range was changed",
            float_repr(target),
            float_repr(flo),
            float_repr(fhi)
        ));
    }
    let mut trace = Vec::new();
    let mut a = f64::NAN;
    let mut met = false;
    for _ in 0..iterations {
        a = f64::midpoint(lo, hi);
        let fraction = measure(a)?;
        trace.push(json!({"coordinate": a, "solid_fraction": fraction}));
        if !fraction.is_finite() {
            return model_err("nonfinite geometric seed quadrature");
        }
        if (fraction - target).abs() <= tol {
            met = true;
            break;
        }
        if fraction < target {
            lo = a;
        } else {
            hi = a;
        }
    }
    if !met {
        return model_err("fraction seed calibration did not meet its authored tolerance");
    }
    let result = with(a);
    let gf = spec.geometry_fields(&result)?;
    let cells = gf.rho.len();
    #[allow(clippy::cast_precision_loss)]
    let inv = 1.0 / cells as f64;
    let adj = gf.vjp(&vec![inv; cells], &vec![0.0; cells])?;
    let slope: f64 = adj[..n].iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let binary = gf.rho.iter().filter(|r| **r >= 0.5).count() as f64 / cells as f64;
    let report = json!({
        "schema": INIT_SCHEMA, "method": "bounded_uniform_thickness_bisection",
        "scope": specification["scope"], "target_solid_fraction": target,
        "achieved_solid_fraction": numpy::mean(&gf.rho), "binary_cell_fraction_at_half": binary,
        "geometry_grid": ggrid, "calibrated_thickness_coordinate": a,
        "d_fraction_d_thickness_coordinate": slope,
        "iterations": trace, "is_optimization_constraint": false,
        "fixed_regions_unchanged": true, "control_channels_changed": [0],
        "interpretation": "Geometric seed only. Canonical cell quadrature is not binary/continuum volume. No physics, channels, field projection or post-step repair.",
    });
    Ok((result, report))
}
