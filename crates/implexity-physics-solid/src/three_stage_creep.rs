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

use implexity_ad::Scalar;
use implexity_core::CaeError;
use serde_json::Value;

pub const SCHEMA: &str = "implexity-strain-hardening-creep/1";

pub fn coefficients(settings: &Value) -> Result<[f64; 11], CaeError> {
    let fail = || {
        CaeError::contract("strain-hardening creep requires finite A[5], B[5], n and provenance")
    };
    let object = settings.as_object().ok_or_else(fail)?;
    if object.len() != 6
        || settings["schema"] != SCHEMA
        || settings["provenance"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err(fail());
    }
    let a = settings["A"]
        .as_array()
        .filter(|v| v.len() == 5)
        .ok_or_else(fail)?;
    let b = settings["B"]
        .as_array()
        .filter(|v| v.len() == 5)
        .ok_or_else(fail)?;
    let mut out = [0.; 11];
    for i in 0..5 {
        out[i] = a[i].as_f64().filter(|x| x.is_finite()).ok_or_else(fail)?;
        out[i + 5] = b[i].as_f64().filter(|x| x.is_finite()).ok_or_else(fail)?;
    }
    out[10] = settings["n"]
        .as_f64()
        .filter(|x| x.is_finite() && *x >= 1.)
        .ok_or_else(fail)?;
    if out[0] <= 0. || out[1..5].iter().any(|x| *x < 0.) || out[1] < out[3] {
        return Err(fail());
    }
    let domain = validity(settings)?;
    for q in [domain[0], domain[1]] {
        let k: [f64; 5] = std::array::from_fn(|i| out[i] + out[i + 5] * q);
        if k.iter().any(|v| !v.is_finite())
            || k[0] <= 0.
            || k[1..].iter().any(|v| *v < 0.)
            || k[0] + k[1] - k[3] <= 0.
            || k[1] < k[3]
        {
            return Err(fail());
        }
    }
    Ok(out)
}

pub fn drag<S: Scalar>(q: S, accumulated: S, c: &[S; 11]) -> S {
    let k: [S; 5] = std::array::from_fn(|i| c[i] + c[i + 5] * q);
    k[0] + k[1] * (S::one() - (-k[2] * accumulated).exp())
        - k[3] * (S::one() - (-k[4] * accumulated).exp())
}

pub fn rate<S: Scalar>(q: S, accumulated: S, c: &[S; 11]) -> S {
    (q / drag(q, accumulated, c)).pow(c[10])
}

pub fn validity(settings: &Value) -> Result<[f64; 5], CaeError> {
    let domain = settings["validity"].as_array().filter(|a|a.len()==5).ok_or_else(|| CaeError::contract("creep validity requires minimum stress, maximum stress, maximum accumulated strain, minimum temperature and maximum temperature in SI units"))?;
    let mut out = [0.; 5];
    for i in 0..5 {
        out[i] = domain[i]
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| CaeError::contract("creep validity must be finite"))?;
    }
    if out[0] < 0. || out[1] <= out[0] || out[2] <= 0. || out[3] <= 0. || out[4] < out[3] {
        return Err(CaeError::contract("invalid creep calibration interval"));
    }
    Ok(out)
}

pub fn authoring_contract() -> serde_json::Map<String, Value> {
    serde_json::json!({"schema":"implexity-component-authoring/1","selection":"solid.creep_parameters","required_settings":["schema","provenance","A","B","n","validity"],"problem_field":"creep_parameters","endpoint_count":2,"coefficient_units":{"A":["Pa","Pa","1","Pa","1"],"B":["1","1","1/Pa","1","1/Pa"],"n":"1"},"validity_units":["Pa","Pa","1","K","K"],"rate_unit":"1/s","calibrated_material_data_supplied":false}).as_object().unwrap().clone()
}
