// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

#[must_use]
pub fn assess_multiphysics(
    diagnostics: Option<&Value>,
    problem: Option<&Value>,
    limits: Option<&Value>,
) -> Value {
    let Some(limits) = limits.filter(|l| implexity_core::pyobj::truthy(l)) else {
        return json!({"ok": true, "issues": [], "values": {}, "units": {}, "limits_checked": []});
    };
    let Some(limit_map) = limits.as_object() else {
        return json!({"ok": false,
                      "issues": ["multiphysics limits must be an object of {registered_metric: {min, max, unit}}"],
                      "values": {}, "units": {}, "limits_checked": []});
    };
    let problem = problem
        .filter(|p| implexity_core::pyobj::truthy(p))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let diagnostics = diagnostics
        .filter(|d| implexity_core::pyobj::truthy(d))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let chk = implexity_core::registries::global().extensions.evaluate_regime_monitors(
        &problem,
        Some(&diagnostics),
        Some(limit_map),
    );
    let mut checked: Vec<String> = limit_map.keys().cloned().collect();
    checked.sort();
    json!({
        "ok": chk.get("ok").is_some_and(implexity_core::pyobj::truthy),
        "issues": chk.get("issues").cloned().unwrap_or_else(|| json!([])),
        "values": chk.get("values").cloned().unwrap_or_else(|| json!({})),
        "units": chk.get("units").cloned().unwrap_or_else(|| json!({})),
        "limits_checked": checked,
        "computable": chk.get("computable").cloned().unwrap_or(Value::Null),
        "engineering_satisfied": chk.get("engineering_satisfied").cloned().unwrap_or(Value::Null),
    })
}
