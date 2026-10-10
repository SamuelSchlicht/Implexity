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

fn budgets(plan: &Value) -> Option<Vec<i64>> {
    plan.get("optimization")?.get("schedule")?.as_array()?.iter()
        .map(|r| r.get("iterations")?.as_i64().filter(|n| *n > 0)).collect()
}

pub(super) fn extend_terminal_stage(plan: &mut Value, total: i64) -> Result<Option<(usize, i64, i64)>, String> {
    let Some(stages) = plan.get("optimization").and_then(|o| o.get("schedule")) else { return Ok(None); };
    if stages.is_null() || stages.as_array().is_some_and(Vec::is_empty) { return Ok(None); }
    let old = budgets(plan).ok_or("Explicit schedule requires positive integer stage budgets")?;
    let sum = old.iter().try_fold(0_i64, |a, b| a.checked_add(*b)).ok_or("Schedule budget overflow")?;
    if total < sum { return Err("Cannot shrink an explicit schedule through an iteration override".into()); }
    if total == sum { return Ok(None); }
    if plan["optimization"]["settings"]["iterations"].as_i64() != Some(sum) {
        return Err("Explicit schedule and recorded iteration budget disagree".into());
    }
    let index = old.len() - 1;
    let after = old[index].checked_add(total - sum).ok_or("Schedule budget overflow")?;
    plan["optimization"]["schedule"][index]["iterations"] = Value::from(after);
    Ok(Some((index, old[index], after)))
}

pub(super) fn resume_plan_equivalent(previous: &Value, next: &Value) -> bool {
    if previous.get("authoring") != next.get("authoring") { return false; }
    let (Some(a), Some(b)) = (previous.get("optimization"), next.get("optimization")) else { return false; };
    let (mut a, mut b) = (a.clone(), b.clone());
    for o in [&mut a, &mut b] {
        if let Some(s) = o.get_mut("settings").and_then(Value::as_object_mut) { s.remove("iterations"); }
    }
    let explicit = |p: &Value| p["optimization"]["schedule"].as_array().is_some_and(|s| !s.is_empty());
    if explicit(previous) || explicit(next) {
        for p in [previous, next] {
            let Some(values) = budgets(p) else { return false; };
            let sum = values.iter().try_fold(0_i64, |a, b| a.checked_add(*b));
            if values.is_empty() || sum.is_none() || p["optimization"]["settings"]["iterations"].as_i64() != sum { return false; }
        }
    }
    if a == b { return true; }
    let (Some(old), Some(new)) = (budgets(previous), budgets(next)) else { return false; };
    if old.is_empty() || old.len() != new.len() || old[..old.len()-1] != new[..new.len()-1]
        || new[new.len()-1] <= old[old.len()-1] { return false; }
    let sum = |v: &[i64]| v.iter().try_fold(0_i64, |a, b| a.checked_add(*b));
    if previous["optimization"]["settings"]["iterations"].as_i64() != sum(&old)
        || next["optimization"]["settings"]["iterations"].as_i64() != sum(&new) { return false; }
    let i = old.len() - 1;
    for o in [&mut a, &mut b] {
        let Some(stage) = o.get_mut("schedule").and_then(Value::as_array_mut)
            .and_then(|rows| rows.get_mut(i)).and_then(Value::as_object_mut) else { return false; };
        stage.remove("iterations");
    }
    a == b
}
