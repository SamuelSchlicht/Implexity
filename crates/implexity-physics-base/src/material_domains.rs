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


use serde_json::{Value, json};

use implexity_core::CaeError;

pub const SCHEMA: &str = "implexity-material-evaluation-domain/1";

fn number(value: &Value, label: &str) -> Result<f64, CaeError> {
    match value {
        Value::Number(n) => n
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| CaeError::contract(format!("{label} must be a finite real scalar"))),
        _ => Err(CaeError::contract(format!("{label} must be a finite real scalar"))),
    }
}

fn finite(value: f64, label: &str) -> Result<f64, CaeError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CaeError::contract(format!("{label} must be a finite real scalar")))
    }
}


pub fn evaluation_interval(
    policy: Option<&Value>,
    variable: &str,
    evidence_lower: f64,
    evidence_upper: f64,
    supported_continuations: &[&str],
) -> Result<(f64, f64), CaeError> {
    let lo = finite(evidence_lower, "material evidence lower")?;
    let hi = finite(evidence_upper, "material evidence upper")?;
    if lo >= hi {
        return Err(CaeError::contract("material evidence interval must have positive width"));
    }
    let Some(policy) = policy.filter(|p| !p.is_null()) else { return Ok((lo, hi)) };
    let keys = ["continuation", "lower", "provenance", "schema", "upper", "variable"];
    let Some(p) =
        policy.as_object().filter(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
    else {
        return Err(CaeError::contract(format!(
            "material evaluation domain requires [{}]",
            keys.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ")
        )));
    };
    if p["schema"].as_str() != Some(SCHEMA) || p["variable"].as_str() != Some(variable) {
        return Err(CaeError::contract("unsupported material evaluation domain schema or variable"));
    }
    if !p["continuation"].as_str().is_some_and(|c| supported_continuations.contains(&c)) {
        return Err(CaeError::contract("material provider does not support this continuation"));
    }
    if p["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return Err(CaeError::contract("material evaluation continuation provenance required"));
    }
    let lower = number(&p["lower"], "material evaluation lower")?;
    let upper = number(&p["upper"], "material evaluation upper")?;
    if !(lower <= lo && lo < hi && hi <= upper) {
        return Err(CaeError::contract(
            "evaluation envelope must contain the original material evidence interval",
        ));
    }
    Ok((lower, upper))
}


pub fn interval_status(
    values: &[f64],
    evidence: (f64, f64),
    evaluation: (f64, f64),
    support: Option<&[bool]>,
) -> Result<Value, CaeError> {
    if let Some(s) = support
        && s.len() != values.len()
    {
        return Err(CaeError::contract("material domain support must be a matching boolean array"));
    }
    let el = finite(evidence.0, "material evidence lower")?;
    let eu = finite(evidence.1, "material evidence upper")?;
    let lo = finite(evaluation.0, "material evaluation lower")?;
    let hi = finite(evaluation.1, "material evaluation upper")?;
    if !(lo <= el && el < eu && eu <= hi) {
        return Err(CaeError::contract("invalid material evidence/evaluation interval ordering"));
    }
    let checked = |i: usize| support.is_none_or(|s| s[i]);
    let (mut count, mut nonfinite, mut out_auth, mut out_eval) = (0usize, 0usize, 0usize, 0usize);
    let (mut all_finite, mut eval_ok, mut orig_ok) = (true, true, true);
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut any_selected = false;
    for (i, &x) in values.iter().enumerate() {
        let fin = x.is_finite();
        if !fin {
            nonfinite += 1;
            all_finite = false;
        }
        let original = fin && x >= el && x <= eu;
        let evaluable = fin && x >= lo && x <= hi;
        if checked(i) {
            count += 1;
            if !original {
                out_auth += 1;
                orig_ok = false;
            }
            if !evaluable {
                out_eval += 1;
                eval_ok = false;
            }
            if fin {
                any_selected = true;
                min = min.min(x);
                max = max.max(x);
            }
        }
    }
    Ok(json!({
        "schema": "implexity-material-domain-status/1",
        "evaluation_ok": all_finite && eval_ok,
        "within_authored_material_domain": all_finite && orig_ok,
        "checked_count": count,
        "nonfinite_count": nonfinite,
        "outside_authored_domain_count": out_auth,
        "outside_evaluation_domain_count": out_eval,
        "authored_lower": el, "authored_upper": eu,
        "evaluation_lower": lo, "evaluation_upper": hi,
        "checked_finite_minimum": if any_selected { json!(min) } else { Value::Null },
        "checked_finite_maximum": if any_selected { json!(max) } else { Value::Null },
        "calibration_verified": false,
        "physical_qualification": false,
        "engineering_acceptance_assessed": false,
    }))
}
