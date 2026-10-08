// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use crate::components::{SolidComponent, selected_component};
use crate::history;

#[must_use]
pub fn edge(
    source: &str,
    target: &str,
    mechanism: &str,
    status: &str,
    reason: &str,
    configuration: Option<Value>,
    derivative_scope: Option<&str>,
) -> Value {
    let scope = derivative_scope.unwrap_or(match status {
        "active" => "declared_discrete",
        "configured_zero" => "declared_zero",
        "unsupported" | "postprocess_only" => "none",
        _ => "unknown",
    });
    let configuration = match configuration {
        Some(Value::Object(m)) => Value::Object(m),
        _ => Value::Object(Map::new()),
    };
    json!({"id": format!("{source}.{target}.{mechanism}"), "source": source, "target": target,
        "mechanism": mechanism, "status": status, "derivative_scope": scope, "reason": reason,
        "configuration": configuration})
}

fn simple(source: &str, target: &str, mechanism: &str, status: &str, reason: &str) -> Value {
    edge(source, target, mechanism, status, reason, None, None)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

fn is_zero(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Bool(b) => !*b,
        _ => false,
    }
}


fn inelastic_heat(solid: &Value) -> Value {
    let coefficients: Vec<Value> = solid["materials"]
        .as_array()
        .map(|a| a.iter().map(|m| m.get("taylor_quinney").cloned().unwrap_or(Value::Null)).collect())
        .unwrap_or_default();
    let all_zero = !coefficients.is_empty() && coefficients.iter().all(is_zero);
    let mut records = Vec::new();
    for (role, kind) in [("plasticity", "plastic_evolution"), ("creep", "creep_evolution")] {
        let Some(name) = solid["components"].get(role).and_then(Value::as_str) else { continue };
        let declaration = match selected_component(name, kind) {
            Ok(SolidComponent::Plastic(p)) => Some(p.heat_coupling()),
            Ok(SolidComponent::Creep(c)) => Some(c.heat_coupling()),
            _ => None,
        };
        let known = declaration.as_ref().filter(|d| {
            matches!(d["dissipation_multiplier"].as_str(), Some("taylor_quinney" | "unity"))
                && d["coefficient_storage_exchange"].is_boolean()
        });
        let row = match known {
            None => json!({"role": role, "component": name, "status": "selection_unknown",
                "reason": "Selected law has no available heat-coupling declaration."}),
            Some(d) => {
                let multiplier = d["dissipation_multiplier"].as_str().unwrap_or_default();
                let exchange = d["coefficient_storage_exchange"].as_bool().unwrap_or(false);
                let zero = multiplier == "taylor_quinney" && all_zero && !exchange;
                json!({"role": role, "component": name,
                    "status": if zero { "configured_zero" } else { "active" },
                    "dissipation_multiplier": multiplier, "coefficient_storage_exchange": exchange})
            }
        };
        records.push(row);
    }
    let has = |s: &str| records.iter().any(|r| r["status"] == json!(s));
    let status = if records.is_empty() {
        "available_not_selected"
    } else if has("selection_unknown") {
        "selection_unknown"
    } else if has("active") {
        "active"
    } else {
        "configured_zero"
    };
    edge(
        "structure",
        "thermal",
        "inelastic_work_to_heat",
        status,
        "Selected-law heat contract. Taylor-Quinney scales plastic dissipation, not unity-weighted creep work or \
         hardening-coefficient stored-energy exchange. Active describes implementation, not a guaranteed nonzero \
         value at every state.",
        Some(json!({"taylor_quinney": coefficients, "selected_laws": records})),
        None,
    )
}

