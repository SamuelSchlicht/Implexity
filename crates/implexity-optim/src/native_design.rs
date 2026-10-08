// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_core::contracts::{CaeProvider, ProviderCapabilities, ProviderProblem};
use implexity_core::numeric_contract::real_scalar_f64;
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value};

use crate::design::{DesignLayout, NamedArrays};
use crate::provider_ops::{
    DesignOp, DesignSensitivities, check_operating_point, design_operations, legacy_evaluate,
};

#[must_use]
pub fn unique_names(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}


pub fn batch_sensitivities(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    responses: &[String],
    operating_point: usize,
) -> CaeResult<DesignSensitivities> {
    let layout = DesignLayout::from_values(design)?;
    let names = unique_names(responses);
    let ops = design_operations(provider);
    let raw = match ops {
        Some(ops) if ops.provides(DesignOp::SensitivitiesDesign) => {
            check_operating_point(provider, DesignOp::SensitivitiesDesign, operating_point)?;
            ops.sensitivities_design(problem, design, &names, operating_point)?
        }
        Some(ops) if ops.provides(DesignOp::SensitivityDesign) => {
            check_operating_point(provider, DesignOp::SensitivityDesign, operating_point)?;
            let mut out = DesignSensitivities::default();
            let mut per = Map::new();
            for n in &names {
                let row = ops.sensitivity_design(problem, design, n, operating_point)?;
                out.responses.insert(n.clone(), row.value);
                out.gradients.insert(n.clone(), row.gradients);
                per.insert(n.clone(), Value::Object(row.diagnostics));
            }
            out.diagnostics.insert("responses".into(), Value::Object(per));
            out.diagnostics.insert("shared_state".into(), Value::Bool(false));
            out
        }
        _ => return Err(CaeError::contract("provider has no named design sensitivity contract")),
    };
    checked_batch(&layout, &names, raw)
}


pub fn checked_batch(
    layout: &DesignLayout,
    names: &[String],
    raw: DesignSensitivities,
) -> CaeResult<DesignSensitivities> {
    let same = |keys: Vec<&String>| keys.len() == names.len() && names.iter().all(|n| keys.contains(&n));
    if !same(raw.responses.keys().collect()) {
        return Err(CaeError::contract("batched responses do not match the requested response names"));
    }
    if !same(raw.gradients.keys().collect()) {
        return Err(CaeError::contract("batched gradients do not match the requested response names"));
    }
    let mut out = DesignSensitivities { diagnostics: raw.diagnostics, ..DesignSensitivities::default() };
    for name in names {
        let value = real_scalar_f64(raw.responses[name], &format!("provider response {}", repr_str(name)))?;
        out.responses.insert(name.clone(), value);
        let packed = layout.repack(&raw.gradients[name], &format!("provider derivative {name}"))?;
        out.gradients.insert(name.clone(), packed);
    }
    Ok(out)
}


pub fn response_values(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    responses: &[String],
    operating_point: usize,
) -> CaeResult<(BTreeMap<String, f64>, Map<String, Value>)> {
    let evaluation =
        if let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::EvaluateDesign)) {
            check_operating_point(provider, DesignOp::EvaluateDesign, operating_point)?;
            ops.evaluate_design(problem, design, operating_point)?
        } else {
            match provider.capabilities()? {
                ProviderCapabilities::Legacy(l) if design.names() == [l.topology_coordinate.clone()] => {
                    let topology = design.get(&l.topology_coordinate).cloned().unwrap_or_default();
                    legacy_evaluate(provider, problem, &topology, operating_point)?
                }
                _ => {
                    return Err(CaeError::contract(
                        "provider has no evaluate_design for primal-only named-design trial",
                    ));
                }
            }
        };
    let mut checked = BTreeMap::new();
    for name in responses {
        let Some(value) = evaluation.responses.get(name) else {
            return Err(CaeError::contract(format!(
                "provider evaluation omitted response {}",
                repr_str(name)
            )));
        };
        checked.insert(
            name.clone(),
            real_scalar_f64(*value, &format!("provider evaluation response {}", repr_str(name)))?,
        );
    }
    Ok((checked, evaluation.diagnostics))
}
