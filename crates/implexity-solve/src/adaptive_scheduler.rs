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



use implexity_core::error::{CaeError, CaeResult};
use implexity_core::py_repr::repr_str;
use serde_json::{Map, Value, json};

use crate::pyfmt::fmt_g;

pub const ACTIONS: [&str; 3] = ["recommend", "auto_authorized", "manual_only"];

pub type MultiphysicsAssessment<'a> = &'a dyn Fn(Option<&Value>, Option<&Value>, Option<&Value>) -> Value;

#[derive(Clone, Debug, PartialEq)]
pub struct AdaptivePolicy {
    pub action: String,
    pub min_iterations: usize,
    pub objective_rel_tol: f64,
    pub gradient_rel_tol: f64,
    pub topology_gray_tol: f64,
    pub adjoint_residual_max: f64,
    pub require_regime_valid: bool,
    pub multiphysics_limits: Option<Value>,
}

impl Default for AdaptivePolicy {
    fn default() -> Self {
        Self {
            action: "recommend".into(),
            min_iterations: 5,
            objective_rel_tol: 1e-3,
            gradient_rel_tol: 2e-2,
            topology_gray_tol: 0.12,
            adjoint_residual_max: 1e-6,
            require_regime_valid: true,
            multiphysics_limits: None,
        }
    }
}

fn pick<'a>(r: &'a Map<String, Value>, a: &str, b: &str) -> Option<&'a Value> {
    r.get(a).or_else(|| r.get(b))
}

fn number(v: Option<&Value>, default: f64, name: &str) -> CaeResult<f64> {
    match v {
        None => Ok(default),
        Some(Value::Number(n)) => {
            n.as_f64().ok_or_else(|| CaeError::contract(format!("{name} must be numeric")))
        }
        Some(Value::Bool(b)) => Ok(if *b { 1.0 } else { 0.0 }),
        Some(Value::String(s)) => {
            s.trim().parse::<f64>().map_err(|_| CaeError::contract(format!("{name} must be numeric")))
        }
        Some(_) => Err(CaeError::contract(format!("{name} must be numeric"))),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

impl AdaptivePolicy {


    pub fn from_dict(raw: Option<&Value>) -> CaeResult<Self> {
        let r = raw.and_then(Value::as_object).cloned().unwrap_or_default();
        let action = match r.get("action") {
            Some(v) if truthy(v) => v.as_str().map_or_else(|| v.to_string(), str::to_string),
            _ => "recommend".to_string(),
        };
        if !ACTIONS.contains(&action.as_str()) {
            return Err(CaeError::contract(format!("unsupported adaptive action {}", repr_str(&action))));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_iterations = number(pick(&r, "min_iterations", "minIterations"), 5.0, "min_iterations")?
            .trunc()
            .max(1.0) as usize;
        Ok(Self {
            action,
            min_iterations,
            objective_rel_tol: number(
                pick(&r, "objective_rel_tol", "objectiveRelTol"),
                1e-3,
                "objective_rel_tol",
            )?,
            gradient_rel_tol: number(
                pick(&r, "gradient_rel_tol", "gradientRelTol"),
                2e-2,
                "gradient_rel_tol",
            )?,
            topology_gray_tol: number(
                pick(&r, "topology_gray_tol", "topologyGrayTol"),
                0.12,
                "topology_gray_tol",
            )?,
            adjoint_residual_max: number(
                pick(&r, "adjoint_residual_max", "adjointResidualMax"),
                1e-6,
                "adjoint_residual_max",
            )?,
            require_regime_valid: pick(&r, "require_regime_valid", "requireRegimeValid").is_none_or(truthy),
            multiphysics_limits: [r.get("multiphysics_limits"), r.get("multiphysicsLimits")]
                .into_iter()
                .flatten()
                .find(|v| truthy(v))
                .cloned(),
        })
    }
}

#[must_use]
pub fn topology_grayness(x: &[f64]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = x.len() as f64;
    x.iter().map(|a| 4.0 * a * (1.0 - a)).sum::<f64>() / n
}

fn objective(row: &Value) -> CaeResult<f64> {
    let v = row.get("L").or_else(|| row.get("objective"));
    number(v, 0.0, "objective")
}



pub fn assess(
    history: &[Value],
    diagnostics: &Value,
    topology: &[f64],
    policy: &AdaptivePolicy,
    problem: Option<&Value>,
    assess_multiphysics: MultiphysicsAssessment<'_>,
) -> CaeResult<Value> {
    let mut reasons: Vec<String> = Vec::new();
    let mut ready = true;
    if history.len() < policy.min_iterations {
        ready = false;
        reasons.push("minimum iteration count not reached".into());
    }
    if history.len() >= 2 {
        let (a, b) = (&history[history.len() - 2], &history[history.len() - 1]);
        let (old, new) = (objective(a)?, objective(b)?);
        let rel = (new - old).abs() / 1f64.max(old.abs()).max(new.abs());
        if rel > policy.objective_rel_tol {
            ready = false;
            reasons.push(format!("objective still changing ({})", fmt_g(rel, 3)));
        }
        let g0 = number(a.get("gradient_norm"), 0.0, "gradient_norm")?;
        let g1 = number(b.get("gradient_norm"), 0.0, "gradient_norm")?;
        let grel = (g1 - g0).abs() / 1f64.max(g0.abs()).max(g1.abs());
        if grel > policy.gradient_rel_tol {
            ready = false;
            reasons.push(format!("gradient still changing ({})", fmt_g(grel, 3)));
        }
    }
    let gray = topology_grayness(topology);
    if gray > policy.topology_gray_tol {
        ready = false;
        reasons.push(format!("topology still diffuse ({})", fmt_g(gray, 3)));
    }
    let ar = diagnostics
        .get("adjointResidual")
        .or_else(|| diagnostics.get("adjoint_residual"))
        .filter(|v| !v.is_null());
    if let Some(ar) = ar {
        let ar = number(Some(ar), 0.0, "adjoint residual")?;
        if ar > policy.adjoint_residual_max {
            ready = false;
            reasons.push(format!("adjoint residual {} exceeds limit", fmt_g(ar, 3)));
        }
    }
    let rv = diagnostics.get("regimeValid").or_else(|| diagnostics.get("regime_valid")).is_none_or(truthy);
    if policy.require_regime_valid && !rv {
        ready = false;
        reasons.push("current provider validity regime is not satisfied".into());
    }
    let mp = assess_multiphysics(Some(diagnostics), problem, policy.multiphysics_limits.as_ref());
    if !mp.get("ok").is_some_and(truthy) {
        ready = false;
        if let Some(issues) = mp.get("issues").and_then(Value::as_array) {
            reasons.extend(issues.iter().map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_string)));
        }
    }
    Ok(json!({"ready": ready, "action": policy.action, "reasons": reasons, "grayness": gray,
        "multiphysics": mp, "authorized": ready && policy.action == "auto_authorized"}))
}

