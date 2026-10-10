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

use implexity_ad::Scalar;
use implexity_core::CaeError;

use crate::array::Tensor;

pub const SCHEMA: &str = "implexity-material-history-numerical-extension/1";
pub const METHOD: &str = "endpoint_c1_bounded_kinetic_argument";
pub const CONTRACT: &str = "separable_endpoint_potential_inventory/1";
pub const STATE_SEMANTICS: &str = "unweighted_potential_inventory_no_physical_claim_at_zero_support";
pub const ENERGY_SEMANTICS: &str = "endpoint_mixture_then_physical_solid_fraction_once";

pub trait EndpointNumericalExtension {
    fn numerical_extension_contract(&self) -> Option<&str>;
    fn declares_endpoint_continuation(&self) -> bool;

    fn validate_numerical_extension(&self, settings: &Value, context: &Value) -> Result<(), CaeError>;
}


pub fn validate_policy(
    policy: &Value,
    context: &Value,
    adapter: &dyn EndpointNumericalExtension,
    settings: &Value,
) -> Result<Value, CaeError> {
    let required = ["energy_semantics", "method", "provenance", "schema", "state_semantics"];
    let complete = policy
        .as_object()
        .is_some_and(|p| p.len() == required.len() && required.iter().all(|k| p.contains_key(*k)));
    if !complete {
        return Err(CaeError::contract(
            "material-history numerical extension requires an explicit complete policy",
        ));
    }
    let expected = [
        ("schema", SCHEMA),
        ("method", METHOD),
        ("state_semantics", STATE_SEMANTICS),
        ("energy_semantics", ENERGY_SEMANTICS),
    ];
    let provenance_ok = policy["provenance"].as_str().is_some_and(|s| !s.trim().is_empty());
    if expected.iter().any(|(k, v)| policy[*k].as_str() != Some(v)) || !provenance_ok {
        return Err(CaeError::contract(
            "unsupported material-history numerical extension or missing provenance",
        ));
    }
    if context.get("inactive_phase_numerical_material").is_none_or(Value::is_null) {
        return Err(CaeError::contract("history extension requires phase-aware physical material validity"));
    }
    if context.get("viscoelasticity").is_some_and(|v| !v.is_null()) {
        return Err(CaeError::contract(
            "viscoelastic storage/eigenstrain continuation is not supplied by this policy",
        ));
    }
    if adapter.numerical_extension_contract() != Some(CONTRACT) || !adapter.declares_endpoint_continuation() {
        return Err(CaeError::contract(
            "material history has not declared separable endpoint numerical continuation",
        ));
    }
    adapter.validate_numerical_extension(settings, context)?;
    Ok(policy.clone())
}

pub fn endpoint_temperature<S: Scalar>(t: S, lo: f64, hi: f64) -> S {
    let v = t.value();
    if v < lo {
        let d = -t + lo;
        -(d / (d + lo / 2.0)) * (lo / 2.0) + lo
    } else if v > hi {
        let d = t - hi;
        (d / (d + hi / 2.0)) * (hi / 2.0) + hi
    } else {
        t
    }
}

#[must_use]
pub fn endpoint_temperatures<S: Scalar>(temperature: &Tensor<S>, materials: &[(f64, f64)]) -> Vec<Tensor<S>> {
    materials.iter().map(|&(lo, hi)| temperature.map(|t| endpoint_temperature(t, lo, hi))).collect()
}


pub fn validate_kinetic_range(
    settings: &Value,
    context: &Value,
    rate_key: &str,
    reference_key: &str,
    source_key: &str,
    source_factors: &[f64],
) -> Result<(), CaeError> {
    let bad = |m: &str| CaeError::contract(m);
    let source = Tensor::from_json(&settings[source_key]).map_err(|e| bad(&e.to_string()))?;
    let times: Vec<f64> = context["times_s"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let dt: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let endmembers = settings["endmembers"].as_array().cloned().unwrap_or_default();
    let materials = context["materials"].as_array().cloned().unwrap_or_default();
    let shape = source.shape().to_vec();
    for (i, ((e, m), factor)) in endmembers.iter().zip(&materials).zip(source_factors).enumerate() {
        let num = |v: &Value, k: &str| v[k].as_f64().unwrap_or(f64::NAN);
        let mut rates = Vec::new();
        for t in [num(m, "T_min") / 2.0, 1.5 * num(m, "T_max")] {
            let exponent =
                -num(e, "activation_J_mol") / 8.314_462_618_153_24 * (1.0 / t - 1.0 / num(e, reference_key));
            if !exponent.is_finite() || exponent.abs() > 600.0 {
                return Err(bad("kinetics exceed finite numerical-extension exponential range"));
            }
            rates.push(num(e, rate_key) * exponent.exp());
        }
        let max_rate = rates.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut finite = rates.iter().all(Scalar::is_finite);
        if shape.len() == 3 && i < shape[2] {
            for (n, step) in dt.iter().enumerate() {
                for cell in 0..shape[1] {
                    let s = source.at(((n + 1) * shape[1] + cell) * shape[2] + i);
                    finite &= (1.0 + step * (s * factor + max_rate)).is_finite();
                }
            }
        }
        if !finite {
            return Err(bad("kinetics/time increments overflow the numerical-extension range"));
        }
    }
    Ok(())
}