fn history_edges(solid: &Value) -> Result<Vec<Value>, implexity_core::CaeError> {
    let Some(h) = solid.get("material_history").filter(|v| !v.is_null()) else {
        return Ok(vec![simple(
            "material_state",
            "constitutive_response",
            "material_evolution",
            "available_not_selected",
            "No local material history is selected.",
        )]);
    };
    let name = h["component"].as_str().unwrap_or_default();
    let adapter = history::selected(name)?;
    let dependencies = adapter.state_dependencies_for(&h["settings"])?;
    let configuration = || Some(json!({"component": name}));
    let mut rows = Vec::new();
    for dependency in dependencies {
        rows.push(edge(
            &dependency,
            "material_state",
            "state_driving",
            "active",
            "Declared driving input of the selected local history.",
            configuration(),
            None,
        ));
    }
    for effect in adapter.property_effects() {
        rows.push(edge(
            "material_state",
            "constitutive_response",
            effect,
            "active",
            "Current-state property contribution, not independent calibration.",
            configuration(),
            None,
        ));
    }
    rows.push(edge(
        "material_state",
        "thermal",
        "stored_energy_exchange",
        "active",
        "Selected material energy residual; coefficients may make the contribution zero.",
        configuration(),
        None,
    ));
    Ok(rows)
}

fn object_or_empty(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => json!({}),
    }
}


