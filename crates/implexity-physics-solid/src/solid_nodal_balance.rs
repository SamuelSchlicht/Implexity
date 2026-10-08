// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::CaeError;
use serde_json::{Value, json};

use crate::solid_history::SolidKernel;

#[derive(Debug, Clone)]
pub struct NodalBalance {
    pub nodal_thermal_residual_w: Vec<f64>,
    pub nodal_mechanical_residual_n: Vec<[f64; 3]>,
    pub temperature_reaction_inward_w: Vec<f64>,
    pub support_reaction_n: Vec<[f64; 3]>,
    pub summary: Value,
}

impl NodalBalance {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({"nodal_thermal_residual_W": self.nodal_thermal_residual_w,
            "nodal_mechanical_residual_N": self.nodal_mechanical_residual_n,
            "temperature_reaction_inward_W": self.temperature_reaction_inward_w,
            "support_reaction_N": self.support_reaction_n,
            "summary": self.summary})
    }
}

fn finite_len(values: &[f64], len: usize, name: &str, shape: &str) -> Result<(), CaeError> {
    if values.len() != len || !values.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract(format!("{name} requires finite real values with shape {shape}")));
    }
    Ok(())
}

fn vec3(v: &Value) -> [f64; 3] {
    [0, 1, 2].map(|i| v[i].as_f64().unwrap_or(f64::NAN))
}



