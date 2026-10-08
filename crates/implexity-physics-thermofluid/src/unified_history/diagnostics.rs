// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_solve::native_history::HistorySolution;

use super::kernel::{UnifiedKernel, design_key};
use super::{TRANSPORT_BOUNDARY_LEDGER_RELATIVE_TOLERANCE, selected_limitations};
use crate::incompressible_transport::GroupSet;

fn add(map: &mut Map<String, Value>, other: &Value) {
    if let Some(o) = other.as_object() {
        for (k, v) in o {
            map.insert(k.clone(), v.clone());
        }
    }
}

impl UnifiedKernel {

    #[allow(clippy::too_many_lines)]
    fn compute_solution_diagnostics(&self, x: &[f64], sol: &HistorySolution) -> CaeResult<Value> {
        let s = &self.s;
        let f = &self.f;
        let sl = self.solid_slice.clone();
        let fl = self.fluid_slice.clone();
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let volume = h[0] * h[1] * h[2];
        let fx = self.fx(x);
        let full: Vec<Vec<f64>> =
            sol.states.iter().enumerate().map(|(n, z)| self.expand(n, z)).collect::<CaeResult<_>>()?;
        let can_balance = s.p["temperature_bcs"].as_array().is_none_or(Vec::is_empty);
        let rho = &x[..self.nc];
        let weighted = |values: &[f64]| -> f64 {
            values.iter().zip(&s.mesh.owners).map(|(v, o)| v * rho[*o]).sum::<f64>() * volume / 6.0
        };
        let mut rows = Vec::new();
        for n in 1..self.nt {
            let dt = s.times[n] - s.times[n - 1];
            let z = &full[n];
            let old = &full[n - 1];
            let fm = f.metrics(n, &z[fl.clone()], &fx);
            let sd = s.observe(n, &z[sl.clone()], &old[sl.clone()], x);
            let nodal = s.nodal_temperature(n, &z[sl.clone()]);
            let boundary_ledger = s.boundary.ledger(n, &z[sl.clone()], &old[sl.clone()], x, &nodal);
            let exchange_outward = boundary_ledger["solid_exchange_outward_W"].as_f64().unwrap_or(0.0);
            let displacement_screen =
                self.fixed_geometry_validity(&s.nodal_displacement(n, &z[sl.clone()]), x);
            let fluid_enthalpy = self.volume_transfer.enthalpy_j(n, z, x)?;
            let old_fluid_enthalpy = self.volume_transfer.enthalpy_j(n - 1, old, x)?;
            let thermoelastic = self.solid_step_ledger(n, &z[sl.clone()], &old[sl.clone()], x);
            let solid_storage = match &thermoelastic {
                Some(t) => t["entropy_thermal_storage_J"].as_f64().unwrap_or(f64::NAN),
                None => {
                    self.solid_enthalpy(n, &z[sl.clone()], x)?
                        - self.solid_enthalpy(n - 1, &old[sl.clone()], x)?
                }
            };
            let storage = (solid_storage + fluid_enthalpy - old_fluid_enthalpy) / dt;
            let external: f64 = match &self.nodal_transport {
                Some(t) => t.boundary_energy(n, z, x)?.iter().map(|(_, v)| v).sum(),
                None => f.boundary_energy(n, &z[fl.clone()], &fx).iter().map(|(_, v)| v).sum(),
            };
            let mut solid_q = 0.0;
            for b in s.p["heat_fluxes"].as_array().into_iter().flatten() {
                let axis = b["axis"].as_u64().unwrap_or(0) as usize;
                let hi = b["side"] == "hi";
                let (_, w) = s.boundary_weights(axis, hi, &h);
                solid_q += w.iter().sum::<f64>() * b["values"][n].as_f64().unwrap_or(0.0);
            }
            solid_q +=
                s.p["volumetric_heat_W_m3"][n].as_f64().unwrap_or(0.0) * rho.iter().sum::<f64>() * volume;
            let fraction: f64 = rho.iter().map(|r| f.law.fraction(*r)).sum();
            let fluid_q = f.p["volumetric_heat_W_m3"][n].as_f64().unwrap_or(0.0) * fraction * volume;
            let inelastic = sd.heat_increment.iter().sum::<f64>() * volume / 6.0 / dt;
            let dissipation_residual =
                f.residual(GroupSet::Dissipation, n, &z[fl.clone()], &old[fl.clone()], &fx)?;
            let diss = -dissipation_residual[f.nv + f.nc..].iter().sum::<f64>() * f.hs;
            let dirichlet_split = self.volume_transfer.dirichlet_absorbed_dissipation_w(n, z, old, x)?;
            let transport_outward = if let Some(t) = &self.nodal_transport {
                t.net_outward_power(n, z, x)?
            } else {
                let rf = self.assembly.residual(n, z, old, x)?;
                rf[self.ft..self.ft + self.nc].iter().sum::<f64>() * f.hs
            };
            let error = transport_outward - external;
            let scale = 1.0_f64.max(transport_outward.abs()).max(external.abs());
            let tolerance = TRANSPORT_BOUNDARY_LEDGER_RELATIVE_TOLERANCE * scale;
            let passed = error.abs() <= tolerance;
            if !passed {
                return Err(CaeError::convergence(format!(
                    "shared fluid transport/boundary energy ledger failed; history_step={n}; error_W={}; tolerance_W={}; transport_net_outward_W={}; boundary_net_outward_W={}",
                    implexity_core::py_repr::repr_float(error),
                    implexity_core::py_repr::repr_float(tolerance),
                    implexity_core::py_repr::repr_float(transport_outward),
                    implexity_core::py_repr::repr_float(external)
                )));
            }
            let evo_heat = weighted(&sd.material_evolution_heat);
            let mut evo_source = weighted(&sd.material_external_energy);
            let current_stored = weighted(&sd.material_stored_energy);
            let old_sd = s.observe(n - 1, &old[sl.clone()], &old[sl.clone()], x);
            let previous_stored = weighted(&old_sd.material_stored_energy);
            let mut source_rows = Vec::new();
            let (mut extra_heat, mut extra_deposition) = (0.0, 0.0);
            for source in &self.sources {
                let row = source.source.diagnostics(n, z, old, x)?;
                extra_heat += row.get("sensible_deposition_W").and_then(Value::as_f64).unwrap_or(0.0);
                extra_deposition += row.get("deposition_W").and_then(Value::as_f64).unwrap_or(0.0);
                if source.source.forcing(n, z, x)?.is_some() {
                    evo_source = row.get("material_production_W").and_then(Value::as_f64).unwrap_or(f64::NAN);
                }
                source_rows.push(Value::Object(row));
            }
            let stored_rate = (current_stored - previous_stored) / dt;
            let field_balance = storage + external + exchange_outward
                - solid_q
                - fluid_q
                - inelastic
                - diss
                - evo_heat
                - extra_heat;
            let reservoir_balance: f64 = boundary_ledger["finite_reservoirs"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(_, r)| r["balance_W"].as_f64().unwrap_or(0.0))
                .sum();
            let solid_material_validity = s.material_validity(n, &z[sl.clone()], x);
            let fluid_dual_validity = self.volume_transfer.material_validity(n, z, x)?;
            let te = thermoelastic.is_some();
            let mut row = Map::new();
            let entries = json!({
                "material_evolution_heat_W": evo_heat, "material_external_source_W": evo_source,
                "material_stored_energy_J": current_stored, "material_stored_energy_rate_W": stored_rate,
                "material_energy_balance_W": stored_rate + evo_heat - evo_source,
                "total_sensible_plus_material_balance_W": if can_balance && !te {
                    json!(storage + stored_rate + external + exchange_outward - solid_q - fluid_q - inelastic - diss - evo_source - extra_heat)
                } else { Value::Null },
                "solid_thermoelastic_step_ledger": thermoelastic.clone().unwrap_or_else(|| json!({})),
                "solid_thermal_storage_kind": if te { "T_new_delta_entropy" } else { "sensible_enthalpy_increment" },
                "total_internal_energy_with_work_and_numerical_defect_balance_W": if can_balance && te { json!(field_balance) } else { Value::Null },
                "step": n, "temperature_history_shared": self.film.is_none(),
                "total_thermal_balance_W": if can_balance { json!(field_balance) } else { Value::Null },
                "fluid_body_force_power_W": fm.body_force_power,
                "fields_and_finite_reservoirs_thermal_balance_W": if can_balance { json!(field_balance + reservoir_balance) } else { Value::Null },
                "field_source_ledger": source_rows, "field_source_sensible_W": extra_heat, "field_source_deposition_W": extra_deposition,
                "solid_boundary_exchange_ledger": boundary_ledger,
                "storage_W": storage, "fluid_nodal_dual_volume_enthalpy_J": fluid_enthalpy,
                "fluid_external_outward_heat_W": external, "solid_external_and_bulk_heat_W": solid_q,
                "fluid_bulk_heat_W": fluid_q, "inelastic_heat_W": inelastic, "fluid_dissipation_W": diss,
            });
            add(&mut row, &entries);
            add(&mut row, &dirichlet_split);
            add(
                &mut row,
                &json!({
                    "fluid_transport_net_outward_W": transport_outward,
                    "fluid_transport_boundary_ledger_error_W": error,
                    "fluid_transport_boundary_ledger_scale_W": scale,
                    "fluid_transport_boundary_ledger_tolerance_W": tolerance,
                    "fluid_transport_boundary_ledger_relative_tolerance": TRANSPORT_BOUNDARY_LEDGER_RELATIVE_TOLERANCE,
                    "fluid_transport_boundary_ledger_passed": passed,
                }),
            );
            add(&mut row, &displacement_screen);
            add(&mut row, &solid_material_validity);
            add(&mut row, &fluid_dual_validity);
            add(&mut row, &f.validity(&z[fl.clone()], &fx)?);
            row.insert("hydraulic_power_W".into(), json!(fm.hydraulic_power));
            rows.push(Value::Object(row));
        }
        let tr = self.transfer.observables(&full[self.nt - 1], x);
        let solid_states: Vec<Vec<f64>> = full.iter().map(|z| z[sl.clone()].to_vec()).collect();
        let literal = (1..self.nt)
            .flat_map(|n| s.nodal_temperature(n, &solid_states[n]))
            .fold(f64::NEG_INFINITY, f64::max);
        Ok(json!({
            "coupling_history": rows,
            "fatigue_observer": s.fatigue_diagnostics(&solid_states, x)?,
            "history_observers": self.validate_observers(&sol.states, x)?,
            "literal_shared_temperature_peak_K": if self.film.is_none() { json!(literal) } else { Value::Null },
            "literal_solid_temperature_peak_K": literal,
            "literal_coolant_temperature_peak_K": self.film.as_ref().map(|film| (1..self.nt)
                .flat_map(|n| film.fluid_nodal_temperature(n, &full[n]))
                .fold(f64::NEG_INFINITY, f64::max)),
            "traction_nodal_resultant_error_N": tr.nodal_minus_face_resultant_n.iter().fold(0.0_f64, |a, v| a.max(v.abs())),
        }))
    }