#[allow(clippy::too_many_lines)]
pub fn report(
    provider_id: &str,
    problem: Option<&Value>,
    porous: bool,
) -> Result<Value, implexity_core::CaeError> {
    let Some(problem) = problem else {
        return Ok(json!({"schema": "implexity-coupling-inventory/1", "provider": provider_id,
            "context": "capability_only", "solve_performed": false,
            "physical_qualification": false, "edges": [],
            "notes": ["Supply an authored problem to inspect selected feedback.",
                      "This inventory is not a completeness or convergence certificate."]}));
    };
    let solid = &problem["solid"];
    let transport = problem.get("temperature_transport").cloned().unwrap_or_else(|| {
        json!(if porous { "conserved_population_enthalpy" } else { "trilinear_cell_average_v1" })
    });
    let mut rows = vec![
        edge(
            "flow",
            "thermal",
            "enthalpy_transport",
            "active",
            "Shared temperature with the selected conservative transport residual.",
            Some(json!({"transport": transport})),
            None,
        ),
        simple(
            "thermal",
            "structure",
            "constitutive_temperature",
            "active",
            "Shared temperature enters the selected solid law and internal states.",
        ),
        edge(
            "flow",
            "structure",
            "pressure_load",
            "active",
            "Pressure loads use the selected flow owner's pressure convention.",
            Some(json!({"absolute_pressure": !porous})),
            None,
        ),
    ];
    if porous {
        let law = &problem["viscosity"];
        let zero = law["kind"] == json!("constant") || law.get("log_slope_per_K").is_some_and(is_zero);
        rows.push(edge(
            "thermal",
            "flow",
            "viscosity",
            if zero { "configured_zero" } else { "active" },
            "Temperature feedback is zero for a constant viscosity law.",
            Some(json!({"law": law["kind"]})),
            None,
        ));
        let viscous = problem.get("viscous_coupling");
        let flag = |key: &str| truthy(viscous.and_then(|v| v.get(key)));
        let on = |b: bool| if b { "active" } else { "available_not_selected" };
        rows.extend([
            simple(
                "structure",
                "flow",
                "relative_drag_velocity",
                "active",
                "Interval skeleton velocity affects relative drag; the grid stays fixed.",
            ),
            simple(
                "relative_drag",
                "thermal",
                "drag_dissipation",
                "active",
                "Drag heat and reaction derive from the same relative-drag exchange.",
            ),
            edge(
                "flow",
                "structure",
                "resolved_viscous_traction",
                on(flag("trace_traction")),
                "Opt-in forced-BGK collisional stress on a fixed authored trace; not moving-interface FSI.",
                Some(object_or_empty(viscous)),
                None,
            ),
            edge(
                "flow",
                "thermal",
                "viscous_dissipation",
                on(flag("heating")),
                "Opt-in second-moment constitutive work. Not drag heat and not the finite-step kinetic-energy defect.",
                Some(object_or_empty(viscous)),
                None,
            ),
        ]);
        let wall = problem.get("reference_wall");
        let has_wall = truthy(wall);
        rows.extend([
            edge(
                "structure",
                "flow",
                "reference_wall_velocity",
                on(has_wall),
                "Planar reference-wall population return uses the solid interval velocity. Not a moving grid.",
                Some(object_or_empty(wall)),
                None,
            ),
            simple(
                "flow",
                "structure",
                "reference_wall_material_traction",
                on(has_wall),
                "Moving-mass momentum correction and transpose velocity/force transfer. Replaces the separate sampled \
                 trace when selected.",
            ),
            simple(
                "structure",
                "thermal",
                "reference_wall_geometric_caloric_transport",
                on(has_wall),
                "Small-displacement normal reference-mass source with local nodal cp*T. Neither reservoir flow nor \
                 mechanical-work heating.",
            ),
            simple(
                "structure",
                "flow",
                "finite_motion_geometry",
                "unsupported",
                "No evolving wall geometry, cell activation or finite-motion geometric conservation law is installed.",
            ),
        ]);
    } else {
        let material = &problem["fluid"]["material"];
        let slope = if material.is_object() {
            material.get("mu_slope").cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let zero = material.is_object() && is_zero(&slope);
        rows.push(edge(
            "thermal",
            "flow",
            "viscosity",
            if zero { "configured_zero" } else { "active" },
            "The selected viscosity law controls temperature-to-momentum feedback.",
            Some(json!({"mu_slope": slope})),
            None,
        ));
        rows.extend([
            simple(
                "flow",
                "structure",
                "resolved_viscous_traction",
                "active",
                "Density-jump transfer uses solved pressure and viscous stress.",
            ),
            simple(
                "flow",
                "thermal",
                "viscous_dissipation",
                "active",
                "The native fluid energy ledger includes resolved viscous dissipation.",
            ),
            simple(
                "structure",
                "flow",
                "moving_boundary",
                "unsupported",
                "Fixed-domain kinematics; displacement does not move the fluid mesh.",
            ),
        ]);
    }
    let reversible = solid["components"]["material"] == json!("constant_strain_thermoelastic_solid");
    let selected = |b: bool| if b { "active" } else { "available_not_selected" };
    rows.push(simple(
        "structure",
        "thermal",
        "reversible_thermoelastic_heat",
        selected(reversible),
        "Reversible entropy coupling requires its explicit constitutive selection.",
    ));
    rows.push(inelastic_heat(solid));
    let viscoelastic = truthy(solid.get("viscoelasticity"));
    rows.push(simple(
        "viscoelastic_state",
        "structure",
        "branch_stress",
        selected(viscoelastic),
        "Selected branch state contributes to solid stress in the shared history.",
    ));
    rows.push(simple(
        "viscoelastic_state",
        "thermal",
        "branch_energy_and_heat",
        selected(viscoelastic),
        "Selected branch storage and heat use the host material-energy contract.",
    ));
    rows.extend(history_edges(solid)?);
    let fatigue = truthy(solid.get("fatigue_observer"));
    rows.push(simple(
        "structure",
        "fatigue_usage",
        "cycle_postprocessing",
        if fatigue { "postprocess_only" } else { "available_not_selected" },
        "The fatigue observer does not feed damage back into stiffness or yield.",
    ));
    rows.extend([
        simple(
            "fatigue_damage",
            "structure",
            "degradation_feedback",
            "unsupported",
            "This host has no generic calibrated cyclic-damage feedback law.",
        ),
        simple(
            "irradiation",
            "structure",
            "swelling_eigenstrain",
            "unsupported",
            "Retention/defect evolution does not by itself implement swelling eigenstrain.",
        ),
        simple(
            "flow",
            "phase_change",
            "cavitation_dynamics",
            "unsupported",
            "A pressure-margin observer is not vapor production, collapse or erosion.",
        ),
    ]);
    for (index, source) in
        problem.get("field_sources").and_then(Value::as_array).into_iter().flatten().enumerate()
    {
        rows.push(edge(
            &format!("field_source_{index}"),
            "shared_history",
            "source_residual",
            "active",
            "Selected source contributes its declared residual and energy terms.",
            Some(object_or_empty(Some(source))),
            None,
        ));
    }
    Ok(json!({"schema": "implexity-coupling-inventory/1", "provider": provider_id,
        "context": "authored_problem", "solve_performed": false,
        "physical_qualification": false, "edges": rows,
        "notes": ["Active means selected discrete implementation, not a nonzero derivative at every state.",
                  "All retained source histories and design derivatives remain provider-owned.",
                  "No numerical solve or physical calibration is performed by this inventory.",
                  "An unsupported extension is not required for every application.",
                  "Auxiliary initialization lagging does not change the recorded full-problem coupling scope."]}))
}
