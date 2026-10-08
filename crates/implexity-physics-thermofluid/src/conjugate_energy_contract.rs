// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::solid_energy_contract::{solid_step_terms, support};

use crate::conjugate_history::ConjugateKernel;
use crate::mac_energy_contract::observe_mechanical_energy;

fn num(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(f64::NAN)
}


#[allow(clippy::too_many_lines)]
pub fn observe_step(
    k: &ConjugateKernel,
    n: usize,
    current: &[f64],
    previous: &[f64],
    design: &[f64],
    row: &Value,
) -> CaeResult<Value> {
    let s = &k.s;
    let f = &k.f;
    let sx = k.solid_design(design);
    let fx = k.fluid_design(design);
    let coverage = support(&s.model);
    let raw = solid_step_terms(s, n, &current[k.solid_slice.clone()], &previous[k.solid_slice.clone()], &sx)?;
    if raw.iter().any(|(_, v)| v.is_some_and(|x| !x.is_finite())) {
        return Err(CaeError::contract("nonfinite solid discrete-energy observation"));
    }
    let solid: Map<String, Value> =
        raw.iter().map(|(key, v)| (key.clone(), v.map_or(Value::Null, |x| json!(x)))).collect();
    let solid = Value::Object(solid);
    let fluid = observe_mechanical_energy(
        f,
        n,
        &current[k.fluid_slice.clone()],
        &previous[k.fluid_slice.clone()],
        &fx,
    )?;
    let complete = coverage["complete_selected_storage_decomposition"] == json!(true)
        && fluid["available"] == json!(true);
    let mut result = json!({"schema": "implexity-conjugate-discrete-energy-step/1",
        "solid_coverage": coverage, "solid": solid, "fluid": fluid,
        "complete_selected_decomposition": complete,
        "total_thermodynamic_energy_closed": false, "terms": null});
    if !complete {
        return Ok(result);
    }
    let dt = num(row, "interval_duration_s");
    let fw = &fluid["terms"];
    let nodal = &row["solid_nodal_balance"];
    let boundary = &row["solid_boundary_exchange_ledger"];
    let h: Vec<f64> = (0..3).map(|a| sx[s.nc + a] * 1e-3).collect();
    let hh = [h[0], h[1], h[2]];
    let mut solid_source = 0.0;
    for b in s.p["heat_fluxes"].as_array().into_iter().flatten() {
        let axis = b["axis"].as_u64().unwrap_or(0) as usize;
        let (_, w) = s.boundary_weights(axis, b["side"] == "hi", &hh);
        solid_source += w.iter().sum::<f64>() * b["values"][n].as_f64().unwrap_or(0.0);
    }
    let volume = h[0] * h[1] * h[2];
    solid_source +=
        s.p["volumetric_heat_W_m3"][n].as_f64().unwrap_or(0.0) * sx[..s.nc].iter().sum::<f64>() * volume;
    let fraction: f64 = fx[..f.nc].iter().map(|x| f.law.fraction(*x)).sum();
    let fluid_source = f.p["volumetric_heat_W_m3"][n].as_f64().unwrap_or(0.0) * fraction * volume;
    let finite: Vec<&Value> =
        boundary["finite_reservoirs"].as_object().map(|m| m.values().collect()).unwrap_or_default();
    let reservoir_storage = dt * finite.iter().map(|r| num(r, "storage_W")).sum::<f64>();
    let reservoir_source = dt * finite.iter().map(|r| num(r, "source_W")).sum::<f64>();
    let reservoir_residual = dt * finite.iter().map(|r| num(r, "balance_W")).sum::<f64>();
    let sensible_form = coverage["material_thermal_form"] == "sensible_enthalpy";
    let sensible = if sensible_form { num(row, "solid_sensible_enthalpy_increment_J") } else { 0.0 };
    let solid_storage = num(&solid, "stored_energy_increment_J") + sensible;
    let fluid_storage = num(row, "fluid_enthalpy_increment_J") + num(fw, "kinetic_energy_increment_J");
    let storage = solid_storage + fluid_storage + reservoir_storage;
    let external_heat = dt
        * (solid_source + fluid_source + num(row, "solid_temperature_reaction_inward_W")
            - num(row, "fluid_external_outward_heat_W")
            - num(boundary, "prescribed_reservoir_inward_W"))
        + reservoir_source;
    let interface_work = num(nodal, "interface_endpoint_work_J");
    let external_solid_work =
        num(nodal, "applied_endpoint_work_J") + num(nodal, "support_endpoint_work_J") - interface_work;
    let external_work =
        external_solid_work + num(fw, "pressure_boundary_work_inward_J") + num(fw, "body_work_inward_J");
    let raw_gap = storage - external_heat - external_work;
    let numerical = num(&solid, "backward_euler_defect_J")
        + num(fw, "backward_euler_defect_J")
        + num(fw, "upwind_kinetic_defect_J");
    let boundary_kinetic = num(fw, "advective_boundary_kinetic_transport_outward_J");
    let constraints = num(fw, "continuity_constraint_work_J") + num(fw, "dual_continuity_work_J");
    let missing_exchange = num(&solid, "unmodeled_thermal_exchange_J");
    let unheated = num(&solid, "nonthermal_dissipation_J");
    let constitutive_defect = num(&solid, "constitutive_work_identity_defect_J");
    let interface_heat = dt * num(row, "interface_balance_W");
    let fluid_transfer_defect =
        num(fw, "dissipation_heat_transfer_defect_J") + num(fw, "advection_identity_defect_J");
    let corrections =
        numerical + boundary_kinetic + constraints + missing_exchange + unheated + constitutive_defect
            - interface_work
            + interface_heat
            + fluid_transfer_defect;
    let accounted = raw_gap + corrections;
    let solid_thermal = if sensible_form {
        num(row, "solid_caloric_thermal_balance_W")
    } else {
        num(row, "solid_entropy_thermal_balance_W")
    };
    let residual_work_heat = dt * (solid_thermal + num(row, "fluid_energy_balance_W"))
        + num(nodal, "free_residual_work_J")
        + num(fw, "actual_momentum_residual_work_J")
        + reservoir_residual;
    let work_assembly = num(&solid, "endpoint_stress_work_J") - num(nodal, "internal_endpoint_work_J");
    let terms = [
        ("solid_stored_energy_increment_J", solid_storage),
        ("fluid_enthalpy_and_kinetic_increment_J", fluid_storage),
        ("finite_reservoir_storage_increment_J", reservoir_storage),
        ("total_modeled_storage_increment_J", storage),
        ("external_heat_inward_J", external_heat),
        ("external_work_inward_J", external_work),
        ("advective_boundary_kinetic_transport_outward_J", boundary_kinetic),
        ("raw_storage_minus_heat_and_work_J", raw_gap),
        ("raw_first_law_gap_J", raw_gap + boundary_kinetic),
        ("backward_euler_and_upwind_defect_J", numerical),
        ("continuity_constraint_work_J", constraints),
        ("unmodeled_solid_thermal_exchange_J", missing_exchange),
        ("unassigned_nonthermal_dissipation_J", unheated),
        ("constitutive_work_identity_defect_J", constitutive_defect),
        ("unpaired_solid_interface_work_J", interface_work),
        ("interface_heat_pairing_defect_J", interface_heat),
        ("fluid_transfer_and_advection_identity_defect_J", fluid_transfer_defect),
        ("balance_after_disclosed_terms_J", accounted),
        ("actual_residual_work_and_heat_J", residual_work_heat),
        ("solid_stress_nodal_work_identity_defect_J", work_assembly),
        ("coupled_accounting_identity_defect_J", accounted - residual_work_heat - work_assembly),
    ];
    if terms.iter().any(|(_, v)| !v.is_finite()) {
        return Err(CaeError::contract("nonfinite combined discrete energy contract"));
    }
    result["terms"] = Value::Object(terms.iter().map(|(k, v)| ((*k).to_string(), json!(v))).collect());
    Ok(result)
}

