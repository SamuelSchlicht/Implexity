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

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("derivative request rejected:\n  {}", problems.join("\n  "))]
pub struct DerivativeError {
    pub problems: Vec<String>,
}

fn reject(p: &str) -> DerivativeError {
    DerivativeError { problems: vec![p.to_string()] }
}

#[must_use]
pub fn catalogue() -> Value {
    json!({
        "kind": "implicit_derivative_operators", "schema": "implexity-derivatives/1",
        "operators": [
            {"id": "jacobian", "input": "selected design parameters", "output": "selected engineering responses",
             "action": "full response Jacobian for small/medium selected design sets"},
            {"id": "jvp", "input": "design-space tangent", "output": "response-space tangent",
             "action": "matrix-free forward directional derivative J v"},
            {"id": "vjp", "input": "response-space cotangent", "output": "design-space cotangent",
             "action": "matrix-free reverse pullback J^T w"},
        ],
        "note": "All operators act on the same coupled response function used by sensitivity and optimisation.",
    })
}


pub fn normalise(req: &Value) -> Result<Value, DerivativeError> {
    let Some(m) = req.as_object() else { return Err(reject("request must be an object")) };
    let op = match m.get("operator") {
        None | Some(Value::Null) => "jacobian".to_string(),
        Some(Value::String(s)) if s.is_empty() => "jacobian".to_string(),
        Some(Value::Bool(false)) => "jacobian".to_string(),
        Some(Value::String(s)) => s.to_lowercase(),
        Some(other) => crate::document::py_str(other).to_lowercase(),
    };
    if !["jacobian", "jvp", "vjp"].contains(&op.as_str()) {
        return Err(reject("operator must be jacobian, jvp or vjp"));
    }
    let mut out = m.clone();
    out.insert("operator".into(), json!(op));
    if op == "jvp" && !m.get("tangent").is_some_and(Value::is_object) {
        return Err(reject("jvp needs tangent={parameter_ref: scalar-or-array, ...}"));
    }
    if op == "vjp" && !m.get("cotangent").is_some_and(Value::is_object) {
        return Err(reject("vjp needs cotangent={response_name: weight, ...}"));
    }
    Ok(Value::Object(out))
}
