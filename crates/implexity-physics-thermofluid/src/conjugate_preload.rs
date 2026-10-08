// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use implexity_core::{CaeError, CaeResult};

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}


pub fn normalise_initialization(value: &Value, count: usize) -> CaeResult<Value> {
    let Some(m) = value.as_object() else {
        return contract("conjugate initialization must be an object");
    };
    if m.get("mode") == Some(&json!("prescribed")) {
        if m.len() != 1 {
            return contract("prescribed initialization accepts only mode");
        }
        return Ok(json!({"mode": "prescribed"}));
    }
    let required = ["mode", "provenance", "response_window", "service_start_index"];
    if m.get("mode") != Some(&json!("preload_history"))
        || m.len() != 4
        || !required.iter().all(|k| m.contains_key(*k))
    {
        return contract(
            "preload initialization requires mode=preload_history, service_start_index, response_window and provenance",
        );
    }
    let start = match &m["service_start_index"] {
        Value::Number(n) if n.is_u64() || n.is_i64() => n.as_i64(),
        _ => None,
    };
    let Some(start) = start.filter(|s| *s >= 1 && i128::from(*s) < count as i128 - 1) else {
        return contract(
            "preload service_start_index must be an integer selecting a solved endpoint with at least one preload and one service interval",
        );
    };
    let window = m["response_window"].as_str().unwrap_or_default();
    if !matches!(window, "service" | "entire_history") {
        return contract("preload response_window must be service or entire_history");
    }
    let Some(provenance) = m["provenance"].as_str().filter(|p| !p.trim().is_empty()) else {
        return contract("preload history requires nonempty provenance");
    };
    Ok(json!({"mode": "preload_history", "service_start_index": start, "response_window": window,
        "provenance": provenance.trim()}))
}

#[must_use]
pub fn initialization_description(problem: &Value) -> Value {
    let config = problem
        .get("initialization")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({"mode": "prescribed"}));
    let start = config.get("service_start_index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let times: Vec<f64> =
        problem["solid"]["times_s"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
    json!({
        "mode": config["mode"],
        "service_start_index": start,
        "service_start_time_s": times.get(start).copied().unwrap_or(f64::NAN),
        "preload_intervals": start,
        "service_intervals": times.len().saturating_sub(1 + start),
        "response_window": config.get("response_window").cloned().unwrap_or_else(|| json!("entire_history")),
        "provenance": config.get("provenance").cloned().unwrap_or(Value::Null),
        "seed_convention": "prescribed_state_then_load_at_first_solved_interval",
        "service_initial_state_source": if start > 0 { "simultaneous_history_endpoint" } else { "prescribed_seed" },
        "derivative_contract": "discrete_implicit_all_preload_and_service_steps",
        "internal_states_reset_at_service_start": false,
        "preload_is_optimization_phase": false,
        "thermal_or_fluid_steady_state_implied": false,
        "initial_seed_equilibrium_implied": false,
        "time_convention": "all authored arrays retain full history time; service time is shifted for reporting only",
        "response_convention": "endpoint strain responses retain total accumulated inelastic state; service window restricts heat increments and includes its initial sample in temperature peaks",
    })
}

#[must_use]
pub fn initialization_editor_schema() -> Value {
    json!({
        "title": "Initial state and preload", "type": "object",
        "description": "Preload uses the full authored solid and fluid time arrays. All fields are solved simultaneously before service starts.",
        "properties": {
            "mode": {"type": "string", "enum": ["prescribed", "preload_history"], "title": "Initialization mode"},
            "service_start_index": {"type": "integer", "minimum": 1, "title": "First service state index",
                "description": "Zero-based index into the full history. Leave at least one later service interval."},
            "response_window": {"type": "string", "enum": ["service", "entire_history"], "title": "Response integration window",
                "description": "Only heat integration and temperature peaks change window. Accumulated strain is never reset."},
            "provenance": {"type": "string", "title": "Physical preload history provenance"},
        },
    })
}