#[must_use]
pub fn history_contract(history: &[Value], service_start_index: usize) -> Value {
    let rows: Vec<&Value> = history.iter().map(|r| &r["discrete_energy_step"]).collect();
    let complete = !rows.is_empty()
        && rows.iter().all(|r| r.is_object() && r["complete_selected_decomposition"] == json!(true));
    let index = |r: &Value| r["history_index"].as_u64().unwrap_or(0) as usize;
    let mut windows = Map::new();
    let sets: [(&str, Vec<usize>); 3] = [
        ("preload", (0..history.len()).filter(|i| index(&history[*i]) <= service_start_index).collect()),
        ("service", (0..history.len()).filter(|i| index(&history[*i]) > service_start_index).collect()),
        ("entire_history", (0..history.len()).collect()),
    ];
    for (name, indices) in sets {
        let available = !indices.is_empty() && indices.iter().all(|i| rows[*i]["terms"].is_object());
        let (mut integrated, mut absolute, mut maximum) = (Value::Null, Value::Null, Value::Null);
        if available {
            let keys: Vec<String> = rows[indices[0]]["terms"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let value = |i: usize, k: &str| rows[i]["terms"][k].as_f64().unwrap_or(f64::NAN);
            integrated = Value::Object(
                keys.iter()
                    .map(|k| (k.clone(), json!(indices.iter().map(|i| value(*i, k)).sum::<f64>())))
                    .collect(),
            );
            absolute = Value::Object(
                keys.iter()
                    .map(|k| (k.clone(), json!(indices.iter().map(|i| value(*i, k).abs()).sum::<f64>())))
                    .collect(),
            );
            maximum = Value::Object(
                keys.iter()
                    .map(|k| {
                        (
                            k.clone(),
                            json!(
                                indices.iter().map(|i| value(*i, k).abs()).fold(f64::NEG_INFINITY, f64::max)
                            ),
                        )
                    })
                    .collect(),
            );
        }
        windows.insert(
            name.into(),
            json!({"history_indices": indices.iter().map(|i| index(&history[*i])).collect::<Vec<_>>(),
                "duration_s": indices.iter().map(|i| num(&history[*i], "interval_duration_s")).sum::<f64>(),
                "available": available, "signed_sum_J": integrated, "absolute_sum_J": absolute,
                "maximum_absolute_interval_J": maximum}),
        );
    }
    json!({"schema": "implexity-conjugate-discrete-energy/1",
        "complete_selected_decomposition": complete, "windows": windows,
        "unit": "J", "positive_heat_and_work": "into_fields_and_finite_capacity_reservoirs",
        "fixed_interface_work": "measured_unpaired_solid_work_not_an_external_physical_source",
        "numerical_defects": "recorded_separately_not_injected_as_heat",
        "unsupported_material_terms": "null_never_zero",
        "history_design": "fixed_during_each_physical_history_with_full_preload_dependence",
        "new_response_objectives_registered": false, "changes_solve_admission": false,
        "total_thermodynamic_energy_closed": false,
        "limitations": [
            "A reconstructed residual identity does not establish a thermodynamic first law.",
            "The fixed fluid wall supplies no reciprocal work for the solid interface displacement.",
            "Ordinary caloric/eigenstrain materials do not declare a common thermodynamic potential.",
            "Taylor-Quinney remainder is unassigned nonthermal dissipation, not invented stored energy.",
            "MAC dual continuity and upwind defects are retained, not assumed zero from cell divergence.",
            "No changes to physical equations, numerical tolerances, objective registration, or final engineering admission."]})
}