#[allow(clippy::too_many_lines)]
pub fn solid_nodal_balance(
    solid: &SolidKernel,
    n: usize,
    state: &[f64],
    previous: &[f64],
    design: &[f64],
    interface_load_n: Option<&[[f64; 3]]>,
    interface_outward_power_w: Option<&[f64]>,
) -> Result<NodalBalance, CaeError> {
    let s = solid;
    if !(1..s.nt).contains(&n) {
        return Err(CaeError::contract("nodal balance requires an actual positive history interval"));
    }
    finite_len(state, s.state_size, "solid current state", &format!("({},)", s.state_size))?;
    finite_len(previous, s.state_size, "solid previous state", &format!("({},)", s.state_size))?;
    finite_len(design, 2 * s.nc + 3, "solid design", &format!("({},)", 2 * s.nc + 3))?;
    let mut load: Vec<[f64; 3]> = match interface_load_n {
        None => vec![[0.0; 3]; s.nn],
        Some(v) => {
            let flat: Vec<f64> = v.iter().flatten().copied().collect();
            finite_len(&flat, s.nn * 3, "solid interface force", &format!("({}, 3)", s.nn))?;
            v.to_vec()
        }
    };
    let contact: Vec<f64> = match interface_outward_power_w {
        None => vec![0.0; s.nn],
        Some(v) => {
            finite_len(v, s.nn, "solid interface outward heat", &format!("({},)", s.nn))?;
            v.to_vec()
        }
    };
    let interface_load = load.clone();
    let m = &s.model;
    let mut thermal = vec![0.0; s.nn];
    let mut internal = vec![[0.0; 3]; s.nn];
    let (fixed, previous_fixed) = s.local_data(n);
    let mut rows = vec![0.0; m.local_size()];
    for e in 0..s.ne {
        let cur = s.element_local(n, e, state, &fixed);
        let prev = s.element_local(n - 1, e, previous, &previous_fixed);
        let x = s.element_design(e, design);
        m.residual(&s.mesh.gradients[e], &cur, &prev, &x, &mut rows);
        for (i, node) in s.mesh.tets[e].iter().enumerate() {
            thermal[*node] += rows[i] * (m.ks * m.ts * m.ls);
            for c in 0..3 {
                internal[*node][c] += rows[4 + 3 * i + c] * (m.ss * m.ls * m.ls);
            }
        }
    }
    let h = [design[s.nc] * 1e-3, design[s.nc + 1] * 1e-3, design[s.nc + 2] * 1e-3];
    let side =
        |b: &Value| (usize::try_from(b["axis"].as_u64().unwrap_or(0)).unwrap_or(0), b["side"] == json!("hi"));
    for boundary in s.p["tractions"].as_array().into_iter().flatten() {
        let (axis, hi) = side(boundary);
        let (nodes, area) = s.boundary_weights(axis, hi, &h);
        let values = vec3(&boundary["values"][n]);
        for (node, a) in nodes.iter().zip(area) {
            for c in 0..3 {
                load[*node][c] += a * values[c];
            }
        }
    }
    if let Some(forces) = s.p.get("nodal_forces_N").filter(|v| !v.is_null()) {
        for (node, row) in load.iter_mut().enumerate() {
            let f = vec3(&forces[n][node]);
            for c in 0..3 {
                row[c] += f[c];
            }
        }
    }
    for boundary in s.p["heat_fluxes"].as_array().into_iter().flatten() {
        let (axis, hi) = side(boundary);
        let (nodes, area) = s.boundary_weights(axis, hi, &h);
        let value = boundary["values"][n].as_f64().unwrap_or(f64::NAN);
        for (node, a) in nodes.iter().zip(area) {
            thermal[*node] -= a * value;
        }
    }
    for group in &s.boundary.groups {
        let values = s.boundary.group_node_values(group, n, state, design);
        for (node, v) in group.face.iter().zip(values) {
            thermal[*node] += v * (m.ks * m.ts * m.ls);
        }
    }
    for (t, c) in thermal.iter_mut().zip(&contact) {
        *t += c;
    }
    let mechanical: Vec<[f64; 3]> =
        internal.iter().zip(&load).map(|(i, l)| [i[0] - l[0], i[1] - l[1], i[2] - l[2]]).collect();
    let mut support = mechanical.clone();
    for dof in &s.free_u {
        support[dof / 3][dof % 3] = 0.0;
    }
    let mut temperature_reaction = thermal.clone();
    for node in &s.free_t {
        temperature_reaction[*node] = 0.0;
    }
    let all_finite = thermal.iter().chain(&temperature_reaction).all(|v| v.is_finite())
        && mechanical.iter().chain(&support).flatten().all(|v| v.is_finite());
    if !all_finite {
        return Err(CaeError::contract("nonfinite recovered solid nodal balance"));
    }
    let displacement = s.nodal_displacement(n, state);
    let old_displacement = s.nodal_displacement(n - 1, previous);
    let increment: Vec<[f64; 3]> = displacement
        .iter()
        .zip(&old_displacement)
        .map(|(a, b)| [a[0] - b[0], a[1] - b[1], a[2] - b[2]])
        .collect();
    let work = |f: &[[f64; 3]]| -> f64 {
        f.iter().zip(&increment).map(|(a, d)| a[0] * d[0] + a[1] * d[1] + a[2] * d[2]).sum()
    };
    let applied_work = work(&load);
    let support_work = work(&support);
    let internal_work = work(&internal);
    let free_work: f64 = s.free_u.iter().map(|d| mechanical[d / 3][d % 3] * increment[d / 3][d % 3]).sum();
    let free_thermal: Vec<f64> = s.free_t.iter().map(|i| thermal[*i]).collect();
    let free_mech: Vec<f64> = s.free_u.iter().map(|d| mechanical[d / 3][d % 3]).collect();
    let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let resultant = |f: &[[f64; 3]]| [0, 1, 2].map(|c| f.iter().map(|r| r[c]).sum::<f64>());
    let summary = json!({
        "temperature_reaction_inward_W": temperature_reaction.iter().sum::<f64>(),
        "free_thermal_residual_sum_W": free_thermal.iter().sum::<f64>(),
        "free_thermal_residual_l2_W": norm(&free_thermal),
        "free_mechanical_residual_l2_N": norm(&free_mech),
        "support_resultant_N": resultant(&support),
        "applied_resultant_N": resultant(&load),
        "applied_endpoint_work_J": applied_work,
        "support_endpoint_work_J": support_work,
        "interface_endpoint_work_J": work(&interface_load),
        "internal_endpoint_work_J": internal_work,
        "free_residual_work_J": free_work,
        "discrete_virtual_work_identity_defect_J": internal_work - applied_work - support_work - free_work,
        "work_quadrature": "current_force_times_displacement_increment",
        "work_is_not_heat": true,
        "reaction_source": "production_local_residual_before_dirichlet_elimination",
        "temperature_reaction_sign": "positive_into_solid",
        "support_reaction_sign": "force_on_solid",
        "total_thermodynamic_energy_closure_claimed": false,
    });
    Ok(NodalBalance {
        nodal_thermal_residual_w: thermal,
        nodal_mechanical_residual_n: mechanical,
        temperature_reaction_inward_w: temperature_reaction,
        support_reaction_n: support,
        summary,
    })
}

