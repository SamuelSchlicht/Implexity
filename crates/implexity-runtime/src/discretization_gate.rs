// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_optim::numeric::{float_value, format_g};
use serde_json::{Map, Value, json};

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn truthy_object<'a>(problem: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    problem.get(key).and_then(Value::as_object)
}

#[must_use]
pub fn assess_feature_resolution(problem: &Value) -> Value {
    let empty = Map::new();
    let domain = truthy_object(problem, "domain").unwrap_or(&empty);
    let design = truthy_object(problem, "design").unwrap_or(&empty);
    let verification = truthy_object(problem, "verification").unwrap_or(&empty);
    let dims = domain.get("dimensionsM").and_then(Value::as_array).cloned().unwrap_or_default();
    let cells = domain.get("cells").and_then(Value::as_array).cloned().unwrap_or_default();
    if dims.len() != 3 || cells.len() != 3 {
        return json!({"ok": true, "available": false, "issues": [], "metrics": {}});
    }
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let spacing: Vec<f64> = (0..3)
        .map(|i| {
            let d = number(&dims[i]).unwrap_or(f64::NAN);
            let c = number(&cells[i]).map_or(1, |c| c.trunc() as i64).max(1);
            d / c as f64
        })
        .collect();
    let h = spacing.iter().copied().fold(f64::NEG_INFINITY, |a, b| if b > a || b.is_nan() { b } else { a });
    let mut features = Vec::new();
    for (key, label) in [
        ("minimumWallThicknessM", "minimum wall thickness"),
        ("minimumChannelDiameterM", "minimum channel diameter"),
    ] {
        if let Some(v) = design.get(key).filter(|v| !v.is_null())
            && let Some(f) = number(v)
            && f.is_finite()
            && f > 0.0
        {
            features.push((key, label, f));
        }
    }
    let minimum = verification.get("minimumCellsAcrossFeature").and_then(number).unwrap_or(4.0);
    let mut issues = Vec::new();
    let mut feature_metrics = Map::new();
    for (key, label, f) in &features {
        let cells_across = f / h.max(1e-30);
        feature_metrics.insert(
            (*key).into(),
            json!({"sizeM": float_value(*f), "cellsAcross": float_value(cells_across)}),
        );
        if cells_across < minimum {
            issues.push(Value::String(format!(
                "{label} is represented by only {} cells across the coarsest grid direction; {} are required",
                format_g(cells_across, 3),
                format_g(minimum, 3)
            )));
        }
    }
    let enforce = verification.get("enforceFeatureResolution").is_some_and(implexity_core::pyobj::truthy);
    json!({
        "ok": issues.is_empty() || !enforce,
        "available": !features.is_empty(),
        "issues": issues,
        "metrics": {"maximumCellSizeM": float_value(h), "minimumCellsAcrossFeature": float_value(minimum), "features": feature_metrics},
        "enforced": enforce,
    })
}
