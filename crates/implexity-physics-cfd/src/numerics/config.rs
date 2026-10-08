// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

