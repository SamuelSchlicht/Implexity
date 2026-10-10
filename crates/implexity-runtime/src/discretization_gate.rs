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
