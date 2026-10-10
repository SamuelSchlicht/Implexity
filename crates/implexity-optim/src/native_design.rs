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