    pub fn solution_diagnostics(&self, x: &[f64], sol: &HistorySolution) -> CaeResult<Value> {
        let key = (design_key(x), Self::history_digest(&sol.states));
        if let Some((k, v)) = self.lock().diagnostics.as_ref()
            && *k == key
        {
            return Ok(v.clone());
        }
        let value = self.compute_solution_diagnostics(x, sol)?;
        self.lock().diagnostics = Some((key, value.clone()));
        Ok(value)
    }


    #[allow(clippy::too_many_lines)]
    pub fn diagnostics(
        &self,
        x: &[f64],
        sol: &HistorySolution,
        control: &[f64],
        spacing_mm: &[f64],
        design_state_id: &str,
    ) -> CaeResult<Map<String, Value>> {
        let s = &self.s;
        let m = &s.model;
        let f = &self.f;
        let h: Vec<f64> = spacing_mm.iter().map(|v| v * 1e-3).collect();
        let can_balance = s.p["temperature_bcs"].as_array().is_none_or(Vec::is_empty);
        let history = self.solution_diagnostics(x, sol)?;
        let history_rows = history["coupling_history"].as_array().ok_or_else(||
            implexity_core::CaeError::convergence("missing solved history diagnostics"))?;
        let rows = self.p.get("field_sources").and_then(Value::as_array).cloned().unwrap_or_default();
        let placement: Vec<Value> = rows
            .iter()
            .zip(&self.sources)
            .map(|(row, source)| {
                let component = row["component"].as_str().unwrap_or_default();
                let placement = source.source.thermal_placement().map_or_else(
                    || {
                        if component == "neutral_transport_deposition" {
                            "solid_T4_element_row_sum_nodal_via_DepositionResidual".to_string()
                        } else {
                            "component_owned_sparse_conservative_rows_not_implicitly_remapped".to_string()
                        }
                    },
                    str::to_string,
                );
                json!({"component": component, "placement": placement,
                    "implicitly_remapped_by_fluid_shared_temperature_split": false})
            })
            .collect();
        let preload = match &self.preload {
            Some(p) => p.report(x)?,
            None => {
                json!({"method": "fixed_reference_legacy", "mechanical_preload_equilibrium_established": false})
            }
        };
        let filtered = self.filtered_control(control, spacing_mm)?;
        let measures = self.phase.measures(&filtered, &h).map_err(|e| CaeError::contract(e.to_string()))?;
        let density_filter = match &self.density_filter {
            None => Value::Null,
            Some(filter) => {
                let mut r = filter.report();
                r["provenance"] = self.p["phase_map"]["density_filter"]["provenance"].clone();
                r
            }
        };
        let plastic_states = match m.plastic {
            Some(pl) => json!(pl.state_metadata(&m.materials)?),
            None => json!([]),
        };
        let policy = s.p.get("inactive_phase_numerical_material").filter(|v| !v.is_null());
        let pv = |key: &str| policy.map_or(Value::Null, |p| p[key].clone());
        let transport = self.p["temperature_transport"].as_str().unwrap_or("trilinear_cell_average_v1");
        let nodal = self.nodal_transport.is_some();
        let film_enabled = self.film.is_some();
        let mut limitations = selected_limitations(&self.p);
        if film_enabled {
            for row in &mut limitations {
                if row.starts_with("Perfect thermal contact through one shared temperature;") {
                    *row = "Separate solid and coolant temperatures coupled by the authored wall-film model; no boiling or critical-heat-flux model.".into();
                } else if row.starts_with("The optimized shared-temperature response") {
                    *row = "The built-in solid nodal temperature response is a smooth order-32 power mean; selected objectives retain their own definitions; solid and coolant literal peaks are separate.".into();
                }
            }
        }
        let vt = self.volume_transfer.report();
        let units: Map<String, Value> =
            self.response_units.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let mut provenance: Vec<Value> =
            m.materials.iter().map(implexity_physics_solid::material::SolidMaterial::provenance).collect();
        provenance.push(f.card.raw["provenance"].clone());
        let out = json!({
            "design_state_id": design_state_id,
            "state_unknowns_per_step": self.state_size,
            "state_residual_norms": sol.residual_norms,
            "initialization": preload,
            "fatigue_observer": history["fatigue_observer"],
            "newton_iterations": sol.newton_iterations,
            "coupled_assembly": self.assembly.report(),
            "shared_state_reduction": self.reduction.report(),
            "phase_measures": measures,
            "density_filter": density_filter,
            "coupling_history": history["coupling_history"],
            "response_units": units,
            "history_observers": history["history_observers"],
            "phase_connectivity": self.connectivity(x)?,
            "material_history": s.p.get("material_history").cloned().unwrap_or(Value::Null),
            "registered_field_source_thermal_placement": placement,
            "inelastic_state_layout": {
                "plastic_size": m.layout.plastic_size, "creep_offset": m.layout.plastic_size,
                "creep_size": m.layout.creep_size, "material_history_offset": m.layout.material_start(),
                "disabled_constitutive_diagnostics": {
                    "plasticity": if m.plastic.is_some() { "stored_native_state" } else { "exact_zero_reconstruction_no_history_coordinates" },
                    "creep": if m.creep.is_some() { "stored_native_state" } else { "exact_zero_reconstruction_no_history_coordinates" }},
                "identity_coordinates_eliminated_per_tetrahedron": (if m.plastic.is_some() { 0 } else { 7 }) + (if m.creep.is_some() { 0 } else { 7 }),
                "additional_plastic_states": plastic_states},
            "material_history_states": m.history.as_ref().map_or_else(Vec::new, |h| h.metadata.clone()),
            "solid_inactive_phase_numerical_material": {
                "enabled": policy.is_some(), "schema": pv("schema"), "method": pv("method"), "scope": pv("scope"),
                "provenance": pv("provenance"),
                "endpoint_match_continuity": "C1_value_and_first_derivative; base PCHIP is not generally C2 at its authored endpoints",
                "physical_material_extrapolation_authorized": self.p["applicability_policy"].as_str() == Some("report_only"),
                "physical_validity_gate": self.p["applicability_policy"].as_str() != Some("report_only"),
                "numerical_continuation_scope": if self.p["applicability_policy"].as_str() == Some("report_only") { "all_positive_temperature_states_for_optimization_exploration" } else { "inactive_phase_only" },
                "material_history_addin_supported": m.history.as_ref().is_some_and(|h| h.numerical_extension.is_some()),
                "material_history_numerical_extension": m.history.as_ref().map_or(Value::Null, implexity_physics_solid::history::MaterialHistoryBinding::numerical_extension_report),
                "physical_qualification": false},
            "physical_qualification": false,
            "applicability_policy": self.p.get("applicability_policy").cloned().unwrap_or(json!("enforce")),
            "regime_valid": history_rows.iter().all(|r| r["finite_state_screen_passed"] == json!(true)
                && r["positive_absolute_pressure_screen_passed"] == json!(true)
                && r["mass_conservation_screen_passed"] == json!(true)),
            "applicability_screens_passed": history_rows.iter().all(|r| r["regime_valid"] == json!(true)
                && r["solid_temperature_material_interval_screen_passed"] == json!(true)
                && r["fluid_dual_volume_nodal_temperature_evaluation_domain_screen_passed"] == json!(true)
                && r["displacement_over_cell_screen_passed"] == json!(true)
                && r["channel_width_screen_passed"] == json!(true)),
            "regime_valid_semantics": "retained_numerical_checks_and_Newton_convergence; physical_applicability_is_reported_separately",
            "limitations": limitations,
            "wall_film": self.film.as_ref().map_or(Value::Null, |film| film.report()),
            "temperature_state_layout": if film_enabled { "separate_solid_and_coolant_nodal_fields" } else { "shared_solid_and_coolant_nodal_field" },
            "fluid_nodal_dual_volume": vt,
            "shared_temperature_discretization": {
                "field_name_scope": "legacy_metadata_container; temperature_state_layout_declares_actual_ownership",
                "temperature_fields_shared": !film_enabled,
                "coolant_rows_reduced": self.film.as_ref().and_then(|film|
                    self.retained.iter().position(|row| *row == film.fluid_rows.start)
                        .map(|start| json!([start, start + film.fluid_rows.len()]))),
                "method": transport,
                "selected_transport": self.nodal_transport.as_ref().map_or(Value::Null, |t| t.report()),
                "cell_anchor": "Cartesian_cell_centre_trilinear_eight_vertex_average",
                "solid_tetrahedralization": "conforming_parity_alternating_kuhn",
                "solid_conduction_quadrature": "T4_Galerkin_retained",
                "solid_transient_and_source_quadrature": "T4_row_sum_lumped_nodal_dual_volume",
                "fluid_caloric_and_authored_source_quadrature": "same_T4_row_sum_lumped_nodal_dual_volume_with_nodewise_enthalpy",
                "fluid_cell_terms_retained_on_Q": if nodal { json!([]) } else { json!(["advection", "conduction"]) },
                "fluid_work_to_heat_quadrature": "native_cell_and_edge_integration_then_T4_row_sum_nodal_distribution_without_nodewise_mu_re_evaluation",
                "optional_field_source_terms": "retain_each_registered_source_component_owned_conservative_rows; neutral_transport_deposition_is_T4_element_row_sum_nodal; each_source_placement_in_registered_field_source_thermal_placement",
                "common_nodal_dual_volume_terms": if film_enabled { Value::Null } else { json!(["solid_caloric_storage", "solid_volumetric_heat", "solid_inelastic_heat",
                    "solid_material_history_heat", "fluid_caloric_storage", "fluid_volumetric_heat",
                    "fluid_normal_viscous_dissipation", "fluid_Brinkman_dissipation", "fluid_shear_dissipation"]) },
                "solid_nodal_dual_volume_terms": ["solid_caloric_storage", "solid_volumetric_heat", "solid_inelastic_heat", "solid_material_history_heat"],
                "coolant_nodal_dual_volume_terms": ["fluid_caloric_storage", "fluid_volumetric_heat", "fluid_normal_viscous_dissipation", "fluid_Brinkman_dissipation", "fluid_shear_dissipation"],
                "reflection_equivariant_axes": (0..3).filter(|a| self.grid[*a].is_multiple_of(2)).collect::<Vec<_>>(),
                "transport_cell_temperature_map_nnz": self.q.nnz(),
                "nodal_dual_volume_map_nnz": self.l.nnz(),
                "nodal_dual_volume_zero_weight_nodes": vt["nodal_dual_volume_zero_weight_nodes"],
                "constant_temperature_preserved": true,
                "linear_cell_temperature_reproduced": true,
                "transport_discretization": transport,
                "nodal_discrete_maximum_principle_claimed": false,
                "summed_cell_energy_balance_preserved": true,
                "uniform_mixed_capacity_patch_preserved": true,
                "maximum_principle_scope": if nodal {
                    "fixed_coefficient_nodal_upwind_M_matrix; nonlinear_coupling_requires_verification"
                } else {
                    "not_claimed_for_Galerkin_Q_transpose_transport; local_material_guards_retained"
                },
                "fully_coupled_nonlinear_maximum_principle_claimed": false,
                "optimization_temperature_metric": "selected_response_definition; not_inferred_from_temperature_layout",
                "built_in_solid_nodal_temperature_metric": "normalized_order_32_power_mean_over_post_initial_solid_nodal_history",
                "literal_shared_temperature_peak_K": history["literal_shared_temperature_peak_K"],
                "literal_solid_temperature_peak_K": history["literal_solid_temperature_peak_K"],
                "literal_coolant_temperature_peak_K": history["literal_coolant_temperature_peak_K"],
                "literal_peak_remains_validity_guard": true,
                "order": "first_order_mesh_sensitivity_required"},
            "traction_nodal_resultant_error_N": history["traction_nodal_resultant_error_N"],
            "total_thermal_balance_available": can_balance,
            "thermal_contact": if film_enabled { "separate_temperatures_authored_wall_film_exchange" } else { "perfect_LTE_no_artificial_interface_conductance" },
            "thermal_balance_control_volume": "solid_and_fluid_fields; reservoir storage is reported separately; exchange power is outward",
            "geometry": "one_common_implicit_occupancy_everywhere_no_fixed_internal_partition",
            "phase_capacity_floor": f.p["regularisation"]["fluid_fraction_floor"],
            "hydraulic_power_definition": "delta_pressure_times_volumetric_flow_not_pump_shaft_power",
            "caloric_mixture_rule": if film_enabled { "separate_solid_endmember_volumetric_enthalpy_on_T_s_and_fluid_enthalpy_on_T_f" } else { "additive_endmember_volumetric_enthalpy_c_is_volume_fraction_revision54" },
            "material_provenance": provenance,
            "fluid_temperature_metric": if film_enabled { "phase_weighted_p12_mean_of_Q_coolant_temperature_samples_not_literal_maximum_or_caloric_quadrature" } else { "phase_weighted_p12_mean_of_Q_shared_temperature_samples_not_literal_maximum_or_caloric_quadrature" },
            "times_s": s.p["times_s"],
        });
        Ok(out.as_object().cloned().unwrap_or_default())
    }
}