#[must_use]
pub fn conjugate_energy_contract(history: &[Value], service_start_index: i64) -> Value {
    const POWERS: [&str; 7] = [
        "fluid_energy_balance_W",
        "solid_caloric_thermal_balance_W",
        "total_caloric_thermal_balance_W",
        "fields_and_finite_reservoirs_caloric_balance_W",
        "solid_entropy_thermal_balance_W",
        "fields_and_finite_reservoirs_entropy_thermal_balance_W",
        "solid_temperature_reaction_inward_W",
    ];
    const WORK: [&str; 5] = [
        "applied_endpoint_work_J",
        "support_endpoint_work_J",
        "interface_endpoint_work_J",
        "internal_endpoint_work_J",
        "free_residual_work_J",
    ];
    let index = |r: &Value| r["history_index"].as_i64().unwrap_or(0);
    let duration = |r: &Value| r["interval_duration_s"].as_f64().unwrap_or(f64::NAN);
    let mut windows = serde_json::Map::new();
    for (name, rows) in [
        ("preload", history.iter().filter(|r| index(r) <= service_start_index).collect::<Vec<_>>()),
        ("service", history.iter().filter(|r| index(r) > service_start_index).collect()),
        ("entire_history", history.iter().collect()),
    ] {
        let mut energy = serde_json::Map::new();
        for key in POWERS {
            let values: Vec<Option<f64>> = rows.iter().map(|r| r.get(key).and_then(Value::as_f64)).collect();
            let integrated = if !rows.is_empty() && values.iter().all(Option::is_some) {
                json!(values.iter().zip(&rows).map(|(v, r)| v.unwrap_or(0.0) * duration(r)).sum::<f64>())
            } else {
                Value::Null
            };
            energy.insert(format!("{}_J", key.trim_end_matches("_W")), integrated);
        }
        for key in WORK {
            let value = if rows.is_empty() {
                Value::Null
            } else {
                json!(
                    rows.iter()
                        .map(|r| r["solid_nodal_balance"][key].as_f64().unwrap_or(f64::NAN))
                        .sum::<f64>()
                )
            };
            energy.insert(key.to_string(), value);
        }
        windows.insert(
            name.to_string(),
            json!({"interval_indices": rows.iter().map(|r| index(r)).collect::<Vec<_>>(),
                "duration_s": rows.iter().map(|r| duration(r)).sum::<f64>(),
                "integrated_terms": energy}),
        );
    }

    let truthy = |v: Option<&Value>| match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(m)) => !m.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
    };
    let reversible =
        !history.is_empty() && history.iter().all(|r| truthy(r.get("solid_thermoelastic_step_ledger")));
    json!({
        "schema": "implexity-conjugate-energy-accounting/1",
        "solid_thermal_equation": if reversible { "entropy_form" } else { "sensible_enthalpy" },
        "fluid_thermal_equation": "incompressible_sensible_enthalpy",
        "dirichlet_heat_reactions_included": true,
        "constrained_mechanical_work_observed": true,
        "mechanical_work_added_as_heat": false,
        "preload_storage_and_work_discarded": false,
        "internal_state_reset_at_service_start": false,
        "windows": windows,
        "total_thermodynamic_energy_closed": false,
        "limitations": [
            "Reaction recovery closes accounting of the implemented thermal rows, not an independently verified total-energy law.",
            "The fluid wall is fixed and no reciprocal moving-wall energy equation is supplied.",
            "Endpoint mechanical work is recorded without reclassifying numerical or reversible energy as heat.",
            "General temperature-dependent material combinations do not imply reciprocal thermomechanical closure.",
        ],
    })
}
