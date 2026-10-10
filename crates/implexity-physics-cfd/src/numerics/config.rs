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


use serde_json::{Map, Value};

use implexity_core::py_repr::{PyValue, repr_str};

use crate::error::{CfdError, CfdResult};
use crate::numerics::preconditioner::FactorizationStrategy;
use crate::numerics::solver::{KrylovMethod, SaddlePointSolverConfig};

const REQUIRED: [&str; 7] = [
    "maximumIterations",
    "method",
    "pressureSchur",
    "relativeTolerance",
    "absoluteTolerance",
    "restart",
    "velocityBlock",
];

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => PyValue::from_json(other).repr(),
    }
}

fn py_float(v: &Value) -> CfdResult<f64> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        Value::String(s) => {
            let t = s.trim().to_ascii_lowercase().replace('_', "");
            let parsed = match t.as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Some(f64::NAN),
                other => other.parse::<f64>().ok(),
            };
            parsed
                .ok_or_else(|| CfdError::Input(format!("could not convert string to float: {}", repr_str(s))))
        }
        Value::Null => {
            Err(CfdError::Type("float() argument must be a string or a real number, not 'NoneType'".into()))
        }
        Value::Array(_) => {
            Err(CfdError::Type("float() argument must be a string or a real number, not 'list'".into()))
        }
        Value::Object(_) => {
            Err(CfdError::Type("float() argument must be a string or a real number, not 'dict'".into()))
        }
    }
}

fn strict_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) if !n.is_f64() => n.as_i64(),
        _ => None,
    }
}

fn get_or<'a>(m: &'a Map<String, Value>, k: &str, default: &'a Value) -> &'a Value {
    m.get(k).unwrap_or(default)
}


pub fn solver_config_from_problem(problem: &Value) -> CfdResult<SaddlePointSolverConfig> {
    let Some(node) = problem.as_object().and_then(|p| p.get("linearSolver")) else {
        return Err(CfdError::Contract(
            "CFD problem is missing the explicit linearSolver declaration.".into(),
        ));
    };
    let Some(node) = node.as_object() else {
        return Err(CfdError::Contract("linearSolver must be an object.".into()));
    };
    let mut missing: Vec<&str> = REQUIRED.iter().copied().filter(|k| !node.contains_key(*k)).collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        return Err(CfdError::Contract(format!(
            "linearSolver is missing required fields: {}",
            missing.join(", ")
        )));
    }
    let (Some(velocity), Some(schur)) =
        (node["velocityBlock"].as_object(), node["pressureSchur"].as_object())
    else {
        return Err(CfdError::Contract("velocityBlock and pressureSchur must be objects.".into()));
    };
    if node.get("transposeMode").and_then(Value::as_str) != Some("exact_preconditioner_transpose") {
        return Err(CfdError::Contract(
            "transposeMode must be 'exact_preconditioner_transpose' for the discrete-adjoint route.".into(),
        ));
    }
    let method = py_str(&node["method"]);
    let rtol = py_float(&node["relativeTolerance"])?;
    let atol = py_float(&node["absoluteTolerance"])?;
    let restart = &node["restart"];
    let maxiter = &node["maximumIterations"];
    let key_err = |k: &str| CfdError::Key(repr_str(k));
    let v_strategy = py_str(velocity.get("strategy").ok_or_else(|| key_err("strategy"))?);
    let d_drop = Value::from(1.0e-4);
    let d_fill = Value::from(12.0);
    let d_reg = Value::from(1.0e-12);
    let v_drop = py_float(get_or(velocity, "dropTolerance", &d_drop))?;
    let v_fill = py_float(get_or(velocity, "fillFactor", &d_fill))?;
    let s_strategy = py_str(schur.get("strategy").ok_or_else(|| key_err("strategy"))?);
    let s_drop = py_float(get_or(schur, "dropTolerance", &d_drop))?;
    let s_fill = py_float(get_or(schur, "fillFactor", &d_fill))?;
    let s_reg = py_float(get_or(schur, "relativeRegularization", &d_reg))?;

    let method = KrylovMethod::parse(&method)?;
    let (restart, maxiter) = match (strict_int(restart), strict_int(maxiter)) {
        (Some(r), Some(m)) if r >= 2 && m >= 1 => (r, m),
        _ => {

            validate_tolerances(rtol, atol)?;
            return Err(CfdError::Contract("Invalid Krylov restart or iteration limit.".into()));
        }
    };
    let strategy = |s: &str| {
        FactorizationStrategy::parse(s)
            .map_err(|_| CfdError::Contract("Unsupported block preconditioner.".into()))
    };
    validate_tolerances(rtol, atol)?;
    let config = SaddlePointSolverConfig {
        method,
        relative_tolerance: rtol,
        absolute_tolerance: atol,
        restart: usize::try_from(restart).unwrap_or(usize::MAX),
        maximum_iterations: usize::try_from(maxiter).unwrap_or(usize::MAX),
        velocity_preconditioner: strategy(&v_strategy)?,
        schur_preconditioner: strategy(&s_strategy)?,
        velocity_drop_tolerance: v_drop,
        velocity_fill_factor: v_fill,
        schur_drop_tolerance: s_drop,
        schur_fill_factor: s_fill,
        schur_relative_regularization: s_reg,
        require_convergence: true,
    };
    config.validate()?;
    Ok(config)
}

fn validate_tolerances(rtol: f64, atol: f64) -> CfdResult<()> {
    if !(rtol > 0.0 && rtol < 1.0) {
        return Err(CfdError::Contract("relative_tolerance must lie in (0, 1).".into()));
    }
    if !atol.is_finite() || atol < 0.0 {
        return Err(CfdError::Contract("absolute_tolerance must be finite and nonnegative.".into()));
    }
    Ok(())
}

